//! Kaleido core: DataRoot, users, sessions, password hashing.
//! Schema reserves user_id / workspace_id for multi-user later.
//! Single-user runtime is the product default.

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("auth: {0}")]
    Auth(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// Auth session cap hit after GC + optional auto-evict (W12).
    #[error("session cap: {message}")]
    SessionCap {
        message: String,
        active: usize,
        cap: usize,
        policy: String,
    },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Stable machine-readable API error (W11+). `details` is JSON object extras.
    #[error("{code}: {message}")]
    Coded {
        code: String,
        message: String,
        details: Value,
    },
    /// Revision CAS write conflict: on-disk revision no longer matches the base.
    #[error("conflict: {0}")]
    Conflict(String),
}

impl CoreError {
    pub fn coded(code: impl Into<String>, message: impl Into<String>) -> Self {
        CoreError::Coded {
            code: code.into(),
            message: message.into(),
            details: json!({}),
        }
    }

    pub fn coded_with(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Value,
    ) -> Self {
        CoreError::Coded {
            code: code.into(),
            message: message.into(),
            details: if details.is_object() { details } else { json!({ "extra": details }) },
        }
    }

    /// W11: file/content over Works text limit.
    pub fn works_too_large(kind: &str, size: u64, max_bytes: u64) -> Self {
        let code = match kind {
            "read" | "file" => "WORKS_FILE_TOO_LARGE",
            "append" => "WORKS_APPEND_TOO_LARGE",
            _ => "WORKS_CONTENT_TOO_LARGE",
        };
        let message = match kind {
            "read" | "file" => format!("file too large (max {max_bytes} bytes)"),
            "append" => format!("content too large after append (max {max_bytes} bytes)"),
            _ => format!("content too large (max {max_bytes} bytes)"),
        };
        CoreError::coded_with(
            code,
            message,
            json!({ "size": size, "maxBytes": max_bytes }),
        )
    }

    pub fn works_parent_missing() -> Self {
        CoreError::coded(
            "WORKS_PARENT_MISSING",
            "parent directory does not exist; mkdir first",
        )
    }

    pub fn works_binary() -> Self {
        CoreError::coded("WORKS_BINARY_REJECTED", "binary files are not allowed")
    }

    pub fn works_not_utf8() -> Self {
        CoreError::coded("WORKS_NOT_UTF8", "file is not valid UTF-8 text")
    }

    pub fn works_not_file() -> Self {
        CoreError::coded("WORKS_NOT_FILE", "not a file")
    }

    pub fn works_is_dir() -> Self {
        CoreError::coded("WORKS_IS_DIR", "path is a directory")
    }

    pub fn works_path_escape(msg: impl Into<String>) -> Self {
        CoreError::Coded {
            code: "WORKS_PATH_ESCAPE".into(),
            message: msg.into(),
            details: json!({}),
        }
    }

    pub fn works_path_traversal() -> Self {
        CoreError::coded("WORKS_PATH_TRAVERSAL", "path traversal is not allowed")
    }

    pub fn works_absolute_path() -> Self {
        CoreError::coded("WORKS_ABSOLUTE_PATH", "absolute paths are not allowed")
    }

    pub fn works_not_found(path: impl Into<String>) -> Self {
        let path = path.into();
        CoreError::Coded {
            code: "WORKS_NOT_FOUND".into(),
            message: format!("path {path}"),
            details: json!({ "path": path }),
        }
    }
}

pub type CoreResult<T> = Result<T, CoreError>;

mod st_import;
mod st_png;
mod st_card_webp;
mod st_card_illustrations;
mod st_card_skill;
mod st_compass;
mod st_review;
mod st_world_info;
mod st_regex;
mod st_regex_library;
mod st_timed_store;
mod st_automation_log;
mod st_token_estimate;
mod st_vector_index;
mod story_tavern;
mod tavern_engine;
pub mod st_simulation;
pub mod st_skimming;
pub mod st_emotional_hooks;
pub mod st_outline;
pub mod st_writing_style;
pub mod st_memory_contract;
pub mod dialogue_fingerprint;
pub mod harness;
pub mod memory_weaver;
pub mod ledger;
pub mod emotion_curve;
pub mod alias_merge;
pub mod entity_resolve; // [ENT] 实体解析层：关系边端点→角色卡 id + 幽灵/稀疏对账
pub mod rel_category; // [L0] rel 自由词→graph_store 五类映射（family/social/emotional/conflict/uncertain）
pub mod style_stats;
pub mod character_arc;
pub mod embed_hash;
pub mod db;
pub mod bakemono_summary;
pub mod bakemono_query_parse;
pub mod bakemono_retrieval;
pub mod chapter_diary;
pub mod hybrid_search;
pub mod graph_store;
pub mod analysis_store;
pub mod ai_admin_store;
pub mod character_archive;
pub mod foreshadow_store;
pub mod scene_card_store;
pub mod plugin;
pub mod world_state;
pub mod time_clock;
pub mod novel_workflow;
pub mod moa_comparison;
pub mod import_security;
pub mod text_chunker;
pub mod progressive_compress;
pub use import_security::{
    decode_utf8_imported_text, inspect_imported_plain_text, ImportThreat,
    MAX_IMPORT_TEXT_BYTES,
};
pub mod image_metadata;
pub use image_metadata::{read_raster_image_metadata, RasterImage, MAX_IMAGE_PIXELS};
pub mod docx_security;
pub use docx_security::validate_docx;
pub mod prompt_safety;
pub use st_import::{
    build_st_import_bundle, character_book_entry_count, character_book_to_world_book,
    extract_embedded_images, import_st_character_card_bundle, import_st_character_card_json,
    parse_st_character_card_json, parse_st_character_card_value, st_card_to_partner_fields,
    st_card_to_partner_item, AssetRef, StCardData, StImportBundle, StImportError,
};
pub use st_png::{
    base64_to_png, embed_st_card_in_png, extract_st_card_from_png, png_to_base64,
};
pub use st_card_webp::{extract_st_card_from_jpeg, extract_st_card_from_webp};
pub use st_card_illustrations::{
    extract_catbox_illustrations, CatboxIllustration,
};
pub use st_card_skill::{render_card_skill, sanitize_skill_name};
pub use st_world_info::{
    check_world_info, check_world_info_timed, chat_to_scan_buffer, content_from_wi_entries,
    entries_from_world_book, entry_values_from_world_book, format_wi_for_system,
    import_card_world_book, merge_wi_entry_value, parse_decorators, parse_mes_examples,
    parse_regex_from_string, parse_wi_entry, st_book_raw_from_entries, substitute_params,
    wi_entry_to_st_json, ActivatedEntry, CharacterFilter, SelectiveLogic, TimedWorldInfo,
    WiChatInjection, WiDepthEntry, WiEntry, WiExampleMessage, WiOutletEntry, WiPosition,
    WiPromptSlots, WiScanContext, WiScanResult, WiSettings, WiTimedEffect,
};
pub use st_timed_store::TimedWorldInfoStore;
pub use st_automation_log::{AutomationTriggerEvent, AutomationTriggerLog, AutomationTriggerStore};
pub use st_compass::{Compass, CompassStore, COMPASS_FILE_NAME, COMPASS_MAX_LEN, COMPASS_SCHEMA_VERSION};
pub use dialogue_fingerprint::{build_all, build_fingerprint, drift_check, CharacterFingerprint, DriftReport};
pub use st_review::{
    run_post_check, PostIssue, ReviewHistory, ReviewIssue, ReviewRun, ReviewStore,
    REVIEW_DIMENSIONS, REVIEW_FILE_NAME, REVIEW_MAX_RUNS, REVIEW_SCHEMA_VERSION,
    REVIEW_STATUS_ACCEPTED, REVIEW_STATUS_FIXED, REVIEW_STATUS_OPEN,
};
pub use st_token_estimate::{estimate_many, estimate_tokens, estimate_tokens_detailed, TokenEstimate, TokenEstimateMode};
pub use st_vector_index::{
    entry_embed_text, hits_to_map, merge_hit_lists, rank_hits, text_hash, vector_cosine_similarity,
    VectorActivationSettings, VectorHit, VectorIndexEntry, VectorIndexFile, VectorIndexStore,
};
pub use st_regex::{
    get_regexed_string, parse_regex_script, run_regex_script, scripts_from_card_fields,
    scripts_from_value, RegexPlacement, RegexScript,
};
pub use st_regex_library::{
    merge_regex_scripts, resolve_runtime_scripts, scripts_from_import_body, RegexLibraryFile,
    RegexLibraryStore,
};

pub use story_tavern::{
    ContentTier, CreateSessionRequest, EntryConfig, EntryRole, EngineTag, MemoryL1, MemoryL2, MemoryL2Event, MemoryL3, MemoryL4,
    MetaKnowledge, NodeExit, PackCharacterRef, PackSource, PackStore, PackSummary, PlayMode, Playable, Quality, TavernSave, TavernSaveMeta,
    ActorStateSystem, ActorStateUpdate, PlayerState, RewriteIntensity, SideBranchCatalog, SideBranchNode, StoryChapter, StoryNode, StoryPack, TavernMessage,
    ActorStatePackConfig,
    TavernPersona, TavernPersonaStore, TavernSession, TavernSessionStore, TavernPanel, WorldlineLine, WorldlineView,
    ToolResultBrief,
    SkillLoadInfo,
    RuleSystem, RuleCheck, RuleStateBinding, TurnCheckRequest, TurnCheckBonus, TurnCheckOutcomes, TurnCheckOutcome, TurnStateChange, CheckResult, CheckHistoryEntry,
    parse_dice, difficulty_to_dc, roll_check,
    DirectorPlan, DirectorPlanRunStatus, DirectorLedgerEntry, DirectorTaskGroup, director_due,
    fit_text_to_token_budget, opening_plan_due,
    retain_guard_events, retain_l2_events, turn_submit_guard,
    TurnDiagnostic, TurnCostLedger,
    // [morphling Wave B3 2026-08-16] 章节剧情摘要账本
    ChapterDiaryEntry, ChapterDiaryConfig,
    StageDirectorConfig,
    EventPackage, EventLogEntry, TellerEventCard, pick_event_card,
    TurnCheckpoint,
    // [morphling ROMA P0 2026-08-19] 回合级进度检查点（崩溃恢复判读用）
    TurnPhase, TurnProgress,
    build_mainline_opening, build_pack_novel_summary, build_side_branch_catalog, build_side_opening,
    build_worldline, clip_chars, enter_side_branch, ensure_focus_character, is_clean_cast_name,
    rotate_focus_character, seed_opening_if_needed, select_side_branch_nodes,
};
pub use tavern_engine::{
    TurnExtraction, apply_extraction, apply_memory_compression, build_compression_prompt,
    build_cross_session_context, build_extraction_prompt,
    build_memory_context, classify_engine_tag,
    cosine_similarity, heuristic_l2_l3_from_turn, parse_extraction_response, persist_cross_session,
    post_turn_extraction, tokenize,
    try_advance_node,
    // [morphling Wave B3 2026-08-16] 章节剧情摘要账本
    build_chapter_diary_prompt, parse_chapter_diary_response,
};

/// Configurable data root (env KALEIDO_DATA).
#[derive(Debug, Clone)]
pub struct DataRoot {
    root: PathBuf,
}

/// Brand directory under the data root, with legacy fallback:
/// prefer `<root>/Kaleido/<name>`; if it does not exist but the pre-rename
/// `<root>/MuseAI/<name>` does, keep using the legacy location (no data
/// migration shock for existing deployments).
pub fn brand_dir(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let new = root.join("Kaleido").join(name);
    let legacy = root.join("MuseAI").join(name);
    if new.exists() || !legacy.exists() { new } else { legacy }
}

impl DataRoot {
    pub fn new(root: impl Into<PathBuf>) -> CoreResult<Self> {
        let root = root.into();
        let me = Self { root };
        me.ensure_layout()?;
        Ok(me)
    }

    pub fn from_env() -> CoreResult<Self> {
        // KALEIDO_DATA primary, legacy MUSEAI_DATA fallback (rename compat).
        let root = std::env::var("KALEIDO_DATA")
            .or_else(|_| std::env::var("MUSEAI_DATA"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));
        Self::new(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure_layout(&self) -> CoreResult<()> {
        for sub in [
            "state",
            "sessions",
            "jobs",
            "artifacts",
            "audit",
            "secrets",
            "works",
            // Brand dir (Kaleido); legacy MuseAI/ dirs keep working via brand_dir()
            "Kaleido/agent-sessions",
            "Kaleido/config",
            "web",
            // Story Tavern (ST-0)
            "story-packs",
            "tavern-sessions",
            "tavern-persona",
            "tavern-saves",
            "cross-session",
        ] {
            fs::create_dir_all(self.root.join(sub))?;
        }
        Ok(())
    }

    pub fn story_packs_dir(&self) -> PathBuf {
        self.root.join("story-packs")
    }

    pub fn tavern_sessions_dir(&self) -> PathBuf {
        self.root.join("tavern-sessions")
    }

    pub fn tavern_persona_dir(&self) -> PathBuf {
        self.root.join("tavern-persona")
    }

    pub fn tavern_saves_dir(&self) -> PathBuf {
        self.root.join("tavern-saves")
    }

    pub fn cross_session_dir(&self) -> PathBuf {
        self.root.join("cross-session")
    }

    pub fn state_file(&self, name: &str) -> PathBuf {
        self.root.join("state").join(name)
    }

    pub fn jobs_dir(&self) -> PathBuf {
        self.root.join("jobs")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Upstream-compatible: $DATA/Kaleido/agent-sessions (legacy MuseAI fallback)
    pub fn agent_sessions_dir(&self) -> PathBuf {
        crate::brand_dir(&self.root, "agent-sessions")
    }

    /// Upstream-compatible app state: $DATA/Kaleido/config/{name}.json (legacy fallback)
    pub fn app_state_path(&self, name: &str) -> PathBuf {
        let safe = name.replace(['/', '\\'], "_");
        crate::brand_dir(&self.root, "config").join(format!("{safe}.json"))
    }

    pub fn audit_path(&self, name: &str) -> PathBuf {
        self.root.join("audit").join(name)
    }

    pub fn append_audit(&self, name: &str, line: &str) -> CoreResult<()> {
        let path = self.audit_path(name);
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    pub username: String,
    pub password_hash: String,
    pub user_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub created_at: Option<String>,
    /// P0-2 审计修复：admin 角色标记；任意登录用户可访问 /api/v1/ai/* 会被 403 拦截。
    /// 老 users.json 文件反序列化时默认 `false`，bootstrap admin 时设为 `true`。
    #[serde(default)]
    pub is_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub token: String,
    pub user_id: String,
    pub username: String,
    pub workspace_id: String,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// P0-2 审计修复：admin 角色标记缓存（登录时从 UserRecord 复制）；
    /// 老 sessions.json 反序列化时默认 `false`，新登录自动重新计算。
    #[serde(default)]
    pub is_admin: bool,
}

/// W12: auth session capacity snapshot (not story-tavern sessions).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapStats {
    pub active: usize,
    pub cap: usize,
    pub free: usize,
    /// `auto_evict` (default) | `reject`
    pub policy: String,
    pub ttl_hours: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_expires_at: Option<String>,
    pub expired_present: usize,
}

/// Canonical job kinds for jobs v2. Unknown kinds map to `other` on create if not listed.
pub const JOB_KINDS: &[&str] = &[
    "background",
    "book_travel",
    "outline",
    "agent",
    "chat",
    "other",
    "noop",
    "test",
];

/// Normalize legacy chat statuses onto the jobs-v2 enum.
/// Legacy: done → succeeded, error → failed, stopped → cancelled.
pub fn normalize_job_status(status: &str) -> String {
    match status {
        "done" => "succeeded".into(),
        "error" => "failed".into(),
        "stopped" => "cancelled".into(),
        "queued" | "running" | "succeeded" | "failed" | "cancelled" => status.into(),
        other => other.into(),
    }
}

pub fn is_terminal_job_status(status: &str) -> bool {
    matches!(
        normalize_job_status(status).as_str(),
        "succeeded" | "failed" | "cancelled"
    )
}

pub fn is_active_job_status(status: &str) -> bool {
    matches!(
        normalize_job_status(status).as_str(),
        "queued" | "running"
    )
}

/// [SSRF 加固 2026-08-15, 吸收 6fef9d12] 校验 LLM base URL：必须 http/https、
/// 非空 hostname，且禁回环/私网地址。
///
/// 放行机制（精确白名单，非全局开关）：`KALEIDO_ALLOW_LOCAL_LLM` 按逗号分隔
/// 列举允许的 host（如 `127.0.0.1,localhost`），仅这些 host 豁免；其余私网/回环
/// 一律拒绝。`=1` 仍兼容为「全放行」但强烈不建议（会绕过全部防护）。
pub fn validate_llm_base_url(raw: &str) -> CoreResult<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(CoreError::BadRequest(
            "llmBaseUrl must start with http:// or https://".into(),
        ));
    }
    let host = lower
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .and_then(|h| h.split(':').next())
        .unwrap_or("");
    if host.is_empty() {
        return Err(CoreError::BadRequest("llmBaseUrl has no hostname".into()));
    }
    let allow_raw = std::env::var("KALEIDO_ALLOW_LOCAL_LLM").unwrap_or_default();
    let allow_all = allow_raw.trim() == "1";
    let allow_hosts: Vec<String> = allow_raw
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let is_local = host == "localhost" || host == "127.0.0.1" || host == "0.0.0.0" || host == "[::1]" || host == "::1";
    if is_local {
        if allow_all || allow_hosts.contains(&host.to_string()) {
            return Ok(());
        }
        return Err(CoreError::BadRequest(
            "llmBaseUrl: localhost/loopback not allowed (set KALEIDO_ALLOW_LOCAL_LLM=127.0.0.1,localhost to override)".into(),
        ));
    }
    let is_private = host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.") // 云元数据服务 link-local（经典 SSRF 目标）
        || (host.starts_with("172.")
            && host[4..]
                .split('.')
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .map(|second| (16..=31).contains(&second))
                .unwrap_or(false));
    if is_private {
        if allow_all || allow_hosts.contains(&host.to_string()) {
            return Ok(());
        }
        return Err(CoreError::BadRequest("llmBaseUrl: private IP not allowed".into()));
    }
    Ok(())
}

/// SSE / progress event attached to a job (also used for stream replay).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    pub event_type: String,
    pub ts: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// D2: machine-readable code for eventType="error" events (P1-4 parity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JobEvent {
    pub fn progress(message: impl Into<String>, progress: f64) -> Self {
        Self {
            event_type: "progress".into(),
            ts: Utc::now(),
            message: Some(message.into()),
            progress: Some(progress.clamp(0.0, 1.0)),
            code: None,
            data: None,
        }
    }

    pub fn event(message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            event_type: "event".into(),
            ts: Utc::now(),
            message: Some(message.into()),
            progress: None,
            code: None,
            data,
        }
    }

    pub fn done(message: Option<String>) -> Self {
        Self {
            event_type: "done".into(),
            ts: Utc::now(),
            message,
            progress: Some(1.0),
            code: None,
            data: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::error_code("JOB_ERROR", message)
    }

    /// D2: error with a stable machine-readable code (P1-4 envelope parity).
    pub fn error_code(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            event_type: "error".into(),
            ts: Utc::now(),
            message: Some(message.into()),
            progress: None,
            code: Some(code.into()),
            data: None,
        }
    }
}

/// Disk + internal representation uses snake_case for S4 chat job JSON compat.
/// API responses use `to_api_json()` (camelCase + id alias).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub run_id: String,
    pub kind: String,
    pub user_id: String,
    pub workspace_id: String,
    /// Canonical: queued | running | succeeded | failed | cancelled
    /// (legacy done/error/stopped are normalized on write)
    pub status: String,
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub meta: Value,
    /// Create payload (jobs v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    /// 0.0–1.0 progress fraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_message: Option<String>,
    /// Opaque resume cursor for long jobs (Background / BookTravel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Recent events for SSE replay (capped).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<JobEvent>,
}

impl JobRecord {
    /// Alias used by jobs v2 JSON responses.
    pub fn id(&self) -> &str {
        &self.run_id
    }

    pub fn to_api_json(&self) -> Value {
        json!({
            "id": self.run_id,
            "runId": self.run_id,
            "kind": self.kind,
            "userId": self.user_id,
            "workspaceId": self.workspace_id,
            "status": normalize_job_status(&self.status),
            "model": self.model,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
            "meta": self.meta,
            "payload": self.payload,
            "progress": self.progress,
            "progressMessage": self.progress_message,
            "cursor": self.cursor,
            "error": self.error,
            "result": self.result,
            "events": self.events,
        })
    }
}

/// Filters for `JobStore::list`.
#[derive(Debug, Clone, Default)]
pub struct JobListFilter {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub user_id: Option<String>,
    pub workspace_id: Option<String>,
    pub limit: usize,
}

const JOB_EVENTS_CAP: usize = 64;

// --- Agent sessions (upstream mobile contract, camelCase JSON) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_blocks: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRecord {
    pub id: String,
    pub title: String,
    pub saved_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    #[serde(default)]
    pub messages: Vec<AgentSessionMessage>,
    #[serde(default)]
    pub selected_reference_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_outline_file: Option<String>,
    #[serde(default)]
    pub todos: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_compaction: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_card_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_card_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_world_book_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_role_loading_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_style_preset_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_style_preset_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_system_prompt_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub book_travel_state: Option<Value>,
    // --- Morphling additions (Liyuan-inspired) ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub world_lines: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionSummary {
    pub id: String,
    pub title: String,
    pub saved_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_card_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_card_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_world_book_id: Option<String>,
}

