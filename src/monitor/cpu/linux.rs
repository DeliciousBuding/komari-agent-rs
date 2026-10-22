// komari-agent-rs: Linux CPU metrics — /proc/stat delta + /proc/cpuinfo parsing.
#![allow(dead_code)]

use super::usage_from_ticks;
use crate::arena::ScratchArena;
use std::fs;
use std::io;
use std::sync::OnceLock;

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
    // Static for the process lifetime; reading + parsing it every tick is
    // expensive on many-core hosts. Only a successful read is cached.
    static STATIC_INFO: OnceLock<(String, u32, u32)> = OnceLock::new();
    let (name, cores, physical_cores) =
        cached_cpuinfo(&STATIC_INFO, || fs::read_to_string("/proc/cpuinfo"))?;
    let (cores, physical_cores) = (*cores, *physical_cores);

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

fn cached_cpuinfo(
    cache: &OnceLock<(String, u32, u32)>,
    read: impl FnOnce() -> io::Result<String>,
) -> io::Result<&(String, u32, u32)> {
    if let Some(value) = cache.get() {
        return Ok(value);
    }
    let parsed = parse_cpuinfo(&read()?);
    Ok(cache.get_or_init(|| parsed))
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
    // guest and guest_nice are already included in user/nice by Linux.
    let total: u64 = nums[..8].iter().sum();
    Ok((total, nums[3] + nums[4])) // idle + iowait
}

fn parse_cpuinfo(data: &str) -> (String, u32, u32) {
    use std::collections::BTreeSet;
    let mut name = None;
    let mut processors = 0u32;
    let mut physical = BTreeSet::new();
    let mut arm_names = BTreeSet::new();
    for section in data.split("\n\n") {
        let (mut socket, mut core, mut implementer, mut part) = (None, None, None, None);
        for (key, value) in section.lines().filter_map(|l| l.split_once(':')) {
            let value = value.trim();
            match key.trim() {
                "model name" | "Hardware" | "Processor" if name.is_none() && !value.is_empty() => {
                    name = Some(value.to_string())
                }
                "processor" => processors += 1,
                "physical id" => socket = value.parse::<u32>().ok(),
                "core id" => core = value.parse::<u32>().ok(),
                "CPU implementer" => implementer = parse_hex(value),
                "CPU part" => part = parse_hex(value),
                _ => {}
            }
        }
        if let (Some(socket), Some(core)) = (socket, core) {
            physical.insert((socket, core));
        }
        if let (Some(i), Some(p)) = (implementer, part) {
            arm_names.insert(arm_core_name(i, p));
        }
    }
    let name = name.unwrap_or_else(|| {
        if arm_names.is_empty() {
            "Unknown".to_string()
        } else {
            arm_names.into_iter().collect::<Vec<_>>().join(" + ")
        }
    });
    // Without topology report the logical count, never mistake socket count for cores.
    let cores = processors.max(1);
    let physical_cores = if physical.is_empty() {
        cores
    } else {
        physical.len() as u32
    };
    (name, cores, physical_cores)
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
    fn guest_time_is_not_counted_twice() {
        assert_eq!(
            parse_stat("cpu 100 20 30 400 50 6 7 8 90 10\n").unwrap(),
            (621, 450)
        );
        assert!(parse_stat("cpu x 0").is_err());
        assert!(parse_stat("intr 1").is_err());
    }

    #[test]
    fn physical_topology_counts_cores_not_sockets_or_threads() {
        let data = "processor: 0\nmodel name : Example x86\nphysical id: 0\ncore id: 0\n\nprocessor: 1\nphysical id: 0\ncore id: 0\n\nprocessor: 2\nphysical id: 0\ncore id: 1\n\nprocessor: 3\nphysical id: 1\ncore id: 0\n";
        assert_eq!(parse_cpuinfo(data), ("Example x86".to_string(), 4, 3));
    }

    #[test]
    fn heterogeneous_arm_and_unknown_ids_are_not_mislabelled() {
        let data = "processor : 0\nCPU implementer : 0x41\nCPU part : 0xd03\n\nprocessor:1\nCPU implementer:0x41\nCPU part:0xd0b\n";
        assert_eq!(
            parse_cpuinfo(data),
            ("ARM Cortex-A53 + ARM Cortex-A76".to_string(), 2, 2)
        );
        assert_eq!(
            arm_core_name(0x51, 0x999),
            "ARM (implementer 0x51, part 0x999)"
        );
    }

    #[test]
    fn cpuinfo_cache_retries_failure_and_reads_success_only_once() {
        let cache = OnceLock::new();
        assert!(cached_cpuinfo(&cache, || Err(io::Error::other("fixture"))).is_err());
        assert!(cache.get().is_none());
        assert_eq!(
            cached_cpuinfo(&cache, || Ok("processor:0\nmodel name: Fixture".into()))
                .unwrap()
                .0,
            "Fixture"
        );
        assert_eq!(
            cached_cpuinfo(&cache, || panic!("cached data must not be read again"))
                .unwrap()
                .0,
            "Fixture"
        );
    }

    #[test]
    fn aarch64_cpuinfo_without_model_name() {
        let data = "processor\t: 0\nBogoMIPS\t: 50.00\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n\nprocessor\t: 1\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n";
        let (name, cores, _) = parse_cpuinfo(data);
        assert_eq!(name, "ARM Neoverse-N1");
        assert_eq!(cores, 2);
    }

    #[test]
    fn unknown_arm_core_shows_ids_and_model_name_wins() {
        let (n, _, _) =
            parse_cpuinfo("processor\t: 0\nCPU implementer\t: 0xaa\nCPU part\t: 0x123\n");
        assert_eq!(n, "ARM (implementer 0xaa, part 0x123)");
        let (n, _, _) =
            parse_cpuinfo("model name\t: Foo CPU\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n");
        assert_eq!(n, "Foo CPU");
        assert_eq!(parse_cpuinfo("processor\t: 0\n").0, "Unknown");
    }
}
