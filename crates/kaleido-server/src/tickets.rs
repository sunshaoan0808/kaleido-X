//! SSE one-shot tickets (P0-1)
use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::state::AppState;
use crate::error_codes::*;
use crate::auth_mw::extract_bearer;
use crate::error_map::map_core_err;

// ── M-3: short-lived one-time SSE tickets ─────────────────────────────────
// EventSource cannot set custom headers; this ticket exists so the long-lived
// bearer never travels in a URL (leaked to logs/history/Referer). Clients POST
// a one-time, 5-minute-expiring ticket with their Authorization header, then
// use `?ticket=` for the SSE subscription. The ticket is single-use and bound
// to the issuing user.
#[derive(Clone)]
pub(crate) struct SseTicket {
    token: String,
    expires_at: u64, // unix seconds
    used: bool,
}

pub(crate) fn sse_ticket_store() -> &'static parking_lot::Mutex<std::collections::HashMap<String, SseTicket>> {
    use std::sync::OnceLock;
    static STORE: OnceLock<parking_lot::Mutex<std::collections::HashMap<String, SseTicket>>> =
        OnceLock::new();
    STORE.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn issue_sse_ticket(token: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut ticket; // [P7] 首值无意义，循环内必赋值后才读取（循环重试 ticket 冲突时需重赋值）
    let seed = format!(
        "{}-{}",
        token,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    // Simple high-entropy pseudo-random ticket (hex digest over seed + counter).
    let mut h: u64 = 14695981039346656037;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    let mut counter = 0u64;
    loop {
        let mut h2 = h ^ (counter << 32) ^ counter;
        let chunk = (0..16)
            .map(|_| {
                h2 = h2.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                format!("{:02x}", (h2 >> 33) & 0xff)
            })
            .collect::<String>();
        ticket = format!("st_{}", chunk);
        let mut store = sse_ticket_store().lock();
        if !store.contains_key(&ticket) {
            store.insert(
                ticket.clone(),
                SseTicket {
                    token: token.to_string(),
                    expires_at: now_unix() + 300,
                    used: false,
                },
            );
            return ticket;
        }
        counter += 1;
    }
}

/// Consume a one-time SSE ticket; returns the bound auth token if valid & unused.
pub(crate) fn consume_sse_ticket(ticket: &str) -> Option<String> {
    if !ticket.starts_with("st_") {
        return None;
    }
    let mut store = sse_ticket_store().lock();
    let entry = store.get_mut(ticket)?;
    if entry.used {
        return None;
    }
    if now_unix() > entry.expires_at {
        store.remove(ticket);
        return None;
    }
    entry.used = true;
    Some(entry.token.clone())
}

pub(crate) async fn sse_ticket_endpoint(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // M-3: require a real bearer token; bind a one-time ticket to it.
    let token = match extract_bearer(&headers) {
        Some(t) => t,
        None => {
            return unauthorized("missing bearer");
        }
    };
    if let Err(e) = state.auth.resolve_session(&token) {
        return map_core_err(e);
    }
    let ticket = issue_sse_ticket(&token);
    Json(json!({
        "ticket": ticket,
        "expires_in": 300,
    }))
    .into_response()
}
