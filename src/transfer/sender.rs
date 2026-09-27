//! Fail before connecting when the source is unusable — opening a socket
//! first would leave a half-open transfer the receiver must time out.
//!
//! The top-level symlink follows scp convention (the user named it, so they
//! meant its content); symlinks inside the tree are preserved instead, since
//! following those would let one entry redirect the rest.

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::transfer::endpoint::{resolve_and_connect, Endpoint};
use crate::transfer::protocol::{
    reason, EntryHeader, FinalFrame, SenderComplete, TransferRequest, PROTOCOL_VERSION,
};
use crate::transfer::validate::pretty_bytes;

#[derive(Debug, Clone)]
pub struct SenderOptions {
    pub source: PathBuf,
    pub endpoint: Endpoint,
    pub dest_hint: Option<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    rel: String,
    abs: PathBuf,
    kind: String,
    mode: u32,
    size: u64,
    link_target: Option<String>,
}

pub async fn run_sender(opts: SenderOptions) -> Result<()> {
    let (root_name, is_dir, entries, total_size) = preflight(&opts.source)?;
    let file_count = entries.iter().filter(|e| e.kind == "file").count() as u64;
    let declared_count = if is_dir { file_count } else { 1 };

    eprintln!(
        "Sending {} ({} {}, {}) to {}:{}",
        root_name,
        declared_count,
        if declared_count == 1 { "file" } else { "files" },
        pretty_bytes(total_size),
        opts.endpoint.host,
        opts.endpoint.port
    );

    let stream = resolve_and_connect(&opts.endpoint)
        .await
        .map_err(|e| anyhow!("Transfer failed: {e}"))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Ctrl-C during a transfer must not report success; the flag is polled
    // between entries and inside the file copy loop.
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let c = cancelled.clone();
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
            c.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }

    let transfer_id = uuid::Uuid::new_v4().to_string();
    let req = TransferRequest {
        version: PROTOCOL_VERSION,
        transfer_id,
        r#type: if is_dir {
            "directory".into()
        } else {
            "file".into()
        },
        name: root_name.clone(),
        size: total_size,
        file_count: declared_count,
        source_addr: None,
        dest_hint: opts.dest_hint.clone(),
        capabilities: vec!["posix-permissions".into(), "symlinks".into()],
    };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    let response = read_json_line(&mut reader, 300)
        .await
        .map_err(|e| anyhow!("Transfer failed: no response from receiver ({e})"))?;
    let v: serde_json::Value = serde_json::from_str(&response)
        .map_err(|e| anyhow!("Transfer failed: malformed receiver response ({e})"))?;
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status == "reject" || status == "error" {
        let reason = v
            .get("reason")
            .and_then(|s| s.as_str())
            .unwrap_or("rejected");
        let message = v
            .get("message")
            .and_then(|s| s.as_str())
            .unwrap_or("rejected");
        return Err(human_reject_error(reason, message));
    }
    if status != "accept" {
        anyhow::bail!("Transfer failed: unexpected receiver response");
    }

    let mut hasher = Sha256::new();
    let start = std::time::Instant::now();
    let mut last_print = std::time::Instant::now();
    let quiet = total_size < 256 * 1024;
    let mut cumulative: u64 = 0;

    for entry in &entries {
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("Transfer cancelled.");
        }
        let header = EntryHeader {
            path: entry.rel.clone(),
            kind: entry.kind.clone(),
            mode: entry.mode,
            size: entry.size,
            link_target: entry.link_target.clone(),
        };
        let mut hline = serde_json::to_string(&header)?;
        hline.push('\n');
        write_half.write_all(hline.as_bytes()).await?;
        if entry.kind == "file" {
            let n = send_file_bytes(&entry.abs, &mut write_half, &mut hasher, &cancelled).await?;
            cumulative += n;
            // Aggregate progress only: per-file chatter would spam small-file trees.
            if !quiet && (last_print.elapsed().as_millis() > 200 || cumulative == total_size) {
                let pct = if total_size > 0 {
                    cumulative as f64 / total_size as f64 * 100.0
                } else {
                    100.0
                };
                let elapsed = start.elapsed().as_secs_f64().max(0.001);
                let rate = pretty_bytes((cumulative as f64 / elapsed) as u64);
                eprint!(
                    "\r[{:>5.1}%] {} / {} ({}/s)",
                    pct,
                    pretty_bytes(cumulative),
                    pretty_bytes(total_size),
                    rate
                );
                use std::io::Write as _;
                let _ = std::io::stderr().flush();
                last_print = std::time::Instant::now();
            }
        }
    }
    if !quiet {
        eprintln!();
    }
    let digest = format!("{:x}", hasher.finalize());

    let done = SenderComplete {
        status: "complete".into(),
        sha256: digest,
    };
    let mut dline = serde_json::to_string(&done)?;
    dline.push('\n');
    write_half.write_all(dline.as_bytes()).await?;
    write_half.flush().await?;

    let fin_line = read_json_line(&mut reader, 600)
        .await
        .map_err(|e| anyhow!("Transfer failed: no completion response from receiver ({e})"))?;
    let fin: FinalFrame = serde_json::from_str(&fin_line)
        .map_err(|e| anyhow!("Transfer failed: malformed completion response ({e})"))?;
    if fin.status == "complete" {
        let elapsed = start.elapsed().as_secs_f64();
        let rate = if elapsed > 0.0 {
            pretty_bytes((cumulative as f64 / elapsed) as u64) + "/s"
        } else {
            "-".into()
        };
        println!(
            "Transfer complete: {} ({}, {rate})",
            root_name,
            pretty_bytes(cumulative)
        );
        Ok(())
    } else {
        let reason = fin.reason.as_deref().unwrap_or("error");
        let message = fin.message.as_deref().unwrap_or("transfer failed");
        Err(human_reject_error(reason, message))
    }
}

