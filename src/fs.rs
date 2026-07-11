use crate::terminal::get_config;
use axum::{extract::Query, response::IntoResponse, Json};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path as StdPath, PathBuf},
    process::Command,
    time::UNIX_EPOCH,
};

#[derive(Debug, Deserialize)]
pub struct PathQuery {
    path: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct WriteRequest {
    path: String,
    content: String,
    encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MkdirRequest {
    path: String,
}

#[derive(Debug, Deserialize)]
pub struct DeleteRequest {
    path: String,
    recursive: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    from: String,
    to: String,
}

#[derive(Debug, Serialize)]
struct FsError {
    error: String,
}

fn workspace_root() -> anyhow::Result<PathBuf> {
    let configured = get_config().filesystem.workspace_root.clone();
    let root = configured
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    Ok(root.canonicalize()?)
}

fn lexical_normalize(path: &StdPath) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn safe_path(requested: &str) -> anyhow::Result<PathBuf> {
    let root = workspace_root()?;
    let requested = StdPath::new(requested);
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let normalized = lexical_normalize(&joined);
    if normalized.starts_with(&root) {
        Ok(normalized)
    } else {
        anyhow::bail!("Path escapes workspace root")
    }
}

fn fs_error(status: axum::http::StatusCode, error: impl ToString) -> axum::response::Response {
    (
        status,
        Json(FsError {
            error: error.to_string(),
        }),
    )
        .into_response()
}

fn filesystem_disabled_response() -> axum::response::Response {
    fs_error(
        axum::http::StatusCode::FORBIDDEN,
        "Filesystem API is disabled",
    )
}

fn filesystem_enabled() -> bool {
    get_config().filesystem.enabled
}

pub async fn read_file(Query(query): Query<PathQuery>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let path = match safe_path(&query.path) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) => return fs_error(axum::http::StatusCode::NOT_FOUND, e),
    };
    let max = get_config().filesystem.max_read_bytes as u64;
    if metadata.len() > max {
        return fs_error(
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "File exceeds read limit",
        );
    }
    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(e) => return fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let (encoding, content) = if content.contains(&0) {
        ("base64", BASE64.encode(content))
    } else {
        ("utf-8", String::from_utf8_lossy(&content).into_owned())
    };
    Json(serde_json::json!({ "path": query.path, "encoding": encoding, "content": content }))
        .into_response()
}

pub async fn write_file(Json(req): Json<WriteRequest>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let path = match safe_path(&req.path) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    }
    let bytes = if req.encoding.as_deref() == Some("base64") {
        match BASE64.decode(req.content.as_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
        }
    } else {
        req.content.into_bytes()
    };
    match fs::write(path, bytes) {
        Ok(()) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn mkdir(Json(req): Json<MkdirRequest>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let path = match safe_path(&req.path) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    match fs::create_dir_all(path) {
        Ok(()) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn delete(Json(req): Json<DeleteRequest>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let path = match safe_path(&req.path) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    if path
        == match workspace_root() {
            Ok(root) => root,
            Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
        }
    {
        return fs_error(
            axum::http::StatusCode::BAD_REQUEST,
            "Refusing to delete workspace root",
        );
    }
    let result = if path.is_dir() {
        if req.recursive.unwrap_or(false) {
            fs::remove_dir_all(path)
        } else {
            fs::remove_dir(path)
        }
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn rename(Json(req): Json<RenameRequest>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let from = match safe_path(&req.from) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    let to = match safe_path(&req.to) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    match fs::rename(from, to) {
        Ok(()) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn stat(Query(query): Query<PathQuery>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let path = match safe_path(&query.path) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) => return fs_error(axum::http::StatusCode::NOT_FOUND, e),
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|ts| ts.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    Json(serde_json::json!({
        "path": query.path,
        "is_dir": metadata.is_dir(),
        "is_file": metadata.is_file(),
        "len": metadata.len(),
        "modified": modified,
    }))
    .into_response()
}

pub async fn file_search(Query(query): Query<SearchQuery>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let root = match workspace_root() {
        Ok(root) => root,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    let needle = query.query.to_lowercase();
    let limit = query.limit.unwrap_or(100).min(1000);
    let mut stack = vec![root.clone()];
    let mut results = Vec::new();
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name == ".git" || name == "target" {
                continue;
            }
            if name.contains(&needle) {
                if let Ok(relative) = path.strip_prefix(&root) {
                    results.push(relative.to_string_lossy().replace('\\', "/"));
                    if results.len() >= limit {
                        return Json(serde_json::json!({ "results": results })).into_response();
                    }
                }
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    Json(serde_json::json!({ "results": results })).into_response()
}

#[derive(Debug, Serialize)]
struct GitFileEntry {
    status: String,
    path: String,
}

fn run_git(args: &[&str]) -> anyhow::Result<String> {
    let root = workspace_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub async fn git_status() -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let branch = match run_git(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(value) => value.trim().to_string(),
        Err(e) => return fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let porcelain = match run_git(&["status", "--porcelain"]) {
        Ok(value) => value,
        Err(e) => return fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let files = porcelain
        .lines()
        .filter(|line| line.len() > 3)
        .map(|line| GitFileEntry {
            status: line[..2].trim().to_string(),
            path: line[3..].to_string(),
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "branch": branch, "files": files })).into_response()
}

pub async fn list_dir(Query(query): Query<PathQuery>) -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let path = match safe_path(&query.path) {
        Ok(path) => path,
        Err(e) => return fs_error(axum::http::StatusCode::BAD_REQUEST, e),
    };
    let read_dir = match fs::read_dir(&path) {
        Ok(read_dir) => read_dir,
        Err(e) => return fs_error(axum::http::StatusCode::NOT_FOUND, e),
    };
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|ts| ts.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        entries.push(serde_json::json!({
            "name": entry.file_name().to_string_lossy(),
            "is_dir": metadata.is_dir(),
            "is_file": metadata.is_file(),
            "size": metadata.len(),
            "modified": modified,
        }));
    }
    Json(serde_json::json!({ "path": query.path, "entries": entries })).into_response()
}

pub async fn root_info() -> impl IntoResponse {
    if !filesystem_enabled() {
        return filesystem_disabled_response();
    }
    let root = match workspace_root() {
        Ok(root) => root,
        Err(e) => return fs_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let path_display = root.to_string_lossy();
    let cleaned = path_display
        .strip_prefix(r"\\?\")
        .unwrap_or(&path_display)
        .to_string();
    let name = root
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| cleaned.clone());
    Json(serde_json::json!({ "path": cleaned, "name": name })).into_response()
}

pub fn fs_routes() -> axum::Router {
    axum::Router::new()
        .route("/fs/read", axum::routing::get(read_file))
        .route("/fs/write", axum::routing::post(write_file))
        .route("/fs/mkdir", axum::routing::post(mkdir))
        .route("/fs/delete", axum::routing::post(delete))
        .route("/fs/rename", axum::routing::post(rename))
        .route("/fs/stat", axum::routing::get(stat))
        .route("/fs/list", axum::routing::get(list_dir))
        .route("/fs/root", axum::routing::get(root_info))
        .route("/fs/git/status", axum::routing::get(git_status))
        .route("/project/file-search", axum::routing::get(file_search))
}
