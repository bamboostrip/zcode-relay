//! axum 应用：把 config + proxy + auth + models 组装成 HTTP 服务。
//!
//! 端点：
//!   GET  /healthz
//!   GET  /v1/models
//!   POST /v1/messages          → anthropic 上游
//!   POST /v1/chat/completions  → openai 上游
//!
//! 鉴权：management_key（Bearer / x-api-key）。body 纯透传。

use crate::auth::check_auth;
use crate::config::Config;
use crate::models::ModelRegistry;
use crate::proxy::{Proxy, ProxyResult};
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;

pub struct AppState {
    pub cfg: Config,
    pub proxy: Proxy,
    pub models: ModelRegistry,
}

pub fn create_app(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/messages", post(forward_anthropic))
        .route("/v1/chat/completions", post(forward_openai))
        .with_state(state)
}

// ---- healthz ---- //
async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({"ok": true, "service": "zcode-relay"}))
}

// ---- /v1/models ---- //
async fn list_models(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !check_auth(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        &state.cfg.management_key,
    ) {
        return err_json(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "invalid management key",
        )
        .into_response();
    }
    let models = state.models.list_models();
    let data: Vec<_> = models
        .into_iter()
        .map(|m| serde_json::json!({"id": m, "object": "model", "created": 0, "owned_by": "zcode-relay"}))
        .collect();
    Json(serde_json::json!({"object": "list", "data": data})).into_response()
}

// ---- 通用转发 ---- //
async fn handle_forward(
    state: &AppState,
    headers: &HeaderMap,
    body: Bytes,
    upstream_base: &str,
    path: &str,
    kind: &str,
) -> Response {
    if !check_auth(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        &state.cfg.management_key,
    ) {
        return err_json(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "invalid management key",
        )
        .into_response();
    }

    let url = format!("{}{}", upstream_base.trim_end_matches('/'), path);
    let is_stream = detect_stream(&body);
    let mut extra = extract_passthrough_headers(headers);
    if kind == "anthropic" && !extra.contains_key("anthropic-version") {
        extra.insert("anthropic-version".into(), "2023-06-01".into());
    }

    let result = state
        .proxy
        .forward(&state.cfg, "POST", &url, body, is_stream, extra)
        .await;

    match result {
        ProxyResult::NonStream {
            status,
            headers,
            body,
        } => {
            let mut resp = Response::builder().status(map_status(status));
            // 透传响应头（去掉 hop-by-hop）
            let mut out_hdrs = HeaderMap::new();
            for (k, v) in &headers {
                let lower = k.to_lowercase();
                if matches!(
                    lower.as_str(),
                    "content-length" | "transfer-encoding" | "content-encoding" | "connection"
                ) {
                    continue;
                }
                if let (Ok(name), Ok(val)) = (
                    header::HeaderName::from_bytes(k.as_bytes()),
                    header::HeaderValue::from_str(v),
                ) {
                    out_hdrs.insert(name, val);
                }
            }
            resp = resp.header(header::CONTENT_TYPE, "application/json");
            resp.body(Body::from(body))
                .unwrap_or_else(|_| fallback_500())
        }
        ProxyResult::Stream {
            status,
            content_type,
            stream,
        } => {
            let body = Body::from_stream(stream);
            Response::builder()
                .status(map_status(status))
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CACHE_CONTROL, "no-cache")
                .body(body)
                .unwrap_or_else(|_| fallback_500())
        }
        ProxyResult::Error { status, body } => Response::builder()
            .status(map_status(status))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap_or_else(|_| fallback_500()),
    }
}

async fn forward_anthropic(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_forward(
        &state,
        &headers,
        body,
        &state.cfg.anthropic_base,
        "/v1/messages",
        "anthropic",
    )
    .await
}

async fn forward_openai(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_forward(
        &state,
        &headers,
        body,
        &state.cfg.openai_base,
        "/chat/completions",
        "openai",
    )
    .await
}

// ---- 辅助 ---- //
fn detect_stream(body: &[u8]) -> bool {
    if !body.windows(8).any(|w| w == b"\"stream\"") {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}

fn extract_passthrough_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut h = HashMap::new();
    for k in &["accept", "anthropic-version", "anthropic-beta"] {
        if let Some(v) = headers.get(*k).and_then(|v| v.to_str().ok()) {
            h.insert((*k).to_string(), v.to_string());
        }
    }
    h
}

fn map_status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn err_json(
    status: StatusCode,
    err_type: &str,
    message: &str,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({
            "error": {"type": err_type, "message": message}
        })),
    )
}

fn fallback_500() -> Response {
    (StatusCode::BAD_GATEWAY, "upstream error").into_response()
}
