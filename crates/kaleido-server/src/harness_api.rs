//! Harness P3 REST API（`/api/v1/harness`）。
//!
//! Routes（均需 session auth）：
//! - `GET  /api/v1/harness/state`   读取当前 HarnessState（JSON）
//! - `POST /api/v1/harness/refine`  plan(LLM)+apply 闭环，返回 ApplyResult
//! - `POST /api/v1/harness/apply`   手动注入一个 RefinementProposal 直接 apply（纯内存，无需 LLM）
//! - `GET  /api/v1/harness/history` 读取全局精炼历史（refinements.jsonl）
//! - `GET/POST /api/v1/harness/guidance` 查看/新增 Guidance
//! - `DELETE /api/v1/harness/guidance/{id}` 软删除 Guidance（active=false）
//! - `POST /api/v1/harness/discuss` 需求讨论 → 可选固化为 Guidance
//!
//! LLM 配置来自环境（见 [`crate::harness_bridge::LlmClientImpl::from_env`]）；
//! provider 未配置时 `/refine` 与 `/discuss` 返回 400。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use kaleido_harness::RefinementProposal;

use crate::harness_bridge::{
    add_guidance, apply_proposal_persist, deactivate_guidance, discuss, list_guidance, load_state,
    run_refine, state_dir, LlmClientImpl,
};
use crate::{session_from, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/harness/state", get(state_h))
        .route("/api/v1/harness/history", get(history_h))
        .route("/api/v1/harness/refine", axum::routing::post(refine_h))
        .route("/api/v1/harness/apply", axum::routing::post(apply_h))
        .route("/api/v1/harness/guidance", get(guidance_h))
        .route("/api/v1/harness/guidance", axum::routing::post(guidance_add_h))
        .route(
            "/api/v1/harness/guidance/{id}",
            axum::routing::delete(guidance_delete_h),
        )
        .route("/api/v1/harness/discuss", axum::routing::post(discuss_h))
}

fn data_root_path(state: &AppState) -> std::path::PathBuf {
    state.auth.data_root().root().to_path_buf()
}

fn harness_err(status: StatusCode, msg: &str) -> Response {
    let code = match status {
        StatusCode::NOT_FOUND => "HARNESS_NOT_FOUND",
        StatusCode::BAD_REQUEST => "HARNESS_BAD_REQUEST",
        StatusCode::FORBIDDEN => "HARNESS_FORBIDDEN",
        _ => "HARNESS_INTERNAL",
    };
    crate::error_codes::err_with_code(status, code, msg, serde_json::Value::Null)
}

async fn state_h(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let root = data_root_path(&state);
    Json(json!({ "state": load_state(&root) })).into_response()
}

async fn history_h(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let root = data_root_path(&state);
    let history = kaleido_harness::store::load_global_history(&state_dir(&root));
    Json(json!({ "history": history })).into_response()
}

#[derive(Deserialize)]
struct RefineBody {
    conversation_tail: String,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    scope_policy: Option<String>,
}

async fn refine_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RefineBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let Some(llm) = LlmClientImpl::from_env() else {
        return harness_err(StatusCode::BAD_REQUEST, "LLM provider not configured");
    };
    let root = data_root_path(&state);
    match run_refine(
        &root,
        &llm,
        &body.conversation_tail,
        body.instructions.as_deref(),
        body.scope_policy.as_deref(),
    )
    .await
    {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(e) => harness_err(StatusCode::BAD_GATEWAY, &e),
    }
}

async fn apply_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(proposal): Json<RefinementProposal>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let root = data_root_path(&state);
    match apply_proposal_persist(&root, &proposal).await {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(e) => harness_err(StatusCode::BAD_GATEWAY, &e),
    }
}

// ── P4：Guidance 管理 + 需求讨论 ────────────────────────────────────────

async fn guidance_h(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let root = data_root_path(&state);
    Json(json!({ "guidances": list_guidance(&root) })).into_response()
}

#[derive(Deserialize)]
struct GuidanceAddBody {
    title: String,
    description: String,
    #[serde(default)]
    source: Option<String>,
}

async fn guidance_add_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GuidanceAddBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let title = body.title.trim().to_string();
    let description = body.description.trim().to_string();
    if title.is_empty() || description.is_empty() {
        return harness_err(StatusCode::BAD_REQUEST, "title and description are required");
    }
    let source = body.source.unwrap_or_else(|| "user".to_string());
    let root = data_root_path(&state);
    match add_guidance(&root, &title, &description, &source) {
        Ok(g) => (StatusCode::CREATED, Json(json!({ "guidance": g }))).into_response(),
        Err(e) => harness_err(StatusCode::BAD_GATEWAY, &e),
    }
}

async fn guidance_delete_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let root = data_root_path(&state);
    match deactivate_guidance(&root, &id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => harness_err(StatusCode::NOT_FOUND, &e),
    }
}

#[derive(Deserialize)]
struct DiscussBody {
    message: String,
    #[serde(default)]
    auto_commit: bool,
}

async fn discuss_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DiscussBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let Some(llm) = LlmClientImpl::from_env() else {
        return harness_err(StatusCode::BAD_REQUEST, "LLM provider not configured");
    };
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return harness_err(StatusCode::BAD_REQUEST, "message is required");
    }
    let root = data_root_path(&state);
    match discuss(&root, &llm, &message, "", body.auto_commit).await {
        Ok(result) => Json(json!({ "result": result })).into_response(),
        Err(e) => harness_err(StatusCode::BAD_GATEWAY, &e),
    }
}