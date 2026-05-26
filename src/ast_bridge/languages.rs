//! Language ID → tree-sitter grammar + scope kind classification.
//!
//! To add a new language: add one arm to `language_for_id` and one block of
//! arms to `node_kind_to_scope_kind`.

use tree_sitter::Language;

pub fn language_for_id(id: &str) -> Option<Language> {
    match id {
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        "javascript" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        _ => None,
    }
}

pub fn node_kind_to_scope_kind(language_id: &str, node_kind: &str) -> Option<&'static str> {
    match (language_id, node_kind) {
        // Python
        ("python", "function_definition") => Some("function"),
        ("python", "class_definition") => Some("class"),

        // JavaScript / JSX
        ("javascript" | "jsx", "function_declaration") => Some("function"),
        ("javascript" | "jsx", "generator_function_declaration") => Some("function"),
        ("javascript" | "jsx", "class_declaration") => Some("class"),
        ("javascript" | "jsx", "method_definition") => Some("method"),

        // TypeScript / TSX
        ("typescript" | "tsx", "function_declaration") => Some("function"),
        ("typescript" | "tsx", "generator_function_declaration") => Some("function"),
        ("typescript" | "tsx", "class_declaration") => Some("class"),
        ("typescript" | "tsx", "abstract_class_declaration") => Some("class"),
        ("typescript" | "tsx", "method_definition") => Some("method"),
        ("typescript" | "tsx", "abstract_method_signature") => Some("method"),
        ("typescript" | "tsx", "interface_declaration") => Some("interface"),
        ("typescript" | "tsx", "enum_declaration") => Some("enum"),

        _ => None,
    }
}
