// komari-agent-rs: Linux GPU detection.
// Priority order: 1) nvidia-smi CSV  2) rocm-smi --json  3) sysfs DRM  4) lspci
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/gpu_linux.go

use crate::arena::{SmallVec, MAX_GPUS};
use super::{GpuBackend, GpuDetectErr, GpuInfo};
use std::fs;
use std::process::Command;

// ── Entry point ────────────────────────────────────────────────────────────

/// Probe GPUs in priority order.  Returns the backend used plus device list.
pub fn detect() -> Result<(GpuBackend, SmallVec<GpuInfo, MAX_GPUS>), GpuDetectErr> {
    // 1. NVIDIA — nvidia-smi CSV (no XML dependency)
    if let Ok(gpus) = detect_nvidia_smi_csv() {
        return Ok((GpuBackend::NvidiaSmi, gpus));
    }

    // 2. AMD — rocm-smi --showallinfo --json (key scanning, no full JSON parse)
    if let Ok(gpus) = detect_rocm_smi() {
        return Ok((GpuBackend::RocmSmi, gpus));
    }

    // 3. Fallback: sysfs DRM device tree
    if let Ok(gpus) = detect_sysfs_drm() {
        return Ok((GpuBackend::Lspci, gpus)); // sysfs is still lspci-class detection
    }

    // 4. Final fallback: lspci grep
    if let Ok(gpus) = detect_lspci() {
        return Ok((GpuBackend::Lspci, gpus));
    }

    Err(GpuDetectErr::NoBackend)
}

// ── 1. NVIDIA: nvidia-smi CSV mode ─────────────────────────────────────────
// Matches DD7 spec: --query-gpu=name,memory.total,memory.used,utilization.gpu,temperature.gpu --format=csv,noheader,nounits

