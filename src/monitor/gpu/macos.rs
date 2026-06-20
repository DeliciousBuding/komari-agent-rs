// komari-agent-rs: macOS GPU detection via system_profiler -xml key scanning.
// No plist/xml parser — simple string scanning for sppci_model and spdisplays_vram.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/gpu_darwin.go

use super::{GpuBackend, GpuDetectErr, GpuInfo};
use crate::arena::{MAX_GPUS, SmallVec};
use std::process::Command;

// ── Entry point ────────────────────────────────────────────────────────────

/// Detect GPUs via `system_profiler SPDisplaysDataType -xml`.
/// Parses the XML output by scanning for "sppci_model" (GPU name) and
/// "spdisplays_vram" (VRAM string).  No full XML or plist parser.
pub fn detect() -> Result<(GpuBackend, SmallVec<GpuInfo, MAX_GPUS>), GpuDetectErr> {
    let gpus = detect_system_profiler()?;
    Ok((GpuBackend::SystemProfiler, gpus))
}

fn detect_system_profiler() -> Result<SmallVec<GpuInfo, MAX_GPUS>, GpuDetectErr> {
    let output = Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-xml"])
        .output()
        .map_err(|e| GpuDetectErr::Subprocess(format!("system_profiler: {}", e)))?;

    if !output.status.success() {
        return Err(GpuDetectErr::Subprocess(
            "system_profiler exited non-zero".into(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus: SmallVec<GpuInfo, MAX_GPUS> = SmallVec::new();

    let mut current_name = String::new();
    let mut current_vram_str = String::new();
    let mut in_gpu_entry = false;

    for line in stdout.lines() {
        let trimmed = line.trim();

        // XML plist structure: <key>sppci_model</key><string>NAME</string>
        // Also: <key>spdisplays_vram</key><string>VRAM</string>
        // Detect GPU boundaries by sppci_model key — each GPU entry starts with this.

        if trimmed.contains("<key>sppci_model</key>") {
            // Push previous GPU if we were collecting one
            if !current_name.is_empty() {
                let mem = parse_vram_to_bytes(&current_vram_str);
                let _ = gpus.push(GpuInfo {
                    name: std::mem::take(&mut current_name),
                    memory_total: mem,
                    memory_used: 0,
                    utilization: 0.0,
                    temperature: 0,
                });
            }
            in_gpu_entry = true;
            current_name = extract_xml_string_value(trimmed, &stdout, line);
            current_vram_str.clear();
            continue;
        }

        if !in_gpu_entry {
            continue;
        }

        if trimmed.contains("<key>spdisplays_vram</key>") {
            current_vram_str = extract_xml_string_value(trimmed, &stdout, line);
        }

        // End of GPU entry heuristics: next sppci_model key or end of dict
        // Handled at the top of the loop and after the loop.
    }

    // Push last GPU
    if !current_name.is_empty() {
        let mem = parse_vram_to_bytes(&current_vram_str);
        let _ = gpus.push(GpuInfo {
            name: current_name,
            memory_total: mem,
            memory_used: 0,
            utilization: 0.0,
            temperature: 0,
        });
    }

    if gpus.is_empty() {
        Err(GpuDetectErr::Parse(
            "system_profiler: no GPU entries found".into(),
        ))
    } else {
        Ok(gpus)
    }
}

/// Find the `<string>VALUE</string>` that follows a `<key>...</key>` on the same or next line.
/// `current_line` is the line containing the key element.
/// `full_text` is the complete stdout for cross-line scanning.
fn extract_xml_string_value(current_line: &str, full_text: &str, _after_line: &str) -> String {
    // Try to find <string> on the same line first
    if let Some(val) = extract_tag_content(current_line, "string") {
        return val;
    }

    // Fallback: scan subsequent lines (simplified — just look for <string> in remaining text)
    let pos_in_full = full_text.find(current_line).unwrap_or(0);
    let remaining = &full_text[pos_in_full + current_line.len()..];

    for rem_line in remaining.lines() {
        let trimmed = rem_line.trim();
        if let Some(val) = extract_tag_content(trimmed, "string") {
            return val;
        }
        // Stop if we hit another key (boundary)
        if trimmed.contains("<key>") {
            break;
        }
    }

    String::new()
}

/// Extract content between `<tag>` and `</tag>` on a single line.
fn extract_tag_content(line: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    let start = line.find(&open)?;
    let start = start + open.len();
    let end = line[start..].find(&close)?;

    Some(line[start..start + end].to_string())
}

/// Parse VRAM string like "4 GB", "2048 MB" → bytes.
fn parse_vram_to_bytes(vram: &str) -> u64 {
    let vram = vram.trim().to_lowercase();
    if vram.is_empty() {
        return 0;
    }

    // Split into number + unit
    let parts: Vec<&str> = vram.split_whitespace().collect();
    if parts.is_empty() {
        return 0;
    }

    let value: f64 = parts[0].parse().unwrap_or(0.0);
    let unit = parts.get(1).copied().unwrap_or("mb");

    match unit {
        "gb" => (value * 1_073_741_824.0) as u64,
        "mb" => (value * 1_048_576.0) as u64,
        "kb" => (value * 1024.0) as u64,
        "b" | "bytes" => value as u64,
        _ => (value * 1_048_576.0) as u64, // assume MB
    }
}
