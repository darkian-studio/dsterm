#![allow(dead_code)]

use crate::protocol::OutgoingMsg;
use crate::proxy::is_localhost;
use crate::relay::wire::ClientCtx;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

struct ProxyWsHandle {
    input_tx: mpsc::Sender<Message>,
    task: JoinHandle<()>,
}

#[derive(Clone, Default)]
pub struct RelayProxyWs {
    inner: Arc<DashMap<String, ProxyWsHandle>>,
}

impl RelayProxyWs {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub async fn open(&self, ctx: &ClientCtx, req_id: Option<String>, url: String) {
        if !crate::terminal::get_config().proxy.enabled {
            ctx.send_error(req_id, "Proxy is disabled").await;
            return;
        }
        if !is_localhost(&url) {
            ctx.send_error(req_id, "Only localhost targets are allowed")
                .await;
            return;
        }
        let ws = match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                ctx.send_error(req_id, format!("ws open failed: {e}")).await;
                return;
            }
        };
        let ws_id = uuid::Uuid::new_v4().to_string();
        let (mut ws_write, mut ws_read) = ws.split();
        let (input_tx, mut input_rx) = mpsc::channel::<Message>(256);
        let task_ctx = ctx.clone();
        let id_for_task = ws_id.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    incoming = ws_read.next() => {
                        match incoming {
                            Some(Ok(Message::Text(text))) => {
                                task_ctx
                                    .send(OutgoingMsg::WsData {
                                        ws_id: id_for_task.clone(),
                                        data: BASE64.encode(text.to_string().as_bytes()),
                                        binary: false,
                                    })
                                    .await;
                            }
                            Some(Ok(Message::Binary(bytes))) => {
                                task_ctx
                                    .send(OutgoingMsg::WsData {
                                        ws_id: id_for_task.clone(),
                                        data: BASE64.encode(&bytes[..]),
                                        binary: true,
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
                            Some(msg) => {
                                if ws_write.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
            task_ctx
                .send(OutgoingMsg::WsClose {
                    ws_id: id_for_task.clone(),
                })
                .await;
        });

        self.inner
            .insert(ws_id.clone(), ProxyWsHandle { input_tx, task });
        ctx.send_result(req_id, json!({ "wsId": ws_id })).await;
    }

    pub async fn data(&self, ctx: &ClientCtx, ws_id: &str, data: &str, binary: bool) {
        let bytes = match BASE64.decode(data.as_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => {
                ctx.send_error(None, format!("invalid ws data: {e}")).await;
                return;
            }
        };
        let msg = if binary {
            Message::Binary(bytes)
        } else {
            Message::Text(String::from_utf8_lossy(&bytes).to_string())
        };
        let sender = self.inner.get(ws_id).map(|h| h.input_tx.clone());
        if let Some(sender) = sender {
            let _ = sender.send(msg).await;
        }
    }

    pub async fn close(&self, ws_id: &str) {
        if let Some((_, handle)) = self.inner.remove(ws_id) {
            handle.task.abort();
        }
    }

    pub fn shutdown(&self) {
        for entry in self.inner.iter() {
            entry.value().task.abort();
        }
        self.inner.clear();
    }
}
