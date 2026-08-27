//! M1: Book annotation anchors — paragraph-level anchor + comment/feedback.
//!
//! Routes:
//! - `POST /api/v1/book/{slug}/anchors` — create anchor + comment (auth required)
//! - `GET  /api/v1/book/{slug}/anchors` — list anchors for a book (public read)
//! - `DELETE /api/v1/book/{slug}/anchors/{id}` — delete an anchor (auth required)

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{post, delete},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{session_from, AppState};

/// Directory for per-book annotation files.
fn annotations_dir() -> std::path::PathBuf {
    crate::config::ServerConfig::data_root().join("state").join("book-annotations")
}

/// Path to the annotation file for a given book slug.
fn annotation_file(slug: &str) -> std::path::PathBuf {
    annotations_dir().join(format!("{slug}.json"))
}

/// Validate a book slug against `^[a-zA-Z0-9_-]{1,64}$` (no dots, no separators).
///
/// The slug is joined directly into a filesystem leaf as `{slug}.json`, so it must
/// never contain path-traversal / separator characters. We reject empty and
/// over-long slugs, and only allow ASCII alphanumerics plus `-` and `_`. This
/// disallows `.` (and thus `.`/`..`), `/`, `\`, and NUL, so the joined path can
/// never escape `annotations_dir()`.
fn validate_slug(slug: &str) -> Result<(), String> {
    let t = slug.trim();
    if t.is_empty() {
        return Err("slug is empty".into());
    }
    if t.len() > 64 {
        return Err(format!("slug too long ({} > 64)", t.len()));
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("slug must match ^[a-zA-Z0-9_-]{1,64}$".into());
    }
    Ok(())
}

/// Reject an invalid slug with a 400 response, or continue the handler.
fn check_slug(slug: &str) -> Result<(), Response> {
    match validate_slug(slug) {
        Ok(()) => Ok(()),
        Err(msg) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": format!("invalid slug: {msg}")})),
        )
            .into_response()),
    }
}

/// Anchor data for a single review comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookAnchor {
    pub id: String,
    pub book_slug: String,
    /// Optional chapter title/heading for context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<String>,
    /// Byte offset into the content where the anchor starts.
    pub offset: usize,
    /// Byte length of the anchored text.
    pub len: usize,
    /// The anchored text (quote) — stored for reference and display.
    pub anchor_text: String,
    /// The review comment / feedback text.
    pub comment: String,
    /// Author username.
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Request body for creating an anchor.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorCreateBody {
    /// Chapter title/heading (optional, for context).
    #[serde(default)]
    pub chapter: Option<String>,
    /// Byte offset into the book content where the anchor starts.
    pub offset: usize,
    /// Byte length of the anchored text.
    pub len: usize,
    /// The anchored text (quote) — must match what's at offset..offset+len.
    pub anchor_text: String,
    /// Review comment.
    pub comment: String,
}

/// Request body for validating an anchor against content snapshot.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorValidateBody {
    /// The full content snapshot of the book/chapter at time of creation.
    pub content: String,
    /// Byte offset.
    pub offset: usize,
    /// Byte length.
    pub len: usize,
    /// The anchored text (quote).
    pub anchor_text: String,
}

/// Public routes for book annotations.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/book/{slug}/anchors",
            post(create_anchor).get(list_anchors),
        )
        .route(
            "/api/v1/book/{slug}/anchors/{id}",
            delete(delete_anchor),
        )
        .route(
            "/api/v1/book/{slug}/anchors/validate",
            post(validate_anchor),
        )
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Check that `offset` falls on a valid UTF-8 character boundary.
/// [P13 2026-08-26] fix: `&content[..offset]` 本身在 offset 非边界时会 panic
/// （与 run_post_check quote 裁剪同源问题）——改用 `is_char_boundary` 纯判断。
fn is_utf8_boundary(content: &str, offset: usize) -> bool {
    content.is_char_boundary(offset)
}

/// Validate that offset+len is valid and the quote matches.
fn validate_anchor_range(content: &str, offset: usize, len: usize, quote: &str) -> Result<(), String> {
    if offset > content.len() {
        return Err("offset exceeds content length".into());
    }
    if offset + len > content.len() {
        return Err("offset+len exceeds content length".into());
    }
    if !is_utf8_boundary(content, offset) {
        return Err("offset is not on a valid UTF-8 character boundary".into());
    }
    if !is_utf8_boundary(content, offset + len) {
        return Err("offset+len is not on a valid UTF-8 character boundary".into());
    }
    // The content at offset..offset+len may be a sub-slice of the actual UTF-8 chars.
    // We need to align to character boundaries for display.
    let actual_start = find_char_boundary_start(content, offset);
    let actual_end = find_char_boundary_end(content, offset + len);
    let actual_quote = &content[actual_start..actual_end];
    if actual_quote != quote {
        return Err(format!(
            "quote mismatch: expected {:?}, got {:?}",
            actual_quote, quote
        ));
    }
    Ok(())
}

/// Find the start of the character containing `offset`.
fn find_char_boundary_start(content: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let mut i = offset;
    while i > 0 && (content.as_bytes()[i] & 0b1100_0000 == 0b1000_0000) {
        i -= 1;
    }
    i
}