// Bounded 64 KiB buffer: a 2 GiB file must never need 2 GiB of RAM.
async fn send_file_bytes(
    path: &Path,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    hasher: &mut Sha256,
    cancelled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<u64> {
    let mut file = tokio::fs::File::open(path).await.with_context(|| {
        format!(
            "source file disappeared during transfer: {}",
            path.display()
        )
    })?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut sent = 0u64;
    loop {
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("Transfer cancelled.");
        }
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        write_half.write_all(&buf[..n]).await?;
        sent += n as u64;
    }
    Ok(sent)
}

async fn read_json_line(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    timeout_secs: u64,
) -> Result<String> {
    let mut line = String::new();
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        reader.read_line(&mut line),
    )
    .await
    .map_err(|_| anyhow!("timed out waiting for receiver"))??;
    if n == 0 {
        anyhow::bail!("connection closed (EOF)");
    }
    Ok(line.trim().to_string())
}

fn human_reject_error(reason: &str, message: &str) -> anyhow::Error {
    let human = match reason {
        reason::BUSY => format!("Transfer failed: receiver is busy ({message})"),
        reason::POLICY_REJECTED => {
            format!("Transfer failed:\nReceiver rejected the transfer.\n{message}")
        }
        reason::UNSUPPORTED_VERSION => {
            format!("Transfer failed: unsupported protocol version ({message})")
        }
        reason::AUTH_FAILED => format!("Transfer failed: not authorized ({message})"),
        reason::PATH_REJECTED => format!("Transfer failed: destination path rejected ({message})"),
        reason::RESOURCE_LIMITS_EXCEEDED => {
            format!("Transfer failed: declared transfer exceeds resource limits ({message})")
        }
        reason::INSUFFICIENT_DISK_SPACE => {
            format!("Transfer failed: insufficient disk space on receiver ({message})")
        }
        reason::CONFIRMATION_TIMEOUT => {
            format!("Transfer failed: no response from receiver within the confirmation window. ({message})")
        }
        reason::DESTINATION_CONFLICT => {
            format!("Transfer failed: destination already exists ({message})")
        }
        reason::CANCELLED => "Transfer cancelled.".to_string(),
        _ => format!("Transfer failed: {message}"),
    };
    anyhow!(human)
}

