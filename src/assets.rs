//! Installed-asset discovery (`GET /assets/v1/discover`).
//!
//! Frozen contract: `docs/assets-discovery-v1.md` in the DS repo. DS
//! discovers DS-installed assets through dsterm and caches the result
//! in memory (rebuild on empty, patch on mutation) — no stored
//! records anywhere, so reinstalling DS cannot orphan payloads.
//!
//! dsterm knows the standard layout beneath the given parent dir and
//! nothing else: `<root>/models/**/*.gguf` are models,
//! `<root>/dspacks/*/` dirs holding `manifest.json` are packs.
//! Everything else is ignored. Missing/unreadable roots yield an
//! empty list (unknown is not missing).

use axum::{extract::Query, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Cap on walked filesystem entries per discovery call: a hostile or
/// absurd `$HOME` tree must not turn one request into a full-disk scan.
const MAX_WALK_ENTRIES: usize = 20000;

/// Cap on recursion depth below the root (layout is shallow by design).
const MAX_WALK_DEPTH: usize = 8;

pub fn assets_routes() -> Router {
    Router::new().route("/assets/v1/discover", get(discover))
}

#[derive(Debug, Deserialize)]
struct DiscoverQuery {
    root: Option<String>,
}

#[derive(Debug, Serialize)]
struct DiscoveredAsset {
    id: String,
    kind: &'static str,
    path: String,
    size_bytes: Option<u64>,
    modified_ms: Option<u64>,
}

async fn discover(Query(query): Query<DiscoverQuery>) -> impl IntoResponse {
    let root = match query.root {
        Some(root) if !root.trim().is_empty() => root,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "api": 1,
                    "error": { "code": "invalid_request", "message": "root is required" },
                })),
            )
                .into_response();
        }
    };
    let assets = walk_assets(Path::new(&root));
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "api": 1,
            "root": root,
            "assets": assets,
        })),
    )
        .into_response()
}

fn walk_assets(root: &Path) -> Vec<DiscoveredAsset> {
    let models_dir = root.join("models");
    let packs_dir = root.join("dspacks");
    let mut assets = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH || seen > MAX_WALK_ENTRIES {
            break;
        }
        let read = match std::fs::read_dir(&dir) {
            Ok(read) => read,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                break;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            // Never follow symlinks: same-UID swaps between check and
            // read are out of scope, and cycles would hang the walk.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                // Packs are direct children of `dspacks/` holding a
                // manifest; they are leaves (entries/ holds thousands of
                // files the inventory doesn't need).
                if path.parent() == Some(packs_dir.as_path()) && is_pack_dir(&path) {
                    if let Some(name) = dir_name(&path) {
                        assets.push(DiscoveredAsset {
                            id: name,
                            kind: "pack",
                            path: path.to_string_lossy().into_owned(),
                            size_bytes: None,
                            modified_ms: modified_ms(&path),
                        });
                    }
                    continue;
                }
                // Dot-dirs (staging, trash, caches) are never inventory.
                if dir_name(&path).is_some() {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if is_model_file(&models_dir, &path) {
                let relative = path
                    .strip_prefix(&models_dir)
                    .ok()
                    .map(|relative| {
                        relative
                            .components()
                            .map(|component| component.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .unwrap_or_default();
                if relative.is_empty() {
                    continue;
                }
                assets.push(DiscoveredAsset {
                    id: relative,
                    kind: "model",
                    path: path.to_string_lossy().into_owned(),
                    size_bytes: file_size(&path),
                    modified_ms: modified_ms(&path),
                });
            }
        }
    }
    assets.sort_by(|left, right| left.path.cmp(&right.path));
    assets
}

/// A pack dir holds `manifest.json` directly beneath it.
fn is_pack_dir(path: &Path) -> bool {
    path.join("manifest.json").is_file()
}

/// Model files are `.gguf` payloads under the `models/` subtree only —
/// same-named files elsewhere (caches, staging) are not inventory.
fn is_model_file(models_dir: &Path, path: &Path) -> bool {
    path.starts_with(models_dir)
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
}

/// Final path component as UTF-8 (lossy: inventory only, never opened).
fn dir_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn modified_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn scratch_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("dsterm-assets-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn seed_fixture(root: &Path) {
        let model_dir = root.join("models").join("Org.Model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("file.gguf"), b"gguf-bytes").unwrap();
        std::fs::write(model_dir.join("notes.txt"), b"not a model").unwrap();
        let pack_dir = root.join("dspacks").join("rust-book");
        std::fs::create_dir_all(pack_dir.join("entries")).unwrap();
        std::fs::write(pack_dir.join("manifest.json"), b"{}").unwrap();
        std::fs::write(pack_dir.join("entries").join("a.json"), b"{}").unwrap();
        // Manifest-less dir: not a pack.
        std::fs::create_dir_all(root.join("dspacks").join("empty")).unwrap();
        // Dot-dir content is never inventory.
        std::fs::create_dir_all(root.join(".staging").join("x")).unwrap();
        std::fs::write(root.join(".staging").join("x").join("y.gguf"), b"nope").unwrap();
        // Same-named payload outside models/: not a model.
        std::fs::create_dir_all(root.join("downloads")).unwrap();
        std::fs::write(root.join("downloads").join("z.gguf"), b"nope").unwrap();
    }

    async fn discover_body(root: &str) -> (StatusCode, serde_json::Value) {
        let app = assets_routes();
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/assets/v1/discover?root={}",
                        urlencoding_like(root)
                    ))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// Minimal percent-encoding for absolute Unix paths in tests
    /// (no extra dependency for one query param).
    fn urlencoding_like(path: &str) -> String {
        path.replace('%', "%25").replace(' ', "%20")
    }

    #[tokio::test]
    async fn discovers_models_and_packs_only() {
        let root = scratch_root("basic");
        seed_fixture(&root);
        let (status, body) = discover_body(&root.to_string_lossy()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["api"], 1);
        let assets = body["assets"].as_array().unwrap();
        assert_eq!(assets.len(), 2);
        let model = assets
            .iter()
            .find(|asset| asset["kind"] == "model")
            .unwrap();
        assert_eq!(model["id"], "Org.Model/file.gguf");
        assert!(model["path"]
            .as_str()
            .unwrap()
            .ends_with("models/Org.Model/file.gguf"));
        assert_eq!(model["size_bytes"], 10);
        let pack = assets.iter().find(|asset| asset["kind"] == "pack").unwrap();
        assert_eq!(pack["id"], "rust-book");
        assert!(pack["size_bytes"].is_null());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn missing_root_yields_empty_list() {
        let missing = std::env::temp_dir().join("dsterm-assets-test-nope");
        let _ = std::fs::remove_dir_all(&missing);
        let (status, body) = discover_body(&missing.to_string_lossy()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["assets"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn missing_root_param_is_rejected() {
        let app = assets_routes();
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/assets/v1/discover")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
