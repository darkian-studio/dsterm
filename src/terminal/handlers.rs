use super::get_default_command;
#[cfg(unix)]
use super::pty_fallback::fallback_open_and_spawn;
use super::scrollback::Scrollback;
use super::types::*;
use crate::utils::parse_u16;
use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
    Json,
};
use futures::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use regex::Regex;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use std::{
    io::Read,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::Duration,
};
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex};
use tokio::task::spawn_blocking;

pub static SESSIONS_CREATED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Human-readable timestamp (unix seconds.millis) for lifecycle tracing.
fn trace_ts() -> String {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", dur.as_secs(), dur.subsec_millis())
}

fn shell_program() -> String {
    #[cfg(windows)]
    {
        return std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    }

    #[cfg(not(windows))]
    {
        String::from("sh")
    }
}

fn default_working_directory() -> PathBuf {
    #[cfg(windows)]
    {
        return std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    }

    #[cfg(not(windows))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

pub struct TerminalSession {
    pub terminal_id: String,
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pub child_killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub scrollback: Arc<Scrollback>,
    pub output_tx: broadcast::Sender<Vec<u8>>,
    pub command_exit_tx: broadcast::Sender<String>,
    pub osc_leftover: Arc<std::sync::Mutex<Vec<u8>>>,
    pub exit_status: Arc<std::sync::Mutex<Option<bool>>>,
    pub exit_notify: Arc<tokio::sync::Notify>,
    pub last_accessed: Arc<Mutex<SystemTime>>,
}

/// Strip OSC 633 sequences in-place from a byte buffer, returning cleaned
/// bytes and a vector of exit codes for D-class events. Sequences that straddle
/// the end of `chunk` are kept in `leftover` for reassembly with the next chunk.
pub fn strip_osc_633(chunk: &[u8], leftover: &mut Vec<u8>) -> (Vec<u8>, Vec<i32>) {
    let mut input: Vec<u8> = std::mem::take(leftover);
    input.extend_from_slice(chunk);

    let mut out = Vec::with_capacity(input.len());
    let mut exits = Vec::new();
    let mut i = 0usize;
    while i < input.len() {
        if input[i] == 0x1b
            && i + 5 < input.len()
            && input[i + 1] == b']'
            && input[i + 2] == b'6'
            && input[i + 3] == b'3'
            && input[i + 4] == b'3'
            && input[i + 5] == b';'
        {
            let mut j = i + 6;
            let mut found_st: Option<usize> = None;
            while j < input.len() && j - i < 256 {
                if input[j] == 0x07 {
                    found_st = Some(j + 1);
                    break;
                }
                if j + 1 < input.len() && input[j] == 0x1b && input[j + 1] == 0x5c {
                    found_st = Some(j + 2);
                    break;
                }
                j += 1;
            }
            match found_st {
                Some(end) => {
                    let body = &input[i + 6..end - if input[end - 1] == 0x07 { 1 } else { 2 }];
                    if body.starts_with(b"D;") {
                        if let Ok(s) = std::str::from_utf8(&body[2..]) {
                            if let Ok(code) = s.trim().parse::<i32>() {
                                exits.push(code);
                            }
                        }
                    }
                    i = end;
                    continue;
                }
                None => {
                    leftover.extend_from_slice(&input[i..]);
                    return (out, exits);
                }
            }
        }
        out.push(input[i]);
        i += 1;
    }
    (out, exits)
}

pub async fn create_terminal(
    State(sessions): State<Sessions>,
    Json(options): Json<TerminalOptions>,
) -> impl IntoResponse {
    let rows = parse_u16(&options.rows, "rows").expect("failed");
    let cols = parse_u16(&options.cols, "cols").expect("failed");
    tracing::info!("Creating new terminal with cols={}, rows={}", cols, rows);

    #[cfg(not(windows))]
    let session_uuid = uuid::Uuid::new_v4().to_string();
    #[cfg(not(windows))]
    let mut env_overrides: Vec<(String, String)> = Vec::new();
    #[cfg(windows)]
    let env_overrides: Vec<(String, String)> = Vec::new();
    let (program, args) = if get_default_command().is_some() {
        let cmd = get_default_command().unwrap();
        let parts: Vec<String> = cmd.split_whitespace().map(|s| s.to_string()).collect();
        if parts.is_empty() {
            #[cfg(windows)]
            {
                (shell_program(), Vec::<String>::new())
            }
            #[cfg(not(windows))]
            {
                (String::from("login"), Vec::<String>::new())
            }
        } else {
            let prog = parts[0].clone();
            let rest: Vec<String> = if parts.len() > 1 {
                parts[1..].to_vec()
            } else {
                vec![]
            };
            (prog, rest)
        }
    } else {
        #[cfg(windows)]
        {
            // ConPTY starts cmd.exe natively; Unix shell integration is not applicable.
            (shell_program(), Vec::<String>::new())
        }
        #[cfg(not(windows))]
        {
            match super::shell_integration::write_integration_files(&session_uuid) {
                Ok(paths) => {
                    let (prog, prog_args) = super::shell_integration::integration_command(&paths);
                    let base = prog.rsplit('/').next().unwrap_or("").to_string();
                    if base == "zsh" {
                        env_overrides
                            .push(("ZDOTDIR".to_string(), paths.zshrc_dir.display().to_string()));
                    }
                    (prog, prog_args)
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to write dsterm integration files: {e}; falling back to login"
                    );
                    (String::from("login"), Vec::<String>::new())
                }
            }
        }
    };

    tracing::info!(
        "[{}] create_terminal: shell_program={:?} args={:?} cwd={:?}",
        trace_ts(),
        program,
        args,
        options.cwd
    );

    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    // --- Try the standard portable-pty path first ---
    let pty_system = native_pty_system();
    let openpty_result = pty_system.openpty(size);

    let std_result = match openpty_result {
        Ok(pair) => {
            tracing::info!("[{}] create_terminal: openpty succeeded", trace_ts());
            let mut cmd = CommandBuilder::new(&program);
            if let Some(dir) = options.cwd.as_deref() {
                cmd.cwd(dir);
            }
            for (k, v) in &env_overrides {
                cmd.env(k, v);
            }
            if !args.is_empty() {
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                cmd.args(arg_refs);
            }
            match pair.slave.spawn_command(cmd) {
                Ok(child) => {
                    tracing::info!(
                        "[{}] create_terminal: spawn_command succeeded (program={})",
                        trace_ts(),
                        program
                    );
                    Ok((pair.master, child))
                }
                Err(e) => {
                    // openpty succeeded but spawn failed — this is a command
                    // error (e.g. missing program), not a PTY capability issue.
                    // Do NOT fall back; report immediately.
                    tracing::error!("spawn_command failed: {}", e);
                    return Json(ErrorResponse {
                        error: format!("Failed to spawn command: {e}"),
                    })
                    .into_response();
                }
            }
        }
        Err(e) => Err(e),
    };

    // --- If openpty itself failed, fall back to TIOCGPTPEER on Unix ---
    let (master, mut child) = match std_result {
        Ok(pair) => pair,
        Err(e) => {
            #[cfg(unix)]
            {
                tracing::warn!(
                    "Standard openpty failed ({}), trying TIOCGPTPEER fallback",
                    e
                );
                match fallback_open_and_spawn(size, &program, &args, options.cwd.as_deref()) {
                    Ok(pair) => pair,
                    Err(fb_err) => {
                        tracing::error!("TIOCGPTPEER fallback also failed: {}", fb_err);
                        return Json(ErrorResponse {
                            error: format!(
                                "Failed to open PTY: {e}; TIOCGPTPEER fallback: {fb_err}"
                            ),
                        })
                        .into_response();
                    }
                }
            }
            #[cfg(not(unix))]
            {
                tracing::error!("Native PTY open failed: {}", e);
                return Json(ErrorResponse {
                    error: format!("Failed to open PTY: {e}"),
                })
                .into_response();
            }
        }
    };

    // --- Common session setup ---
    let pid = child.process_id().unwrap_or(0);
    tracing::info!("[{}] create_terminal: process id = {}", trace_ts(), pid);
    tracing::info!("Terminal created successfully with PID: {}", pid);

    let reader = match master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to clone PTY reader: {}", e);
            let _ = child.kill();
            let _ = child.wait();
            return Json(ErrorResponse {
                error: format!("Failed to clone PTY reader: {e}"),
            })
            .into_response();
        }
    };
    let writer = match master.take_writer() {
        Ok(w) => Arc::new(Mutex::new(w)),
        Err(e) => {
            tracing::error!("Failed to take PTY writer: {}", e);
            let _ = child.kill();
            let _ = child.wait();
            return Json(ErrorResponse {
                error: format!("Failed to take PTY writer: {e}"),
            })
            .into_response();
        }
    };
    let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(master));
    let child_killer = Arc::new(Mutex::new(child.clone_killer()));

    let scrollback = Arc::new(Scrollback::new(pid));
    let terminal_id = uuid::Uuid::new_v4().to_string();
    let (output_tx, _) = broadcast::channel::<Vec<u8>>(256);
    let (command_exit_tx, _) = broadcast::channel::<String>(64);
    let osc_leftover: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let exit_status: Arc<std::sync::Mutex<Option<bool>>> = Arc::new(std::sync::Mutex::new(None));
    let exit_notify = Arc::new(tokio::sync::Notify::new());

    // Background PTY reader — runs for the session lifetime
    {
        let scrollback = scrollback.clone();
        let output_tx = output_tx.clone();
        let command_exit_tx = command_exit_tx.clone();
        let osc_leftover = osc_leftover.clone();
        spawn_blocking(move || {
            let mut reader = reader;
            let buf_size = super::get_config().terminal.read_buffer_bytes;
            let mut read_buffer = vec![0u8; buf_size];
            let mut first_bytes_logged = false;
            loop {
                match reader.read(&mut read_buffer) {
                    Ok(0) => {
                        tracing::info!("[{}] PTY reader EOF for PID {}", trace_ts(), pid);
                        break;
                    }
                    Ok(n) => {
                        if !first_bytes_logged {
                            tracing::info!(
                                "[{}] PTY reader first bytes received for PID {} ({} bytes)",
                                trace_ts(),
                                pid,
                                n
                            );
                            first_bytes_logged = true;
                        }
                        let data = &read_buffer[..n];
                        let mut leftover_guard = osc_leftover.lock().unwrap();
                        let (stripped, exits) = strip_osc_633(data, &mut leftover_guard);
                        drop(leftover_guard);
                        let _ = scrollback.append(&stripped);

                        let _ = output_tx.send(stripped);

                        if !exits.is_empty() {
                            for code in exits {
                                let msg = serde_json::to_string(&CommandExitMessage {
                                    msg_type: "command_exit".to_string(),
                                    exit_code: code,
                                })
                                .unwrap_or_default();
                                let _ = command_exit_tx.send(msg);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("[{}] PTY reader error for PID {}: {}", trace_ts(), pid, e);
                        break;
                    }
                }
            }
            tracing::info!("Background PTY reader exited for PID {}", pid);
        });
    }

    // Background child waiter — signals when process exits
    {
        let exit_status = exit_status.clone();
        let exit_notify = exit_notify.clone();
        let child = Arc::new(std::sync::Mutex::new(child));
        spawn_blocking(move || {
            let mut child_guard = child.lock().unwrap();
            let status = match child_guard.wait() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(
                        "[{}] child waiter wait() error for PID {}: {}",
                        trace_ts(),
                        pid,
                        e
                    );
                    return;
                }
            };
            let success = status.success();
            let code = status.exit_code();
            *exit_status.lock().unwrap() = Some(success);
            exit_notify.notify_waiters();
            tracing::info!(
                "[{}] child waiter exited for PID {} (success={}, exit_code={})",
                trace_ts(),
                pid,
                success,
                code
            );
        });
    }

    let session = TerminalSession {
        terminal_id: terminal_id.clone(),
        master,
        child_killer,
        writer,
        scrollback,
        output_tx,
        command_exit_tx,
        osc_leftover,
        exit_status,
        exit_notify,
        last_accessed: Arc::new(Mutex::new(SystemTime::now())),
    };

    SESSIONS_CREATED_TOTAL.fetch_add(1, Ordering::Relaxed);
    sessions.insert(pid, session);
    tracing::info!(
        "[{}] create_terminal: returning HTTP 200 with pid={}",
        trace_ts(),
        pid
    );
    (
        axum::http::StatusCode::OK,
        [("x-dsterm-terminal-id", terminal_id)],
        pid.to_string(),
    )
        .into_response()
}