fn preflight(source: &Path) -> Result<(String, bool, Vec<Entry>, u64)> {
    let link_meta = std::fs::symlink_metadata(source).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!("no such file or directory: '{}'", source.display())
        } else if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow!("permission denied: '{}'", source.display())
        } else {
            anyhow!("cannot stat '{}': {e}", source.display())
        }
    })?;
    let root_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("invalid source path: '{}'", source.display()))?
        .to_string();
    if root_name.is_empty() || root_name == "." || root_name == ".." {
        anyhow::bail!("invalid source path: '{}'", source.display());
    }
    let target: PathBuf = if link_meta.file_type().is_symlink() {
        let resolved = std::fs::canonicalize(source)
            .map_err(|e| anyhow!("cannot resolve symlink '{}': {e}", source.display()))?;
        let tmeta = std::fs::metadata(&resolved).map_err(|e| {
            anyhow!(
                "cannot transfer dangling symlink '{}': {e}",
                source.display()
            )
        })?;
        if !tmeta.is_file() && !tmeta.is_dir() {
            anyhow::bail!("unsupported source type: '{}'", source.display());
        }
        resolved
    } else {
        source.to_path_buf()
    };
    let meta = std::fs::metadata(&target).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow!("permission denied: '{}'", source.display())
        } else {
            anyhow!("cannot read '{}': {e}", source.display())
        }
    })?;
    if meta.is_file() {
        let mode = unix_mode(&target, false);
        let size = meta.len();
        return Ok((
            root_name,
            false,
            vec![Entry {
                rel: String::new(),
                abs: target,
                kind: "file".into(),
                mode,
                size,
                link_target: None,
            }],
            size,
        ));
    }
    if meta.is_dir() {
        let mut entries = Vec::new();
        let mut total = 0u64;
        walk_dir(&target, &target, &mut entries, &mut total)?;
        entries.sort_by(|a, b| a.rel.cmp(&b.rel));
        return Ok((root_name, true, entries, total));
    }
    anyhow::bail!("unsupported source type: '{}'", source.display())
}

fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<Entry>, total: &mut u64) -> Result<()> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| anyhow!("permission denied: '{}': {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    children.sort();
    for child in children {
        let rel = child
            .strip_prefix(root)
            .map_err(|_| anyhow!("internal path error"))?
            .to_string_lossy()
            .replace('\\', "/");
        let lmeta = std::fs::symlink_metadata(&child).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "source file disappeared during transfer: '{}'",
                    child.display()
                )
            } else {
                anyhow!("cannot stat '{}': {e}", child.display())
            }
        })?;
        if lmeta.file_type().is_symlink() {
            // A dangling target is still a valid symlink — its string is the payload.
            let target = std::fs::read_link(&child)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(Entry {
                rel,
                abs: child,
                kind: "symlink".into(),
                mode: 0o777,
                size: 0,
                link_target: Some(target),
            });
            continue;
        }
        if lmeta.is_dir() {
            out.push(Entry {
                rel: rel.clone(),
                abs: child.clone(),
                kind: "dir".into(),
                mode: unix_mode(&child, true),
                size: 0,
                link_target: None,
            });
            walk_dir(root, &child, out, total)?;
            continue;
        }
        if lmeta.is_file() {
            let size = lmeta.len();
            *total += size;
            let mode = unix_mode(&child, false);
            out.push(Entry {
                rel,
                abs: child,
                kind: "file".into(),
                mode,
                size,
                link_target: None,
            });
            continue;
        }
        // Sockets, FIFOs and device nodes have no portable file content:
        // skipping one entry beats failing the whole tree over it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            let ft = lmeta.file_type();
            let what = if ft.is_socket() {
                "unix socket"
            } else if ft.is_fifo() {
                "fifo"
            } else if ft.is_block_device() {
                "block device"
            } else if ft.is_char_device() {
                "char device"
            } else {
                "special file"
            };
            eprintln!("skipped: {} ({what}, not transferable)", rel);
        }
        #[cfg(not(unix))]
        {
            eprintln!("skipped: {} (special file, not transferable)", rel);
        }
    }
    Ok(())
}

fn unix_mode(path: &Path, is_dir: bool) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(path) {
            return m.permissions().mode() & 0o777;
        }
    }
    let _ = path;
    if is_dir {
        0o755
    } else {
        0o644
    }
}
