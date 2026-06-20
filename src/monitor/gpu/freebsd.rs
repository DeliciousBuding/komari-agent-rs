// komari-agent-rs: FreeBSD GPU detection via pciconf -lv.
// Filters for class=0x03 (display controller), parses vendor/device strings.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/gpu_freebsd.go

use crate::arena::{SmallVec, MAX_GPUS};
use super::{GpuBackend, GpuDetectErr, GpuInfo};
use std::process::Command;

// ── Entry point ────────────────────────────────────────────────────────────

/// Detect GPUs via `pciconf -lv`.  Filters for class=0x03 (display) devices
/// and parses vendor + device description strings.
pub fn detect() -> Result<(GpuBackend, SmallVec<GpuInfo, MAX_GPUS>), GpuDetectErr> {
    let gpus = detect_pciconf()?;
    Ok((GpuBackend::Pciconf, gpus))
}

fn detect_pciconf() -> Result<SmallVec<GpuInfo, MAX_GPUS>, GpuDetectErr> {
    let output = Command::new("pciconf")
        .args(["-lv"])
        .output()
        .map_err(|e| GpuDetectErr::Subprocess(format!("pciconf: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus: SmallVec<GpuInfo, MAX_GPUS> = SmallVec::new();

    let mut in_display_device = false;
    let mut current_vendor = String::new();
    let mut current_device = String::new();

    for line in stdout.lines() {
        let trimmed = line.trim();

        // Device header line: "vgapci0@pci0:1:0:0:    class=0x030000 ..."
        // Check if this is a display-class device (class starts with 0x03)
        if trimmed.contains('@') && !trimmed.starts_with("    ") {
            // Push previous device if we were collecting one
            if in_display_device && !current_device.is_empty() {
                let name = build_gpu_name(&current_vendor, &current_device);
                let _ = gpus.push(GpuInfo {
                    name,
                    memory_total: 0,
                    memory_used: 0,
                    utilization: 0.0,
                    temperature: 0,
                });
            }

            // Check for class=0x03 (display controller)
            in_display_device = contains_display_class(trimmed);
            current_vendor.clear();
            current_device.clear();
            continue;
        }

        if !in_display_device {
            continue;
        }

        // Indented property lines: "    vendor     = 'NVIDIA Corporation'"
        //                           "    device     = 'AD102 [GeForce RTX 4090]'"
        if let Some(val) = extract_quoted_field(trimmed, "vendor") {
            current_vendor = val;
        } else if let Some(val) = extract_quoted_field(trimmed, "device") {
            current_device = val;
        }
    }

    // Push last device
    if in_display_device && !current_device.is_empty() {
        let name = build_gpu_name(&current_vendor, &current_device);
        let _ = gpus.push(GpuInfo {
            name,
            memory_total: 0,
            memory_used: 0,
            utilization: 0.0,
            temperature: 0,
        });
    }

    if gpus.is_empty() {
        Err(GpuDetectErr::Parse("pciconf: no display-class (0x03) devices found".into()))
    } else {
        Ok(gpus)
    }
}

/// Check if a pciconf header line contains display-class identifier (class=0x03...).
fn contains_display_class(header: &str) -> bool {
    // Look for "class=0x03" pattern (display controller base class)
    header.contains("class=0x03")
        || header.to_lowercase().contains("vga")
        || header.to_lowercase().contains("display")
}

/// Extract a value from `field = 'value'` or `field = "value"` syntax.
fn extract_quoted_field(line: &str, field_name: &str) -> Option<String> {
    // Match "field_name = 'value'" or "field_name = \"value\""
    let prefix = format!("{}", field_name);
    let pos = line.find(&prefix)?;

    // After the field name, expect " = '..." or " = \"..."
    let after_field = &line[pos + prefix.len()..];
    let eq_pos = after_field.find('=')?;
    let after_eq = after_field[eq_pos + 1..].trim_start();

    // Check for single or double quote
    let quote_char = after_eq.chars().next()?;
    if quote_char != '\'' && quote_char != '"' {
        return None;
    }

    let inner = &after_eq[1..];
    let end_quote = inner.find(quote_char)?;
    Some(inner[..end_quote].to_string())
}

/// Build a human-readable GPU name from vendor + device description.
fn build_gpu_name(vendor: &str, device: &str) -> String {
    match (vendor.is_empty(), device.is_empty()) {
        (true, true) => "Unknown GPU".to_string(),
        (true, false) => device.to_string(),
        (false, true) => vendor.to_string(),
        (false, false) => format!("{} {}", vendor, device),
    }
}
