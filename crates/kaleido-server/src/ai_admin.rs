//! AI Provider & Usage admin API (P5).
//!
//! Routes (all require session auth):
//! - `GET  /api/v1/ai/providers`                          list providers (keys stripped)
//! - `POST /api/v1/ai/providers`                          create provider
//! - `PATCH /api/v1/ai/providers/{id}`                    update provider (key optional)
//! - `DELETE /api/v1/ai/providers/{id}`                   delete provider + its models
//! - `GET  /api/v1/ai/providers/{id}/models`              list models of provider
//! - `POST /api/v1/ai/providers/{id}/models`              create model
//! - `PATCH /api/v1/ai/models/{id}`                       update model
//! - `DELETE /api/v1/ai/models/{id}`                      delete model
//! - `GET  /api/v1/ai/usage?days=`                        usage summary + recent calls
//!
//! `RpmLimiter` is an in-memory per-provider token window shared through
//! `AppState`; call-side integration lives in `llm_stream` / chat handlers.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use kaleido_core::ai_admin_store::AiAdminError;
use serde::Deserialize;
use serde_json::json;

use crate::{admin_session_from, AppState};

/// RpmLimiter is implemented in kaleido-core so resolve_llm (core) can enforce
/// per-provider per-minute budgets process-wide. Re-exported here so AppState
/// can keep a handle for admin-side resets.
pub use kaleido_core::RpmLimiter;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ai/providers", get(list_providers_h).post(create_provider_h))
        .route(
            "/api/v1/ai/providers/{id}",
            patch(update_provider_h).delete(delete_provider_h),
        )
        .route(
            "/api/v1/ai/providers/{id}/models",
            get(list_models_h).post(create_model_h),
        )
        .route("/api/v1/ai/models/{id}", patch(update_model_h).delete(delete_model_h))
        .route("/api/v1/ai/usage", get(usage_h))
        // [酒馆对齐] active 指针: 查询当前激活 / 切换激活
        .route("/api/v1/ai/active", get(active_h))
        .route("/api/v1/ai/providers/{id}/activate", post(activate_h))
}

fn ai_err(e: AiAdminError) -> Response {
    let (code, body) = match e {
        AiAdminError::Db(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("{e}") })),
        AiAdminError::NotFound(w) => (StatusCode::NOT_FOUND, json!({ "error": format!("{w} not found") })),
        AiAdminError::BadRequest(m) => (StatusCode::BAD_REQUEST, json!({ "error": m })),
    };
    (code, Json(body)).into_response()
}

#[derive(Deserialize)]
struct ProviderBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    concurrency_limit: Option<i64>,
    #[serde(default)]
    rpm_limit: Option<i64>,
    #[serde(default)]
    max_tokens: Option<i64>,
    #[serde(default)]
    note: String,
}

async fn list_providers_h(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.list_providers() {
        Ok(ps) => Json(json!({ "providers": ps })).into_response(),
        Err(e) => ai_err(e),
    }
}

async fn create_provider_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(b): Json<ProviderBody>,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.create_provider(
        &b.name,
        &b.base_url,
        &b.protocol,
        &b.api_key,
        b.concurrency_limit.unwrap_or(10),
        b.rpm_limit.unwrap_or(60),
        b.max_tokens.unwrap_or(32000),
        &b.note,
    ) {
        Ok(p) => (StatusCode::CREATED, Json(json!({ "provider": p }))).into_response(),
        Err(e) => ai_err(e),
    }
}

#[derive(Deserialize)]
struct UpdateProviderBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    concurrency_limit: Option<i64>,
    #[serde(default)]
    rpm_limit: Option<i64>,
    #[serde(default)]
    max_tokens: Option<i64>,
    #[serde(default)]
    note: Option<String>,
}

async fn update_provider_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<UpdateProviderBody>,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.update_provider(
        &id,
        b.name.as_deref(),
        b.base_url.as_deref(),
        b.api_key.as_deref(),
        b.concurrency_limit.map(|v| v as i64),
        b.rpm_limit.map(|v| v as i64),
        b.max_tokens.map(|v| v as i64),
        b.note.as_deref(),
    ) {
        Ok(p) => {
            if let Some(rpm) = b.rpm_limit {
                let _ = rpm;
            }
            Json(json!({ "provider": p })).into_response()
        }
        Err(e) => ai_err(e),
    }
}

async fn delete_provider_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    state.rpm.reset(&id);
    match state.ai_admin.delete_provider(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ai_err(e),
    }
}

#[derive(Deserialize)]
struct ModelBody {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    model_id: String,
    #[serde(default)]
    purposes: Vec<String>,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    thinking_enabled: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    note: String,
}

async fn list_models_h(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.list_models(&pid) {
        Ok(ms) => Json(json!({ "models": ms })).into_response(),
        Err(e) => ai_err(e),
    }
}

async fn create_model_h(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    headers: HeaderMap,
    Json(b): Json<ModelBody>,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.create_model(
        &pid,
        &b.display_name,
        &b.model_id,
        &b.purposes.iter().map(|x| x.as_str()).collect::<Vec<_>>(),
        b.context_window.unwrap_or(128000) as i64,
        b.thinking_enabled.unwrap_or(true),
        b.enabled.unwrap_or(true),
        &b.note,
    ) {
        Ok(m) => (StatusCode::CREATED, Json(json!({ "model": m }))).into_response(),
        Err(e) => ai_err(e),
    }
}

#[derive(Deserialize)]
struct UpdateModelBody {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    purposes: Option<Vec<String>>,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    thinking_enabled: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    note: Option<String>,
}

async fn update_model_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<UpdateModelBody>,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.update_model(
        &id,
        b.display_name.as_deref(),
        b.model_id.as_deref(),
        b.purposes.clone(),
        b.context_window.map(|v| v as i64),
        b.thinking_enabled,
        b.enabled,
        b.note.as_deref(),
    ) {
        Ok(m) => Json(json!({ "model": m })).into_response(),
        Err(e) => ai_err(e),
    }
}

async fn delete_model_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.delete_model(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ai_err(e),
    }
}

#[derive(Deserialize)]
struct UsageQuery {
    #[serde(default = "default_days")]
    days: i64,
}

fn default_days() -> i64 {
    7
}

async fn usage_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    let days = q.days.clamp(1, 90);
    match state.ai_admin.usage_summary(days) {
        Ok(s) => Json(json!({
            "days": days,
            "total_calls": s.total_calls,
            "total_input_tokens": s.total_input_tokens,
            "total_output_tokens": s.total_output_tokens,
            "by_day": s.by_day,
            "recent": s.recent,
        }))
        .into_response(),
        Err(e) => ai_err(e),
    }
}

/// [酒馆对齐] 当前激活供应商 + 其默认模型 (前端只读卡片 / 激活徽标用)。
async fn active_h(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.active_provider() {
        Ok(Some(p)) => {
            let model = if !p.default_model_id.is_empty() {
                state.ai_admin.get_model(&p.default_model_id).ok()
            } else {
                None
            };
            Json(json!({
                "active": true,
                "provider": p,
                "model": model,
            }))
            .into_response()
        }
        Ok(None) => {
            Json(json!({ "active": false, "provider": null, "model": null })).into_response()
        }
        Err(e) => ai_err(e),
    }
}

/// [酒馆对齐] 切换激活: 目标 provider active=1, 其余 active=0。
async fn activate_h(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    match state.ai_admin.set_active_provider(&id) {
        Ok(p) => Json(json!({ "ok": true, "provider": p })).into_response(),
        Err(e) => ai_err(e),
    }
}
