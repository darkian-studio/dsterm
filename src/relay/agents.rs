#![allow(dead_code)]

use crate::protocol::OutgoingMsg;
use crate::relay::loopback;
use crate::relay::wire::ClientCtx;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

struct AgentHandle {
    input_tx: mpsc::Sender<String>,
    task: JoinHandle<()>,
}

#[derive(Clone, Default)]
pub struct RelayAgents {
    inner: Arc<DashMap<String, AgentHandle>>,
}

impl RelayAgents {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        &self,
        ctx: &ClientCtx,
        http: &reqwest::Client,
        port: u16,
        req_id: Option<String>,
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: Option<HashMap<String, String>>,
    ) {
        let agent_id = uuid::Uuid::new_v4().to_string();
        let body = json!({
            "id": agent_id,
            "command": command,
            "args": args,
            "cwd": cwd,
            "env": env,
        });
        if let Err(e) = loopback::post_json(http, port, "/agents/start", &body).await {
            ctx.send_error(req_id, format!("agent start failed: {e}"))
                .await;
            return;
        }

        let url = format!("ws://127.0.0.1:{port}/agents/{agent_id}");
        let ws = match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                ctx.send_error(req_id, format!("agent attach failed: {e}"))
                    .await;
                return;
            }
        };
        let (mut ws_write, mut ws_read) = ws.split();

        let (input_tx, mut input_rx) = mpsc::channel::<String>(256);
        let task_ctx = ctx.clone();
        let id_for_task = agent_id.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    incoming = ws_read.next() => {
                        match incoming {
                            Some(Ok(Message::Text(text))) => {
                                task_ctx
                                    .send(OutgoingMsg::AgentOutput {
                                        agent_id: id_for_task.clone(),
                                        data: text.to_string(),
                                    })
                                    .await;
                            }
                            Some(Ok(Message::Binary(bytes))) => {
                                if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                                    task_ctx
                                        .send(OutgoingMsg::AgentOutput {
                                            agent_id: id_for_task.clone(),
                                            data: text,
                                        })
                                        .await;
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Err(_)) => break,
                            _ => {}
                        }
                    }
                    input = input_rx.recv() => {
                        match input {
                            Some(line) => {
                                if ws_write.send(Message::Text(line)).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
            task_ctx
                .send(OutgoingMsg::AgentExit {
                    agent_id: id_for_task.clone(),
                })
                .await;
        });

        self.inner
            .insert(agent_id.clone(), AgentHandle { input_tx, task });
        ctx.send_result(req_id, json!({ "agentId": agent_id }))
            .await;
    }

    pub async fn input(&self, ctx: &ClientCtx, agent_id: &str, data: &str) {
        let sender = self.inner.get(agent_id).map(|h| h.input_tx.clone());
        if let Some(sender) = sender {
            let _ = sender.send(data.to_string()).await;
        } else {
            ctx.send_error(None, format!("unknown agent: {agent_id}"))
                .await;
        }
    }

    pub async fn kill(
        &self,
        ctx: &ClientCtx,
        http: &reqwest::Client,
        port: u16,
        req_id: Option<String>,
        agent_id: &str,
    ) {
        if let Some((_, handle)) = self.inner.remove(agent_id) {
            handle.task.abort();
        }
        let body = json!({ "id": agent_id });
        match loopback::post_json(http, port, "/agents/kill", &body).await {
            Ok(value) => ctx.send_result(req_id, value).await,
            Err(e) => ctx.send_error(req_id, e.to_string()).await,
        }
    }

    pub fn shutdown(&self) {
        for entry in self.inner.iter() {
            entry.value().task.abort();
        }
        self.inner.clear();
    }
}
