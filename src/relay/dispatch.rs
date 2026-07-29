#![allow(dead_code)]

use crate::protocol::{IncomingMsg, OutgoingMsg};
use crate::relay::loopback;
use crate::relay::wire::ClientCtx;
use serde_json::json;

/// Handle every non-terminal request/response message by proxying to the local
/// HTTP API. Terminal messages are routed by the transport to the terminal
/// manager and must not reach here.
pub async fn dispatch(msg: IncomingMsg, ctx: &ClientCtx, http: &reqwest::Client, port: u16) {
    match msg {
        IncomingMsg::Ping { id } => {
            ctx.send(OutgoingMsg::Pong {
                id: None,
                resp_to: id,
            })
            .await;
        }
        IncomingMsg::SysmonGet { id } => {
            reply(ctx, id, loopback::get_json(http, port, "/sysmon").await).await;
        }
        IncomingMsg::PortsList { id } => {
            reply(ctx, id, loopback::get_json(http, port, "/ports").await).await;
        }
        IncomingMsg::PortsKill { id, port: target } => {
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/ports/kill", &json!({ "port": target })).await,
            )
            .await;
        }
        IncomingMsg::FsRead { id, path } => {
            reply(
                ctx,
                id,
                loopback::get_json_query(http, port, "/fs/read", &[("path", path)]).await,
            )
            .await;
        }
        IncomingMsg::FsWrite {
            id,
            path,
            content,
            encoding,
        } => {
            reply(
                ctx,
                id,
                loopback::post_json(
                    http,
                    port,
                    "/fs/write",
                    &json!({ "path": path, "content": content, "encoding": encoding }),
                )
                .await,
            )
            .await;
        }
        IncomingMsg::FsMkdir { id, path } => {
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/fs/mkdir", &json!({ "path": path })).await,
            )
            .await;
        }
        IncomingMsg::FsDelete {
            id,
            path,
            recursive,
        } => {
            reply(
                ctx,
                id,
                loopback::post_json(
                    http,
                    port,
                    "/fs/delete",
                    &json!({ "path": path, "recursive": recursive }),
                )
                .await,
            )
            .await;
        }
        IncomingMsg::FsRename { id, from, to } => {
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/fs/rename", &json!({ "from": from, "to": to }))
                    .await,
            )
            .await;
        }
        IncomingMsg::FsStat { id, path } => {
            reply(
                ctx,
                id,
                loopback::get_json_query(http, port, "/fs/stat", &[("path", path)]).await,
            )
            .await;
        }
        IncomingMsg::FsList { id, path } => {
            reply(
                ctx,
                id,
                loopback::get_json_query(http, port, "/fs/list", &[("path", path)]).await,
            )
            .await;
        }
        IncomingMsg::ProjectFileSearch { id, query, limit } => {
            let mut params: Vec<(&str, String)> = vec![("query", query)];
            if let Some(limit) = limit {
                params.push(("limit", limit.to_string()));
            }
            reply(
                ctx,
                id,
                loopback::get_json_query(http, port, "/project/file-search", &params).await,
            )
            .await;
        }
        IncomingMsg::Exec {
            id,
            command,
            cwd,
            timeout_ms,
        } => {
            let body = json!({
                "type": "silent_exec",
                "id": id.clone().unwrap_or_default(),
                "command": command,
                "cwd": cwd,
                "timeout_ms": timeout_ms,
            });
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/silent-exec", &body).await,
            )
            .await;
        }
        IncomingMsg::HttpRequest {
            id,
            url,
            method,
            headers,
            body,
        } => {
            let payload = json!({
                "url": url,
                "method": method,
                "headers": headers,
                "body": body,
            });
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/proxy/http", &payload).await,
            )
            .await;
        }
        IncomingMsg::AiInspect { id, path } => {
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/ai/inspect", &json!({ "path": path })).await,
            )
            .await;
        }
        IncomingMsg::AiListModels { id } => {
            reply(ctx, id, loopback::get_json(http, port, "/ai/models").await).await;
        }
        IncomingMsg::AiHealth { id } => {
            reply(ctx, id, loopback::get_json(http, port, "/ai/health").await).await;
        }
        IncomingMsg::AiCapabilities { id } => {
            reply(
                ctx,
                id,
                loopback::get_json(http, port, "/ai/capabilities").await,
            )
            .await;
        }
        IncomingMsg::AiGenerate {
            id,
            session_id,
            prompt,
        } => {
            reply(
                ctx,
                id,
                loopback::post_json(
                    http,
                    port,
                    "/ai/generate",
                    &json!({ "session_id": session_id, "prompt": prompt }),
                )
                .await,
            )
            .await;
        }
        IncomingMsg::AiComplete { id, prefix, suffix } => {
            reply(
                ctx,
                id,
                loopback::post_json(
                    http,
                    port,
                    "/ai/complete",
                    &json!({ "prefix": prefix, "suffix": suffix }),
                )
                .await,
            )
            .await;
        }
        IncomingMsg::AiEmbed { id, texts } => {
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/ai/embed", &json!({ "texts": texts })).await,
            )
            .await;
        }
        IncomingMsg::AiLoad { id, path, args } => {
            reply(
                ctx,
                id,
                loopback::post_json(
                    http,
                    port,
                    "/ai/load",
                    &json!({ "path": path, "args": args }),
                )
                .await,
            )
            .await;
        }
        IncomingMsg::AiUnload { id, model_id } => {
            reply(
                ctx,
                id,
                loopback::post_json(http, port, "/ai/unload", &json!({ "model_id": model_id }))
                    .await,
            )
            .await;
        }
        IncomingMsg::TerminalCreate { .. }
        | IncomingMsg::TerminalData { .. }
        | IncomingMsg::TerminalResize { .. }
        | IncomingMsg::TerminalClose { .. }
        | IncomingMsg::TerminalList { .. }
        | IncomingMsg::TerminalAttach { .. }
        | IncomingMsg::SysmonSubscribe { .. }
        | IncomingMsg::SysmonUnsubscribe { .. }
        | IncomingMsg::AgentsStart { .. }
        | IncomingMsg::AgentsInput { .. }
        | IncomingMsg::AgentsKill { .. }
        | IncomingMsg::WsOpen { .. }
        | IncomingMsg::WsData { .. }
        | IncomingMsg::WsClose { .. } => {
            tracing::warn!("message reached dispatch; should be routed to a relay manager");
        }
        IncomingMsg::Unknown => {
            ctx.send_error(None, "Unknown message type").await;
        }
    }
}

async fn reply(ctx: &ClientCtx, id: Option<String>, result: anyhow::Result<serde_json::Value>) {
    match result {
        Ok(value) => ctx.send_result(id, value).await,
        Err(e) => ctx.send_error(id, e.to_string()).await,
    }
}
