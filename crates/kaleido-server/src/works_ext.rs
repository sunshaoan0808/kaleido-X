//! Works extensions (S7-W2).
//! POST /api/v1/works/move
//! POST /api/v1/works/create-untitled
//! GET  /api/v1/works/export?path=
//! GET  /api/v1/works/image-data-url?path=

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::path::Path;

use kaleido_core::{WORKS_MAX_IMAGE_BYTES, CoreError};
use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/works/move", post(works_move))
        .route("/api/v1/works/create-untitled", post(create_untitled))
        .route("/api/v1/works/export", get(works_export))
        .route("/api/v1/works/image-data-url", get(image_data_url))
}

#[derive(Debug, Deserialize)]
struct MoveBody {
    from: String,
    to: String,
}

#[derive(Debug, Deserialize)]
struct CreateUntitledBody {
    #[serde(default)]
    dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    path: String,
}

async fn works_move(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MoveBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if body.from.trim().is_empty() || body.to.trim().is_empty() {
        return bad_request("WKX_MISSING_FIELD", "from and to are required");
    }
    // Path-jail via WorksFs::rename (same as /works/rename).
    match state
        .works
        .rename(&session.workspace_id, &body.from, &body.to)
    {
        Ok(st) => Json(st).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn create_untitled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateUntitledBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let dir = body
        .dir
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_matches('/')
        .to_string();

    // Ensure parent dir exists when non-root.
    if !dir.is_empty() {
        // mkdir is idempotent via create_dir_all in WorksFs.
        if let Err(e) = state.works.mkdir(&session.workspace_id, &dir) {
            // Ignore "already exists as file" style errors only if stat says dir.
            match state.works.stat(&session.workspace_id, &dir) {
                Ok(st) if st.kind == "dir" => {}
                _ => return map_core_err(e),
            }
        }
    }

    let next = match next_untitled_name(&state, &session.workspace_id, &dir) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let rel = if dir.is_empty() {
        next.clone()
    } else {
        format!("{dir}/{next}")
    };

    match state
        .works
        .write_text(&session.workspace_id, &rel, "")
    {
        Ok(body) => Json(json!({
            "path": body.path,
            "name": next,
            "content": body.content,
            "size": body.size,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

fn next_untitled_name(
    state: &AppState,
    workspace_id: &str,
    dir: &str,
) -> Result<String, Response> {
    // Scan shallow list of the target directory for Untitled-N.md.
    let entry = state
        .works
        .list(workspace_id, dir, 1)
        .map_err(map_core_err)?;
    let mut used = std::collections::HashSet::new();
    for child in &entry.children {
        let name = child.name.as_str();
        if let Some(n) = parse_untitled_n(name) {
            used.insert(n);
        }
    }
    let mut n: u32 = 1;
    while used.contains(&n) {
        n = n.saturating_add(1);
        if n > 10_000 {
            return Err(internal("WKX_TOO_MANY_FILES", "too many Untitled-N.md files"));
        }
    }
    Ok(format!("Untitled-{n}.md"))
}

fn parse_untitled_n(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("Untitled-")?.strip_suffix(".md")?;
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

async fn works_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PathQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if q.path.trim().is_empty() {
        return bad_request("WKX_MISSING_FIELD", "path required");
    }
    match state.works.read_text(&session.workspace_id, &q.path) {
        Ok(body) => Json(json!({
            "path": body.path,
            "content": body.content,
            "exportedAt": Utc::now().to_rfc3339(),
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn image_data_url(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PathQuery>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if q.path.trim().is_empty() {
        return bad_request("WKX_MISSING_FIELD", "path required");
    }
    let mime = match image_mime(&q.path) {
        Some(m) => m,
        None => {
            return bad_request("WKX_BAD_REQUEST", "not an image path");
        }
    };
    let abs = match state.works.resolve(&session.workspace_id, &q.path, false) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let meta = match fs::metadata(&abs) {
        Ok(m) => m,
        Err(e) => {
            return internal("WKX_INTERNAL", e.to_string())
        }
    };
    if !meta.is_file() {
        return map_core_err(CoreError::works_not_file());
    }
    // Cap image reads similarly to works text limit (W11).
    if meta.len() > WORKS_MAX_IMAGE_BYTES {
        return map_core_err(CoreError::works_too_large(
            "file",
            meta.len(),
            WORKS_MAX_IMAGE_BYTES,
        ));
    }
    let bytes = match fs::read(&abs) {
        Ok(b) => b,
        Err(e) => {
            return internal("WKX_INTERNAL", e.to_string())
        }
    };
    // Magic-byte sniff as a soft check; extension already gated mime.
    if !looks_like_image(&bytes, mime) {
        return bad_request("WKX_BAD_REQUEST", "file content is not a recognized image");
    }
    let b64 = B64.encode(&bytes);
    let data_url = format!("data:{mime};base64,{b64}");
    Json(json!({
        "path": q.path,
        "mime": mime,
        "dataUrl": data_url,
        "size": bytes.len(),
    }))
    .into_response()
}

fn image_mime(path: &str) -> Option<&'static str> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())?;
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "svg" => Some("image/svg+xml"),
        "ico" => Some("image/x-icon"),
        _ => None,
    }
}

fn looks_like_image(bytes: &[u8], mime: &str) -> bool {
    if mime == "image/svg+xml" {
        // UTF-8-ish text containing svg
        let sample = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
        return sample.contains("<svg");
    }
    if bytes.len() < 4 {
        return false;
    }
    match mime {
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G']),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => bytes.starts_with(b"GIF8"),
        "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/bmp" => bytes.starts_with(b"BM"),
        "image/x-icon" => bytes.starts_with(&[0x00, 0x00, 0x01, 0x00])
            || bytes.starts_with(&[0x00, 0x00, 0x02, 0x00]),
        _ => true,
    }
}
