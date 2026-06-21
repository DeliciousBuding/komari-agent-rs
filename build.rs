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
        // libkvm provides kvm_getswapinfo (swap stats in mem/freebsd.rs).
        // libkvm internally depends on libelf (elf_begin/elf_end/elf_kind) to
        // parse the running kernel's ELF headers. A native FreeBSD ld pulls
        // libelf in transitively via libkvm.so's NEEDED entries, but the
        // cross-toolchain (cross's freebsd12 gcc 6.4.0 / ld) does not resolve
        // the versioned elf_*@R1.0 symbols without an explicit -lelf, failing
        // the release CI's freebsd cross build. Link both explicitly so the
        // artifact is identical whether built natively or via cross.
        println!("cargo:rustc-link-lib=kvm");
        println!("cargo:rustc-link-lib=elf");
    }
}
