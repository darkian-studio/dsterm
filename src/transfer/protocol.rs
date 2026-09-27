//! Control frames stay human-readable on the wire: a broken transfer can be
//! diagnosed with netcat instead of a protocol dissector.
//!
//! The sha256 here catches corruption, nothing more. An active middleman can
//! rewrite payload and digest together, so non-loopback trust must come from
//! a PAKE or pinned TLS — never from this digest alone.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRequest {
    pub version: u32,
    pub transfer_id: String,
    pub r#type: String,
    pub name: String,
    pub size: u64,
    pub file_count: u64,
    #[serde(default)]
    pub source_addr: Option<String>,
    #[serde(default)]
    pub dest_hint: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptFrame {
    pub status: String,
    #[serde(default)]
    pub final_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectFrame {
    pub status: String,
    pub reason: String,
    pub message: String,
}

pub mod reason {
    pub const POLICY_REJECTED: &str = "policy_rejected";
    pub const BUSY: &str = "busy";
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    pub const AUTH_FAILED: &str = "auth_failed";
    pub const PATH_REJECTED: &str = "path_rejected";
    pub const RESOURCE_LIMITS_EXCEEDED: &str = "resource_limits_exceeded";
    pub const INSUFFICIENT_DISK_SPACE: &str = "insufficient_disk_space";
    pub const CONFIRMATION_TIMEOUT: &str = "confirmation_timeout";
    pub const DESTINATION_CONFLICT: &str = "destination_conflict";
    pub const INTEGRITY_MISMATCH: &str = "integrity_mismatch";
    pub const CANCELLED: &str = "cancelled";
    pub const MALFORMED_REQUEST: &str = "malformed_request";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryHeader {
    pub path: String,
    #[serde(default)]
    pub mode: u32,
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderComplete {
    pub status: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalFrame {
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[allow(dead_code)]
pub fn reject(reason: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&RejectFrame {
        status: "reject".to_string(),
        reason: reason.to_string(),
        message: message.into(),
    })
    .unwrap()
}

pub fn final_error(reason: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&FinalFrame {
        status: "error".to_string(),
        reason: Some(reason.to_string()),
        message: Some(message.into()),
    })
    .unwrap()
}
