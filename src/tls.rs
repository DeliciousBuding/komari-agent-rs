// tls.rs — TLS configuration with OS native root certificates.
//
// DD5 (OS native roots) + DD6 (ring crypto) from docs/plan/spec.md.
// Only dependency is rustls. All FFI is inline extern blocks.
//
// Platform-specific cert loading:
//   Linux/FreeBSD → read PEM bundle from /etc/ssl/certs (or /usr/local on FreeBSD)
//   Windows       → CertOpenSystemStoreW + CertEnumCertificatesInStore via FFI
//   macOS         → Security.framework SecTrustCopyAnchorCertificates via FFI

use std::fmt;
use std::sync::Arc;

use rustls::pki_types::CertificateDer;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use std::io::Read;

use crate::config::Config;

// ============================================================================
// TlsErr — unified error type for TLS config bootstrapping
// ============================================================================

#[derive(Debug)]
pub enum TlsErr {
    /// An I/O error occurred (e.g. reading the PEM cert bundle).
    Io(std::io::Error),
    /// A rustls error occurred (e.g. invalid certificate).
    Rustls(rustls::Error),
    /// No root certificates were found on the system.
    NoCertsFound,
}

impl fmt::Display for TlsErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "TLS I/O error: {}", e),
            Self::Rustls(e) => write!(f, "TLS error: {}", e),
            Self::NoCertsFound => write!(f, "no root certificates found on this system"),
        }
    }
}

impl std::error::Error for TlsErr {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Rustls(e) => Some(e),
            Self::NoCertsFound => None,
        }
    }
}

impl From<std::io::Error> for TlsErr {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rustls::Error> for TlsErr {
    fn from(e: rustls::Error) -> Self {
        Self::Rustls(e)
    }
}

// ============================================================================
// NoServerVerification — skip all certificate checks (ignore_unsafe_cert)
// ============================================================================

#[derive(Debug)]
struct NoServerVerification;

impl rustls::client::danger::ServerCertVerifier for NoServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

// ============================================================================
// make_tls_config — public entry point
// ============================================================================

/// Build a `rustls::ClientConfig` using ring crypto and OS native root certificates.
///
/// When `config.ignore_unsafe_cert` is true, all server certificate verification
/// is skipped (dangerous — only use for development/trusted networks).
///
/// Returns `TlsErr::NoCertsFound` when no root certificates could be loaded and
/// certificate verification is not disabled.
pub fn make_tls_config(config: &Config) -> Result<rustls::ClientConfig, TlsErr> {
    // Install ring as the process-level default crypto provider.
    // This is idempotent — if already installed, returns `Err` which we ignore.
    let provider = rustls::crypto::ring::default_provider();
    let _ = rustls::crypto::CryptoProvider::install_default(provider.into());

    // Unsafe cert mode: skip all verification.
    if config.ignore_unsafe_cert {
        let client_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoServerVerification))
            .with_no_client_auth();
        return Ok(client_config);
    }

    // Normal mode: load OS native roots and verify.
    let mut root_store = rustls::RootCertStore::empty();
    load_platform_certs(&mut root_store)?;

    if root_store.is_empty() {
        return Err(TlsErr::NoCertsFound);
    }

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(config)
}

