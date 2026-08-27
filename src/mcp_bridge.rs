//! MCP HTTP+WebSocket bridge — process lifecycle owned by dsterm.
use crate::process_bridge::{self, ProcessRegistry};
use axum::Router;

pub type McpRegistry = ProcessRegistry;

pub fn mcp_routes() -> Router<McpRegistry> {
    process_bridge::routes("mcp")
}
