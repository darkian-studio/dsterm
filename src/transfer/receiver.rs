//! One listener, one transfer at a time. A second sender while busy gets an
//! immediate rejection instead of a queue — queuing would let a stranger hold
//! slots the owner never agreed to.
//!
//! Payloads land in a temp sibling first and rename into place only after
//! hash, metadata and cancellation checks all pass. A failed transfer must
//! never read as success.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::transfer::{
    protocol::{
        reason, AcceptFrame, EntryHeader, SenderComplete, TransferRequest, PROTOCOL_VERSION,
    },
    validate::{
        check_case_collision, destination_conflict, ensure_dest_dir, free_space, is_loopback_peer,
        join_within, pretty_bytes, resolve_dest_base, resolve_rename, validate_dest_hint,
        validate_entry_path, validate_root_name,
    },
    DEFAULT_CONFIRM_TIMEOUT_SECS, MAX_FILES, MAX_TOTAL_SIZE,
};

#[derive(Debug, Clone)]
pub struct ReceiverOptions {
    pub port: u16,
    pub auto_receive: bool,
    pub dest: Option<String>,
    pub overwrite: bool,
    pub rename: bool,
    pub allow_remote: bool,
    pub confirm_timeout_secs: u64,
    pub expose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerState {
    Idle,
    AwaitingConfirmation,
    Transferring,
}

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

fn install_signal_handler() {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = async {
                    if let Some(t) = sigterm.as_mut() {
                        t.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        SHUTDOWN.store(true, Ordering::SeqCst);
    });
}

pub async fn run_listener(opts: ReceiverOptions) -> Result<()> {
    if opts.overwrite && opts.rename {
        anyhow::bail!("--overwrite and --rename are mutually exclusive");
    }
    let dest_base = resolve_dest_base(&opts.dest, opts.auto_receive)?;
    eprintln!(
        "Transfer listener started. ({}, dest: {})",
        if opts.auto_receive {
            "automatic"
        } else {
            "interactive"
        },
        dest_base.display()
    );

    install_signal_handler();

    let mut listeners = Vec::new();
    if opts.expose {
        let l4: Result<TcpListener, _> =
            TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, opts.port)).await;
        match l4 {
            Ok(l) => listeners.push(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                anyhow::bail!("listener port already in use: {}", opts.port);
            }
            Err(e) => anyhow::bail!("failed to bind port {}: {e}", opts.port),
        }
        if let Ok(l6) = TcpListener::bind((std::net::Ipv6Addr::UNSPECIFIED, opts.port)).await {
            listeners.push(l6);
        }
    } else {
        let l4 = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, opts.port)).await;
        match l4 {
            Ok(l) => listeners.push(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                anyhow::bail!("listener port already in use: {}", opts.port);
            }
            Err(e) => anyhow::bail!("failed to bind port {}: {e}", opts.port),
        }
        if let Ok(l6) = TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, opts.port)).await {
            listeners.push(l6);
        }
    }
    if listeners.is_empty() {
        anyhow::bail!("failed to bind transfer listener on port {}", opts.port);
    }

    let state = Arc::new(tokio::sync::Mutex::new(ListenerState::Idle));
    eprintln!("Waiting for incoming transfers...");

    let mut tasks = Vec::new();
    for listener in listeners {
        let state = state.clone();
        let opts = opts.clone();
        let dest_base = dest_base.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                if SHUTDOWN.load(Ordering::SeqCst) {
                    break;
                }
                let (stream, peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let state = state.clone();
                let opts = opts.clone();
                let dest_base = dest_base.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, peer, state, opts, dest_base).await {
                        eprintln!("{e}");
                    }
                });
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
    println!("Shutting down transfer listener");
    Ok(())
}

async fn send_reject(
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    reason: &str,
    message: String,
) {
    let line = format!(
        "{}\n",
        serde_json::to_string(&crate::transfer::protocol::RejectFrame {
            status: "reject".to_string(),
            reason: reason.to_string(),
            message,
        })
        .unwrap()
    );
    let _ = write_half.write_all(line.as_bytes()).await;
    let _ = write_half.flush().await;
}

