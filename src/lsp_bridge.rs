//! LSP HTTP+WebSocket bridge — process lifecycle owned by dsterm.
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::process::{Child, ChildStdin, ChildStdout};
use serde::{Deserialize, Serialize};

pub struct LspSession {
    pub child: Arc<Mutex<Child>>,
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub stdout: Arc<Mutex<Option<ChildStdout>>>,
    pub pid: u32,
}

pub type LspRegistry = Arc<RwLock<HashMap<String, Arc<LspSession>>>>;

#[derive(Deserialize)]
pub struct LspStartRequest {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

#[derive(Serialize)]
pub struct LspStartResponse {
    pub id: String,
    #[serde(rename = "ws_path")]
    pub ws_path: String,
}

#[derive(Deserialize, Default)]
pub struct LspKillRequest {
    pub id: Option<String>,
}

#[derive(Serialize)]
pub struct LspKillResponse {
    pub killed: Vec<String>,
}