// ============================================================================
// Platform-specific cert loading
// ============================================================================

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn load_platform_certs(root_store: &mut rustls::RootCertStore) -> Result<(), TlsErr> {
    // Common PEM bundle paths.  Try each in order; stop on first file that
    // yields at least one valid certificate.
    #[cfg(target_os = "freebsd")]
    const PATHS: &[&str] = &[
        "/usr/local/etc/ssl/certs/ca-root-nss.crt",
        "/usr/local/etc/ssl/cert.pem",
        "/usr/local/share/certs/ca-root-nss.crt",
        "/etc/ssl/certs/ca-certificates.crt",
    ];

    #[cfg(target_os = "linux")]
    const PATHS: &[&str] = &[
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/ssl/cert.pem",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    ];

    for path in PATHS {
        if let Ok(certs) = load_pem_file(path) {
            for cert in certs {
                // Silently skip individual certs that fail to parse.
                let _ = root_store.add(cert);
            }
            if !root_store.is_empty() {
                return Ok(());
            }
        }
    }

    // No file succeeded — caller will see an empty store and return NoCertsFound.
    Ok(())
}

/// Read a PEM file and parse all CERTIFICATE blocks into `CertificateDer` values.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn load_pem_file(path: &str) -> Result<Vec<CertificateDer<'static>>, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(parse_pem_certs(&content))
}

/// Extract every `-----BEGIN CERTIFICATE-----` … `-----END CERTIFICATE-----` block
/// from a PEM string, base64-decode the body, and return as `CertificateDer`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn parse_pem_certs(pem: &str) -> Vec<CertificateDer<'static>> {
    let mut certs: Vec<CertificateDer<'static>> = Vec::new();
    let mut in_cert = false;
    let mut b64_buf = String::new();

    for line in pem.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN CERTIFICATE-----" {
            in_cert = true;
            b64_buf.clear();
        } else if trimmed == "-----END CERTIFICATE-----" {
            if in_cert {
                if let Some(der_bytes) = base64_decode_strip(&b64_buf) {
                    certs.push(CertificateDer::from(der_bytes));
                }
            }
            in_cert = false;
        } else if in_cert {
            // PEM allows whitespace anywhere in the base64 body.
            b64_buf.push_str(trimmed);
        }
    }

    certs
}

// ── Windows: CryptoAPI via FFI ────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn load_platform_certs(root_store: &mut rustls::RootCertStore) -> Result<(), TlsErr> {
    // Open the "ROOT" system store (only trusted root CAs).
    let store_name: Vec<u16> = "ROOT\0".encode_utf16().collect();

    let h_store = unsafe { CertOpenSystemStoreW(0, store_name.as_ptr()) };
    if h_store == 0 {
        return Ok(()); // no store available — caller will see empty
    }

    let mut prev: isize = 0;
    loop {
        let cert_ctx = unsafe { CertEnumCertificatesInStore(h_store, prev) };
        if cert_ctx == 0 {
            break;
        }

        // CERT_CONTEXT layout on x86-64 (40 bytes total):
        //   offset 0:  dwCertEncodingType  (DWORD, 4 bytes)
        //   offset 4:  padding             (4 bytes)
        //   offset 8:  pbCertEncoded       (BYTE*, 8 bytes)
        //   offset 16: cbCertEncoded       (DWORD, 4 bytes)
        //   offset 20: padding             (4 bytes)
        //   offset 24: pCertInfo           (PCERT_INFO, 8 bytes)
        //   offset 32: hCertStore          (HCERTSTORE, 8 bytes)
        unsafe {
            let ctx = cert_ctx as *const u8;
            let pb = std::ptr::read_unaligned(ctx.add(8) as *const *const u8);
            let cb = std::ptr::read_unaligned(ctx.add(16) as *const u32);

            if !pb.is_null() && cb > 0 {
                let der_slice = std::slice::from_raw_parts(pb, cb as usize);
                let _ = root_store.add(CertificateDer::from(der_slice.to_vec()));
            }
        }

        prev = cert_ctx;
    }

    unsafe {
        CertCloseStore(h_store, 0);
    }

    Ok(())
}

