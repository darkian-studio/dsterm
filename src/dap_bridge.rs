//! DAP HTTP+WebSocket bridge — process lifecycle owned by dsterm.
use crate::process_bridge::{self, ProcessRegistry};
use axum::Router;

pub type DapRegistry = ProcessRegistry;

pub fn dap_routes() -> Router<DapRegistry> {
    process_bridge::routes("dap")
}
