//! SillyTavern character card import → partner characterCards (+ embedded world book / regex).
//!
//! Route (wired in `main.rs`):
//! - `POST /api/v1/partner/st-import`
//!
//! Body forms:
//! - raw ST JSON (v2/v3 or legacy)
//! - `{ "card": {...}, "worldBookId": "wb-..." }`
//! - `{ "json": "<stringified card>", "worldBookId": "..." }`
//! - `{ "pngBase64": "<base64 png with tEXt chara>", "worldBookId": "..." }`
//! - raw PNG bytes (Content-Type: image/png)
//!
//! Side effects when the card embeds `data.character_book`:
//! - auto-create a partner world_book from enabled lore entries
//! - link the new character card to that world book (unless worldBookId was provided)
//!
//! `data.extensions.regex_scripts` are stored on the card fields as `stRegexScripts`
//! for client-side bubble transforms (Kaleido has no full ST regex engine server-side).

use axum::{
    extract::State,
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    Json,
};
use kaleido_core::{
    base64_to_png, build_st_import_bundle, extract_st_card_from_jpeg, extract_st_card_from_png,
    extract_st_card_from_webp, import_st_character_card_bundle,
    parse_st_character_card_value, PartnerItem, StImportBundle, StImportError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StImportWrapper {
    #[serde(default)]
    card: Option<Value>,
    #[serde(default)]
    json: Option<String>,
    /// base64 PNG (optional data:image/png;base64, prefix) with tEXt chara
    #[serde(default, alias = "png")]
    png_base64: Option<String>,
    #[serde(default, alias = "world_book_id")]
    world_book_id: Option<String>,
    /// When true (default), auto-import embedded character_book as a world book.
    #[serde(default = "default_true")]
    import_character_book: Option<bool>,
}

fn default_true() -> Option<bool> {
    Some(true)
}

fn st_err(e: StImportError) -> Response {
    bad_request("STI_BAD_REQUEST", e.to_string())
}

fn resolve_world_book_id(w: &StImportWrapper) -> Option<String> {
    w.world_book_id
        .clone()
        .filter(|s| !s.trim().is_empty())
}

fn want_import_book(w: &StImportWrapper) -> bool {
    w.import_character_book.unwrap_or(true)
}

fn bundle_from_json_body(body: &str) -> Result<(StImportBundle, bool, &'static str), StImportError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(StImportError("empty body".into()));
    }

    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| StImportError(format!("invalid JSON: {e}")))?;

    if let Ok(wrapper) = serde_json::from_value::<StImportWrapper>(value.clone()) {
        let wb = resolve_world_book_id(&wrapper);
        let import_book = want_import_book(&wrapper);
        if let Some(png_b64) = wrapper.png_base64 {
            let bytes = base64_to_png(&png_b64)?;
            let card = extract_st_card_from_png(&bytes)?;
            let mut bundle = build_st_import_bundle(card, wb);
            if !import_book {
                bundle.world_book = None;
            }
            return Ok((bundle, import_book, "png_tEXt"));
        }
        if let Some(card_val) = wrapper.card {
            let card = parse_st_character_card_value(&card_val)?;
            let mut bundle = build_st_import_bundle(card, wb);
            if !import_book {
                bundle.world_book = None;
            }
            return Ok((bundle, import_book, "json"));
        }
        if let Some(raw) = wrapper.json {
            let mut bundle = import_st_character_card_bundle(&raw, wb)?;
            if !import_book {
                bundle.world_book = None;
            }
            return Ok((bundle, import_book, "json"));
        }
    }

    let is_st = value
        .get("spec")
        .and_then(|s| s.as_str())
        .map(|s| s.starts_with("chara_card"))
        .unwrap_or(false)
        || value.get("data").is_some()
        || value.get("name").is_some()
        || value.get("char_name").is_some(); // V1 flat (吞噬 X6)

    if is_st {
        let wb = value
            .get("worldBookId")
            .or_else(|| value.get("world_book_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let import_book = value
            .get("importCharacterBook")
            .or_else(|| value.get("import_character_book"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let card = parse_st_character_card_value(&value)?;
        let mut bundle = build_st_import_bundle(card, wb);
        if !import_book {
            bundle.world_book = None;
        }
        return Ok((bundle, import_book, "json"));
    }

    Err(StImportError(
        "expected ST character card JSON, { card|json|pngBase64, worldBookId }, or raw PNG".into(),
    ))
}

fn persist_bundle(
    partner: &kaleido_core::PartnerStore,
    mut bundle: StImportBundle,
    external_wb: bool,
) -> Result<(PartnerItem, Option<PartnerItem>), kaleido_core::CoreError> {
    let mut saved_wb = None;
    // If caller did not pass worldBookId, create embedded book first and link.
    if bundle.character.world_book_id.is_none() {
        if let Some(wb_item) = bundle.world_book.take() {
            let saved = partner.upsert_world_book(wb_item)?;
            bundle.character.world_book_id = Some(saved.id.clone());
            saved_wb = Some(saved);
        }
    } else if external_wb {
        // still optionally materialize embedded book as extra world book (linked only via fields note)
        if let Some(mut wb_item) = bundle.world_book.take() {
            // name distinguish
            if !wb_item.name.contains("(embedded)") {
                wb_item.name = format!("{} (embedded)", wb_item.name);
            }
            let saved = partner.upsert_world_book(wb_item)?;
            saved_wb = Some(saved);
        }
    } else if let Some(wb_item) = bundle.world_book.take() {
        let saved = partner.upsert_world_book(wb_item)?;
        // keep character linked to external id; return embedded as side product
        saved_wb = Some(saved);
    }

    let saved_cc = partner.upsert_character_card(bundle.character)?;
    Ok((saved_cc, saved_wb))
}

/// `POST /api/v1/partner/st-import`
pub async fn import_st(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // C2 审计修复：per-user 隔离。
    let partner = state.partner.clone().scoped(&sess.user_id);

    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let (bundle, _import_book, source) = if ct.contains("image/png")
        || body.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        match extract_st_card_from_png(&body) {
            Ok(card) => (build_st_import_bundle(card, None), true, "png_tEXt"),
            Err(e) => return st_err(e),
        }
    } else if ct.contains("image/webp") || (body.len() >= 12 && &body[0..4] == b"RIFF" && &body[8..12] == b"WEBP")
    {
        // X6: WEBP RIFF container (EXIF/XMP chunks)
        match extract_st_card_from_webp(&body) {
            Ok(card) => (build_st_import_bundle(card, None), true, "webp_exif/xmp"),
            Err(e) => return st_err(e),
        }
    } else if ct.contains("image/jpeg") || ct.contains("image/jpg") || body.starts_with(b"\xFF\xD8")
    {
        // X6: JPEG APP1 EXIF/XMP segment
        match extract_st_card_from_jpeg(&body) {
            Ok(card) => (build_st_import_bundle(card, None), true, "jpeg_app1"),
            Err(e) => return st_err(e),
        }
    } else {
        let text = String::from_utf8_lossy(&body);
        match bundle_from_json_body(&text) {
            Ok(x) => x,
            Err(e) => return st_err(e),
        }
    };

    let lore_n = bundle.lore_entry_count;
    let regex_n = bundle.regex_count;
    let has_book = bundle.world_book.is_some() || bundle.card.character_book.is_some();
    let external_wb = bundle.character.world_book_id.is_some();

    match persist_bundle(&partner, bundle, external_wb) {
        Ok((saved_cc, saved_wb)) => {
            let note = if has_book || regex_n > 0 {
                format!(
                    "imported character card; lore_entries={lore_n}; regex_scripts={regex_n}; world_book={}",
                    saved_wb
                        .as_ref()
                        .map(|w| w.id.as_str())
                        .or(saved_cc.world_book_id.as_deref())
                        .unwrap_or("-")
                )
            } else {
                "JSON v2/v3/legacy + PNG tEXt chara/ccv3; no embedded character_book/regex".into()
            };
            Json(json!({
                "ok": true,
                "item": saved_cc,
                "worldBook": saved_wb,
                "source": source,
                "loreEntryCount": lore_n,
                "regexScriptCount": regex_n,
                "note": note,
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_raw_v2_body() {
        let body = r#"{
          "spec": "chara_card_v2",
          "spec_version": "2.0",
          "data": { "name": "Test", "personality": "x" }
        }"#;
        let (bundle, _, src) = bundle_from_json_body(body).unwrap();
        assert!(bundle.character.world_book_id.is_none());
        assert_eq!(bundle.character.name, "Test");
        assert_eq!(src, "json");
        assert!(bundle.world_book.is_none());
    }

    #[test]
    fn parse_wrapper_body() {
        let body = r#"{
          "worldBookId": "wb-1",
          "card": {
            "spec": "chara_card_v2",
            "data": { "name": "Wrapped", "description": "d" }
          }
        }"#;
        let (bundle, _, _) = bundle_from_json_body(body).unwrap();
        assert_eq!(bundle.character.world_book_id.as_deref(), Some("wb-1"));
        assert_eq!(bundle.character.name, "Wrapped");
    }

    #[test]
    fn parse_with_character_book() {
        let body = r#"{
          "spec": "chara_card_v2",
          "data": {
            "name": "WithBook",
            "character_book": {
              "name": "B",
              "entries": [{"keys":["a"],"content":"alpha lore","enabled":true}]
            },
            "extensions": {"regex_scripts":[{"scriptName":"r","findRegex":"/x/","replaceString":"y"}]}
          }
        }"#;
        let (bundle, import_book, _) = bundle_from_json_body(body).unwrap();
        assert!(import_book);
        assert!(bundle.world_book.is_some());
        assert_eq!(bundle.lore_entry_count, 1);
        assert_eq!(bundle.regex_count, 1);
        assert!(bundle
            .world_book
            .as_ref()
            .unwrap()
            .content
            .contains("alpha lore"));
    }

    // ─── P9: 路由存在性守卫（防再断链）─────────────────────────────────
    // P0-1c 拆分（b6f5304）曾把本模组的两条路由从 main.rs 删掉而未挂到
    // routes_partner::router()，前端调用 404 达两天（P7 才发现修复）。
    // 以下测试直接打 router()，若有人再次移除挂载即红。

    #[cfg(test)]
    mod route_guards {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
            Router,
        };
        use tower::util::ServiceExt; // oneshot

        fn test_router() -> Router {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("kaleido-stguard-{nanos}"));
            std::fs::create_dir_all(&root).unwrap();
            // AuthStore 引导需要显式管理员密码（拒绝默认密码），测试内注入一次性值。
            std::env::set_var("KALEIDO_ADMIN_PASSWORD", "route-guard-test-pw");
            let data = kaleido_core::DataRoot::new(root.join("d")).unwrap();
            let app_state = kaleido_core::AppStateStore::new(data.clone());
            let state = crate::state::AppState {
                auth: kaleido_core::AuthStore::load(data.clone()).unwrap(),
                jobs: kaleido_core::JobStore::with_max_concurrent(data.clone(), 2),
                sessions: kaleido_core::AgentSessionStore::new(data.clone()),
                app_state: app_state.clone(),
                partner: kaleido_core::PartnerStore::new(app_state.clone()),
                works: kaleido_core::WorksFs::new(data.clone()),
                packs: kaleido_core::PackStore::new(data.clone()),
                search: kaleido_core::hybrid_search::SearchIndex::new(data.clone()).unwrap(),
                sessions_tavern: kaleido_core::TavernSessionStore::new(data.clone()),
                personas: kaleido_core::TavernPersonaStore::new(data.clone()),
                regex_library: kaleido_core::RegexLibraryStore::new(&data),
                vector_index: kaleido_core::VectorIndexStore::new(&data),
                reviews: kaleido_core::ReviewStore::new(data.clone()),
                hub: std::sync::Arc::new(crate::state::StreamHub::new()),
                llm_base: None,
                llm_key: None,
                llm_model: "test-model".into(),
                provider_kind: "OpenAI".into(),
                embedding_base: None,
                image_base_url: None,
                image_api_key: None,
                image_model: "t".into(),
                cf_image_base_url: None,
                cf_image_model: None,
                grok2api_image_base_url: None,
                grok2api_image_key: None,
                grok2api_image_model: None,
                plugin_registry: std::sync::Arc::new(kaleido_core::plugin::PluginRegistry::new()),
                world_state: Default::default(),
                weaver_config: Default::default(),
                graph: kaleido_core::graph_store::GraphStore::open_in_memory().unwrap(),
                foreshadow: kaleido_core::foreshadow_store::ForeshadowStore::open_in_memory().unwrap(),
                analysis: kaleido_core::analysis_store::AnalysisStore::open_in_memory().unwrap(),
                ai_admin: kaleido_core::ai_admin_store::AiAdminStore::open_in_memory().unwrap(),
                scene_cards: kaleido_core::scene_card_store::SceneCardStore::open_in_memory().unwrap(),
                reference_library: crate::reference_library::ReferenceLibraryStore::new(data.root()),
                rpm: crate::ai_admin::RpmLimiter::new(),
                director_tasks: std::sync::Arc::new(kaleido_core::DirectorTaskGroup::new()),
            };
            crate::routes_partner::router().with_state(state)
        }

        async fn code_of(req: Request<Body>) -> StatusCode {
            test_router().oneshot(req).await.unwrap().status()
        }

        fn card_body() -> String {
            r#"{"spec":"chara_card_v2","spec_version":"2.0",
                "data":{"name":"RouteGuard","description":"d"}}"#.into()
        }

        /// st-import 路由必须存在（404 = 挂载丢失，P7 回归守卫）
        #[tokio::test]
        async fn st_import_route_is_mounted() {
            let code = code_of(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/partner/st-import")
                    .header("content-type", "application/json")
                    .body(Body::from(card_body()))
                    .unwrap(),
            )
            .await;
            assert_ne!(code, StatusCode::NOT_FOUND, "st-import 路由丢失（P0-1c 回归复发）");
        }

        /// wi-preview 路由必须存在（同上）
        #[tokio::test]
        async fn wi_preview_route_is_mounted() {
            let code = code_of(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/partner/wi-preview")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"messages":[]}"#))
                    .unwrap(),
            )
            .await;
            assert_ne!(code, StatusCode::NOT_FOUND, "wi-preview 路由丢失");
        }

        /// 无认证时应返回 401 而不是 404/500（证明 handler 真正接到了请求）
        #[tokio::test]
        async fn st_import_requires_auth() {
            let code = code_of(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/partner/st-import")
                    .header("content-type", "application/json")
                    .body(Body::from(card_body()))
                    .unwrap(),
            )
            .await;
            assert_eq!(code, StatusCode::UNAUTHORIZED);
        }
    }
}

/// `POST /api/v1/partner/wi-preview` — dry-run ST World Info scan + prompt assembly.
/// Body: `{ worldBookId?, characterCardId?, messages?: [{role,content}], worldInfoSettings? }`
pub async fn wi_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // C2 审计修复：per-user 隔离。
    let partner = state.partner.clone().scoped(&sess.user_id);
    let wb = body
        .get("worldBookId")
        .or_else(|| body.get("world_book_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let cc = body
        .get("characterCardId")
        .or_else(|| body.get("character_card_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let mut chat_pairs: Vec<(String, String)> = Vec::new();
    if let Some(arr) = body.get("messages").and_then(|v| v.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !content.is_empty() {
                chat_pairs.push((role.to_string(), content.to_string()));
            }
        }
    }
    let wi_settings = body
        .get("worldInfoSettings")
        .cloned()
        .and_then(|v| serde_json::from_value::<kaleido_core::WiSettings>(v).ok());
    let base = body
        .get("basePrompt")
        .and_then(|v| v.as_str())
        .unwrap_or("You are a test harness.");
    let chat_id = body
        .get("sessionId")
        .or_else(|| body.get("chatId"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let dry = body
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let timed_store = kaleido_core::TimedWorldInfoStore::new(state.auth.data_root());
    let timed_in = if chat_id.is_empty() {
        None
    } else {
        Some(timed_store.load(&chat_id))
    };
    let mut scan_ctx = body
        .get("worldInfoScanContext")
        .cloned()
        .and_then(|v| serde_json::from_value::<kaleido_core::WiScanContext>(v).ok())
        .unwrap_or_default();
    if scan_ctx.trigger.is_empty() {
        scan_ctx.trigger = body
            .get("trigger")
            .and_then(|v| v.as_str())
            .unwrap_or("normal")
            .to_string();
    }
    let max_ctx = body
        .get("maxContextTokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(8192) as i32;
    // W5: inject vector hits for vectorized entries (if index present)
    {
        let vset = crate::vector_settings_from_value(
            body.get("vectorSettings")
                .or_else(|| body.get("worldInfoVectorSettings")),
        );
        let depth = body
            .get("worldInfoSettings")
            .and_then(|w| w.get("depth"))
            .and_then(|d| d.as_i64())
            .unwrap_or(2) as i32;
        let qtext = crate::vector_query_text(&chat_pairs, depth);
        let wb_ids = crate::resolve_wb_ids_for_prompt(&partner, wb, cc);
        let (hits, _verr) = crate::collect_vector_hits(&state, &wb_ids, &qtext, &vset);
        scan_ctx.vector_hits = hits;
        scan_ctx.vector_settings = Some(vset);
    }
    match partner.build_generation_prompt_full(
        base, wb, cc, &chat_pairs, wi_settings, timed_in, dry, Some(scan_ctx), max_ctx,
    ) {
        Ok(r) => {
            let mut auto_recorded = 0usize;
            if !dry {
                if let Some(ref tw) = r.timed_world_info {
                    if !chat_id.is_empty() {
                        let _ = timed_store.save(&chat_id, tw);
                    }
                }
                // W7: record automation triggers on non-dry preview
                if !r.automation_ids.is_empty() {
                    let detailed: Vec<(String, String, String, String, String)> = r
                        .wi
                        .activated
                        .iter()
                        .filter(|a| !a.automation_id.trim().is_empty())
                        .map(|a| {
                            (
                                a.automation_id.clone(),
                                a.uid.clone(),
                                a.world.clone(),
                                a.comment.clone(),
                                a.reason.clone(),
                            )
                        })
                        .collect();
                    let auto_store =
                        kaleido_core::AutomationTriggerStore::new(state.auth.data_root());
                    auto_recorded = if !detailed.is_empty() {
                        auto_store
                            .record_detailed(&detailed, &chat_id, "wi_preview")
                            .unwrap_or(0)
                    } else {
                        auto_store
                            .record(&r.automation_ids, &[], &chat_id, "wi_preview")
                            .unwrap_or(0)
                    };
                }
            }
            Json(json!({
                "ok": true,
                "systemPrompt": r.system_prompt,
                "wiActivated": r.wi.activated.len(),
                "wiBudgetTokens": r.wi.budget_tokens,
                "tokenEstimateMode": if r.wi.token_estimate_mode.is_empty() {
                    "heuristic".to_string()
                } else {
                    r.wi.token_estimate_mode.clone()
                },
                "wiOverflowed": r.wi.overflowed,
                "worldInfoBefore": r.wi.world_info_before,
                "worldInfoAfter": r.wi.world_info_after,
                "anBefore": r.wi.an_before,
                "anAfter": r.wi.an_after,
                "emBefore": r.wi.em_before,
                "emAfter": r.wi.em_after,
                "outletEntries": r.wi.outlet_entries,
                "depthEntries": r.wi.depth_entries,
                "promptSlots": r.wi.prompt_slots,
                "messageInjections": r.message_injections,
                "skippedVectorized": r.wi.skipped_vectorized,
                "vectorActivated": r.wi.vector_activated,
                "skippedFilter": r.wi.skipped_filter,
                "skippedTrigger": r.wi.skipped_trigger,
                "automationIds": r.automation_ids,
                "exampleMessages": r.example_messages,
                "regexScriptCount": r.regex_script_count,
                "activated": r.wi.activated,
                "timedWorldInfo": r.timed_world_info,
                "dryRun": dry,
                "automationRecorded": auto_recorded,
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}
