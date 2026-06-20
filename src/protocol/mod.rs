//! Komari wire protocol: JSON-RPC 2.0 types and v1 flat-report backward
//! compatibility.
//!
//! v2 wraps monitoring data in a JSON-RPC 2.0 envelope;
//! v1 sends raw flat JSON for older `/api/clients/report` endpoints.

pub mod v1;
pub mod v2;
