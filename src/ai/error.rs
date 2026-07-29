use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct AiErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct AiError {
    pub status: StatusCode,
    pub body: AiErrorBody,
}

impl AiError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            body: AiErrorBody {
                code: code.into(),
                message: message.into(),
                details: None,
            },
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.body.details = Some(details);
        self
    }
}

impl IntoResponse for AiError {
    fn into_response(self) -> axum::response::Response {
        let body = json!({
            "success": false,
            "error": self.body
        });
        (self.status, Json(body)).into_response()
    }
}

pub fn bad_request(message: impl Into<String>) -> AiError {
    AiError::new(StatusCode::BAD_REQUEST, "INVALID_REQUEST", message)
}

pub fn file_not_found(path: impl Into<String>) -> AiError {
    AiError::new(
        StatusCode::NOT_FOUND,
        "FILE_NOT_FOUND",
        format!("file not found: {}", path.into()),
    )
}

pub fn invalid_gguf(detail: impl Into<String>) -> AiError {
    AiError::new(StatusCode::UNPROCESSABLE_ENTITY, "INVALID_GGUF", detail)
}

pub fn unsupported_gguf_version(version: u32) -> AiError {
    AiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "UNSUPPORTED_GGUF_VERSION",
        format!("unsupported GGUF version: {version}"),
    )
}

pub fn model_not_found(id: impl Into<String>) -> AiError {
    AiError::new(
        StatusCode::NOT_FOUND,
        "MODEL_NOT_FOUND",
        format!("model not found: {}", id.into()),
    )
}

pub fn model_already_registered(id: impl Into<String>) -> AiError {
    AiError::new(
        StatusCode::CONFLICT,
        "MODEL_ALREADY_REGISTERED",
        format!("model already registered: {}", id.into()),
    )
}

pub fn internal_error(message: impl Into<String>) -> AiError {
    AiError::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", message)
}