/// Find the end of the character containing `offset`.
fn find_char_boundary_end(content: &str, offset: usize) -> usize {
    if offset >= content.len() {
        return content.len();
    }
    let mut i = offset;
    while i < content.len() && (content.as_bytes()[i] & 0b1100_0000 == 0b1000_0000) {
        i += 1;
    }
    i
}

fn load_annotations(slug: &str) -> Vec<BookAnchor> {
    let path = annotation_file(slug);
    if !path.exists() {
        return vec![];
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_annotations(slug: &str, anchors: &[BookAnchor]) -> Result<(), String> {
    let dir = annotations_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = annotation_file(slug);
    let s = serde_json::to_string_pretty(anchors).map_err(|e| e.to_string())?;
    std::fs::write(&path, s).map_err(|e| e.to_string())
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// POST /api/v1/book/{slug}/anchors — create a new anchor + comment.
async fn create_anchor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<AnchorCreateBody>,
) -> Response {
    // Auth required for write
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    // Path-traversal guard: slug must be a single safe token.
    if let Err(resp) = check_slug(&slug) {
        return resp;
    }

    // Basic validation
    if body.anchor_text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "anchor_text is required"})),
        )
            .into_response();
    }
    if body.comment.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "comment is required"})),
        )
            .into_response();
    }
    if body.len == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "len must be > 0"})),
        )
            .into_response();
    }
    // Check UTF-8 boundary on offset
    // We can't verify against the full content here since we don't pass it,
    // but we enforce that offset is non-negative. Full validation is done via
    // the /validate endpoint or on client-side content snapshot.
    // However, offset must at least be a reasonable number.
    // The client is expected to have validated against its content snapshot.

    let now = Utc::now().to_rfc3339();
    let anchor = BookAnchor {
        id: Uuid::new_v4().to_string(),
        book_slug: slug.clone(),
        chapter: body.chapter,
        offset: body.offset,
        len: body.len,
        anchor_text: body.anchor_text,
        comment: body.comment,
        author: session.username.clone(),
        created_at: now.clone(),
        updated_at: now,
    };

    let mut anchors = load_annotations(&slug);
    anchors.push(anchor.clone());
    if let Err(e) = save_annotations(&slug, &anchors) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({"ok": true, "anchor": anchor})),
    )
        .into_response()
}

/// GET /api/v1/book/{slug}/anchors — list anchors (public read).
async fn list_anchors(
    Path(slug): Path<String>,
) -> Response {
    if let Err(resp) = check_slug(&slug) {
        return resp;
    }
    let anchors = load_annotations(&slug);
    Json(json!({"ok": true, "anchors": anchors, "count": anchors.len()})).into_response()
}

/// DELETE /api/v1/book/{slug}/anchors/{id} — delete an anchor (auth required).
async fn delete_anchor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((slug, id)): Path<(String, String)>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    // Path-traversal guard.
    if let Err(resp) = check_slug(&slug) {
        return resp;
    }

    let mut anchors = load_annotations(&slug);
    let before = anchors.len();
    anchors.retain(|a| a.id != id);
    if anchors.len() == before {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "anchor not found"})),
        )
            .into_response();
    }
    if let Err(e) = save_annotations(&slug, &anchors) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response();
    }

    Json(json!({"ok": true, "deleted": true})).into_response()
}

/// POST /api/v1/book/{slug}/anchors/validate — validate an anchor against content.
async fn validate_anchor(
    Path(slug): Path<String>,
    Json(body): Json<AnchorValidateBody>,
) -> Response {
    if let Err(resp) = check_slug(&slug) {
        return resp;
    }
    if body.anchor_text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "anchor_text is required"})),
        )
            .into_response();
    }
    if body.len == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "len must be > 0"})),
        )
            .into_response();
    }

    match validate_anchor_range(&body.content, body.offset, body.len, &body.anchor_text) {
        Ok(()) => Json(json!({"ok": true, "valid": true})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "valid": false, "error": e})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod anchor_boundary_tests {
    use super::*;

    /// [P13 2026-08-26] 回归：is_utf8_boundary 旧实现用 &content[..offset] 探测，
    /// offset 非边界时探测本身 panic（500 而非 400）。
    #[test]
    fn is_utf8_boundary_never_panics_and_judges_correctly() {
        let content = "第一段内容在这里。第二段继续。"; // 每个汉字 3 字节
        assert!(is_utf8_boundary(content, 0));
        assert!(is_utf8_boundary(content, content.len()));
        assert!(is_utf8_boundary(content, 3)); // '第' 之后
        assert!(!is_utf8_boundary(content, 1)); // '第' 内部
        assert!(!is_utf8_boundary(content, 28)); // '。'(27..30) 内部
        assert!(!is_utf8_boundary(content, content.len() + 10));
    }

    #[test]
    fn validate_anchor_range_rejects_bad_offset_gracefully() {
        let content = "第一段内容在这里。第二段继续。";
        // 非边界 offset → 应返回 Err 而非 panic。
        assert!(validate_anchor_range(content, 28, 3, "第二").is_err());
        // 合法锚点通过。
        assert!(validate_anchor_range(content, 27, 9, "第二段").is_ok());
        // quote 不匹配报错。
        assert!(validate_anchor_range(content, 27, 9, "第三段").is_err());
    }
}
