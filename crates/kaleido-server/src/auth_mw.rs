//! Auth middleware + bearer/session helpers + login DTOs (P0-1)
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, Method},
    middleware::Next,
    response::Response,
};
use kaleido_core::SessionRecord;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error_map::map_core_err;
use crate::error_codes::*;
use crate::tickets::consume_sse_ticket;
use std::net::SocketAddr;

use crate::state::AppState;

#[derive(Deserialize)]
pub(crate) struct LoginRequest {
    pub(crate) username: String,
    pub(crate) password: String,
}

#[derive(Serialize)]
pub(crate) struct LoginResponse {
    pub(crate) token: String,
    pub(crate) user_id: String,
    pub(crate) username: String,
    pub(crate) workspace_id: String,
    pub(crate) expires_at: String,
}

pub(crate) fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
        {
            return Some(t.trim().to_string());
        }
    }
    // mobile compat: X-Mobile-Token / X-Kaleido-Token
    for key in ["x-mobile-token", "x-kaleido-token"] {
        if let Some(v) = headers.get(key).and_then(|v| v.to_str().ok()) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

pub(crate) fn client_key(headers: &HeaderMap, connect: Option<&SocketAddr>) -> String {
    if let Some(xff) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
    {
        return format!("ip:{}", xff.trim());
    }
    if let Some(addr) = connect {
        return format!("ip:{}", addr.ip());
    }
    "ip:unknown".into()
}

pub(crate) async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // public
    // Public: health/login + GET bookshelf list/content/cover only (writes need auth)
    let novels_get_public = method == Method::GET
        && (path == "/api/v1/crawler/novels"
            || path.starts_with("/api/v1/crawler/novels/"));
    if matches!(
        path.as_str(),
        "/health" | "/api/v1/auth/login" | "/api/v1/public/info"
    ) || novels_get_public
        || path.starts_with("/web")
        || path == "/"
        || path == "/index.html"
        || path.starts_with("/assets/")
    {
        return next.run(req).await;
    }

    // OPTIONS for CORS preflight
    if method == Method::OPTIONS {
        return next.run(req).await;
    }

    // SSE 认证：仅接受一次性 ticket（M-3）。?token=/access_token= 回退已移除
    // （URL 会进访问日志/历史/Referer，见 docs/SECURITY_NOTES.md）。
    let token = extract_bearer(req.headers());
    if token.is_none() {
        if let Some(q) = req.uri().query() {
            let has_ticket = q.split('&').any(|part| {
                let mut kv = part.splitn(2, '=');
                matches!((kv.next(), kv.next()), (Some("ticket"), Some(_)))
            });
            if has_ticket {
                // Ticket-authenticated SSE request — let the handler consume & verify it.
                return next.run(req).await;
            }
        }
    }

    let Some(token) = token else {
        return unauthorized("missing bearer");
    };
    match state.auth.resolve_session(&token) {
        Ok(_) => next.run(req).await,
        Err(_) => unauthorized("invalid or expired session"),
    }
}


// ── M-3: short-lived one-time SSE tickets ─────────────────────────────────
// EventSource cannot set custom headers; the one-time ticket exists so the
// long-lived bearer never travels in a URL (leaked to logs/history/Referer).
// Clients POST a
// one-time, 5-minute-expiring ticket with their Authorization header, then use
// `?ticket=` for the SSE subscription. The ticket is single-use and bound to
// the issuing user.
pub(crate) fn session_from(state: &AppState, headers: &HeaderMap) -> Result<SessionRecord, Response> {
    session_from_any(state, headers, None)
}

/// P0-2 审计修复：admin 专用 session 解析——非 admin 一律 403。
/// ai_admin / sessions_stats / prune 等管理 handler 用它，防止越权访问管理面。
pub(crate) fn admin_session_from(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<SessionRecord, Response> {
    let session = session_from(state, headers)?;
    if !session.is_admin {
        return Err(forbidden("ADMIN_REQUIRED", "admin role required"));
    }
    Ok(session)
}

/// SSE/EventSource 场景认证：header Bearer 或一次性 ticket（M-3）。
/// 历史 `?token=`/`?access_token=` query 回退已移除——URL 凭据会泄漏到
/// 访问日志、浏览器历史与 Referer；EventSource 一律走 ticket。
pub(crate) fn session_from_any(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&HashMap<String, String>>,
) -> Result<SessionRecord, Response> {
    if let Some(ticket) = query.and_then(|q| q.get("ticket")) {
        if let Some(token) = consume_sse_ticket(ticket) {
            return state.auth.resolve_session(&token).map_err(map_core_err);
        }
        return Err(unauthorized("invalid or expired SSE ticket"));
    }
    let token =
        extract_bearer(headers).ok_or_else(|| unauthorized("missing bearer"))?;
    state.auth.resolve_session(&token).map_err(map_core_err)
}
