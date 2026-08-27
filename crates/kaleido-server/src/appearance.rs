//! Appearance 2.0 — durable wallpaper + helpers for per-user theme assets.
//!
//! Routes:
//! - POST   /api/v1/appearance/wallpaper  JSON { filename?, contentType?, dataBase64 }
//! - GET    /api/v1/appearance/wallpaper  raw image bytes (Bearer or one-time ?ticket= — M-3; CSS background-image cannot send headers, so the client fetches a ticket first)
//! - DELETE /api/v1/appearance/wallpaper
//!
//! App-state appearance blob lives in GET/PUT /api/v1/app-state under `ui.appearance`
//! (no schema lock — free-form JSON merge on client).

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

use crate::{extract_bearer, map_core_err, tickets::consume_sse_ticket, AppState};
use crate::error_codes::*;
use kaleido_core::SessionRecord;

const MAX_WALLPAPER_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/v1/appearance/wallpaper",
        post(upload_wallpaper)
            .get(get_wallpaper)
            .delete(delete_wallpaper),
    )
}

fn user_appearance_dir(state: &AppState, user_id: &str) -> PathBuf {
    state
        .auth
        .data_root()
        .root()
        .join("users")
        .join(user_id)
        .join("appearance")
}

fn wallpaper_bin(state: &AppState, user_id: &str) -> PathBuf {
    user_appearance_dir(state, user_id).join("wallpaper.bin")
}

fn wallpaper_meta(state: &AppState, user_id: &str) -> PathBuf {
    user_appearance_dir(state, user_id).join("wallpaper.meta.json")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WallpaperUpload {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default, alias = "content_type", alias = "mime")]
    content_type: Option<String>,
    /// Accept camelCase dataBase64 (rename_all) + snake/aliases.
    #[serde(alias = "data", alias = "base64", alias = "data_base64")]
    data_base64: String,
}

/// GET wallpaper query params. Legacy `token` kept only for serde tolerance —
/// it is parsed then ignored (raw query tokens are no longer an auth path).
#[derive(Debug, Deserialize, Default)]
struct TokenQuery {
    #[serde(default)]
    #[allow(dead_code)]
    token: Option<String>,
    /// M-3: one-time SSE ticket — the sanctioned query credential now that
    /// raw `?token=` is gone (CSS background-image cannot send headers).
    #[serde(default)]
    ticket: Option<String>,
}

fn session_from_headers_or_ticket(
    state: &AppState,
    headers: &HeaderMap,
    query_ticket: Option<&str>,
) -> Result<SessionRecord, Response> {
    if let Some(t) = query_ticket.map(str::trim).filter(|s| !s.is_empty()) {
        // Single-use: consume immediately; wrong/expired ticket → 401.
        if let Some(token) = consume_sse_ticket(t) {
            return state.auth.resolve_session(&token).map_err(map_core_err);
        }
        return Err(unauthorized("invalid or expired SSE ticket"));
    }
    let token =
        extract_bearer(headers).ok_or_else(|| unauthorized("missing bearer"))?;
    state.auth.resolve_session(&token).map_err(map_core_err)
}

fn sniff_content_type(bytes: &[u8], hinted: Option<&str>) -> &'static str {
    if bytes.len() >= 3 && bytes[0] == 0xff && bytes[1] == 0xd8 && bytes[2] == 0xff {
        return "image/jpeg";
    }
    if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        return "image/png";
    }
    if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        return "image/gif";
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    match hinted.unwrap_or("").to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

async fn upload_wallpaper(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WallpaperUpload>,
) -> Response {
    let sess = match session_from_headers_or_ticket(&state, &headers, None) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let raw_b64 = body.data_base64.trim();
    // allow data URL prefix
    let raw_b64 = raw_b64.split(',').last().unwrap_or(raw_b64).trim();
    let bytes = match B64.decode(raw_b64) {
        Ok(b) => b,
        Err(e) => {
            return bad_request("APPEAR_INVALID", format!("invalid base64: {e}"));
        }
    };
    if bytes.is_empty() {
        return bad_request("APPEAR_EMPTY", "empty wallpaper");
    }
    if bytes.len() > MAX_WALLPAPER_BYTES {
        return err_with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            "APPEAR_TOO_LARGE",
            format!("wallpaper exceeds {} bytes", MAX_WALLPAPER_BYTES),
            serde_json::json!({ "maxBytes": MAX_WALLPAPER_BYTES, "got": bytes.len() }),
        );
    }

    let ct = sniff_content_type(&bytes, body.content_type.as_deref());
    if !ct.starts_with("image/") {
        return err_with_code(
            StatusCode::BAD_REQUEST,
            "APPEAR_BAD_REQUEST", "only image/* wallpapers are accepted",
            serde_json::json!({"contentType": ct}),
        );
    }

    let dir = user_appearance_dir(&state, &sess.user_id);
    if let Err(e) = fs::create_dir_all(&dir) {
        return internal("APPEAR_INTERNAL", e.to_string());
    }
    let bin = wallpaper_bin(&state, &sess.user_id);
    let meta_path = wallpaper_meta(&state, &sess.user_id);
    if let Err(e) = fs::write(&bin, &bytes) {
        return internal("APPEAR_INTERNAL", e.to_string());
    }
    let meta = json!({
        "contentType": ct,
        "filename": body.filename.clone().unwrap_or_else(|| "wallpaper".into()),
        "bytes": bytes.len(),
        "updatedAt": chrono::Utc::now().to_rfc3339(),
    });
    let _ = fs::write(
        &meta_path,
        serde_json::to_string_pretty(&meta).unwrap_or_else(|_| "{}".into()),
    );

    // Relative URL — client resolves a one-time ticket and appends ?ticket= (M-3)
    Json(json!({
        "ok": true,
        "url": "/api/v1/appearance/wallpaper",
        "contentType": ct,
        "bytes": bytes.len(),
        "filename": meta.get("filename").cloned().unwrap_or(json!("wallpaper")),
    }))
    .into_response()
}

async fn get_wallpaper(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    let sess = match session_from_headers_or_ticket(&state, &headers, q.ticket.as_deref()) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let bin = wallpaper_bin(&state, &sess.user_id);
    if !bin.exists() {
        return not_found("APPEAR_NOT_FOUND", "no wallpaper uploaded");
    }
    let bytes = match fs::read(&bin) {
        Ok(b) => b,
        Err(e) => {
            return internal("APPEAR_INTERNAL", e.to_string());
        }
    };
    let mut ct = "application/octet-stream".to_string();
    let meta_path = wallpaper_meta(&state, &sess.user_id);
    if let Ok(raw) = fs::read_to_string(&meta_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(s) = v.get("contentType").and_then(|x| x.as_str()) {
                ct = s.to_string();
            }
        }
    }
    if ct == "application/octet-stream" {
        ct = sniff_content_type(&bytes, None).to_string();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ct)
        .header(header::CACHE_CONTROL, "private, max-age=3600")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| {
            return internal("APPEAR_FAILED", "response build failed");
        })
}

async fn delete_wallpaper(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let sess = match session_from_headers_or_ticket(&state, &headers, None) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let bin = wallpaper_bin(&state, &sess.user_id);
    let meta = wallpaper_meta(&state, &sess.user_id);
    let _ = fs::remove_file(&bin);
    let _ = fs::remove_file(&meta);
    Json(json!({"ok": true})).into_response()
}
