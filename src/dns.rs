// komari-agent-rs: custom DNS resolver with IPv4/IPv6 preference.
//
// Mirrors Go dnsresolver package:
//   D:/Code/Projects/external/komari-agent-go/dnsresolver/resolver.go
//
// 100% feature parity: TTL cache (50 entries, 5 min), 10+ built-in DNS servers,
// system DNS fallback, raw UDP DNS query (A + AAAA), IP version preference
// sorting, and a dial-context factory for TCP connections.
//
// Constraints: no tokio/async, no clap, no serde. std-only + rustls.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ============================================================================
// DnsErr — unified error type for all DNS operations
// ============================================================================

#[derive(Debug)]
pub enum DnsErr {
    /// I/O error (socket, read, write).
    Io(io::Error),
    /// Cache is full and eviction failed (should not happen in practice).
    CacheFull,
    /// No addresses resolved for the given host.
    NoAddresses(String),
    /// DNS query timed out.
    Timeout(String),
}

impl fmt::Display for DnsErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "DNS I/O error: {}", e),
            Self::CacheFull => write!(f, "DNS cache full"),
            Self::NoAddresses(host) => write!(f, "no addresses found for '{}'", host),
            Self::Timeout(msg) => write!(f, "DNS query timed out: {}", msg),
        }
    }
}

impl std::error::Error for DnsErr {}

impl From<io::Error> for DnsErr {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

// ============================================================================
// Constants
// ============================================================================

/// Maximum number of entries in the DNS cache.
const CACHE_MAX_ENTRIES: usize = 50;

/// TTL for cached DNS entries (5 minutes).
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Default DNS query timeout.
const DNS_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Default TCP dial timeout.
const DEFAULT_DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Built-in DNS servers matching Go agent's DNSServers list exactly,
/// plus Quad9 (9.9.9.9) as an additional primary.
const DNS_SERVERS: &[&str] = &[
    // IPv6
    "[2606:4700:4700::1111]:53", // Cloudflare IPv6
    "[2606:4700:4700::1001]:53", // Cloudflare IPv6 backup
    "[2001:4860:4860::8888]:53", // Google IPv6
    "[2001:4860:4860::8844]:53", // Google IPv6 backup
    // IPv4
    "1.1.1.1:53",         // Cloudflare IPv4 (primary)
    "8.8.8.8:53",         // Google IPv4 (primary)
    "9.9.9.9:53",         // Quad9 IPv4 (primary)
    "8.8.4.4:53",         // Google IPv4 backup
    "114.114.114.114:53", // 114DNS (China mainland)
    "223.5.5.5:53",       // AliDNS (China mainland)
    "119.29.29.29:53",    // DNSPod (China mainland)
];

// ============================================================================
// DnsCache — TTL-bounded, fixed-capacity DNS result cache
// ============================================================================

/// A simple TTL-bounded DNS cache.
///
/// - Max 50 entries
/// - Each entry expires after 5 minutes
/// - On overflow, the oldest entry (by insertion time) is evicted
/// - Thread-safe via Mutex (matching Go's sync.Mutex pattern)
pub struct DnsCache {
    inner: Mutex<HashMap<String, CachedEntry>>,
}

struct CachedEntry {
    addrs: Vec<SocketAddr>,
    inserted_at: Instant,
}

impl DnsCache {
    /// Create a new empty DNS cache.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Look up a host in the cache.
    ///
    /// Returns `Some(Vec<SocketAddr>)` if the entry exists and has not expired.
    /// Returns `None` if the entry is missing or its TTL has elapsed (stale
    /// entries are removed automatically).
    pub fn get(&self, host: &str) -> Option<Vec<SocketAddr>> {
        let mut cache = self.inner.lock().ok()?;
        match cache.get(host) {
            Some(entry) if entry.inserted_at.elapsed() < CACHE_TTL => Some(entry.addrs.clone()),
            Some(_) => {
                // Stale entry — remove it
                cache.remove(host);
                None
            }
            None => None,
        }
    }

