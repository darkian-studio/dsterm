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

fn parse_budgets_and_payload(body: serde_json::Value) -> (serde_json::Value, Budgets) {
    if let Some(obj) = body.as_object() {
        if obj.contains_key("payload") || obj.contains_key("budgets") {
            let budgets = obj
                .get("budgets")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let payload = obj.get("payload").cloned().unwrap_or_else(|| {
                let mut rest = obj.clone();
                rest.remove("budgets");
                serde_json::Value::Object(rest)
            });
            return (payload, budgets);
        }
    }
    (body, Budgets::default())
}

async fn web_fetch(
    State(provider): State<WebState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let (payload, budgets) = parse_budgets_and_payload(body);

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
    let (payload, budgets) = parse_budgets_and_payload(body);

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
    let (payload, budgets) = parse_budgets_and_payload(body);

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
    let (payload, budgets) = parse_budgets_and_payload(body);

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
