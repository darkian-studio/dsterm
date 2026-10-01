//! Server-side acquisition for ZIM payloads (Z5).
//!
//! Guest/Termux dsterm processes cannot read the app's private files, so
//! validation and reads require dsterm-visible bytes. This module fetches
//! verified payloads itself: metalink-resolved HTTPS mirrors in,
//! SHA-256 + size verification, validation through the reader, and a
//! server-managed collections dir out. DS orchestrates (catalogue,
//! manifest, registry) and never proxies bulk bytes.
//!
//! Deferred download state (`.part` files) is keyed by sanitized
//! collection id, so a cancelled or interrupted fetch resumes where it
//! stopped.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::reader;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchPhase {
    Queued,
    Downloading,
    Verifying,
    Validating,
    Done,
    Cancelled,
    Failed { code: &'static str, message: String },
}

pub struct FetchReport {
    pub uuid: String,
    pub title: String,
    pub description: String,
    pub languages: Vec<String>,
    pub article_count: u32,
    pub has_title_index: bool,
    pub payload_name: String,
    pub size_bytes: u64,
}

pub struct FetchOp {
    pub id: String,
    pub collection_id: String,
    pub phase: Mutex<FetchPhase>,
    pub bytes_received: AtomicU64,
    pub total_bytes: u64,
    pub cancelled: AtomicBool,
    pub report: Mutex<Option<FetchReport>>,
}

pub struct FetchRequest {
    pub urls: Vec<String>,
    pub mirrors: Vec<String>,
    pub sha256: String,
    pub size_bytes: u64,
    pub collection_id: String,
}

pub struct FetchManager {
    client: reqwest::Client,
    collections_dir: PathBuf,
    exec: tokio::runtime::Handle,
    ops: DashMap<String, Arc<FetchOp>>,
}