impl From<&AgentSessionRecord> for AgentSessionSummary {
    fn from(r: &AgentSessionRecord) -> Self {
        Self {
            id: r.id.clone(),
            title: r.title.clone(),
            saved_at: r.saved_at,
            session_kind: r.session_kind.clone(),
            character_card_id: r.character_card_id.clone(),
            character_card_ids: r.character_card_ids.clone(),
            selected_world_book_id: r.selected_world_book_id.clone(),
        }
    }
}

#[derive(Clone)]
pub struct AgentSessionStore {
    data: DataRoot,
}

impl AgentSessionStore {
    pub fn new(data: DataRoot) -> Self {
        Self { data }
    }

    fn dir(&self) -> PathBuf {
        self.data.agent_sessions_dir()
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir().join(format!("{id}.json"))
    }

    pub fn validate_id(id: &str) -> CoreResult<()> {
        // Prefix + strict charset only — no `/`, `\`, `..`, or path separators
        // (prevents dir.join("{id}.json") from escaping agent_sessions/).
        let prefix_ok =
            id.starts_with("partner-session-") || id.starts_with("story-session-");
        let charset_ok = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !id.contains("..");
        if prefix_ok && charset_ok {
            Ok(())
        } else {
            Err(CoreError::Forbidden(
                "invalid session id (partner-session-*|story-session-* alnum/_/- only)".into(),
            ))
        }
    }

    pub fn list(
        &self,
        prefix: &str,
        session_kind: Option<&str>,
    ) -> CoreResult<Vec<AgentSessionSummary>> {
        if prefix != "partner-session-" && prefix != "story-session-" {
            return Err(CoreError::BadRequest("invalid session prefix".into()));
        }
        let mut out = Vec::new();
        let dir = self.dir();
        if !dir.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }
            let id = name.trim_end_matches(".json").to_string();
            if !id.starts_with(prefix) {
                continue;
            }
            let Ok(raw) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(rec) = serde_json::from_str::<AgentSessionRecord>(&raw) else {
                continue;
            };
            if prefix == "story-session-" {
                let kind = rec.session_kind.as_deref().unwrap_or("story");
                let want = session_kind.unwrap_or("story");
                if kind != want {
                    continue;
                }
            }
            out.push(AgentSessionSummary::from(&rec));
        }
        out.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
        Ok(out)
    }

    pub fn load(&self, id: &str) -> CoreResult<AgentSessionRecord> {
        Self::validate_id(id)?;
        let path = self.path(id);
        if !path.exists() {
            return Err(CoreError::NotFound(format!("session {id}")));
        }
        let raw = fs::read_to_string(path)?;
        let rec: AgentSessionRecord = serde_json::from_str(&raw)?;
        if id.starts_with("story-session-")
            && !matches!(rec.session_kind.as_deref(), None | Some("story"))
        {
            return Err(CoreError::Forbidden("book travel sessions blocked".into()));
        }
        Ok(rec)
    }

    pub fn save(&self, mut record: AgentSessionRecord) -> CoreResult<AgentSessionSummary> {
        Self::validate_id(&record.id)?;
        if record.id.starts_with("story-session-") {
            match record.session_kind.as_deref() {
                None => record.session_kind = Some("story".into()),
                Some("story") => {}
                Some(_) => {
                    return Err(CoreError::Forbidden("book travel sessions blocked".into()));
                }
            }
        } else if record.session_kind.is_some() {
            return Err(CoreError::BadRequest(
                "chat session must not include sessionKind".into(),
            ));
        }
        if record.saved_at == 0 {
            record.saved_at = now_millis();
        }
        fs::create_dir_all(self.dir())?;
        fs::write(self.path(&record.id), serde_json::to_string_pretty(&record)?)?;
        Ok(AgentSessionSummary::from(&record))
    }

    pub fn delete(&self, id: &str) -> CoreResult<()> {
        Self::validate_id(id)?;
        let path = self.path(id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn update_title(&self, id: &str, title: &str) -> CoreResult<AgentSessionSummary> {
        let mut rec = self.load(id)?;
        rec.title = title.to_string();
        rec.saved_at = now_millis();
        self.save(rec)
    }
}

#[derive(Clone)]
pub struct AppStateStore {
    data: DataRoot,
}

impl AppStateStore {
    pub fn new(data: DataRoot) -> Self {
        Self { data }
    }

    pub fn data_root(&self) -> &DataRoot {
        &self.data
    }

    /// Allowlist for S2 (partner + settings). Expand later.
    /// C2: `partner-store-*` prefix covers per-user scoped stores
    /// (`partner-store-{user_scope}.json`).
    pub fn is_allowed(name: &str) -> bool {
        if name.starts_with("partner-store-") {
            return true;
        }
        matches!(
            name,
            "partner-store"
                | "settings-store"
                | "style-presets"
                | "character-card-groups"
                | "works-store"
                | "author-projects"
                | "chat-shelf-schedule"
        )
    }

    pub fn load(&self, name: &str) -> CoreResult<String> {
        if !Self::is_allowed(name) {
            return Err(CoreError::Forbidden(format!("state {name} not allowed")));
        }
        let path = self.data.app_state_path(name);
        if !path.exists() {
            return Ok(default_state_json(name));
        }
        Ok(fs::read_to_string(path)?)
    }

    pub fn save(&self, name: &str, content: &str) -> CoreResult<()> {
        if !Self::is_allowed(name) {
            return Err(CoreError::Forbidden(format!("state {name} not allowed")));
        }
        // settings: never let client overwrite server-held secrets with empty if we inject later
        let path = self.data.app_state_path(name);
        if name == "settings-store" {
            let mut final_json: Value = serde_json::from_str(content)
                .unwrap_or_else(|_| json_settings_shell(content));
            if let Ok(existing) = fs::read_to_string(&path) {
                if let Ok(mut old) = serde_json::from_str::<Value>(&existing) {
                    merge_settings_preserving_keys(&mut old, &final_json);
                    final_json = old;
                }
            }
            // strip plaintext keys from client writes into secrets file instead
            if let Some(state) = final_json.get_mut("state").and_then(|s| s.as_object_mut()) {
                if let Some(key) = state.get("llmApiKey").and_then(|v| v.as_str()) {
                    if !key.is_empty() && !key.contains('•') && key != "[server]" {
                        let secrets = self.data.root().join("secrets").join("llm_api_key.txt");
                        let _ = fs::write(&secrets, key).map(|_| {
                            // M-6: restrict plaintext key file to owner (0600).
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let _ = fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600));
                            }
                        });
                        state.insert("llmApiKey".into(), Value::String("[server]".into()));
                    }
                }
            }
            fs::write(&path, serde_json::to_string_pretty(&final_json)?)?;
        } else {
            fs::write(&path, content)?;
        }
        // P2-3 审计修复：敏感 state 文件收紧为 owner-only。
        let _ = restrict_owner_only(&path);
        Ok(())
    }
}

fn json_settings_shell(content: &str) -> Value {
    serde_json::json!({ "state": { "raw": content } })
}

fn default_state_json(name: &str) -> String {
    match name {
        "partner-store" => serde_json::to_string_pretty(&serde_json::json!({
            "state": {
                "worldBooks": [],
                "characterCards": [],
                "selectedWorldBookId": null,
                "selectedCharacterCardId": null
            }
        }))
        .unwrap_or_else(|_| "{}".into()),
        "settings-store" => serde_json::to_string_pretty(&serde_json::json!({
            "state": {
                "modelInterface": "OpenAI",
                "llmBaseUrl": "",
                "llmApiKey": "[server]",
                "llmModel": "",
                "partnerChatPrompt": "你是一个体贴温和的伴侣，请用温暖、真实而细节丰富的语言与用户交谈，避免机器感。"
            }
        }))
        .unwrap_or_else(|_| "{}".into()),
        "author-projects" => "[]".into(),
        "chat-shelf-schedule" => serde_json::to_string_pretty(&serde_json::json!({
            "enabled": false,
            "intervalHours": 24,
            "minTurns": 3,
            "toPack": true,
            "source": "tavern",
            "lastRunAt": null,
            "lastResult": null
        })).unwrap_or_else(|_| "{}".into()),
        _ => "{}".into(),
    }
}

fn merge_settings_preserving_keys(existing: &mut Value, incoming: &Value) {
    let Some(ex_state) = existing.get_mut("state").and_then(|s| s.as_object_mut()) else {
        *existing = incoming.clone();
        return;
    };
    let Some(in_state) = incoming.get("state").and_then(|s| s.as_object()) else {
        return;
    };
    for (k, v) in in_state {
        if k == "llmApiKey" {
            let s = v.as_str().unwrap_or("");
            if s.is_empty() || s.contains('•') || s == "[server]" {
                continue; // keep existing secret marker / value
            }
        }
        if k == "llmBaseUrl" && v.as_str().map(|s| s.is_empty()).unwrap_or(false) {
            continue;
        }
        ex_state.insert(k.clone(), v.clone());
    }
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hash")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// P2-3 审计修复：收紧敏感数据文件权限为 owner-only（0600），
/// 防止同机其他用户读取 users.json / sessions.json / partner-store 等。
pub(crate) fn restrict_owner_only(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(path)?;
        let current = meta.permissions().mode();
        let masked = current & 0o777;
        // 仅 owner 读写；strip group/other 读/写/执行
        let desired = masked & 0o700 | 0o600;
        if desired != masked {
            let mut perm = meta.permissions();
            perm.set_mode(desired);
            fs::set_permissions(path, perm)?;
        }
    }
    Ok(())
}

/// In-memory + JSON-backed auth store. Multi-user schema ready; runtime single-user.
#[derive(Clone)]
pub struct AuthStore {
    data: DataRoot,
    users: Arc<Mutex<HashMap<String, UserRecord>>>,
    sessions: Arc<Mutex<HashMap<String, SessionRecord>>>,
    login_attempts: Arc<Mutex<HashMap<String, (DateTime<Utc>, u32)>>>,
    session_ttl_hours: i64,
    max_login_per_window: u32,
    login_window_secs: i64,
    #[allow(dead_code)] // [P7] 引导值仅作 session_cap_live 初值来源；运行时统一读 live 覆盖
    max_sessions: usize,
    /// `auto_evict` (default) | `reject`
    #[allow(dead_code)] // [P7] 同上——normalized 读取走 session_cap_live.1
    session_cap_policy: String,
    /// Live overrides (settings PATCH) — (cap, policy). None fields keep boot value.
    session_cap_live: Arc<Mutex<(usize, String)>>,
}

impl AuthStore {
    pub fn load(data: DataRoot) -> CoreResult<Self> {
        let users = load_or_bootstrap_users(&data)?;
        let sessions = load_sessions(&data).unwrap_or_default();
        // Settings-store can override env for cap/policy (W12).
        let (max_sessions, policy) = load_session_cap_config(&data);
        Ok(Self {
            data,
            users: Arc::new(Mutex::new(users)),
            sessions: Arc::new(Mutex::new(sessions)),
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
            session_ttl_hours: std::env::var("KALEIDO_SESSION_TTL_HOURS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(12),
            max_login_per_window: std::env::var("KALEIDO_LOGIN_MAX_ATTEMPTS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10),
            login_window_secs: std::env::var("KALEIDO_LOGIN_WINDOW_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(300),
            max_sessions,
            session_cap_policy: policy.clone(),
            session_cap_live: Arc::new(Mutex::new((max_sessions, policy))),
        })
    }

    pub fn data_root(&self) -> &DataRoot {
        &self.data
    }

    fn check_login_rate(&self, key: &str) -> CoreResult<()> {
        let mut map = self.login_attempts.lock();
        let now = Utc::now();
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if (now - entry.0).num_seconds() > self.login_window_secs {
            *entry = (now, 0);
        }
        entry.1 += 1;
        if entry.1 > self.max_login_per_window {
            return Err(CoreError::RateLimited(format!(
                "too many login attempts for {key}; retry later"
            )));
        }
        Ok(())
    }

    pub fn login(&self, username: &str, password: &str, rate_key: &str) -> CoreResult<SessionRecord> {
        self.check_login_rate(rate_key)?;
        self.check_login_rate(&format!("user:{username}"))?;

        {
            let mut sessions = self.sessions.lock();
            // Drop expired first so dogfood / multi-device does not stick at max_sessions
            let now = Utc::now();
            let before = sessions.len();
            sessions.retain(|_, s| s.expires_at > now);
            let expired_dropped = before.saturating_sub(sessions.len());
            if expired_dropped > 0 {
                let _ = persist_sessions(&self.data, &sessions);
                let _ = self.data.append_audit(
                    "auth.log",
                    &format!(
                        "{} session_gc expired={} active={} cap={}",
                        Utc::now().to_rfc3339(),
                        expired_dropped,
                        sessions.len(),
                        self.effective_max_sessions()
                    ),
                );
            }
            // auto_evict: free slots by oldest until room for one new login
            let policy = self.session_cap_policy_normalized();
            let cap = self.effective_max_sessions();
            if sessions.len() >= cap && policy == "auto_evict" {
                let need = sessions.len() + 1 - cap;
                let mut victims: Vec<(String, DateTime<Utc>, DateTime<Utc>)> = sessions
                    .iter()
                    .map(|(k, s)| {
                        let created = s.created_at.unwrap_or(s.expires_at);
                        (k.clone(), s.expires_at, created)
                    })
                    .collect();
                victims.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));
                let mut evicted = 0usize;
                for (tok, _, _) in victims.into_iter().take(need) {
                    sessions.remove(&tok);
                    evicted += 1;
                    let _ = self.data.append_audit(
                        "auth.log",
                        &format!(
                            "{} session_evict token={} reason=max_sessions policy=auto_evict",
                            Utc::now().to_rfc3339(),
                            &tok[..8.min(tok.len())]
                        ),
                    );
                }
                if evicted > 0 {
                    let _ = persist_sessions(&self.data, &sessions);
                }
            }
            if sessions.len() >= cap {
                return Err(CoreError::SessionCap {
                    message: format!(
                        "too many active sessions ({}/{})",
                        sessions.len(),
                        cap
                    ),
                    active: sessions.len(),
                    cap,
                    policy,
                });
            }
        }

        let users = self.users.lock();
        let user = users.get(username).ok_or_else(|| {
            let _ = self.data.append_audit(
                "auth.log",
                &format!("{} login user={} fail=unknown", Utc::now().to_rfc3339(), username),
            );
            CoreError::Auth("invalid credentials".into())
        })?;
        if !verify_password(password, &user.password_hash) {
            let _ = self.data.append_audit(
                "auth.log",
                &format!("{} login user={} fail=badpass", Utc::now().to_rfc3339(), username),
            );
            return Err(CoreError::Auth("invalid credentials".into()));
        }

        let token = Uuid::new_v4().to_string();
        let expires = Utc::now() + Duration::hours(self.session_ttl_hours);
        let rec = SessionRecord {
            token: token.clone(),
            user_id: user.user_id.clone(),
            username: user.username.clone(),
            workspace_id: user.workspace_id.clone(),
            expires_at: expires,
            created_at: Some(Utc::now()),
            // P0-2 审计修复：登录时缓存 admin 角色，下游 handler 直接读取 SessionRecord
            // 即可判定 admin，避免每次都查 users map。
            is_admin: user.is_admin,
        };
        drop(users);

        self.sessions.lock().insert(token, rec.clone());
        let _ = persist_sessions(&self.data, &self.sessions.lock());
        let _ = self.data.append_audit(
            "auth.log",
            &format!(
                "{} login user={} ok session={}",
                Utc::now().to_rfc3339(),
                rec.username,
                &rec.token[..8]
            ),
        );
        Ok(rec)
    }

    pub fn logout(&self, token: &str) -> CoreResult<()> {
        self.sessions.lock().remove(token);
        let _ = persist_sessions(&self.data, &self.sessions.lock());
        Ok(())
    }

    pub fn resolve_session(&self, token: &str) -> CoreResult<SessionRecord> {
        let mut sessions = self.sessions.lock();
        let Some(s) = sessions.get(token).cloned() else {
            return Err(CoreError::Auth("invalid or expired session".into()));
        };
        if s.expires_at <= Utc::now() {
            sessions.remove(token);
            let _ = persist_sessions(&self.data, &sessions);
            return Err(CoreError::Auth("invalid or expired session".into()));
        }
        Ok(s)
    }

    pub fn user_count(&self) -> usize {
        self.users.lock().len()
    }

    pub fn normalize_policy(p: &str) -> String {
        match p.trim().to_ascii_lowercase().as_str() {
            "reject" | "hard" | "fail" => "reject".into(),
            _ => "auto_evict".into(),
        }
    }

    fn session_cap_policy_normalized(&self) -> String {
        let live = self.session_cap_live.lock();
        Self::normalize_policy(&live.1)
    }

    pub fn effective_max_sessions(&self) -> usize {
        self.session_cap_live.lock().0.max(1)
    }

    pub fn max_sessions(&self) -> usize {
        self.effective_max_sessions()
    }

    pub fn session_ttl_hours(&self) -> i64 {
        self.session_ttl_hours
    }

    pub fn session_cap_policy(&self) -> String {
        self.session_cap_policy_normalized()
    }

    /// Hot-apply cap/policy (from settings PATCH). Persists only via settings-store separately.
    pub fn apply_session_cap_config(&self, cap: Option<usize>, policy: Option<&str>) {
        let mut live = self.session_cap_live.lock();
        if let Some(c) = cap {
            if c >= 1 {
                live.0 = c.min(10_000);
            }
        }
        if let Some(p) = policy {
            live.1 = Self::normalize_policy(p);
        }
    }

    /// Snapshot for GET /api/v1/sessions/stats.
    pub fn session_stats(&self) -> SessionCapStats {
        let now = Utc::now();
        let sessions = self.sessions.lock();
        let mut active = 0usize;
        let mut expired_present = 0usize;
        let mut oldest_created: Option<DateTime<Utc>> = None;
        let mut oldest_exp: Option<DateTime<Utc>> = None;
        for s in sessions.values() {
            if s.expires_at <= now {
                expired_present += 1;
                continue;
            }
            active += 1;
            let created = s.created_at.unwrap_or(s.expires_at);
            oldest_created = Some(match oldest_created {
                Some(o) if o < created => o,
                _ => created,
            });
            oldest_exp = Some(match oldest_exp {
                Some(o) if o < s.expires_at => o,
                _ => s.expires_at,
            });
        }
        let cap = self.effective_max_sessions();
        SessionCapStats {
            active,
            cap,
            free: cap.saturating_sub(active),
            policy: self.session_cap_policy_normalized(),
            ttl_hours: self.session_ttl_hours,
            oldest_created_at: oldest_created.map(|d| d.to_rfc3339()),
            oldest_expires_at: oldest_exp.map(|d| d.to_rfc3339()),
            expired_present,
        }
    }

    /// Drop expired auth sessions; returns how many removed.
    pub fn prune_expired_sessions(&self) -> CoreResult<usize> {
        let mut sessions = self.sessions.lock();
        let now = Utc::now();
        let before = sessions.len();
        sessions.retain(|_, s| s.expires_at > now);
        let n = before.saturating_sub(sessions.len());
        if n > 0 {
            persist_sessions(&self.data, &sessions)?;
            let _ = self.data.append_audit(
                "auth.log",
                &format!(
                    "{} session_prune_expired removed={} active={}",
                    Utc::now().to_rfc3339(),
                    n,
                    sessions.len()
                ),
            );
        }
        Ok(n)
    }

    /// Evict up to `count` oldest active sessions (by expires_at, then created_at).
    pub fn prune_oldest_sessions(&self, count: usize) -> CoreResult<usize> {
        if count == 0 {
            return Ok(0);
        }
        let mut sessions = self.sessions.lock();
        let now = Utc::now();
        sessions.retain(|_, s| s.expires_at > now);
        let mut victims: Vec<(String, DateTime<Utc>, DateTime<Utc>)> = sessions
            .iter()
            .map(|(k, s)| {
                (
                    k.clone(),
                    s.expires_at,
                    s.created_at.unwrap_or(s.expires_at),
                )
            })
            .collect();
        victims.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));
        let mut removed = 0usize;
        for (tok, _, _) in victims.into_iter().take(count) {
            sessions.remove(&tok);
            removed += 1;
            let _ = self.data.append_audit(
                "auth.log",
                &format!(
                    "{} session_prune_oldest token={} reason=api",
                    Utc::now().to_rfc3339(),
                    &tok[..8.min(tok.len())]
                ),
            );
        }
        if removed > 0 {
            persist_sessions(&self.data, &sessions)?;
        }
        Ok(removed)
    }

    /// List active session summaries (no full tokens — prefix only).
    pub fn list_session_summaries(&self, limit: usize) -> Vec<serde_json::Value> {
        let now = Utc::now();
        let sessions = self.sessions.lock();
        let mut rows: Vec<_> = sessions
            .values()
            .filter(|s| s.expires_at > now)
            .cloned()
            .collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        rows.into_iter()
            .take(limit.clamp(1, 500))
            .map(|s| {
                json!({
                    "tokenPrefix": &s.token[..8.min(s.token.len())],
                    "userId": s.user_id,
                    "username": s.username,
                    "createdAt": s.created_at.map(|d| d.to_rfc3339()),
                    "expiresAt": s.expires_at.to_rfc3339(),
                })
            })
            .collect()
    }
}