pub async fn list_terminals(State(sessions): State<Sessions>) -> impl IntoResponse {
    let terminals: Vec<serde_json::Value> = sessions
        .iter()
        .map(|entry| {
            serde_json::json!({ "pid": *entry.key(), "terminalId": entry.value().terminal_id.clone() })
        })
        .collect();
    Json(serde_json::json!({ "terminals": terminals })).into_response()
}

pub async fn resize_terminal(
    State(sessions): State<Sessions>,
    Path(pid): Path<u32>,
    Json(options): Json<TerminalOptions>,
) -> impl IntoResponse {
    let rows = parse_u16(&options.rows, "rows").expect("Failed");
    let cols = parse_u16(&options.cols, "cols").expect("Failed");
    tracing::info!("Resizing terminal {} to cols={}, rows={}", pid, cols, rows);

    if let Some(session) = sessions.get(&pid) {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        match session.master.lock().await.resize(size) {
            Ok(_) => Json(serde_json::json!({"success": true})).into_response(),
            Err(e) => Json(ErrorResponse {
                error: format!("Failed to resize: {e}"),
            })
            .into_response(),
        }
    } else {
        Json(ErrorResponse {
            error: "Session not found".to_string(),
        })
        .into_response()
    }
}

pub async fn terminal_websocket(
    ws: WebSocketUpgrade,
    Path(pid): Path<u32>,
    State(sessions): State<Sessions>,
) -> impl IntoResponse {
    tracing::info!("WebSocket connection request for terminal {}", pid);
    ws.on_upgrade(move |socket| handle_socket(socket, pid, sessions))
}

