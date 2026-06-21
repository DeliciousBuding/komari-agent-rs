//! Server orchestration: WebSocket event loop, reconnection, heartbeat,
//! and server-message dispatch.
//!
//! This is the agent's runtime backbone after config loading.  It runs a
//! single-threaded, never-returning loop that:
//!
//! 1. Initialises TLS (OS-native root certificates via rustls/ring)
//! 2. Uploads basic system info (once, then periodically)
//! 3. Connects to the Komari server via WebSocket
//! 4. Sends heartbeats, reads/dispatches server messages
//! 5. Reconnects on connection loss with protocol FSM + backoff
//!
//! # Architecture (post-refactor)
//!
//! `run()` delegates to [`reconnection::run_reconnection_loop`], which owns
//! the full connect→maintain→reconnect lifecycle driven by:
//! - [`crate::protocol::fsm::ProtocolFsm`] — 3-strike v2→v1 fallback
//! - [`backoff::Backoff`] — exponential retry delays
//! - [`update_basic_info`] — periodic system-info refresh
//! - [`build_static_heartbeat`] — fallback heartbeat (until monitor wired)

pub mod backoff;
pub mod cf_access;
#[cfg(feature = "ping")]
pub mod ping_http;
#[cfg(feature = "ping")]
pub mod ping_icmp;
#[cfg(feature = "ping")]
pub mod ping_tcp;
pub mod reconnection;
pub mod task;

use crate::config::Config;
use crate::protocol::v2;
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════
// Logging shim — switch to `log` crate when it lands in Cargo.toml.
// ═══════════════════════════════════════════════════════════════════════════

macro_rules! info {
    ($($arg:tt)*) => (eprintln!("[komari] {}", format!($($arg)*)));
}
macro_rules! warn {
    ($($arg:tt)*) => (eprintln!("[komari] WARN: {}", format!($($arg)*)));
}
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => (eprintln!("[komari] ERROR: {}", format!($($arg)*)));
}
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!("[komari] DEBUG: {}", format!($($arg)*));
        }
    };
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Main entry point for the agent runtime.  **Never returns.**
///
/// Delegates all orchestration to [`reconnection::run_reconnection_loop`].
pub fn run(config: &Config) -> ! {
    reconnection::run_reconnection_loop(config)
}

// ═══════════════════════════════════════════════════════════════════════════
// Heartbeat (fallback — kept until monitor is fully wired)
// ═══════════════════════════════════════════════════════════════════════════

/// Build a minimal JSON-RPC v2 `agent.report` notification for connectivity
/// testing.  Placeholder until real system metrics (via `monitor::generate_report`)
/// are wired in Phase 2.
#[allow(dead_code)]
fn build_static_heartbeat() -> Vec<u8> {
    let params = br#"{"cpu":{"usage":0.0},"ram":{"total":0,"used":0,"swap":0},"disk":[],"network":[],"uptime":0,"os":"","arch":""}"#;
    v2::new_notification(v2::METHOD_AGENT_REPORT, params)
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic info upload (periodic — called from reconnection loop)
// ═══════════════════════════════════════════════════════════════════════════

/// Upload basic system identification info to the Komari server.
///
/// Called at startup and periodically thereafter.  POSTs a JSON-RPC v2
/// `agent.basicInfo` notification or v1 raw JSON.
pub(super) fn update_basic_info(
    config: &Config,
    tls_cfg: &Arc<rustls::ClientConfig>,
    dial: &crate::proxy::Dialer,
) -> Result<(), String> {
    let base = config.endpoint.trim_end_matches('/');
    let encoded_token = crate::ws::url_encode(&config.token);

    // Protocol + payload ladder.
    //
    // Protocol: try v2 first (if configured), then v1. Some Komari forks
    // (e.g. v1.1.9) lack the v2/rpc HTTP route entirely (POST → 404), so we
    // descend to the v1 uploadBasicInfo endpoint — mirroring the WS FSM's
    // WsV2→WsV1 downshift. This keeps the agent working across both v2-capable
    // and v1-only Komari deployments.
    //
    // Payload: within each protocol, try the full payload (extended fields
    // kernel_version / cpu_physical_cores) then a compat payload; older
    // servers reject the extended fields and need the compat retry.
    let protos: &[bool] = if config.protocol_version >= 2 {
        &[true, false]
    } else {
        &[false]
    };

    let cf_access = crate::server::cf_access::CfAccess::from_config(config);
    let mut extra_headers: Vec<(String, String)> = Vec::new();
    if let Some(ref cf) = cf_access {
        cf.inject_http_headers(&mut extra_headers);
    }

    let mut last_code: u16 = 0;
    'outer: for &is_v2 in protos {
        let url = if is_v2 {
            format!("{}/api/clients/v2/rpc?token={}", base, encoded_token)
        } else {
            format!(
                "{}/api/clients/uploadBasicInfo?token={}",
                base, encoded_token
            )
        };
        for &extended in &[true, false] {
            let body = if is_v2 {
                build_basic_info_v2(config, extended)
            } else {
                build_basic_info_v1(config, extended)
            };
            match crate::http::http_post(
                &url,
                &body,
                "application/json",
                None,
                &extra_headers,
                tls_cfg,
                dial,
            ) {
                Ok(resp) if resp.status_code == 200 => {
                    info!(
                        "Basic info uploaded (v{}, {})",
                        if is_v2 { 2 } else { 1 },
                        if extended { "full" } else { "compat" }
                    );
                    return Ok(());
                }
                Ok(resp) => {
                    last_code = resp.status_code;
                    // v2 route missing on this server → abandon v2 payloads and
                    // fall through to the v1 endpoint on the next outer iter.
                    if is_v2 && resp.status_code == 404 {
                        warn!("v2 basicInfo endpoint missing (HTTP 404), falling back to v1");
                        continue 'outer;
                    }
                    warn!(
                        "basic info HTTP {} (v{}, {})",
                        resp.status_code,
                        if is_v2 { 2 } else { 1 },
                        if extended {
                            "full, retrying compat"
                        } else {
                            "compat"
                        }
                    );
                }
                Err(e) => {
                    warn!("basic info transport error: {}", e);
                }
            }
        }
    }
    let msg = format!("Basic info upload returned HTTP {}", last_code);
    warn!("{}", msg);
    Err(msg)
}

