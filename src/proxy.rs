//! Localhost proxy: forwards HTTP requests and tunnels WebSocket connections to
//! services bound on the local machine. Targets are restricted to localhost.
use crate::terminal::get_config;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use tokio_tungstenite::tungstenite::Message as TMessage;

#[derive(Debug, Deserialize)]
pub struct HttpProxyRequest {
    pub url: String,
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WsProxyQuery {
    pub url: String,
}

fn proxy_enabled() -> bool {
    get_config().proxy.enabled
}

pub fn is_localhost(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(parsed) => match parsed.host_str() {
            Some(host) => {
                host == "localhost" || host == "::1" || host == "[::1]" || host.starts_with("127.")
            }
            None => false,
        },
        Err(_) => false,
    }
}

fn forbidden() -> axum::response::Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "Proxy is disabled" })),
    )
        .into_response()
}

fn bad_request(msg: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

pub async fn proxy_http(Json(req): Json<HttpProxyRequest>) -> impl IntoResponse {
    if !proxy_enabled() {
        return forbidden();
    }
    if !is_localhost(&req.url) {
        return bad_request("Only localhost targets are allowed");
    }
    let method = req.method.as_deref().unwrap_or("GET").to_uppercase();
    let method = match reqwest::Method::from_bytes(method.as_bytes()) {
        Ok(method) => method,
        Err(_) => return bad_request("Invalid HTTP method"),
    };
    let client = reqwest::Client::new();
    let mut builder = client.request(method, &req.url);
    if let Some(headers) = &req.headers {
        for (key, value) in headers {
            builder = builder.header(key, value);
        }
    }
    if let Some(body) = req.body {
        builder = builder.body(body);
    }
    let response = match builder.send().await {
        Ok(response) => response,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Upstream request failed: {e}") })),
            )
                .into_response()
        }
    };
    let status = response.status().as_u16();
    let mut headers = HashMap::new();
    for (key, value) in response.headers() {
        if let Ok(value) = value.to_str() {
            headers.insert(key.to_string(), value.to_string());
        }
    }
    let bytes =
        match response.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("Failed to read upstream body: {e}") })),
            )
                .into_response(),
        };
    Json(serde_json::json!({
        "status": status,
        "headers": headers,
        "body_base64": BASE64.encode(&bytes),
    }))
    .into_response()
}

pub async fn proxy_ws(
    ws: WebSocketUpgrade,
    Query(query): Query<WsProxyQuery>,
) -> impl IntoResponse {
    if !proxy_enabled() {
        return forbidden();
    }
    if !is_localhost(&query.url) {
        return bad_request("Only localhost targets are allowed");
    }
    ws.on_upgrade(move |socket| proxy_ws_pump(socket, query.url))
}

async fn proxy_ws_pump(socket: WebSocket, url: String) {
    let (mut client_send, mut client_recv) = socket.split();
    let upstream = match tokio_tungstenite::connect_async(url.as_str()).await {
        Ok((upstream, _)) => upstream,
        Err(e) => {
            tracing::warn!("proxy upstream connect failed: {e}");
            let _ = client_send.send(Message::Close(None)).await;
            return;
        }
    };
    let (mut up_send, mut up_recv) = upstream.split();

    loop {
        tokio::select! {
            client_msg = client_recv.next() => {
                match client_msg {
                    Some(Ok(Message::Text(text))) => {
                        if up_send.send(TMessage::Text(text.to_string())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        if up_send.send(TMessage::Binary(data.to_vec())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        let _ = up_send.send(TMessage::Close(None)).await;
                        break;
                    }
                    _ => {}
                }
            }
            up_msg = up_recv.next() => {
                match up_msg {
                    Some(Ok(TMessage::Text(text))) => {
                        if client_send.send(Message::Text(text.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(TMessage::Binary(data))) => {
                        if client_send.send(Message::Binary(data.to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(TMessage::Close(_))) | None | Some(Err(_)) => {
                        let _ = client_send.send(Message::Close(None)).await;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

pub fn proxy_routes() -> Router {
    Router::new()
        .route("/proxy/http", post(proxy_http))
        .route("/proxy/ws", get(proxy_ws))
}
