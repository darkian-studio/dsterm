//! dsterm ZIM subsystem (`zim` Cargo feature, `/zim/v1`, Z2).
//!
//! Native archive reading behind the frozen contract
//! (`docs/zim-api-v1.md` in the DS repo). General capability: no DS
//! names, paths or catalogue logic here (DZ-1). All crate calls run on
//! a dedicated blocking pool, never the terminal executor (D26).
//!
//! mmap containment (DZ-25): the `zim` crate maps whole archives. Close
//! removes the registry reference only after in-flight work drains
//! (bounded wait, else `busy`), so no mapped ZIM outlives a successful
//! DELETE. DS deletes files only after close succeeds (UNI-1).

mod fetch;
mod reader;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use lru::LruCache;
use serde::Deserialize;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info, warn};

use crate::config::ZimConfig;

use fetch::FetchManager;
use reader::{
    lookup_entry, mime_of, read_entry_bytes, read_metadata, resolve_chain, title_lists,
    titles_in_order, Fault,
};
use zim_reader::MimeType;

const API_VERSION: u32 = 1;
const READER_CRATE_VERSION: &str = "zim-0.5.0";
const MAX_SUGGEST: usize = 50;
const GLOBAL_INFLIGHT: u32 = 32;
const ARCHIVE_INFLIGHT: u32 = 8;
const DECOMP_BUDGET_BYTES: u32 = 48 * 1024 * 1024;
const ENTRY_CACHE_BYTES: usize = 32 * 1024 * 1024;
const ENTRY_CACHE_ENTRIES: usize = 4096;
/// Heuristic quantum charged against the decompression budget per
/// decode (D26). Entry sizes are unknown before decoding, so each
/// decode holds 4 MiB of budget; the 32 MiB single-entry cap is the
/// hard bound, the budget bounds how many decodes stack concurrently.
const DECOMP_QUANTUM_BYTES: u32 = 4 * 1024 * 1024;
const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Stable §6.10 failure with its HTTP status.
#[derive(Debug)]
struct ZimError {
    code: &'static str,
    status: StatusCode,
    message: String,
    hint: Option<String>,
    retry_after_secs: Option<u64>,
}

impl ZimError {
    fn new(code: &'static str, status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            code,
            status,
            message: message.into(),
            hint: None,
            retry_after_secs: None,
        }
    }

    fn hinted(
        code: &'static str,
        status: StatusCode,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            code,
            status,
            message: message.into(),
            hint: Some(hint.into()),
            retry_after_secs: None,
        }
    }

    fn busy(message: impl Into<String>) -> Self {
        Self {
            code: "busy",
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
            hint: None,
            retry_after_secs: Some(5),
        }
    }

    fn bad_request(msg: impl Into<String>) -> Self {
        Self::new("invalid_request", StatusCode::BAD_REQUEST, msg)
    }

    fn unauthorized() -> Self {
        Self::new(
            "unauthorized",
            StatusCode::UNAUTHORIZED,
            "missing or bad auth",
        )
    }

    fn archive_closed() -> Self {
        Self::hinted(
            "archive_closed",
            StatusCode::GONE,
            "unknown or closed archive handle",
            "reopen the archive and retry once",
        )
    }
}

