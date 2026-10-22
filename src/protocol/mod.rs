//! Komari v2 wire protocol: JSON-RPC 2.0 over WebSocket or HTTP POST.
//!
//! Both transports use `/api/clients/v2/rpc` (Komari >= 1.5).

pub mod fsm;
pub mod v2;
