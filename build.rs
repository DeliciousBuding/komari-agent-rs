fn main() {
    #[cfg(target_os = "windows")]
    {
        // Windows system libraries needed by monitor collectors
        println!("cargo:rustc-link-lib=iphlpapi"); // GetIfTable2, GetTcpTable2, GetUdpTable2, GetAdaptersAddresses
        println!("cargo:rustc-link-lib=kernel32"); // GlobalMemoryStatusEx, GetSystemTimes, GetLogicalDriveStringsW, etc.
        println!("cargo:rustc-link-lib=advapi32"); // Registry access (CPU name)
        println!("cargo:rustc-link-lib=user32"); // (if needed)
        println!("cargo:rustc-link-lib=ole32"); // DXGI COM (GPU detection)
    }

    #[cfg(target_os = "freebsd")]
    {
        // libkvm provides kvm_getswapinfo (swap stats in mem/freebsd.rs). It
        // transitively depends on libelf's versioned elf_*@R1.0 symbols, which
        // the cross-toolchain (freebsd12 gcc 6.4.0) cannot resolve at link
        // time. We link kvm explicitly here; the unresolved elf_* symbols are
        // handled by `--allow-shlib-undefined` in .cargo/config.toml (they
        // resolve at runtime on a real FreeBSD host via libkvm.so's NEEDED
        // entry pulling in libelf).
        println!("cargo:rustc-link-lib=kvm");
    }
}
