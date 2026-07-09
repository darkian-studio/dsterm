use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IncomingMsg {
    #[serde(rename = "ping")]
    Ping { id: Option<String> },
    #[serde(rename = "terminal:create")]
    TerminalCreate {
        id: Option<String>,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
    },
    #[serde(rename = "terminal:data")]
    TerminalData {
        id: Option<String>,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        data: String,
    },
    #[serde(rename = "terminal:resize")]
    TerminalResize {
        id: Option<String>,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal:close")]
    TerminalClose {
        id: Option<String>,
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    #[serde(rename = "terminal:list")]
    TerminalList { id: Option<String> },
    #[serde(rename = "fs:read")]
    FsRead { id: Option<String>, path: String },
    #[serde(rename = "fs:write")]
    FsWrite {
        id: Option<String>,
        path: String,
        content: String,
        encoding: Option<String>,
    },
    #[serde(rename = "fs:mkdir")]
    FsMkdir { id: Option<String>, path: String },
    #[serde(rename = "fs:delete")]
    FsDelete {
        id: Option<String>,
        path: String,
        recursive: Option<bool>,
    },
    #[serde(rename = "fs:rename")]
    FsRename {
        id: Option<String>,
        from: String,
        to: String,
    },
    #[serde(rename = "fs:stat")]
    FsStat { id: Option<String>, path: String },
    #[serde(rename = "project:file-search")]
    ProjectFileSearch {
        id: Option<String>,
        query: String,
        limit: Option<usize>,
    },
    #[serde(rename = "sysmon:get")]
    SysmonGet { id: Option<String> },
    #[serde(rename = "ports:list")]
    PortsList { id: Option<String> },
    #[serde(rename = "ports:kill")]
    PortsKill { id: Option<String>, port: u16 },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OutgoingMsg {
    #[serde(rename = "pong")]
    Pong {
        id: Option<String>,
        #[serde(rename = "respTo")]
        resp_to: Option<String>,
    },
    #[serde(rename = "error")]
    Error {
        id: Option<String>,
        #[serde(rename = "respTo")]
        resp_to: Option<String>,
        error: String,
    },
    #[serde(rename = "terminal:data")]
    TerminalData {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        data: String,
    },
    #[serde(rename = "result")]
    Result {
        id: Option<String>,
        #[serde(rename = "respTo")]
        resp_to: Option<String>,
        data: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Envelope {
    #[serde(rename = "encrypted")]
    Encrypted {
        id: Option<String>,
        #[serde(rename = "clientId")]
        client_id: String,
        nonce: String,
        ciphertext: String,
    },
    #[serde(rename = "ping")]
    Ping { id: Option<String> },
    #[serde(rename = "pong")]
    Pong { id: Option<String> },
}

pub fn is_plaintext_allowed(msg_type: &str) -> bool {
    msg_type == "ping" || msg_type == "pong" || msg_type.starts_with("session:")
}