/// Resolve max sessions + policy: settings-store overrides env.
fn load_session_cap_config(data: &DataRoot) -> (usize, String) {
    let env_cap = std::env::var("KALEIDO_MAX_SESSIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50usize)
        .max(1);
    let env_policy = std::env::var("KALEIDO_SESSION_CAP_POLICY")
        .unwrap_or_else(|_| "auto_evict".into());
    let path = data.app_state_path("settings-store");
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            let state = v.get("state").cloned().unwrap_or(v);
            let cap = state
                .get("sessionMax")
                .or_else(|| state.get("maxSessions"))
                .and_then(|x| x.as_u64())
                .map(|n| n as usize)
                .filter(|n| *n >= 1)
                .unwrap_or(env_cap)
                .min(10_000);
            let policy = state
                .get("sessionCapPolicy")
                .and_then(|x| x.as_str())
                .unwrap_or(env_policy.as_str())
                .to_string();
            return (cap.max(1), AuthStore::normalize_policy(&policy));
        }
    }
    (env_cap, AuthStore::normalize_policy(&env_policy))
}

fn load_or_bootstrap_users(data: &DataRoot) -> CoreResult<HashMap<String, UserRecord>> {
    let path = data.state_file("users.json");
    if path.exists() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, UserRecord>>(&raw) {
                if !map.is_empty() {
                    return Ok(map);
                }
            }
        }
    }

    let username = std::env::var("KALEIDO_ADMIN_USER").unwrap_or_else(|_| "admin".into());
    let password = std::env::var("KALEIDO_ADMIN_PASSWORD").map_err(|_| {
        CoreError::Auth(
            "KALEIDO_ADMIN_PASSWORD is not set — refusing to bootstrap admin with a default password; set it before starting"
                .into(),
        )
    })?;
    if password.len() < 8 {
        eprintln!("[kaleido-core] WARNING: admin password shorter than 8 chars");
    }
    let user = UserRecord {
        username: username.clone(),
        password_hash: hash_password(&password),
        user_id: Uuid::new_v4().to_string(),
        workspace_id: Uuid::new_v4().to_string(),
        created_at: Some(Utc::now().to_rfc3339()),
        // P0-2 审计修复：bootstrap admin 默认带 admin 角色。
        is_admin: true,
    };
    let mut map = HashMap::new();
    map.insert(username, user);
    fs::write(&path, serde_json::to_string_pretty(&map)?)?;
    let _ = restrict_owner_only(&path);
    eprintln!(
        "[kaleido-core] bootstrapped admin into {} (set KALEIDO_ADMIN_PASSWORD in prod)",
        path.display()
    );
    let _ = data.append_audit(
        "auth.log",
        &format!("{} bootstrap admin users={}", Utc::now().to_rfc3339(), map.len()),
    );
    Ok(map)
}

