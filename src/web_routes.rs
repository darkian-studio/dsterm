use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use std::sync::Arc;

use crate::providers::web::WebProvider;
use crate::providers::{Budgets, Provider, ProviderRequest};

pub type WebState = Arc<WebProvider>;

pub fn web_routes() -> Router<WebState> {
    Router::new()
        .route("/web/fetch", post(web_fetch))
        .route("/web/extract", post(web_extract))
        .route("/web/search", post(web_search))
        .route("/web/crawl", post(web_crawl))
}

async fn web_fetch(
    State(provider): State<WebState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let budgets: Budgets = body
        .get("budgets")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let payload = body.get("payload").cloned().unwrap_or(body);

    let request = ProviderRequest {
        operation: "fetch".to_string(),
        payload,
        budgets,
    };

    let response = provider.execute(request).await;
    let status = if response.success {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };

    (status, Json(serde_json::to_value(&response).unwrap()))
}

async fn web_extract(
    State(provider): State<WebState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let budgets: Budgets = body
        .get("budgets")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let payload = body.get("payload").cloned().unwrap_or(body);

    let request = ProviderRequest {
        operation: "extract".to_string(),
        payload,
        budgets,
    };

    let response = provider.execute(request).await;
    let status = if response.success {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };

    (status, Json(serde_json::to_value(&response).unwrap()))
}

async fn web_search(
    State(provider): State<WebState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let budgets: Budgets = body
        .get("budgets")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let payload = body.get("payload").cloned().unwrap_or(body);

    let request = ProviderRequest {
        operation: "search".to_string(),
        payload,
        budgets,
    };

    let response = provider.execute(request).await;
    let status = if response.success {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };

    (status, Json(serde_json::to_value(&response).unwrap()))
}

async fn web_crawl(
    State(provider): State<WebState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let budgets: Budgets = body
        .get("budgets")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let payload = body.get("payload").cloned().unwrap_or(body);

    let request = ProviderRequest {
        operation: "crawl".to_string(),
        payload,
        budgets,
    };

    let response = provider.execute(request).await;
    let status = if response.success {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };

    (status, Json(serde_json::to_value(&response).unwrap()))
}
