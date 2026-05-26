//! Request and response types for the AST scope endpoint.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct AstScopeRequest {
    pub language: String,
    pub document_id: String,
    pub version: i64,
    pub content: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopeEntry {
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AstScopeResponse {
    pub scopes: Vec<ScopeEntry>,
}