fn detect_nvidia_smi_csv() -> Result<SmallVec<GpuInfo, MAX_GPUS>, GpuDetectErr> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.used,utilization.gpu,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .map_err(|e| GpuDetectErr::Subprocess(format!("nvidia-smi: {}", e)))?;

    if !output.status.success() {
        return Err(GpuDetectErr::Subprocess("nvidia-smi exited non-zero".into()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus: SmallVec<GpuInfo, MAX_GPUS> = SmallVec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // CSV: name, memory_total, memory_used, utilization, temperature
        // All values are unit-less per --nounits.
        let fields: Vec<&str> = trimmed.split(',').map(|s| s.trim()).collect();
        if fields.len() < 5 {
            continue;
        }

        let name = fields[0].to_string();
        // Memory is in MiB when nounits is used (nvidia-smi default unit) —
        // but with --nounits it's just the raw number; nvidia-smi reports in MiB.
        let mem_total: u64 = fields[1]
            .parse::<f64>()
            .map(|v| (v * 1_048_576.0) as u64)
            .unwrap_or(0);
        let mem_used: u64 = fields[2]
            .parse::<f64>()
            .map(|v| (v * 1_048_576.0) as u64)
            .unwrap_or(0);
        let utilization: f64 = fields[3].parse().unwrap_or(0.0);
        let temperature: u64 = fields[4].parse::<f64>().map(|v| v as u64).unwrap_or(0);

        gpus.push(GpuInfo {
            name,
            memory_total: mem_total,
            memory_used: mem_used,
            utilization,
            temperature,
        })
        .map_err(|_| GpuDetectErr::TooManyGpus)?;
    }

    if gpus.is_empty() {
        Err(GpuDetectErr::Parse("nvidia-smi: no GPU lines parsed".into()))
    } else {
        Ok(gpus)
    }
}

// ── 2. AMD: rocm-smi --showallinfo --json (key scanning) ──────────────────
// DD7 spec: scan for "GPU use (%)", "VRAM Total Memory (B)", "VRAM Total Used Memory (B)",
// "Temperature" keys ONLY. No full JSON parse.

fn detect_rocm_smi() -> Result<SmallVec<GpuInfo, MAX_GPUS>, GpuDetectErr> {
    let output = Command::new("rocm-smi")
        .args(["--showallinfo", "--json"])
        .output()
        .map_err(|e| GpuDetectErr::Subprocess(format!("rocm-smi: {}", e)))?;

    if !output.status.success() {
        return Err(GpuDetectErr::Subprocess("rocm-smi exited non-zero".into()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus: SmallVec<GpuInfo, MAX_GPUS> = SmallVec::new();

    // rocm-smi JSON is a flat map: "card0": { "Card series": "...", "GPU use (%)": "...", ... }
    // Scan by looking for "card" keys and extracting the four target sub-keys via string search.
    let mut current_name = String::new();
    let mut current_mem_total: u64 = 0;
    let mut current_mem_used: u64 = 0;
    let mut current_util: f64 = 0.0;
    let mut current_temp: u64 = 0;
    let mut in_card = false;

    for line in stdout.lines() {
        let trimmed = line.trim();

        // Detect "cardN": { start
        if !in_card && trimmed.starts_with('"') && trimmed.contains("card") && trimmed.contains(":{") {
            in_card = true;
            current_name.clear();
            current_mem_total = 0;
            current_mem_used = 0;
            current_util = 0.0;
            current_temp = 0;
            continue;
        }

        if !in_card {
            continue;
        }

        // Detect end of card object
        if trimmed == "}," || trimmed == "}" {
            // Push the current card if we have at least a name
            if !current_name.is_empty() {
                gpus.push(GpuInfo {
                    name: std::mem::take(&mut current_name),
                    memory_total: current_mem_total,
                    memory_used: current_mem_used,
                    utilization: current_util,
                    temperature: current_temp,
                })
                .map_err(|_| GpuDetectErr::TooManyGpus)?;
            }
            in_card = false;
            continue;
        }

        // Scan for the four target keys via substring matching
        if let Some(val) = extract_json_string_value(trimmed, "Card series") {
            current_name = val.to_string();
        } else if let Some(val) = extract_json_string_value(trimmed, "GPU use (%)") {
            current_util = val.trim_end_matches('%').trim().parse().unwrap_or(0.0);
        } else if let Some(val) = extract_json_string_value(trimmed, "VRAM Total Memory (B)") {
            current_mem_total = val.parse().unwrap_or(0);
        } else if let Some(val) = extract_json_string_value(trimmed, "VRAM Total Used Memory (B)") {
            current_mem_used = val.parse().unwrap_or(0);
        } else if let Some(val) = extract_json_string_value(trimmed, "Temperature (Sensor junction) (C)") {
            current_temp = val.trim_end_matches('C').trim().parse().unwrap_or(0);
        }
    }

    if gpus.is_empty() {
        Err(GpuDetectErr::Parse("rocm-smi: no GPU cards found".into()))
    } else {
        Ok(gpus)
    }
}

/// Extract a JSON string value from a line like `"key": "value",` or `"key": "value"`.
/// No allocations — returns a sub-slice of the input.
fn extract_json_string_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    // Look for the key pattern: "KEY"
    let search = format!("\"{}\"", key);
    let pos = line.find(&search)?;
    let after_key = &line[pos + search.len()..];

    // Expect ": " then a quoted string
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();

    if !after_colon.starts_with('"') {
        return None;
    }

    // Extract the quoted string value
    let inner = &after_colon[1..];
    let end_quote = inner.find('"')?;
    Some(&inner[..end_quote])
}

// ── 3. Fallback: sysfs DRM ────────────────────────────────────────────────
// Reads /sys/class/drm/card*/device/vendor + device → matches vendor ID.
// Reads device name from uevent.

fn detect_sysfs_drm() -> Result<SmallVec<GpuInfo, MAX_GPUS>, GpuDetectErr> {
    let mut gpus: SmallVec<GpuInfo, MAX_GPUS> = SmallVec::new();

    // Iterate card0..card15
    for idx in 0..16u32 {
        let card_path = format!("/sys/class/drm/card{}", idx);
        let device_path = format!("{}/device", card_path);

        // Check vendor
        let vendor_path = format!("{}/vendor", device_path);
        let vendor_str = match fs::read_to_string(&vendor_path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => continue,
        };

        // Parse 0xNNNN vendor ID
        let vendor_id = u32::from_str_radix(vendor_str.trim_start_matches("0x"), 16).unwrap_or(0);

        // Only match known GPU vendors
        let vendor_name = match vendor_id {
            0x10de => "NVIDIA",
            0x1002 => "AMD",
            0x8086 => "Intel",
            _ => continue,
        };

        // Read device ID
        let device_path_file = format!("{}/device", device_path);
        let device_str = fs::read_to_string(&device_path_file).unwrap_or_default();
        let device_id = u32::from_str_radix(device_str.trim().trim_start_matches("0x"), 16).unwrap_or(0);

        // Read device name from uevent
        let uevent_path = format!("{}/uevent", device_path);
        let uevent = fs::read_to_string(&uevent_path).unwrap_or_default();
        let model = uevent
            .lines()
            .find(|l| l.starts_with("DRIVER="))
            .map(|l| l.trim_start_matches("DRIVER=").to_string())
            .unwrap_or_else(|| format!("{} {:04x}:{:04x}", vendor_name, vendor_id, device_id));

        gpus.push(GpuInfo {
            name: model,
            memory_total: 0,
            memory_used: 0,
            utilization: 0.0,
            temperature: 0,
        })
        .map_err(|_| GpuDetectErr::TooManyGpus)?;
    }

    if gpus.is_empty() {
        Err(GpuDetectErr::Parse("sysfs: no DRM GPU devices found".into()))
    } else {
        Ok(gpus)
    }
}

// ── 4. Final fallback: lspci ──────────────────────────────────────────────
// Runs `lspci` and filters for VGA / 3D / Display devices.

fn detect_lspci() -> Result<SmallVec<GpuInfo, MAX_GPUS>, GpuDetectErr> {
    let output = Command::new("lspci")
        .output()
        .map_err(|e| GpuDetectErr::Subprocess(format!("lspci: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus: SmallVec<GpuInfo, MAX_GPUS> = SmallVec::new();

    for line in stdout.lines() {
        let lower = line.to_lowercase();
        if !lower.contains("vga") && !lower.contains("3d") && !lower.contains("display") {
            continue;
        }

        // Extract name after the last colon
        let name = match line.rfind(':') {
            Some(idx) if idx + 1 < line.len() => line[idx + 1..].trim(),
            _ => line,
        };

        // Strip trailing "(rev XX)"
        let name = match name.rfind('(') {
            Some(idx) => name[..idx].trim(),
            None => name,
        };

        if name.is_empty() {
            continue;
        }

        gpus.push(GpuInfo {
            name: name.to_string(),
            memory_total: 0,
            memory_used: 0,
            utilization: 0.0,
            temperature: 0,
        })
        .map_err(|_| GpuDetectErr::TooManyGpus)?;
    }

    if gpus.is_empty() {
        Err(GpuDetectErr::Parse("lspci: no VGA/3D/Display devices".into()))
    } else {
        Ok(gpus)
    }
}
