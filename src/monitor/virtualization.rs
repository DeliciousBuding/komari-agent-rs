// komari-agent-rs: VM/container detection.
// Reference: D:/Code/Projects/external/komari-agent-go/monitoring/unit/virtualization.go

/// Detect virtualization environment.
/// Returns "HyperV", "VMware", "VirtualBox", "KVM", "Xen", "bhyve",
/// "Docker", "Kubernetes", "LXC", "podman", "containerd", "crio",
/// or "none" (bare metal / unknown).
pub fn detect() -> &'static str {
    // 1. CPUID-based hypervisor detection (x86_64 only)
    if let Some(virt) = detect_via_cpuid() {
        return virt;
    }

    // 2. Linux-specific: container detection, then systemd-detect-virt
    #[cfg(target_os = "linux")]
    {
        if let Some(ct) = detect_container() {
            return ct;
        }
        if let Some(virt) = detect_via_systemd() {
            return virt;
        }
    }

    "none" // bare metal
}

// ── CPUID detection (x86_64) ──────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
fn detect_via_cpuid() -> Option<&'static str> {
    use core::arch::x86_64::__cpuid;

    // Leaf 0x1, ECX bit 31: hypervisor present
    let leaf1 = __cpuid(0x1);
    if leaf1.ecx & (1 << 31) == 0 {
        return None;
    }

    // Leaf 0x40000000: 12-byte vendor signature in EBX, ECX, EDX
    let leaf = __cpuid(0x40000000);
    let mut sig = [0u8; 12];
    sig[..4].copy_from_slice(&leaf.ebx.to_le_bytes());
    sig[4..8].copy_from_slice(&leaf.ecx.to_le_bytes());
    sig[8..12].copy_from_slice(&leaf.edx.to_le_bytes());

    match core::str::from_utf8(&sig).unwrap_or("") {
        "VMwareVMware" => Some("VMware"),
        "VBoxVBoxVBox" => Some("VirtualBox"),
        "Microsoft Hv" => Some("HyperV"),
        "KVMKVMKVM" => Some("KVM"),
        "XenVMMXenVMM" => Some("Xen"),
        "bhyve bhyve" => Some("bhyve"),
        _ => None,
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn detect_via_cpuid() -> Option<&'static str> {
    None
}

// ── Linux container detection ─────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn detect_container() -> Option<&'static str> {
    // /run/.containerenv — podman / CRI-O marker file
    if std::path::Path::new("/run/.containerenv").exists() {
        return Some("podman");
    }

    // /dev/.lxc-boot-id — LXC marker file
    if std::path::Path::new("/dev/.lxc-boot-id").exists() {
        return Some("lxc");
    }

    // /.dockerenv — Docker marker file
    if std::path::Path::new("/.dockerenv").exists() {
        return Some("docker");
    }

    // /proc/self/cgroup — per-line precise runtime-engine matching
    if let Ok(data) = std::fs::read_to_string("/proc/self/cgroup") {
        for line in data.lines() {
            let lower = line.to_lowercase();
            if lower.contains("docker") {
                return Some("docker");
            }
            if lower.contains("containerd") {
                return Some("containerd");
            }
            if lower.contains("kubepods") {
                return Some("kubernetes");
            }
            if lower.contains("libpod") || lower.contains("podman") {
                return Some("podman");
            }
            if lower.contains("crio") {
                return Some("crio");
            }
            if lower.contains("lxc") {
                return Some("lxc");
            }
        }
    }

    None
}

// ── systemd-detect-virt fallback ──────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn detect_via_systemd() -> Option<&'static str> {
    let output = std::process::Command::new("systemd-detect-virt")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    match std::str::from_utf8(&output.stdout).ok()?.trim() {
        "" | "none" => None,
        "kvm" => Some("KVM"),
        "vmware" | "vmware-other" => Some("VMware"),
        "microsoft" | "hyperv" => Some("HyperV"),
        "oracle" | "virtualbox" => Some("VirtualBox"),
        "xen" => Some("Xen"),
        "bhyve" => Some("bhyve"),
        _ => None,
    }
}
