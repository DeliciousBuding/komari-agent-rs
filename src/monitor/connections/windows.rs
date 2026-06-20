// komari-agent-rs: Windows connection counts — GetTcpTable + GetUdpTable
#![cfg(windows)]
#![allow(dead_code)]

use std::io;

#[derive(Debug)]
pub enum MetricErr { Io(io::Error) }
impl From<io::Error> for MetricErr { fn from(e: io::Error) -> Self { MetricErr::Io(e) } }

pub struct ConnectionsInfo { pub tcp: u32, pub udp: u32 }

#[repr(C)]
#[allow(non_snake_case)]
struct MIB_TCPTABLE { dwNumEntries: u32 }
#[repr(C)]
#[allow(non_snake_case)]
struct MIB_UDPTABLE { dwNumEntries: u32 }

#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn GetTcpTable(tcpTable: *mut MIB_TCPTABLE, sizePointer: *mut u32, order: i32) -> u32;
    fn GetUdpTable(udpTable: *mut MIB_UDPTABLE, sizePointer: *mut u32, order: i32) -> u32;
}

const NO_ERROR: u32 = 0;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

fn get_entry_count(table_ptr: *const u8, size: u32) -> u32 {
    if size < 4 { return 0; }
    let buf = unsafe { std::slice::from_raw_parts(table_ptr, size as usize) };
    u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]])
}

pub fn collect_connections() -> Result<ConnectionsInfo, MetricErr> {
    let tcp = {
        let mut size: u32 = 0;
        let ret = unsafe { GetTcpTable(std::ptr::null_mut(), &mut size, 0) };
        if ret != ERROR_INSUFFICIENT_BUFFER || size == 0 { 0 }
        else {
            let mut buf: Vec<u8> = vec![0u8; size as usize];
            let ret = unsafe { GetTcpTable(buf.as_mut_ptr() as *mut MIB_TCPTABLE, &mut size, 0) };
            if ret != NO_ERROR { 0 } else { get_entry_count(buf.as_ptr(), size) }
        }
    };
    let udp = {
        let mut size: u32 = 0;
        let ret = unsafe { GetUdpTable(std::ptr::null_mut(), &mut size, 0) };
        if ret != ERROR_INSUFFICIENT_BUFFER || size == 0 { 0 }
        else {
            let mut buf: Vec<u8> = vec![0u8; size as usize];
            let ret = unsafe { GetUdpTable(buf.as_mut_ptr() as *mut MIB_UDPTABLE, &mut size, 0) };
            if ret != NO_ERROR { 0 } else { get_entry_count(buf.as_ptr(), size) }
        }
    };
    Ok(ConnectionsInfo { tcp, udp })
}
