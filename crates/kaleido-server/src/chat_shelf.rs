//! Chat / Story Tavern session → bookshelf work (manual + optional schedule).
//!
//! Routes:
//! - `POST /api/v1/crawler/chat-to-shelf` — one-shot publish
//! - `GET|PUT /api/v1/crawler/chat-to-shelf/schedule` — optional timer config
//! - `POST /api/v1/crawler/chat-to-shelf/run-due` — process due sessions (also called by bg tick)

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
    routing::{get, post},
};
use chrono::{Duration as ChronoDuration, Utc};
use kaleido_core::TavernSession;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{crawler, session_from, AppState};

const STATE_NAME: &str = "chat-shelf-schedule";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/crawler/chat-to-shelf",
            post(chat_to_shelf),
        )
        .route(
            "/api/v1/crawler/chat-to-shelf/schedule",
            get(get_schedule).put(put_schedule),
        )
        .route(
            "/api/v1/crawler/chat-to-shelf/run-due",
            post(run_due),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToShelfBody {
    /// `tavern` (default) | `text` (paste raw transcript)
    #[serde(default)]
    pub source: Option<String>,
    /// Tavern session id when source=tavern
    #[serde(default)]
    pub session_id: Option<String>,
    /// Raw transcript when source=text
    #[serde(default)]
    pub text: Option<String>,
    /// Override title
    #[serde(default)]
    pub title: Option<String>,
    /// Also create/update Story Pack (default true)
    #[serde(default)]
    pub to_pack: Option<bool>,
    /// Force re-publish even if fingerprint unchanged
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Hours between auto runs per session (min 1).
    #[serde(default = "default_interval")]
    pub interval_hours: u64,
    /// Minimum tavern turns before auto-publish.
    #[serde(default = "default_min_turns")]
    pub min_turns: u64,
    #[serde(default = "default_true")]
    pub to_pack: bool,
    /// Currently only `tavern` is auto-scanned.
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_result: Option<Value>,
}

fn default_interval() -> u64 {
    24
}
fn default_min_turns() -> u64 {
    3
}
fn default_true() -> bool {
    true
}
fn default_source() -> String {
    "tavern".into()
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_hours: 24,
            min_turns: 3,
            to_pack: true,
            source: "tavern".into(),
            last_run_at: None,
            last_result: None,
        }
    }
}

fn load_schedule(state: &AppState) -> ScheduleConfig {
    let raw = state
        .app_state
        .load(STATE_NAME)
        .unwrap_or_else(|_| "{}".into());
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_schedule(state: &AppState, cfg: &ScheduleConfig) -> Result<(), String> {
    let s = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    state
        .app_state
        .save(STATE_NAME, &s)
        .map_err(|e| e.to_string())
}

fn load_published_map(state: &AppState) -> serde_json::Map<String, Value> {
    let path = kaleido_data_config_path(state, "chat-shelf-published.json");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(Value::Object(m)) = serde_json::from_str(&s) {
            return m;
        }
    }
    serde_json::Map::new()
}

fn save_published_map(state: &AppState, map: &serde_json::Map<String, Value>) {
    let path = kaleido_data_config_path(state, "chat-shelf-published.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(map.clone())).unwrap_or_else(|_| "{}".into()),
    );
}

fn kaleido_data_config_path(_state: &AppState, file: &str) -> std::path::PathBuf {
    // AppStateStore paths live under data/Kaleido/config — mirror that
    // (legacy data/MuseAI/config fallback via brand_dir). Env: KALEIDO_DATA.
    let root = crate::config::ServerConfig::data_root();
    kaleido_core::brand_dir(&root, "config").join(file)
}

fn role_label(role: &str) -> &'static str {
    match role {
        "user" => "你",
        "assistant" => "叙事",
        "narrator" => "旁白",
        "system" => "系统",
        _ => "角色",
    }
}