#[cfg(target_os = "windows")]
#[link(name = "crypt32")]
unsafe extern "system" {
    /// Opens the named certificate system store.
    /// hProv=0 for the default CSP, szSubsystemProtocol is a null-terminated UTF-16
    /// wide string (e.g. "ROOT", "CA", "MY").
    /// Returns HCERTSTORE (NULL on failure).
    fn CertOpenSystemStoreW(hProv: usize, szSubsystemProtocol: *const u16) -> isize;

    /// Enumerates certificates in a store.
    /// Pass prev=0 to start, then pass each previous PCCERT_CONTEXT to advance.
    /// Returns PCCERT_CONTEXT (NULL when enumeration is complete).
    fn CertEnumCertificatesInStore(hCertStore: isize, pPrevCertContext: isize) -> isize;

    /// Closes a certificate store handle.
    /// dwFlags: typically 0 or CERT_CLOSE_STORE_FORCE_FLAG (1).
    /// Returns TRUE (non-zero) on success.
    fn CertCloseStore(hCertStore: isize, dwFlags: u32) -> i32;
}

// ── macOS: Security.framework via FFI ─────────────────────────────────────

#[cfg(target_os = "macos")]
fn load_platform_certs(root_store: &mut rustls::RootCertStore) -> Result<(), TlsErr> {
    type CFTypeRef = *const std::ffi::c_void;
    type CFArrayRef = CFTypeRef;
    type SecCertificateRef = CFTypeRef;

    unsafe {
        let mut certs_array: CFArrayRef = std::ptr::null();
        let status =
            SecTrustCopyAnchorCertificates(&mut certs_array as *mut CFArrayRef as *mut CFTypeRef);
        if status != 0 || certs_array.is_null() {
            return Ok(()); // no trust store available
        }

        let count = CFArrayGetCount(certs_array as CFTypeRef);
        for i in 0..count {
            let cert_ref = CFArrayGetValueAtIndex(certs_array as CFTypeRef, i) as SecCertificateRef;
            if cert_ref.is_null() {
                continue;
            }
            let data_ref = SecCertificateCopyData(cert_ref as CFTypeRef);
            if data_ref.is_null() {
                continue;
            }

            let len = CFDataGetLength(data_ref as CFTypeRef);
            let bytes = CFDataGetBytePtr(data_ref as CFTypeRef);

            if !bytes.is_null() && len > 0 {
                let der_slice = std::slice::from_raw_parts(bytes, len as usize);
                let _ = root_store.add(CertificateDer::from(der_slice.to_vec()));
            }

            CFRelease(data_ref as CFTypeRef);
        }

        CFRelease(certs_array as CFTypeRef);
    }

    Ok(())
}

#[cfg(target_os = "macos")]
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    /// Returns an array of trusted anchor (root) certificates.
    /// The caller owns the returned CFArrayRef and must CFRelease it.
    fn SecTrustCopyAnchorCertificates(certificates: *mut *const std::ffi::c_void) -> i32;

    /// Returns the DER-encoded content of a SecCertificate.
    /// The caller owns the returned CFDataRef and must CFRelease it.
    fn SecCertificateCopyData(certificate: *const std::ffi::c_void) -> *const std::ffi::c_void;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(theArray: *const std::ffi::c_void) -> isize;
    fn CFArrayGetValueAtIndex(
        theArray: *const std::ffi::c_void,
        idx: isize,
    ) -> *const std::ffi::c_void;
    fn CFDataGetLength(theData: *const std::ffi::c_void) -> isize;
    fn CFDataGetBytePtr(theData: *const std::ffi::c_void) -> *const u8;
    fn CFRelease(cf: *const std::ffi::c_void);
}

// ── Fallback: unsupported platform ────────────────────────────────────────

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "windows",
    target_os = "macos"
)))]
fn load_platform_certs(_root_store: &mut rustls::RootCertStore) -> Result<(), TlsErr> {
    // Unsupported OS — no native root certs available.
    Ok(())
}

// ============================================================================
// Base64 decode (for PEM cert parsing on Linux/FreeBSD)
// ============================================================================