async fn read_line_timeout(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    secs: u64,
) -> Result<String> {
    let mut line = String::new();
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(secs),
        reader.read_line(&mut line),
    )
    .await
    .map_err(|_| anyhow!("timed out"))??;
    if n == 0 {
        anyhow::bail!("connection closed");
    }
    Ok(line.trim().to_string())
}

async fn handle_connection(
    stream: TcpStream,
    peer: std::net::SocketAddr,
    state: Arc<tokio::sync::Mutex<ListenerState>>,
    opts: ReceiverOptions,
    dest_base: PathBuf,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let raw = match read_line_timeout(&mut reader, 30).await {
        Ok(l) => l,
        Err(_) => {
            send_reject(
                &mut write_half,
                reason::MALFORMED_REQUEST,
                "could not read transfer request".into(),
            )
            .await;
            return Ok(());
        }
    };
    let req: TransferRequest = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(e) => {
            send_reject(
                &mut write_half,
                reason::MALFORMED_REQUEST,
                format!("malformed transfer request: {e}"),
            )
            .await;
            return Ok(());
        }
    };

    // Version gates everything: no allocation or prompt on a dialect we cannot speak.
    if req.version != PROTOCOL_VERSION {
        send_reject(
            &mut write_half,
            reason::UNSUPPORTED_VERSION,
            format!("unsupported protocol version {}", req.version),
        )
        .await;
        return Ok(());
    }

    {
        let mut s = state.lock().await;
        if *s != ListenerState::Idle {
            send_reject(
                &mut write_half,
                reason::BUSY,
                "receiver is busy with another transfer".into(),
            )
            .await;
            return Ok(());
        }
        *s = ListenerState::AwaitingConfirmation;
    }
    let reset = |state: &Arc<tokio::sync::Mutex<ListenerState>>| {
        let state = state.clone();
        async move {
            *state.lock().await = ListenerState::Idle;
        }
    };

    let peer_label = peer.ip().to_string();
    let is_windows = cfg!(windows);

    let name = match validate_root_name(&req.name, is_windows) {
        Ok(n) => n,
        Err(e) => {
            send_reject(&mut write_half, reason::PATH_REJECTED, e.to_string()).await;
            reset(&state).await;
            return Ok(());
        }
    };
    let hint = match validate_dest_hint(&req.dest_hint, is_windows) {
        Ok(h) => h,
        Err(e) => {
            send_reject(&mut write_half, reason::PATH_REJECTED, e.to_string()).await;
            reset(&state).await;
            return Ok(());
        }
    };
    if req.file_count > MAX_FILES || req.size > MAX_TOTAL_SIZE {
        send_reject(
            &mut write_half,
            reason::RESOURCE_LIMITS_EXCEEDED,
            format!(
                "declared {} files / {} exceeds limits",
                req.file_count,
                pretty_bytes(req.size)
            ),
        )
        .await;
        reset(&state).await;
        return Ok(());
    }
    // Loopback bounds origin, not identity — but for unattended mode it is
    // still the only default keeping strangers from writing to disk.
    if opts.auto_receive && !opts.allow_remote && !is_loopback_peer(&peer) {
        send_reject(
            &mut write_half,
            reason::AUTH_FAILED,
            "automatic receiving accepts loopback senders only (use --allow-remote to override)"
                .into(),
        )
        .await;
        reset(&state).await;
        return Ok(());
    }
    if req.size > 0 {
        let free = free_space(&dest_base);
        if free != u64::MAX && req.size > free {
            send_reject(
                &mut write_half,
                reason::INSUFFICIENT_DISK_SPACE,
                format!(
                    "declared {} exceeds free space {}",
                    pretty_bytes(req.size),
                    pretty_bytes(free)
                ),
            )
            .await;
            reset(&state).await;
            return Ok(());
        }
    }

    let incoming_is_dir = req.r#type == "directory";
    if req.r#type != "file" && !incoming_is_dir {
        send_reject(
            &mut write_half,
            reason::MALFORMED_REQUEST,
            format!("unknown transfer type '{}'", req.r#type),
        )
        .await;
        reset(&state).await;
        return Ok(());
    }

    let final_rel: String = hint.unwrap_or_else(|| name.clone());
    let final_path = match join_within(&dest_base, &final_rel) {
        Ok(p) => p,
        Err(e) => {
            send_reject(&mut write_half, reason::PATH_REJECTED, e.to_string()).await;
            reset(&state).await;
            return Ok(());
        }
    };
    let conflict = destination_conflict(&final_path, incoming_is_dir);

    let mut final_path = final_path;
    let mut final_name = final_rel.clone();
    if conflict.is_conflict {
        if opts.overwrite {
            // proceed; existing tree is replaced only after the new one verifies.
        } else if opts.rename {
            let parent = final_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| dest_base.clone());
            let leaf = final_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&final_name)
                .to_string();
            let new_leaf = resolve_rename(&parent, &leaf);
            final_path = parent.join(&new_leaf);
            final_name = new_leaf;
        }
    }

    if !opts.auto_receive {
        // The conflict rides along in the prompt so it is decided before a
        // single payload byte moves — never discovered mid-write.
        let dest_display = final_path.display().to_string();
        let conflict_note = if conflict.is_conflict {
            if opts.overwrite {
                format!("  ({} — will overwrite)", conflict.detail)
            } else if opts.rename {
                format!(
                    "  ({} — will rename to {})",
                    conflict.detail,
                    final_path.display()
                )
            } else {
                format!(
                    "  ({} — will be rejected; see --overwrite)",
                    conflict.detail
                )
            }
        } else {
            String::new()
        };
        println!("Incoming transfer\n");
        println!("Source: {peer_label}");
        println!("Name: {}", req.name);
        println!("Type: {}", req.r#type);
        println!("Size: {}", pretty_bytes(req.size));
        println!("Files: {}", req.file_count);
        println!("Destination: {dest_display}{conflict_note}");
        print!("Accept transfer? [y/N] ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        match ask_confirm(opts.confirm_timeout_secs).await {
            Confirm::Yes => {}
            Confirm::No => {
                send_reject(
                    &mut write_half,
                    reason::POLICY_REJECTED,
                    "rejected by receiver".into(),
                )
                .await;
                println!("Transfer denied.");
                reset(&state).await;
                return Ok(());
            }
            Confirm::Timeout => {
                send_reject(
                    &mut write_half,
                    reason::CONFIRMATION_TIMEOUT,
                    "no response within the confirmation window".into(),
                )
                .await;
                eprintln!("Transfer failed: confirmation timed out.");
                reset(&state).await;
                return Ok(());
            }
        }
        // Fail closed: without an explicit resolution flag, an accepted-but-
        // conflicting prompt still rejects rather than guessing overwrite.
        if conflict.is_conflict && !opts.overwrite && !opts.rename {
            send_reject(
                &mut write_half,
                reason::DESTINATION_CONFLICT,
                conflict.detail.clone(),
            )
            .await;
            eprintln!(
                "Transfer failed: destination already exists ({}).",
                conflict.detail
            );
            reset(&state).await;
            return Ok(());
        }
    } else if conflict.is_conflict && !opts.overwrite && !opts.rename {
        send_reject(
            &mut write_half,
            reason::DESTINATION_CONFLICT,
            conflict.detail.clone(),
        )
        .await;
        reset(&state).await;
        return Ok(());
    }

    let accept = AcceptFrame {
        status: "accept".to_string(),
        final_name: Some(final_name.clone()),
    };
    let mut aline = serde_json::to_string(&accept)?;
    aline.push('\n');
    write_half.write_all(aline.as_bytes()).await?;
    write_half.flush().await?;
    *state.lock().await = ListenerState::Transferring;

    let short_id = &req.transfer_id[..req.transfer_id.len().min(8)];
    let stem = final_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("transfer");
    let temp_path = dest_base.join(format!(".{stem}.dsterm-transfer-{short_id}"));

    let result = receive_payload(
        &mut reader,
        &mut write_half,
        &req,
        &dest_base,
        &temp_path,
        &final_path,
        opts.overwrite,
    )
    .await;

    *state.lock().await = ListenerState::Idle;
    match result {
        Ok(_) => {
            let done = serde_json::to_string(&crate::transfer::protocol::FinalFrame {
                status: "complete".to_string(),
                reason: None,
                message: None,
            })?;
            let mut d = done;
            d.push('\n');
            let _ = write_half.write_all(d.as_bytes()).await;
            println!("Transfer received.\nTransfer complete.\n\nWaiting for incoming transfer...");
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            let reason_code = if msg.contains("integrity") {
                reason::INTEGRITY_MISMATCH
            } else if msg.contains("cancelled") || SHUTDOWN.load(Ordering::SeqCst) {
                reason::CANCELLED
            } else {
                "transfer_failed"
            };
            let err_line = crate::transfer::protocol::final_error(reason_code, msg.clone());
            let _ = write_half
                .write_all(format!("{err_line}\n").as_bytes())
                .await;
            cleanup_temp(&temp_path);
            Err(anyhow!("Transfer failed: {msg}"))
        }
    }
}