/// Build markdown work from tavern session messages.
pub(crate) fn tavern_session_to_markdown(sess: &TavernSession) -> (String, String) {
    let title = {
        let t = sess.title.trim();
        if t.is_empty() {
            format!("故事馆会话 {}", &sess.session_id.chars().take(8).collect::<String>())
        } else {
            t.to_string()
        }
    };
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# {title}"));
    lines.push(String::new());
    lines.push(format!(
        "> 来源：故事馆 · {} · {} · turn {} · {}",
        sess.playable_label(),
        sess.play_mode_label(),
        sess.turn,
        sess.session_id
    ));
    lines.push(String::new());

    // Group into pseudo-chapters every ~8 messages or by turn markers
    let msgs: Vec<_> = sess
        .messages
        .iter()
        .filter(|m| {
            let r = m.role.as_str();
            r == "user" || r == "assistant" || r == "narrator"
        })
        .collect();

    if msgs.is_empty() {
        lines.push("（尚无对话内容）".into());
        return (title, lines.join("\n"));
    }

    let chunk = 10usize;
    let mut chapter_i = 1;
    for (idx, chunk_msgs) in msgs.chunks(chunk).enumerate() {
        let head = chunk_msgs
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.chars().take(24).collect::<String>())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("片段 {}", idx + 1));
        lines.push(format!("第{chapter_i}章 {head}"));
        lines.push(String::new());
        for m in chunk_msgs {
            let body = m.content.trim();
            if body.is_empty() {
                continue;
            }
            // strip option protocol leftovers
            let body = body
                .lines()
                .filter(|l| !l.trim_start().starts_with("【选项】"))
                .collect::<Vec<_>>()
                .join("\n");
            if body.trim().is_empty() {
                continue;
            }
            lines.push(format!("**{}：**", role_label(&m.role)));
            lines.push(body);
            lines.push(String::new());
        }
        chapter_i += 1;
    }
    (title, lines.join("\n"))
}

// helpers on session without adding trait noise
trait SessLabel {
    fn playable_label(&self) -> String;
    fn play_mode_label(&self) -> String;
}
impl SessLabel for TavernSession {
    fn playable_label(&self) -> String {
        serde_json::to_value(&self.playable)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "P?".into())
    }
    fn play_mode_label(&self) -> String {
        self.play_mode.as_str().to_string()
    }
}

fn fingerprint(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    format!("{:x}", h.finish())
}

struct PublishOutcome {
    slug: String,
    title: String,
    chapter_count: usize,
    pack_id: Option<String>,
    path: String,
    skipped: bool,
    reason: Option<String>,
}

fn publish_markdown(
    state: &AppState,
    title: &str,
    markdown: &str,
    to_pack: bool,
    source_key: &str,
    force: bool,
) -> Result<PublishOutcome, String> {
    let mut published = load_published_map(state);
    let fp = fingerprint(markdown);
    if !force {
        if let Some(prev) = published.get(source_key).and_then(|v| v.as_str()) {
            if prev == fp {
                // still ensure shelf file exists
                return Ok(PublishOutcome {
                    slug: crawler::shelf_slug(title),
                    title: title.to_string(),
                    chapter_count: 0,
                    pack_id: crawler::find_existing_pack_for_shelf(
                        &state.packs,
                        &crawler::shelf_slug(title),
                        title,
                    ),
                    path: String::new(),
                    skipped: true,
                    reason: Some("unchanged".into()),
                });
            }
        }
    }

    let chapters = crawler::split_novel_chapters(markdown);
    let (slug, path) = crawler::write_shelf_markdown(title, markdown, &chapters)?;
    let mut pack_id = None;
    if to_pack {
        if let Some(existing) =
            crawler::find_existing_pack_for_shelf(&state.packs, &slug, title)
        {
            // rebuild content into same id when force or always refresh bodies
            let (pack, bodies) =
                crawler::build_pack_from_chapters(title, &chapters, &existing, &slug);
            match state.packs.save(pack) {
                Ok(saved) => {
                    for (rel, content) in bodies {
                        let _ = state.packs.write_chapter_body(&saved.id, &rel, &content);
                    }
                    pack_id = Some(saved.id);
                }
                Err(e) => return Err(format!("pack save: {e}")),
            }
        } else {
            let pid = format!(
                "pack-shelf-{}-{}",
                slug.chars().take(24).collect::<String>(),
                Utc::now().timestamp() % 100_000
            );
            let (pack, bodies) =
                crawler::build_pack_from_chapters(title, &chapters, &pid, &slug);
            match state.packs.save(pack) {
                Ok(saved) => {
                    for (rel, content) in bodies {
                        let _ = state.packs.write_chapter_body(&saved.id, &rel, &content);
                    }
                    pack_id = Some(saved.id);
                }
                Err(e) => return Err(format!("pack save: {e}")),
            }
        }
    }

    published.insert(source_key.to_string(), json!(fp));
    published.insert(
        format!("{source_key}::meta"),
        json!({
            "slug": slug,
            "title": title,
            "packId": pack_id,
            "at": Utc::now().to_rfc3339(),
        }),
    );
    save_published_map(state, &published);

    Ok(PublishOutcome {
        slug,
        title: title.to_string(),
        chapter_count: chapters.len(),
        pack_id,
        path: path.display().to_string(),
        skipped: false,
        reason: None,
    })
}