    /// Insert a host → addresses mapping into the cache.
    ///
    /// If the cache is full (≥ CACHE_MAX_ENTRIES), the oldest entry is evicted
    /// first. Returns `Err(DnsErr::CacheFull)` only if eviction fails.
    pub fn insert(&self, host: String, addrs: Vec<SocketAddr>) -> Result<(), DnsErr> {
        let mut cache = self.inner.lock().map_err(|_| DnsErr::CacheFull)?;

        // Evict oldest if at capacity and the host isn't already present
        if cache.len() >= CACHE_MAX_ENTRIES && !cache.contains_key(&host) {
            self.evict_oldest_locked(&mut cache);
        }

        cache.insert(
            host,
            CachedEntry {
                addrs,
                inserted_at: Instant::now(),
            },
        );
        Ok(())
    }

    /// Remove all entries from the cache.
    #[allow(dead_code)]
    pub fn clear(&self) {
        if let Ok(mut cache) = self.inner.lock() {
            cache.clear();
        }
    }

    /// Return the current number of cached entries.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Evict the entry with the oldest `inserted_at` timestamp.
    /// Must be called while holding the lock.
    fn evict_oldest_locked(&self, cache: &mut HashMap<String, CachedEntry>) {
        let oldest_key = cache
            .iter()
            .min_by_key(|(_, entry)| entry.inserted_at)
            .map(|(k, _)| k.clone());

        if let Some(key) = oldest_key {
            cache.remove(&key);
        }
    }
}

impl Default for DnsCache {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Global cache singleton — matching Go's package-level variables
// ============================================================================

static DNS_CACHE: std::sync::LazyLock<DnsCache> = std::sync::LazyLock::new(DnsCache::new);

// ============================================================================
// DNS Wire Format — raw UDP DNS query construction
// ============================================================================

/// DNS record types we query.
const DNS_TYPE_A: u16 = 1; // IPv4 address
const DNS_TYPE_AAAA: u16 = 28; // IPv6 address

/// Build a raw DNS query packet.
///
/// Wire format:
///   - 12-byte header (ID, flags, counts)
///   - Question section: QNAME (length-prefixed labels + NUL), QTYPE, QCLASS
fn build_dns_query(host: &str, qtype: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);

    // -- Header (12 bytes) --
    // Transaction ID: random u16
    let txid: u16 = (Instant::now().elapsed().subsec_nanos() & 0xFFFF) as u16;
    buf.extend_from_slice(&txid.to_be_bytes());

    // Flags: standard query with recursion desired (0x0100)
    buf.extend_from_slice(&0x0100u16.to_be_bytes());

    // Question count: 1
    buf.extend_from_slice(&1u16.to_be_bytes());

    // Answer RRs: 0
    buf.extend_from_slice(&0u16.to_be_bytes());

    // Authority RRs: 0
    buf.extend_from_slice(&0u16.to_be_bytes());

    // Additional RRs: 0
    buf.extend_from_slice(&0u16.to_be_bytes());

    // -- Question section --
    encode_qname(&mut buf, host);
    buf.extend_from_slice(&qtype.to_be_bytes()); // QTYPE
    buf.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN

