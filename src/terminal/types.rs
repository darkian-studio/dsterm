use super::handlers::TerminalSession;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const MAX_SCROLLBACK_BYTES: usize = 262_144; // 256 KB

#[derive(Deserialize)]
pub struct TerminalOptions {
    pub cols: serde_json::Value,
    pub rows: serde_json::Value,
}

#[derive(Deserialize)]
pub struct ExecuteCommandOption {
    pub command: String,
    pub cwd: Option<String>,
    pub u_cwd: Option<String>,
}

#[derive(Serialize)]
pub struct CommandResponse {
    pub output: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct ProcessExitMessage {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub message: String,
}

#[derive(Deserialize)]
pub struct SilentExecRequest {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub msg_type: String,
    pub id: String,
    pub command: String,
    pub cwd: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "timeout_ms")]
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct SilentExecResponse {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub id: String,
    pub success: bool,
    #[serde(rename = "exit_code")]
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(rename = "timed_out")]
    pub timed_out: bool,
}

#[derive(Deserialize)]
pub struct SilentExecStreamRequest {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub msg_type: String,
    pub id: String,
    pub command: String,
    pub cwd: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "timeout_ms")]
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct SilentExecChunk {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub id: String,
    pub stream: String,
    pub data: String,
}

#[derive(Serialize)]
pub struct SilentExecDone {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub id: String,
    #[serde(rename = "exit_code")]
    pub exit_code: i32,
    #[serde(rename = "timed_out")]
    pub timed_out: bool,
}

pub type Sessions = Arc<DashMap<u32, TerminalSession>>;
