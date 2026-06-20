// komari-agent-rs: Linux CPU metrics — /proc/stat delta + /proc/cpuinfo parsing.
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

    let usage = if prev.total > 0 && total > prev.total {
        let td = (total - prev.total) as f64;
        ((td - (idle - prev.idle) as f64) / td) * 100.0
    } else {
        0.0
    };
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
        {
            if let Ok(id) = v.trim().parse::<u32>() {
                let used = &phys[..phys_n as usize];
                if !used.contains(&id) && (phys_n as usize) < phys.len() {
                    phys[phys_n as usize] = id;
                    phys_n += 1;
                }
            }
        }
    }
    (name, processors.max(1), phys_n)
}
