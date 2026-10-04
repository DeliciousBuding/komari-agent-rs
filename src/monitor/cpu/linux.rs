// komari-agent-rs: Linux CPU metrics — /proc/stat delta + /proc/cpuinfo parsing.
#![allow(dead_code)]

use super::usage_from_ticks;
use crate::arena::ScratchArena;
use std::fs;
use std::io;

#[derive(Debug)]
pub enum MetricErr {
    Io(io::Error),
    Parse(String),
}
impl From<io::Error> for MetricErr {
    fn from(e: io::Error) -> Self {
        MetricErr::Io(e)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PrevCpu {
    pub total: u64,
    pub idle: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct CpuInfo<'a> {
    pub name: &'a str,
    pub cores: u32,
    pub physical_cores: u32,
    pub arch: &'a str,
    pub usage: f64,
}

/// Collect CPU metrics: model name, core counts, arch, and usage percentage.
/// `arena` stores returned `&str` fields; `prev` is updated in-place for delta.
pub fn collect_cpu<'a>(
    arena: &'a mut ScratchArena,
    prev: &mut PrevCpu,
) -> Result<CpuInfo<'a>, MetricErr> {
    // ── /proc/stat: aggregate CPU jiffies → delta usage % ──
    let stat = fs::read_to_string("/proc/stat")?;
    let (total, idle) = parse_stat(&stat)?;

    let usage = usage_from_ticks(prev.total, prev.idle, total, idle);
    prev.total = total;
    prev.idle = idle;

    // ── /proc/cpuinfo: name, logical cores, physical cores ──
    let (name, cores, physical_cores) = parse_cpuinfo(&fs::read_to_string("/proc/cpuinfo")?);

    // Single arena alloc for both strings (avoids double &mut borrow).
    let nb = name.as_bytes();
    let ab = std::env::consts::ARCH.as_bytes();
    let buf = arena.alloc_bytes(nb.len() + ab.len());
    buf[..nb.len()].copy_from_slice(nb);
    buf[nb.len()..].copy_from_slice(ab);
    // SAFETY: both inputs are valid UTF-8; byte copy preserves content.
    let name_ref = unsafe { std::str::from_utf8_unchecked(&buf[..nb.len()]) };
    let arch_ref = unsafe { std::str::from_utf8_unchecked(&buf[nb.len()..]) };
    Ok(CpuInfo {
        name: name_ref,
        cores,
        physical_cores,
        arch: arch_ref,
        usage,
    })
}

fn parse_stat(stat: &str) -> Result<(u64, u64), MetricErr> {
    let line = stat
        .lines()
        .find(|l| l.starts_with("cpu "))
        .ok_or_else(|| MetricErr::Parse("missing 'cpu' line in /proc/stat".into()))?;
    let mut nums = [0u64; 10];
    for (i, tok) in line.split_ascii_whitespace().skip(1).take(10).enumerate() {
        nums[i] = tok
            .parse()
            .map_err(|_| MetricErr::Parse(format!("bad cpu field: {tok}")))?;
    }
    let total: u64 = nums.iter().sum();
    Ok((total, nums[3] + nums[4])) // idle + iowait
}

fn parse_cpuinfo(data: &str) -> (String, u32, u32) {
    let mut name = String::from("Unknown");
    let mut processors: u32 = 0;
    // aarch64 /proc/cpuinfo has no "model name"; keep implementer/part as fallback.
    let mut implementer: Option<u32> = None;
    let mut part: Option<u32> = None;
    let mut phys = [u32::MAX; 256];
    let mut phys_n: u32 = 0;

    for line in data.lines() {
        if name == "Unknown" {
            for pfx in &["model name", "Hardware", "Processor"] {
                if let Some(v) = line.strip_prefix(pfx).and_then(|r| r.strip_prefix("\t: ")) {
                    name = v.trim().to_string();
                    break;
                }
            }
        }
        if implementer.is_none()
            && let Some(v) = line
                .strip_prefix("CPU implementer")
                .and_then(|r| r.strip_prefix("\t: "))
        {
            implementer = parse_hex(v);
        }
        if part.is_none()
            && let Some(v) = line
                .strip_prefix("CPU part")
                .and_then(|r| r.strip_prefix("\t: "))
        {
            part = parse_hex(v);
        }
        if line
            .strip_prefix("processor")
            .and_then(|r| r.strip_prefix("\t: "))
            .is_some()
        {
            processors += 1;
        }
        if let Some(v) = line
            .strip_prefix("physical id")
            .and_then(|r| r.strip_prefix("\t: "))
            && let Ok(id) = v.trim().parse::<u32>()
        {
            let used = &phys[..phys_n as usize];
            if !used.contains(&id) && (phys_n as usize) < phys.len() {
                phys[phys_n as usize] = id;
                phys_n += 1;
            }
        }
    }
    if name == "Unknown"
        && let (Some(i), Some(p)) = (implementer, part)
    {
        name = arm_core_name(i, p);
    }
    (name, processors.max(1), phys_n)
}

fn parse_hex(v: &str) -> Option<u32> {
    u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()
}

/// Human-readable name for an ARM `(implementer, part)` pair; unknown cores
/// fall back to the raw ids so the display is never just "Unknown".
fn arm_core_name(implementer: u32, part: u32) -> String {
    let known = match (implementer, part) {
        (0x41, 0xd03) => "Cortex-A53",
        (0x41, 0xd05) => "Cortex-A55",
        (0x41, 0xd07) => "Cortex-A57",
        (0x41, 0xd08) => "Cortex-A72",
        (0x41, 0xd09) => "Cortex-A73",
        (0x41, 0xd0a) => "Cortex-A75",
        (0x41, 0xd0b) => "Cortex-A76",
        (0x41, 0xd0c) => "Neoverse-N1",
        (0x41, 0xd40) => "Neoverse-V1",
        (0x41, 0xd49) => "Neoverse-N2",
        (0x41, 0xd4f) => "Neoverse-V2",
        (0x61, _) => "Apple Silicon",
        (0x51, _) => "Qualcomm Kryo",
        _ => "",
    };
    if !known.is_empty() {
        return format!("ARM {known}");
    }
    format!("ARM (implementer 0x{implementer:02x}, part 0x{part:03x})")
}

#[cfg(test)]
mod arm_name_tests {
    use super::*;

    #[test]
    fn aarch64_cpuinfo_without_model_name() {
        let data = "processor\t: 0\nBogoMIPS\t: 50.00\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n\nprocessor\t: 1\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n";
        let (name, cores, _) = parse_cpuinfo(data);
        assert_eq!(name, "ARM Neoverse-N1");
        assert_eq!(cores, 2);
    }

    #[test]
    fn unknown_arm_core_shows_ids_and_model_name_wins() {
        let (n, _, _) = parse_cpuinfo("processor\t: 0\nCPU implementer\t: 0xaa\nCPU part\t: 0x123\n");
        assert_eq!(n, "ARM (implementer 0xaa, part 0x123)");
        let (n, _, _) = parse_cpuinfo("model name\t: Foo CPU\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n");
        assert_eq!(n, "Foo CPU");
        assert_eq!(parse_cpuinfo("processor\t: 0\n").0, "Unknown");
    }
}
