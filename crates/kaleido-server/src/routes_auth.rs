//! Auth/session endpoints: health/public-info/login/logout/stats/prune/me/ping — P0-1 Stage5
use axum::{
    extract::{ConnectInfo, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::auth_mw::{session_from, admin_session_from, client_key, LoginRequest, LoginResponse};
use crate::error_codes::*;
use crate::error_map::map_core_err;
use crate::state::AppState;
use chrono::Utc;
use serde_json::Value;
use std::net::SocketAddr;
use crate::auth_mw::extract_bearer;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/public/info", get(public_info))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        // M-3: one-time SSE ticket exchange (lost in the P0-1e route extraction —
        // restored; requires a valid bearer, so it stays behind auth_middleware).
        .route("/api/v1/auth/sse-ticket", post(crate::sse_ticket_endpoint))
        .route("/api/v1/auth/sessions/stats", get(sessions_stats))
        .route("/api/v1/auth/sessions/prune", post(sessions_prune))
        .route("/api/v1/auth/me", get(me))
        // T8/P13: frontend-guard compat alias (same handler, same payload shape).
        .route("/api/v1/me", get(me))
        .route("/api/v1/data/ping", get(data_ping))
}

pub(crate) async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let llm_ok = {
        let rt = state.app_state.resolve_llm(
            state.llm_base.as_deref(),
            state.llm_key.as_deref(),
            &state.llm_model,
        );
        !rt.base_url.trim().is_empty() && !rt.api_key.trim().is_empty()
    };
    Json(json!({
        "ok": true,
        "service": "kaleido-server",
        "version": env!("CARGO_PKG_VERSION"),
        "phase": "S8",
        "llm_configured": llm_ok,
        "embedding": crate::embed_local::status(),
        "max_concurrent_jobs": state.jobs.max_concurrent(),
        "running_jobs": state.jobs.running_count(),
        "queued_jobs": state.jobs.queued_count(),
        "jobs_metrics": {
            "uptime_secs": Utc::now().timestamp().max(0) as u64
                - state.jobs.metrics.boot_at_unix.load(std::sync::atomic::Ordering::Relaxed),
            "peak_running_since_boot": state.jobs.metrics.peak_running.load(std::sync::atomic::Ordering::Relaxed),
            // P10: totals 已跨重启持久化（jobs_dir/metrics.json），口径为累计值而非 since_boot。
            "totals": {
                "created": state.jobs.metrics.total_created.load(std::sync::atomic::Ordering::Relaxed),
                "succeeded": state.jobs.metrics.total_succeeded.load(std::sync::atomic::Ordering::Relaxed),
                "failed": state.jobs.metrics.total_failed.load(std::sync::atomic::Ordering::Relaxed),
                "cancelled": state.jobs.metrics.total_cancelled.load(std::sync::atomic::Ordering::Relaxed),
            }
        },
        "time": Utc::now().to_rfc3339(),
    }))
}

pub(crate) async fn public_info(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "name": "kaleido-server",
        "auth": "password",
        "phase": "S8",
        "multi_user_ready": true,
        "note": "single-user runtime; schema has user_id/workspace_id; jobs v2 queues on overflow",
        "llm_model_default": state.llm_model,
        "max_concurrent_jobs": state.jobs.max_concurrent(),
        "features": {
            "chat_sse": true,
            "mobile_compat": true,
            "agent_sessions": true,
            "jobs": true,
            "jobs_v2": true,
            "works_fs": true,
            "crawler": "settings-gated-live",
            "bash_sandbox": "settings-gated",
            "outline_llm_polish": true,
            "android_shell": "capacitor"
        },
        "endpoints": {
            "login": "/api/v1/auth/login",
            "mobile_chat_start": "/api/mobile/chat/start",
            "mobile_stream": "/api/mobile/stream?runId=",
            "sessions": "/api/mobile/sessions",
            "works": "/api/v1/works",
            "jobs": "/api/v1/jobs",
            "jobs_stream": "/api/v1/jobs/{id}/stream"
        },
        "concurrency": {
            "max": state.jobs.max_concurrent(),
            "overflow": "queue",
            "env": "KALEIDO_MAX_CONCURRENT_JOBS",
            "note": "create() queues when at capacity; try_start (chat) still RateLimited/429"
        }
    }))
}

