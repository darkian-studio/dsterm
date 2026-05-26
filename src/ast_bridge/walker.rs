//! Walks a tree-sitter Tree to produce the enclosing scope chain at a line.

use crate::ast_bridge::languages::node_kind_to_scope_kind;
use crate::ast_bridge::types::ScopeEntry;
use tree_sitter::{Node, Point, Tree};

pub fn scope_chain_at_line(
    tree: &Tree,
    source: &[u8],
    language_id: &str,
    line_1based: u32,
) -> Vec<ScopeEntry> {
    if line_1based == 0 {
        return Vec::new();
    }
    let row = (line_1based - 1) as usize;
    let point = Point { row, column: 0 };

    let root = tree.root_node();
    let leaf = match root.descendant_for_point_range(point, point) {
        Some(n) => n,
        None => return Vec::new(),
    };

    let mut chain: Vec<ScopeEntry> = Vec::new();
    let mut current: Option<Node> = Some(leaf);
    while let Some(node) = current {
        if let Some(scope_kind) = node_kind_to_scope_kind(language_id, node.kind()) {
            if let Some(name) = extract_name(node, source) {
                chain.push(ScopeEntry {
                    name,
                    kind: scope_kind.to_string(),
                    start_line: (node.start_position().row as u32) + 1,
                    end_line: (node.end_position().row as u32) + 1,
                });
            }
        }
        current = node.parent();
    }

    chain.reverse();
    chain
}

fn extract_name(node: Node, source: &[u8]) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    name_node.utf8_text(source).ok().map(|s| s.to_string())
}