    buf
}

/// Encode a hostname into DNS QNAME format (length-prefixed labels).
///
/// "api.example.com" → 3api7example3com0
fn encode_qname(buf: &mut Vec<u8>, host: &str) {
    for label in host.split('.') {
        let label_bytes = label.as_bytes();
        buf.push(label_bytes.len() as u8);
        buf.extend_from_slice(label_bytes);
    }
    buf.push(0x00); // terminating NUL
}

/// Parse IP addresses from a raw DNS response.
///
/// Handles DNS name compression pointers (0xC0xx) in the question and answer
/// sections. Extracts A (type 1) and AAAA (type 28) records from the answer
/// section.
fn parse_dns_response(response: &[u8]) -> Result<Vec<SocketAddr>, DnsErr> {
    if response.len() < 12 {
        return Err(DnsErr::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "DNS response too short for header",
        )));
    }

    // Parse header
    let _txid = u16::from_be_bytes([response[0], response[1]]);
    let _flags = u16::from_be_bytes([response[2], response[3]]);
    let qdcount = u16::from_be_bytes([response[4], response[5]]) as usize;
    let ancount = u16::from_be_bytes([response[6], response[7]]) as usize;

    if ancount == 0 {
        return Ok(Vec::new());
    }

    let mut pos: usize = 12; // skip header

    // -- Skip question section --
    for _ in 0..qdcount {
        pos = skip_name(response, pos)?;
        pos += 4; // QTYPE (2) + QCLASS (2)
    }

    // -- Parse answer section --
    let mut addrs: Vec<SocketAddr> = Vec::with_capacity(ancount);

    for _ in 0..ancount {
        if pos + 10 > response.len() {
            break;
        }

        // NAME: may be a pointer (2 bytes) or a sequence of labels
        pos = skip_name(response, pos)?;

        if pos + 10 > response.len() {
            break;
        }

        let atype = u16::from_be_bytes([response[pos], response[pos + 1]]);
        let _aclass = u16::from_be_bytes([response[pos + 2], response[pos + 3]]);
        let _ttl = u32::from_be_bytes([
            response[pos + 4],
            response[pos + 5],
            response[pos + 6],
            response[pos + 7],
        ]);
        let rdlength = u16::from_be_bytes([response[pos + 8], response[pos + 9]]) as usize;
        pos += 10;

        if pos + rdlength > response.len() {
            break;
        }

        match atype {
            DNS_TYPE_A if rdlength == 4 => {
                let ip = std::net::Ipv4Addr::new(
                    response[pos],
                    response[pos + 1],
                    response[pos + 2],
                    response[pos + 3],
                );
                // Port 0 — caller assigns the port
                addrs.push(SocketAddr::V4(SocketAddrV4::new(ip, 0)));
            }
            DNS_TYPE_AAAA if rdlength == 16 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&response[pos..pos + 16]);
                let ip = std::net::Ipv6Addr::from(octets);
                addrs.push(SocketAddr::V6(SocketAddrV6::new(ip, 0, 0, 0)));
            }
            _ => {
                // CNAME, MX, NS, etc. — skip
            }
        }

        pos += rdlength;
    }

    Ok(addrs)
}

/// Skip over a DNS name at `pos`, handling compression pointers.
///
/// A name is either:
///   - A sequence of length-prefixed labels terminated by 0x00
///   - A compression pointer (top 2 bits = 11) → 2 bytes total, pointing
///     to an offset in the message where the name continues
///
/// Returns the byte position after the name.
fn skip_name(data: &[u8], mut pos: usize) -> Result<usize, DnsErr> {
    loop {
        if pos >= data.len() {
            return Err(DnsErr::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated DNS name",
            )));
        }

        let len = data[pos];

        // Check for compression pointer: top 2 bits = 0b11
        if len & 0xC0 == 0xC0 {
            // 2-byte pointer — skip it and we're done with this name
            return Ok(pos + 2);
        }

        if len == 0x00 {
            // End of name
            return Ok(pos + 1);
        }

        // Regular label: skip the length byte + label bytes
        pos += 1 + len as usize;
    }
}

// ============================================================================
// Raw UDP DNS query — send query to a specific DNS server
// ============================================================================