async fn handle_socket(socket: WebSocket, pid: u32, sessions: Sessions) {
    let (mut sender, mut receiver) = socket.split();
    tracing::info!(
        "[{}] handle_socket entered for PID {} (websocket upgraded)",
        trace_ts(),
        pid
    );

    let (
        writer,
        scrollback,
        output_tx,
        command_exit_tx,
        _osc_leftover_arc,
        exit_status_arc,
        exit_notify,
    ) = {
        let Some(session) = sessions.get(&pid) else {
            tracing::error!(
                "[{}] handle_socket: session NOT found for PID {}",
                trace_ts(),
                pid
            );
            return;
        };

        tracing::info!(
            "[{}] handle_socket: session found for PID {}",
            trace_ts(),
            pid
        );

        *session.last_accessed.lock().await = SystemTime::now();
        tracing::info!("WebSocket connection established for terminal {}", pid);

        (
            session.writer.clone(),
            session.scrollback.clone(),
            session.output_tx.clone(),
            session.command_exit_tx.clone(),
            session.osc_leftover.clone(),
            session.exit_status.clone(),
            session.exit_notify.clone(),
        )
    };

    // Check if process already exited
    let already_exited = {
        let guard = exit_status_arc.lock().unwrap();
        let v = *guard;
        tracing::info!(
            "[{}] handle_socket: exit_status for PID {} = {:?}",
            trace_ts(),
            pid,
            v
        );
        v
    };
    tracing::info!(
        "[{}] handle_socket: already_exited decision for PID {} = {}",
        trace_ts(),
        pid,
        already_exited.is_some()
    );
    if let Some(success) = already_exited {
        let exit_message = ProcessExitMessage {
            exit_code: Some(if success { 0 } else { 1 }),
            signal: None,
            message: if success {
                "Process exited successfully"
            } else {
                "Process exited with non-zero status"
            }
            .to_string(),
        };
        let exit_json = serde_json::to_string(&exit_message).unwrap_or_default();
        let _ = sender
            .send(Message::Text(
                format!("{{\"type\":\"exit\",\"data\":{exit_json}}}").into(),
            ))
            .await;
        sessions.remove(&pid);
        return;
    }

    let mut ws_output_rx = output_tx.subscribe();
    let mut cmd_exit_rx = command_exit_tx.subscribe();

    // Send full scrollback history (client should clear terminal before connecting)
    let mut first_ws_output = true;
    let scrollback_for_replay = scrollback.clone();
    let scrollback_limit = super::get_config().terminal.max_scrollback_bytes;
    match spawn_blocking(move || scrollback_for_replay.read_tail(scrollback_limit)).await {
        Ok(Ok(contents)) if !contents.is_empty() => {
            tracing::info!(
                "[{}] handle_socket: scrollback replay sent for PID {} (first output)",
                trace_ts(),
                pid
            );
            let _ = sender.send(Message::Binary(Bytes::from(contents))).await;
        }
        Ok(Err(e)) => {
            tracing::warn!("Failed to read scrollback for terminal {}: {}", pid, e);
        }
        _ => {}
    }

    // WS input → PTY writer channel
    let (ws_input_tx, ws_input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let write_handle = {
        let writer = writer.clone();
        spawn_blocking(move || {
            let mut first_input_logged = false;
            while let Ok(data) = ws_input_rx.recv() {
                if !first_input_logged {
                    tracing::info!(
                        "[{}] PTY writer first client input for PID {} ({} bytes)",
                        trace_ts(),
                        pid,
                        data.len()
                    );
                    first_input_logged = true;
                }
                let mut guard = writer.blocking_lock();
                if guard.write_all(&data).is_err() || guard.flush().is_err() {
                    tracing::error!("[{}] PTY writer failed for PID {}", trace_ts(), pid);
                    break;
                }
            }
        })
    };

    // Main loop with output coalescing
    let mut coalesce_buf: Vec<u8> = Vec::with_capacity(16384);
    let coalesce_ms = super::get_config().terminal.output_coalesce_ms;
    let mut interval = tokio::time::interval(Duration::from_millis(coalesce_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut close_reason: &str = "unknown";
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if !coalesce_buf.is_empty() {
                    let frame = std::mem::replace(&mut coalesce_buf, Vec::with_capacity(16384));
                    if sender.send(Message::Binary(Bytes::from(frame))).await.is_err() {
                        close_reason = "output flush send failed";
                        break;
                    }
                }
            }
            data = ws_output_rx.recv() => {
                match data {
                    Ok(data) => {
                        coalesce_buf.extend_from_slice(&data);
                        if coalesce_buf.len() >= super::get_config().terminal.read_buffer_bytes {
                            let frame = std::mem::replace(&mut coalesce_buf, Vec::with_capacity(16384));
                            if sender.send(Message::Binary(Bytes::from(frame))).await.is_err() {
                                close_reason = "output send failed";
                                break;
                            }
                            if first_ws_output {
                                tracing::info!(
                                    "[{}] handle_socket: first live output sent for PID {}",
                                    trace_ts(),
                                    pid
                                );
                                first_ws_output = false;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        close_reason = "output broadcast closed";
                        break;
                    }
                }
            }
            maybe_msg = cmd_exit_rx.recv() => {
                match maybe_msg {
                    Ok(json) => {
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            close_reason = "command_exit send failed";
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        close_reason = "command_exit broadcast closed";
                        break;
                    }
                }
            }
            _ = exit_notify.notified() => {
                // Give the reader a moment to flush remaining output
                tokio::time::sleep(Duration::from_millis(50)).await;

                while let Ok(data) = ws_output_rx.try_recv() {
                    coalesce_buf.extend_from_slice(&data);
                }
                if !coalesce_buf.is_empty() {
                    let _ = sender.send(Message::Binary(Bytes::from(std::mem::take(&mut coalesce_buf)))).await;
                }

                let success = exit_status_arc.lock().unwrap().unwrap_or(false);
                let exit_message = ProcessExitMessage {
                    exit_code: Some(if success { 0 } else { 1 }),
                    signal: None,
                    message: if success {
                        "Process exited successfully"
                    } else {
                        "Process exited with non-zero status"
                    }
                    .to_string(),
                };
                let exit_json = serde_json::to_string(&exit_message).unwrap_or_default();
                let _ = sender
                    .send(Message::Text(
                        format!("{{\"type\":\"exit\",\"data\":{exit_json}}}").into(),
                    ))
                    .await;

                sessions.remove(&pid);
                close_reason = "process exited";
                break;
            }
            msg = receiver.next() => {
                match msg {
                    Some(Ok(message)) => {
                        let data = match message {
                            Message::Text(text) => text.as_bytes().to_vec(),
                            Message::Binary(data) => data.to_vec(),
                            Message::Close(_) => {
                                close_reason = "client close";
                                break;
                            }
                            _ => continue,
                        };
                        if ws_input_tx.send(data).is_err() {
                            close_reason = "input forward failed";
                            break;
                        }
                    }
                    None | Some(Err(_)) => {
                        close_reason = "client stream ended";
                        break;
                    }
                }
            }
        }
    }

    drop(ws_input_tx);
    let _ = write_handle.await;

    tracing::info!(
        "[{}] WebSocket disconnected for terminal {} (reason: {})",
        trace_ts(),
        pid,
        close_reason
    );
}

pub async fn terminate_terminal(
    State(sessions): State<Sessions>,
    Path(pid): Path<u32>,
) -> impl IntoResponse {
    tracing::info!("Terminating terminal {}", pid);

    if let Some((_, session)) = sessions.remove(&pid) {
        let result = session.child_killer.lock().await.kill();

        drop(session.writer.lock().await);
        session.scrollback.cleanup();

        match result {
            Ok(_) => {
                tracing::info!("Terminal {} terminated successfully", pid);
                Json(serde_json::json!({"success": true})).into_response()
            }
            Err(e) if e.raw_os_error() == Some(10035) => {
                tracing::warn!(
                    "Terminal {} kill returned WSAEWOULDBLOCK (os error 10035); \
                     session already removed, treating as terminated",
                    pid
                );
                Json(serde_json::json!({"success": true})).into_response()
            }
            Err(e) => {
                tracing::error!("Failed to terminate terminal {}: {}", pid, e);
                Json(ErrorResponse {
                    error: format!("Failed to terminate terminal {pid}: {e}"),
                })
                .into_response()
            }
        }
    } else {
        tracing::error!("Failed to terminate terminal {}: session not found", pid);
        Json(ErrorResponse {
            error: "Session not found".to_string(),
        })
        .into_response()
    }
}

pub async fn execute_command(Json(options): Json<ExecuteCommandOption>) -> impl IntoResponse {
    let cwd = options.cwd.or(options.u_cwd).unwrap_or("".to_string());

    tracing::info!(
        command = %options.command,
        cwd = %cwd,
        "Executing command"
    );

    let shell = shell_program();
    let cwd = if cwd.is_empty() {
        default_working_directory()
    } else {
        PathBuf::from(cwd)
    };

    if !cwd.exists() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(CommandResponse {
                output: String::new(),
                error: Some("Working directory does not exist".to_string()),
            }),
        )
            .into_response();
    }

    let command = options.command.clone();

    let result = spawn_blocking(move || {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(size)?;

        let mut cmd = CommandBuilder::new(shell);
        #[cfg(windows)]
        cmd.args(["/C", &command]);
        #[cfg(not(windows))]
        cmd.args(["-c", &command]);
        cmd.cwd(cwd);

        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

        let read_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let timeout_duration = Duration::from_secs(30);
        let start_time = SystemTime::now();
        let mut output = Vec::new();

        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(data) => {
                    output.extend(data);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if start_time.elapsed().unwrap_or_default() > timeout_duration {
                        child.kill()?;
                        return Err("Command execution timed out".into());
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
        }

        drop(writer);
        let _ = read_thread.join();
        child.wait()?;

        Ok::<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>(output)
    })
    .await;

    match result {
        Ok(Ok(output)) => {
            let output_str = String::from_utf8_lossy(&output).into_owned();

            let ansi_regex =
                Regex::new(r"\x1B\[([0-9]{1,2}(;[0-9]{1,2})?)?[m|K]|\x1B\[[0-9]+[A-Za-z]").unwrap();
            let cleaned_output = ansi_regex.replace_all(&output_str, "").to_string();

            tracing::info!(
                output_length = cleaned_output.len(),
                "Command completed successfully"
            );

            (
                axum::http::StatusCode::OK,
                Json(CommandResponse {
                    output: cleaned_output,
                    error: None,
                }),
            )
                .into_response()
        }
        Ok(Err(e)) => {
            tracing::error!("Command execution failed: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(CommandResponse {
                    output: String::new(),
                    error: Some(e.to_string()),
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Blocking task failed: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(CommandResponse {
                    output: String::new(),
                    error: Some("Internal server error".to_string()),
                }),
            )
                .into_response()
        }
    }
}

pub async fn silent_exec(Json(options): Json<SilentExecRequest>) -> impl IntoResponse {
    let id = options.id.clone();
    let command = options.command.clone();
    let cwd = options.cwd.clone();
    let env = options.env.clone();
    let timeout_ms = options.timeout_ms.unwrap_or(30000);

    tracing::info!(id = %id, command = %command, cwd = ?cwd, "Executing silent command");

    if command.trim().is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(SilentExecResponse {
                msg_type: "silent_exec_result".to_string(),
                id,
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: "Empty command string".to_string(),
                timed_out: false,
            }),
        )
            .into_response();
    }

    let cwd_path = if let Some(c) = cwd {
        PathBuf::from(c)
    } else {
        default_working_directory()
    };

    if !cwd_path.exists() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(SilentExecResponse {
                msg_type: "silent_exec_result".to_string(),
                id,
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: "Working directory does not exist".to_string(),
                timed_out: false,
            }),
        )
            .into_response();
    }

    let result = execute_silent_command(command, cwd_path, env, timeout_ms).await;

    match result {
        Ok((exit_code, stdout, stderr, timed_out)) => {
            let success = exit_code == 0;
            (
                axum::http::StatusCode::OK,
                Json(SilentExecResponse {
                    msg_type: "silent_exec_result".to_string(),
                    id,
                    success,
                    exit_code,
                    stdout,
                    stderr,
                    timed_out,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Silent command execution failed: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(SilentExecResponse {
                    msg_type: "silent_exec_result".to_string(),
                    id,
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: e,
                    timed_out: false,
                }),
            )
                .into_response()
        }
    }
}

async fn execute_silent_command(
    command: String,
    cwd: PathBuf,
    env: Option<HashMap<String, String>>,
    timeout_ms: u64,
) -> Result<(i32, String, String, bool), String> {
    let mut cmd = Command::new(shell_program());
    #[cfg(windows)]
    cmd.arg("/C").arg(&command);
    #[cfg(not(windows))]
    cmd.arg("-c").arg(&command);
    cmd.current_dir(&cwd);

    if let Some(env_vars) = env {
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to spawn command: {}", e)),
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let timeout_duration = Duration::from_millis(timeout_ms);

    let read_stdout = async {
        if let Some(mut stdout) = stdout {
            let mut buf = String::new();
            use tokio::io::AsyncReadExt;
            let _ = stdout.read_to_string(&mut buf).await;
            buf
        } else {
            String::new()
        }
    };

    let read_stderr = async {
        if let Some(mut stderr) = stderr {
            let mut buf = String::new();
            use tokio::io::AsyncReadExt;
            let _ = stderr.read_to_string(&mut buf).await;
            buf
        } else {
            String::new()
        }
    };

    let wait_child = async { tokio::time::timeout(timeout_duration, child.wait()).await };

    let (stdout_result, stderr_result, wait_result) =
        tokio::join!(read_stdout, read_stderr, wait_child);

    match wait_result {
        Ok(Ok(status)) => {
            let exit_code = status.code().unwrap_or(-1);
            Ok((exit_code, stdout_result, stderr_result, false))
        }
        Ok(Err(e)) => Err(format!("Failed to wait for command: {}", e)),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Ok((-1, String::new(), "Command timed out".to_string(), true))
        }
    }
}

pub async fn silent_exec_stream(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket| async move {
        handle_silent_exec_stream(socket).await;
    })
}

