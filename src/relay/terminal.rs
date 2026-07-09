#![allow(dead_code)]

use crate::protocol::OutgoingMsg;
use crate::relay::loopback;
use crate::relay::wire::ClientCtx;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

struct TerminalHandle {
    input_tx: mpsc::Sender<Vec<u8>>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct RelayTerminals {
    inner: Arc<DashMap<String, TerminalHandle>>,
}

impl RelayTerminals {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        ctx: &ClientCtx,
        http: &reqwest::Client,
        port: u16,
        req_id: Option<String>,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
    ) {
        let body = json!({ "cols": cols, "rows": rows, "cwd": cwd });
        let pid = match loopback::post_text(http, port, "/terminals", &body).await {
            Ok(text) => text.trim().to_string(),
            Err(e) => {
                ctx.send_error(req_id, format!("terminal create failed: {e}"))
                    .await;
                return;
            }
        };
        if pid.parse::<u32>().is_err() {
            ctx.send_error(
                req_id,
                format!("terminal create returned invalid response: {pid}"),
            )
            .await;
            return;
        }

        let url = format!("ws://127.0.0.1:{port}/terminals/{pid}");
        let ws = match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                ctx.send_error(req_id, format!("terminal attach failed: {e}"))
                    .await;
                return;
            }
        };
        let (mut ws_write, mut ws_read) = ws.split();

        let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);
        let task_ctx = ctx.clone();
        let terminal_id = pid.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    incoming = ws_read.next() => {
                        match incoming {
                            Some(Ok(Message::Binary(bytes))) => {
                                task_ctx
                                    .send(OutgoingMsg::TerminalData {
                                        terminal_id: terminal_id.clone(),
                                        data: BASE64.encode(&bytes[..]),
                                    })
                                    .await;
                            }
                            Some(Ok(Message::Text(text))) => {
                                task_ctx
                                    .send(OutgoingMsg::TerminalEvent {
                                        terminal_id: terminal_id.clone(),
                                        event: text.to_string(),
                                    })
                                    .await;
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Err(_)) => break,
                            _ => {}
                        }
                    }
                    input = input_rx.recv() => {
                        match input {
                            Some(bytes) => {
                                if ws_write.send(Message::Binary(bytes)).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        self.inner
            .insert(pid.clone(), TerminalHandle { input_tx, task });
        ctx.send_result(req_id, json!({ "terminalId": pid })).await;
    }

    pub async fn data(&self, ctx: &ClientCtx, terminal_id: &str, data: &str) {
        let bytes = match BASE64.decode(data.as_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => {
                ctx.send_error(None, format!("invalid terminal data: {e}"))
                    .await;
                return;
            }
        };
        let sender = self.inner.get(terminal_id).map(|h| h.input_tx.clone());
        if let Some(sender) = sender {
            let _ = sender.send(bytes).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn resize(
        &self,
        ctx: &ClientCtx,
        http: &reqwest::Client,
        port: u16,
        req_id: Option<String>,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) {
        let path = format!("/terminals/{terminal_id}/resize");
        let body = json!({ "cols": cols, "rows": rows });
        match loopback::post_json(http, port, &path, &body).await {
            Ok(value) => ctx.send_result(req_id, value).await,
            Err(e) => ctx.send_error(req_id, e.to_string()).await,
        }
    }

    pub async fn close(
        &self,
        ctx: &ClientCtx,
        http: &reqwest::Client,
        port: u16,
        req_id: Option<String>,
        terminal_id: &str,
    ) {
        if let Some((_, handle)) = self.inner.remove(terminal_id) {
            handle.task.abort();
        }
        let path = format!("/terminals/{terminal_id}/terminate");
        match loopback::post_json(http, port, &path, &json!({})).await {
            Ok(value) => ctx.send_result(req_id, value).await,
            Err(e) => ctx.send_error(req_id, e.to_string()).await,
        }
    }

    pub async fn list(
        &self,
        ctx: &ClientCtx,
        http: &reqwest::Client,
        port: u16,
        req_id: Option<String>,
    ) {
        match loopback::get_json(http, port, "/terminals").await {
            Ok(value) => ctx.send_result(req_id, value).await,
            Err(e) => ctx.send_error(req_id, e.to_string()).await,
        }
    }

    /// Abort all pump tasks (called when the relay connection drops).
    pub fn shutdown(&self) {
        for entry in self.inner.iter() {
            entry.value().task.abort();
        }
        self.inner.clear();
    }
}