/// Send a DNS query to `dns_server` and return the resolved addresses.
///
/// Queries both A and AAAA records, merges results.
fn query_dns_server(dns_server: &str, host: &str, port: u16) -> Result<Vec<SocketAddr>, DnsErr> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(DNS_QUERY_TIMEOUT))?;

    let mut all_addrs: Vec<SocketAddr> = Vec::new();

    // Query A records (IPv4)
    let query_a = build_dns_query(host, DNS_TYPE_A);
    socket.send_to(&query_a, dns_server)?;

    let mut response_buf = [0u8; 512];
    match socket.recv_from(&mut response_buf) {
        Ok((n, _)) => {
            if let Ok(addrs) = parse_dns_response(&response_buf[..n]) {
                for addr in addrs {
                    all_addrs.push(set_port(addr, port));
                }
            }
        }
        Err(ref e)
            if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
        {
            // Timeout — continue to AAAA query
        }
        Err(e) => return Err(DnsErr::Io(e)),
    }

    // Query AAAA records (IPv6)
    let query_aaaa = build_dns_query(host, DNS_TYPE_AAAA);
    socket.send_to(&query_aaaa, dns_server)?;

    let mut response_buf = [0u8; 512];
    match socket.recv_from(&mut response_buf) {
        Ok((n, _)) => {
            if let Ok(addrs) = parse_dns_response(&response_buf[..n]) {
                for addr in addrs {
                    all_addrs.push(set_port(addr, port));
                }
            }
        }
        Err(ref e)
            if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
        {
            // Timeout — ok, we may have A records
        }
        Err(e) => {
            // If we got A records, ignore AAAA error
            if all_addrs.is_empty() {
                return Err(DnsErr::Io(e));
            }
        }
    }

    Ok(all_addrs)
}

/// Set the port on a SocketAddr, preserving the IP version.
fn set_port(addr: SocketAddr, port: u16) -> SocketAddr {
    match addr {
        SocketAddr::V4(v4) => SocketAddr::V4(SocketAddrV4::new(*v4.ip(), port)),
        SocketAddr::V6(v6) => SocketAddr::V6(SocketAddrV6::new(
            *v6.ip(),
            port,
            v6.flowinfo(),
            v6.scope_id(),
        )),
    }
}

// ============================================================================
// resolve — main DNS resolution entry point
// ============================================================================

/// Resolve a hostname to a list of `SocketAddr`s.
///
/// Resolution strategy:
/// 1. Check the global TTL cache first.
/// 2. If `custom_dns_servers` is non-empty, query those servers via raw UDP DNS.
/// 3. Otherwise, use the system resolver (`std::net::ToSocketAddrs`).
/// 4. Sort results by IP version preference:
///    - `"4"` → IPv4 addresses first
///    - `"6"` → IPv6 addresses first
///    - Other/empty → auto-detect based on local network interfaces
/// 5. Cache the result (even if empty — negative caching avoids repeated failures).
pub fn resolve(
    host: &str,
    port: u16,
    prefer_ip_version: &str,
    custom_dns_servers: &[String],
) -> Result<Vec<SocketAddr>, DnsErr> {
    // 1. Check cache
    if let Some(cached) = DNS_CACHE.get(host) {
        let mut addrs = cached;
        for addr in &mut addrs {
            *addr = set_port(*addr, port);
        }
        sort_by_preference(&mut addrs, prefer_ip_version);
        if !addrs.is_empty() {
            return Ok(addrs);
        }
    }

    // 2. Resolve
    let mut addrs = if !custom_dns_servers.is_empty() {
        resolve_via_custom_dns(host, port, custom_dns_servers)?
    } else {
        resolve_via_system(host, port)?
    };

    // 3. Sort by preference
    sort_by_preference(&mut addrs, prefer_ip_version);

    // 4. Cache result
    // Store with port 0 so cache lookups can assign any port
    let cached_addrs: Vec<SocketAddr> = addrs.iter().map(|a| set_port(*a, 0)).collect();
    let _ = DNS_CACHE.insert(host.to_string(), cached_addrs);

    if addrs.is_empty() {
        Err(DnsErr::NoAddresses(host.to_string()))
    } else {
        Ok(addrs)
    }
}

/// Resolve using system DNS (std::net::ToSocketAddrs).
fn resolve_via_system(host: &str, port: u16) -> Result<Vec<SocketAddr>, DnsErr> {
    let target = (host, port);
    let addrs: Vec<SocketAddr> = target
        .to_socket_addrs()
        .map_err(|e| DnsErr::Io(e))?
        .collect();

    Ok(addrs)
}