impl IntoResponse for ZimError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "api": API_VERSION,
            "error": {
                "code": self.code,
                "message": self.message,
                "hint": self.hint,
            },
        });
        if let Some(secs) = self.retry_after_secs {
            (
                self.status,
                [(
                    axum::http::header::RETRY_AFTER,
                    HeaderValue::from_str(&secs.to_string())
                        .unwrap_or(HeaderValue::from_static("5")),
                )],
                Json(body),
            )
                .into_response()
        } else {
            (self.status, Json(body)).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct OpenArchive {
    identity_uuid: String,
    file_size: u64,
    path: PathBuf,
    zim: zim_reader::Zim,
    titles: std::sync::OnceLock<Vec<(String, u32, String)>>,
    inflight: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    closing: AtomicBool,
    poisoned: AtomicBool,
    last_used: Mutex<Instant>,
}

struct ActiveGuard {
    counter: Arc<AtomicUsize>,
}

impl ActiveGuard {
    fn hold(archive: &OpenArchive) -> Self {
        let counter = Arc::clone(&archive.active);
        counter.fetch_add(1, Ordering::SeqCst);
        Self { counter }
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

struct EntryCache {
    map: LruCache<(String, u32), (String, Vec<u8>)>,
    bytes: usize,
}

impl EntryCache {
    fn new() -> Self {
        Self {
            map: LruCache::new(std::num::NonZeroUsize::new(ENTRY_CACHE_ENTRIES).unwrap()),
            bytes: 0,
        }
    }

    fn get(&mut self, key: &(String, u32)) -> Option<(String, Vec<u8>)> {
        self.map.get(key).cloned()
    }

    fn put(&mut self, key: (String, u32), mime: String, bytes: Vec<u8>) {
        self.bytes += bytes.len();
        if let Some((_, old)) = self.map.put(key, (mime, bytes)) {
            self.bytes = self.bytes.saturating_sub(old.len());
        }
        while self.bytes > ENTRY_CACHE_BYTES || self.map.len() > ENTRY_CACHE_ENTRIES {
            match self.map.pop_lru() {
                Some((_, (_, old))) => {
                    self.bytes = self.bytes.saturating_sub(old.len());
                }
                None => break,
            }
        }
    }

    fn evict_archive(&mut self, handle: &str) {
        let keys: Vec<(String, u32)> = self
            .map
            .iter()
            .filter(|(k, _)| k.0 == handle)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some((_, bytes)) = self.map.pop(&key) {
                self.bytes = self.bytes.saturating_sub(bytes.len());
            }
        }
    }
}

struct Inner {
    config: ZimConfig,
    roots: Vec<PathBuf>,
    auth_token: String,
    archives: DashMap<String, Arc<OpenArchive>>,
    by_path: DashMap<PathBuf, String>,
    entry_cache: Mutex<EntryCache>,
    decode_locks: DashMap<(String, u32), Arc<tokio::sync::Mutex<()>>>,
    global_sem: Arc<Semaphore>,
    decomp_budget: Arc<Semaphore>,
    exec: tokio::runtime::Handle,
    fetch: Arc<FetchManager>,
    boot_secs: u64,
}

/// Process-wide zim blocking pool (D26). One runtime per process,
/// separate from the terminal executors; states share it via cheap
/// handles (dropping a `Handle` is async-safe, dropping a `Runtime`
/// is not — hence no `Runtime` lives in request-scoped state).
static ZIM_EXEC: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

/// Serializes pool creation so two concurrent initializers cannot build
/// two runtimes and drop the loser (dropping a `Runtime` in async
/// context panics). Held only across initialization, never awaited.
static ZIM_EXEC_INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Cloneable handle to the subsystem (Axum state).
#[derive(Clone)]
pub struct ZimState {
    inner: Arc<Inner>,
}

impl ZimState {
    /// Canonicalizes allowed roots (unresolvable roots are skipped with
    /// a warning) and builds the dedicated blocking pool. `data_home`
    /// anchors the server-managed collections dir when the config does
    /// not name one explicitly. Fails only if the executor itself
    /// cannot start.
    pub fn new(config: &ZimConfig, auth_token: String, data_home: &Path) -> anyhow::Result<Self> {
        let mut roots = Vec::new();
        for root in &config.archive_roots {
            match std::fs::canonicalize(root) {
                Ok(canonical) => roots.push(canonical),
                Err(e) => {
                    warn!(root = %root, error = %e, "zim: skipping unresolvable archive root")
                }
            }
        }
        let exec = {
            let _init = ZIM_EXEC_INIT_LOCK
                .lock()
                .map_err(|e| anyhow::anyhow!("zim executor init lock poisoned: {e}"))?;
            if ZIM_EXEC.get().is_none() {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .max_blocking_threads(2)
                    .thread_name("zim-blocking")
                    .enable_all()
                    .build()
                    .map_err(|e| anyhow::anyhow!("zim executor failed to start: {e}"))?;
                let _ = ZIM_EXEC.set(runtime);
            }
            ZIM_EXEC
                .get()
                .expect("zim executor initialized above")
                .handle()
                .clone()
        };
        let boot_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let collections_dir = match config.collections_dir.as_deref() {
            Some(dir) => PathBuf::from(dir),
            // Server-managed payloads live outside user archives;
            // resolved against the explicit data home (never CWD).
            None => data_home.join(".cache/ds-zim-collections"),
        };
        let fetch = Arc::new(FetchManager::new(collections_dir, exec.clone())?);
        Ok(Self {
            inner: Arc::new(Inner {
                config: config.clone(),
                roots,
                auth_token,
                archives: DashMap::new(),
                by_path: DashMap::new(),
                entry_cache: Mutex::new(EntryCache::new()),
                decode_locks: DashMap::new(),
                global_sem: Arc::new(Semaphore::new(GLOBAL_INFLIGHT as usize)),
                decomp_budget: Arc::new(Semaphore::new(DECOMP_BUDGET_BYTES as usize)),
                exec,
                fetch,
                boot_secs,
            }),
        })
    }

    /// Test/state constructor with explicit knobs (no globals involved).
    #[cfg(test)]
    fn for_tests(roots: Vec<PathBuf>) -> Self {
        let config = ZimConfig {
            enabled: true,
            archive_roots: roots
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            max_open_archives: 3,
            idle_ttl_secs: 600,
            collections_dir: Some(
                std::env::temp_dir()
                    .join(format!("dsterm-zim-test-{}", std::process::id()))
                    .to_string_lossy()
                    .into_owned(),
            ),
        };
        let data_home = std::env::temp_dir();
        Self::new(&config, "test-token".to_string(), &data_home).expect("test state")
    }

    fn roots_configured(&self) -> bool {
        self.inner.config.enabled && !self.inner.roots.is_empty()
    }

    /// Runs blocking reader work on the dedicated pool with panic
    /// containment (DZ-21): a panic becomes `internal_error` and poisons
    /// the archive instead of taking down dsterm. Panics in native/C
    /// code, aborts and OOM kills are NOT contained by this — see the
    /// DZ-27 analysis, not this function.
    ///
    /// Domain failures cross as [`Fault`] and map to stable codes;
    /// anything else is `internal_error`.
    async fn blocking<F, T>(
        &self,
        archive: &Arc<OpenArchive>,
        label: &'static str,
        f: F,
    ) -> Result<T, ZimError>
    where
        F: FnOnce() -> Result<T, Fault> + Send + 'static,
        T: Send + 'static,
    {
        if archive.poisoned.load(Ordering::SeqCst) {
            return Err(ZimError::new(
                "internal_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                "archive poisoned by a previous failure",
            ));
        }
        let result = self
            .inner
            .exec
            .spawn_blocking(move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)))
            .await;
        match result {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(fault))) => Err(match fault {
                Fault::RedirectLoop => ZimError::new(
                    "redirect_invalid",
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "redirect chain invalid",
                ),
                Fault::NoTitleIndex => ZimError::new(
                    "unsupported_operation",
                    StatusCode::NOT_IMPLEMENTED,
                    "archive has no title index",
                ),
                Fault::TooLarge { bytes } => ZimError::new(
                    "content_unavailable",
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("limit_exceeded: entry is {bytes} bytes"),
                ),
                Fault::Unexpected(error) => {
                    warn!(label, error = %error, "zim: reader error");
                    ZimError::new(
                        "internal_error",
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("{label} failed"),
                    )
                }
            }),
            Ok(Err(_)) => {
                archive.poisoned.store(true, Ordering::SeqCst);
                warn!(label, "zim: reader panic contained; archive poisoned");
                Err(ZimError::new(
                    "internal_error",
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{label} panicked"),
                ))
            }
            Err(join) => {
                warn!(label, error = %join, "zim: blocking task failed");
                Err(ZimError::new(
                    "internal_error",
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{label} failed"),
                ))
            }
        }
    }

    fn touch(&self, archive: &Arc<OpenArchive>) {
        if let Ok(mut last) = archive.last_used.try_lock() {
            *last = Instant::now();
        }
    }

    fn get_archive(&self, id: &str) -> Result<Arc<OpenArchive>, ZimError> {
        let archive = self
            .inner
            .archives
            .get(id)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(ZimError::archive_closed)?;
        if archive.closing.load(Ordering::SeqCst) {
            return Err(ZimError::archive_closed());
        }
        Ok(archive)
    }

    /// Evicts idle archives beyond cap/TTL. Called on every open so no
    /// background task is needed (deterministic, test-friendly).
    fn sweep_idle(&self) {
        let now = Instant::now();
        let ttl = Duration::from_secs(self.inner.config.idle_ttl_secs);
        let max = self.inner.config.max_open_archives.max(1);
        let mut candidates: Vec<(String, Instant)> = self
            .inner
            .archives
            .iter()
            .filter(|entry| {
                entry.value().active.load(Ordering::SeqCst) == 0
                    && !entry.value().closing.load(Ordering::SeqCst)
            })
            .filter_map(|entry| {
                entry
                    .value()
                    .last_used
                    .try_lock()
                    .ok()
                    .map(|last| (entry.key().clone(), *last))
            })
            .collect();
        candidates.sort_by_key(|(_, last)| *last);
        let over_cap = self.inner.archives.len().saturating_sub(max);
        let mut evicted = 0;
        for (id, last) in candidates {
            let idle = now.duration_since(last);
            if evicted < over_cap || idle > ttl {
                if let Some((_, archive)) = self.inner.archives.remove(&id) {
                    self.inner.by_path.remove(&archive.path);
                    self.forget_cached(&id);
                    evicted += 1;
                    info!(archive = %id, "zim: evicted idle archive");
                }
            }
        }
    }

    fn forget_cached(&self, handle: &str) {
        if let Ok(mut cache) = self.inner.entry_cache.try_lock() {
            cache.evict_archive(handle);
        }
    }

    async fn acquire_all(
        &self,
        archive: &Arc<OpenArchive>,
    ) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), ZimError> {
        let busy = || ZimError::busy("zim subsystem saturated");
        let global = tokio::time::timeout(
            ACQUIRE_TIMEOUT,
            Arc::clone(&self.inner.global_sem).acquire_owned(),
        )
        .await
        .map_err(|_| busy())?
        .map_err(|_| busy())?;
        let per = tokio::time::timeout(
            ACQUIRE_TIMEOUT,
            Arc::clone(&archive.inflight).acquire_owned(),
        )
        .await
        .map_err(|_| busy())?
        .map_err(|_| busy())?;
        Ok((global, per))
    }
}

