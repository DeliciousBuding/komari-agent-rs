// komari-agent-rs: GPU detection — platform dispatch via #[cfg] gates.
// DD7 in spec.md: nvidia-smi CSV → rocm-smi key-scan → sysfs DRM → lspci → DXGI → system_profiler → pciconf.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/gpu_*.go
//
// The full backend enum (`GpuBackend` variants for every platform tool), the
// `NoBackend` error, and the `GpuDetector` trait form a cross-platform parity
// surface; on any single target most are unused. Allow dead_code for the surface.
#![allow(dead_code)]

use crate::arena::{ArenaErr, MAX_GPUS, SmallVec};
use std::fmt;

#[cfg(target_os = "freebsd")]
mod freebsd;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

// ── GpuBackend ─────────────────────────────────────────────────────────────

/// Which detection backend produced the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    /// No GPU detection backend available or all failed.
    None,
    /// NVIDIA Management Library (`nvidia-smi` CSV mode).
    NvidiaSmi,
    /// AMD ROCm System Management Interface (`rocm-smi` key-scan).
    RocmSmi,
    /// Windows DXGI COM API.
    Dxgi,
    /// Linux `lspci` VGA / 3D / Display scan.
    Lspci,
    /// FreeBSD `pciconf -lv` class=0x03 filter.
    Pciconf,
    /// macOS `system_profiler SPDisplaysDataType -xml` key scan.
    SystemProfiler,
}

impl fmt::Display for GpuBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::NvidiaSmi => write!(f, "nvidia-smi"),
            Self::RocmSmi => write!(f, "rocm-smi"),
            Self::Dxgi => write!(f, "dxgi"),
            Self::Lspci => write!(f, "lspci"),
            Self::Pciconf => write!(f, "pciconf"),
            Self::SystemProfiler => write!(f, "system_profiler"),
        }
    }
}

// ── GpuInfo ────────────────────────────────────────────────────────────────

/// Hardware metrics for a single GPU device.
#[derive(Debug, Clone)]
pub struct GpuInfo {
    /// Human-readable GPU name (e.g. "NVIDIA GeForce RTX 4090").
    pub name: String,
    /// Total video memory in bytes.
    pub memory_total: u64,
    /// Currently used video memory in bytes.
    pub memory_used: u64,
    /// GPU core utilisation as a percentage (0.0–100.0).
    pub utilization: f64,
    /// GPU temperature in degrees Celsius.
    pub temperature: u64,
}

// ── GpuDetectErr ───────────────────────────────────────────────────────────

/// Error type for GPU detection failures.
#[derive(Debug, Clone)]
pub enum GpuDetectErr {
    /// No supported detection backend was found on this system.
    NoBackend,
    /// A subprocess failed to launch or returned a non-zero exit code.
    Subprocess(String),
    /// Output from a tool could not be parsed.
    Parse(String),
    /// More than [`MAX_GPUS`] devices detected (should not happen in practice).
    TooManyGpus,
}

impl From<ArenaErr> for GpuDetectErr {
    fn from(_: ArenaErr) -> Self {
        GpuDetectErr::TooManyGpus
    }
}

impl fmt::Display for GpuDetectErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBackend => write!(f, "no GPU detection backend available"),
            Self::Subprocess(msg) => write!(f, "subprocess error: {}", msg),
            Self::Parse(msg) => write!(f, "parse error: {}", msg),
            Self::TooManyGpus => write!(f, "too many GPU devices (max {})", MAX_GPUS),
        }
    }
}

// ── GpuDetector trait ──────────────────────────────────────────────────────

/// Platform-specific GPU detection.  Each platform module provides a
/// top-level `detect()` free function that walks the backend priority chain.
pub trait GpuDetector {
    /// Probe all available GPU backends in priority order and return the
    /// backend used plus the list of detected devices.
    fn detect() -> Result<(GpuBackend, SmallVec<GpuInfo, MAX_GPUS>), GpuDetectErr>;
}

// ── Platform dispatch ─────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use linux::detect as detect_gpus;

#[cfg(windows)]
#[allow(unused_imports)]
pub use windows::detect as detect_gpus;

#[cfg(target_os = "macos")]
#[allow(unused_imports)]
pub use macos::detect as detect_gpus;

#[cfg(target_os = "freebsd")]
#[allow(unused_imports)]
pub use freebsd::detect as detect_gpus;

#[cfg(not(any(
    target_os = "linux",
    windows,
    target_os = "macos",
    target_os = "freebsd"
)))]
pub fn detect_gpus() -> Result<(GpuBackend, SmallVec<GpuInfo, MAX_GPUS>), GpuDetectErr> {
    Err(GpuDetectErr::NoBackend)
}