fn load_sessions(data: &DataRoot) -> CoreResult<HashMap<String, SessionRecord>> {
    let path = data.state_file("sessions.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = fs::read_to_string(path)?;
    let mut map: HashMap<String, SessionRecord> = serde_json::from_str(&raw)?;
    let now = Utc::now();
    map.retain(|_, s| s.expires_at > now);
    Ok(map)
}

fn persist_sessions(data: &DataRoot, sessions: &HashMap<String, SessionRecord>) -> CoreResult<()> {
    let path = data.state_file("sessions.json");
    fs::write(&path, serde_json::to_string_pretty(sessions)?)?;
    // P2-3 审计修复：sessions 含 bearer token，收紧为 owner-only。
    let _ = restrict_owner_only(&path);
    Ok(())
}

#[derive(Clone)]
pub struct JobStore {
    data: DataRoot,
    /// In-memory index of non-terminal jobs (queued + running). Terminal jobs live on disk only.
    active: Arc<Mutex<HashMap<String, JobRecord>>>,
    max_concurrent: usize,
    /// After a restart, re-schedules recovered non-terminal jobs. kaleido-core stays
    /// server-agnostic: the hook only receives the JobRecord and the AppState side decides
    /// whether/how to restart the underlying worker (default no-op).
    recover_hook: Arc<dyn Fn(&JobRecord) + Send + Sync>,
    /// P8: 并发水位/终态计数（见 [`JobMetrics`]）。
    pub metrics: Arc<JobMetrics>,
}

/// P8: 进程生命周期内的作业并发水位与终态计数（Atomic，无锁采样）。
/// P10: totals（created/succeeded/failed/cancelled）落盘 `jobs_dir/metrics.json`
/// 并在 boot 恢复 —— 跨重启累计；peak_running / boot_at_unix 保持进程作用域。
/// /health 暴露为 `jobs_metrics`。
#[derive(Debug, Default)]
pub struct JobMetrics {
    pub boot_at_unix: AtomicU64,
    pub peak_running: AtomicU64,
    pub total_created: AtomicU64,
    pub total_succeeded: AtomicU64,
    pub total_failed: AtomicU64,
    pub total_cancelled: AtomicU64,
}

/// P10: metrics.json 落盘快照（仅累计 totals；peak/boot 是进程作用域，不落盘）。
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct JobMetricsSnapshot {
    total_created: u64,
    total_succeeded: u64,
    total_failed: u64,
    total_cancelled: u64,
}

impl JobMetrics {
    /// 与 job 记录同目录；reload/prune 均按文件名显式排除本文件。
    const FILE_NAME: &'static str = "metrics.json";

    fn snapshot(&self) -> JobMetricsSnapshot {
        JobMetricsSnapshot {
            total_created: self.total_created.load(Ordering::Relaxed),
            total_succeeded: self.total_succeeded.load(Ordering::Relaxed),
            total_failed: self.total_failed.load(Ordering::Relaxed),
            total_cancelled: self.total_cancelled.load(Ordering::Relaxed),
        }
    }

    /// P10: totals 落盘（tmp+rename 原子替换）。失败静默 —— 指标采集不得影响主流程。
    fn persist(&self, dir: &Path) {
        let body = match serde_json::to_string(&self.snapshot()) {
            Ok(b) => b,
            Err(_) => return,
        };
        let tmp = dir.join(format!("{}.tmp", Self::FILE_NAME));
        let dst = dir.join(Self::FILE_NAME);
        if fs::write(&tmp, body).is_ok() {
            let _ = fs::rename(&tmp, &dst);
        }
    }

    /// P10: boot 时恢复累计 totals（store 语义：无文件/损坏文件 → 从 0 起算）。
    fn restore_totals(&self, dir: &Path) {
        if let Ok(raw) = fs::read_to_string(dir.join(Self::FILE_NAME)) {
            if let Ok(snap) = serde_json::from_str::<JobMetricsSnapshot>(&raw) {
                self.total_created.store(snap.total_created, Ordering::Relaxed);
                self.total_succeeded.store(snap.total_succeeded, Ordering::Relaxed);
                self.total_failed.store(snap.total_failed, Ordering::Relaxed);
                self.total_cancelled.store(snap.total_cancelled, Ordering::Relaxed);
            }
        }
    }

    fn note_terminal(&self, status: &str) {
        match status {
            "succeeded" | "done" => self.total_succeeded.fetch_add(1, Ordering::Relaxed),
            "failed" | "error" => self.total_failed.fetch_add(1, Ordering::Relaxed),
            "cancelled" | "stopped" => self.total_cancelled.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }
}

/// P10: env 解析纯函数化 —— `JobStore::new` 即 `parse_max_concurrent(env) + with_max_concurrent`。
/// 抽出来是为了让测试直测解析/clamp 语义，无需 set_var 污染进程级全局态（并行竞态根源）。
fn parse_max_concurrent(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.parse().ok()).unwrap_or(2).clamp(1, 4)
}

impl JobStore {
    pub fn new(data: DataRoot) -> Self {
        let raw = std::env::var("KALEIDO_MAX_CONCURRENT_JOBS").ok();
        Self::with_max_concurrent(data, parse_max_concurrent(raw.as_deref()))
    }

    /// Construct with an explicit concurrency cap (clamped to 1..=2).
    /// Prefer this in tests so capacity is not affected by process-wide env races.
    pub fn with_max_concurrent(data: DataRoot, max_concurrent: usize) -> Self {
        let max_concurrent = max_concurrent.clamp(1, 4);
        let store = Self {
            data,
            active: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
            recover_hook: Arc::new(|_| {}),
            metrics: Arc::new(JobMetrics {
                boot_at_unix: AtomicU64::new(Utc::now().timestamp().max(0) as u64),
                ..Default::default()
            }),
        };
        // Reload non-terminal jobs from disk so restart keeps queue state.
        if let Ok(entries) = fs::read_dir(store.data.jobs_dir()) {
            let mut active = store.active.lock();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                // P10: 指标快照不是 job 记录，跳过（否则 serde 解析失败走 if-let 静默分支）。
                if path.file_name().and_then(|e| e.to_str()) == Some(JobMetrics::FILE_NAME) {
                    continue;
                }
                if let Ok(raw) = fs::read_to_string(&path) {
                    if let Ok(mut rec) = serde_json::from_str::<JobRecord>(&raw) {
                        rec.status = normalize_job_status(&rec.status);
                        if is_active_job_status(&rec.status) {
                            active.insert(rec.run_id.clone(), rec);
                        }
                    }
                }
            }
        }
        // P10: 恢复跨重启累计 totals（peak/boot_at_unix 保持本进程作用域）。
        store.metrics.restore_totals(&store.data.jobs_dir());
        store
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    pub fn running_count(&self) -> usize {
        self.active
            .lock()
            .values()
            .filter(|j| normalize_job_status(&j.status) == "running")
            .count()
    }

    pub fn queued_count(&self) -> usize {
        self.active
            .lock()
            .values()
            .filter(|j| normalize_job_status(&j.status) == "queued")
            .count()
    }

    /// Chat / immediate-start path. Fails with RateLimited when at capacity (no queue).
    /// Preserves S4 mobile chat contract.
    pub fn try_start(
        &self,
        kind: &str,
        user_id: &str,
        workspace_id: &str,
        model: Option<String>,
        meta: Value,
    ) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        let running = active
            .values()
            .filter(|j| normalize_job_status(&j.status) == "running")
            .count();
        if running >= self.max_concurrent {
            return Err(CoreError::RateLimited(format!(
                "max concurrent jobs ({})",
                self.max_concurrent
            )));
        }
        let now = Utc::now();
        let run_id = Uuid::new_v4().to_string();
        let rec = JobRecord {
            run_id: run_id.clone(),
            kind: kind.into(),
            user_id: user_id.into(),
            workspace_id: workspace_id.into(),
            status: "running".into(),
            model,
            created_at: now,
            updated_at: now,
            meta,
            payload: None,
            progress: Some(0.0),
            progress_message: None,
            cursor: None,
            error: None,
            result: None,
            events: Vec::new(),
        };
        active.insert(run_id.clone(), rec.clone());
        let _ = self.persist(&rec);
        Ok(rec)
    }

    /// Jobs v2 create: queues when at concurrency limit (does NOT 429).
    /// Overflow policy: **queue** (`status=queued`), never reject.
    pub fn create(
        &self,
        kind: &str,
        user_id: &str,
        workspace_id: &str,
        payload: Value,
        model: Option<String>,
        meta: Option<Value>,
    ) -> CoreResult<JobRecord> {
        let kind = normalize_job_kind(kind);
        let mut active = self.active.lock();
        let running = active
            .values()
            .filter(|j| normalize_job_status(&j.status) == "running")
            .count();
        let status = if running < self.max_concurrent {
            "running"
        } else {
            "queued"
        };
        let now = Utc::now();
        let run_id = Uuid::new_v4().to_string();
        let rec = JobRecord {
            run_id: run_id.clone(),
            kind,
            user_id: user_id.into(),
            workspace_id: workspace_id.into(),
            status: status.into(),
            model,
            created_at: now,
            updated_at: now,
            meta: meta.unwrap_or_else(|| json!({})),
            payload: Some(payload),
            progress: Some(0.0),
            progress_message: Some(if status == "queued" {
                "queued".into()
            } else {
                "started".into()
            }),
            cursor: None,
            error: None,
            result: None,
            events: vec![JobEvent::event(
                if status == "queued" {
                    "job queued (at concurrency limit)"
                } else {
                    "job started"
                },
                None,
            )],
        };
        active.insert(run_id.clone(), rec.clone());
        self.metrics.total_created.fetch_add(1, Ordering::Relaxed);
        // P8: 记录并发水位峰值（含本 job）。
        let running_now = active
            .values()
            .filter(|j| normalize_job_status(&j.status) == "running")
            .count() as u64;
        self.metrics
            .peak_running
            .fetch_max(running_now, Ordering::Relaxed);
        self.persist(&rec)?;
        drop(active);
        // P10: totals 增量落盘（锁外，避免拉长 active 持锁时间）。
        self.metrics.persist(&self.data.jobs_dir());
        Ok(rec)
    }

    /// Finish a job (chat path). Accepts legacy done/error/stopped and maps them.
    /// Also promotes the oldest queued job to running when a slot frees.
    pub fn finish(&self, run_id: &str, status: &str) {
        let canonical = normalize_job_status(status);
        self.metrics.note_terminal(&canonical);
        let mut active = self.active.lock();
        if let Some(j) = active.get_mut(run_id) {
            j.status = canonical.clone();
            j.updated_at = Utc::now();
            if canonical == "succeeded" {
                j.progress = Some(1.0);
            }
            if canonical == "failed" && j.error.is_none() {
                j.error = Some("failed".into());
            }
            let _ = self.persist(j);
        } else if let Some(mut rec) = self.load_from_disk(run_id) {
            rec.status = canonical.clone();
            rec.updated_at = Utc::now();
            let _ = self.persist(&rec);
        }
        active.retain(|_, j| is_active_job_status(&j.status));
        drop(active);
        // P10: 终态计数落盘。
        self.metrics.persist(&self.data.jobs_dir());
        let _ = self.promote_queued();
    }

    /// Jobs v2 cancel — idempotent. Terminal jobs stay as-is; active → cancelled.
    pub fn cancel(&self, run_id: &str) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        if let Some(j) = active.get_mut(run_id) {
            let status = normalize_job_status(&j.status);
            if is_terminal_job_status(&status) {
                let rec = j.clone();
                return Ok(rec);
            }
            j.status = "cancelled".into();
            j.updated_at = Utc::now();
            j.progress_message = Some("cancelled".into());
            j.events.push(JobEvent::event("cancelled", None));
            trim_events_with_delta_tail(&mut j.events);
            self.metrics.note_terminal("cancelled");
            let rec = j.clone();
            let _ = self.persist(&rec);
            active.retain(|_, j| is_active_job_status(&j.status));
            drop(active);
            // P10: 终态计数落盘。
            self.metrics.persist(&self.data.jobs_dir());
            let _ = self.promote_queued();
            return Ok(rec);
        }
        drop(active);
        if let Some(mut rec) = self.load_from_disk(run_id) {
            if is_terminal_job_status(&rec.status) {
                return Ok(rec);
            }
            rec.status = "cancelled".into();
            rec.updated_at = Utc::now();
            rec.progress_message = Some("cancelled".into());
            rec.events.push(JobEvent::event("cancelled", None));
            trim_events_with_delta_tail(&mut rec.events);
            self.metrics.note_terminal("cancelled");
            self.persist(&rec)?;
            // P10: 终态计数落盘（磁盘分支，无锁持有）。
            self.metrics.persist(&self.data.jobs_dir());
            let _ = self.promote_queued();
            return Ok(rec);
        }
        Err(CoreError::NotFound(format!("job {run_id}")))
    }

    /// Write a non-destructive control request into `payload.control` (pause/resume).
    ///
    /// Unlike `cancel` (which flips status to a terminal state immediately), pause keeps the
    /// job `running` in the active set so its exec can pause at the next checkpoint and resume
    /// from disk. Cancel is still expressed via `cancel()` + status check in the exec loop.
    /// Returns the (possibly updated) job record.
    pub fn control(&self, run_id: &str, action: &str) -> CoreResult<JobRecord> {
        let action = match action {
            "pause" | "resume" => action,
            other => {
                return Err(CoreError::BadRequest(format!(
                    "unsupported control action: {other} (pause|resume)"
                )))
            }
        };
        let mut active = self.active.lock();
        let mut rec = if let Some(j) = active.get(run_id).cloned() {
            j
        } else if let Some(j) = self.load_from_disk(run_id) {
            j
        } else {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        };
        if is_terminal_job_status(&rec.status) {
            return Err(CoreError::BadRequest(format!(
                "job {run_id} is {}; cannot {}",
                normalize_job_status(&rec.status),
                action
            )));
        }
        let mut payload = rec.payload.unwrap_or_else(|| json!({}));
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "control".into(),
                json!({
                    "action": action,
                    "at": Utc::now().to_rfc3339(),
                }),
            );
        } else {
            payload = json!({ "control": { "action": action } });
        }
        rec.payload = Some(payload);
        rec.updated_at = Utc::now();
        rec.progress_message = Some(match action {
            "pause" => "pause 请求已发出，等待任务在检查点暂停…".into(),
            _ => "resume 请求已发出…".into(),
        });
        rec.events.push(JobEvent::event(
            format!("control: {action} requested"),
            None,
        ));
        trim_events_with_delta_tail(&mut rec.events);
        if is_active_job_status(&rec.status) {
            active.insert(rec.run_id.clone(), rec.clone());
        }
        self.persist(&rec)?;
        Ok(rec)
    }

    /// Read the control request (pause/resume) stored in `payload.control` for an exec loop.
    pub fn control_action(&self, run_id: &str) -> Option<String> {
        let rec = self.get(run_id)?;
        let action = rec
            .payload
            .as_ref()
            .and_then(|p| p.get("control"))
            .and_then(|c| c.get("action"))
            .and_then(|a| a.as_str())?;
        Some(action.to_string())
    }

    /// Clear the control request once an exec has processed it (e.g. after resume).
    pub fn clear_control(&self, run_id: &str) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        let mut rec = if let Some(j) = active.get(run_id).cloned() {
            j
        } else if let Some(j) = self.load_from_disk(run_id) {
            j
        } else {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        };
        if let Some(obj) = rec.payload.as_mut().and_then(|p| p.as_object_mut()) {
            obj.remove("control");
        }
        rec.updated_at = Utc::now();
        if is_active_job_status(&rec.status) {
            active.insert(rec.run_id.clone(), rec.clone());
        }
        self.persist(&rec)?;
        Ok(rec)
    }

    pub fn get(&self, run_id: &str) -> Option<JobRecord> {
        if let Some(j) = self.active.lock().get(run_id).cloned() {
            return Some(j);
        }
        self.load_from_disk(run_id)
    }

    /// P2: 按 id 物理删除一条终态 job（active 索引 + 磁盘 JSON）。
    ///
    /// 安全保护：只允许删除**终态** job（succeeded/failed/cancelled）。若目标仍在
    /// `active` 集合且非终态（running/queued/paused），返回 `BadRequest`，防止误删
    /// 正在执行或排队中的任务。删除是物理性的——文件从磁盘移除后无法通过 `get`
    /// 找回，也不会再出现在 `list` 中。jobs 目录的批量增长整理见 `prune_terminal`。
    pub fn delete(&self, run_id: &str) -> CoreResult<()> {
        let mut active = self.active.lock();
        let disk_exists = self.data.jobs_dir().join(format!("{run_id}.json")).exists();
        let in_active = active.contains_key(run_id);
        if !in_active && !disk_exists {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        }
        if let Some(j) = active.get(run_id) {
            if !is_terminal_job_status(&normalize_job_status(&j.status)) {
                return Err(CoreError::BadRequest(format!(
                    "job {run_id} is {}; only terminal jobs can be deleted",
                    normalize_job_status(&j.status)
                )));
            }
        }
        // 先从内存 active 索引移除，再删磁盘文件；文件不存在时静默忽略（rm 语义）。
        active.retain(|id, _| id != run_id);
        drop(active);
        if let Ok(err) = fs::remove_file(self.data.jobs_dir().join(format!("{run_id}.json"))) {
            // remove_file Ok(路径) —— 实际返回 io::Result<()>；类型签名见下方
            let _ = err;
        }
        Ok(())
    }


    /// List jobs (memory active + disk), newest first. Filters optional.
    pub fn list(&self, filter: JobListFilter) -> CoreResult<Vec<JobRecord>> {
        let limit = if filter.limit == 0 {
            50
        } else {
            filter.limit.min(500)
        };
        let mut by_id: HashMap<String, JobRecord> = HashMap::new();

        // Disk first, then overlay active (fresher).
        if let Ok(entries) = fs::read_dir(self.data.jobs_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(raw) = fs::read_to_string(&path) {
                    if let Ok(mut rec) = serde_json::from_str::<JobRecord>(&raw) {
                        rec.status = normalize_job_status(&rec.status);
                        by_id.insert(rec.run_id.clone(), rec);
                    }
                }
            }
        }
        for (id, rec) in self.active.lock().iter() {
            by_id.insert(id.clone(), rec.clone());
        }

        let mut items: Vec<JobRecord> = by_id.into_values().collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let status_filter = filter
            .status
            .as_ref()
            .map(|s| normalize_job_status(s));
        items.retain(|j| {
            if let Some(ref st) = status_filter {
                if normalize_job_status(&j.status) != *st {
                    return false;
                }
            }
            if let Some(ref k) = filter.kind {
                if &j.kind != k {
                    return false;
                }
            }
            if let Some(ref u) = filter.user_id {
                if &j.user_id != u {
                    return false;
                }
            }
            if let Some(ref w) = filter.workspace_id {
                if &j.workspace_id != w {
                    return false;
                }
            }
            true
        });
        items.truncate(limit);
        Ok(items)
    }

    /// Update progress / cursor / custom event (for long jobs + SSE).
    pub fn push_event(
        &self,
        run_id: &str,
        event: JobEvent,
        progress: Option<f64>,
        cursor: Option<String>,
    ) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        if let Some(j) = active.get_mut(run_id) {
            if let Some(p) = progress {
                j.progress = Some(p.clamp(0.0, 1.0));
            }
            if let Some(msg) = event.message.clone() {
                j.progress_message = Some(msg);
            }
            if let Some(c) = cursor {
                j.cursor = Some(c);
            }
            j.updated_at = Utc::now();
            j.events.push(event);
            trim_events_with_delta_tail(&mut j.events);
            let rec = j.clone();
            self.persist(&rec)?;
            return Ok(rec);
        }
        drop(active);
        if let Some(mut rec) = self.load_from_disk(run_id) {
            if let Some(p) = progress {
                rec.progress = Some(p.clamp(0.0, 1.0));
            }
            if let Some(msg) = event.message.clone() {
                rec.progress_message = Some(msg);
            }
            if let Some(c) = cursor {
                rec.cursor = Some(c);
            }
            rec.updated_at = Utc::now();
            rec.events.push(event);
            trim_events_with_delta_tail(&mut rec.events);
            if is_active_job_status(&rec.status) {
                self.active.lock().insert(rec.run_id.clone(), rec.clone());
            }
            self.persist(&rec)?;
            return Ok(rec);
        }
        Err(CoreError::NotFound(format!("job {run_id}")))
    }

    /// Complete a jobs-v2 job with optional result payload.
    pub fn complete(
        &self,
        run_id: &str,
        status: &str,
        result: Option<Value>,
        error: Option<String>,
    ) -> CoreResult<JobRecord> {
        let canonical = normalize_job_status(status);
        if !is_terminal_job_status(&canonical) {
            return Err(CoreError::BadRequest(format!(
                "complete requires terminal status, got {canonical}"
            )));
        }
        let mut active = self.active.lock();
        let mut rec = if let Some(j) = active.remove(run_id) {
            j
        } else if let Some(j) = self.load_from_disk(run_id) {
            j
        } else {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        };
        // Cancel wins: a late complete(succeeded) after stop must not resurrect the job.
        let prior = normalize_job_status(&rec.status);
        if prior == "cancelled" && canonical != "cancelled" {
            rec.updated_at = Utc::now();
            rec.events.push(JobEvent::event(
                format!("complete ignored (already cancelled; wanted {canonical})"),
                None,
            ));
            trim_events_with_delta_tail(&mut rec.events);
            self.persist(&rec)?;
            drop(active);
            self.metrics.persist(&self.data.jobs_dir());
            let _ = self.promote_queued();
            return Ok(rec);
        }
        if is_terminal_job_status(&prior) && prior != canonical {
            // Keep first terminal status (failed stays failed, etc.)
            rec.updated_at = Utc::now();
            rec.events.push(JobEvent::event(
                format!("complete ignored (already {prior}; wanted {canonical})"),
                None,
            ));
            trim_events_with_delta_tail(&mut rec.events);
            self.persist(&rec)?;
            drop(active);
            self.metrics.persist(&self.data.jobs_dir());
            let _ = self.promote_queued();
            return Ok(rec);
        }
        rec.status = canonical.clone();
        rec.updated_at = Utc::now();
        // P10: complete() 是 director-plan 等后台任务的正式终态路径 —— P8 漏了此处计数，
        // 导致 /health totals 漏记该类 job（冒烟发现：director_plan succeeded 后 total_succeeded=0）。
        // 注意：必须放在「cancel wins / already-terminal」早退之后 —— 迟到/重复的 complete
        // 不复活记录、也不重复计数。
        self.metrics.note_terminal(&canonical);
        if let Some(r) = result {
            rec.result = Some(r);
        }
        if let Some(e) = error {
            rec.error = Some(e.clone());
            rec.events.push(JobEvent::error(e));
        } else if canonical == "succeeded" {
            rec.progress = Some(1.0);
            rec.events.push(JobEvent::done(Some("succeeded".into())));
        } else if canonical == "cancelled" {
            rec.events.push(JobEvent::event("cancelled", None));
        }
        trim_events_with_delta_tail(&mut rec.events);
        self.persist(&rec)?;
        drop(active);
        // P10: 终态计数落盘。
        self.metrics.persist(&self.data.jobs_dir());
        let _ = self.promote_queued();
        Ok(rec)
    }

    /// Promote oldest queued job(s) into running until at capacity.
    /// Returns newly promoted jobs (callers may start workers).
    pub fn promote_queued(&self) -> CoreResult<Vec<JobRecord>> {
        let mut active = self.active.lock();
        let mut promoted = Vec::new();
        loop {
            let running = active
                .values()
                .filter(|j| normalize_job_status(&j.status) == "running")
                .count();
            if running >= self.max_concurrent {
                break;
            }
            let next_id = active
                .values()
                .filter(|j| normalize_job_status(&j.status) == "queued")
                .min_by_key(|j| j.created_at)
                .map(|j| j.run_id.clone());
            let Some(id) = next_id else {
                break;
            };
            if let Some(j) = active.get_mut(&id) {
                j.status = "running".into();
                j.updated_at = Utc::now();
                j.progress_message = Some("started".into());
                j.events
                    .push(JobEvent::event("promoted from queue", None));
                trim_events_with_delta_tail(&mut j.events);
                let rec = j.clone();
                let _ = self.persist(&rec);
                promoted.push(rec);
            } else {
                break;
            }
        }
        Ok(promoted)
    }

    /// Invoke the registered hook for every currently active non-terminal job.
    /// Called once after startup wiring completes (AppState has built its worker closures).
    pub fn dispatch_recovered(&self) {
        let recs: Vec<JobRecord> = self
            .active
            .lock()
            .values()
            .filter(|j| is_active_job_status(&j.status))
            .cloned()
            .collect();
        for rec in &recs {
            (self.recover_hook)(rec);
        }
    }

    /// Register the restart-resume callback (set once during AppState construction,
    /// before `dispatch_recovered` is called). Default is a no-op.
    pub fn set_recover_hook(&mut self, hook: Arc<dyn Fn(&JobRecord) + Send + Sync>) {
        self.recover_hook = hook;
    }

    /// After process restart, re-mark a disk-orphaned active job as `queued` so it no
    /// longer counts against the running concurrency ceiling, and append a resume notice
    /// event. The caller then re-schedules the actual worker.
    pub fn rearm_interrupted(&self, run_id: &str, notice: &str) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        let mut rec = if let Some(j) = active.get(run_id).cloned() {
            j
        } else if let Some(j) = self.load_from_disk(run_id) {
            j
        } else {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        };
        if is_terminal_job_status(&rec.status) {
            active.insert(rec.run_id.clone(), rec.clone());
            return Err(CoreError::BadRequest(
                "job already terminal; nothing to resume".into(),
            ));
        }
        rec.status = "queued".into();
        rec.updated_at = Utc::now();
        rec.progress_message = Some(notice.into());
        rec.events.push(JobEvent::event(notice, None));
        trim_events_with_delta_tail(&mut rec.events);
        active.insert(rec.run_id.clone(), rec.clone());
        self.persist(&rec)?;
        Ok(rec)
    }

    fn load_from_disk(&self, run_id: &str) -> Option<JobRecord> {
        let path = self.data.jobs_dir().join(format!("{run_id}.json"));
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<JobRecord>(&s).ok())
            .map(|mut rec| {
                rec.status = normalize_job_status(&rec.status);
                rec
            })
    }


    /// Persist pipeline checkpoint into `payload.checkpoint` + optional cursor/progress.
    /// Used by Background (W1+) and similar multi-stage jobs for resume-after-restart.
    pub fn set_checkpoint(
        &self,
        run_id: &str,
        checkpoint: Value,
        cursor: Option<String>,
        progress: Option<f64>,
        progress_message: Option<String>,
    ) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        let mut rec = if let Some(j) = active.get(run_id).cloned() {
            j
        } else if let Some(j) = self.load_from_disk(run_id) {
            j
        } else {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        };
        let mut payload = rec.payload.unwrap_or_else(|| json!({}));
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("checkpoint".into(), checkpoint);
        } else {
            payload = json!({ "checkpoint": checkpoint });
        }
        rec.payload = Some(payload);
        if let Some(c) = cursor {
            rec.cursor = Some(c);
        }
        if let Some(p) = progress {
            rec.progress = Some(p.clamp(0.0, 1.0));
        }
        if let Some(m) = progress_message {
            rec.progress_message = Some(m);
        }
        rec.updated_at = Utc::now();
        if is_active_job_status(&rec.status) {
            active.insert(rec.run_id.clone(), rec.clone());
        }
        self.persist(&rec)?;
        Ok(rec)
    }

    /// Merge keys into existing job.payload (top-level). Creates payload object if missing.
    pub fn merge_job_payload(
        &self,
        run_id: &str,
        extra: Value,
    ) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        let mut rec = if let Some(j) = active.get(run_id).cloned() {
            j
        } else if let Some(j) = self.load_from_disk(run_id) {
            j
        } else {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        };
        let mut payload = rec.payload.unwrap_or_else(|| json!({}));
        if let Some(extra_obj) = extra.as_object() {
            if let Some(obj) = payload.as_object_mut() {
                for (k, v) in extra_obj {
                    obj.insert(k.clone(), v.clone());
                }
            } else {
                payload = extra;
            }
        }
        rec.payload = Some(payload);
        rec.updated_at = Utc::now();
        if is_active_job_status(&rec.status) {
            active.insert(rec.run_id.clone(), rec.clone());
        } else if active.contains_key(run_id) {
            active.insert(rec.run_id.clone(), rec.clone());
        }
        // Always keep disk in sync; also re-insert cancelled jobs into active only if rearm later.
        if active.contains_key(&rec.run_id) {
            active.insert(rec.run_id.clone(), rec.clone());
        }
        self.persist(&rec)?;
        Ok(rec)
    }

    /// Force a non-terminal status (e.g. resume: cancelled/failed/orphaned-running → running).
    pub fn rearm_running(&self, run_id: &str) -> CoreResult<JobRecord> {
        let mut active = self.active.lock();
        let mut rec = if let Some(j) = active.remove(run_id) {
            j
        } else if let Some(j) = self.load_from_disk(run_id) {
            j
        } else {
            return Err(CoreError::NotFound(format!("job {run_id}")));
        };
        let prior = normalize_job_status(&rec.status);
        if prior == "succeeded" {
            active.insert(rec.run_id.clone(), rec.clone());
            return Err(CoreError::BadRequest(
                "job already succeeded; nothing to resume".into(),
            ));
        }
        // If genuinely running in-memory with a live worker, caller should 409 — we still allow
        // rearm for disk-orphaned running after process restart.
        rec.status = "running".into();
        rec.updated_at = Utc::now();
        rec.error = None;
        rec.progress_message = Some("resuming".into());
        rec.events.push(JobEvent::event("job rearmed for resume", None));
        trim_events_with_delta_tail(&mut rec.events);
        active.insert(rec.run_id.clone(), rec.clone());
        self.persist(&rec)?;
        Ok(rec)
    }

    fn persist(&self, rec: &JobRecord) -> CoreResult<()> {
        let path = self.data.jobs_dir().join(format!("{}.json", rec.run_id));
        fs::write(path, serde_json::to_string_pretty(rec)?)?;
        Ok(())
    }

    /// P0-3(审计): 裁剪终态 job 文件——jobs 目录无界增长修复。
    /// 保留最新 `keep` 个文件（按 mtime），删除更旧的；active 内存索引不受影响。
    /// 在服务启动时调用一次（main.rs），也可按需调用。
    pub fn prune_terminal(&self, keep: usize) {
        let dir = self.data.jobs_dir();
        let Ok(entries) = fs::read_dir(&dir) else { return };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            // P10: 指标快照常驻 jobs 目录，不参与 LRU 裁剪。
            .filter(|e| e.file_name() != JobMetrics::FILE_NAME)
            .filter_map(|e| {
                let m = e.metadata().ok()?.modified().ok()?;
                Some((m, e.path()))
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0)); // 最新在前
        if files.len() > keep {
            for (_, path) in files.into_iter().skip(keep) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn normalize_job_kind(kind: &str) -> String {
    let k = kind.trim().to_ascii_lowercase().replace('-', "_");
    match k.as_str() {
        "background" | "book_travel" | "outline" | "agent" | "chat" | "other" | "noop"
        | "test" => k,
        "booktravel" => "book_travel".into(),
        "" => "other".into(),
        _ => {
            // Allow free-form kinds but keep known aliases; unknown stays as given (trimmed).
            if k.is_empty() {
                "other".into()
            } else {
                k
            }
        }
    }
}

/// 旧版纯容量截断（已被 trim_events_with_delta_tail 取代；保留供单测对照）。
#[cfg_attr(not(test), allow(dead_code))]
fn trim_events(events: &mut Vec<JobEvent>) {
    if events.len() > JOB_EVENTS_CAP {
        let drain = events.len() - JOB_EVENTS_CAP;
        events.drain(0..drain);
    }
}

/// P13: delta 事件不占 JOB_EVENTS_CAP 名额——流式 token 会瞬间挤掉
/// started/progress 等关键事件（实测 book-travel assemble 62 个空 delta 全部
/// 覆盖了 "job started"）。delta 只保留最近 DELTA_TAIL_CAP 条供 SSE 补发。
const DELTA_TAIL_CAP: usize = 16;

fn trim_events_with_delta_tail(events: &mut Vec<JobEvent>) {
    // 先按常规规则裁剪，再从被裁掉的头部回收最近的 delta 到尾部保留窗。
    if events.len() <= JOB_EVENTS_CAP {
        return;
    }
    let drain = events.len() - JOB_EVENTS_CAP;
    let mut kept_deltas: Vec<JobEvent> = events[..drain]
        .iter()
        .filter(|e| e.event_type == "delta")
        .cloned()
        .collect();
    if kept_deltas.len() > DELTA_TAIL_CAP {
        let skip = kept_deltas.len() - DELTA_TAIL_CAP;
        kept_deltas.drain(0..skip);
    }
    events.drain(0..drain);
    // 尾插保留的 delta（保持时间序：老 delta 在新非 delta 之前）。
    for ev in kept_deltas {
        events.insert(0, ev);
    }
}

// --- Partner (world books + character cards) ---

#[derive(Debug, Clone)]
pub struct GenerationPromptResult {
    pub system_prompt: String,
    pub wi: crate::WiScanResult,
    pub regex_script_count: usize,
    /// Updated timed effects to persist for this chat (if any).
    pub timed_world_info: Option<crate::TimedWorldInfo>,
    /// Extra OpenAI-style messages to splice into the chat (EM/depth/outlet slots).
    pub message_injections: Vec<serde_json::Value>,
    /// automationId from activated WI entries.
    pub automation_ids: Vec<String>,
    /// EM example dialogue pairs (already role-split).
    pub example_messages: Vec<crate::WiExampleMessage>,
}



#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartnerItem {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub item_type: String, // world_book | character_card
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_book_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartnerState {
    #[serde(default)]
    pub world_books: Vec<PartnerItem>,
    #[serde(default)]
    pub character_cards: Vec<PartnerItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_world_book_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_character_card_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_type: Option<String>,
}

impl Default for PartnerState {
    fn default() -> Self {
        Self {
            world_books: Vec::new(),
            character_cards: Vec::new(),
            selected_world_book_id: None,
            selected_character_card_id: None,
            selected_id: None,
            selected_type: None,
        }
    }
}

/// Minimal markdown compile from fields (subset of upstream compileItemToMarkdown).
pub fn compile_partner_markdown(name: &str, item_type: &str, fields: &Value) -> String {
    let get = |k: &str| -> String {
        fields
            .get(k)
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else if let Some(a) = v.as_array() {
                    Some(
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                } else {
                    v.as_i64().map(|n| n.to_string())
                }
            })
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let mut out = String::new();
    if item_type == "world_book" {
        out.push_str(&format!("# {name}\n\n"));
        let mut lines = Vec::new();
        for (label, key) in [
            ("主题", "theme"),
            ("时代", "era"),
            ("科技水平", "techLevel"),
            ("魔法水平", "magicLevel"),
        ] {
            let v = get(key);
            if !v.is_empty() {
                lines.push(format!("- **{label}**: {v}"));
            }
        }
        if !lines.is_empty() {
            out.push_str("## 核心设定\n");
            out.push_str(&lines.join("\n"));
            out.push_str("\n\n");
        }
        for (label, key) in [
            ("地理格局", "geography"),
            ("关键场景", "keyScenes"),
            ("文化特色", "culturalFeatures"),
            ("历史事件", "history"),
            ("核心矛盾", "conflict"),
        ] {
            let v = get(key);
            if !v.is_empty() {
                out.push_str(&format!("## {label}\n{v}\n\n"));
            }
        }
    } else {
        out.push_str(&format!("# 角色卡：{name}\n\n"));
        let mut lines = Vec::new();
        for (label, key) in [
            ("姓名", "name"),
            ("年龄", "age"),
            ("性别", "gender"),
            ("种族", "race"),
            ("出生地", "birthplace"),
            ("职业", "occupation"),
            ("社会阶层", "socialClass"),
        ] {
            let v = if key == "name" && get(key).is_empty() {
                name.to_string()
            } else {
                get(key)
            };
            if !v.is_empty() {
                lines.push(format!("- **{label}**: {v}"));
            }
        }
        if !lines.is_empty() {
            out.push_str("## 基础信息\n");
            out.push_str(&lines.join("\n"));
            out.push_str("\n\n");
        }
        let tags = get("identityTags");
        if !tags.is_empty() {
            out.push_str(&format!("## 身份标签\n{tags}\n\n"));
        }
        for (section, keys) in [
            (
                "外貌气质",
                &[
                    ("身高体型", "heightBuild"),
                    ("标志性特征", "iconicFeatures"),
                    ("衣着风格", "clothingStyle"),
                    ("整体气质", "overallVibe"),
                ][..],
            ),
            (
                "性格特征",
                &[
                    ("外在性格", "externalPersonality"),
                    ("内在性格", "internalPersonality"),
                    ("核心欲望", "coreDesire"),
                    ("恐惧和弱点", "fearWeakness"),
                    ("道德观念", "moralValues"),
                    ("怪癖", "quirk"),
                ][..],
            ),
            (
                "角色记忆",
                &[
                    ("与用户关系类型", "userRelationType"),
                    ("与用户相处模式", "userInteractionModel"),
                    ("与用户关系底线", "userRelationBottomLine"),
                ][..],
            ),
        ] {
            let mut sl = Vec::new();
            for (label, key) in keys {
                let v = get(key);
                if !v.is_empty() {
                    sl.push(format!("- **{label}**: {v}"));
                }
            }
            if !sl.is_empty() {
                out.push_str(&format!("## {section}\n"));
                out.push_str(&sl.join("\n"));
                out.push_str("\n\n");
            }
        }
        for (label, key) in [
            ("技能专长", "skills"),
            ("背景故事", "backgroundStory"),
            ("人际关系", "relationships"),
            ("说话方式", "speakingStyle"),
            ("典型反应", "typicalReactions"),
            ("关键事件", "keyEvents"),
        ] {
            let v = get(key);
            if !v.is_empty() {
                out.push_str(&format!("## {label}\n{v}\n\n"));
            }
        }
    }
    out.trim().to_string() + "\n"
}

/// C2 审计修复：sanitize 一个 user_id 用于作为存储分区后缀。
/// 仅允许 ASCII alnum、`-`、`_`，使 id 不可能引入路径分隔符或穿越出 config 目录。
pub fn validate_user_scope(user_id: &str) -> String {
    let mut out = String::with_capacity(user_id.len());
    for c in user_id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("anonymous");
    }
    out
}

#[derive(Clone)]
pub struct PartnerStore {
    app_state: AppStateStore,
    /// C2 审计修复：per-user partition。设置时所有读写走
    /// `partner-store-{user_scope}.json` 而非共享全局文件。
    user_scope: Option<String>,
}

impl PartnerStore {
    pub fn new(app_state: AppStateStore) -> Self {
        Self {
            app_state,
            user_scope: None,
        }
    }

    /// C2：返回绑定到 `user_id` 的 per-user scoped 副本。
    /// Handler 必须用认证会话的 user_id 调用本方法，防止跨租户读写。
    pub fn scoped(&self, user_id: &str) -> PartnerStore {
        PartnerStore {
            app_state: self.app_state.clone(),
            user_scope: Some(validate_user_scope(user_id)),
        }
    }

    fn store_name(&self) -> String {
        match &self.user_scope {
            Some(u) => format!("partner-store-{u}"),
            None => "partner-store".into(),
        }
    }

    pub fn load(&self) -> CoreResult<PartnerState> {
        let raw = self.app_state.load(&self.store_name())?;
        let v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
        let state = v.get("state").cloned().unwrap_or(v);
        let mut ps: PartnerState = serde_json::from_value(state).unwrap_or_default();
        // ensure content compiled if empty but fields present
        for item in ps.world_books.iter_mut().chain(ps.character_cards.iter_mut()) {
            if item.content.trim().is_empty() {
                if let Some(fields) = &item.fields {
                    item.content =
                        compile_partner_markdown(&item.name, &item.item_type, fields);
                }
            }
        }
        Ok(ps)
    }

    pub fn save(&self, mut state: PartnerState) -> CoreResult<PartnerState> {
        for item in state.world_books.iter_mut() {
            item.item_type = "world_book".into();
            if let Some(fields) = &item.fields {
                if item.content.trim().is_empty() {
                    item.content = compile_partner_markdown(&item.name, "world_book", fields);
                }
            }
        }
        for item in state.character_cards.iter_mut() {
            item.item_type = "character_card".into();
            if let Some(fields) = &item.fields {
                if item.content.trim().is_empty() {
                    item.content =
                        compile_partner_markdown(&item.name, "character_card", fields);
                }
            }
        }
        let payload = json!({ "state": state });
        self.app_state
            .save(&self.store_name(), &serde_json::to_string_pretty(&payload)?)?;
        Ok(state)
    }

    pub fn upsert_world_book(&self, mut item: PartnerItem) -> CoreResult<PartnerItem> {
        let mut st = self.load()?;
        item.item_type = "world_book".into();
        if item.id.is_empty() {
            item.id = format!("wb-{}", Uuid::new_v4());
        }
        // Preserve caller-built content (e.g. ST character_book compile). Only
        // synthesize markdown from fields when content is empty.
        if item.content.trim().is_empty() {
            if let Some(fields) = &item.fields {
                item.content = compile_partner_markdown(&item.name, "world_book", fields);
            }
        }
        if let Some(slot) = st.world_books.iter_mut().find(|x| x.id == item.id) {
            *slot = item.clone();
        } else {
            st.world_books.push(item.clone());
        }
        self.save(st)?;
        Ok(item)
    }

    pub fn upsert_character_card(&self, mut item: PartnerItem) -> CoreResult<PartnerItem> {
        let mut st = self.load()?;
        item.item_type = "character_card".into();
        if item.id.is_empty() {
            item.id = format!("cc-{}", Uuid::new_v4());
        }
        if let Some(fields) = &item.fields {
            item.content = compile_partner_markdown(&item.name, "character_card", fields);
        }
        if let Some(slot) = st.character_cards.iter_mut().find(|x| x.id == item.id) {
            *slot = item.clone();
        } else {
            st.character_cards.push(item.clone());
        }
        self.save(st)?;
        Ok(item)
    }

    pub fn delete_world_book(&self, id: &str, cascade: bool) -> CoreResult<()> {
        let mut st = self.load()?;
        st.world_books.retain(|x| x.id != id);
        if cascade {
            st.character_cards.retain(|x| x.world_book_id.as_deref() != Some(id));
        } else {
            for c in st.character_cards.iter_mut() {
                if c.world_book_id.as_deref() == Some(id) {
                    c.world_book_id = None;
                }
            }
        }
        if st.selected_world_book_id.as_deref() == Some(id) {
            st.selected_world_book_id = None;
        }
        self.save(st)?;
        Ok(())
    }


    pub fn get_world_book(&self, id: &str) -> CoreResult<PartnerItem> {
        let st = self.load()?;
        st.world_books
            .into_iter()
            .find(|w| w.id == id)
            .ok_or_else(|| CoreError::NotFound(format!("world book {id}")))
    }

    /// List structured WI entries for a world book (ST JSON objects).
    pub fn list_world_book_entries(&self, id: &str) -> CoreResult<Vec<Value>> {
        let wb = self.get_world_book(id)?;
        Ok(crate::entry_values_from_world_book(
            &wb.name,
            wb.fields.as_ref(),
            &wb.content,
        ))
    }

    /// Replace entire entry set; writes fields.stBookRaw + fields.stEntries + content.
    pub fn put_world_book_entries(&self, id: &str, entries: Vec<Value>) -> CoreResult<PartnerItem> {
        let mut st = self.load()?;
        let slot = st
            .world_books
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or_else(|| CoreError::NotFound(format!("world book {id}")))?;
        let world = slot.name.clone();
        let mut parsed = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            let mut e = e.clone();
            // ensure uid
            if e.get("uid").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                && e.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty()
            {
                if let Some(obj) = e.as_object_mut() {
                    let uid = format!("{world}-{i}-{}", Uuid::new_v4());
                    obj.insert("uid".into(), json!(uid));
                    obj.insert("id".into(), json!(uid));
                }
            }
            if let Some(ent) = crate::parse_wi_entry(&e, &world, i) {
                parsed.push(ent);
            } else {
                // keep raw if parse fails but has content/keys
                parsed.push(
                    crate::parse_wi_entry(
                        &json!({
                            "uid": e.get("uid").or_else(|| e.get("id")).cloned().unwrap_or(json!(format!("{i}"))),
                            "keys": e.get("keys").or_else(|| e.get("key")).cloned().unwrap_or(json!([])),
                            "content": e.get("content").cloned().unwrap_or(json!("")),
                            "constant": e.get("constant").cloned().unwrap_or(json!(false)),
                            "comment": e.get("comment").cloned().unwrap_or(json!("")),
                        }),
                        &world,
                        i,
                    )
                    .ok_or_else(|| CoreError::BadRequest(format!("invalid entry at index {i}")))?,
                );
            }
        }
        let raw = crate::st_book_raw_from_entries(&world, &parsed);
        let st_entries: Vec<Value> = parsed.iter().map(crate::wi_entry_to_st_json).collect();
        let mut fields = slot.fields.clone().unwrap_or_else(|| json!({}));
        if let Some(obj) = fields.as_object_mut() {
            obj.insert("stBookRaw".into(), raw);
            obj.insert("stEntries".into(), json!(st_entries));
            // drop ambiguous plain entries map to avoid dual-source drift
            obj.remove("entries");
        } else {
            fields = json!({ "stBookRaw": raw, "stEntries": st_entries });
        }
        slot.fields = Some(fields);
        slot.content = crate::content_from_wi_entries(&world, &parsed);
        let out = slot.clone();
        self.save(st)?;
        Ok(out)
    }

    pub fn create_world_book_entry(&self, id: &str, entry: Value) -> CoreResult<Value> {
        let mut list = self.list_world_book_entries(id)?;
        let world = self.get_world_book(id)?.name;
        let mut entry = entry;
        let uid = entry
            .get("uid")
            .or_else(|| entry.get("id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("e-{}", Uuid::new_v4()));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("uid".into(), json!(uid));
            obj.insert("id".into(), json!(uid));
        }
        // reject dup
        if list.iter().any(|e| {
            e.get("uid").and_then(|v| v.as_str()) == Some(uid.as_str())
                || e.get("id").and_then(|v| v.as_str()) == Some(uid.as_str())
        }) {
            return Err(CoreError::BadRequest(format!("entry uid already exists: {uid}")));
        }
        // validate parseable
        let _ = crate::parse_wi_entry(&entry, &world, list.len())
            .ok_or_else(|| CoreError::BadRequest("invalid entry".into()))?;
        list.push(entry.clone());
        self.put_world_book_entries(id, list)?;
        Ok(entry)
    }

    pub fn patch_world_book_entry(
        &self,
        id: &str,
        entry_id: &str,
        patch: Value,
    ) -> CoreResult<Value> {
        let mut list = self.list_world_book_entries(id)?;
        let pos = list.iter().position(|e| {
            e.get("uid").and_then(|v| v.as_str()) == Some(entry_id)
                || e.get("id").and_then(|v| v.as_str()) == Some(entry_id)
        });
        let Some(i) = pos else {
            return Err(CoreError::NotFound(format!("entry {entry_id}")));
        };
        let merged = crate::merge_wi_entry_value(&list[i], &patch);
        // keep uid stable unless patch renames
        let new_uid = merged
            .get("uid")
            .or_else(|| merged.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or(entry_id)
            .to_string();
        if new_uid != entry_id {
            if list.iter().enumerate().any(|(j, e)| {
                j != i
                    && (e.get("uid").and_then(|v| v.as_str()) == Some(new_uid.as_str())
                        || e.get("id").and_then(|v| v.as_str()) == Some(new_uid.as_str()))
            }) {
                return Err(CoreError::BadRequest(format!("entry uid already exists: {new_uid}")));
            }
        }
        list[i] = merged.clone();
        self.put_world_book_entries(id, list)?;
        Ok(merged)
    }

    pub fn delete_world_book_entry(&self, id: &str, entry_id: &str) -> CoreResult<()> {
        let mut list = self.list_world_book_entries(id)?;
        let before = list.len();
        list.retain(|e| {
            e.get("uid").and_then(|v| v.as_str()) != Some(entry_id)
                && e.get("id").and_then(|v| v.as_str()) != Some(entry_id)
        });
        if list.len() == before {
            return Err(CoreError::NotFound(format!("entry {entry_id}")));
        }
        self.put_world_book_entries(id, list)?;
        Ok(())
    }


    /// Rebuild `stBookRaw` / `stEntries` / content for a world book.
    ///
    /// Sources (via `entries_from_world_book`): existing stBookRaw / character_book /
    /// stEntries / fields.entries, else freeform `content` as one constant legacy entry.
    ///
    /// - `force=false`: if stBookRaw already has ≥1 parseable entry, re-normalize writeback only
    ///   (still ensures stEntries/content sync). Returns `alreadyHadRaw=true`.
    /// - `force=true`: always re-derive from current fields+content (may re-materialize legacy).
    pub fn rebuild_world_book_st_book(
        &self,
        id: &str,
        force: bool,
    ) -> CoreResult<(PartnerItem, Vec<Value>, bool)> {
        let wb = self.get_world_book(id)?;
        let had_raw = wb
            .fields
            .as_ref()
            .and_then(|f| f.get("stBookRaw"))
            .map(|b| {
                b.get("entries")
                    .map(|e| e.as_array().map(|a| !a.is_empty()).unwrap_or(e.as_object().map(|m| !m.is_empty()).unwrap_or(false)))
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        let entries = crate::entries_from_world_book(&wb.name, wb.fields.as_ref(), &wb.content);
        if entries.is_empty() {
            return Err(CoreError::BadRequest(format!(
                "world book {id} has no rebuildable entries (empty content and no ST raw)"
            )));
        }
        let values: Vec<Value> = entries.iter().map(crate::wi_entry_to_st_json).collect();
        // put_world_book_entries always writes stBookRaw/stEntries/content
        let item = self.put_world_book_entries(id, values.clone())?;
        let list = self.list_world_book_entries(id)?;
        Ok((item, list, had_raw && !force))
    }

    /// Rebuild ST book for a character card.
    ///
    /// Priority:
    /// 1. Linked `world_book_id` → rebuild that world book
    /// 2. Card fields carry `character_book` / `stBookRaw` → upsert linked world book then rebuild
    /// 3. Else if card has non-empty content and `create_from_content` → create constant-entry WB + link
    pub fn rebuild_character_card_st_book(
        &self,
        id: &str,
        force: bool,
        create_from_content: bool,
    ) -> CoreResult<(PartnerItem, PartnerItem, Vec<Value>, bool)> {
        let st = self.load()?;
        let cc = st
            .character_cards
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("character card {id}")))?;

        // 1) linked world book
        if let Some(ref wid) = cc.world_book_id {
            if st.world_books.iter().any(|w| &w.id == wid) {
                drop(st);
                let (wb, entries, already) = self.rebuild_world_book_st_book(wid, force)?;
                let cc = self
                    .load()?
                    .character_cards
                    .into_iter()
                    .find(|c| c.id == id)
                    .ok_or_else(|| CoreError::NotFound(format!("character card {id}")))?;
                return Ok((cc, wb, entries, already));
            }
        }

        // 2) embedded book on card fields
        let fields = cc.fields.clone().unwrap_or_else(|| json!({}));
        let embedded = fields
            .get("character_book")
            .or_else(|| fields.get("stBookRaw"))
            .or_else(|| fields.get("stCharacterBookRaw"))
            .cloned();
        if let Some(book) = embedded {
            // synthesize PartnerItem via temporary StCardData-like path
            let book_name = book
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{} · 角色世界书", cc.name));
            let mut wb_item = PartnerItem {
                id: String::new(),
                name: book_name.clone(),
                item_type: "world_book".into(),
                content: String::new(),
                fields: Some(json!({
                    "theme": book_name,
                    "stCharacterBook": true,
                    "stSourceCharacter": cc.name,
                    "stBookRaw": book,
                })),
                world_book_id: None,
            };
            // materialize content from entries
            let ents = crate::entries_from_world_book(
                &book_name,
                wb_item.fields.as_ref(),
                "",
            );
            wb_item.content = crate::content_from_wi_entries(&book_name, &ents);
            let saved = self.upsert_world_book(wb_item)?;
            // link card
            let mut st = self.load()?;
            if let Some(slot) = st.character_cards.iter_mut().find(|c| c.id == id) {
                slot.world_book_id = Some(saved.id.clone());
                if let Some(f) = slot.fields.get_or_insert_with(|| json!({})).as_object_mut() {
                    f.insert("stHasCharacterBook".into(), json!(true));
                    f.insert("stLoreEntryCount".into(), json!(ents.len()));
                }
            }
            self.save(st)?;
            let (wb, entries, already) = self.rebuild_world_book_st_book(&saved.id, force)?;
            let cc = self
                .load()?
                .character_cards
                .into_iter()
                .find(|c| c.id == id)
                .ok_or_else(|| CoreError::NotFound(format!("character card {id}")))?;
            return Ok((cc, wb, entries, already));
        }

        // 3) create from freeform card content
        if create_from_content && !cc.content.trim().is_empty() {
            let book_name = format!("{} · 迁移世界书", cc.name);
            let wb_item = PartnerItem {
                id: String::new(),
                name: book_name.clone(),
                item_type: "world_book".into(),
                content: cc.content.clone(),
                fields: Some(json!({
                    "theme": book_name,
                    "stMigratedFromCard": true,
                    "stSourceCharacter": cc.name,
                })),
                world_book_id: None,
            };
            let saved = self.upsert_world_book(wb_item)?;
            let mut st = self.load()?;
            if let Some(slot) = st.character_cards.iter_mut().find(|c| c.id == id) {
                slot.world_book_id = Some(saved.id.clone());
            }
            self.save(st)?;
            let (wb, entries, already) = self.rebuild_world_book_st_book(&saved.id, force)?;
            let cc = self
                .load()?
                .character_cards
                .into_iter()
                .find(|c| c.id == id)
                .ok_or_else(|| CoreError::NotFound(format!("character card {id}")))?;
            return Ok((cc, wb, entries, already));
        }

        Err(CoreError::BadRequest(format!(
            "character card {id} has no linked world book, embedded character_book, or rebuildable content"
        )))
    }

    /// Rebuild all world books. When `force=false`, still visits every book but
    /// `already=true` marks those that already had stBookRaw (re-normalized only).
    /// Returns vec of (world_book_id, entry_count, already_had_raw).
    pub fn migrate_legacy_world_books(
        &self,
        force: bool,
    ) -> CoreResult<Vec<(String, usize, bool)>> {
        let st = self.load()?;
        let ids: Vec<String> = st.world_books.iter().map(|w| w.id.clone()).collect();
        let mut out = Vec::new();
        for id in ids {
            match self.rebuild_world_book_st_book(&id, force) {
                Ok((_item, entries, already)) => out.push((id, entries.len(), already)),
                Err(_) => {}
            }
        }
        Ok(out)
    }

    pub fn delete_character_card(&self, id: &str) -> CoreResult<()> {
        let mut st = self.load()?;
        st.character_cards.retain(|x| x.id != id);
        if st.selected_character_card_id.as_deref() == Some(id) {
            st.selected_character_card_id = None;
        }
        self.save(st)?;
        Ok(())
    }

    pub fn select(
        &self,
        world_book_id: Option<String>,
        character_card_id: Option<String>,
    ) -> CoreResult<PartnerState> {
        let mut st = self.load()?;
        if let Some(ref id) = world_book_id {
            if id.is_empty() {
                st.selected_world_book_id = None;
            } else if st.world_books.iter().any(|w| &w.id == id) {
                st.selected_world_book_id = Some(id.clone());
            } else {
                return Err(CoreError::NotFound(format!("world book {id}")));
            }
        }
        if let Some(ref id) = character_card_id {
            if id.is_empty() {
                st.selected_character_card_id = None;
            } else if st.character_cards.iter().any(|c| &c.id == id) {
                st.selected_character_card_id = Some(id.clone());
            } else {
                return Err(CoreError::NotFound(format!("character card {id}")));
            }
        }
        self.save(st)
    }

    /// Build partner chat system prompt like MobileChat.tsx
    ///
    /// Legacy entry — no chat scan (constant entries + full legacy books only via
    /// empty chat buffer). Prefer [`Self::build_generation_prompt`].
    pub fn build_system_prompt(
        &self,
        base_prompt: &str,
        world_book_id: Option<&str>,
        character_card_id: Option<&str>,
    ) -> CoreResult<String> {
        self.build_generation_prompt(base_prompt, world_book_id, character_card_id, &[], None)
            .map(|r| r.system_prompt)
    }

    /// ST-aligned generation prompt: World Info scan + character card + regex (prompt path).
    pub fn build_generation_prompt(
        &self,
        base_prompt: &str,
        world_book_id: Option<&str>,
        character_card_id: Option<&str>,
        chat_messages_oldest_first: &[(String, String)],
        wi_settings: Option<crate::WiSettings>,
    ) -> CoreResult<GenerationPromptResult> {
        self.build_generation_prompt_ex(
            base_prompt,
            world_book_id,
            character_card_id,
            chat_messages_oldest_first,
            wi_settings,
            None,
            false,
        )
    }

    /// Extended: pass prior timed state; set dry_run to skip persisting sticky/cooldown writes.
    pub fn build_generation_prompt_ex(
        &self,
        base_prompt: &str,
        world_book_id: Option<&str>,
        character_card_id: Option<&str>,
        chat_messages_oldest_first: &[(String, String)],
        wi_settings: Option<crate::WiSettings>,
        timed_in: Option<crate::TimedWorldInfo>,
        dry_run: bool,
    ) -> CoreResult<GenerationPromptResult> {
        self.build_generation_prompt_full(
            base_prompt,
            world_book_id,
            character_card_id,
            chat_messages_oldest_first,
            wi_settings,
            timed_in,
            dry_run,
            None,
            8192,
        )
    }

    pub fn build_generation_prompt_full(
        &self,
        base_prompt: &str,
        world_book_id: Option<&str>,
        character_card_id: Option<&str>,
        chat_messages_oldest_first: &[(String, String)],
        wi_settings: Option<crate::WiSettings>,
        timed_in: Option<crate::TimedWorldInfo>,
        dry_run: bool,
        scan_ctx: Option<crate::WiScanContext>,
        max_context_tokens: i32,
    ) -> CoreResult<GenerationPromptResult> {
        let st = self.load()?;
        let wb_id = world_book_id
            .map(|s| s.to_string())
            .or(st.selected_world_book_id.clone());
        let cc_id = character_card_id
            .map(|s| s.to_string())
            .or(st.selected_character_card_id.clone());

        let mut prompt = base_prompt.trim().to_string();
        if prompt.is_empty() {
            prompt = "你是一个体贴温和的伴侣，请用温暖、真实而细节丰富的语言与用户交谈，避免机器感。"
                .into();
        }

        let mut settings = wi_settings.unwrap_or_default();
        // W4: default token estimate mode from PublicSettings when not set on request.
        if settings.token_estimate_mode.trim().is_empty() {
            if let Ok(ps) = self.app_state.load_settings_public() {
                let m = ps.token_estimate_mode.trim();
                if !m.is_empty() {
                    settings.token_estimate_mode = m.to_string();
                }
            }
        }
        let scan_buf = crate::chat_to_scan_buffer(chat_messages_oldest_first);

        // Collect WI entries from selected world book (+ card-linked book if different)
        let mut entries: Vec<crate::WiEntry> = Vec::new();
        let mut wb_ids: Vec<String> = Vec::new();
        if let Some(ref id) = wb_id {
            wb_ids.push(id.clone());
        }
        if let Some(ref id) = cc_id {
            if let Some(cc) = st.character_cards.iter().find(|c| c.id == *id) {
                if let Some(ref wid) = cc.world_book_id {
                    if !wb_ids.iter().any(|x| x == wid) {
                        wb_ids.push(wid.clone());
                    }
                }
            }
        }
        for id in &wb_ids {
            if let Some(wb) = st.world_books.iter().find(|w| w.id == *id) {
                let name = if wb.name.is_empty() { id.clone() } else { wb.name.clone() };
                entries.extend(crate::entries_from_world_book(
                    &name,
                    wb.fields.as_ref(),
                    &wb.content,
                ));
            }
        }

        let mut ctx = scan_ctx.unwrap_or_default();
        if ctx.char_name.is_empty() {
            if let Some(ref id) = cc_id {
                if let Some(cc) = st.character_cards.iter().find(|c| c.id == *id) {
                    ctx.char_name = cc.name.clone();
                    ctx.character_name = cc.name.clone();
                    // tags from fields.identityTags if any
                    if let Some(fields) = cc.fields.as_ref() {
                        let mut tags: Vec<String> = Vec::new();
                        if let Some(s) = fields.get("identityTags").and_then(|v| v.as_str()) {
                            tags.extend(s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()));
                        }
                        if let Some(arr) = fields.get("tags").and_then(|v| v.as_array()) {
                            for x in arr {
                                if let Some(s) = x.as_str() {
                                    let t = s.trim();
                                    if !t.is_empty() { tags.push(t.to_string()); }
                                }
                            }
                        }
                        if let Some(st) = fields.get("stSource") {
                            if let Some(arr) = st.get("tags").and_then(|v| v.as_array()) {
                                for x in arr {
                                    if let Some(s) = x.as_str() {
                                        let t = s.trim();
                                        if !t.is_empty() { tags.push(t.to_string()); }
                                    }
                                }
                            }
                        }
                        tags.sort();
                        tags.dedup();
                        ctx.character_tags = tags;
                    }
                }
            }
        }
        if ctx.user_name.is_empty() {
            ctx.user_name = "User".into();
        }
        if ctx.trigger.is_empty() {
            ctx.trigger = "normal".into();
        }
        if ctx.max_context_tokens <= 0 {
            ctx.max_context_tokens = max_context_tokens.max(1024);
        }
        let max_ctx = ctx.max_context_tokens;
        let scan = crate::check_world_info_timed(
            &entries,
            &scan_buf,
            max_ctx,
            &settings,
            timed_in,
            dry_run,
            Some(ctx.clone()),
        );
        let wi_block = crate::format_wi_for_system(&scan); // combined; sections used below

        // Regex scripts: global library ∪ character-scoped (W6). Card > library by default.
        let lib_store = crate::RegexLibraryStore::new(self.app_state.data_root());
        let mut cc_content = String::new();
        let scripts = if let Some(ref id) = cc_id {
            if let Some(cc) = st.character_cards.iter().find(|c| c.id == *id) {
                cc_content = cc.content.trim().to_string();
                crate::resolve_runtime_scripts(&lib_store, cc.fields.as_ref())
            } else {
                lib_store.parsed_scripts()
            }
        } else {
            // No card — still apply global library (W6 gate)
            lib_store.parsed_scripts()
        };

        // Apply promptOnly / dual-mode regex to WI + card before assembly (placement WORLD_INFO / AI paths)
        let _wi_block_regexed = if !wi_block.is_empty() {
            crate::get_regexed_string(
                &wi_block,
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            )
        } else {
            wi_block
        };

        // Assemble like ST: before WI + base + card + after WI
        // We keep Kaleido section headers for clarity.
        if !scan.world_info_before.trim().is_empty() {
            let before = crate::get_regexed_string(
                scan.world_info_before.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 世界书（前置）\n{before}"));
        }
        if !scan.an_before.trim().is_empty() {
            let an = crate::get_regexed_string(
                scan.an_before.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 作者注释（顶）\n{an}"));
        }
        if !cc_content.is_empty() {
            let cc_rx = crate::get_regexed_string(
                &cc_content,
                crate::RegexPlacement::AiOutput,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 你的角色人设设定（伴侣设定）\n{cc_rx}"));
        } else if let Some(ref id) = cc_id {
            let _ = id;
        }
        if !scan.em_before.trim().is_empty() {
            let em = crate::get_regexed_string(
                scan.em_before.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 示例消息（前）\n{em}"));
        }
        if !scan.world_info_after.trim().is_empty() {
            let after = crate::get_regexed_string(
                scan.world_info_after.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 世界书（后置）\n{after}"));
        }
        if !scan.an_after.trim().is_empty() {
            let an = crate::get_regexed_string(
                scan.an_after.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 作者注释（底）\n{an}"));
        }
        if !scan.em_after.trim().is_empty() {
            let em = crate::get_regexed_string(
                scan.em_after.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 示例消息（后）\n{em}"));
        }
        for o in &scan.outlet_entries {
            if o.content.trim().is_empty() { continue; }
            let body = crate::get_regexed_string(
                o.content.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                None,
            );
            prompt.push_str(&format!("\n\n## 世界书出口（{}）\n{}", o.name, body));
        }
        for d in &scan.depth_entries {
            if d.content.trim().is_empty() { continue; }
            let body = crate::get_regexed_string(
                d.content.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                Some(d.depth),
            );
            prompt.push_str(&format!(
                "\n\n## 世界书（深度 {}）\n{}",
                d.depth, body
            ));
        }

        // If scan empty but legacy book had only freeform and constants already handled —
        // also: if no structured entries matched and book has legacy constant, scan covers it.

        // Fallback: if zero entries parsed but wb content exists, inject full content (compat)
        if entries.is_empty() {
            if let Some(ref id) = wb_id {
                if let Some(wb) = st.world_books.iter().find(|w| w.id == *id) {
                    let content = wb.content.trim();
                    if !content.is_empty() {
                        prompt.push_str(&format!("\n\n## 伴侣对话世界设定\n{content}"));
                    }
                }
            }
        }

        // Multi-slot injections for chat transcript (ST IN_CHAT / examples / outlets)
        let mut message_injections = Vec::new();
        for inj in &scan.prompt_slots.chat_injections {
            if inj.content.trim().is_empty() {
                continue;
            }
            let body = crate::get_regexed_string(
                inj.content.trim(),
                crate::RegexPlacement::WorldInfo,
                &scripts,
                false,
                true,
                Some(inj.depth),
            );
            // Prefer not duplicating EM/outlet already mirrored in system sections for depth-only?
            // Include all slots as explicit messages so clients/PromptManager can place them.
            if inj.kind == "depth" || inj.kind.starts_with("outlet:") {
                message_injections.push(serde_json::json!({
                    "role": inj.role,
                    "content": body,
                    "depth": inj.depth,
                    "kind": inj.kind,
                    "wiSlot": true,
                }));
            } else if inj.kind == "em_before" || inj.kind == "em_after" {
                // Also as example-style system messages tagged for UI
                message_injections.push(serde_json::json!({
                    "role": "system",
                    "content": body,
                    "depth": inj.depth,
                    "kind": inj.kind,
                    "wiSlot": true,
                    "example": true,
                }));
            }
        }

        // Prefer structured example pairs at head of injections (ST mesExamples before chat)
        let example_messages = scan.example_messages.clone();
        let mut ordered_inj = Vec::new();
        for ex in &example_messages {
            ordered_inj.push(serde_json::json!({
                "role": ex.role,
                "content": ex.content,
                "depth": 0,
                "kind": format!("em_example_{}", ex.anchor),
                "wiSlot": true,
                "example": true,
            }));
        }
        // dedupe: skip em_example_* already in message_injections from chat_injections path
        for inj in message_injections {
            let kind = inj.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if kind.starts_with("em_example_") {
                continue; // already from example_messages
            }
            ordered_inj.push(inj);
        }
        let automation_ids = scan.automation_ids.clone();
        let timed_out = scan.timed_world_info.clone();
        Ok(GenerationPromptResult {
            system_prompt: prompt,
            wi: scan,
            regex_script_count: scripts.len(),
            timed_world_info: timed_out,
            message_injections: ordered_inj,
            automation_ids,
            example_messages,
        })
    }
}


// --- Settings helpers ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicSettings {
    #[serde(default)]
    pub llm_model: String,
    #[serde(default)]
    pub model_interface: String,
    #[serde(default)]
    pub partner_chat_prompt: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// [酒馆对齐] 采样参数: topP / frequencyPenalty / presencePenalty (与 temperature 同级存储)
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    /// Masked display only — never full secret in API responses.
    #[serde(default)]
    pub llm_api_key: String,
    #[serde(default)]
    pub llm_api_key_configured: bool,
    /// OpenAI-compatible base URL (editable from web).
    #[serde(default)]
    pub llm_base_url: String,
    #[serde(default)]
    pub llm_base_url_configured: bool,
    /// Fanqie crawler switch (S5-W1 T5). Default false — never auto-enable.
    #[serde(default)]
    pub crawler_enabled: bool,
    /// Agent tools bash sandbox gate (S5-W1 T4). Default false.
    #[serde(default)]
    pub bash_sandbox_enabled: bool,
    /// Agent tools read/list/grep/glob gate. Default true.
    #[serde(default = "default_true_flag")]
    pub agent_tools_enabled: bool,
    /// Agent write/edit gate (W10). Default **false** — dangerous tools off until toggled.
    #[serde(default)]
    pub agent_write_enabled: bool,
    /// When true (default), write/edit/bash REST calls require `confirmDangerous: true`.
    #[serde(default = "default_true_flag")]
    pub agent_confirm_dangerous: bool,
    /// W4: token estimate mode for WI budget / estimate API.
    /// `heuristic` (default) | `cl100k_approx`.
    #[serde(default)]
    pub token_estimate_mode: String,
    /// W12: max concurrent auth sessions (optional; live from AuthStore if unset).
    #[serde(default)]
    pub session_max: Option<u64>,
    /// W12: `auto_evict` | `reject`.
    #[serde(default)]
    pub session_cap_policy: String,
    /// Story Tavern adult content confirmation (server-persisted, survives browser reset).
    #[serde(default)]
    pub tavern_adult_ok: bool,
}

/// Runtime LLM credentials resolved from settings-store + secrets + env fallback.
#[derive(Debug, Clone)]
pub struct LlmRuntime {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// P5 call-side: set when the runtime resolved from a managed ai_admin
    /// provider (enabled provider + model). None for settings/env fallback.
    pub provider_id: Option<String>,
    /// G6: protocol of the resolved runtime ("openai"|"anthropic"|"google").
    /// Empty string = env fallback → use the KALEIDO_LLM_PROVIDER default.
    pub provider_kind: String,
    /// P5 call-side: true when the provider's per-minute RPM window is full and
    /// the call must be rejected (base_url is emptied so callers fail fast).
    pub rpm_hit: bool,
    pub rpm_retry_secs: u64,
}

/// In-memory per-provider RPM limiter (sliding 60s window), shared process-wide
/// so all resolve_llm call sites enforce the same budget for a provider.
/// Not persisted by design: the window resets on restart.
#[derive(Clone, Default)]
pub struct RpmLimiter {
    inner: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, std::collections::VecDeque<std::time::Instant>>>>,
}

static RPM_GLOBAL: std::sync::OnceLock<RpmLimiter> = std::sync::OnceLock::new();

impl RpmLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process-wide singleton used by resolve_llm call-side integration.
    pub fn global() -> &'static RpmLimiter {
        RPM_GLOBAL.get_or_init(RpmLimiter::default)
    }

    /// Try to acquire one slot for `provider_id` within `rpm` per minute.
    /// Ok(remaining) on success; Err(retry_after_secs) when the window is full.
    pub fn try_acquire(&self, provider_id: &str, rpm: u32) -> Result<u32, u64> {
        if rpm == 0 {
            return Ok(0);
        }
        let now = std::time::Instant::now();
        let mut m = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let v = m.entry(provider_id.to_string()).or_default();
        v.retain(|t| now.duration_since(*t).as_secs() < 60);
        if (v.len() as u32) < rpm {
            v.push_back(now);
            Ok(rpm - v.len() as u32)
        } else {
            let oldest = v.front().copied().unwrap_or(now);
            let wait = now.duration_since(oldest).as_secs().saturating_add(1);
            Err(wait)
        }
    }

    /// Drop recorded slots (used when a provider is deleted).
    pub fn reset(&self, provider_id: &str) {
        if let Ok(mut m) = self.inner.lock() {
            m.remove(provider_id);
        }
    }
}

fn mask_api_key(key: &str) -> String {
    let k = key.trim();
    if k.is_empty() {
        return String::new();
    }
    if k == "[server]" || k.contains('•') {
        return k.to_string();
    }
    let chars: Vec<char> = k.chars().collect();
    if chars.len() <= 8 {
        return format!("{}•••", chars.first().map(|c| c.to_string()).unwrap_or_default());
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}•••{tail}")
}

fn default_true_flag() -> bool {
    true
}

impl AppStateStore {
    pub fn load_settings_public(&self) -> CoreResult<PublicSettings> {
        let raw = self.load("settings-store")?;
        let v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
        let state = v.get("state").cloned().unwrap_or(v);
        let partner = state
            .get("partnerChatPrompt")
            .and_then(|x| x.as_str())
            .unwrap_or("你是一个体贴温和的伴侣，请用温暖、真实而细节丰富的语言与用户交谈，避免机器感。")
            .to_string();
        let mut model = state
            .get("llmModel")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let agent = state
            .get("agentConfigs")
            .and_then(|x| x.get("partnerChat"));
        let mut base_url = state
            .get("llmBaseUrl")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let secret_path = self.data.root().join("secrets").join("llm_api_key.txt");
        let secret_key = fs::read_to_string(&secret_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let mut key_configured = !secret_key.is_empty();
        let mut key_display = if key_configured {
            mask_api_key(&secret_key)
        } else {
            String::new()
        };
        // [酒馆对齐] LLM 连接字段以 active provider 为准 (settings-store 退役,
        // 仅在无 provider 时回退 legacy 值)。避免重复打开 sqlite: 一次派生。
        {
            let derived = crate::ai_admin_store::AiAdminStore::open(&self.data.root().join("plot.sqlite"))
                .and_then(|s| s.active_provider())
                .ok()
                .flatten();
            if let Some(p) = derived {
                if !p.base_url.is_empty() {
                    // keep legacy base_url when provider base empty (rare)
                    base_url = p.base_url;
                }
                let d_model = p.default_model_id.clone();
                let d_model_name = if !d_model.is_empty() {
                    crate::ai_admin_store::AiAdminStore::open(&self.data.root().join("plot.sqlite"))
                        .and_then(|s| s.get_model(&d_model))
                        .map(|m| m.model_id)
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                if !d_model_name.is_empty() {
                    model = d_model_name;
                }
                if p.configured {
                    key_configured = true;
                    key_display = p.key_hint.clone();
                }
            }
        }
        Ok(PublicSettings {
            llm_model: model,
            model_interface: state
                .get("modelInterface")
                .and_then(|x| x.as_str())
                .unwrap_or("OpenAI")
                .to_string(),
            partner_chat_prompt: partner,
            temperature: agent
                .and_then(|a| a.get("temperature"))
                .and_then(|x| x.as_f64()),
            max_output_tokens: agent
                .and_then(|a| a.get("maxOutputTokens"))
                .and_then(|x| x.as_u64()),
            // [酒馆对齐] 采样参数 (与 temperature 同级存储于 agentConfigs.partnerChat)
            top_p: agent
                .and_then(|a| a.get("topP"))
                .and_then(|x| x.as_f64()),
            frequency_penalty: agent
                .and_then(|a| a.get("frequencyPenalty"))
                .and_then(|x| x.as_f64()),
            presence_penalty: agent
                .and_then(|a| a.get("presencePenalty"))
                .and_then(|x| x.as_f64()),
            llm_api_key: key_display,
            llm_api_key_configured: key_configured,
            llm_base_url: base_url.clone(),
            llm_base_url_configured: !base_url.is_empty(),
            crawler_enabled: state
                .get("crawlerEnabled")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            bash_sandbox_enabled: state
                .get("bashSandboxEnabled")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            agent_tools_enabled: state
                .get("agentToolsEnabled")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            agent_write_enabled: state
                .get("agentWriteEnabled")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            agent_confirm_dangerous: state
                .get("agentConfirmDangerous")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            token_estimate_mode: state
                .get("tokenEstimateMode")
                .and_then(|x| x.as_str())
                .unwrap_or("heuristic")
                .to_string(),
            session_max: state
                .get("sessionMax")
                .or_else(|| state.get("maxSessions"))
                .and_then(|x| x.as_u64()),
            session_cap_policy: state
                .get("sessionCapPolicy")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            tavern_adult_ok: state
                .get("tavernAdultOk")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        })
    }

    /// Resolve LLM endpoint for chat/jobs.
    /// Priority: active managed provider (ai_providers.active=1) → 懒迁移
    /// (settings-store 残留 llmBaseUrl → 建 active provider) → env fallbacks.
    /// [酒馆对齐] settings-store 的 llmBaseUrl/llmModel/llmApiKey 不再直接消费。
    pub fn resolve_llm(
        &self,
        env_base: Option<&str>,
        env_key: Option<&str>,
        env_model: &str,
    ) -> LlmRuntime {
        // P5: managed provider (active/enabled provider + enabled model) wins so
        // the admin UI can switch providers instantly (酒馆对齐: active 指针)。
        if let Some(rt) = self.resolve_from_provider() {
            return rt;
        }
        // 懒迁移: 老安装 settings-store 还残留 llmBaseUrl 时, 迁移成 active
        // provider 再重试一次 (成功后后续都走 provider 分支)。
        if self.migrate_legacy_llm() {
            if let Some(rt) = self.resolve_from_provider() {
                return rt;
            }
        }
        LlmRuntime {
            base_url: env_base
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_default(),
            api_key: env_key
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_default(),
            model: env_model.trim().to_string(),
            provider_id: None,
            provider_kind: String::new(),
            rpm_hit: false,
            rpm_retry_secs: 0,
        }
    }

    /// Try the managed-provider path (active/enabled) with per-provider RPM gating.
    fn resolve_from_provider(&self) -> Option<LlmRuntime> {
        let rt = crate::ai_admin_store::AiAdminStore::open(&self.data.root().join("plot.sqlite"))
            .and_then(|s| s.resolve_call_runtime())
            .ok()
            .flatten()?;
        // P5 RPM: enforce per-provider per-minute budget across every
        // resolve_llm call site. When full, empty the base_url so callers
        // fail fast, and surface the retry window for friendly 429s.
        let (rpm_hit, rpm_retry_secs) =
            match RpmLimiter::global().try_acquire(&rt.provider_id, rt.rpm_limit) {
                Ok(_) => (false, 0),
                Err(wait) => {
                    tracing::warn!(
                        provider_id = %rt.provider_id,
                        rpm = rt.rpm_limit,
                        retry_after_secs = wait,
                        "LLM provider RPM limit hit"
                    );
                    (true, wait)
                }
            };
        Some(LlmRuntime {
            base_url: if rpm_hit { String::new() } else { rt.base_url },
            api_key: rt.api_key,
            model: rt.model,
            provider_id: Some(rt.provider_id),
            provider_kind: rt.protocol,
            rpm_hit,
            rpm_retry_secs,
        })
    }

    /// [酒馆对齐] 懒迁移: settings-store 残留 llmBaseUrl/llmModel/llmApiKey 且
    /// 无 active provider 时, 自动建成 active provider (只做一次——成功后走
    /// provider 分支)。有 provider 但无 active 时, 挑第一个 enabled 激活。
    fn migrate_legacy_llm(&self) -> bool {
        let store = match crate::ai_admin_store::AiAdminStore::open(&self.data.root().join("plot.sqlite")) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let providers = match store.list_providers() {
            Ok(p) => p,
            Err(_) => return false,
        };
        if providers.iter().any(|p| p.active) {
            return false; // 已有 active, 无需迁移
        }
        if !providers.is_empty() {
            // 无 active 但有 provider: 挑第一个 enabled (否则第一个) 激活
            let pick = providers
                .iter()
                .find(|p| p.status == "enabled")
                .or_else(|| providers.first());
            if let Some(p) = pick {
                return store.set_active_provider(&p.id).is_ok();
            }
        }
        // 无 provider: 从 settings-store 迁移
        let raw = self.load("settings-store").unwrap_or_default();
        let v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
        let state = v.get("state").cloned().unwrap_or(v);
        let base = state
            .get("llmBaseUrl")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(base) = base else { return false };
        let secret_path = self.data.root().join("secrets").join("llm_api_key.txt");
        let key = fs::read_to_string(&secret_path)
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let model = state
            .get("llmModel")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let iface = state
            .get("modelInterface")
            .and_then(|x| x.as_str())
            .unwrap_or("OpenAI");
        let proto = if iface.to_ascii_lowercase().contains("anthropic") {
            "anthropic"
        } else if iface.to_ascii_lowercase().contains("google") {
            "google"
        } else {
            "openai"
        };
        let p = match store.create_provider("默认供应商", &base, proto, &key, 10, 60, 32000, "从设置页自动迁移") {
            Ok(p) => p,
            Err(_) => return false,
        };
        if store.set_active_provider(&p.id).is_err() {
            return false;
        }
        if !model.is_empty() {
            if let Ok(m) = store.create_model(&p.id, &model, &model, &["chat"], 128000, true, true, "设置页指定") {
                let _ = store.set_default_model(&p.id, &m.id);
            }
        }
        true
    }

    pub fn patch_settings_public(&self, patch: &Value) -> CoreResult<PublicSettings> {
        let raw = self.load("settings-store")?;
        let mut root: Value = serde_json::from_str(&raw).unwrap_or(json!({"state":{}}));
        if !root.get("state").map(|s| s.is_object()).unwrap_or(false) {
            root = json!({"state": root});
        }
        let state = root
            .get_mut("state")
            .and_then(|s| s.as_object_mut())
            .ok_or_else(|| CoreError::BadRequest("settings state missing".into()))?;

        if let Some(p) = patch.get("partnerChatPrompt").and_then(|x| x.as_str()) {
            state.insert("partnerChatPrompt".into(), json!(p));
        }
        // [酒馆对齐] LLM 连接字段 (llmModel/llmBaseUrl/llmApiKey/modelInterface)
        // 转发到 active provider —— settings-store 不再持有连接配置。
        let llm_conn_touched = ["llmModel", "llmBaseUrl", "llmApiKey", "modelInterface"]
            .iter()
            .any(|k| patch.get(*k).is_some());
        if llm_conn_touched {
            self.apply_llm_patch_to_provider(patch)?;
        }
        if let Some(temp) = patch.get("temperature").and_then(|x| x.as_f64()) {
            let agents = state
                .entry("agentConfigs".to_string())
                .or_insert_with(|| json!({}));
            let obj = agents.as_object_mut().unwrap();
            let pc = obj
                .entry("partnerChat".to_string())
                .or_insert_with(|| json!({}));
            pc.as_object_mut()
                .unwrap()
                .insert("temperature".into(), json!(temp));
        }
        if let Some(mt) = patch.get("maxOutputTokens").and_then(|x| x.as_u64()) {
            let agents = state
                .entry("agentConfigs".to_string())
                .or_insert_with(|| json!({}));
            let obj = agents.as_object_mut().unwrap();
            let pc = obj
                .entry("partnerChat".to_string())
                .or_insert_with(|| json!({}));
            pc.as_object_mut()
                .unwrap()
                .insert("maxOutputTokens".into(), json!(mt));
        }
        // [酒馆对齐] 采样参数: topP / frequencyPenalty / presencePenalty
        for name in ["topP", "frequencyPenalty", "presencePenalty"] {
            if let Some(v) = patch.get(name).and_then(|x| x.as_f64()) {
                let agents = state
                    .entry("agentConfigs".to_string())
                    .or_insert_with(|| json!({}));
                let obj = agents.as_object_mut().unwrap();
                let pc = obj
                    .entry("partnerChat".to_string())
                    .or_insert_with(|| json!({}));
                pc.as_object_mut().unwrap().insert(name.into(), json!(v));
            }
        }
        if let Some(en) = patch.get("crawlerEnabled").and_then(|x| x.as_bool()) {
            state.insert("crawlerEnabled".into(), json!(en));
        }
        // S5-W1 T4 agent tools flags
        if let Some(b) = patch.get("bashSandboxEnabled").and_then(|x| x.as_bool()) {
            state.insert("bashSandboxEnabled".into(), json!(b));
        }
        if let Some(b) = patch.get("agentToolsEnabled").and_then(|x| x.as_bool()) {
            state.insert("agentToolsEnabled".into(), json!(b));
        }
        if let Some(b) = patch.get("agentWriteEnabled").and_then(|x| x.as_bool()) {
            state.insert("agentWriteEnabled".into(), json!(b));
        }
        if let Some(b) = patch.get("agentConfirmDangerous").and_then(|x| x.as_bool()) {
            state.insert("agentConfirmDangerous".into(), json!(b));
        }
        if let Some(m) = patch.get("tokenEstimateMode").and_then(|x| x.as_str()) {
            let mode = crate::TokenEstimateMode::parse(m);
            state.insert("tokenEstimateMode".into(), json!(mode.as_str()));
        }
        if let Some(n) = patch
            .get("sessionMax")
            .or_else(|| patch.get("maxSessions"))
            .and_then(|x| x.as_u64())
        {
            if n >= 1 {
                state.insert("sessionMax".into(), json!(n.min(10_000)));
            }
        }
        if let Some(p) = patch.get("sessionCapPolicy").and_then(|x| x.as_str()) {
            let pol = match p.trim().to_ascii_lowercase().as_str() {
                "reject" | "hard" | "fail" => "reject",
                _ => "auto_evict",
            };
            state.insert("sessionCapPolicy".into(), json!(pol));
        }
        if let Some(b) = patch.get("tavernAdultOk").and_then(|x| x.as_bool()) {
            state.insert("tavernAdultOk".into(), json!(b));
        }
        self.save("settings-store", &serde_json::to_string_pretty(&root)?)?;
        self.load_settings_public()
    }

    /// [酒馆对齐] settings 的 llm* 字段 → active provider 薄封装。
    /// 目标 provider: active → 第一个 enabled → 新建 (auto-active)。
    /// llmModel 匹配 provider 下 model_id; 无匹配则自动建模型并设为默认。
    fn apply_llm_patch_to_provider(&self, patch: &Value) -> CoreResult<()> {
        let base = patch
            .get("llmBaseUrl")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string());
        let key = patch
            .get("llmApiKey")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string());
        let model = patch
            .get("llmModel")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string());
        let iface = patch
            .get("modelInterface")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string());
        let store = crate::ai_admin_store::AiAdminStore::open(&self.data.root().join("plot.sqlite"))
            .map_err(|e| CoreError::BadRequest(format!("ai store: {e}")))?;

        // 目标 provider: active → 第一个 enabled → 新建
        let pid = match store.active_provider() {
            Ok(Some(p)) => p.id,
            _ => {
                let existing = store
                    .list_providers()
                    .map_err(|e| CoreError::BadRequest(format!("{e}")))?;
                let pick = existing
                    .iter()
                    .find(|p| p.status == "enabled")
                    .or_else(|| existing.first());
                match pick {
                    Some(p) => p.id.clone(),
                    None => {
                        let url = base.clone().unwrap_or_default();
                        if url.is_empty() {
                            return Ok(()); // 无可建内容
                        }
                        let proto = if iface.as_deref().map(|i| i.to_ascii_lowercase().contains("anthropic")).unwrap_or(false) {
                            "anthropic"
                        } else if iface.as_deref().map(|i| i.to_ascii_lowercase().contains("google")).unwrap_or(false) {
                            "google"
                        } else {
                            "openai"
                        };
                        let p = store
                            .create_provider(
                                "默认供应商",
                                &url,
                                proto,
                                key.as_deref().unwrap_or(""),
                                10,
                                60,
                                32000,
                                "设置页创建",
                            )
                            .map_err(|e| CoreError::BadRequest(format!("{e}")))?;
                        let _ = store.set_active_provider(&p.id);
                        p.id
                    }
                }
            }
        };

        // base_url / api_key 更新 (SSRF 校验在 store.update_provider 内)
        if base.is_some() || key.is_some() {
            let cur = store
                .get_provider(&pid)
                .map_err(|e| CoreError::BadRequest(format!("{e}")))?;
            let new_url = base.clone().unwrap_or(cur.base_url.clone());
            let new_key = match key.as_deref() {
                Some(k) if !k.is_empty() && k != "[server]" && !k.contains('•') => k.to_string(),
                _ => "keep".to_string(),
            };
            store
                .update_provider(&pid, None, Some(&new_url), Some(&new_key), None, None, None, None)
                .map_err(|e| CoreError::BadRequest(format!("{e}")))?;
        }
        // llmModel → provider 下 model_id; 无匹配则建模型并设为默认
        if let Some(m) = model {
            if !m.is_empty() {
                let models = store
                    .list_models(&pid)
                    .map_err(|e| CoreError::BadRequest(format!("{e}")))?;
                match models.iter().find(|mm| mm.model_id == m) {
                    Some(mm) => {
                        store
                            .set_default_model(&pid, &mm.id)
                            .map_err(|e| CoreError::BadRequest(format!("{e}")))?;
                    }
                    None => {
                        if let Ok(nm) = store.create_model(&pid, &m, &m, &["chat"], 128000, true, true, "设置页指定") {
                            let _ = store.set_default_model(&pid, &nm.id);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn partner_chat_prompt(&self) -> String {
        self.load_settings_public()
            .map(|s| s.partner_chat_prompt)
            .unwrap_or_else(|_| {
                "你是一个体贴温和的伴侣，请用温暖、真实而细节丰富的语言与用户交谈，避免机器感。"
                    .into()
            })
    }
}

// --- Works filesystem (S4): path-jailed text tree under works/{workspace_id} ---

/// Max text file size accepted by WorksFs (2 MiB).
pub const WORKS_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Max image payload for works image-data-url (same cap as text).
pub const WORKS_MAX_IMAGE_BYTES: u64 = WORKS_MAX_FILE_BYTES;
/// Max list recursion depth exposed by GET /works (server clamps to this).
pub const WORKS_MAX_LIST_DEPTH: u32 = 8;
/// Default list depth when query omits depth.
pub const WORKS_DEFAULT_LIST_DEPTH: u32 = 2;

/// Public limits + directory convention snapshot (W11).
pub fn works_limits_public() -> Value {
    json!({
        "ok": true,
        "maxFileBytes": WORKS_MAX_FILE_BYTES,
        "maxImageBytes": WORKS_MAX_IMAGE_BYTES,
        "maxListDepth": WORKS_MAX_LIST_DEPTH,
        "defaultListDepth": WORKS_DEFAULT_LIST_DEPTH,
        "jail": "works/{workspaceId}",
        "textOnly": true,
        "parentsMustExist": true,
        "conventions": {
            "book-travel": "BookTravel pipeline artifacts (md/json under book-travel/…)",
            "book-travel/steps": "Per-step BookTravel dumps",
            "outline": "Outline module drafts",
            "crawler": "Crawler scratch under works (novel_workspace is separate root)",
            "author": "Author-zone drafts when present",
        },
        "codes": [
            "WORKS_FILE_TOO_LARGE",
            "WORKS_CONTENT_TOO_LARGE",
            "WORKS_APPEND_TOO_LARGE",
            "WORKS_PARENT_MISSING",
            "WORKS_BINARY_REJECTED",
            "WORKS_NOT_UTF8",
            "WORKS_NOT_FILE",
            "WORKS_IS_DIR",
            "WORKS_PATH_ESCAPE",
            "WORKS_PATH_TRAVERSAL",
            "WORKS_ABSOLUTE_PATH",
            "WORKS_NOT_FOUND",
            "WORKS_DIR_NOT_EMPTY",
            "WORKS_ROOT_FORBIDDEN",
            "WORKS_LIST_NOT_DIR",
            "WORKS_BINARY_CONTENT",
            "WORKS_INVALID_PATH",
            "WORKS_INVALID_WORKSPACE",
        ]
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksEntry {
    pub path: String,
    pub name: String,
    pub kind: String, // "file" | "dir"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<WorksEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksStat {
    pub path: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<u64>,
    pub is_text: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksFileBody {
    pub path: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Path-jailed filesystem rooted at `$KALEIDO_DATA/works/{workspace_id}`.
#[derive(Clone)]
pub struct WorksFs {
    data: DataRoot,
}

impl WorksFs {
    pub fn new(data: DataRoot) -> Self {
        Self { data }
    }

    pub fn workspace_root(&self, workspace_id: &str) -> CoreResult<PathBuf> {
        validate_workspace_id(workspace_id)?;
        let root = self.data.root().join("works").join(workspace_id);
        fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn jail_canonical(&self, workspace_id: &str) -> CoreResult<PathBuf> {
        let root = self.workspace_root(workspace_id)?;
        fs::canonicalize(&root).map_err(|e| {
            CoreError::Io(io::Error::new(
                e.kind(),
                format!("canonicalize jail: {e}"),
            ))
        })
    }

    /// Resolve a relative works path into an absolute path that is inside the jail.
    /// `for_create`: when true, the leaf may not exist yet (parent must exist or be creatable).
    pub fn resolve(
        &self,
        workspace_id: &str,
        rel: &str,
        for_create: bool,
    ) -> CoreResult<PathBuf> {
        let rel = normalize_rel_path(rel)?;
        let jail = self.jail_canonical(workspace_id)?;
        if rel.is_empty() || rel == "." {
            return Ok(jail);
        }

        let candidate = jail.join(&rel);
        if for_create {
            // Canonicalize the deepest existing ancestor, then re-join remaining components.
            let mut existing = candidate.as_path();
            let mut missing: Vec<std::ffi::OsString> = Vec::new();
            loop {
                if existing.exists() {
                    break;
                }
                match existing.file_name() {
                    Some(name) => {
                        missing.push(name.to_os_string());
                        existing = existing
                            .parent()
                            .ok_or_else(|| CoreError::works_path_escape("path escape"))?;
                    }
                    None => {
                        return Err(CoreError::works_path_escape("path escape"));
                    }
                }
            }
            let mut resolved = fs::canonicalize(existing).map_err(|e| {
                CoreError::Io(io::Error::new(e.kind(), format!("canonicalize parent: {e}")))
            })?;
            ensure_under_jail(&resolved, &jail)?;
            for part in missing.into_iter().rev() {
                // Reject sneaky components even after join
                let s = part.to_string_lossy();
                if s == ".." || s == "." || s.contains('\0') {
                    return Err(CoreError::coded(
                        "WORKS_INVALID_PATH",
                        "invalid path component",
                    ));
                }
                resolved.push(part);
            }
            ensure_under_jail(&resolved, &jail)?;
            Ok(resolved)
        } else {
            if !candidate.exists() {
                // Still reject escapes even when missing (use parent resolution)
                let _ = self.resolve(workspace_id, &rel, true)?;
                return Err(CoreError::works_not_found(rel));
            }
            let resolved = fs::canonicalize(&candidate).map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    CoreError::works_not_found(rel)
                } else {
                    CoreError::Io(e)
                }
            })?;
            ensure_under_jail(&resolved, &jail)?;
            Ok(resolved)
        }
    }

    pub fn list(
        &self,
        workspace_id: &str,
        path: &str,
        depth: u32,
    ) -> CoreResult<WorksEntry> {
        let jail = self.jail_canonical(workspace_id)?;
        let abs = self.resolve(workspace_id, path, false)?;
        let meta = fs::metadata(&abs)?;
        if !meta.is_dir() {
            return Err(CoreError::coded(
                "WORKS_LIST_NOT_DIR",
                "list requires a directory",
            ));
        }
        let rel = display_rel(&abs, &jail)?;
        self.list_entry(&abs, &rel, depth, &jail)
    }

    fn list_entry(
        &self,
        abs: &Path,
        rel: &str,
        depth: u32,
        jail: &Path,
    ) -> CoreResult<WorksEntry> {
        // Resolve through symlinks; reject escapes.
        let abs = if abs.exists() {
            let canon = fs::canonicalize(abs).map_err(CoreError::Io)?;
            ensure_under_jail(&canon, jail)?;
            canon
        } else {
            return Err(CoreError::works_not_found(rel));
        };
        let name = if rel.is_empty() {
            "".into()
        } else {
            abs.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let meta = fs::metadata(&abs)?;
        let mut entry = WorksEntry {
            path: if rel.is_empty() {
                "".into()
            } else {
                rel.into()
            },
            name,
            kind: if meta.is_dir() {
                "dir".into()
            } else {
                "file".into()
            },
            size: if meta.is_file() {
                Some(meta.len())
            } else {
                None
            },
            modified_ms: system_time_ms(meta.modified().ok()),
            children: Vec::new(),
        };
        if meta.is_dir() && depth > 0 {
            let mut children = Vec::new();
            let mut entries: Vec<_> = fs::read_dir(&abs)?.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let child_name = e.file_name().to_string_lossy().to_string();
                if child_name == "." || child_name == ".." {
                    continue;
                }
                let child_abs = e.path();
                // Symlink out of jail → skip
                if let Ok(canon) = fs::canonicalize(&child_abs) {
                    if ensure_under_jail(&canon, jail).is_err() {
                        continue;
                    }
                } else {
                    continue;
                }
                let child_rel = if rel.is_empty() {
                    child_name
                } else {
                    format!("{rel}/{child_name}")
                };
                match self.list_entry(&child_abs, &child_rel, depth.saturating_sub(1), jail) {
                    Ok(c) => children.push(c),
                    Err(_) => continue,
                }
            }
            entry.children = children;
        }
        Ok(entry)
    }

    pub fn stat(&self, workspace_id: &str, path: &str) -> CoreResult<WorksStat> {
        let abs = self.resolve(workspace_id, path, false)?;
        let meta = fs::metadata(&abs)?;
        let jail = self.jail_canonical(workspace_id)?;
        let rel = display_rel(&abs, &jail)?;
        let name = if rel.is_empty() {
            "".into()
        } else {
            abs.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let is_file = meta.is_file();
        let is_text = if is_file {
            is_probably_text(&abs, meta.len())
        } else {
            true
        };
        Ok(WorksStat {
            path: rel,
            name,
            kind: if meta.is_dir() {
                "dir".into()
            } else if meta.is_file() {
                "file".into()
            } else {
                "other".into()
            },
            size: if is_file { Some(meta.len()) } else { None },
            modified_ms: system_time_ms(meta.modified().ok()),
            is_text,
        })
    }

    pub fn read_text(&self, workspace_id: &str, path: &str) -> CoreResult<WorksFileBody> {
        let abs = self.resolve(workspace_id, path, false)?;
        let meta = fs::metadata(&abs)?;
        if !meta.is_file() {
            return Err(CoreError::works_not_file());
        }
        if meta.len() > WORKS_MAX_FILE_BYTES {
            return Err(CoreError::works_too_large("file", meta.len(), WORKS_MAX_FILE_BYTES));
        }
        if !is_probably_text(&abs, meta.len()) {
            return Err(CoreError::works_binary());
        }
        let content = fs::read_to_string(&abs).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                CoreError::works_not_utf8()
            } else {
                CoreError::Io(e)
            }
        })?;
        let jail = self.jail_canonical(workspace_id)?;
        let rel = display_rel(&abs, &jail)?;
        Ok(WorksFileBody {
            path: rel,
            content,
            size: Some(meta.len()),
        })
    }

    pub fn write_text(
        &self,
        workspace_id: &str,
        path: &str,
        content: &str,
    ) -> CoreResult<WorksFileBody> {
        if content.len() as u64 > WORKS_MAX_FILE_BYTES {
            return Err(CoreError::works_too_large(
                "write",
                content.len() as u64,
                WORKS_MAX_FILE_BYTES,
            ));
        }
        if content.bytes().any(|b| b == 0) {
            return Err(CoreError::coded(
                "WORKS_BINARY_CONTENT",
                "binary content rejected",
            ));
        }
        let rel = normalize_rel_path(path)?;
        if rel.is_empty() || rel == "." {
            return Err(CoreError::coded(
                "WORKS_ROOT_FORBIDDEN",
                "cannot write to workspace root",
            ));
        }
        let abs = self.resolve(workspace_id, &rel, true)?;
        // Parents are not auto-created (use mkdir). Parent must already exist inside jail.
        if let Some(parent) = abs.parent() {
            if !parent.exists() {
                return Err(CoreError::works_parent_missing());
            }
            let parent_canon = fs::canonicalize(parent)?;
            let jail = self.jail_canonical(workspace_id)?;
            ensure_under_jail(&parent_canon, &jail)?;
        }
        // If path exists as a dir, reject
        if abs.exists() {
            let meta = fs::metadata(&abs)?;
            if meta.is_dir() {
                return Err(CoreError::works_is_dir());
            }
            // Re-check jail after existence (symlink race)
            let canon = fs::canonicalize(&abs)?;
            let jail = self.jail_canonical(workspace_id)?;
            ensure_under_jail(&canon, &jail)?;
        }
        fs::write(&abs, content.as_bytes())?;
        let jail = self.jail_canonical(workspace_id)?;
        // After write, canonicalize and verify still in jail
        let canon = fs::canonicalize(&abs)?;
        ensure_under_jail(&canon, &jail)?;
        let out_rel = display_rel(&canon, &jail)?;
        Ok(WorksFileBody {
            path: out_rel,
            content: content.to_string(),
            size: Some(content.len() as u64),
        })
    }

    /// Append UTF-8 text to a file (create if missing). Parents must exist.
    /// Fail-open friendly for live docs.
    pub fn append_text(
        &self,
        workspace_id: &str,
        path: &str,
        chunk: &str,
    ) -> CoreResult<WorksFileBody> {
        if chunk.bytes().any(|b| b == 0) {
            return Err(CoreError::coded(
                "WORKS_BINARY_CONTENT",
                "binary content rejected",
            ));
        }
        let existing = match self.read_text(workspace_id, path) {
            Ok(b) => b.content,
            Err(CoreError::NotFound(_)) => String::new(),
            Err(e) => return Err(e),
        };
        let combined = if existing.is_empty() {
            chunk.to_string()
        } else if existing.ends_with('\n') {
            format!("{existing}{chunk}")
        } else {
            format!("{existing}\n{chunk}")
        };
        if combined.len() as u64 > WORKS_MAX_FILE_BYTES {
            return Err(CoreError::works_too_large(
                "append",
                combined.len() as u64,
                WORKS_MAX_FILE_BYTES,
            ));
        }
        // Ensure parent for first create
        let rel = normalize_rel_path(path)?;
        if let Some(parent) = Path::new(&rel).parent() {
            let parent_s = parent.to_string_lossy();
            if !parent_s.is_empty() && parent_s != "." {
                let _ = self.mkdir(workspace_id, &parent_s);
            }
        }
        self.write_text(workspace_id, path, &combined)
    }

    pub fn mkdir(&self, workspace_id: &str, path: &str) -> CoreResult<WorksStat> {
        let rel = normalize_rel_path(path)?;
        if rel.is_empty() || rel == "." {
            return Err(CoreError::coded(
                "WORKS_ROOT_FORBIDDEN",
                "cannot mkdir workspace root",
            ));
        }
        let abs = self.resolve(workspace_id, &rel, true)?;
        fs::create_dir_all(&abs)?;
        let jail = self.jail_canonical(workspace_id)?;
        let canon = fs::canonicalize(&abs)?;
        ensure_under_jail(&canon, &jail)?;
        self.stat(workspace_id, &rel)
    }

    pub fn rename(&self, workspace_id: &str, from: &str, to: &str) -> CoreResult<WorksStat> {
        let from_rel = normalize_rel_path(from)?;
        let to_rel = normalize_rel_path(to)?;
        if from_rel.is_empty() || to_rel.is_empty() {
            return Err(CoreError::coded(
                "WORKS_ROOT_FORBIDDEN",
                "cannot rename workspace root",
            ));
        }
        let from_abs = self.resolve(workspace_id, &from_rel, false)?;
        let to_abs = self.resolve(workspace_id, &to_rel, true)?;
        if let Some(parent) = to_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from_abs, &to_abs)?;
        let jail = self.jail_canonical(workspace_id)?;
        let canon = fs::canonicalize(&to_abs)?;
        ensure_under_jail(&canon, &jail)?;
        self.stat(workspace_id, &to_rel)
    }

    pub fn delete(
        &self,
        workspace_id: &str,
        path: &str,
        recursive: bool,
    ) -> CoreResult<()> {
        let rel = normalize_rel_path(path)?;
        if rel.is_empty() || rel == "." {
            return Err(CoreError::coded(
                "WORKS_ROOT_FORBIDDEN",
                "cannot delete workspace root",
            ));
        }
        let abs = self.resolve(workspace_id, &rel, false)?;
        let meta = fs::metadata(&abs)?;
        if meta.is_dir() {
            if recursive {
                fs::remove_dir_all(&abs)?;
            } else {
                fs::remove_dir(&abs).map_err(|e| {
                    if e.kind() == io::ErrorKind::DirectoryNotEmpty {
                        CoreError::coded(
                            "WORKS_DIR_NOT_EMPTY",
                            "directory not empty; pass recursive=true",
                        )
                    } else {
                        CoreError::Io(e)
                    }
                })?;
            }
        } else {
            fs::remove_file(&abs)?;
        }
        Ok(())
    }
}

fn validate_workspace_id(id: &str) -> CoreResult<()> {
    if id.is_empty() || id.len() > 128 {
        return Err(CoreError::coded(
            "WORKS_INVALID_WORKSPACE",
            "invalid workspace_id",
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CoreError::coded(
            "WORKS_INVALID_WORKSPACE",
            "invalid workspace_id",
        ));
    }
    Ok(())
}

/// Normalize a client-supplied relative path. Rejects absolute / drive / escape.
fn normalize_rel_path(input: &str) -> CoreResult<String> {
    let s = input.trim();
    let s = s.trim_start_matches("./");
    if s.is_empty() || s == "." {
        return Ok(String::new());
    }
    // Reject absolute (unix + windows)
    if s.starts_with('/') || s.starts_with('\\') {
        return Err(CoreError::works_absolute_path());
    }
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return Err(CoreError::works_absolute_path());
    }
    if s.contains('\0') {
        return Err(CoreError::coded("WORKS_INVALID_PATH", "invalid path"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in s.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            // Logical escape attempt — always reject (no stack pop that could leave jail)
            return Err(CoreError::works_path_traversal());
        }
        if part.contains(':') {
            return Err(CoreError::coded("WORKS_INVALID_PATH", "invalid path component"));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn ensure_under_jail(path: &Path, jail: &Path) -> CoreResult<()> {
    if path == jail {
        return Ok(());
    }
    if path.starts_with(jail) {
        // Extra guard: next component boundary (starts_with can be fooled by prefix names
        // only if jail lacks trailing sep — Path::starts_with is component-aware on Rust).
        return Ok(());
    }
    Err(CoreError::works_path_escape("path escapes works jail"))
}

fn display_rel(abs: &Path, jail: &Path) -> CoreResult<String> {
    if abs == jail {
        return Ok(String::new());
    }
    let rel = abs
        .strip_prefix(jail)
        .map_err(|_| CoreError::works_path_escape("path escapes works jail"))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

fn system_time_ms(t: Option<SystemTime>) -> Option<u64> {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

fn is_probably_text(path: &Path, len: u64) -> bool {
    if len == 0 {
        return true;
    }
    let sample_len = std::cmp::min(len, 8192) as usize;
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let sample = &bytes[..sample_len.min(bytes.len())];
    if sample.contains(&0) {
        return false;
    }
    // Reject if high ratio of non-text control chars
    let bad = sample
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0d && b < 0x20))
        .count();
    bad * 20 < sample.len() // allow up to 5% weird controls
}

#[cfg(test)]
mod brand_dir_tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp_root() -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "kaleido_brand_dir_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn brand_dir_prefers_new_and_falls_back_to_legacy() {
        let root = tmp_root();
        // neither exists → new path
        assert_eq!(brand_dir(&root, "config"), root.join("Kaleido").join("config"));
        // legacy exists, new doesn't → legacy
        fs::create_dir_all(root.join("MuseAI").join("config")).unwrap();
        assert_eq!(brand_dir(&root, "config"), root.join("MuseAI").join("config"));
        // new exists → new wins even if legacy also present
        fs::create_dir_all(root.join("Kaleido").join("config")).unwrap();
        assert_eq!(brand_dir(&root, "config"), root.join("Kaleido").join("config"));
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod works_fs_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_data() -> (tempfile_shim::Tmp, DataRoot, WorksFs, String) {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).expect("data root");
        let fs = WorksFs::new(data.clone());
        let ws = "ws-test-001";
        (tmp, data, fs, ws.into())
    }

    /// Minimal temp dir without adding a dependency.
    mod tempfile_shim {
        use super::*;
        pub struct Tmp {
            path: PathBuf,
        }
        impl Tmp {
            pub fn new() -> Self {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!("kaleido-works-test-{nanos}"));
                fs::create_dir_all(&path).unwrap();
                Self { path }
            }
            pub fn path(&self) -> &Path {
                &self.path
            }
        }
        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    #[test]
    fn happy_path_crud() {
        let (_tmp, _data, wfs, ws) = tmp_data();
        wfs.mkdir(&ws, "notes").unwrap();
        wfs.write_text(&ws, "notes/hello.md", "# hi\n").unwrap();
        let body = wfs.read_text(&ws, "notes/hello.md").unwrap();
        assert_eq!(body.content, "# hi\n");
        let list = wfs.list(&ws, "", 2).unwrap();
        assert_eq!(list.kind, "dir");
        assert!(list.children.iter().any(|c| c.name == "notes"));
        wfs.rename(&ws, "notes/hello.md", "notes/renamed.md")
            .unwrap();
        assert!(wfs.read_text(&ws, "notes/renamed.md").is_ok());
        assert!(wfs.read_text(&ws, "notes/hello.md").is_err());
        wfs.delete(&ws, "notes", true).unwrap();
        assert!(wfs.stat(&ws, "notes").is_err());
    }

    #[test]
    fn rejects_dotdot_traversal() {
        let (_tmp, _data, wfs, ws) = tmp_data();
        let err = wfs.read_text(&ws, "../../etc/passwd").unwrap_err();
        match err {
            CoreError::Forbidden(_) | CoreError::BadRequest(_) | CoreError::Coded { .. } => {}
            other => panic!("unexpected: {other}"),
        }
        let err = wfs.write_text(&ws, "../escape.txt", "x").unwrap_err();
        match err {
            CoreError::Forbidden(_) | CoreError::BadRequest(_) | CoreError::Coded { .. } => {}
            other => panic!("unexpected: {other}"),
        }
        let err = wfs.list(&ws, "foo/../../..", 1).unwrap_err();
        match err {
            CoreError::Forbidden(_) | CoreError::BadRequest(_) | CoreError::Coded { .. } => {}
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn rejects_absolute_paths() {
        let (_tmp, _data, wfs, ws) = tmp_data();
        for p in ["/etc/passwd", "\\Windows\\System32", "C:\\secrets"] {
            let err = wfs.stat(&ws, p).unwrap_err();
            match err {
                CoreError::Forbidden(_) | CoreError::BadRequest(_) | CoreError::Coded { .. } => {}
                other => panic!("path {p}: unexpected {other}"),
            }
        }
    }

    #[test]
    fn rejects_binary_and_huge() {
        let (_tmp, _data, wfs, ws) = tmp_data();
        let huge = "x".repeat((WORKS_MAX_FILE_BYTES as usize) + 1);
        let err = wfs.write_text(&ws, "big.txt", &huge).unwrap_err();
        assert!(matches!(
            err,
            CoreError::BadRequest(_) | CoreError::Coded { .. }
        ));
        let err = wfs
            .write_text(&ws, "bin.txt", "a\0b")
            .unwrap_err();
        assert!(matches!(
            err,
            CoreError::BadRequest(_) | CoreError::Coded { .. }
        ));
    }

    #[test]
    fn symlink_escape_rejected_on_read() {
        let (_tmp, data, wfs, ws) = tmp_data();
        let jail = wfs.workspace_root(&ws).unwrap();
        // Create a file outside jail
        let outside = data.root().join("outside-secret.txt");
        fs::write(&outside, "secret").unwrap();
        // Symlink inside jail pointing out
        let link = jail.join("leak");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let err = wfs.read_text(&ws, "leak").unwrap_err();
            match err {
                CoreError::Forbidden(_)
                | CoreError::BadRequest(_)
                | CoreError::NotFound(_)
                | CoreError::Coded { .. } => {}
                other => panic!("unexpected: {other}"),
            }
            let err = wfs.stat(&ws, "leak").unwrap_err();
            match err {
                CoreError::Forbidden(_)
                | CoreError::BadRequest(_)
                | CoreError::NotFound(_)
                | CoreError::Coded { .. } => {}
                other => panic!("unexpected: {other}"),
            }
        }
    }

    #[test]
    fn normalize_rel_path_unit() {
        assert_eq!(normalize_rel_path("").unwrap(), "");
        assert_eq!(normalize_rel_path("./a/b").unwrap(), "a/b");
        assert_eq!(normalize_rel_path("a//b/./c").unwrap(), "a/b/c");
        assert!(normalize_rel_path("/etc/passwd").is_err());
        assert!(normalize_rel_path("../x").is_err());
        assert!(normalize_rel_path("a/../../b").is_err());
    }
}

#[cfg(test)]
mod job_store_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    mod tempfile_shim {
        use super::*;
        pub struct Tmp {
            path: PathBuf,
        }
        impl Tmp {
            pub fn new() -> Self {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!("kaleido-jobs-test-{nanos}"));
                fs::create_dir_all(&path).unwrap();
                Self { path }
            }
            pub fn path(&self) -> &Path {
                &self.path
            }
        }
        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    fn store_with_max(max: usize) -> (tempfile_shim::Tmp, JobStore) {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).expect("data root");
        // Explicit max avoids races on process-wide KALEIDO_MAX_CONCURRENT_JOBS.
        let store = JobStore::with_max_concurrent(data, max);
        (tmp, store)
    }

    #[test]
    fn normalize_status_legacy_map() {
        assert_eq!(normalize_job_status("done"), "succeeded");
        assert_eq!(normalize_job_status("error"), "failed");
        assert_eq!(normalize_job_status("stopped"), "cancelled");
        assert_eq!(normalize_job_status("running"), "running");
        assert_eq!(normalize_job_status("queued"), "queued");
    }

    #[test]
    fn try_start_rate_limits_at_max() {
        let (_tmp, store) = store_with_max(1);
        let a = store
            .try_start("chat", "u1", "ws1", None, json!({}))
            .unwrap();
        assert_eq!(a.status, "running");
        let err = store
            .try_start("chat", "u1", "ws1", None, json!({}))
            .unwrap_err();
        assert!(matches!(err, CoreError::RateLimited(_)));
        store.finish(&a.run_id, "done");
        let b = store
            .try_start("chat", "u1", "ws1", None, json!({}))
            .unwrap();
        assert_eq!(normalize_job_status(&store.get(&b.run_id).unwrap().status), "running");
        // finished job maps done → succeeded on disk
        let finished = store.get(&a.run_id).unwrap();
        assert_eq!(normalize_job_status(&finished.status), "succeeded");
    }

    #[test]
    fn create_queues_when_at_capacity() {
        let (_tmp, store) = store_with_max(1);
        let a = store
            .create("background", "u1", "ws1", json!({"n": 1}), None, None)
            .unwrap();
        assert_eq!(a.status, "running");
        let b = store
            .create("book_travel", "u1", "ws1", json!({"n": 2}), None, None)
            .unwrap();
        assert_eq!(b.status, "queued");
        assert_eq!(store.queued_count(), 1);
        assert_eq!(store.running_count(), 1);

        // finish a → b promoted
        store.finish(&a.run_id, "succeeded");
        let b2 = store.get(&b.run_id).unwrap();
        assert_eq!(normalize_job_status(&b2.status), "running");
        assert_eq!(store.queued_count(), 0);
        assert_eq!(store.running_count(), 1);
    }

    #[test]
    fn list_filter_and_cancel() {
        let (_tmp, store) = store_with_max(2);
        let a = store
            .create("outline", "u1", "ws1", json!({}), None, None)
            .unwrap();
        let b = store
            .create("agent", "u1", "ws1", json!({}), None, None)
            .unwrap();
        let _c = store
            .create("other", "u2", "ws2", json!({}), None, None)
            .unwrap();

        let all = store
            .list(JobListFilter {
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert!(all.len() >= 3);

        let outlines = store
            .list(JobListFilter {
                kind: Some("outline".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(outlines.len(), 1);
        assert_eq!(outlines[0].run_id, a.run_id);

        let cancelled = store.cancel(&b.run_id).unwrap();
        assert_eq!(normalize_job_status(&cancelled.status), "cancelled");
        // idempotent
        let again = store.cancel(&b.run_id).unwrap();
        assert_eq!(normalize_job_status(&again.status), "cancelled");

        let by_status = store
            .list(JobListFilter {
                status: Some("cancelled".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert!(by_status.iter().any(|j| j.run_id == b.run_id));
    }

    #[test]
    fn push_event_and_complete() {
        let (_tmp, store) = store_with_max(2);
        let j = store
            .create("noop", "u1", "ws1", json!({"mode": "test"}), None, None)
            .unwrap();
        store
            .push_event(
                &j.run_id,
                JobEvent::progress("halfway", 0.5),
                Some(0.5),
                Some("cursor-1".into()),
            )
            .unwrap();
        let mid = store.get(&j.run_id).unwrap();
        assert_eq!(mid.progress, Some(0.5));
        assert_eq!(mid.cursor.as_deref(), Some("cursor-1"));
        assert!(mid.events.iter().any(|e| e.event_type == "progress"));

        let done = store
            .complete(&j.run_id, "succeeded", Some(json!({"ok": true})), None)
            .unwrap();
        assert_eq!(normalize_job_status(&done.status), "succeeded");
        assert_eq!(done.progress, Some(1.0));
        assert_eq!(done.result, Some(json!({"ok": true})));
        assert!(is_terminal_job_status(&done.status));
    }

    #[test]
    fn finish_legacy_done_error_stopped() {
        let (_tmp, store) = store_with_max(2);
        let a = store
            .try_start("chat", "u1", "ws1", None, json!({}))
            .unwrap();
        store.finish(&a.run_id, "done");
        assert_eq!(
            normalize_job_status(&store.get(&a.run_id).unwrap().status),
            "succeeded"
        );

        let b = store
            .try_start("chat", "u1", "ws1", None, json!({}))
            .unwrap();
        store.finish(&b.run_id, "error");
        assert_eq!(
            normalize_job_status(&store.get(&b.run_id).unwrap().status),
            "failed"
        );

        let c = store
            .try_start("chat", "u1", "ws1", None, json!({}))
            .unwrap();
        store.finish(&c.run_id, "stopped");
        assert_eq!(
            normalize_job_status(&store.get(&c.run_id).unwrap().status),
            "cancelled"
        );
    }

    #[test]
    fn persist_survives_reload() {
        let tmp = tempfile_shim::Tmp::new();
        // P10: 不再 set_var（进程级全局态与并行测试竞态）；容量改显式构造，语义不变。
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = JobStore::with_max_concurrent(data.clone(), 2);
        let j = store
            .create("background", "u1", "ws1", json!({"x": 1}), None, None)
            .unwrap();
        let id = j.run_id.clone();
        drop(store);

        let store2 = JobStore::with_max_concurrent(data, 2);
        let reloaded = store2.get(&id).unwrap();
        assert_eq!(reloaded.kind, "background");
        assert_eq!(normalize_job_status(&reloaded.status), "running");
        assert_eq!(reloaded.payload, Some(json!({"x": 1})));
    }

    #[test]
    fn max_concurrent_clamped_to_4() {
        // P10 根治 flake：原实现 set_var("99") + JobStore::new()，进程级 env 与并行测试
        // （persist_survives_reload 曾 set_var("2")）竞态导致偶发红。现直测解析纯函数，
        // 语义等价（new() = parse_max_concurrent(env) → with_max_concurrent 内同款 clamp）。
        assert_eq!(parse_max_concurrent(Some("99")), 4);
        assert_eq!(parse_max_concurrent(Some("3")), 3);
        assert_eq!(parse_max_concurrent(Some("2")), 2);
        // [并行 agent 2026-08-17] JobStore 并发上限 2→4（semantics 合入）
        assert_eq!(parse_max_concurrent(Some("4")), 4);
        // 边界：下限钳到 1；解析失败/未设置回落默认 2
        assert_eq!(parse_max_concurrent(Some("0")), 1);
        assert_eq!(parse_max_concurrent(Some("-5")), 2);
        assert_eq!(parse_max_concurrent(Some("abc")), 2);
        assert_eq!(parse_max_concurrent(None), 2);
    }

    /// P8: 并发水位 metrics —— create 抬升峰值、finish/cancel 计入终态计数。
    #[test]
    fn jobs_metrics_track_peak_and_terminals() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = JobStore::with_max_concurrent(data, 2);
        let m = |s: &JobStore| s.metrics.clone();

        let a = store.create("t", "u", "w", json!({}), None, None).unwrap();
        let b = store.create("t", "u", "w", json!({}), None, None).unwrap();
        let c = store.create("t", "u", "w", json!({}), None, None).unwrap(); // queued（并发=2 满）
        assert_eq!(m(&store).peak_running.load(Ordering::Relaxed), 2);
        assert_eq!(store.queued_count(), 1);
        assert_eq!(m(&store).total_created.load(Ordering::Relaxed), 3);

        store.finish(&a.run_id, "succeeded");
        assert_eq!(m(&store).total_succeeded.load(Ordering::Relaxed), 1);
        // 槽位释放 → c 被提升，峰值不变仍为 2
        assert_eq!(store.running_count(), 2);

        store.cancel(&b.run_id).unwrap();
        assert_eq!(m(&store).total_cancelled.load(Ordering::Relaxed), 1);

        store.finish(&c.run_id, "failed");
        assert_eq!(m(&store).total_failed.load(Ordering::Relaxed), 1);
        assert_eq!(store.running_count(), 0);

        // legacy 状态码映射也计入（done→succeeded, error→failed）
        let d = store.create("t", "u", "w", json!({}), None, None).unwrap();
        store.finish(&d.run_id, "done");
        assert_eq!(m(&store).total_succeeded.load(Ordering::Relaxed), 2);
        assert!(m(&store).boot_at_unix.load(Ordering::Relaxed) > 0);

        // P10: complete() 也是终态路径（director-plan 等后台任务专用），必须同样计数
        let e = store.create("t", "u", "w", json!({}), None, None).unwrap();
        store
            .complete(&e.run_id, "succeeded", Some(json!({"ok": true})), None)
            .unwrap();
        assert_eq!(m(&store).total_succeeded.load(Ordering::Relaxed), 3);
        // complete 对已取消 job 的迟到调用不复活、也不重复计数
        let f = store.create("t", "u", "w", json!({}), None, None).unwrap();
        store.cancel(&f.run_id).unwrap();
        store
            .complete(&f.run_id, "succeeded", Some(json!({})), None)
            .unwrap();
        assert_eq!(m(&store).total_cancelled.load(Ordering::Relaxed), 2);
        assert_eq!(m(&store).total_succeeded.load(Ordering::Relaxed), 3);
    }

    /// P10: totals 跨重启持久化 —— metrics.json 落盘 + boot 恢复；
    /// peak_running 保持进程作用域（不恢复，/health 语义为 since_boot）。
    #[test]
    fn jobs_metrics_persist_across_restart() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = JobStore::with_max_concurrent(data.clone(), 2);

        let a = store.create("t", "u", "w", json!({}), None, None).unwrap();
        store.finish(&a.run_id, "succeeded");
        let b = store.create("t", "u", "w", json!({}), None, None).unwrap();
        store.cancel(&b.run_id).unwrap();

        // 快照文件已落盘
        let snap: JobMetricsSnapshot = serde_json::from_str(
            &fs::read_to_string(data.jobs_dir().join(JobMetrics::FILE_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(snap.total_created, 2);
        assert_eq!(snap.total_succeeded, 1);
        assert_eq!(snap.total_cancelled, 1);

        drop(store);
        let store2 = JobStore::with_max_concurrent(data, 2);
        assert_eq!(store2.metrics.total_created.load(Ordering::Relaxed), 2);
        assert_eq!(store2.metrics.total_succeeded.load(Ordering::Relaxed), 1);
        assert_eq!(store2.metrics.total_cancelled.load(Ordering::Relaxed), 1);
        assert_eq!(store2.metrics.total_failed.load(Ordering::Relaxed), 0);
        // peak 是本进程水位：新 boot 从 0 起算
        assert_eq!(store2.metrics.peak_running.load(Ordering::Relaxed), 0);
        // 累计基数上继续累加
        let c = store2.create("t", "u", "w", json!({}), None, None).unwrap();
        assert_eq!(store2.metrics.total_created.load(Ordering::Relaxed), 3);
        store2.finish(&c.run_id, "error");
        assert_eq!(store2.metrics.total_failed.load(Ordering::Relaxed), 1);
    }
}