/// Decode a base64 string (after whitespace already stripped) into raw bytes.
/// Returns `None` if the input contains invalid base64 characters.
///
/// This is a minimal, stack-oriented decoder — no heap allocations on the hot
/// path beyond the output `Vec<u8>`.  PEM cert bodies are typically small
/// (≤ a few KB), so the allocation is negligible.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn base64_decode_strip(input: &str) -> Option<Vec<u8>> {
    const DECODE_TABLE: [u8; 128] = {
        let mut t = [0xFFu8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0u8;
        while i < 64 {
            t[chars[i as usize] as usize] = i;
            i += 1;
        }
        t
    };

    let bytes = input.as_bytes();
    let len = bytes.len();

    // Strip '=' padding from end (but keep the count).
    let mut padded = 0u8;
    let mut end = len;
    if end > 0 && bytes[end - 1] == b'=' {
        padded += 1;
        end -= 1;
    }
    if end > 0 && bytes[end - 1] == b'=' {
        padded += 1;
        end -= 1;
    }

    // Output size: every 4 base64 chars → 3 bytes (minus padding).
    let quad_count = (end + 3) / 4;
    let mut out = Vec::with_capacity(quad_count * 3);

    let mut i = 0;
    while i + 3 < end {
        // Fast path: process 4 characters at a time.
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        let b2 = bytes[i + 2];
        let b3 = bytes[i + 3];

        if b0 >= 128 || b1 >= 128 || b2 >= 128 || b3 >= 128 {
            return None;
        }

        let v0 = DECODE_TABLE[b0 as usize];
        let v1 = DECODE_TABLE[b1 as usize];
        let v2 = DECODE_TABLE[b2 as usize];
        let v3 = DECODE_TABLE[b3 as usize];

        if (v0 | v1 | v2 | v3) == 0xFF {
            return None; // invalid character
        }

        out.push((v0 << 2) | (v1 >> 4));
        out.push((v1 << 4) | (v2 >> 2));
        out.push((v2 << 6) | v3);

        i += 4;
    }

    // Handle trailing (partial) quad.
    let remaining = end - i;
    if remaining > 0 {
        let b0 = bytes[i];
        let b1 = if remaining > 1 { bytes[i + 1] } else { b'A' };
        let b2 = if remaining > 2 { bytes[i + 2] } else { b'A' };
        let b3 = if remaining > 3 { bytes[i + 3] } else { b'A' };

        if b0 >= 128 || b1 >= 128 || b2 >= 128 || b3 >= 128 {
            return None;
        }

        let v0 = DECODE_TABLE[b0 as usize];
        let v1 = DECODE_TABLE[b1 as usize];
        let v2 = DECODE_TABLE[b2 as usize];
        let v3 = DECODE_TABLE[b3 as usize];

        if v0 == 0xFF || (remaining > 1 && v1 == 0xFF) {
            return None;
        }
        if remaining > 2 && v2 == 0xFF {
            return None;
        }
        if remaining > 3 && v3 == 0xFF {
            return None;
        }

        out.push((v0 << 2) | (v1 >> 4));
        if remaining > 2 {
            out.push((v1 << 4) | (v2 >> 2));
        }
        if remaining > 3 {
            out.push((v2 << 6) | v3);
        }
    }

    // Trim padding bytes.
    let final_len = out.len().saturating_sub(padded as usize);
    out.truncate(final_len);

    Some(out)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── base64_decode_strip tests ──────────────────────────────────────────

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_base64_decode_empty() {
        let result = base64_decode_strip("");
        assert_eq!(result, Some(vec![]));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_base64_decode_simple() {
        // "Zm9v" = "foo"
        let result = base64_decode_strip("Zm9v");
        assert_eq!(result, Some(b"foo".to_vec()));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_base64_decode_with_padding() {
        // "Zm8=" = "fo" (2 bytes → 3 chars + 1 pad)
        let result = base64_decode_strip("Zm8=");
        assert_eq!(result, Some(b"fo".to_vec()));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_base64_decode_double_padding() {
        // "Zg==" = "f" (1 byte → 2 chars + 2 pad)
        let result = base64_decode_strip("Zg==");
        assert_eq!(result, Some(b"f".to_vec()));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_base64_decode_invalid_char() {
        let result = base64_decode_strip("Zm@v");
        assert_eq!(result, None);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_base64_decode_all_chars() {
        // Full alphabet: every base64 char used, producing a known byte sequence.
        let all = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0NTY3ODkrLw==";
        let result = base64_decode_strip(all);
        assert!(result.is_some());
    }

    // ── PEM parsing tests ──────────────────────────────────────────────────

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_parse_pem_single_cert() {
        // A minimal self-signed cert DER encoded as:
        //   30 03 02 01 01
        // Base64: "MAMCAQE="
        let pem = "-----BEGIN CERTIFICATE-----\nMAMCAQE=\n-----END CERTIFICATE-----\n";
        let certs = parse_pem_certs(pem);
        assert_eq!(certs.len(), 1);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_parse_pem_multiple_certs() {
        let pem = "\
-----BEGIN CERTIFICATE-----\nMAMCAQE=\n-----END CERTIFICATE-----\n\
-----BEGIN CERTIFICATE-----\nMAMCAQI=\n-----END CERTIFICATE-----\n";
        let certs = parse_pem_certs(pem);
        assert_eq!(certs.len(), 2);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_parse_pem_empty() {
        let certs = parse_pem_certs("");
        assert_eq!(certs.len(), 0);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_parse_pem_no_cert_markers() {
        let pem = "just some random text\nwith multiple\nlines\n";
        let certs = parse_pem_certs(pem);
        assert_eq!(certs.len(), 0);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_parse_pem_with_whitespace_in_b64() {
        // Space-separated base64 chunks (common in real PEM files).
        let pem = "-----BEGIN CERTIFICATE-----\nMAMC AQE=\n-----END CERTIFICATE-----\n";
        let certs = parse_pem_certs(pem);
        assert_eq!(certs.len(), 1);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn test_parse_pem_skips_invalid_cert() {
        let pem = "\
-----BEGIN CERTIFICATE-----\n!!!invalid!!!\n-----END CERTIFICATE-----\n\
-----BEGIN CERTIFICATE-----\nMAMCAQE=\n-----END CERTIFICATE-----\n";
        let certs = parse_pem_certs(pem);
        assert_eq!(certs.len(), 1); // only the valid one
    }

    // ── make_tls_config tests ───────────────────────────────────────────────

    #[test]
    fn test_unsafe_cert_mode_skips_cert_loading() {
        let mut config = Config::default();
        config.ignore_unsafe_cert = true;
        config.endpoint = "https://example.com".to_string();
        config.token = "secret".to_string();

        let result = make_tls_config(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_normal_mode_no_certs_on_unsupported_os() {
        let mut config = Config::default();
        config.ignore_unsafe_cert = false;
        config.endpoint = "https://example.com".to_string();
        config.token = "secret".to_string();

        let result = make_tls_config(&config);
        // On platforms without certs (e.g. CI, unsupported OS), we expect NoCertsFound.
        // This test documents the behavior — actual result depends on the test host.
        match result {
            Ok(_) => {}                     // Platform has native certs
            Err(TlsErr::NoCertsFound) => {} // Expected on minimal systems
            Err(e) => panic!("unexpected error: {}", e),
        }
    }

    // ── TlsErr trait tests ──────────────────────────────────────────────────

    #[test]
    fn test_tls_err_display() {
        let io_err = TlsErr::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "test"));
        assert!(io_err.to_string().contains("test"));

        let no_certs = TlsErr::NoCertsFound;
        assert!(no_certs.to_string().contains("no root certificates"));
    }

    #[test]
    fn test_tls_err_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let tls_err: TlsErr = io_err.into();
        assert!(matches!(tls_err, TlsErr::Io(_)));
    }
}