fn publish_tavern_session(
    state: &AppState,
    session_id: &str,
    title_override: Option<&str>,
    to_pack: bool,
    force: bool,
) -> Result<PublishOutcome, String> {
    let sess = state
        .sessions_tavern
        .get(session_id)
        .map_err(|e| e.to_string())?;
    let (mut title, md) = tavern_session_to_markdown(&sess);
    if let Some(t) = title_override.map(str::trim).filter(|s| !s.is_empty()) {
        title = t.to_string();
    }
    // rewrite heading if override
    let md = if title_override.is_some() {
        let rest = md.splitn(2, '\n').nth(1).unwrap_or("");
        format!("# {title}\n{rest}")
    } else {
        md
    };
    publish_markdown(
        state,
        &title,
        &md,
        to_pack,
        &format!("tavern:{session_id}"),
        force,
    )
}

/// POST manual publish
async fn chat_to_shelf(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChatToShelfBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let source = body
        .source
        .as_deref()
        .unwrap_or("tavern")
        .to_ascii_lowercase();
    let to_pack = body.to_pack.unwrap_or(true);
    let force = body.force.unwrap_or(false);

    let result = match source.as_str() {
        "text" => {
            let text = body.text.as_deref().unwrap_or("").trim();
            if text.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "error": "text required for source=text"})),
                )
                    .into_response();
            }
            let title = body
                .title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("聊天整理")
                .to_string();
            // ensure markdown has a title heading
            let md = if text.lines().next().map(|l| l.starts_with("# ")).unwrap_or(false) {
                text.to_string()
            } else {
                format!("# {title}\n\n{text}")
            };
            let key = format!("text:{}", fingerprint(&md));
            publish_markdown(&state, &title, &md, to_pack, &key, force)
        }
        _ => {
            let sid = match body.session_id.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                Some(s) => s,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "error": "sessionId required"})),
                    )
                        .into_response();
                }
            };
            publish_tavern_session(
                &state,
                sid,
                body.title.as_deref(),
                to_pack,
                force,
            )
        }
    };

    match result {
        Ok(o) => Json(json!({
            "ok": true,
            "skipped": o.skipped,
            "reason": o.reason,
            "slug": o.slug,
            "title": o.title,
            "chapterCount": o.chapter_count,
            "packId": o.pack_id,
            "path": o.path,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
    }
}

async fn get_schedule(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let cfg = load_schedule(&state);
    Json(json!({"ok": true, "schedule": cfg})).into_response()
}

async fn put_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScheduleConfig>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut cfg = body;
    if cfg.interval_hours < 1 {
        cfg.interval_hours = 1;
    }
    if cfg.interval_hours > 24 * 30 {
        cfg.interval_hours = 24 * 30;
    }
    if cfg.source.trim().is_empty() {
        cfg.source = "tavern".into();
    }
    // preserve last run fields if client omitted
    let prev = load_schedule(&state);
    if cfg.last_run_at.is_none() {
        cfg.last_run_at = prev.last_run_at;
    }
    if cfg.last_result.is_none() {
        cfg.last_result = prev.last_result;
    }
    match save_schedule(&state, &cfg) {
        Ok(()) => Json(json!({"ok": true, "schedule": cfg})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
    }
}

/// Process due tavern sessions according to schedule (manual trigger or bg tick).
pub fn run_due_inner(state: &AppState) -> Value {
    let mut cfg = load_schedule(state);
    if !cfg.enabled {
        return json!({"ok": true, "ran": false, "reason": "disabled", "published": []});
    }
    let min_turns = cfg.min_turns.max(1);
    let interval = ChronoDuration::hours(cfg.interval_hours.max(1) as i64);
    let now = Utc::now();

    let list = match state.sessions_tavern.list() {
        Ok(v) => v,
        Err(e) => {
            return json!({"ok": false, "error": e.to_string()});
        }
    };

    let mut published = Vec::new();
    let mut skipped = 0u32;
    let mut errors = Vec::new();

    for item in list {
        // list returns Value summaries
        let sid = item
            .get("sessionId")
            .or_else(|| item.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if sid.is_empty() {
            continue;
        }
        let turn = item
            .get("turn")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if turn < min_turns {
            skipped += 1;
            continue;
        }
        let updated = item
            .get("updatedAt")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));

        // throttle: if we published this session recently via fingerprint meta
        let map = load_published_map(state);
        if let Some(meta) = map.get(&format!("tavern:{sid}::meta")) {
            if let Some(at) = meta.get("at").and_then(|v| v.as_str()) {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(at) {
                    if now - ts.with_timezone(&Utc) < interval {
                        skipped += 1;
                        continue;
                    }
                }
            }
        }
        // also require session updated since last publish roughly
        if let Some(u) = updated {
            if let Some(meta) = map.get(&format!("tavern:{sid}::meta")) {
                if let Some(at) = meta.get("at").and_then(|v| v.as_str()) {
                    if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(at) {
                        if u <= ts.with_timezone(&Utc) {
                            skipped += 1;
                            continue;
                        }
                    }
                }
            }
        }

        match publish_tavern_session(state, &sid, None, cfg.to_pack, false) {
            Ok(o) => {
                if !o.skipped {
                    published.push(json!({
                        "sessionId": sid,
                        "slug": o.slug,
                        "title": o.title,
                        "packId": o.pack_id,
                        "chapterCount": o.chapter_count,
                    }));
                } else {
                    skipped += 1;
                }
            }
            Err(e) => errors.push(json!({"sessionId": sid, "error": e})),
        }
    }

    cfg.last_run_at = Some(now.to_rfc3339());
    cfg.last_result = Some(json!({
        "publishedCount": published.len(),
        "skipped": skipped,
        "errors": errors,
        "items": published,
    }));
    let _ = save_schedule(state, &cfg);

    json!({
        "ok": true,
        "ran": true,
        "publishedCount": published.len(),
        "skipped": skipped,
        "errors": errors,
        "published": published,
        "schedule": cfg,
    })
}

async fn run_due(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    Json(run_due_inner(&state)).into_response()
}

/// Background tick — call from main on interval. No auth.
pub fn spawn_schedule_tick(state: AppState) {
    tokio::spawn(async move {
        // first delay so boot is calm
        tokio::time::sleep(std::time::Duration::from_secs(45)).await;
        loop {
            let cfg = load_schedule(&state);
            if cfg.enabled {
                let out = run_due_inner(&state);
                tracing::info!(target: "chat_shelf", result=%out, "chat-to-shelf schedule tick");
            }
            // wake every 30 minutes to check due work (actual per-session throttle uses intervalHours)
            tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
        }
    });
}