enum Confirm {
    Yes,
    No,
    Timeout,
}

async fn ask_confirm(timeout_secs: u64) -> Confirm {
    let timeout_secs = if timeout_secs == 0 {
        DEFAULT_CONFIRM_TIMEOUT_SECS
    } else {
        timeout_secs
    };
    let line = tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        match std::io::stdin().read_line(&mut buf) {
            Ok(_) => buf,
            Err(_) => String::new(),
        }
    });
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), line).await {
        Err(_) => Confirm::Timeout,
        Ok(Ok(s)) => {
            let t = s.trim().to_lowercase();
            if t == "y" || t == "yes" {
                Confirm::Yes
            } else {
                Confirm::No
            }
        }
        Ok(Err(_)) => Confirm::No,
    }
}

fn cleanup_temp(temp: &Path) {
    if let Ok(m) = std::fs::symlink_metadata(temp) {
        if m.is_dir() && !m.file_type().is_symlink() {
            let _ = std::fs::remove_dir_all(temp);
        } else {
            let _ = std::fs::remove_file(temp);
        }
    }
}

// Every write target resolves against the temp root directly, never through
// a symlink a previous entry planted — otherwise entry N redirects N+1.
async fn receive_payload(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    _write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    req: &TransferRequest,
    dest_base: &Path,
    temp_path: &Path,
    final_path: &Path,
    overwrite: bool,
) -> Result<()> {
    let _ = dest_base;
    let incoming_is_dir = req.r#type == "directory";
    let is_windows = cfg!(windows);
    cleanup_temp(temp_path);

    let mut hasher = Sha256::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut total_written: u64 = 0;
    let start = std::time::Instant::now();
    let quiet = req.size < 256 * 1024;

    if incoming_is_dir {
        std::fs::create_dir_all(temp_path)
            .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
    }

    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            anyhow::bail!("cancelled by signal");
        }
        let raw = read_line_timeout(reader, 600)
            .await
            .map_err(|_| anyhow!("sender disconnect"))?;
        if raw.is_empty() {
            continue;
        }
        // SenderComplete and EntryHeader share the line channel; the presence
        // of a digest distinguishes the terminator from the next entry.
        if let Ok(done) = serde_json::from_str::<SenderComplete>(&raw) {
            if done.status == "complete" && !done.sha256.is_empty() {
                let actual = format!("{:x}", hasher.finalize());
                if !constant_time_eq(&actual, &done.sha256) {
                    anyhow::bail!("integrity verification failed");
                }
                break;
            }
        }
        let header: EntryHeader =
            serde_json::from_str(&raw).map_err(|e| anyhow!("malformed transfer payload: {e}"))?;
        validate_entry_path(&header.path, is_windows).map_err(|e| anyhow!("path rejected: {e}"))?;
        if is_windows {
            check_case_collision(&mut seen, &header.path)
                .map_err(|e| anyhow!("path rejected: {e}"))?;
        }
        if header.kind != "dir" && header.kind != "file" && header.kind != "symlink" {
            anyhow::bail!("malformed transfer payload: unknown entry kind");
        }
        if !incoming_is_dir {
            if header.kind != "file" || !header.path.is_empty() {
                anyhow::bail!("malformed transfer payload for single file");
            }
            let n = read_exact_bytes(
                reader,
                header.size,
                temp_path,
                &mut hasher,
                true,
                header.mode,
            )
            .await?;
            total_written += n;
            if !quiet {
                print_progress(total_written, req.size, &start);
            }
            continue;
        }
        let dest =
            join_within(temp_path, &header.path).map_err(|e| anyhow!("path rejected: {e}"))?;
        refuse_symlink_parents(temp_path, &dest)?;
        match header.kind.as_str() {
            "dir" => {
                std::fs::create_dir_all(&dest)
                    .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                apply_mode(&dest, header.mode, true);
            }
            "symlink" => {
                let target = header.link_target.unwrap_or_default();
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                    refuse_symlink_parents(temp_path, parent)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &dest)
                    .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                #[cfg(windows)]
                {
                    if target.ends_with('/') || target.ends_with('\\') {
                        std::os::windows::fs::symlink_dir(&target, &dest)
                            .or_else(|_| std::os::windows::fs::symlink_file(&target, &dest))
                            .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                    } else {
                        std::os::windows::fs::symlink_file(&target, &dest)
                            .or_else(|_| std::os::windows::fs::symlink_dir(&target, &dest))
                            .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                    }
                }
            }
            _ => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                    refuse_symlink_parents(temp_path, parent)?;
                }
                let n =
                    read_exact_bytes(reader, header.size, &dest, &mut hasher, false, header.mode)
                        .await?;
                total_written += n;
                if !quiet {
                    print_progress(total_written, req.size, &start);
                }
            }
        }
    }
    if !quiet {
        eprintln!();
    }

    if let Some(parent) = final_path.parent() {
        ensure_dest_dir(parent)?;
    }
    let conflict = destination_conflict(final_path, incoming_is_dir);
    if conflict.is_conflict {
        if overwrite {
            if let Ok(m) = std::fs::symlink_metadata(final_path) {
                if m.is_dir() && !m.file_type().is_symlink() {
                    std::fs::remove_dir_all(final_path)
                        .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                } else {
                    std::fs::remove_file(final_path)
                        .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
                }
            }
        } else {
            cleanup_temp(temp_path);
            anyhow::bail!("destination already exists: {}", conflict.detail);
        }
    }
    tokio::fs::rename(temp_path, final_path)
        .await
        .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
    Ok(())
}