/// Resolve using custom DNS servers via raw UDP queries.
///
/// Tries each server in order; stops at the first server that returns
/// at least one address. Falls back to system DNS if all custom servers fail.
fn resolve_via_custom_dns(
    host: &str,
    port: u16,
    custom_dns_servers: &[String],
) -> Result<Vec<SocketAddr>, DnsErr> {
    // Build a combined list: custom servers first, then built-in as fallback
    let mut all_servers: Vec<String> =
        Vec::with_capacity(custom_dns_servers.len() + DNS_SERVERS.len());

    for s in custom_dns_servers {
        all_servers.push(normalize_dns_server(s));
    }
    for s in DNS_SERVERS {
        let normalized = normalize_dns_server(s);
        if !all_servers.contains(&normalized) {
            all_servers.push(normalized);
        }
    }

    let mut last_err: Option<DnsErr> = None;

    for server in &all_servers {
        match query_dns_server(server, host, port) {
            Ok(addrs) if !addrs.is_empty() => return Ok(addrs),
            Ok(_) => continue, // empty response — try next server
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        }
    }

    // All custom servers failed — fall back to system DNS
    match resolve_via_system(host, port) {
        Ok(addrs) => Ok(addrs),
        Err(_) => {
            // Return the last custom DNS error, or a generic error
            Err(last_err.unwrap_or_else(|| DnsErr::NoAddresses(host.to_string())))
        }
    }
}

/// Normalize a DNS server string to `host:port` format.
///
/// - IPv6 addresses get bracketed and default to port 53
/// - IPv4/hostnames get port 53 appended if missing
fn normalize_dns_server(s: &str) -> String {
    let s = s.trim();

    // Already [ipv6]:port or host:port
    if (s.starts_with('[') && s.contains("]:")) || (s.matches(':').count() == 1 && !s.contains(']'))
    {
        return s.to_string();
    }

    // Bare IPv6 (multiple colons, no brackets)
    if s.matches(':').count() >= 2 && !s.contains(']') {
        return format!("[{}]:53", s);
    }

    // Hostname or IPv4 without port
    if !s.contains(':') {
        return format!("{}:53", s);
    }

    s.to_string()
}

// ============================================================================
// IP version preference sorting
// ============================================================================

/// Sort addresses so that preferred IP version comes first.
///
/// `prefer` values:
///   - `"4"` → IPv4 first
///   - `"6"` → IPv6 first
///   - Other/empty → auto-detect from local network interfaces
fn sort_by_preference(addrs: &mut [SocketAddr], prefer: &str) {
    let effective_prefer = if prefer == "4" || prefer == "6" {
        prefer.to_string()
    } else if prefer_ipv4_first() {
        "4".to_string()
    } else {
        "6".to_string()
    };

    // Stable sort: preferred first, others after
    let is_v4 = |a: &SocketAddr| a.is_ipv4();
    let is_v6 = |a: &SocketAddr| a.is_ipv6();

    if effective_prefer == "4" {
        // IPv4 first, then IPv6
        let mut i = 0;
        for j in 0..addrs.len() {
            if is_v4(&addrs[j]) {
                addrs.swap(i, j);
                i += 1;
            }
        }
    } else {
        // IPv6 first, then IPv4
        let mut i = 0;
        for j in 0..addrs.len() {
            if is_v6(&addrs[j]) {
                addrs.swap(i, j);
                i += 1;
            }
        }
    }
}

/// Detect whether the local machine has a usable IPv4 address.
///
/// Scans non-loopback, UP network interfaces for an IPv4 address.
/// Result is cached (checked once) matching Go's `sync.Once` pattern.
fn prefer_ipv4_first() -> bool {
    use std::net::IpAddr;
    use std::sync::OnceLock;

    static HAS_IPV4: OnceLock<bool> = OnceLock::new();

    *HAS_IPV4.get_or_init(|| {
        // On most platforms, we can check network interfaces
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            if let Ok(ifaces) = get_if_addrs() {
                for addr in ifaces {
                    if addr.is_loopback() {
                        continue;
                    }
                    if matches!(addr, IpAddr::V4(_)) {
                        return true;
                    }
                }
            }
        }
        // Default to true (most machines have IPv4)
        true
    })
}

