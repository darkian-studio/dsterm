//! LSP HTTP+WebSocket bridge — process lifecycle owned by dsterm.
use crate::process_bridge::{self, ProcessRegistry};
use axum::Router;

pub type LspRegistry = ProcessRegistry;

pub fn lsp_routes() -> Router<LspRegistry> {
    process_bridge::routes("lsp")
}