fn refuse_symlink_parents(root: &Path, dest: &Path) -> Result<()> {
    let rel = dest
        .strip_prefix(root)
        .map_err(|_| anyhow!("path rejected: escapes root"))?;
    let mut cur = root.to_path_buf();
    let mut comps = rel.components().peekable();
    while let Some(c) = comps.next() {
        cur.push(c);
        // The leaf itself may legitimately be a symlink entry; only parents matter.
        if comps.peek().is_none() {
            break;
        }
        if let Ok(m) = std::fs::symlink_metadata(&cur) {
            if m.file_type().is_symlink() {
                anyhow::bail!("path rejected: parent is a symlink");
            }
        }
    }
    Ok(())
}

async fn read_exact_bytes(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    size: u64,
    dest: &Path,
    hasher: &mut Sha256,
    is_root_file: bool,
    mode: u32,
) -> Result<u64> {
    let _ = is_root_file;
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
    let mut remaining = size;
    let mut buf = vec![0u8; 64 * 1024];
    while remaining > 0 {
        if SHUTDOWN.load(Ordering::SeqCst) {
            drop(file);
            anyhow::bail!("cancelled by signal");
        }
        let want = (remaining as usize).min(buf.len());
        reader
            .read_exact(&mut buf[..want])
            .await
            .map_err(|_| anyhow!("sender disconnect"))?;
        hasher.update(&buf[..want]);
        file.write_all(&buf[..want]).await.map_err(|e| {
            if e.to_string().contains("space") {
                anyhow!("insufficient disk space")
            } else {
                anyhow!("receiver filesystem error: {e}")
            }
        })?;
        remaining -= want as u64;
    }
    file.flush()
        .await
        .map_err(|e| anyhow!("receiver filesystem error: {e}"))?;
    drop(file);
    apply_mode(dest, mode, false);
    Ok(size)
}

fn apply_mode(path: &Path, mode: u32, is_dir: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if mode != 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777));
        } else if is_dir {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode, is_dir);
    }
}

fn print_progress(written: u64, total: u64, start: &std::time::Instant) {
    if total == 0 {
        return;
    }
    let pct = written as f64 / total as f64 * 100.0;
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    let rate = pretty_bytes((written as f64 / elapsed) as u64);
    eprint!(
        "\r[{:>5.1}%] {} / {} ({}/s)",
        pct,
        pretty_bytes(written),
        pretty_bytes(total),
        rate
    );
    use std::io::Write as _;
    let _ = std::io::stderr().flush();
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}
