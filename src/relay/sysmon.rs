#![allow(dead_code)]

use crate::protocol::OutgoingMsg;
use crate::relay::loopback;
use crate::relay::wire::ClientCtx;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

const PUSH_INTERVAL_SECS: u64 = 30;

#[derive(Clone, Default)]
pub struct RelaySysmon {
    inner: Arc<DashMap<String, JoinHandle<()>>>,
}

impl RelaySysmon {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub async fn subscribe(
        &self,
        ctx: &ClientCtx,
        http: &reqwest::Client,
        port: u16,
        req_id: Option<String>,
    ) {
        if self.inner.contains_key(&ctx.client_id) {
            ctx.send_result(req_id, serde_json::json!({ "subscribed": true }))
                .await;
            return;
        }
        let task_ctx = ctx.clone();
        let http = http.clone();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(PUSH_INTERVAL_SECS));
            loop {
                tick.tick().await;
                match loopback::get_json(&http, port, "/sysmon").await {
                    Ok(data) => {
                        task_ctx.send(OutgoingMsg::SysmonUpdate { data }).await;
                    }
                    Err(e) => tracing::warn!("sysmon push failed: {e}"),
                }
            }
        });
        self.inner.insert(ctx.client_id.clone(), task);
        ctx.send_result(req_id, serde_json::json!({ "subscribed": true }))
            .await;
    }

    pub async fn unsubscribe(&self, ctx: &ClientCtx, req_id: Option<String>) {
        if let Some((_, task)) = self.inner.remove(&ctx.client_id) {
            task.abort();
        }
        ctx.send_result(req_id, serde_json::json!({ "subscribed": false }))
            .await;
    }

    pub fn shutdown(&self) {
        for entry in self.inner.iter() {
            entry.value().abort();
        }
        self.inner.clear();
    }
}
