//! ACP agent bridge — spawns an AI coding agent as a
//! subprocess and bridges its newline-delimited JSON (NDJSON)
//! stdio to a WebSocket.
use crate::process_bridge::{self, ProcessRegistry};
use axum::Router;

pub type AgentRegistry = ProcessRegistry;

pub fn agent_routes() -> Router<AgentRegistry> {
    process_bridge::routes("agents")
}