// ---------------------------------------------------------------------------
// Policy (D28, DZ-7, DZ-8)
// ---------------------------------------------------------------------------

/// Admit one archive path: absolute, non-symlink, regular file under an
/// allowed root. Returns the canonical path. TOCTOU note: the crate maps
/// by path, so a same-UID swap between this check and `Zim::new` is a
/// residual race (reviewed, not fenced — the path allowlist still
/// bounds it to configured roots).
fn resolve_archive_file(roots: &[PathBuf], raw: &str) -> Result<PathBuf, ZimError> {
    if raw.is_empty() || raw.len() > 4096 || raw.contains('\0') {
        return Err(ZimError::bad_request("malformed archive path"));
    }
    let requested = Path::new(raw);
    if !requested.is_absolute() {
        return Err(ZimError::bad_request("archive path must be absolute"));
    }
    match std::fs::symlink_metadata(requested) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(ZimError::new(
                "path_not_allowed",
                StatusCode::FORBIDDEN,
                "symlink archives are rejected",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ZimError::new(
                "archive_not_found",
                StatusCode::NOT_FOUND,
                "no archive at path",
            ));
        }
        Err(e) => {
            return Err(ZimError::new(
                "io_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stat failed: {e}"),
            ));
        }
    }
    let file = std::fs::File::open(requested).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ZimError::new(
                "archive_not_found",
                StatusCode::NOT_FOUND,
                "no archive at path",
            )
        } else {
            ZimError::new(
                "io_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("open failed: {e}"),
            )
        }
    })?;
    let is_file = file.metadata().map(|m| m.is_file()).unwrap_or(false);
    drop(file);
    if !is_file {
        return Err(ZimError::new(
            "path_not_allowed",
            StatusCode::FORBIDDEN,
            "not a regular file",
        ));
    }
    let canonical = std::fs::canonicalize(requested).map_err(|e| {
        ZimError::new(
            "io_error",
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("canonicalize failed: {e}"),
        )
    })?;
    if !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(ZimError::new(
            "path_not_allowed",
            StatusCode::FORBIDDEN,
            "archive outside allowed roots",
        ));
    }
    Ok(canonical)
}

fn reject_bad_entry_path(path: &str) -> Result<(), ZimError> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') || path.starts_with('/') {
        return Err(ZimError::bad_request("malformed entry path"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

fn require_enabled(state: &ZimState) -> Result<(), ZimError> {
    if state.inner.config.enabled {
        Ok(())
    } else {
        Err(ZimError::new(
            "unsupported_operation",
            StatusCode::NOT_IMPLEMENTED,
            "zim subsystem disabled",
        ))
    }
}

async fn check_auth(state: &ZimState, headers: &HeaderMap) -> Result<(), ZimError> {
    // Same posture as the filesystem routes (`fs.rs`): loopback callers
    // presenting no token are treated as direct clients and allowed; a
    // present-but-wrong token is rejected. DS's Dart clients send no
    // token today, so requiring one would lock out the app.
    match headers
        .get("X-Dsterm-Loopback")
        .and_then(|v| v.to_str().ok())
    {
        None => Ok(()),
        Some(token) => {
            if !state.inner.auth_token.is_empty() && token == state.inner.auth_token {
                Ok(())
            } else {
                Err(ZimError::unauthorized())
            }
        }
    }
}

pub fn zim_routes() -> Router<ZimState> {
    Router::new()
        .route("/zim/v1/capabilities", get(capabilities))
        .route("/zim/v1/archives", post(open_archive))
        .route(
            "/zim/v1/archives/{id}",
            get(archive_status).delete(close_archive),
        )
        .route(
            "/zim/v1/archives/{id}/entries/lookup",
            get(lookup_entry_route),
        )
        .route("/zim/v1/archives/{id}/content/{*path}", get(content))
        .route("/zim/v1/archives/{id}/suggest", get(suggest))
        .route("/zim/v1/archives/{id}/verify", post(verify))
        .route("/zim/v1/fetch", post(fetch_start))
        .route("/zim/v1/fetch/{op}", get(fetch_status).delete(fetch_cancel))
        .route("/zim/v1/staged/{name}", delete(remove_staged))
}

async fn capabilities(
    State(state): State<ZimState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    Ok(Json(serde_json::json!({
        "api": API_VERSION,
        "crate_version": READER_CRATE_VERSION,
        "features": ["suggest", "ranges"],
        "limits": {"maxSuggest": MAX_SUGGEST, "maxOpenArchives": 3},
    })))
}

#[derive(Debug, Deserialize)]
struct OpenBody {
    path: String,
    expect_uuid: Option<String>,
}

async fn open_archive(
    State(state): State<ZimState>,
    Json(body): Json<OpenBody>,
) -> Result<impl IntoResponse, ZimError> {
    if !state.roots_configured() {
        return Err(ZimError::new(
            "unsupported_operation",
            StatusCode::NOT_IMPLEMENTED,
            "no archive roots configured",
        ));
    }
    let canonical = resolve_archive_file(&state.inner.roots, &body.path)?;

    state.sweep_idle();

    // Idempotent reopen (DZ-10).
    if let Some(id) = state.inner.by_path.get(&canonical).map(|e| e.clone()) {
        if let Some(archive) = state.inner.archives.get(&id).map(|e| Arc::clone(e.value())) {
            if let Some(expect) = body.expect_uuid.as_deref() {
                if expect != archive.identity_uuid {
                    return Err(ZimError::new(
                        "identity_mismatch",
                        StatusCode::CONFLICT,
                        "archive identity changed",
                    ));
                }
            }
            state.touch(&archive);
            return Ok(Json(open_dto(&id, &archive, state.inner.boot_secs)));
        }
    }

    if state.inner.archives.len() >= state.inner.config.max_open_archives.max(1) {
        return Err(ZimError::busy("too many open archives"));
    }

    let open_path = canonical.clone();
    let opened = state
        .inner
        .exec
        .spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reader::open_validated(&open_path)
            }))
        })
        .await
        .map_err(|e| {
            ZimError::new(
                "internal_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("open task failed: {e}"),
            )
        })?
        .map_err(|_| {
            ZimError::new(
                "internal_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                "archive open panicked",
            )
        })?
        .map_err(|e| {
            debug!(error = %e, "zim: open validation failed");
            ZimError::new(
                "invalid_archive",
                StatusCode::UNPROCESSABLE_ENTITY,
                "archive failed validation",
            )
        })?;

    if let Some(expect) = body.expect_uuid.as_deref() {
        if expect != opened.uuid {
            return Err(ZimError::new(
                "identity_mismatch",
                StatusCode::CONFLICT,
                "archive identity changed",
            ));
        }
    }

    let Some(zim) = opened.zim else {
        return Err(ZimError::new(
            "internal_error",
            StatusCode::INTERNAL_SERVER_ERROR,
            "archive open produced no reader",
        ));
    };
    let id = uuid::Uuid::new_v4().to_string();
    let archive = Arc::new(OpenArchive {
        identity_uuid: opened.uuid.clone(),
        file_size: opened.file_size,
        path: opened.path.clone(),
        zim,
        titles: std::sync::OnceLock::new(),
        inflight: Arc::new(Semaphore::new(ARCHIVE_INFLIGHT as usize)),
        active: Arc::new(AtomicUsize::new(0)),
        closing: AtomicBool::new(false),
        poisoned: AtomicBool::new(false),
        last_used: Mutex::new(Instant::now()),
    });
    state.inner.by_path.insert(opened.path, id.clone());
    state
        .inner
        .archives
        .insert(id.clone(), Arc::clone(&archive));
    info!(
        archive = %id,
        uuid = %opened.uuid,
        version = %format!("{}.{}", opened.version_major, opened.version_minor),
        articles = %opened.article_count,
        clusters = %opened.cluster_count,
        "zim: opened archive"
    );
    Ok(Json(open_dto(&id, &archive, state.inner.boot_secs)))
}