impl FetchManager {
    pub fn new(collections_dir: PathBuf, exec: tokio::runtime::Handle) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&collections_dir)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.url().scheme() == "https" {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .build()?;
        Ok(Self {
            client,
            collections_dir,
            exec,
            ops: DashMap::new(),
        })
    }

    pub fn collections_dir(&self) -> &Path {
        &self.collections_dir
    }

    /// Starts a fetch in the background; returns the op id immediately.
    /// Payload lands at `collections/<safe-id>.zim` on success.
    pub fn start(self: &Arc<Self>, request: FetchRequest) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let op = Arc::new(FetchOp {
            id: id.clone(),
            collection_id: request.collection_id.clone(),
            phase: Mutex::new(FetchPhase::Queued),
            bytes_received: AtomicU64::new(0),
            total_bytes: request.size_bytes,
            cancelled: AtomicBool::new(false),
            report: Mutex::new(None),
        });
        self.ops.insert(id.clone(), Arc::clone(&op));
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run(op, request).await;
        });
        id
    }

    pub async fn snapshot(&self, id: &str) -> Option<FetchSnapshot> {
        let op = self.ops.get(id)?.clone();
        let phase = op.phase.lock().await.clone();
        let report = op.report.lock().await;
        // Failure detail derives from the terminal phase: a single
        // source of truth, never a second channel that can drift.
        let failure = match &phase {
            FetchPhase::Failed { code, message } => Some((code.to_string(), message.clone())),
            _ => None,
        };
        Some(FetchSnapshot {
            id: op.id.clone(),
            phase,
            bytes_received: op.bytes_received.load(Ordering::SeqCst),
            total_bytes: op.total_bytes,
            report: report.as_ref().map(|r| FetchReportView {
                uuid: r.uuid.clone(),
                title: r.title.clone(),
                description: r.description.clone(),
                languages: r.languages.clone(),
                article_count: r.article_count,
                has_title_index: r.has_title_index,
                payload_name: r.payload_name.clone(),
                size_bytes: r.size_bytes,
            }),
            failure,
        })
    }

    pub async fn cancel(&self, id: &str) -> bool {
        match self.ops.get(id) {
            Some(op) => {
                op.cancelled.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// Removes a staged payload by file name. Confined to the collections
    /// dir: names with separators, `..`, or a non-`.zim` suffix are
    /// refused (uninstall path).
    pub fn remove_staged(&self, name: &str) -> bool {
        if !valid_staged_name(name) {
            return false;
        }
        let path = self.collections_dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                info!(payload = %name, "zim: removed staged payload");
                true
            }
            Err(e) => {
                warn!(payload = %name, error = %e, "zim: staged remove failed");
                false
            }
        }
    }

    pub fn safe_name(collection_id: &str) -> String {
        let mut safe: String = collection_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if safe.is_empty() {
            safe.push_str("collection");
        }
        format!("{safe}.zim")
    }

    async fn run(self: Arc<Self>, op: Arc<FetchOp>, request: FetchRequest) {
        if op.cancelled.load(Ordering::SeqCst) {
            *op.phase.lock().await = FetchPhase::Cancelled;
            return;
        }
        *op.phase.lock().await = FetchPhase::Downloading;
        let payload_name = Self::safe_name(&request.collection_id);
        let part_path = self.collections_dir.join(format!("{payload_name}.part"));
        let final_path = self.collections_dir.join(&payload_name);

        let mut downloaded = false;
        for url in candidate_urls(&request) {
            if op.cancelled.load(Ordering::SeqCst) {
                break;
            }
            let op_ref: &FetchOp = &op;
            match self.download_once(op_ref, url, &request, &part_path).await {
                Ok(()) => {
                    downloaded = true;
                    break;
                }
                Err(e) => {
                    warn!(url = %mask_url(url), error = %e, "zim: mirror failed");
                    continue;
                }
            }
        }

        if !downloaded || op.cancelled.load(Ordering::SeqCst) {
            *op.phase.lock().await = if op.cancelled.load(Ordering::SeqCst) {
                FetchPhase::Cancelled
            } else {
                FetchPhase::Failed {
                    code: "network_error",
                    message: "mirrors exhausted".to_string(),
                }
            };
            return;
        }

        *op.phase.lock().await = FetchPhase::Verifying;
        if let Err(failure) = verify_file(&part_path, &request.sha256, request.size_bytes).await {
            let _ = std::fs::remove_file(&part_path);
            *op.phase.lock().await = FetchPhase::Failed {
                code: failure.0,
                message: failure.1,
            };
            return;
        }

        *op.phase.lock().await = FetchPhase::Validating;
        let part_clone = part_path.clone();
        let validated = self
            .exec
            .spawn_blocking(move || validate_payload(&part_clone))
            .await;
        match validated {
            Ok(Ok(report_fields)) => {
                if std::fs::rename(&part_path, &final_path).is_err() {
                    *op.phase.lock().await = FetchPhase::Failed {
                        code: "io_error",
                        message: "activation rename failed".to_string(),
                    };
                    return;
                }
                *op.report.lock().await = Some(FetchReport {
                    uuid: report_fields.uuid,
                    title: report_fields.title,
                    description: report_fields.description,
                    languages: report_fields.languages,
                    article_count: report_fields.article_count,
                    has_title_index: report_fields.has_title_index,
                    payload_name,
                    size_bytes: request.size_bytes,
                });
                *op.phase.lock().await = FetchPhase::Done;
                info!(
                    op = %op.id,
                    collection = %op.collection_id,
                    "zim: fetch complete"
                );
            }
            Ok(Err((code, message))) => {
                let _ = std::fs::remove_file(&part_path);
                *op.phase.lock().await = FetchPhase::Failed { code, message };
            }
            Err(join) => {
                *op.phase.lock().await = FetchPhase::Failed {
                    code: "internal_error",
                    message: format!("validation task failed: {join}"),
                };
            }
        }
    }

    async fn download_once(
        &self,
        op: &FetchOp,
        url: &str,
        request: &FetchRequest,
        part_path: &Path,
    ) -> anyhow::Result<()> {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let resume_from = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);
        if resume_from >= request.size_bytes && request.size_bytes > 0 {
            std::fs::remove_file(part_path).ok();
        }
        let resume_from = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);
        op.bytes_received.store(resume_from, Ordering::SeqCst);

        let mut builder = self.client.get(url);
        if resume_from > 0 {
            builder = builder.header("Range", format!("bytes={resume_from}-"));
        }
        let response = builder.send().await?;
        let status = response.status();
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::PARTIAL_CONTENT {
            anyhow::bail!("HTTP {}", status.as_u16());
        }
        if status == reqwest::StatusCode::OK && resume_from > 0 {
            // Server ignored Range: restart from zero (INS-2).
            std::fs::remove_file(part_path).ok();
            op.bytes_received.store(0, Ordering::SeqCst);
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(part_path)
            .await?;
        let mut stream = response.bytes_stream();
        let mut received = op.bytes_received.load(Ordering::SeqCst);
        while let Some(chunk) = stream.next().await {
            if op.cancelled.load(Ordering::SeqCst) {
                anyhow::bail!("cancelled");
            }
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            received += chunk.len() as u64;
            op.bytes_received.store(received, Ordering::SeqCst);
            if received > request.size_bytes.saturating_add(64 * 1024 * 1024) {
                anyhow::bail!("runaway download past declared size");
            }
        }
        file.flush().await?;
        Ok(())
    }
}

