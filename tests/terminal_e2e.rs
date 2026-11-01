// Real PTY lifecycle e2e: spawn the compiled dsterm binary, create a
// session over HTTP, drive raw stdin through its WebSocket, resize it
// over HTTP, terminate it, and assert the exit frame plus /metrics
// bookkeeping. Execution proof is the shell dumping Cargo.toml's
// "[package]" header — content that can only come from the command
// actually running, not from the tty echoing our input back.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsClient = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn dsterm() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dsterm"))
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn wait_for_port(port: u16) {
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("listener on {port} never came up");
}

struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Prometheus sample parser: value of `<name> <value>` (skips # lines).
fn metric(body: &str, name: &str) -> Option<i64> {
    let prefix = format!("{name} ");
    body.lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .and_then(|v| v.trim().parse().ok())
}

/// Read WS frames until `accept` passes or the deadline runs out;
/// control Text frames and binary PTY output accumulate into `seen`.
/// Terminal queries (`ESC[6n` cursor-position reports) get the reply a
/// real terminal would send, so PSReadLine-style shells cannot block
/// waiting for one.
async fn read_until<F>(ws: &mut WsClient, seen: &mut String, what: &str, secs: u64, accept: F)
where
    F: Fn(&str) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for {what}; got {seen:?}");
        }
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => panic!("ws read error while waiting for {what}: {e}"),
            Ok(None) => panic!("ws closed while waiting for {what}; got {seen:?}"),
            Err(_) => panic!("timed out waiting for {what}; got {seen:?}"),
        };
        let chunk = match msg {
            Message::Binary(data) => String::from_utf8_lossy(&data).into_owned(),
            Message::Text(text) => text,
            Message::Close(_) => panic!("ws closed while waiting for {what}; got {seen:?}"),
            _ => continue,
        };
        for _ in chunk.matches("\u{1b}[6n") {
            let _ = ws.send(Message::Binary(b"\x1b[1;1R".to_vec())).await;
        }
        seen.push_str(&chunk);
        if accept(seen) {
            return;
        }
    }
}

#[tokio::test]
async fn terminal_lifecycle_e2e() {
    let port = free_port();
    let mut spawn = Command::new(dsterm());
    spawn
        .args(["-p", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Windows: cmd.exe instead of PowerShell — same console shell
    // class, but without PSReadLine's ESC[6n cursor query; a
    // non-powershell DSTERM_SHELL gets no extra args (windows_shell_args).
    #[cfg(windows)]
    spawn.env("DSTERM_SHELL", "cmd.exe");
    let _server = ServerGuard(spawn.spawn().expect("spawn dsterm server"));
    wait_for_port(port);

    let base = format!("http://127.0.0.1:{port}");
    let client = http_client();

    // --- create (retry briefly: port is open, router may not be) ---
    // Explicit cwd = the package root (compile-time), so the probe below
    // never depends on where the server happened to be launched from.
    let repo_root = env!("CARGO_MANIFEST_DIR");
    let pid = {
        let mut body: Option<String> = None;
        let mut header: Option<String> = None;
        for _ in 0..50 {
            let resp = client
                .post(format!("{base}/terminals"))
                .json(&serde_json::json!({ "cols": 80, "rows": 24, "cwd": repo_root }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    header = r
                        .headers()
                        .get("x-dsterm-terminal-id")
                        .map(|v| v.to_str().unwrap().to_string());
                    body = Some(r.text().await.unwrap());
                    break;
                }
                Ok(r) => {
                    panic!(
                        "POST /terminals failed: {} {}",
                        r.status(),
                        r.text().await.unwrap()
                    )
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        let body = body.expect("POST /terminals never succeeded");
        let pid: u32 = body.trim().parse().expect("pid body");
        let terminal_id = header.expect("x-dsterm-terminal-id header missing");
        assert!(!terminal_id.is_empty());
        pid
    };

    // --- list contains the new session ---
    let listed = client
        .get(format!("{base}/terminals"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(listed.contains(&format!("\"pid\":{pid}")), "{listed}");

    // --- websocket: raw stdin in, PTY output out ---
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/terminals/{pid}"))
        .await
        .expect("ws connect");

    // Execution proof: dump Cargo.toml and look for "[package]" — a
    // string our input never contains, so the tty echoing stdin back
    // cannot fake it. Platform default shell differs on type vs cat.
    #[cfg(windows)]
    let probe = "type Cargo.toml";
    #[cfg(not(windows))]
    let probe = "cat Cargo.toml";
    let marker = format!("dsterm-e2e-{port}");
    ws.send(Message::Binary(
        format!("echo {marker}\r{probe}\r\n").into_bytes(),
    ))
    .await
    .expect("send stdin");

    let mut output = String::new();
    read_until(
        &mut ws,
        &mut output,
        "the shell dumping Cargo.toml",
        20,
        |s| s.contains("[package]"),
    )
    .await;
    assert!(output.contains(&marker), "marker echo missing: {output:?}");

    // --- resize ---
    let resized = client
        .post(format!("{base}/terminals/{pid}/resize"))
        .json(&serde_json::json!({ "cols": 132, "rows": 43 }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(resized.contains("\"success\":true"), "{resized}");

    // --- metrics: one active session, one created ---
    let metrics_before = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        metric(&metrics_before, "dsterm_terminal_sessions_active"),
        Some(1)
    );
    let created_before =
        metric(&metrics_before, "dsterm_terminal_sessions_total").expect("total metric present");
    assert!(created_before >= 1, "{metrics_before}");

    // --- terminate ---
    let terminated = client
        .post(format!("{base}/terminals/{pid}/terminate"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(terminated.contains("\"success\":true"), "{terminated}");

    // The still-open ws receives the process exit control frame...
    let mut exit_seen = String::new();
    read_until(&mut ws, &mut exit_seen, "the exit control frame", 10, |s| {
        s.contains("\"type\":\"exit\"")
    })
    .await;

    // --- list no longer has it; metrics back to zero, total frozen ---
    let listed_after = client
        .get(format!("{base}/terminals"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !listed_after.contains(&format!("\"pid\":{pid}")),
        "{listed_after}"
    );

    let metrics_after = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        metric(&metrics_after, "dsterm_terminal_sessions_active"),
        Some(0)
    );
    assert_eq!(
        metric(&metrics_after, "dsterm_terminal_sessions_total"),
        Some(created_before),
        "terminate must not bump the created counter: {metrics_after}"
    );

    drop(ws);
}