fn open_dto(id: &str, archive: &OpenArchive, epoch: u64) -> serde_json::Value {
    serde_json::json!({
        "archive_id": id,
        "identity": {"uuid": archive.identity_uuid, "file_size": archive.file_size},
        "metadata": {},
        "capabilities": {"search": {"kind": "title-prefix"}},
        "session": {"epoch": epoch},
    })
}

async fn archive_status(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    require_enabled(&state)?;
    let archive = state.get_archive(&id)?;
    state.touch(&archive);
    let _guard = ActiveGuard::hold(&archive);
    let _permits = state.acquire_all(&archive).await?;
    let info = state
        .blocking(&archive, "status", {
            let archive = Arc::clone(&archive);
            move || {
                let metadata = read_metadata(&archive.zim);
                let (article_list, entry_list) = title_lists(&archive.zim);
                let languages: Vec<String> = metadata
                    .get("Language")
                    .map(|langs| {
                        langs
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                Ok((metadata, languages, article_list || entry_list))
            }
        })
        .await?;
    let (metadata, languages, has_title_index) = info;
    Ok(Json(serde_json::json!({
        "identity": {"uuid": archive.identity_uuid, "file_size": archive.file_size},
        "title": metadata.get("Title").cloned().unwrap_or_default(),
        "description": metadata.get("Description").cloned().unwrap_or_default(),
        "languages": languages,
        "article_count": archive.zim.header.article_count,
        "has_title_index": has_title_index,
        "metadata": metadata,
    })))
}

async fn close_archive(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    let archive = match state.inner.archives.get(&id) {
        Some(entry) => Arc::clone(entry.value()),
        // Idempotent close (contract): unknown handles succeed.
        None => return Ok(Json(serde_json::json!({})) as Json<serde_json::Value>),
    };
    archive.closing.store(true, Ordering::SeqCst);
    // Drain-wait so no mapped ZIM outlives a successful DELETE (DZ-25).
    // In-flight streams hold Arcs; bounded wait, then `busy` (the file
    // must not be deleted until close succeeds — DS uninstall waits).
    let deadline = Instant::now() + CLOSE_DRAIN_TIMEOUT;
    while archive.active.load(Ordering::SeqCst) > 0 {
        if Instant::now() >= deadline {
            return Err(ZimError::busy("archive has in-flight requests"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    state.inner.archives.remove(&id);
    state.inner.by_path.remove(&archive.path);
    state.forget_cached(&id);
    info!(archive = %id, "zim: closed archive");
    Ok(Json(serde_json::json!({})))
}

#[derive(Debug, Deserialize)]
struct LookupQuery {
    path: String,
    #[serde(default)]
    resolve: bool,
}

async fn lookup_entry_route(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<LookupQuery>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    reject_bad_entry_path(&query.path)?;
    let archive = state.get_archive(&id)?;
    state.touch(&archive);
    let _guard = ActiveGuard::hold(&archive);
    let _permits = state.acquire_all(&archive).await?;
    let path = query.path.clone();
    let do_resolve = query.resolve;
    let found = state
        .blocking(&archive, "lookup", {
            let archive = Arc::clone(&archive);
            move || {
                let Some((entry, idx)) = lookup_entry(&archive.zim, &path)? else {
                    return Ok(LookupOutcome::Missing);
                };
                // Redirects are identified by MIME, not by target shape:
                // content entries legitimately carry Cluster targets.
                if !matches!(entry.mime_type, MimeType::Redirect) {
                    let mime = mime_of(&entry);
                    let url = entry.url.clone();
                    return Ok(LookupOutcome::Content { url, mime });
                }
                if !do_resolve {
                    let target = first_hop_target(&archive.zim, &entry)?;
                    return Ok(LookupOutcome::Redirect {
                        target,
                        chain: vec![path.clone()],
                    });
                }
                let (terminal, _, chain) = resolve_chain(&archive.zim, entry, idx)?;
                let target = terminal.url.clone();
                let mime = mime_of(&terminal);
                Ok(LookupOutcome::Resolved {
                    target,
                    chain,
                    mime,
                })
            }
        })
        .await?;
    match found {
        LookupOutcome::Missing => Ok(Json(serde_json::json!({
            "exists": false,
            "path": query.path,
        }))),
        LookupOutcome::Content { url, mime } => Ok(Json(serde_json::json!({
            "exists": true,
            "path": url,
            "type": "content",
            "mime": mime,
        }))),
        LookupOutcome::Redirect { target, chain } => Ok(Json(serde_json::json!({
            "exists": true,
            "path": query.path,
            "type": "redirect",
            "redirect_to": target,
            "chain": chain,
        }))),
        LookupOutcome::Resolved {
            target,
            chain,
            mime,
        } => Ok(Json(serde_json::json!({
            "exists": true,
            "path": target,
            "type": "content",
            "mime": mime,
            "chain": chain,
        }))),
    }
}

/// Blocking-layer lookup outcome.
enum LookupOutcome {
    Missing,
    Content {
        url: String,
        mime: String,
    },
    Redirect {
        target: String,
        chain: Vec<String>,
    },
    Resolved {
        target: String,
        chain: Vec<String>,
        mime: String,
    },
}

/// First-hop redirect target without following the chain.
fn first_hop_target(
    zim: &zim_reader::Zim,
    entry: &zim_reader::DirectoryEntry,
) -> Result<String, Fault> {
    match &entry.target {
        Some(zim_reader::Target::Redirect(idx)) => Ok(zim.get_by_url_index(*idx)?.url),
        _ => Err(Fault::Unexpected(anyhow::anyhow!(
            "redirect entry without redirect target"
        ))),
    }
}

#[derive(Debug, Deserialize)]
struct SuggestQuery {
    q: String,
    #[serde(default = "default_suggest_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_suggest_limit() -> usize {
    10
}

async fn suggest(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<SuggestQuery>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    if query.q.len() > 256 {
        return Err(ZimError::bad_request("query exceeds 256 bytes"));
    }
    let limit = query.limit.clamp(1, MAX_SUGGEST);
    let offset: usize = query
        .cursor
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let archive = state.get_archive(&id)?;
    state.touch(&archive);
    let _guard = ActiveGuard::hold(&archive);
    let _permits = state.acquire_all(&archive).await?;
    let q = query.q.clone();
    let hits = state
        .blocking(&archive, "suggest", {
            let archive = Arc::clone(&archive);
            move || {
                let titles = archive.titles.get_or_init(|| titles_in_order(&archive.zim));
                if titles.is_empty() {
                    return Err(Fault::NoTitleIndex);
                }
                // Actual index semantics: byte-ordered prefix match, no
                // case folding (DZ-31).
                let matched: Vec<serde_json::Value> = titles
                    .iter()
                    .filter(|(title, _, _)| title.starts_with(&q))
                    .skip(offset)
                    .take(limit)
                    .map(|(title, _, url)| serde_json::json!({"path": url, "title": title}))
                    .collect();
                Ok(matched)
            }
        })
        .await?;
    Ok(Json(serde_json::json!({ "hits": hits })))
}

#[derive(Debug, Deserialize, Default)]
struct ContentQuery {
    max_bytes: Option<usize>,
}

async fn content(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath((id, path)): UrlPath<(String, String)>,
    Query(query): Query<ContentQuery>,
) -> Result<Response, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    reject_bad_entry_path(&path)?;
    let archive = state.get_archive(&id)?;
    state.touch(&archive);
    let _guard = ActiveGuard::hold(&archive);
    let _permits = state.acquire_all(&archive).await?;

    // If-Match style freshness is out of scope; ETag identifies content.
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range);

    let target = state
        .blocking(&archive, "content", {
            let archive = Arc::clone(&archive);
            let path = path.clone();
            move || {
                let Some((entry, idx)) = lookup_entry(&archive.zim, &path)? else {
                    return Ok(ContentTarget::Missing);
                };
                if matches!(entry.mime_type, MimeType::Redirect) {
                    let (terminal, _, _) = resolve_chain(&archive.zim, entry, idx)?;
                    return Ok(ContentTarget::Redirect {
                        target: terminal.url,
                    });
                }
                Ok(ContentTarget::Entry { idx })
            }
        })
        .await?;
    match target {
        ContentTarget::Missing => Err(ZimError::new(
            "entry_not_found",
            StatusCode::NOT_FOUND,
            "no readable entry at path",
        )),
        ContentTarget::Redirect { target } => {
            let location = format!(
                "/zim/v1/archives/{id}/content/{}",
                urlencoding::encode(&target)
            );
            let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
            response
                .headers_mut()
                .insert(axum::http::header::LOCATION, header_value(&location, "/"));
            Ok(response)
        }
        ContentTarget::Entry { idx } => {
            let (mime, full) = read_cached(&state, &archive, &id, idx).await?;
            let etag = format!("\"{}-{idx}\"", archive.identity_uuid);
            let total = full.len();
            let (bytes, truncated) = match query.max_bytes {
                Some(cap) if full.len() > cap => (full[..cap].to_vec(), true),
                _ => (full, false),
            };
            let (bytes, served) = match range {
                Some((start, end)) => {
                    let from = start.min(bytes.len());
                    let to = end.min(bytes.len()).max(from);
                    (bytes[from..to].to_vec(), Some((from, to)))
                }
                None => (bytes, None),
            };
            let mut builder = Response::builder()
                .header(
                    axum::http::header::CONTENT_TYPE,
                    header_value(&mime, "application/octet-stream"),
                )
                .header("X-Content-Type-Options", "nosniff")
                .header(axum::http::header::ETAG, header_value(&etag, "\"\""))
                .header(
                    axum::http::header::ACCEPT_RANGES,
                    HeaderValue::from_static("bytes"),
                );
            if truncated {
                builder = builder.header("X-DS-Truncated", "true");
            }
            let body = match served {
                Some((from, to)) => {
                    builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
                        axum::http::header::CONTENT_RANGE,
                        HeaderValue::from_str(&format!(
                            "bytes {from}-{}/{total}",
                            to.saturating_sub(1)
                        ))
                        .unwrap_or(HeaderValue::from_static("bytes */*")),
                    );
                    axum::body::Body::from(bytes)
                }
                None => {
                    builder = builder
                        .status(StatusCode::OK)
                        .header(axum::http::header::CONTENT_LENGTH, total.to_string());
                    axum::body::Body::from(bytes)
                }
            };
            builder.body(body).map_err(|_| {
                ZimError::new(
                    "internal_error",
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "response build failed",
                )
            })
        }
    }
}

/// Blocking-layer content routing: redirects surface explicitly
/// (content route → 307), entries by index for the cached read below.
enum ContentTarget {
    Missing,
    Redirect { target: String },
    Entry { idx: u32 },
}

/// Cached entry read with single-flight decode (D26). Returns
/// (mime, full decoded bytes); slicing and truncation are the
/// caller's, so the cache stays canonical per entry.
async fn read_cached(
    state: &ZimState,
    archive: &Arc<OpenArchive>,
    handle: &str,
    idx: u32,
) -> Result<(String, Vec<u8>), ZimError> {
    let key = (handle.to_string(), idx);
    if let Ok(mut cache) = state.inner.entry_cache.try_lock() {
        if let Some(hit) = cache.get(&key) {
            return Ok(hit);
        }
    }
    let slot = state
        .inner
        .decode_locks
        .entry(key.clone())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _single = slot.lock().await;
    if let Ok(mut cache) = state.inner.entry_cache.try_lock() {
        if let Some(hit) = cache.get(&key) {
            state.inner.decode_locks.remove(&key);
            return Ok(hit);
        }
    }
    let _quantum = tokio::time::timeout(
        ACQUIRE_TIMEOUT,
        Arc::clone(&state.inner.decomp_budget).acquire_many_owned(DECOMP_QUANTUM_BYTES),
    )
    .await
    .map_err(|_| ZimError::busy("decompression budget exhausted"))?
    .map_err(|_| ZimError::busy("decompression budget exhausted"))?;
    let decoded = state
        .blocking(archive, "content", {
            let archive = Arc::clone(archive);
            move || {
                let entry = archive.zim.get_by_url_index(idx)?;
                if matches!(entry.mime_type, MimeType::Redirect) {
                    return Err(Fault::Unexpected(anyhow::anyhow!(
                        "redirect in cached read"
                    )));
                }
                let bytes = read_entry_bytes(&archive.zim, &entry)?;
                let mime = mime_of(&entry);
                Ok((mime, bytes))
            }
        })
        .await;
    state.inner.decode_locks.remove(&key);
    let (mime, bytes) = decoded?;
    if let Ok(mut cache) = state.inner.entry_cache.try_lock() {
        cache.put(key, mime.clone(), bytes.clone());
    }
    Ok((mime, bytes))
}

fn parse_range(header: &str) -> Option<(usize, usize)> {
    let spec = header.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let end: usize = if end.is_empty() {
        usize::MAX
    } else {
        end.parse().ok()?
    };
    Some((start, end))
}

fn header_value(value: &str, fallback: &'static str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(fallback))
}

async fn verify(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    let archive = state.get_archive(&id)?;
    state.touch(&archive);
    let _guard = ActiveGuard::hold(&archive);
    let _permits = state.acquire_all(&archive).await?;
    // Full checksum pass; not cancellable mid-pass (documented) —
    // client disconnect still drops the response.
    state
        .blocking(&archive, "verify", {
            let archive = Arc::clone(&archive);
            move || {
                archive.zim.verify_checksum()?;
                Ok(())
            }
        })
        .await
        .map_err(|_| {
            ZimError::new(
                "checksum_error",
                StatusCode::UNPROCESSABLE_ENTITY,
                "checksum verification failed",
            )
        })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct FetchBody {
    url: String,
    #[serde(default)]
    mirrors: Vec<String>,
    sha256: String,
    size_bytes: u64,
    collection_id: String,
}

fn fetch_phase_name(phase: &fetch::FetchPhase) -> &'static str {
    match phase {
        fetch::FetchPhase::Queued => "queued",
        fetch::FetchPhase::Downloading => "downloading",
        fetch::FetchPhase::Verifying => "verifying",
        fetch::FetchPhase::Validating => "validating",
        fetch::FetchPhase::Done => "done",
        fetch::FetchPhase::Cancelled => "cancelled",
        fetch::FetchPhase::Failed { .. } => "failed",
    }
}

async fn fetch_start(
    State(state): State<ZimState>,
    headers: HeaderMap,
    Json(body): Json<FetchBody>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    if !body.url.starts_with("https://") {
        return Err(ZimError::bad_request("fetch url must be HTTPS"));
    }
    if body.collection_id.is_empty() || body.collection_id.len() > 128 {
        return Err(ZimError::bad_request("invalid collection id"));
    }
    if body.size_bytes == 0 {
        return Err(ZimError::bad_request("invalid expected size"));
    }
    let clean = body.sha256.trim().to_lowercase();
    if clean.len() != 64 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ZimError::bad_request("invalid SHA-256"));
    }
    let id = state.inner.fetch.start(fetch::FetchRequest {
        urls: vec![body.url],
        mirrors: body
            .mirrors
            .into_iter()
            .filter(|u| u.starts_with("https://"))
            .collect(),
        sha256: clean,
        size_bytes: body.size_bytes,
        collection_id: body.collection_id,
    });
    Ok(Json(serde_json::json!({"op": id, "state": "queued"})))
}

async fn fetch_status(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(op): UrlPath<String>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    let Some(snapshot) = state.inner.fetch.snapshot(&op).await else {
        return Err(ZimError::new(
            "invalid_request",
            StatusCode::NOT_FOUND,
            "unknown fetch op",
        ));
    };
    Ok(Json(serde_json::json!({
        "op": snapshot.id,
        "state": fetch_phase_name(&snapshot.phase),
        "bytes_received": snapshot.bytes_received,
        "total_bytes": snapshot.total_bytes,
            "report": snapshot.report.map(|r| serde_json::json!({
                "uuid": r.uuid,
                "title": r.title,
                "description": r.description,
                "languages": r.languages,
                "article_count": r.article_count,
                "has_title_index": r.has_title_index,
                "payload_name": r.payload_name,
                "payload_path": r.payload_path,
                "size_bytes": r.size_bytes,
            })),
        "failure": snapshot.failure.map(|(code, message)| serde_json::json!({
            "code": code,
            "message": message,
        })),
    })))
}

async fn fetch_cancel(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(op): UrlPath<String>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    let cancelled = state.inner.fetch.cancel(&op).await;
    Ok(Json(serde_json::json!({ "cancelled": cancelled })))
}

async fn remove_staged(
    State(state): State<ZimState>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
) -> Result<impl IntoResponse, ZimError> {
    check_auth(&state, &headers).await?;
    require_enabled(&state)?;
    let removed = state.inner.fetch.remove_staged(&name);
    Ok(Json(serde_json::json!({ "removed": removed })))
}

// ---------------------------------------------------------------------------
// Tests (HTTP-level, tower oneshot — no TCP)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn fixtures_root() -> PathBuf {
        std::fs::canonicalize("tests/fixtures/zim").expect("fixtures dir")
    }

    fn test_router() -> Router {
        let state = ZimState::for_tests(vec![fixtures_root()]);
        zim_routes().with_state(state)
    }

    async fn call(
        router: Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        token: Option<&str>,
    ) -> (StatusCode, HeaderMap, bytes::Bytes) {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("X-Dsterm-Loopback", token);
        }
        let body = match body {
            Some(json) => Body::from(json.to_string()),
            None => Body::empty(),
        };
        let response = router.oneshot(builder.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        (status, headers, bytes)
    }

    const TOKEN: Option<&str> = Some("test-token");

    async fn open_fixture(router: &Router, name: &str) -> String {
        let path = fixtures_root().join(name);
        let (status, _, body) = call(
            router.clone(),
            "POST",
            "/zim/v1/archives",
            Some(serde_json::json!({"path": path.to_string_lossy()})),
            TOKEN,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "open {name}: {}",
            String::from_utf8_lossy(&body)
        );
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        json["archive_id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn unauthorized_with_wrong_token() {
        let router = test_router();
        let (status, _, body) = call(
            router,
            "GET",
            "/zim/v1/capabilities",
            None,
            Some("wrong-token"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn direct_clients_without_token_are_allowed() {
        // Mirrors the filesystem routes' posture: DS Dart clients send
        // no token today.
        let router = test_router();
        let (status, _, _) = call(router, "GET", "/zim/v1/capabilities", None, None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn capabilities_reports_api_v1() {
        let router = test_router();
        let (status, _, body) = call(router, "GET", "/zim/v1/capabilities", None, TOKEN).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["api"], 1);
        assert!(json["features"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("suggest")));
    }

    #[tokio::test]
    async fn open_validates_policy() {
        let router = test_router();
        // Missing file.
        let (status, _, _) = call(
            router.clone(),
            "POST",
            "/zim/v1/archives",
            Some(serde_json::json!({"path": "/fixtures-root/nope.zim"})),
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Existing file outside the allowed roots (this source file).
        let me = std::fs::canonicalize("src/zim/mod.rs").unwrap();
        let (status, _, _) = call(
            router.clone(),
            "POST",
            "/zim/v1/archives",
            Some(serde_json::json!({"path": me.to_string_lossy()})),
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Corrupt archive.
        let corrupt = fixtures_root().join("invalid-header.zim");
        let (status, _, body) = call(
            router.clone(),
            "POST",
            "/zim/v1/archives",
            Some(serde_json::json!({"path": corrupt.to_string_lossy()})),
            TOKEN,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            String::from_utf8_lossy(&body)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_symlinks() {
        let router = test_router();
        let link = fixtures_root().join("link.zim");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(fixtures_root().join("lit.zim"), &link).unwrap();
        let (status, _, _) = call(
            router.clone(),
            "POST",
            "/zim/v1/archives",
            Some(serde_json::json!({"path": link.to_string_lossy()})),
            TOKEN,
        )
        .await;
        let _ = std::fs::remove_file(&link);
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn open_is_idempotent_and_uuid_checked() {
        let router = test_router();
        let first = open_fixture(&router, "lit.zim").await;
        let path = fixtures_root().join("lit.zim");
        let (status, _, body) = call(
            router.clone(),
            "POST",
            "/zim/v1/archives",
            Some(serde_json::json!({"path": path.to_string_lossy()})),
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["archive_id"].as_str().unwrap(), first);
        assert_eq!(
            json["identity"]["uuid"].as_str().unwrap(),
            "0002ed21-81ff-39eb-7274-d80240a8ea78"
        );

        let (status, _, _) = call(
            router.clone(),
            "POST",
            "/zim/v1/archives",
            Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "expect_uuid": "bogus",
            })),
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn status_lookup_suggest_content_roundtrip() {
        let router = test_router();
        let id = open_fixture(&router, "lit.zim").await;

        let (status, _, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{id}"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["title"], "Lit Docs");
        assert_eq!(json["has_title_index"], true);

        // Redirect unresolved, then resolved (old-scheme fixture whose
        // favicon entry is a redirect).
        let old_id = open_fixture(&router, "withns-small.zim").await;
        let (status, _, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{old_id}/entries/lookup?path=favicon"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["type"], "redirect");
        assert!(!json["redirect_to"].as_str().unwrap().is_empty());

        let (status, _, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{old_id}/entries/lookup?path=favicon&resolve=true"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["type"], "content");

        // Missing entry.
        let (status, _, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{id}/entries/lookup?path=nope"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["exists"], false);

        // Title-prefix suggest, byte-ordered, unfiltered case.
        let (status, _, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{id}/suggest?q=Comp&limit=10"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let titles: Vec<&str> = json["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["title"].as_str().unwrap())
            .collect();
        assert!(titles.iter().all(|t| t.starts_with("Comp")));
        assert!(!titles.is_empty());

        // Content with headers.
        let (status, headers, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{id}/content/api/controllers/index"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.contains_key("etag"));
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert!(!body.is_empty());

        // Redirecting content URL → 307.
        let (status, headers, _) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{old_id}/content/favicon"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert!(headers.contains_key("location"));

        // Range slice → 206.
        let builder = axum::http::Request::builder()
            .method("GET")
            .uri(format!(
                "/zim/v1/archives/{id}/content/api/controllers/index"
            ))
            .header("X-Dsterm-Loopback", "test-token")
            .header("Range", "bytes=0-3");
        let req = builder.body(Body::empty()).unwrap();
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    }

    #[tokio::test]
    async fn fault_mapping_is_stable() {
        let state = ZimState::for_tests(vec![fixtures_root()]);
        let router = zim_routes().with_state(state.clone());
        let id = open_fixture(&router, "lit.zim").await;
        let archive = state.get_archive(&id).expect("open registers");

        let err = state
            .blocking(&archive, "probe", || -> Result<(), Fault> {
                Err(Fault::NoTitleIndex)
            })
            .await
            .unwrap_err();
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        let err = state
            .blocking(&archive, "probe", || -> Result<(), Fault> {
                Err(Fault::RedirectLoop)
            })
            .await
            .unwrap_err();
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn close_drops_handles_and_double_close_ok() {
        let router = test_router();
        let id = open_fixture(&router, "lit.zim").await;
        let (status, _, _) = call(
            router.clone(),
            "DELETE",
            &format!("/zim/v1/archives/{id}"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = call(
            router.clone(),
            "DELETE",
            &format!("/zim/v1/archives/{id}"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{id}"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::GONE);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "archive_closed");
    }

    #[tokio::test]
    async fn verify_route_checks_checksum() {
        let router = test_router();
        let id = open_fixture(&router, "lit.zim").await;
        let (status, _, body) = call(
            router.clone(),
            "POST",
            &format!("/zim/v1/archives/{id}/verify"),
            Some(serde_json::json!({})),
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
    }

    #[tokio::test]
    async fn old_scheme_archive_reads() {
        let router = test_router();
        let id = open_fixture(&router, "withns-small.zim").await;
        let (status, _, body) = call(
            router.clone(),
            "GET",
            &format!("/zim/v1/archives/{id}/entries/lookup?path=main.html&resolve=true"),
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["type"], "content");
    }

    /// Local static file server for fetch tests (no external network).
    /// `/lit.zim` honors Range (206 partial / 416 past end) so resume
    /// paths are exercised, not just fresh downloads.
    async fn file_server() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use axum::body::Body;
        use axum::http::{HeaderMap, HeaderValue};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_route = Arc::clone(&hits);
        let app = Router::new()
            .route(
                "/lit.zim",
                get(|headers: HeaderMap| async move {
                    use axum::http::header::{CONTENT_LENGTH, CONTENT_RANGE};
                    let bytes = std::fs::read("tests/fixtures/zim/lit.zim").unwrap();
                    let mut response_headers = HeaderMap::new();
                    if let Some(range) = headers
                        .get(axum::http::header::RANGE)
                        .and_then(|v| v.to_str().ok())
                    {
                        if let Some(spec) = range.strip_prefix("bytes=") {
                            let start: usize = spec
                                .split('-')
                                .next()
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0);
                            if start >= bytes.len() {
                                return (
                                    StatusCode::RANGE_NOT_SATISFIABLE,
                                    HeaderMap::new(),
                                    Body::empty(),
                                );
                            }
                            let partial = bytes[start..].to_vec();
                            let total = bytes.len();
                            let end = total - 1;
                            response_headers.insert(
                                CONTENT_RANGE,
                                HeaderValue::from_str(&format!("bytes {start}-{end}/{total}"))
                                    .unwrap(),
                            );
                            response_headers.insert(
                                CONTENT_LENGTH,
                                HeaderValue::from_str(&partial.len().to_string()).unwrap(),
                            );
                            return (
                                StatusCode::PARTIAL_CONTENT,
                                response_headers,
                                Body::from(partial),
                            );
                        }
                    }
                    response_headers.insert(
                        CONTENT_LENGTH,
                        HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
                    );
                    (StatusCode::OK, response_headers, Body::from(bytes))
                }),
            )
            .route(
                "/flaky.zim",
                get(move || {
                    let hits_route = Arc::clone(&hits_route);
                    async move {
                        let n = hits_route.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            return (StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string());
                        }
                        let bytes = std::fs::read("tests/fixtures/zim/lit.zim").unwrap();
                        (StatusCode::OK, format!("{} bytes", bytes.len()))
                    }
                }),
            )
            .route(
                "/slow.zim",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    (StatusCode::OK, "too late".to_string())
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), hits)
    }

    /// Fetch tests drive the manager directly: the HTTP layer refuses
    /// non-HTTPS URLs (KC-7), while the loopback file server is plain
    /// HTTP. Handler shape validation stays HTTP-level (see below).
    fn test_manager() -> (Arc<FetchManager>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("dsterm-fetch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = FetchManager::new(dir.clone(), tokio::runtime::Handle::current())
            .expect("fetch manager");
        (Arc::new(manager), dir)
    }

    async fn await_done(manager: &Arc<FetchManager>, op: &str) -> fetch::FetchSnapshot {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let snapshot = manager.snapshot(op).await.expect("op snapshot");
            match &snapshot.phase {
                fetch::FetchPhase::Done
                | fetch::FetchPhase::Failed { .. }
                | fetch::FetchPhase::Cancelled => return snapshot,
                _ => {}
            }
            assert!(Instant::now() < deadline, "fetch op stuck");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    async fn fetch_verifies_validates_and_reports() {
        let (manager, dir) = test_manager();
        let (files, _) = file_server().await;
        let op = manager.start(fetch::FetchRequest {
            urls: vec![format!("{files}/lit.zim")],
            mirrors: vec![],
            sha256: "a00095a3aca3e4bfe92843b59e641ad370a7a77dd1af4d85194a1ef4b2f52cd3".to_string(),
            size_bytes: 739061,
            collection_id: "lit-docs".to_string(),
        });
        let snapshot = await_done(&manager, &op).await;
        assert!(
            matches!(snapshot.phase, fetch::FetchPhase::Done),
            "unexpected: {:?}",
            snapshot.failure,
        );
        let report = snapshot.report.expect("report on done");
        assert_eq!(report.uuid, "0002ed21-81ff-39eb-7274-d80240a8ea78");
        assert_eq!(report.title, "Lit Docs");
        assert!(report.has_title_index);
        assert_eq!(report.payload_name, "lit-docs.zim");
        assert!(dir.join(&report.payload_name).exists());
    }

    #[tokio::test]
    async fn fetch_fails_over_mirrors_and_rejects_bad_hash() {
        let (manager, _) = test_manager();
        let (files, _) = file_server().await;
        // Primary 500s once, mirror serves: failover path.
        let op = manager.start(fetch::FetchRequest {
            urls: vec![format!("{files}/flaky.zim")],
            mirrors: vec![format!("{files}/lit.zim")],
            sha256: "a00095a3aca3e4bfe92843b59e641ad370a7a77dd1af4d85194a1ef4b2f52cd3".to_string(),
            size_bytes: 739061,
            collection_id: "flaky-docs".to_string(),
        });
        let snapshot = await_done(&manager, &op).await;
        assert!(
            matches!(snapshot.phase, fetch::FetchPhase::Done),
            "unexpected: {:?}",
            snapshot.failure,
        );

        // Wrong hash with correct size: fail-closed, no payload kept.
        let op = manager.start(fetch::FetchRequest {
            urls: vec![format!("{files}/lit.zim")],
            mirrors: vec![],
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            size_bytes: 739061,
            collection_id: "bad-docs".to_string(),
        });
        let snapshot = await_done(&manager, &op).await;
        assert!(
            matches!(snapshot.phase, fetch::FetchPhase::Failed { .. }),
            "expected failure, got {:?}",
            snapshot.phase
        );
    }

    #[tokio::test]
    async fn fetch_cancel_and_staged_remove() {
        let (manager, dir) = test_manager();
        let (files, _) = file_server().await;
        let op = manager.start(fetch::FetchRequest {
            urls: vec![format!("{files}/slow.zim")],
            mirrors: vec![],
            sha256: "a00095a3aca3e4bfe92843b59e641ad370a7a77dd1af4d85194a1ef4b2f52cd3".to_string(),
            size_bytes: 739061,
            collection_id: "slow-docs".to_string(),
        });
        assert!(manager.cancel(&op).await);
        let snapshot = await_done(&manager, &op).await;
        assert!(
            matches!(snapshot.phase, fetch::FetchPhase::Cancelled),
            "unexpected: {:?}",
            snapshot.phase
        );

        // Staged removal is confined: traversal and non-zim names refuse.
        assert!(!manager.remove_staged("../evil.zim"));
        assert!(!manager.remove_staged("nope.txt"));
        // Unknown but well-formed names report false, never error.
        assert!(!manager.remove_staged("ghost.zim"));
        let _ = &dir;
    }

    #[tokio::test]
    async fn fetch_resumes_partial_downloads() {
        let (manager, dir) = test_manager();
        let (files, _) = file_server().await;
        // Seed a partial file: the first 1000 bytes of the fixture.
        let lit = std::fs::read("tests/fixtures/zim/lit.zim").unwrap();
        std::fs::write(dir.join("resume-docs.zim.part"), &lit[..1000]).unwrap();
        let op = manager.start(fetch::FetchRequest {
            urls: vec![format!("{files}/lit.zim")],
            mirrors: vec![],
            sha256: "a00095a3aca3e4bfe92843b59e641ad370a7a77dd1af4d85194a1ef4b2f52cd3".to_string(),
            size_bytes: 739061,
            collection_id: "resume-docs".to_string(),
        });
        let snapshot = await_done(&manager, &op).await;
        assert!(
            matches!(snapshot.phase, fetch::FetchPhase::Done),
            "unexpected: {:?}",
            snapshot.failure,
        );
        let payload = std::fs::read(dir.join("resume-docs.zim")).unwrap();
        assert_eq!(payload, lit);
    }

    #[tokio::test]
    async fn fetch_start_validates_shape() {
        let router = test_router();
        // Non-HTTPS fetch URLs are refused up front (KC-7).
        let (status, _, _) = call(
            router.clone(),
            "POST",
            "/zim/v1/fetch",
            Some(serde_json::json!({
                "url": "http://example.com/x.zim",
                "mirrors": [],
                "sha256": "a00095a3aca3e4bfe92843b59e641ad370a7a77dd1af4d85194a1ef4b2f52cd3",
                "size_bytes": 739061,
                "collection_id": "plain-docs",
            })),
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Malformed hash and empty size likewise.
        for body in [
            serde_json::json!({
                "url": "https://example.com/x.zim",
                "mirrors": [],
                "sha256": "not-hex",
                "size_bytes": 739061,
                "collection_id": "x",
            }),
            serde_json::json!({
                "url": "https://example.com/x.zim",
                "mirrors": [],
                "sha256": "a00095a3aca3e4bfe92843b59e641ad370a7a77dd1af4d85194a1ef4b2f52cd3",
                "size_bytes": 0,
                "collection_id": "x",
            }),
        ] {
            let (status, _, _) =
                call(router.clone(), "POST", "/zim/v1/fetch", Some(body), TOKEN).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn staged_remove_is_confined() {
        let router = test_router();
        // Traversal and absolute paths never reach the handler: the
        // router itself rejects them (defense in depth with the
        // server-side name check).
        for name in ["../evil.zim", "/abs.zim"] {
            let (status, _, _) = call(
                router.clone(),
                "DELETE",
                &format!("/zim/v1/staged/{name}"),
                None,
                TOKEN,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        // Well-formed but unknown names report false, never error.
        let (status, _, body) = call(
            router.clone(),
            "DELETE",
            "/zim/v1/staged/ghost.zim",
            None,
            TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["removed"], false);
    }
}