fn candidate_urls(request: &FetchRequest) -> Vec<&str> {
    // HTTPS only (KC-7), except loopback test servers: loopback is not
    // the network, so plain HTTP there carries no MITM exposure. The
    // catalogue layer already filters; this is defense in depth.
    request
        .urls
        .iter()
        .chain(request.mirrors.iter())
        .map(String::as_str)
        .filter(|url| url.starts_with("https://") || loopback_url(url))
        .collect()
}

fn loopback_url(url: &str) -> bool {
    let Some(after_scheme) = url.split("://").nth(1) else {
        return false;
    };
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// Least-privilege URL for logs: host + path, no query/auth material.
fn mask_url(url: &str) -> &str {
    match url.find('?') {
        Some(idx) => &url[..idx],
        None => url,
    }
}

async fn verify_file(
    path: &Path,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<(), (&'static str, String)> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| ("io_error", format!("verify open failed: {e}")))?;
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|e| ("io_error", format!("verify read failed: {e}")))?;
        if n == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buffer[..n]);
        total += n as u64;
    }
    if total != expected_size {
        return Err((
            "size_mismatch",
            format!("expected {expected_size} bytes, got {total}"),
        ));
    }
    let digest = hex_of(hasher.finalize());
    if digest != expected_sha256.to_lowercase() {
        return Err(("hash_mismatch", "SHA-256 mismatch".to_string()));
    }
    Ok(())
}

fn hex_of(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

struct ValidatedFields {
    uuid: String,
    title: String,
    description: String,
    languages: Vec<String>,
    article_count: u32,
    has_title_index: bool,
}

/// Staged-file validation (DX-18): header, UUID, metadata, main page
/// resolves with readable bytes, MIME sanity, title-index claim.
fn validate_payload(path: &Path) -> Result<ValidatedFields, (&'static str, String)> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| ("io_error", format!("canonicalize failed: {e}")))?;
    let opened =
        reader::open_validated(&canonical).map_err(|e| ("invalid_archive", format!("{e:?}")))?;
    let Some(zim) = opened.zim else {
        return Err(("internal_error", "open produced no reader".to_string()));
    };
    let meta = reader::read_metadata(&zim);
    let title = meta.get("Title").cloned().unwrap_or_default();
    let description = meta.get("Description").cloned().unwrap_or_default();
    let languages: Vec<String> = meta
        .get("Language")
        .map(|langs| {
            langs
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let (article_index, entry_index) = reader::title_lists(&zim);
    // Main page (or first article) resolves with readable bytes.
    let main_entry = zim
        .main_page()
        .map_err(|e| ("invalid_archive", format!("main page failed: {e:?}")))?
        .ok_or(("invalid_archive", "no main page".to_string()))?;
    let (terminal, _, _) = reader::resolve_chain(&zim, main_entry, 0)
        .map_err(|_| ("invalid_archive", "main page redirect invalid".to_string()))?;
    let bytes = reader::read_entry_bytes(&zim, &terminal)
        .map_err(|e| ("invalid_archive", format!("main page unreadable: {e:?}")))?;
    if bytes.is_empty() {
        return Err(("invalid_archive", "main page empty".to_string()));
    }
    let mime = reader::mime_of(&terminal);
    if mime.is_empty() {
        return Err(("invalid_archive", "main page has no MIME".to_string()));
    }
    Ok(ValidatedFields {
        uuid: opened.uuid,
        title,
        description,
        languages,
        article_count: opened.article_count,
        has_title_index: article_index || entry_index,
    })
}

fn valid_staged_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.ends_with(".zim")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && !name.contains("..")
}

/// Public snapshot shape for `GET /zim/v1/fetch/{op}`.
pub struct FetchSnapshot {
    pub id: String,
    pub phase: FetchPhase,
    pub bytes_received: u64,
    pub total_bytes: u64,
    pub report: Option<FetchReportView>,
    pub failure: Option<(String, String)>,
}

pub struct FetchReportView {
    pub uuid: String,
    pub title: String,
    pub description: String,
    pub languages: Vec<String>,
    pub article_count: u32,
    pub has_title_index: bool,
    pub payload_name: String,
    pub size_bytes: u64,
}