pub(crate) async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let rate_key = client_key(&headers, Some(&addr));
    match state.auth.login(&body.username, &body.password, &rate_key) {
        Ok(s) => Json(LoginResponse {
            token: s.token,
            user_id: s.user_id,
            username: s.username,
            workspace_id: s.workspace_id,
            expires_at: s.expires_at.to_rfc3339(),
        })
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(token) = extract_bearer(&headers) {
        let _ = state.auth.logout(&token);
    }
    Json(json!({"ok": true}))
}

/// W12: auth session capacity stats (not story-tavern sessions).
pub(crate) async fn sessions_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // P0-2 审计修复：管理面信息仅 admin 可见，防越权枚举其他用户 session。
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    let stats = state.auth.session_stats();
    let samples = state.auth.list_session_summaries(10);
    Json(json!({
        "ok": true,
        "kind": "auth_sessions",
        "active": stats.active,
        "cap": stats.cap,
        "free": stats.free,
        "policy": stats.policy,
        "ttlHours": stats.ttl_hours,
        "oldestCreatedAt": stats.oldest_created_at,
        "oldestExpiresAt": stats.oldest_expires_at,
        "expiredPresent": stats.expired_present,
        "samples": samples,
        "actions": {
            "pruneExpired": {"method": "POST", "path": "/api/v1/sessions/prune", "body": {"mode": "expired"}},
            "pruneOldest": {"method": "POST", "path": "/api/v1/sessions/prune", "body": {"mode": "oldest", "count": 5}},
            "raiseCap": {"method": "PATCH", "path": "/api/v1/settings", "body": {"sessionMax": stats.cap.saturating_add(20)}},
        }
    }))
    .into_response()
}

/// W12: prune auth sessions — mode=expired|oldest.
pub(crate) async fn sessions_prune(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    // P0-2 审计修复：管理面操作仅 admin 可用。
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }
    let v: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
            }
        }
    };
    let mode = v
        .get("mode")
        .and_then(|x| x.as_str())
        .unwrap_or("expired")
        .to_ascii_lowercase();
    let count = v
        .get("count")
        .and_then(|x| x.as_u64())
        .unwrap_or(5)
        .clamp(1, 500) as usize;
    let result = match mode.as_str() {
        "oldest" | "all_oldest" | "evict" => state.auth.prune_oldest_sessions(count),
        _ => state.auth.prune_expired_sessions(),
    };
    match result {
        Ok(removed) => {
            let stats = state.auth.session_stats();
            Json(json!({
                "ok": true,
                "mode": mode,
                "removed": removed,
                "active": stats.active,
                "cap": stats.cap,
                "free": stats.free,
                "policy": stats.policy,
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match session_from(&state, &headers) {
        Ok(s) => Json(json!({
            "user_id": s.user_id,
            "username": s.username,
            "workspace_id": s.workspace_id,
            "expires_at": s.expires_at.to_rfc3339(),
        }))
        .into_response(),
        Err(r) => r,
    }
}

pub(crate) async fn data_ping(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // L-3: admin-only; strip internal data_root path & pid from response payload.
    if let Err(r) = admin_session_from(&state, &headers) {
        return r;
    }

    let marker = state.auth.data_root().state_file("s1-ping.json");
    let payload = json!({
        "ts": Utc::now().to_rfc3339(),
        "service": "kaleido-server",
        "phase": "S8",
    });
    match std::fs::write(&marker, serde_json::to_string_pretty(&payload).unwrap()) {
        Ok(()) => Json(json!({"ok": true, "wrote": marker.file_name(), "payload": payload})).into_response(),
        Err(e) => internal("AUTHEP_INTERNAL", e.to_string()),
    }
}
