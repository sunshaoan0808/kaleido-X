//! Partner(world books/character cards) + settings + vector-index + regex-library (P0-1 Stage3)
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use kaleido_core::PartnerItem;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::auth_mw::session_from;
use crate::error_codes::*;
use crate::error_map::map_core_err;
use crate::embed_local;
use crate::state::AppState;
use kaleido_core::PartnerStore;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/partner", get(partner_get).put(partner_put))
        .route("/api/v1/partner/world-books", post(partner_upsert_wb))
        .route("/api/v1/partner/world-books/{id}/entries",
            get(partner_list_wb_entries).put(partner_put_wb_entries).post(partner_create_wb_entry))
        .route("/api/v1/partner/world-books/{id}/entries/{entry_id}",
            patch(partner_patch_wb_entry).delete(partner_delete_wb_entry))
        .route("/api/v1/partner/world-books/{id}", delete(partner_delete_wb))
        .route("/api/v1/partner/world-books/{id}/rebuild-st-book", post(partner_rebuild_wb_st_book))
        .route("/api/v1/partner/character-cards/{id}/rebuild-st-book", post(partner_rebuild_cc_st_book))
        .route("/api/v1/partner/world-books/migrate-legacy", post(partner_migrate_legacy_world_books))
        .route("/api/v1/partner/character-cards", post(partner_upsert_cc))
        .route("/api/v1/partner/character-cards/{id}", delete(partner_delete_cc))
        .route("/api/v1/partner/select", post(partner_select))
        .route("/api/v1/partner/prompt-preview", get(prompt_preview))
        // [P7 修复] P0-1c 拆分时遗漏——st-import/wi-preview 路由自 b6f5304 起未挂载，
        // 前端 src/js/jobs.js + web/assets/app.js 一直在调用 404。恢复挂载。
        .route("/api/v1/partner/st-import", post(crate::st_import::import_st))
        .route("/api/v1/partner/wi-preview", post(crate::st_import::wi_preview))
        .route("/api/v1/regex-library", get(regex_library_get).put(regex_library_put))
        .route("/api/v1/regex-library/import", post(regex_library_import))
        .route("/api/v1/partner/world-books/{id}/vector-index", get(wi_vector_index_get))
        .route("/api/v1/partner/world-books/{id}/vector-index/rebuild", post(wi_vector_index_rebuild))
        .route("/api/v1/partner/vector-query", post(wi_vector_query))
        .route("/api/v1/partner/automation-triggers",
            get(automation_triggers_list).delete(automation_triggers_clear))
        .route("/api/v1/tokenize/estimate", post(tokenize_estimate))
        .route("/api/v1/partner/tokenize/estimate", post(tokenize_estimate))
        .route("/api/v1/settings", get(settings_get).patch(settings_patch))
}

#[derive(Deserialize)]
pub(crate) struct PartnerSelectBody {
    #[serde(default, alias = "worldBookId")]
    world_book_id: Option<String>,
    #[serde(default, alias = "characterCardId")]
    character_card_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    cascade: Option<bool>,
}

// ---------- Partner / settings (S3) ----------