async fn handle_silent_exec_stream(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    let msg = match receiver.next().await {
        Some(Ok(Message::Text(text))) => text.to_string(),
        Some(Ok(Message::Binary(data))) => String::from_utf8_lossy(&data).to_string(),
        _ => {
            tracing::warn!("Silent exec stream: expected initial message");
            return;
        }
    };

    let options: SilentExecStreamRequest = match serde_json::from_str(&msg) {
        Ok(opt) => opt,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&SilentExecResponse {
                        msg_type: "silent_exec_result".to_string(),
                        id: String::new(),
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: format!("Invalid request: {}", e),
                        timed_out: false,
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
            return;
        }
    };

    let id = options.id.clone();
    let command = options.command.clone();
    let cwd = options.cwd.clone();
    let env = options.env.clone();
    let timeout_ms = options.timeout_ms.unwrap_or(60000);

    tracing::info!(id = %id, command = %command, cwd = ?cwd, "Starting silent stream command");

    if command.trim().is_empty() {
        let _ = sender
            .send(Message::Text(
                serde_json::to_string(&SilentExecResponse {
                    msg_type: "silent_exec_result".to_string(),
                    id,
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: "Empty command string".to_string(),
                    timed_out: false,
                })
                .unwrap()
                .into(),
            ))
            .await;
        return;
    }

    let cwd_path = if let Some(c) = cwd {
        PathBuf::from(c)
    } else {
        default_working_directory()
    };

    if !cwd_path.exists() {
        let _ = sender
            .send(Message::Text(
                serde_json::to_string(&SilentExecResponse {
                    msg_type: "silent_exec_result".to_string(),
                    id,
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: "Working directory does not exist".to_string(),
                    timed_out: false,
                })
                .unwrap()
                .into(),
            ))
            .await;
        return;
    }

    let mut cmd = Command::new(shell_program());
    #[cfg(windows)]
    cmd.arg("/C").arg(&command);
    #[cfg(not(windows))]
    cmd.arg("-c").arg(&command);
    cmd.current_dir(&cwd_path);

    if let Some(env_vars) = env {
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&SilentExecResponse {
                        msg_type: "silent_exec_result".to_string(),
                        id,
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: format!("Failed to spawn command: {}", e),
                        timed_out: false,
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let mut stdout_reader = tokio::io::BufReader::new(stdout);
    let mut stderr_reader = tokio::io::BufReader::new(stderr);

    let (stdout_tx, mut stdout_rx) = tokio::sync::mpsc::channel::<String>(100);
    let (stderr_tx, mut stderr_rx) = tokio::sync::mpsc::channel::<String>(100);

    let id_stdout = id.clone();
    let id_stderr = id.clone();

    let stdout_task = tokio::spawn(async move {
        let mut buf = String::new();
        while let Ok(n) = stdout_reader.read_line(&mut buf).await {
            if n == 0 {
                break;
            }
            let chunk = SilentExecChunk {
                msg_type: "silent_exec_chunk".to_string(),
                id: id_stdout.clone(),
                stream: "stdout".to_string(),
                data: buf.clone(),
            };
            if stdout_tx
                .send(serde_json::to_string(&chunk).unwrap())
                .await
                .is_err()
            {
                break;
            }
            buf.clear();
        }
    });

    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        while let Ok(n) = stderr_reader.read_line(&mut buf).await {
            if n == 0 {
                break;
            }
            let chunk = SilentExecChunk {
                msg_type: "silent_exec_chunk".to_string(),
                id: id_stderr.clone(),
                stream: "stderr".to_string(),
                data: buf.clone(),
            };
            if stderr_tx
                .send(serde_json::to_string(&chunk).unwrap())
                .await
                .is_err()
            {
                break;
            }
            buf.clear();
        }
    });

    loop {
        tokio::select! {
            msg = stdout_rx.recv() => {
                if let Some(msg) = msg {
                    let _ = sender.send(Message::Text(msg.into())).await;
                }
            }
            msg = stderr_rx.recv() => {
                if let Some(msg) = msg {
                    let _ = sender.send(Message::Text(msg.into())).await;
                }
            }
            status = child.wait() => {
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                let (exit_code, timed_out) = match status {
                    Ok(s) => (s.code().unwrap_or(-1), false),
                    Err(_) => (-1, true),
                };
                let done = SilentExecDone {
                    msg_type: "silent_exec_done".to_string(),
                    id,
                    exit_code,
                    timed_out,
                };
                let _ = sender.send(Message::Text(serde_json::to_string(&done).unwrap().into())).await;
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                let done = SilentExecDone {
                    msg_type: "silent_exec_done".to_string(),
                    id,
                    exit_code: -1,
                    timed_out: true,
                };
                let _ = sender.send(Message::Text(serde_json::to_string(&done).unwrap().into())).await;
                break;
            }
        }
    }
}