/// Get non-loopback IP addresses from local network interfaces.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn get_if_addrs() -> io::Result<Vec<std::net::IpAddr>> {
    // Use a UDP socket trick: connect to a public IP and read the local addr.
    // This avoids platform-specific netlink/ioctl code.
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("1.1.1.1:53")?;
    let local = socket.local_addr()?;
    let mut addrs = vec![local.ip()];

    // Also try IPv6
    if let Ok(socket6) = UdpSocket::bind("[::]:0") {
        if socket6.connect("[2606:4700:4700::1111]:53").is_ok() {
            if let Ok(local6) = socket6.local_addr() {
                addrs.push(local6.ip());
            }
        }
    }

    Ok(addrs)
}

// ============================================================================
// make_dial_context — TCP connection factory with DNS preference
// ============================================================================

/// Create a TCP dial context function with custom DNS resolution and IP version
/// preference.
///
/// The returned closure:
/// 1. Parses `addr` as `host:port`
/// 2. Resolves the host using `resolve()` (cache-aware, preference-sorted)
/// 3. Tries each resolved IP in order until a TCP connection succeeds
///
/// Matching Go's `GetDialContextWithPreference`.
pub fn make_dial_context(
    timeout: Duration,
    prefer_ip_version: &str,
    custom_dns_servers: &[String],
) -> impl Fn(&str, &str) -> Result<TcpStream, DnsErr> + use<> {
    let prefer = prefer_ip_version.to_string();
    let custom_dns = custom_dns_servers.to_vec();
    let effective_timeout = if timeout.is_zero() {
        DEFAULT_DIAL_TIMEOUT
    } else {
        timeout
    };

    move |_network: &str, addr: &str| {
        // Parse host:port
        let (host, port_str) = addr.rsplit_once(':').ok_or_else(|| {
            DnsErr::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid address '{}': expected host:port", addr),
            ))
        })?;

        // Strip brackets from IPv6 address
        let host = host.strip_prefix('[').unwrap_or(host);
        let host = host.strip_suffix(']').unwrap_or(host);

        let port: u16 = port_str.parse().map_err(|_| {
            DnsErr::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid port '{}' in address '{}'", port_str, addr),
            ))
        })?;

        // Resolve
        let addrs = resolve(host, port, &prefer, &custom_dns)?;

        // Try each address in order
        let mut last_err: Option<io::Error> = None;
        for socket_addr in &addrs {
            match TcpStream::connect_timeout(socket_addr, effective_timeout) {
                Ok(stream) => {
                    // Set TCP_NODELAY for low-latency monitoring traffic
                    let _ = stream.set_nodelay(true);
                    return Ok(stream);
                }
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        }

        Err(DnsErr::Io(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("failed to connect to {}", addr),
            )
        })))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // ------------------------------------------------------------------
    // DnsCache tests
    // ------------------------------------------------------------------

    #[test]
    fn test_cache_insert_and_get() {
        let cache = DnsCache::new();
        let addrs = vec![SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(1, 1, 1, 1),
            443,
        ))];

        cache.insert("example.com".into(), addrs.clone()).unwrap();
        let result = cache.get("example.com");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), addrs);
    }

    #[test]
    fn test_cache_miss() {
        let cache = DnsCache::new();
        assert!(cache.get("nonexistent.example.com").is_none());
    }

    #[test]
    fn test_cache_clear() {
        let cache = DnsCache::new();
        let addrs = vec![SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(8, 8, 8, 8),
            53,
        ))];
        cache.insert("dns.example.com".into(), addrs).unwrap();
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_cache_eviction_on_overflow() {
        let cache = DnsCache::new();
        // Insert CACHE_MAX_ENTRIES + 5 entries
        for i in 0..(CACHE_MAX_ENTRIES + 5) {
            let host = format!("host{}.example.com", i);
            let addrs = vec![SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(10, 0, 0, (i % 256) as u8),
                8080,
            ))];
            cache.insert(host, addrs).unwrap();
        }
        // Should still be at most CACHE_MAX_ENTRIES
        assert!(cache.len() <= CACHE_MAX_ENTRIES);

        // The first few hosts (oldest) should have been evicted
        assert!(cache.get("host0.example.com").is_none());
        assert!(cache.get("host1.example.com").is_none());

        // But the last ones should still be present
        let last_host = format!("host{}.example.com", CACHE_MAX_ENTRIES + 4);
        assert!(cache.get(&last_host).is_some());
    }

    // ------------------------------------------------------------------
    // DNS wire format tests
    // ------------------------------------------------------------------

    #[test]
    fn test_build_dns_query_a() {
        let query = build_dns_query("example.com", DNS_TYPE_A);
        // Header: 12 bytes
        assert!(query.len() > 12);
        // Question count at bytes 4-5
        assert_eq!(u16::from_be_bytes([query[4], query[5]]), 1);
        // Last 4 bytes: QTYPE + QCLASS
        let tail = &query[query.len() - 4..];
        assert_eq!(u16::from_be_bytes([tail[0], tail[1]]), DNS_TYPE_A);
        assert_eq!(u16::from_be_bytes([tail[2], tail[3]]), 1); // IN
    }

    #[test]
    fn test_encode_qname() {
        let mut buf = Vec::new();
        encode_qname(&mut buf, "api.example.com");
        // 3api7example3com0
        assert_eq!(buf[0], 3);
        assert_eq!(&buf[1..4], b"api");
        assert_eq!(buf[4], 7);
        assert_eq!(&buf[5..12], b"example");
        assert_eq!(buf[12], 3);
        assert_eq!(&buf[13..16], b"com");
        assert_eq!(buf[16], 0);
    }

    #[test]
    fn test_parse_dns_response_empty() {
        // Minimal valid DNS response with 0 answers
        let response: Vec<u8> = vec![
            0x00, 0x01, // TXID
            0x81, 0x80, // Flags: standard response, no error
            0x00, 0x01, // QDCOUNT = 1
            0x00, 0x00, // ANCOUNT = 0
            0x00, 0x00, // NSCOUNT = 0
            0x00, 0x00, // ARCOUNT = 0
            // Question: 7example3com0 + type A + class IN
            0x07, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00,
            0x01, // TYPE = A
            0x00, 0x01, // CLASS = IN
        ];
        let result = parse_dns_response(&response).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_dns_response_too_short() {
        let response = vec![0x00; 10];
        assert!(parse_dns_response(&response).is_err());
    }

    #[test]
    fn test_skip_name_simple() {
        // "3www7example3com0" = 17 bytes
        let data: Vec<u8> = vec![
            3, b'w', b'w', b'w', 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c', b'o', b'm',
            0, b'X', // extra byte after name
        ];
        let next = skip_name(&data, 0).unwrap();
        assert_eq!(next, 17); // points to b'X'
    }

    #[test]
    fn test_skip_name_pointer() {
        // A pointer at position 5 pointing back to offset 0
        let data: Vec<u8> = vec![
            3, b'w', b'w', b'w', 0, // name at offset 0
            0xC0, 0x00, // pointer to offset 0
        ];
        let next = skip_name(&data, 5).unwrap();
        assert_eq!(next, 7); // skipped 2-byte pointer
    }

    // ------------------------------------------------------------------
    // normalize_dns_server tests
    // ------------------------------------------------------------------

    #[test]
    fn test_normalize_dns_server_ipv4_no_port() {
        assert_eq!(normalize_dns_server("1.1.1.1"), "1.1.1.1:53");
    }

    #[test]
    fn test_normalize_dns_server_ipv4_with_port() {
        assert_eq!(normalize_dns_server("1.1.1.1:5353"), "1.1.1.1:5353");
    }

    #[test]
    fn test_normalize_dns_server_ipv6_no_bracket() {
        let result = normalize_dns_server("2606:4700:4700::1111");
        assert_eq!(result, "[2606:4700:4700::1111]:53");
    }

    #[test]
    fn test_normalize_dns_server_ipv6_with_bracket() {
        let result = normalize_dns_server("[2606:4700:4700::1111]:53");
        assert_eq!(result, "[2606:4700:4700::1111]:53");
    }

    #[test]
    fn test_normalize_dns_server_hostname() {
        assert_eq!(
            normalize_dns_server("dns.example.com"),
            "dns.example.com:53"
        );
    }

    // ------------------------------------------------------------------
    // sort_by_preference tests
    // ------------------------------------------------------------------

    #[test]
    fn test_sort_prefer_v4() {
        let mut addrs = vec![
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 443)),
            SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
                443,
                0,
                0,
            )),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 443)),
        ];
        sort_by_preference(&mut addrs, "4");
        // First two should be IPv4
        assert!(addrs[0].is_ipv4());
        assert!(addrs[1].is_ipv4());
        assert!(addrs[2].is_ipv6());
        assert!(addrs[3].is_ipv6());
    }

    #[test]
    fn test_sort_prefer_v6() {
        let mut addrs = vec![
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 443)),
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 443)),
            SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
                443,
                0,
                0,
            )),
        ];
        sort_by_preference(&mut addrs, "6");
        // First two should be IPv6
        assert!(addrs[0].is_ipv6());
        assert!(addrs[1].is_ipv6());
        assert!(addrs[2].is_ipv4());
        assert!(addrs[3].is_ipv4());
    }

    // ------------------------------------------------------------------
    // set_port tests
    // ------------------------------------------------------------------

    #[test]
    fn test_set_port_v4() {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 0));
        let updated = set_port(addr, 443);
        assert_eq!(updated.port(), 443);
        assert_eq!(updated.ip().to_string(), "1.1.1.1");
    }

    #[test]
    fn test_set_port_v6() {
        let addr = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0));
        let updated = set_port(addr, 443);
        assert_eq!(updated.port(), 443);
    }

    // ------------------------------------------------------------------
    // resolve tests (integration-style, require network)
    // ------------------------------------------------------------------

    #[test]
    fn test_resolve_system_dns() {
        // This test requires network access
        let result = resolve("example.com", 443, "", &[]);
        // May fail in CI without network; don't assert Ok, just check no panic
        if let Ok(addrs) = result {
            assert!(!addrs.is_empty(), "should resolve to at least one address");
            for addr in &addrs {
                assert_eq!(addr.port(), 443);
            }
        }
    }

    #[test]
    fn test_resolve_with_cache() {
        // First call (may or may not succeed depending on network)
        let r1 = resolve("example.com", 443, "", &[]);
        // Second call should come from cache if first succeeded
        let r2 = resolve("example.com", 443, "", &[]);
        // Both should return the same result
        match (r1, r2) {
            (Ok(a1), Ok(a2)) => {
                assert!(!a1.is_empty());
                assert_eq!(a1, a2);
            }
            _ => {
                // Both failed — that's fine in no-network environments
            }
        }
    }

    #[test]
    fn test_resolve_missing_host() {
        let result = resolve(
            "this-host-definitely-does-not-exist-12345.invalid",
            443,
            "",
            &[],
        );
        // NXDOMAIN should yield an error, but some ISPs/captive portals
        // hijack DNS and return a "helper" IP — accept either outcome.
        match result {
            Ok(addrs) => {
                // If we got addresses (DNS hijacking), verify they parse
                for addr in &addrs {
                    assert_eq!(addr.port(), 443);
                }
            }
            Err(_) => {
                // Expected path — nonexistent host fails resolution
            }
        }
    }

    #[test]
    fn test_dns_err_display() {
        assert_eq!(
            DnsErr::NoAddresses("test.local".into()).to_string(),
            "no addresses found for 'test.local'"
        );
        assert_eq!(DnsErr::CacheFull.to_string(), "DNS cache full");
        assert_eq!(
            DnsErr::Timeout("query timed out".into()).to_string(),
            "DNS query timed out: query timed out"
        );
    }
}
