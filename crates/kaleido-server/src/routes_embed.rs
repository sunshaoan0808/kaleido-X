//! Embedding endpoints (OpenAI-compatible /v1/embeddings shim) — P0-1 Stage5
use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::auth_mw::session_from;
use crate::error_codes::*;
use crate::state::AppState;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/embed/status", get(embed_status))
        .route("/v1/embeddings", post(embeddings_openai))
}

#[derive(serde::Deserialize)]
#[derive(Debug)]
pub(crate) struct EmbeddingsBody {
    pub input: serde_json::Value,
    #[serde(default)]
    pub model: Option<String>,
}

pub(crate) async fn embed_status() -> Response {
    let _ = tokio::task::spawn_blocking(|| crate::embed_local::ensure_local()).await;
    Json(json!({
        "ok": true,
        "embedding": crate::embed_local::status(),
    }))
    .into_response()
}

pub(crate) async fn embeddings_openai(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EmbeddingsBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let texts: Vec<String> = match &body.input {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            return bad_request("EMB_BAD_REQUEST", "input must be string or array of strings");
        }
    };
    if texts.is_empty() {
        return bad_request("EMB_EMPTY", "empty input");
    }
    let client = reqwest::Client::new();
    let base = state.embedding_base.clone().unwrap_or_default();
    let result = if crate::embed_local::inline_enabled() && texts.len() >= 1 {
        // always prefer batch local path when inline on
        tokio::task::spawn_blocking({
            let texts = texts.clone();
            move || crate::embed_local::embed_many(&texts)
        })
        .await
        .map_err(|e| format!("join: {e}"))
        .and_then(|r| r)
    } else {
        let mut out = Vec::new();
        for t in &texts {
            match crate::llm_stream::get_embedding(&base, t, &client).await {
                Ok(v) => out.push(v),
                Err(e) => {
                    return bad_gateway("EMB_ERROR", e);
                }
            }
        }
        Ok(out)
    };
    match result {
        Ok(vecs) => {
            let data: Vec<serde_json::Value> = vecs
                .into_iter()
                .enumerate()
                .map(|(i, embedding)| {
                    json!({
                        "object": "embedding",
                        "index": i,
                        "embedding": embedding,
                    })
                })
                .collect();
            let model = body
                .model
                .unwrap_or_else(|| "BAAI/bge-small-zh-v1.5".into());
            Json(json!({
                "object": "list",
                "model": model,
                "data": data,
                "backend": crate::embed_local::status().get("backend").cloned().unwrap_or(json!("unknown")),
            }))
            .into_response()
        }
        Err(e) => return bad_gateway("EMB_ERROR", e),
    }
}

