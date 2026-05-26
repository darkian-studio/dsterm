//! AST scope chain HTTP endpoint backed by tree-sitter.
//!
//! Single endpoint: `POST /ast/scope`. The request carries the full document
//! content along with a monotonic `version`; identical-version repeat calls
//! hit a 256-entry LRU cache and skip parsing entirely.

mod cache;
mod languages;
pub mod types;
mod walker;

use crate::ast_bridge::cache::{CachedDocument, DocumentCache};
use crate::ast_bridge::languages::language_for_id;
use crate::ast_bridge::types::{AstScopeRequest, AstScopeResponse};
use crate::ast_bridge::walker::scope_chain_at_line;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use std::sync::Arc;
use tree_sitter::Parser;

pub type AstRegistry = Arc<DocumentCache>;

pub fn new_registry() -> AstRegistry {
    Arc::new(DocumentCache::new(256))
}

pub async fn ast_scope(
    State(cache): State<AstRegistry>,
    Json(req): Json<AstScopeRequest>,
) -> impl IntoResponse {
    let language = match language_for_id(&req.language) {
        Some(l) => l,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "unsupported language",
                    "language": req.language,
                })),
            )
                .into_response();
        }
    };

    if let Some(cached) = cache.get(&req.document_id) {
        if cached.version == req.version {
            let scopes = scope_chain_at_line(
                &cached.tree,
                &cached.source,
                &req.language,
                req.line,
            );
            return Json(AstScopeResponse { scopes }).into_response();
        }
    }

    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "failed to set language" })),
        )
            .into_response();
    }

    let source_bytes = req.content.as_bytes().to_vec();
    let tree = match parser.parse(&source_bytes, None) {
        Some(t) => t,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "parse failed" })),
            )
                .into_response();
        }
    };

    let scopes = scope_chain_at_line(&tree, &source_bytes, &req.language, req.line);

    cache.insert(
        req.document_id,
        CachedDocument {
            version: req.version,
            source: source_bytes,
            tree,
        },
    );

    Json(AstScopeResponse { scopes }).into_response()
}

pub fn ast_routes() -> Router<AstRegistry> {
    Router::new().route("/ast/scope", post(ast_scope))
}
