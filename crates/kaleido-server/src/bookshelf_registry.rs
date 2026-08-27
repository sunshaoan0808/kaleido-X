//! M2: BookRegistry — server-side structured bookshelf registry.
//!
//! Routes:
//! - `POST /api/v1/bookshelf/registry` — register / update a book record (auth required)
//! - `GET  /api/v1/bookshelf/registry` — list all registered books (public)
//! - `POST /api/v1/bookshelf/registry/reorder` — reorder books (auth required)
//! - `DELETE /api/v1/bookshelf/registry/{index}` — remove a book by index (auth required, does NOT delete files)

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{session_from, AppState};

// ── Data model ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookRecord {
    /// Shelf slug (unique identifier, matches crawler shelf_slug).
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// Optional metadata (author, genre, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// Manual sort order index (lower = higher).
    pub sort: u32,
    /// When this record was last touched / opened.
    pub last_opened_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RegistryData {
    pub books: Vec<BookRecord>,
    /// Sort mode: "recent" | "manual".
    #[serde(default)]
    pub sort_mode: String,
}

// ── Storage ────────────────────────────────────────────────────────────────

fn registry_file() -> std::path::PathBuf {
    crate::config::ServerConfig::data_root().join("state").join("bookshelf-registry.json")
}

fn load_registry() -> RegistryData {
    let path = registry_file();
    if !path.exists() {
        return RegistryData::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_registry(data: &RegistryData) -> Result<(), String> {
    let path = registry_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let s = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    std::fs::write(&path, s).map_err(|e| e.to_string())
}

// ── Routes ─────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/bookshelf/registry",
            get(list_registry).post(upsert_book),
        )
        .route("/api/v1/bookshelf/registry/reorder", post(reorder_books))
        .route("/api/v1/bookshelf/registry/{index}", delete(remove_book))
}

// ── Handlers ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertBookBody {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub meta: Option<Value>,
}

/// GET /api/v1/bookshelf/registry — list all registered books (public read).
async fn list_registry() -> Response {
    let data = load_registry();
    Json(json!({
        "ok": true,
        "books": data.books,
        "count": data.books.len(),
        "sortMode": data.sort_mode,
    }))
    .into_response()
}

/// POST /api/v1/bookshelf/registry — register or update a book (auth required).
async fn upsert_book(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpsertBookBody>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    if body.slug.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "slug is required"})),
        )
            .into_response();
    }
    if body.title.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "title is required"})),
        )
            .into_response();
    }

    let mut data = load_registry();
    let now = Utc::now().to_rfc3339();

    // Upsert: if slug already exists, update it; else append.
    if let Some(existing) = data.books.iter_mut().find(|b| b.slug == body.slug) {
        existing.title = body.title;
        existing.meta = body.meta;
        existing.last_opened_at = Some(now);
    } else {
        let next_sort = data.books.iter().map(|b| b.sort).max().unwrap_or(0) + 1;
        data.books.push(BookRecord {
            slug: body.slug,
            title: body.title,
            meta: body.meta,
            sort: next_sort,
            last_opened_at: Some(now),
        });
    }

    if let Err(e) = save_registry(&data) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response();
    }

    Json(json!({"ok": true, "books": data.books})).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderBody {
    /// Ordered list of slugs. Books not in the list are appended at the end.
    pub slugs: Vec<String>,
}

/// POST /api/v1/bookshelf/registry/reorder — reorder books (auth required).
async fn reorder_books(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReorderBody>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let mut data = load_registry();
    let slugs_in_order = &body.slugs;

    // Build a map of slug -> record.
    let mut slug_map: std::collections::HashMap<String, BookRecord> = std::collections::HashMap::new();
    for book in data.books.drain(..) {
        slug_map.insert(book.slug.clone(), book);
    }

    // Place books in requested order.
    let mut new_books: Vec<BookRecord> = Vec::new();
    let mut sort_idx: u32 = 0;
    for slug in slugs_in_order {
        if let Some(mut book) = slug_map.remove(slug) {
            book.sort = sort_idx;
            new_books.push(book);
            sort_idx += 1;
        }
    }
    // Append remaining books that weren't in the reorder list.
    let mut remaining: Vec<BookRecord> = slug_map.into_values().collect();
    remaining.sort_by_key(|b| b.sort);
    for mut book in remaining {
        book.sort = sort_idx;
        new_books.push(book);
        sort_idx += 1;
    }

    data.books = new_books;
    if let Err(e) = save_registry(&data) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response();
    }

    Json(json!({"ok": true, "books": data.books})).into_response()
}

/// DELETE /api/v1/bookshelf/registry/{index} — remove a book by index (auth required).
/// Does NOT delete the actual book file on disk, only the registry entry.
async fn remove_book(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(index): Path<usize>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    let mut data = load_registry();
    if index >= data.books.len() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "index out of range"})),
        )
            .into_response();
    }
    let removed = data.books.remove(index);
    if let Err(e) = save_registry(&data) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response();
    }

    Json(json!({
        "ok": true,
        "removed": removed,
    }))
    .into_response()
}
