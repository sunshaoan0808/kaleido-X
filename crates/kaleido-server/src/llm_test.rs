//! LLM connectivity + model catalog (S5-W3 T3 / S8+ settings UX).
//!
//! - `POST /api/v1/llm/test` — optional body `{ model?, prompt?, maxTokens? }`
//! - `GET  /api/v1/llm/models` — proxy OpenAI-compatible `{base}/models`
//!   optional query: `q` filter. Base URL is always the server-configured upstream
//!   (`LLM_BASE_URL`); clients cannot override it (SSRF guard).

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration as StdDuration, Instant};

use crate::{session_from, AppState};

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LlmTestBody {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelsQuery {
    /// Optional substring filter (case-insensitive).
    #[serde(default)]
    pub q: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/llm/test", post(llm_test))
        .route("/api/v1/llm/models", get(llm_models))
}

fn resolve_runtime(state: &AppState) -> kaleido_core::LlmRuntime {
    state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    )
}

async fn llm_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<LlmTestBody>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let body = body.map(|j| j.0).unwrap_or_default();
    let llm = resolve_runtime(&state);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "ok": false,
                "error": "llm not configured",
                "latencyMs": null,
            })),
        )
            .into_response();
    }
    let model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if llm.model.is_empty() {
                state.llm_model.as_str()
            } else {
                llm.model.as_str()
            }
        })
        .to_string();
    let prompt = body
        .prompt
        .as_deref()
        .unwrap_or("ping")
        .to_string();
    let max_tokens = body.max_tokens.unwrap_or(64).min(256);

    let client = match reqwest::Client::builder()
        .timeout(StdDuration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e.to_string(), "latencyMs": null})),
            )
                .into_response();
        }
    };
    let url = format!("{}/chat/completions", llm.base_url.trim_end_matches('/'));
    let req_body = json!({
        "model": model,
        "stream": false,
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "messages": [
            {"role": "user", "content": prompt}
        ]
    });
    let started = Instant::now();
    let resp = client
        .post(&url)
        .bearer_auth(&llm.api_key)
        .header("content-type", "application/json")
        .json(&req_body)
        .send()
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match resp {
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            if !status.is_success() {
                return Json(json!({
                    "ok": false,
                    "error": format!("upstream {status}: {}", text.chars().take(200).collect::<String>()),
                    "latencyMs": latency_ms,
                    "model": model,
                    "baseUrl": llm.base_url,
                }))
                .into_response();
            }
            let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
            let content = v
                .pointer("/choices/0/message/content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            Json(json!({
                "ok": true,
                "latencyMs": latency_ms,
                "model": model,
                "baseUrl": llm.base_url,
                "content": content,
                "sample": content.chars().take(80).collect::<String>(),
                "message": "chat/completions probe succeeded",
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "ok": false,
            "error": format!("connect: {e}"),
            "latencyMs": latency_ms,
            "model": model,
            "baseUrl": llm.base_url,
        }))
        .into_response(),
    }
}

/// Pull OpenAI-compatible model list from the configured (or override) base URL.
async fn llm_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LlmModelsQuery>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let llm = resolve_runtime(&state);
    let base = llm.base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": "llm baseUrl empty — set Base URL first",
                "models": [],
            })),
        )
            .into_response();
    }
    if llm.api_key.trim().is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "ok": false,
                "error": "llm api key not configured",
                "models": [],
                "baseUrl": base,
            })),
        )
            .into_response();
    }

    let client = match reqwest::Client::builder()
        .timeout(StdDuration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e.to_string(), "models": []})),
            )
                .into_response();
        }
    };

    // Accept base with or without trailing /v1
    let url = if base.ends_with("/models") {
        base.clone()
    } else {
        format!("{base}/models")
    };
    let started = Instant::now();
    let resp = client
        .get(&url)
        .bearer_auth(&llm.api_key)
        .header("accept", "application/json")
        .send()
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match resp {
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            if !status.is_success() {
                return Json(json!({
                    "ok": false,
                    "error": format!("upstream {status}: {}", text.chars().take(240).collect::<String>()),
                    "latencyMs": latency_ms,
                    "baseUrl": base,
                    "models": [],
                }))
                .into_response();
            }
            let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
            let mut ids = extract_model_ids(&v);
            if let Some(filter) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                let f = filter.to_ascii_lowercase();
                ids.retain(|id| id.to_ascii_lowercase().contains(&f));
            }
            // stable unique sort
            ids.sort();
            ids.dedup();
            let current = if llm.model.is_empty() {
                state.llm_model.clone()
            } else {
                llm.model.clone()
            };
            // ensure current model appears even if upstream omits aliases
            if !current.is_empty() && !ids.iter().any(|m| m == &current) {
                ids.insert(0, current.clone());
            }
            let models: Vec<Value> = ids
                .iter()
                .map(|id| json!({"id": id, "object": "model"}))
                .collect();
            Json(json!({
                "ok": true,
                "object": "list",
                "baseUrl": base,
                "latencyMs": latency_ms,
                "count": models.len(),
                "current": current,
                "models": models,
                "data": models, // OpenAI-shaped alias
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "ok": false,
            "error": format!("connect: {e}"),
            "latencyMs": latency_ms,
            "baseUrl": base,
            "models": [],
        }))
        .into_response(),
    }
}

fn extract_model_ids(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        let t = s.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
    };
    // OpenAI: { data: [ {id} ] }
    if let Some(arr) = v.get("data").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                push(id);
            } else if let Some(id) = item.as_str() {
                push(id);
            }
        }
    }
    // some gateways: { models: [ {id|name} ] }
    if let Some(arr) = v.get("models").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(id) = item
                .get("id")
                .or_else(|| item.get("name"))
                .and_then(|x| x.as_str())
            {
                push(id);
            } else if let Some(id) = item.as_str() {
                push(id);
            }
        }
    }
    // bare array
    if let Some(arr) = v.as_array() {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                push(id);
            } else if let Some(id) = item.as_str() {
                push(id);
            }
        }
    }
    out
}

// silence unused if Query HashMap re-export path changes
#[allow(dead_code)]
fn _keep_hashmap(_: HashMap<String, String>) {}