/// Collect static system identification from every collector and encode as
/// flat JSON (the inner `info` object shared by v1 and v2 uploads).
///
/// Field set matches Go `uploadBasicInfo` exactly:
/// cpu_name, cpu_cores, cpu_physical_cores, arch, os, kernel_version, ipv4,
/// ipv6, mem_total, swap_total, disk_total, gpu_name, virtualization, version.
fn collect_basic_info(config: &Config, include_extended: bool) -> Vec<u8> {
    let mut arena = crate::arena::ScratchArena::new();
    let mut prev_cpu = crate::monitor::cpu::PrevCpu::default();
    let mut buf = vec![0u8; 2048];
    let len = {
        let mut j = crate::json::JsonBuf::new(&mut buf);
        let _ = encode_basic_info(&mut j, &mut arena, &mut prev_cpu, config, include_extended);
        j.finish().len()
    };
    buf.truncate(len);
    buf
}

/// Encode the basic-info JSON into `j`.  Each collector is best-effort: a
/// failure yields a sensible default rather than aborting the whole payload.
///
/// `include_extended` controls the `kernel_version` and `cpu_physical_cores`
/// fields. Older Komari servers (<= 1.0.2 / <= 1.2.0, and the v1.1.9 fork)
/// reject the payload when these are present — the caller retries with this
/// flag off (mirrors Go `uploadBasicInfo` fallback).
fn encode_basic_info(
    j: &mut crate::json::JsonBuf,
    arena: &mut crate::arena::ScratchArena,
    prev_cpu: &mut crate::monitor::cpu::PrevCpu,
    config: &Config,
    include_extended: bool,
) -> Result<(), crate::json::JsonErr> {
    use crate::json::Field;

    // CPU (name/cores/arch — usage is ignored here, it lives in the report).
    let cpu = match crate::monitor::cpu::collect_cpu(arena, prev_cpu) {
        Ok(info) => info,
        Err(_) => {
            let fb = arena.alloc_bytes(7);
            fb.copy_from_slice(b"Unknown");
            let fb_str = unsafe { std::str::from_utf8_unchecked(fb) };
            crate::monitor::cpu::CpuInfo {
                name: fb_str,
                cores: 0,
                physical_cores: 0,
                arch: fb_str,
                usage: 0.0,
            }
        }
    };

    let os = crate::monitor::os::collect();
    let mem = crate::monitor::mem::collect(config);
    let disks = crate::monitor::disk::collect(config);
    let (disk_total, _) = crate::monitor::disk::aggregate(&disks);
    let (ipv4, ipv6) = crate::monitor::ip::collect_ip(config).unwrap_or((None, None));
    let gpu_name = crate::monitor::gpu::detect_gpus()
        .ok()
        .and_then(|(_, gpus)| gpus.as_slice().first().map(|g| g.name.clone()))
        .unwrap_or_default();
    let virt = crate::monitor::virtualization::detect();

    j.begin_obj()?;
    j.str_field(Field::CpuName, cpu.name)?;
    j.u64_field(Field::CpuCores, cpu.cores as u64)?;
    if include_extended {
        j.u64_field(Field::CpuPhysicalCores, cpu.physical_cores as u64)?;
    }
    j.str_field(Field::Arch, cpu.arch)?;
    j.str_field(Field::Os, &os.name)?;
    if include_extended {
        j.str_field(Field::KernelVersion, &os.kernel_version)?;
    }
    j.str_field(Field::Ipv4, ipv4.as_deref().unwrap_or(""))?;
    j.str_field(Field::Ipv6, ipv6.as_deref().unwrap_or(""))?;
    j.u64_field(Field::MemTotal, mem.total)?;
    j.u64_field(Field::SwapTotal, mem.swap_total)?;
    j.u64_field(Field::DiskTotal, disk_total)?;
    j.str_field(Field::GpuName, &gpu_name)?;
    j.str_field(Field::Virtualization, virt)?;
    j.str_field(Field::Version, env!("CARGO_PKG_VERSION"))?;
    j.end_obj()?;
    Ok(())
}

fn build_basic_info_v2(config: &Config, include_extended: bool) -> Vec<u8> {
    let info = collect_basic_info(config, include_extended);
    // Wrap as JSON-RPC notification: {"jsonrpc":"2.0","method":"agent.basicInfo","params":{"info":<info>}}
    let mut params = Vec::with_capacity(info.len() + 20);
    params.extend_from_slice(b"{\"info\":");
    params.extend_from_slice(&info);
    params.push(b'}');
    v2::new_notification(v2::METHOD_AGENT_BASIC_INFO, &params)
}

fn build_basic_info_v1(config: &Config, include_extended: bool) -> Vec<u8> {
    // V1: flat JSON, no JSON-RPC wrapper.
    collect_basic_info(config, include_extended)
}