/// P6 hybrid search: story-pack 正文/角色/章节 + 图谱实体（RRF 融合）。
#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    q: String,
    #[serde(default)]
    work_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(crate) async fn api_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if q.q.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "empty query" })),
        )
            .into_response();
    }
    match state
        .search
        .search(q.work_id.as_deref(), &q.q, q.limit.unwrap_or(20))
    {
        Ok(hits) => {
            Json(json!({ "ok": true, "results": hits, "count": hits.len() })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn partner_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.partner.clone().scoped(&sess.user_id).load() {
        Ok(st) => Json(st).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_put(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let st: kaleido_core::PartnerState = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(e) => {
            return bad_request("PARTNER_BAD_STATE", format!("Invalid partner state: {e}"));
        }
    };
    match state.partner.clone().scoped(&sess.user_id).save(st) {
        Ok(s) => Json(s).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_upsert_wb(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let item: PartnerItem = match serde_json::from_str(&body) {
        Ok(i) => i,
        Err(e) => {
            return bad_request("PARTNER_BAD_STATE", format!("Invalid item: {e}"));
        }
    };
    match state.partner.clone().scoped(&sess.user_id).upsert_world_book(item) {
        Ok(i) => Json(i).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_upsert_cc(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let item: PartnerItem = match serde_json::from_str(&body) {
        Ok(i) => i,
        Err(e) => {
            return bad_request("PARTNER_BAD_STATE", format!("Invalid item: {e}"));
        }
    };
    match state.partner.clone().scoped(&sess.user_id).upsert_character_card(item) {
        Ok(i) => Json(i).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_delete_wb(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .delete_world_book(&id, q.cascade.unwrap_or(false))
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_delete_cc(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.partner.clone().scoped(&sess.user_id).delete_character_card(&id) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => map_core_err(e),
    }
}


pub(crate) async fn partner_list_wb_entries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let partner = state.partner.clone().scoped(&sess.user_id);
    match partner.list_world_book_entries(&id) {
        Ok(entries) => Json(json!({
            "ok": true,
            "worldBookId": id,
            "entries": entries,
            "count": entries.len(),
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_put_wb_entries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let partner = state.partner.clone().scoped(&sess.user_id);
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    let entries = if let Some(arr) = v.get("entries").and_then(|x| x.as_array()) {
        arr.clone()
    } else if let Some(arr) = v.as_array() {
        arr.clone()
    } else {
        return bad_request("BAD_ENTRIES_BODY", "请求体需为 {entries:[...]} 或条目数组");
    };
    match partner.put_world_book_entries(&id, entries) {
        Ok(item) => match partner.list_world_book_entries(&id) {
            Ok(list) => Json(json!({
                "ok": true,
                "worldBook": item,
                "entries": list,
                "count": list.len(),
            }))
            .into_response(),
            Err(e) => map_core_err(e),
        },
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_create_wb_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut entry: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    // allow {entry:{...}} wrapper
    if entry.get("entry").is_some() {
        if let Some(inner) = entry.get("entry").cloned() {
            entry = inner;
        }
    }
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .create_world_book_entry(&id, entry)
    {
        Ok(e) => Json(json!({"ok": true, "entry": e})).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_patch_wb_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, entry_id)): Path<(String, String)>,
    body: String,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let patch: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    let patch = patch
        .get("entry")
        .cloned()
        .or_else(|| patch.get("patch").cloned())
        .unwrap_or(patch);
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .patch_world_book_entry(&id, &entry_id, patch)
    {
        Ok(e) => Json(json!({"ok": true, "entry": e})).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_delete_wb_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, entry_id)): Path<(String, String)>,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .delete_world_book_entry(&id, &entry_id)
    {
        Ok(()) => Json(json!({"ok": true, "deleted": entry_id})).into_response(),
        Err(e) => map_core_err(e),
    }
}


pub(crate) async fn partner_rebuild_wb_st_book(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let force = if body.trim().is_empty() {
        false
    } else {
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => v
                .get("force")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            Err(e) => {
                return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
            }
        }
    };
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .rebuild_world_book_st_book(&id, force)
    {
        Ok((item, entries, already_had_raw)) => Json(json!({
            "ok": true,
            "worldBookId": id,
            "worldBook": item,
            "entries": entries,
            "count": entries.len(),
            "alreadyHadRaw": already_had_raw,
            "force": force,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_rebuild_cc_st_book(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (force, create_from_content) = if body.trim().is_empty() {
        (false, false)
    } else {
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => (
                v.get("force").and_then(|x| x.as_bool()).unwrap_or(false),
                v.get("createFromContent")
                    .or_else(|| v.get("create_from_content"))
                    .or_else(|| v.get("migrateLegacy"))
                    .or_else(|| v.get("migrate_legacy"))
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
            ),
            Err(e) => {
                return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
            }
        }
    };
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .rebuild_character_card_st_book(&id, force, create_from_content)
    {
        Ok((card, wb, entries, already_had_raw)) => Json(json!({
            "ok": true,
            "characterCardId": id,
            "characterCard": card,
            "worldBook": wb,
            "worldBookId": wb.id,
            "entries": entries,
            "count": entries.len(),
            "alreadyHadRaw": already_had_raw,
            "force": force,
            "createFromContent": create_from_content,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_migrate_legacy_world_books(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let force = if body.trim().is_empty() {
        false
    } else {
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => v
                .get("force")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            Err(e) => {
                return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
            }
        }
    };
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .migrate_legacy_world_books(force)
    {
        Ok(rows) => {
            let rebuilt: Vec<Value> = rows
                .iter()
                .map(|(id, n, already)| {
                    json!({
                        "worldBookId": id,
                        "count": n,
                        "alreadyHadRaw": already,
                    })
                })
                .collect();
            let migrated = rows.iter().filter(|(_, _, a)| !*a).count();
            let skipped = rows.iter().filter(|(_, _, a)| *a).count();
            Json(json!({
                "ok": true,
                "force": force,
                "total": rows.len(),
                "migrated": migrated,
                "skippedAlreadyRaw": skipped,
                "results": rebuilt,
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}



/// Parse vector activation knobs from request JSON (W5).
pub(crate) fn vector_settings_from_value(v: Option<&Value>) -> kaleido_core::VectorActivationSettings {
    let mut s = kaleido_core::VectorActivationSettings::default();
    let Some(v) = v else { return s };
    if let Some(b) = v.get("enabled").and_then(|x| x.as_bool()) {
        s.enabled = b;
    }
    if let Some(n) = v
        .get("scoreThreshold")
        .or_else(|| v.get("score_threshold"))
        .and_then(|x| x.as_f64())
    {
        s.score_threshold = n;
    }
    if let Some(n) = v
        .get("topK")
        .or_else(|| v.get("top_k"))
        .and_then(|x| x.as_i64())
    {
        s.top_k = n as i32;
    }
    s
}

/// Recent chat text used as vector query (newest-first scan buffer join).
pub(crate) fn vector_query_text(chat_oldest_first: &[(String, String)], depth: i32) -> String {
    let depth = depth.max(1) as usize;
    let newest_first: Vec<String> = chat_oldest_first
        .iter()
        .rev()
        .take(depth)
        .map(|(_, c)| c.clone())
        .collect();
    newest_first.join("\n")
}

/// Collect vector hits for the given world-book ids against chat text.
pub(crate) fn collect_vector_hits(
    state: &AppState,
    world_book_ids: &[String],
    query_text: &str,
    settings: &kaleido_core::VectorActivationSettings,
) -> (Vec<kaleido_core::VectorHit>, Option<String>) {
    if !settings.enabled || world_book_ids.is_empty() {
        return (Vec::new(), None);
    }
    if query_text.trim().is_empty() {
        return (Vec::new(), None);
    }
    // Ensure local embed; if fail, skip vector path (keyword still works).
    if let Err(e) = embed_local::ensure_local() {
        return (Vec::new(), Some(format!("embed unavailable: {e}")));
    }
    let qv = match embed_local::embed_one(query_text) {
        Ok(v) => v,
        Err(e) => return (Vec::new(), Some(format!("embed query failed: {e}"))),
    };
    let mut lists = Vec::new();
    for id in world_book_ids {
        let idx = state.vector_index.load(id);
        if idx.entries.is_empty() {
            continue;
        }
        lists.push(kaleido_core::rank_hits(&idx, &qv, settings));
    }
    let hits = kaleido_core::merge_hit_lists(&lists, settings.top_k);
    (hits, None)
}

/// Resolve world-book ids the same way PartnerStore does for prompt build.
pub(crate) fn resolve_wb_ids_for_prompt(
    partner: &PartnerStore,
    world_book_id: Option<&str>,
    character_card_id: Option<&str>,
) -> Vec<String> {
    let Ok(st) = partner.load() else {
        return Vec::new();
    };
    let mut wb_ids: Vec<String> = Vec::new();
    let wb_id = world_book_id
        .map(|s| s.to_string())
        .or(st.selected_world_book_id.clone());
    let cc_id = character_card_id
        .map(|s| s.to_string())
        .or(st.selected_character_card_id.clone());
    if let Some(id) = wb_id {
        wb_ids.push(id);
    }
    if let Some(id) = cc_id {
        if let Some(cc) = st.character_cards.iter().find(|c| c.id == id) {
            if let Some(ref wid) = cc.world_book_id {
                if !wb_ids.iter().any(|x| x == wid) {
                    wb_ids.push(wid.clone());
                }
            }
        }
    }
    wb_ids
}

pub(crate) async fn wi_vector_index_get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    Json(state.vector_index.status(&id)).into_response()
}

pub(crate) async fn wi_vector_index_rebuild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let body_v: Value = body.map(|j| j.0).unwrap_or_else(|_| json!({}));
    let force = body_v
        .get("force")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    let st = match state.partner.clone().scoped(&sess.user_id).load() {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    let Some(wb) = st.world_books.iter().find(|w| w.id == id) else {
        return not_found("PARTNER_WB_NOT_FOUND", format!("world book not found: {id}"));
    };
    let name = if wb.name.is_empty() {
        id.clone()
    } else {
        wb.name.clone()
    };
    let entries = kaleido_core::entries_from_world_book(&name, wb.fields.as_ref(), &wb.content);
    // Index only vectorized entries (ST). If none flagged, client can pass onlyVectorized=false.
    let only_vectorized = body_v
        .get("onlyVectorized")
        .or_else(|| body_v.get("only_vectorized"))
        .and_then(|x| x.as_bool())
        .unwrap_or(true);

    let targets: Vec<kaleido_core::WiEntry> = entries
        .into_iter()
        .filter(|e| !e.disable && (!only_vectorized || e.vectorized))
        .collect();

    if targets.is_empty() {
        // Save empty index so status is honest
        let file = kaleido_core::VectorIndexFile {
            world_book_id: id.clone(),
            model: "BAAI/bge-small-zh-v1.5".into(),
            dim: 0,
            entries: vec![],
            updated_at: None,
        };
        match state.vector_index.save(file) {
            Ok(f) => {
                return Json(json!({
                    "ok": true,
                    "worldBookId": id,
                    "indexed": 0,
                    "skippedUnchanged": 0,
                    "onlyVectorized": only_vectorized,
                    "model": f.model,
                    "dim": f.dim,
                    "note": "no vectorized entries to index",
                }))
                .into_response();
            }
            Err(e) => return map_core_err(e),
        }
    }

    if let Err(e) = embed_local::ensure_local() {
        return service_unavailable("EMBED_UNAVAILABLE", format!("embed engine: {e}"));
    }

    let prev = state.vector_index.load(&id);
    let prev_map: HashMap<String, kaleido_core::VectorIndexEntry> = prev
        .entries
        .into_iter()
        .map(|e| (format!("{}.{}", e.world, e.uid), e))
        .collect();

    let mut texts: Vec<String> = Vec::new();
    let mut meta: Vec<(String, String, String, String)> = Vec::new(); // uid, world, text, hash
    let mut reused: Vec<kaleido_core::VectorIndexEntry> = Vec::new();
    let mut skipped_unchanged = 0i32;

    for e in &targets {
        let text = kaleido_core::entry_embed_text(e);
        let hash = kaleido_core::text_hash(&text);
        let key = format!("{}.{}", e.world, e.uid);
        if !force {
            if let Some(old) = prev_map.get(&key) {
                if old.text_hash == hash && !old.vector.is_empty() {
                    reused.push(old.clone());
                    skipped_unchanged += 1;
                    continue;
                }
            }
        }
        texts.push(text.clone());
        meta.push((e.uid.clone(), e.world.clone(), text, hash));
    }

    let new_vecs = if texts.is_empty() {
        Vec::new()
    } else {
        match tokio::task::spawn_blocking(move || embed_local::embed_many(&texts)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                return internal("EMBED_FAIL", format!("embed batch: {e}"));
            }
            Err(e) => {
                return internal("EMBED_FAIL", format!("embed join: {e}"));
            }
        }
    };

    if new_vecs.len() != meta.len() {
        return internal("EMBED_MISMATCH", format!("embed count mismatch: got {} want {}", new_vecs.len(), meta.len()));
    }

    let mut out_entries = reused;
    let dim = new_vecs.first().map(|v| v.len()).unwrap_or(
        out_entries.first().map(|e| e.vector.len()).unwrap_or(0),
    );
    for ((uid, world, text, hash), vec) in meta.into_iter().zip(new_vecs.into_iter()) {
        out_entries.push(kaleido_core::VectorIndexEntry {
            uid,
            world,
            text,
            text_hash: hash,
            vector: vec,
        });
    }
    // stable order
    out_entries.sort_by(|a, b| a.world.cmp(&b.world).then(a.uid.cmp(&b.uid)));

    let file = kaleido_core::VectorIndexFile {
        world_book_id: id.clone(),
        model: "BAAI/bge-small-zh-v1.5".into(),
        dim,
        entries: out_entries,
        updated_at: None,
    };
    match state.vector_index.save(file) {
        Ok(f) => Json(json!({
            "ok": true,
            "worldBookId": id,
            "indexed": f.entries.len(),
            "embeddedNow": f.entries.len() as i32 - skipped_unchanged,
            "skippedUnchanged": skipped_unchanged,
            "onlyVectorized": only_vectorized,
            "force": force,
            "model": f.model,
            "dim": f.dim,
            "updatedAt": f.updated_at,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}


pub(crate) async fn automation_triggers_list(
    State(state): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 200);
    let store = kaleido_core::AutomationTriggerStore::new(state.auth.data_root());
    let events = store.recent(limit);
    let log = store.load();
    Json(json!({
        "ok": true,
        "count": events.len(),
        "cap": log.cap,
        "events": events,
    }))
    .into_response()
}

pub(crate) async fn automation_triggers_clear(State(state): State<AppState>) -> Response {
    let store = kaleido_core::AutomationTriggerStore::new(state.auth.data_root());
    match store.clear() {
        Ok(()) => Json(json!({"ok": true, "cleared": true})).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// W4: token estimate API — no tiktoken dep; heuristic | cl100k_approx.
pub(crate) async fn tokenize_estimate(State(state): State<AppState>, body: String) -> Response {
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    // Resolve mode: body.mode → body.tokenEstimateMode → settings → heuristic
    let mut mode_s = v
        .get("mode")
        .or_else(|| v.get("tokenEstimateMode"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if mode_s.trim().is_empty() {
        if let Ok(ps) = state.app_state.load_settings_public() {
            mode_s = ps.token_estimate_mode;
        }
    }
    let mode = kaleido_core::TokenEstimateMode::parse(&mode_s);
    let with_breakdown = v
        .get("breakdown")
        .or_else(|| v.get("withBreakdown"))
        .and_then(|x| x.as_bool())
        .unwrap_or(true);

    // Single text or texts[]
    let mut texts: Vec<String> = Vec::new();
    if let Some(arr) = v.get("texts").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                texts.push(s.to_string());
            } else if let Some(obj) = item.as_object() {
                if let Some(s) = obj.get("text").or_else(|| obj.get("content")).and_then(|x| x.as_str()) {
                    texts.push(s.to_string());
                }
            }
        }
    }
    if texts.is_empty() {
        if let Some(s) = v
            .get("text")
            .or_else(|| v.get("content"))
            .or_else(|| v.get("prompt"))
            .and_then(|x| x.as_str())
        {
            texts.push(s.to_string());
        }
    }
    if texts.is_empty() {
        return bad_request("BAD_REQUEST", "text or texts[] required");
    }

    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let (items, total) = kaleido_core::estimate_many(&refs, mode);
    // optionally strip breakdown
    let items_out: Vec<_> = items
        .into_iter()
        .map(|mut e| {
            if !with_breakdown {
                e.breakdown = None;
            }
            e
        })
        .collect();
    Json(json!({
        "ok": true,
        "mode": mode.as_str(),
        "count": items_out.len(),
        "totalTokens": total,
        "items": items_out,
        // convenience for single-text callers
        "tokens": total,
        "method": items_out.first().map(|e| e.method.clone()).unwrap_or_default(),
    }))
    .into_response()
}

pub(crate) async fn wi_vector_query(State(state): State<AppState>, body: String) -> Response {
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    let wb_id = v
        .get("worldBookId")
        .or_else(|| v.get("world_book_id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if wb_id.is_empty() {
        return bad_request("BAD_REQUEST", "worldBookId required");
    }
    let query = v
        .get("query")
        .or_else(|| v.get("text"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if query.trim().is_empty() {
        return bad_request("BAD_REQUEST", "query required");
    }
    let settings = vector_settings_from_value(
        v.get("vectorSettings")
            .or_else(|| v.get("settings"))
            .or(Some(&v)),
    );
    if let Err(e) = embed_local::ensure_local() {
        return service_unavailable("EMBED_UNAVAILABLE", format!("embed engine: {e}"));
    }
    let qv = match tokio::task::spawn_blocking(move || embed_local::embed_one(&query)).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            return internal("EMBED_FAIL", format!("embed: {e}"));
        }
        Err(e) => {
            return internal("EMBED_FAIL", format!("embed join: {e}"));
        }
    };
    let idx = state.vector_index.load(&wb_id);
    let hits = kaleido_core::rank_hits(&idx, &qv, &settings);
    Json(json!({
        "ok": true,
        "worldBookId": wb_id,
        "entryCount": idx.entries.len(),
        "hitCount": hits.len(),
        "scoreThreshold": settings.score_threshold,
        "topK": settings.top_k,
        "hits": hits,
        "embed": embed_local::status(),
    }))
    .into_response()
}


pub(crate) async fn regex_library_get(State(state): State<AppState>) -> Response {
    Json(state.regex_library.to_public()).into_response()
}

pub(crate) async fn regex_library_put(State(state): State<AppState>, body: String) -> Response {
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    let scripts = kaleido_core::scripts_from_import_body(
        v.get("scripts")
            .or_else(|| v.get("regexScripts"))
            .or_else(|| v.get("regex_scripts"))
            .unwrap_or(&v),
    );
    // if body is {scripts, priority} use that; scripts_from_import_body already handles
    let scripts = if v.get("scripts").is_some()
        || v.get("regexScripts").is_some()
        || v.get("regex_scripts").is_some()
    {
        kaleido_core::scripts_from_import_body(&v)
    } else if v.as_array().is_some() {
        kaleido_core::scripts_from_import_body(&v)
    } else if v.get("findRegex").is_some() || v.get("find_regex").is_some() {
        kaleido_core::scripts_from_import_body(&v)
    } else {
        scripts
    };
    let priority = v
        .get("priority")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    match state.regex_library.put_scripts(scripts, priority) {
        Ok(file) => Json(json!({
            "ok": true,
            "priority": file.priority,
            "scripts": file.scripts,
            "count": file.scripts.len(),
            "updatedAt": file.updated_at,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn regex_library_import(State(state): State<AppState>, body: String) -> Response {
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    let replace = v
        .get("replace")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let priority = v
        .get("priority")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let scripts = kaleido_core::scripts_from_import_body(&v);
    if scripts.is_empty() {
        return bad_request("BAD_REGEX_IMPORT", "no parseable regex scripts in body (need scripts[] or ST script object)");
    }
    match state
        .regex_library
        .import_scripts(scripts, replace, priority)
    {
        Ok(file) => Json(json!({
            "ok": true,
            "replaced": replace,
            "priority": file.priority,
            "scripts": file.scripts,
            "count": file.scripts.len(),
            "updatedAt": file.updated_at,
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn partner_select(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PartnerSelectBody>,
) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state
        .partner
        .clone()
        .scoped(&sess.user_id)
        .select(body.world_book_id, body.character_card_id)
    {
        Ok(st) => Json(st).into_response(),
        Err(e) => map_core_err(e),
    }
}

pub(crate) fn fill_settings_gaps(state: &AppState, s: &mut kaleido_core::PublicSettings) {
    // Fill gaps from env defaults so UI shows effective config.
    let rt = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    if s.llm_base_url.is_empty() {
        s.llm_base_url = rt.base_url.clone();
    }
    s.llm_base_url_configured = !s.llm_base_url.trim().is_empty();
    if s.llm_model.is_empty() {
        s.llm_model = if rt.model.is_empty() {
            state.llm_model.clone()
        } else {
            rt.model.clone()
        };
    }
    if !s.llm_api_key_configured {
        // env key present but no secrets file yet
        s.llm_api_key_configured = state
            .llm_key
            .as_ref()
            .map(|k| !k.is_empty())
            .unwrap_or(false);
        if s.llm_api_key_configured && s.llm_api_key.is_empty() {
            s.llm_api_key = "[server]".into();
        }
    }
    // W4: default token estimate mode for settings GET / patch echo
    if s.token_estimate_mode.trim().is_empty() {
        s.token_estimate_mode = "heuristic".into();
    } else {
        s.token_estimate_mode = kaleido_core::TokenEstimateMode::parse(&s.token_estimate_mode)
            .as_str()
            .to_string();
    }
    // W12: session cap — fill from live AuthStore if settings unset
    if s.session_max.is_none() {
        s.session_max = Some(state.auth.max_sessions() as u64);
    }
    if s.session_cap_policy.trim().is_empty() {
        s.session_cap_policy = state.auth.session_cap_policy();
    } else {
        s.session_cap_policy = kaleido_core::AuthStore::normalize_policy(&s.session_cap_policy);
    }
}

pub(crate) async fn settings_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.app_state.load_settings_public() {
        Ok(mut s) => {
            fill_settings_gaps(&state, &mut s);
            Json(s).into_response()
        }
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn settings_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let patch: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return bad_request("BAD_JSON", format!("Invalid JSON: {e}"));
        }
    };
    // Reject empty body {} soft-success that looks like "saved" but wrote nothing useful
    // when client meant to send fields — still allow intentional partial patches.
    match state.app_state.patch_settings_public(&patch) {
        Ok(mut s) => {
            // W12: hot-apply session cap to live AuthStore
            let cap = patch
                .get("sessionMax")
                .or_else(|| patch.get("maxSessions"))
                .and_then(|x| x.as_u64())
                .map(|n| n as usize);
            let pol = patch
                .get("sessionCapPolicy")
                .and_then(|x| x.as_str());
            if cap.is_some() || pol.is_some() {
                state.auth.apply_session_cap_config(cap, pol);
            }
            fill_settings_gaps(&state, &mut s);
            // Echo back what we persisted so the web UI can re-bind without a second GET race.
            Json(json!({
                "ok": true,
                "saved": true,
                "llmModel": s.llm_model,
                "modelInterface": s.model_interface,
                "partnerChatPrompt": s.partner_chat_prompt,
                "temperature": s.temperature,
                "maxOutputTokens": s.max_output_tokens,
                "topP": s.top_p,
                "frequencyPenalty": s.frequency_penalty,
                "presencePenalty": s.presence_penalty,
                "llmApiKey": s.llm_api_key,
                "llmApiKeyConfigured": s.llm_api_key_configured,
                "llmBaseUrl": s.llm_base_url,
                "llmBaseUrlConfigured": s.llm_base_url_configured,
                "crawlerEnabled": s.crawler_enabled,
                "bashSandboxEnabled": s.bash_sandbox_enabled,
                "agentToolsEnabled": s.agent_tools_enabled,
                "agentWriteEnabled": s.agent_write_enabled,
                "agentConfirmDangerous": s.agent_confirm_dangerous,
                "tokenEstimateMode": if s.token_estimate_mode.trim().is_empty() {
                    "heuristic".to_string()
                } else {
                    s.token_estimate_mode.clone()
                },
                "sessionMax": s.session_max.unwrap_or(state.auth.max_sessions() as u64),
                "sessionCapPolicy": if s.session_cap_policy.trim().is_empty() {
                    state.auth.session_cap_policy()
                } else {
                    s.session_cap_policy.clone()
                },
                "tavernAdultOk": s.tavern_adult_ok,
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

pub(crate) async fn prompt_preview(State(state): State<AppState>, headers: HeaderMap, Query(params): Query<HashMap<String, String>>) -> Response {
    // C2: scoped to the authenticated user's own partner store.
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let base = state.app_state.partner_chat_prompt();
    let wb = params.get("worldBookId").map(|s| s.as_str());
    let cc = params.get("characterCardId").map(|s| s.as_str());
    match state.partner.clone().scoped(&sess.user_id).build_system_prompt(&base, wb, cc) {
        Ok(prompt) => Json(json!({
            "systemPrompt": prompt,
            "length": prompt.len(),
        }))
        .into_response(),
        Err(e) => map_core_err(e),
    }
}