pub async fn get_metrics(State(sessions): State<Sessions>) -> impl IntoResponse {
    let active = sessions.len();
    let total = SESSIONS_CREATED_TOTAL.load(Ordering::Relaxed);
    let body = format!(
        "# HELP dsterm_terminal_sessions_total Terminal sessions created since startup\n\
         # TYPE dsterm_terminal_sessions_total counter\n\
         dsterm_terminal_sessions_total {total}\n\
         # HELP dsterm_terminal_sessions_active Currently active terminal sessions\n\
         # TYPE dsterm_terminal_sessions_active gauge\n\
         dsterm_terminal_sessions_active {active}\n"
    );
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_silent_command_success() {
        let result = execute_silent_command(
            "echo hello".to_string(),
            std::env::current_dir().unwrap(),
            None,
            5000,
        )
        .await;
        assert!(result.is_ok());
        let (exit_code, stdout, stderr, timed_out) = result.unwrap();
        assert_eq!(exit_code, 0);
        assert!(stdout.contains("hello"));
        assert!(stderr.is_empty());
        assert!(!timed_out);
    }

    #[tokio::test]
    async fn test_execute_silent_command_failure() {
        let result = execute_silent_command(
            "exit 1".to_string(),
            std::env::current_dir().unwrap(),
            None,
            5000,
        )
        .await;
        assert!(result.is_ok());
        let (exit_code, stdout, _stderr, timed_out) = result.unwrap();
        assert_eq!(exit_code, 1);
        assert!(stdout.is_empty());
        assert!(!timed_out);
    }

    #[tokio::test]
    async fn test_execute_silent_command_with_env() {
        let mut env = std::collections::HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());
        let result = execute_silent_command(
            "echo $TEST_VAR".to_string(),
            std::env::current_dir().unwrap(),
            Some(env),
            5000,
        )
        .await;
        assert!(result.is_ok());
        let (exit_code, stdout, _, timed_out) = result.unwrap();
        assert_eq!(exit_code, 0);
        assert!(stdout.contains("test_value"));
        assert!(!timed_out);
    }

    #[tokio::test]
    async fn test_execute_silent_command_timeout() {
        let result = execute_silent_command(
            "sleep 10".to_string(),
            std::env::current_dir().unwrap(),
            None,
            100,
        )
        .await;
        assert!(result.is_ok());
        let (exit_code, stdout, stderr, timed_out) = result.unwrap();
        assert_eq!(exit_code, -1);
        assert!(stdout.is_empty());
        assert!(stderr.contains("timed out"));
        assert!(timed_out);
    }

    #[tokio::test]
    async fn test_execute_silent_command_stderr() {
        let result = execute_silent_command(
            "echo error >&2".to_string(),
            std::env::current_dir().unwrap(),
            None,
            5000,
        )
        .await;
        assert!(result.is_ok());
        let (exit_code, stdout, stderr, timed_out) = result.unwrap();
        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.contains("error"));
        assert!(!timed_out);
    }
}
