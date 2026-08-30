#![allow(dead_code)]

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
    #[serde(rename = "terminal:attach")]
    TerminalAttach {
        id: Option<String>,
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
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
    #[serde(rename = "fs:list")]
    FsList { id: Option<String>, path: String },
    #[serde(rename = "project:file-search")]
    ProjectFileSearch {
        id: Option<String>,
        query: String,
        limit: Option<usize>,
    },
    #[serde(rename = "sysmon:get")]
    SysmonGet { id: Option<String> },
    #[serde(rename = "sysmon:subscribe")]
    SysmonSubscribe { id: Option<String> },
    #[serde(rename = "sysmon:unsubscribe")]
    SysmonUnsubscribe { id: Option<String> },
    #[serde(rename = "ports:list")]
    PortsList { id: Option<String> },
    #[serde(rename = "ports:kill")]
    PortsKill { id: Option<String>, port: u16 },
    #[serde(rename = "exec")]
    Exec {
        id: Option<String>,
        command: String,
        cwd: Option<String>,
        #[serde(rename = "timeout_ms")]
        timeout_ms: Option<u64>,
    },
    #[serde(rename = "http:request")]
    HttpRequest {
        id: Option<String>,
        url: String,
        method: Option<String>,
        headers: Option<std::collections::HashMap<String, String>>,
        body: Option<String>,
    },
    #[serde(rename = "agents:start")]
    AgentsStart {
        id: Option<String>,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        cwd: Option<String>,
        env: Option<std::collections::HashMap<String, String>>,
    },
    #[serde(rename = "agents:input")]
    AgentsInput {
        id: Option<String>,
        #[serde(rename = "agentId")]
        agent_id: String,
        data: String,
    },
    #[serde(rename = "agents:kill")]
    AgentsKill {
        id: Option<String>,
        #[serde(rename = "agentId")]
        agent_id: String,
    },
    #[serde(rename = "ws:open")]
    WsOpen { id: Option<String>, url: String },
    #[serde(rename = "ws:data")]
    WsData {
        id: Option<String>,
        #[serde(rename = "wsId")]
        ws_id: String,
        data: String,
        #[serde(default)]
        binary: bool,
    },
    #[serde(rename = "ws:close")]
    WsClose {
        id: Option<String>,
        #[serde(rename = "wsId")]
        ws_id: String,
    },
    #[serde(rename = "ai:inspect")]
    AiInspect {
        id: Option<String>,
        path: Option<String>,
    },
    #[serde(rename = "ai:load")]
    AiLoad {
        id: Option<String>,
        path: String,
        #[serde(default)]
        args: Vec<String>,
    },
    #[serde(rename = "ai:unload")]
    AiUnload {
        id: Option<String>,
        model_id: String,
    },
    #[serde(rename = "ai:generate")]
    AiGenerate {
        id: Option<String>,
        session_id: String,
        prompt: String,
    },
    #[serde(rename = "ai:complete")]
    AiComplete {
        id: Option<String>,
        prefix: String,
        suffix: Option<String>,
    },
    #[serde(rename = "ai:embed")]
    AiEmbed {
        id: Option<String>,
        texts: Vec<String>,
    },
    #[serde(rename = "ai:list")]
    AiListModels { id: Option<String> },
    #[serde(rename = "ai:health")]
    AiHealth { id: Option<String> },
    #[serde(rename = "ai:capabilities")]
    AiCapabilities { id: Option<String> },
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
    #[serde(rename = "terminal:event")]
    TerminalEvent {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        event: String,
    },
    #[serde(rename = "result")]
    Result {
        id: Option<String>,
        #[serde(rename = "respTo")]
        resp_to: Option<String>,
        data: Value,
    },
    #[serde(rename = "agent:output")]
    AgentOutput {
        #[serde(rename = "agentId")]
        agent_id: String,
        data: String,
    },
    #[serde(rename = "agent:exit")]
    AgentExit {
        #[serde(rename = "agentId")]
        agent_id: String,
    },
    #[serde(rename = "ws:data")]
    WsData {
        #[serde(rename = "wsId")]
        ws_id: String,
        data: String,
        binary: bool,
    },
    #[serde(rename = "ws:close")]
    WsClose {
        #[serde(rename = "wsId")]
        ws_id: String,
    },
    #[serde(rename = "sysmon:update")]
    SysmonUpdate { data: Value },
    #[serde(rename = "ai:result")]
    AiResult {
        id: Option<String>,
        #[serde(rename = "respTo")]
        resp_to: Option<String>,
        data: Value,
    },
    #[serde(rename = "ai:token")]
    AiToken {
        #[serde(rename = "sessionId")]
        session_id: String,
        token: String,
    },
    #[serde(rename = "ai:done")]
    AiDone {
        #[serde(rename = "sessionId")]
        session_id: String,
        usage: Value,
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
    matches!(
        msg_type,
        "ping"
            | "pong"
            | "session:host"
            | "session:hosted"
            | "session:error"
            | "session:client-join"
            | "session:client-left"
            | "session:client-approve"
    )
}
