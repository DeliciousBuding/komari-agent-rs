fn main() {
    #[cfg(target_os = "windows")]
    {
        // Windows system libraries needed by monitor collectors
        println!("cargo:rustc-link-lib=iphlpapi");  // GetIfTable2, GetTcpTable2, GetUdpTable2, GetAdaptersAddresses
        println!("cargo:rustc-link-lib=kernel32");   // GlobalMemoryStatusEx, GetSystemTimes, GetLogicalDriveStringsW, etc.
        println!("cargo:rustc-link-lib=advapi32");    // Registry access (CPU name)
        println!("cargo:rustc-link-lib=user32");      // (if needed)
        println!("cargo:rustc-link-lib=ole32");       // DXGI COM (GPU detection)
    }
}
