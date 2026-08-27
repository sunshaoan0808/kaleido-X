//! U12 双 Agent 分工与工作流（吞噬 Openwrite「Goethe 规划 → Dante 写作」+ 滚动规划 + 写作流程调度器）。
//!
//! 角色模式：
//!   - planning（Goethe）：设定 / 大纲 / 伏笔规划，产出结构化 `PlanOutput`，规划完成后调用 handoff 交接。
//!   - writing（Dante）：消费写作窗口（`WritingWindow`）清单，接单撰写正文 / 审稿 / 运行态。
//!
//! 工作流状态机（持久化到 `data_root/dual-agent/{id}.json`，支持跨会话 resume）：
//!   STAGE_NAMES = [context_assembly, writing, review, user_confirm, styling, compression]
//!
//! Routes (all require session auth):
//!   GET  /api/v1/dual-agent/sessions                              list sessions
//!   POST /api/v1/dual-agent/sessions                              create planning session (work_id/book_id)
//!   GET  /api/v1/dual-agent/sessions/{id}                         get session
//!   POST /api/v1/dual-agent/sessions/{id}/plan                    run planning (Goethe: settings/outline/foreshadow)；携带 {instruction} 可多轮对话式迭代（U12-A1）
//!   GET  /api/v1/dual-agent/sessions/{id}/plan                    read planning output (+ pendingConfirmation)
//!   POST /api/v1/dual-agent/sessions/{id}/chat                    NL 会话：显式确认/交接触发，其余进入规划迭代（U12-A1/A2）
//!   POST /api/v1/dual-agent/sessions/{id}/confirm-plan            plan.state proposed → confirmed，确认后才允许 handoff（U12-A2）
//!   POST /api/v1/dual-agent/sessions/{id}/handoff                 planning done → writing windows → Dante takes over（U12-A3 完整协议）
//!   GET  /api/v1/dual-agent/sessions/{id}/windows                 writing window list
//!   POST /api/v1/dual-agent/sessions/{id}/windows/{wid}/write     Dante writes a window draft
//!   POST /api/v1/dual-agent/sessions/{id}/stage                   advance workflow stage (start/complete/fail/skip)
//!   POST /api/v1/dual-agent/sessions/{id}/resume                  resume interrupted session from persisted state
//!   GET  /api/v1/dual-agent/sessions/{id}/state                   current role/stage/progress
//!   GET  /api/v1/dual-agent/sessions/{id}/ledger                  ContextLedger 上下文账本（U12-A4/D3）

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use kaleido_core::LlmRuntime;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use crate::llm_stream::{chat_completion_dispatch, extract_json_value};
use crate::harness_bridge::{auto_refine_gate, run_refine, LlmClientImpl};
use crate::{session_from, AppState};
use crate::error_codes::*;

/// 双 Agent 角色名（会话级切换）。
pub const AGENT_PLANNING: &str = "planning";
pub const AGENT_WRITING: &str = "writing";

/// 写作流程调度阶段（对应 workflow_scheduler.py 的 STAGE_NAMES）。
pub const STAGE_NAMES: &[&str] = &[
    "context_assembly",
    "writing",
    "review",
    "user_confirm",
    "styling",
    "compression",
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/dual-agent/sessions",
            get(list_sessions_h).post(create_session_h),
        )
        .route("/api/v1/dual-agent/sessions/{id}", get(get_session_h))
        .route(
            "/api/v1/dual-agent/sessions/{id}/plan",
            post(plan_session_h).get(get_plan_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/chat",
            post(chat_session_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/confirm-plan",
            post(confirm_plan_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/handoff",
            post(handoff_session_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/windows",
            get(get_windows_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/windows/{window_id}/write",
            post(write_window_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/windows/{window_id}/generate",
            post(generate_window_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/windows/{window_id}/publish",
            post(publish_window_h),
        )
        .route("/api/v1/dual-agent/sessions/{id}/stage", post(advance_stage_h))
        .route(
            "/api/v1/dual-agent/sessions/{id}/review",
            post(review_session_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/styling",
            post(styling_session_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/compress",
            post(compress_session_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/resume",
            post(resume_session_h),
        )
        .route(
            "/api/v1/dual-agent/sessions/{id}/auto-confirm",
            post(set_auto_confirm_h),
        )
        .route("/api/v1/dual-agent/sessions/{id}/state", get(get_state_h))
        .route(
            "/api/v1/dual-agent/sessions/{id}/ledger",
            get(get_ledger_h),
        )
}

// ── 数据结构 ─────────────────────────────────────────────────────────────

/// 阶段执行记录（status: pending / running / completed / failed / skipped）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageRecord {
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub completed_at: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Value,
}

impl StageRecord {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            status: "pending".into(),
            started_at: String::new(),
            completed_at: String::new(),
            message: String::new(),
            data: Value::Null,
        }
    }
}

/// Goethe 规划产出（设定 / 大纲 / 伏笔 / 滚动窗口）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOutput {
    #[serde(default)]
    pub settings: Vec<Value>,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub outline: Vec<Value>,
    #[serde(default)]
    pub foreshadow_items: Vec<Value>,
    #[serde(default)]
    pub current_arc: String,
    #[serde(default)]
    pub current_window: Vec<String>,
    #[serde(default)]
    pub next_window: Vec<String>,
    #[serde(default)]
    pub next_arc_goals: Vec<String>,
    #[serde(default)]
    pub arc_summary: String,
    #[serde(default)]
    pub note: String,
    /// 提案状态：proposed（待确认） / confirmed（已确认，确认后才允许 handoff）。
    #[serde(default = "default_plan_state")]
    pub state: String,
}

fn default_plan_state() -> String {
    "proposed".into()
}

/// 写作窗口：Dante 接单的最小正文单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WritingWindow {
    pub id: String,
    pub chapter_id: String,
    pub title: String,
    /// pending | assigned | written | reviewed
    pub status: String,
    #[serde(default)]
    pub outline: String,
    #[serde(default)]
    pub prompt: String,
    pub word_target: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_at: Option<String>,
    /// [morphling WriteHERE P1 2026-08-19] 原子写作子任务 DAG。
    /// 空 = 旧单次写稿（零回归）；非空 = Dante 按拓扑序逐任务生成，依赖产出注入。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_tasks: Vec<WindowTask>,
}

/// [morphling WriteHERE P1 2026-08-19] 写作窗口的原子化子任务（递归 think/write DAG）。
/// - kind="think"：产出设计/分析/节拍决策（不直接进正文，仅作为依赖上下文）。
/// - kind="write"：产出正文片段，依赖的 think/write 产出会注入其 prompt。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowTask {
    pub id: String,
    /// think | write
    pub kind: String,
    pub goal: String,
    /// 依赖的其他任务 id（先行产出注入本任务 prompt）
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub word_target: u32,
    /// pending | done
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
}

/// 会话内对话记录（规划讨论 / 交接提示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurn {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub at: String,
}

// ── P1 审稿 / 风格 / 压缩 数据结构 ─────────────────────────────────────

/// 审稿条目（REVIEWER_SYS 输出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    /// "major" | "minor"
    pub severity: String,
    pub issue: String,
    #[serde(default)]
    pub window_id: String,
}

/// 章节压缩摘要条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterSummary {
    pub window_id: String,
    pub chapter_id: String,
    pub summary: String,
}

/// P1: 风格化窗口快照（styling 阶段产出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StyledWindow {
    pub window_id: String,
    pub styled_draft: String,
}

/// 多轮对话式规划记录（U12-A1）：记录每轮的 user/assistant 消息与业务标签。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub at: String,
}

/// ContextLedger 上下文账本条目（U12-A4/D3）：审计每步真实注入上下文量。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextLedgerEntry {
    pub stage: String,
    #[serde(default)]
    pub plan_hash: String,
    #[serde(default)]
    pub outline_chars: usize,
    #[serde(default)]
    pub foreshadow_count: usize,
    #[serde(default)]
    pub timestamp: String,
}

/// 双 Agent 会话（一等持久化资产，独立前缀 `dual-agent-*`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DualAgentSession {
    pub id: String,
    pub work_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub book_id: Option<String>,
    pub title: String,
    /// active_role: planning | writing
    pub active_role: String,
    /// 当前工作流阶段（STAGE_NAMES 之一）。
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanOutput>,
    #[serde(default)]
    pub windows: Vec<WritingWindow>,
    #[serde(default)]
    pub stages: Vec<StageRecord>,
    #[serde(default)]
    pub transcript: Vec<AgentTurn>,
    /// 多轮对话式规划记录（U12-A1）。
    #[serde(default)]
    pub chat_transcript: Vec<ChatTurn>,
    /// ContextLedger 上下文账本（U12-A4/D3）。
    #[serde(default)]
    pub context_ledger: Vec<ContextLedgerEntry>,
    pub handoff_ok: bool,
    #[serde(default)]
    pub llm_note: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub error: String,
    pub workspace_id: String,
    /// P1: 审稿结果（review 阶段产出）。
    #[serde(default)]
    pub review: Vec<ReviewItem>,
    /// P1: 章节压缩摘要（compression 阶段产出）。
    #[serde(default)]
    pub summaries: Vec<ChapterSummary>,
    /// P1: 风格化后的窗口 drafts（styling 阶段产出，None 表示未风格化）。
    #[serde(default)]
    pub styled_windows: Vec<StyledWindow>,
    /// P1: 自动确认规划提案（开启后 plan 产出自动 confirmed，跳过手动确认）。
    #[serde(default)]
    pub auto_confirm: bool,
}

// ── 请求体 ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionBody {
    work_id: String,
    #[serde(default)]
    book_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteWindowBody {
    content: String,
}

/// P0: AI 写稿请求体（可选字段，若 generate=true 则调用 LLM）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // [P7] generate=true 写稿请求体预留
struct GenerateWindowBody {
    #[serde(default)]
    pub generate: bool,
}

/// 多轮规划请求体（U12-A1）：instruction 为空且已有规划时幂等返回。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanBody {
    #[serde(default)]
    instruction: Option<String>,
}

/// 自然语言会话请求体（U12-A1/A2）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatBody {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StageBody {
    name: String,
    /// start | complete | fail | skip
    #[serde(default = "default_stage_status")]
    status: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

fn default_stage_status() -> String {
    "complete".into()
}

/// 自动确认开关请求体。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoConfirmBody {
    enabled: bool,
}

// ── 持久化（data_root/dual-agent/{id}.json）───────────────────────────────

fn dual_agent_dir(state: &AppState) -> PathBuf {
    state.auth.data_root().root().join("dual-agent")
}

fn session_path(state: &AppState, id: &str) -> PathBuf {
    dual_agent_dir(state).join(format!("{id}.json"))
}

fn safe_session_id(id: &str) -> Result<String, Response> {
    let s = id.trim();
    if s.is_empty() || s.contains('/') || s.contains('\\') || s.contains("..") {
        return Err(bad_request("DUAL_BAD_ID", "invalid dual-agent session id"));
    }
    Ok(s.to_string())
}

fn save_session(state: &AppState, s: &DualAgentSession) -> Result<(), Response> {
    let dir = dual_agent_dir(state);
    std::fs::create_dir_all(&dir).map_err(|e| {
        internal("DUAL_CREATE_DIR", format!("create dual-agent dir: {e}"))
    })?;
    let body = serde_json::to_string_pretty(s).map_err(|e| {
        internal("DUAL_SERIALIZE", format!("serialize session: {e}"))
    })?;
    let path = session_path(state, &s.id);
    let tmp = dir.join(format!("{}.tmp", s.id));
    std::fs::write(&tmp, &body).map_err(|e| {
        internal("DUAL_WRITE", format!("write session: {e}"))
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        internal("DUAL_COMMIT", format!("commit session: {e}"))
    })
}

fn load_session(state: &AppState, id: &str) -> Result<DualAgentSession, Response> {
    let id = safe_session_id(id)?;
    let body = std::fs::read_to_string(session_path(state, &id)).map_err(|_| {
        not_found("DUAL_NOT_FOUND", format!("dual-agent session not found: {id}"))
    })?;
    serde_json::from_str::<DualAgentSession>(&body).map_err(|e| {
        internal("DUAL_CORRUPT", format!("corrupt session {id}: {e}"))
    })
}

/// audit P1 IDOR: 会话归属校验——非本 workspace 的 dual-agent 会话一律 403。
/// 兼容历史空 workspace_id（旧数据）视为可见，但写操作仍受限。
fn require_workspace(
    s: &DualAgentSession,
    session: &kaleido_core::SessionRecord,
) -> Result<(), Response> {
    if !s.workspace_id.is_empty() && s.workspace_id != session.workspace_id {
        return Err(forbidden("DUAL_FORBIDDEN", "dual-agent session not in your workspace"));
    }
    Ok(())
}

fn list_sessions(state: &AppState) -> Result<Vec<DualAgentSession>, Response> {
    let dir = dual_agent_dir(state);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().map(|x| x == "json").unwrap_or(false) {
                if let Ok(body) = std::fs::read_to_string(&path) {
                    if let Ok(s) = serde_json::from_str::<DualAgentSession>(&body) {
                        out.push(s);
                    }
                }
            }
        }
    }
    Ok(out)
}

// ── 状态机工具 ─────────────────────────────────────────────────────────────

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn next_stage(name: &str) -> Option<&'static str> {
    STAGE_NAMES
        .iter()
        .position(|n| *n == name)
        .and_then(|i| STAGE_NAMES.get(i + 1).copied())
}

fn start_stage(s: &mut DualAgentSession, name: &str) {
    if let Some(st) = s.stages.iter_mut().find(|st| st.name == name) {
        st.status = "running".into();
        if st.started_at.is_empty() {
            st.started_at = now();
        }
        st.completed_at.clear();
        st.message.clear();
    }
    s.stage = name.to_string();
}

fn complete_stage(s: &mut DualAgentSession, name: &str, message: &str, data: Option<Value>) {
    if let Some(st) = s.stages.iter_mut().find(|st| st.name == name) {
        st.status = "completed".into();
        st.completed_at = now();
        st.message = message.to_string();
        if let Some(d) = data {
            st.data = d;
        }
    }
    if let Some(next) = next_stage(name) {
        s.stage = next.to_string();
    }
}

fn fail_stage(s: &mut DualAgentSession, name: &str, message: &str) {
    if let Some(st) = s.stages.iter_mut().find(|st| st.name == name) {
        st.status = "failed".into();
        st.completed_at = now();
        st.message = message.to_string();
    }
    s.error = format!("{name}: {message}");
}

fn skip_stage(s: &mut DualAgentSession, name: &str, message: &str) {
    if let Some(st) = s.stages.iter_mut().find(|st| st.name == name) {
        st.status = "skipped".into();
        st.message = if message.is_empty() { "跳过".into() } else { message.to_string() };
    }
    if let Some(next) = next_stage(name) {
        s.stage = next.to_string();
    }
}

fn role_label(role: &str) -> &'static str {
    match role {
        AGENT_WRITING => "Dante · 写作",
        _ => "Goethe · 规划",
    }
}

fn next_action(s: &DualAgentSession) -> &'static str {
    if s.plan.is_none() {
        return "run_plan";
    }
    if plan_pending_confirmation(s) {
        return "confirm_plan";
    }
    if !s.handoff_ok {
        return "run_handoff";
    }
    if s.windows.iter().any(|w| w.status != "written") {
        return "write_windows";
    }
    if s.stage != "compression" {
        return "advance_stage";
    }
    "done"
}

/// 截断辅助：将字符串截断到 max_chars，超出时加省略号。
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

fn session_state_json(s: &DualAgentSession) -> Value {
    let stage_idx = STAGE_NAMES
        .iter()
        .position(|n| *n == s.stage)
        .unwrap_or(0);
    let done = s
        .stages
        .iter()
        .filter(|st| st.status == "completed" || st.status == "skipped")
        .count();
    let written = s.windows.iter().filter(|w| w.status == "written").count();
    // ── 审稿明细（截断防大）──
    let review_details: Vec<Value> = s
        .review
        .iter()
        .map(|r| {
            json!({
                "severity": r.severity,
                "issue": truncate_str(&r.issue, 200),
                "windowId": r.window_id,
            })
        })
        .collect();
    // ── 章节摘要明细（截断防大）──
    let summary_details: Vec<Value> = s
        .summaries
        .iter()
        .map(|sm| {
            json!({
                "windowId": sm.window_id,
                "chapterId": sm.chapter_id,
                "summary": truncate_str(&sm.summary, 300),
            })
        })
        .collect();
    // ── 风格化窗口明细（截断防大）──
    let styled_details: Vec<Value> = s
        .styled_windows
        .iter()
        .map(|sw| {
            json!({
                "windowId": sw.window_id,
                "styledDraft": truncate_str(&sw.styled_draft, 300),
            })
        })
        .collect();
    json!({
        "sessionId": s.id,
        "workId": s.work_id,
        "title": s.title,
        "activeRole": s.active_role,
        "activeRoleLabel": role_label(&s.active_role),
        "stage": s.stage,
        "stageIndex": stage_idx,
        "stageCount": STAGE_NAMES.len(),
        "stageProgress": done,
        "windowsTotal": s.windows.len(),
        "windowsWritten": written,
        "planReady": s.plan.is_some(),
        "planConfirmed": s.plan.as_ref().map(|p| p.state == "confirmed").unwrap_or(false),
        "pendingConfirmation": plan_pending_confirmation(s),
        "handoffDone": s.handoff_ok,
        "llmNote": s.llm_note,
        "error": s.error,
        "nextAction": next_action(s),
        "createdAt": s.created_at,
        "updatedAt": s.updated_at,
        "stages": s.stages,
        "reviewCount": s.review.len(),
        "summariesCount": s.summaries.len(),
        "styledWindowsCount": s.styled_windows.len(),
        "review": review_details,
        "summaries": summary_details,
        "styledWindows": styled_details,
        "autoConfirm": s.auto_confirm,
    })
}

fn ok_value(v: Value) -> Response {
    Json(v).into_response()
}

// ── U12-A 上下文治理（D2 token 预算拟合 / D3 ContextLedger 账本）─────────────

/// 反馈给 Goethe 的上一轮规划上下文 token 预算上限（A1 迭代输入裁剪）。
const PLAN_FEEDBACK_MAX_TOKENS: usize = 12000;

/// 粗略 token 估算（CJK 为主：非 ASCII 约 3 字节/词元，ASCII 约 4 字节/词元）。
fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let bytes = text.len();
    let ascii = text.bytes().filter(|b| b.is_ascii()).count();
    (bytes - ascii) / 3 + ascii / 4 + 1
}

/// 截取不超过 max_bytes 字节的 UTF-8 前缀（保证落在字符边界）。
fn take_utf8_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A4/D2：token 预算内拟合长上下文 —— 前缀保留 + 尾部摘要标记。
/// 返回 (fitted, dropped_chars)。超预算时对长上下文（plan outline / 窗口 draft）
/// 做前缀保留 + 尾部摘要裁剪，避免 serde/写入时全量溢出报错。
pub fn fit_text_to_token_budget(text: &str, max_tokens: usize) -> (String, usize) {
    let original_chars = text.chars().count();
    if max_tokens == 0 || text.is_empty() {
        return (String::new(), original_chars);
    }
    if estimate_tokens(text) <= max_tokens {
        return (text.to_string(), 0);
    }
    const TAIL: &str = "\n…（已省略尾部 N 字符，按 token 预算裁剪）";
    let tail_tokens = estimate_tokens(TAIL);
    let mut low = 0usize;
    let mut high = text.len();
    while low < high {
        let mid = low + (high - low + 1) / 2;
        let prefix = take_utf8_prefix(text, mid);
        if estimate_tokens(prefix) + tail_tokens <= max_tokens {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let prefix = take_utf8_prefix(text, low);
    let dropped = original_chars - prefix.chars().count();
    let mut fitted = prefix.to_string();
    fitted.push_str(&TAIL.replace('N', &dropped.to_string()));
    (fitted, dropped)
}

/// 规划内容指纹（账本用，短哈希）。
fn plan_hash(p: &PlanOutput) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(p)
        .unwrap_or_else(|_| "{}".to_string())
        .hash(&mut h);
    format!("{:016x}", h.finish())
}

/// 当前规划 outline 序列化的字符数（审计注入上下文量）。
fn outline_chars(s: &DualAgentSession) -> usize {
    s.plan
        .as_ref()
        .and_then(|p| serde_json::to_string(&p.outline).ok())
        .map(|v| v.chars().count())
        .unwrap_or(0)
}

fn foreshadow_count(s: &DualAgentSession) -> usize {
    s.plan.as_ref().map(|p| p.foreshadow_items.len()).unwrap_or(0)
}

fn plan_pending_confirmation(s: &DualAgentSession) -> bool {
    s.plan.as_ref().map(|p| p.state != "confirmed").unwrap_or(true)
}

/// 记录一条多轮对话（U12-A1 chat_transcript）。
fn push_chat(s: &mut DualAgentSession, role: &str, content: &str, tag: &str) {
    s.chat_transcript.push(ChatTurn {
        role: role.into(),
        content: content.into(),
        tag: tag.into(),
        at: now(),
    });
}

/// 记录一条 ContextLedger 账本（U12-A4/D3）。
fn ledger_push(
    s: &mut DualAgentSession,
    stage: &str,
    outline_chars: usize,
    foreshadow_count: usize,
) {
    s.context_ledger.push(ContextLedgerEntry {
        stage: stage.to_string(),
        plan_hash: s
            .plan
            .as_ref()
            .map(plan_hash)
            .unwrap_or_else(|| "none".to_string()),
        outline_chars,
        foreshadow_count,
        timestamp: now(),
    });
}

// ── U12-A2 自然语言触发识别（参考 goethe.py _confirm_outline_if_explicit / _looks_like_handoff_request）

/// 输入是否构成显式确认（确认 / ok / 没问题 / 就按这个 …）。
fn looks_like_explicit_confirm(text: &str) -> bool {
    let normalized: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    if normalized.is_empty() {
        return false;
    }
    const OVERRIDE_NEGATIVE: &[&str] = &[
        "不要再确认",
        "不用确认",
        "无需确认",
        "别再确认",
        "不需要确认",
        "直接应用",
        "直接修改",
        "直接写入",
        "直接保存",
    ];
    const NEGATIVE: &[&str] = &[
        "不确认", "不同意", "先不", "暂不", "不要", "别改", "取消", "放弃",
    ];
    const SHORT: &[&str] = &[
        "确认", "同意", "可以", "可以的", "应用", "保存", "提交", "就这样", "改吧", "好", "好的",
        "行", "行的", "嗯", "yes", "y", "ok", "okay", "apply", "confirm", "lgtm",
    ];
    const EXPLICIT: &[&str] = &[
        "确认应用",
        "确认执行",
        "确认修改",
        "确认大纲",
        "确认写入",
        "确认",
        "同意修改",
        "同意应用",
        "应用这版",
        "应用修改",
        "写入大纲",
        "保存修改",
        "保存大纲",
        "采用这版",
        "就按这版",
        "就按这个",
        "没问题",
        "提交修改",
        "可以应用",
        "可以写入",
        "可以保存",
    ];
    if OVERRIDE_NEGATIVE.iter().any(|m| normalized.contains(m)) {
        return true;
    }
    if NEGATIVE.iter().any(|m| normalized.contains(m)) {
        return false;
    }
    let core = normalized.trim_matches(|c: char| {
        c.is_ascii_punctuation() || matches!(c, '。' | '，' | '！' | '？' | '、')
    });
    if SHORT.iter().any(|m| core == *m) {
        return true;
    }
    EXPLICIT.iter().any(|m| normalized.contains(m))
}

/// 输入是否构成 handoff 触发（交接 / 开始写 / 递给写作 / 交给Dante / dante来 …）。
fn looks_like_handoff_request(text: &str) -> bool {
    let lowered = text.trim().to_lowercase();
    if lowered.is_empty() {
        return false;
    }
    const TOKENS: &[&str] = &[
        "交接",
        "handoff",
        "开始写",
        "递给写作",
        "交给 dante",
        "交给dante",
        "dante 来",
        "dante来",
        "切到 dante",
        "切到dante",
        "切换到 dante",
        "切换到dante",
    ];
    TOKENS.iter().any(|t| lowered.contains(t))
}

// ── handler：sessions ─────────────────────────────────────────────────────

async fn list_sessions_h(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match list_sessions(&state) {
        Ok(mut list) => {
            list.retain(|s| s.workspace_id == session.workspace_id || s.workspace_id.is_empty());
            list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            let states: Vec<Value> = list.iter().map(session_state_json).collect();
            ok_value(json!({
                "ok": true,
                "count": states.len(),
                "sessions": states,
            }))
        }
        Err(r) => r,
    }
}

async fn create_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateSessionBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let work_id = body.work_id.trim().to_string();
    if work_id.is_empty() {
        return bad_request("DUAL_WORKID_REQUIRED", "workId is required");
    }
    let ts = now();
    let title = body
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("双Agent · {work_id}"));
    let stages: Vec<StageRecord> = STAGE_NAMES.iter().map(|n| StageRecord::new(n)).collect();
    let s = DualAgentSession {
        id: format!("dual-agent-{}", Uuid::new_v4()),
        work_id: work_id.clone(),
        book_id: body
            .book_id
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty()),
        title,
        active_role: AGENT_PLANNING.into(),
        stage: STAGE_NAMES[0].into(),
        plan: None,
        windows: vec![],
        stages,
        transcript: vec![AgentTurn {
            role: "assistant".into(),
            content: "Goethe 规划会话已创建。建议顺序：1) 执行规划 2) 交接 Dante 写作".into(),
            at: ts.clone(),
        }],
        chat_transcript: vec![],
        context_ledger: vec![],
        handoff_ok: false,
        llm_note: String::new(),
        created_at: ts.clone(),
        updated_at: ts,
        error: String::new(),
        workspace_id: session.workspace_id.clone(),
        review: vec![],
        summaries: vec![],
        styled_windows: vec![],
        auto_confirm: false,
    };
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    (
        StatusCode::CREATED,
        Json(json!({"ok": true, "session": s.clone(), "state": session_state_json(&s)})),
    )
        .into_response()
}

async fn get_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match load_session(&state, &id) {
        Ok(s) => {
            if let Err(r) = require_workspace(&s, &session) {
                return r;
            }
            ok_value(json!({"ok": true, "session": s.clone(), "state": session_state_json(&s)}))
        }
        Err(r) => r,
    }
}

// ── handler：plan ─────────────────────────────────────────────────────────

/// Goethe 规划指令（产出 JSON 契约，参考 goethe.py 的规划职责）。
const GOETHE_SYS: &str = "你是 Kaleido 的 Goethe，长期会话规划 Agent。\
你的职责：汇总设定、收敛人物/设定/大纲、埋设伏笔，产出结构化规划资产。\
只输出 JSON，不输出 Markdown、不输出解释文字。\
JSON 结构必须为：{\"settings\":[{\"key\":\"...\",\"value\":\"...\"}],\
\"direction\":\"全书一句话方向\",\
\"outline\":[{\"chapter\":\"第1章\",\"title\":\"...\",\"goal\":\"本章目标/事件\",\"characters\":[\"角色名\"]}],\
\"foreshadowItems\":[{\"id\":\"f1\",\"desc\":\"...\",\"plantChapter\":\"第1章\",\"payoffChapter\":\"第3章\"}],\
\"currentArc\":\"当前弧\",\
\"currentWindow\":[\"已写章节\"],\
\"nextWindow\":[\"待规划章节\"],\
\"nextArcGoals\":[\"下一弧候选目标\"],\
\"arcSummary\":\"当前弧摘要\"}";

// ── context_assembly：真实上下文装配 ──────────────────────────────────────

/// 过滤掉备份/隐藏文件（如 `_recover_ver.md`）。
fn is_backup_file(name: &str) -> bool {
    name.starts_with('_') || name.starts_with('.') || name.ends_with('~')
}

/// sanitize work_id：只保留安全字符，防止路径逃逸。
fn sanitize_work_id(work_id: &str) -> String {
    work_id
        .chars()
        .filter(|c| {
            matches!(c,
                'A'..='Z' | 'a'..='z' | '0'..='9'
                | '_' | '-' | '.'
                | '\u{4E00}'..='\u{9FFF}'
            )
        })
        .collect()
}

/// 从作品目录装配真实上下文：已写章节 .md + outlines/ 下的大纲文件。
/// work_id 不安全或目录不存在时安全降级（返回空串），不 panic。
fn assemble_work_context(state: &AppState, s: &DualAgentSession) -> String {
    let ws_root = match state.works.workspace_root(&s.workspace_id) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };

    let safe_work_id = sanitize_work_id(&s.work_id);
    let work_dir = ws_root.join(&safe_work_id);
    // 如果 work_id 对应目录不存在，回退到 workspace_root 扫描
    let scan_dir = if work_dir.is_dir() { work_dir.clone() } else { ws_root.clone() };

    // ── a. 已写章节 .md 文件 ──
    let mut md_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&scan_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().map(|e| e == "md").unwrap_or(false) {
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                if !is_backup_file(&name) {
                    md_files.push(p);
                }
            }
        }
    }
    md_files.sort();
    md_files.truncate(3); // 最多 3 个文件

    let mut chapters_text = String::new();
    for (i, path) in md_files.iter().enumerate() {
        if let Ok(content) = std::fs::read_to_string(path) {
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let prefix = take_utf8_prefix(&content, 3000); // 每个文件前 3000 字符
            chapters_text.push_str(&format!(
                "【文稿 {}：{}】\n{}\n\n",
                i + 1,
                name,
                prefix
            ));
        }
    }

    // ── b. outlines 目录 ──
    let outline_dir = ws_root.join(&safe_work_id).join("outlines");
    let mut outline_text = String::new();
    if outline_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&outline_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&p) {
                        let prefix = take_utf8_prefix(&content, 2000);
                        outline_text.push_str(&format!(
                            "【大纲文件：{}】\n{}\n\n",
                            p.file_name().unwrap_or_default().to_string_lossy(),
                            prefix
                        ));
                    }
                }
            }
        }
    }

    // ── c. 组装并裁剪 ──
    let mut combined = String::new();
    if !chapters_text.is_empty() {
        combined.push_str(&chapters_text);
    }
    if !outline_text.is_empty() {
        combined.push_str(&outline_text);
    }
    if combined.is_empty() {
        return String::new();
    }
    let (fitted, _) = fit_text_to_token_budget(&combined, 8000);
    fitted
}

fn goethe_brief(s: &DualAgentSession) -> String {
    let book = s.book_id.clone().unwrap_or_default();
    format!(
        "作品：{}（work_id={}，book_id={}）\n\
         请按 JSON 契约输出：基础设定（settings）、全书方向（direction）、章级大纲（outline，至少 2 章）、\
         伏笔清单（foreshadowItems）、当前弧（currentArc）、下一弧候选目标（nextArcGoals）、\
         以及已写/待写章节窗口（currentWindow / nextWindow）。\
         大纲草案只进入待确认区，后续由 Dante 按写作窗口接单撰写正文。",
        s.title, s.work_id, book
    )
}

fn parse_plan(text: &str) -> Result<PlanOutput, String> {
    let v = extract_json_value(text).ok_or_else(|| "plan response contains no JSON".to_string())?;
    serde_json::from_value::<PlanOutput>(v).map_err(|e| format!("plan json mismatch: {e}"))
}

/// A1：构造 Goethe 规划 prompt —— 首轮用作品简报；迭代轮把上一轮 PlanOutput
/// （token 预算内裁剪）与用户新增指示一起喂回。
/// `work_context`：context_assembly 装配的真实文稿上下文（可为空）。
fn build_goethe_user_prompt(s: &DualAgentSession, instruction: &str, work_context: &str) -> String {
    let brief = goethe_brief(s);
    let inst = instruction.trim();
    let mut parts = vec![format!("【作品信息】\n{brief}\n")];
    // ── 注入真实上下文 ──
    if !work_context.is_empty() {
        parts.push(format!("【作品真实内容】（来自作者区文稿，用于规划参考）\n{work_context}\n"));
    }
    if let Some(p) = s.plan.as_ref() {
        parts.push("【上一轮规划】（本次迭代输入，已按 token 预算裁剪，超预算部分省略）".to_string());
        let raw = serde_json::to_string_pretty(p).unwrap_or_else(|_| "{}".to_string());
        let (fitted, dropped) = fit_text_to_token_budget(&raw, PLAN_FEEDBACK_MAX_TOKENS);
        parts.push(fitted);
        if dropped > 0 {
            parts.push(format!("（上一轮规划裁剪省略约 {dropped} 字符）"));
        }
    }
    if !inst.is_empty() {
        parts.push(format!("【用户新增指示】\n{inst}\n"));
    }
    parts.push(
        "请基于以上信息迭代更新规划，并输出**完整**规划 JSON（契约字段不变：\
         settings/direction/outline/foreshadowItems/currentArc/currentWindow/nextWindow/\
         nextArcGoals/arcSummary）。"
            .to_string(),
    );
    parts.join("\n")
}

async fn run_goethe_plan_iter(
    prov_kind: &str,
    llm: &LlmRuntime,
    s: &DualAgentSession,
    instruction: &str,
    work_context: &str,
) -> Result<PlanOutput, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let user = build_goethe_user_prompt(s, instruction, work_context);
    let text = chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        prov_kind,
        GOETHE_SYS,
        &user,
        0.1, 16384, 300,
        &client,
    )
    .await?;
    parse_plan(&text)
}

/// LLM 不可用时的启发式占位规划：保证状态机可端到端走通（handoff → 写作窗口）。
fn heuristic_plan(s: &DualAgentSession) -> PlanOutput {
    PlanOutput {
        settings: vec![
            json!({"key": "世界观", "value": format!("{} 的基础设定（待补充）", s.title)}),
            json!({"key": "主角", "value": "主角设定（待补充）"}),
        ],
        direction: format!("{} 的故事方向（待 Goethe 收敛）", s.title),
        outline: vec![
            json!({"chapter": "第1章", "title": "开端", "goal": "引出主角与核心冲突", "characters": []}),
            json!({"chapter": "第2章", "title": "推进", "goal": "展开主要情节线", "characters": []}),
        ],
        foreshadow_items: vec![json!({
            "id": "f1",
            "desc": "伏笔占位（待 Goethe 细化）",
            "plantChapter": "第1章",
            "payoffChapter": "第3章"
        })],
        current_arc: "arc_001".into(),
        current_window: vec![],
        next_window: vec!["第1章".into(), "第2章".into()],
        next_arc_goals: vec!["由 Goethe 基于全书方向提出下一弧目标".into()],
        arc_summary: "当前弧尚无可用章节摘要".into(),
        note: "启发式占位规划（LLM 未配置或不可用）".into(),
        state: "proposed".into(),
    }
}

fn heuristic_plan_iter(s: &DualAgentSession, instruction: Option<&str>) -> PlanOutput {
    let mut p = heuristic_plan(s);
    if let Some(inst) = instruction.filter(|i| !i.trim().is_empty()) {
        p.note = format!(
            "启发式占位规划（LLM 未配置或不可用；新增指示未精调）：{}",
            inst.trim()
        );
    }
    p
}

// ── P0 Dante AI 写稿引擎 ────────────────────────────────────────────────

/// Dante 写作指令（中文网文长篇风格，续写正文）。
const DANTE_SYS: &str = "你是 Kaleido 的 Dante，中文网文长篇写作 Agent。\n\
你的职责：根据 Goethe 的规划大纲、伏笔清单、作品设定和已有正文，续写当前章节正文。\n\
写作要求：\n\
1. 严格按大纲目标撰写，不偏离方向；\n\
2. 承接已有正文的剧情，不重复已有内容；\n\
3. 融入相关伏笔（伏笔描述中 plantChapter ≤ 当前章 ≤ payoffChapter 的伏笔）；\n\
4. 风格：中文网文长篇，叙事流畅，有场景感和人物对话；\n\
5. 输出纯正文，不带 Markdown 标题、不带\"好的\"自白、不带任何元注释；\n\
6. 字数约 1500-2000 字（目标字数以写作窗口 wordTarget 为准）。";

/// P0：构造 Dante 写稿 user prompt。
fn build_dante_user_prompt(s: &DualAgentSession, win: &WritingWindow) -> String {
    let mut parts = Vec::new();

    // 作品标题 + 方向
    parts.push(format!(
        "【作品信息】\n标题：{}\n全书方向：{}",
        s.title,
        s.plan.as_ref().map(|p| p.direction.as_str()).unwrap_or("未定")
    ));

    // 设定
    if let Some(p) = s.plan.as_ref() {
        if !p.settings.is_empty() {
            let settings_str: Vec<String> = p
                .settings
                .iter()
                .map(|sv| {
                    let key = sv.get("key").and_then(|v| v.as_str()).unwrap_or("?");
                    let val = sv.get("value").and_then(|v| v.as_str()).unwrap_or("?");
                    format!("  - {key}：{val}")
                })
                .collect();
            parts.push(format!("【作品设定】\n{}", settings_str.join("\n")));
        }
    }

    // 当前窗口信息
    parts.push(format!(
        "【当前写作窗口】\n章节：{}\n标题：{}\n大纲目标：{}\n目标字数：{}字",
        win.chapter_id, win.title, win.outline, win.word_target
    ));

    // 相关伏笔：plantChapter ≤ 当前章 ≤ payoffChapter
    if let Some(p) = s.plan.as_ref() {
        let relevant_foreshadow: Vec<&Value> = p
            .foreshadow_items
            .iter()
            .filter(|f| {
                let plant = f.get("plantChapter").and_then(|v| v.as_str()).unwrap_or("");
                let payoff = f.get("payoffChapter").and_then(|v| v.as_str()).unwrap_or("");
                // 伏笔描述（无精确章号匹配时，只要非空即包含）
                !plant.is_empty() || !payoff.is_empty()
            })
            .collect();
        if !relevant_foreshadow.is_empty() {
            let fs_lines: Vec<String> = relevant_foreshadow
                .iter()
                .map(|f| {
                    let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let desc = f.get("desc").and_then(|v| v.as_str()).unwrap_or("");
                    let plant = f.get("plantChapter").and_then(|v| v.as_str()).unwrap_or("?");
                    let payoff = f.get("payoffChapter").and_then(|v| v.as_str()).unwrap_or("?");
                    format!("  - [{id}] {desc}（植入：{plant}，回收：{payoff}）")
                })
                .collect();
            parts.push(format!("【相关伏笔】\n{}", fs_lines.join("\n")));
        }
    }

    // 已写窗口摘要（token 预算裁剪）
    let written_drafts: Vec<String> = s
        .windows
        .iter()
        .filter(|w| w.status == "written" && w.id != win.id)
        .filter_map(|w| {
            w.draft.as_ref().map(|d| {
                format!("【{}】\n{}", w.title, d)
            })
        })
        .collect();
    if !written_drafts.is_empty() {
        let all_drafts = written_drafts.join("\n\n");
        let (fitted, dropped) = fit_text_to_token_budget(&all_drafts, 6000);
        let mut header = "【已有章节正文（已按 token 预算裁剪，用于续写衔接）】\n以下为已写章节，仅用于剧情衔接参考。严禁复用、模仿或重复以下任何句子的开头、句式或表达。每一章必须用全新的叙述开头。".to_string();
        if dropped > 0 {
            header.push_str(&format!("（已裁剪约 {dropped} 字符）"));
        }
        parts.push(format!("{header}\n{fitted}"));
    }

    parts.push(format!(
        "请撰写「{}」的正文。输出纯正文，不带标题和元注释。\n注意：不要重复已有章节的任何句子或段落，特别是开头句式。以全新的方式开启本章，与已有章节形成连续但不重复的叙事。",
        win.title
    ));
    parts.join("\n\n")
}

/// [morphling WriteHERE P1 2026-08-19] 从 LLM 回复中稳健地提取 JSON 数组：
/// 从第一个 `[` 开始做括号配对（正确处理嵌套数组与字符串内括号），忽略前后说明文字。
/// 找不到配对 `]` 时返回 None。
fn extract_json_array(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('[')?;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return text.get(start..=i);
                }
            }
            _ => {}
        }
    }
    None
}

/// [morphling WriteHERE P1 2026-08-19] 惰性 LLM 分解：把窗口的本章分解成语义原子化的
/// think/write 子任务 DAG。任何失败（LLM 错误 / JSON 解析 / 无 write 任务）返回 None，
/// 上层回退旧单次写稿（零回归）。仅当 `KALEIDO_DUAL_DECOMPOSE` 环境变量非 0/1 时才触发。
async fn try_decompose_window(
    prov_kind: &str,
    llm: &LlmRuntime,
    s: &DualAgentSession,
    win: &WritingWindow,
) -> Option<Vec<WindowTask>> {
    if !std::env::var("KALEIDO_DUAL_DECOMPOSE")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return None;
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .ok()?;
    let mut book = format!("书名《{}》\n", s.title);
    if let Some(ref p) = s.plan {
        if !p.direction.is_empty() {
            book.push_str(&format!("【创作方向】{}\n", p.direction));
        }
        if !p.current_arc.is_empty() {
            book.push_str(&format!("【当前篇章】{}\n", p.current_arc));
        }
    }
    let user = format!(
        "为以下写作窗口设计一组原子化子任务（DAG），用于把本章从「设计决策」到「正文落地」逐级完成。\n\
{book}\
【本章标题】{title}\n\
【本章大纲目标】{outline}\n\
【目标篇幅】约 {word} 字\n\
要求：\n\
1. 产出 {n} 个子任务，至少 1 个 kind=\"write\"，其余可为 kind=\"think\"（设计/节拍分析，供 write 承接）。\n\
2. 用 deps 表达依赖：write 任务应 depend 于若干 think 或更早的 write 任务（其产出会注入 prompt）。\n\
3. 首个子任务 deps 为空。避免循环依赖。\n\
4. 用 JSON 数组输出即可（可输出一段话，把 JSON 数组放中间）。元素形如 {{\"id\":\"t1\",\"kind\":\"think\",\"goal\":\"...\",\"deps\":[],\"wordTarget\":50}}，kind 为 \"think\" 或 \"write\"，wordTarget 为字数，deps 是前置任务 id 数组，首任务 deps 为空。除 JSON 数组外其它文字忽略。",
        n = 4,
        title = win.title,
        outline = win.outline,
        word = win.word_target,
    );
    let resp = chat_completion_dispatch(
        &llm.base_url, &llm.api_key, &llm.model, prov_kind, DANTE_SYS, &user,
        0.1, 16384, 60, &client,
    )
    .await
    .ok()?;
    let trimmed = resp.trim();
    let json_str = extract_json_array(trimmed);
    let arr = serde_json::from_str::<Vec<serde_json::Value>>(json_str?).ok()?;
    let mut tasks: Vec<WindowTask> = Vec::new();
    for v in arr {
        let id = v["id"].as_str()?.to_string();
        let kind = v["kind"].as_str().unwrap_or("write").to_string();
        if kind != "think" && kind != "write" {
            continue;
        }
        let goal = v["goal"].as_str().unwrap_or("").to_string();
        if goal.trim().is_empty() {
            continue;
        }
        let deps = v["deps"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect::<Vec<_>>())
            .unwrap_or_default();
        let word = v["wordTarget"].as_u64().unwrap_or(0) as u32;
        tasks.push(WindowTask {
            id,
            kind,
            goal,
            deps,
            word_target: word,
            status: "pending".into(),
            draft: None,
        });
    }
    if tasks.iter().any(|t| t.kind == "write") {
        tracing::info!(session_id=%s.id, window_id=%win.id, n=%tasks.len(), "dante window decomposed into DAG");
        Some(tasks)
    } else {
        None
    }
}

async fn run_dante_write_iter(
    prov_kind: &str,
    llm: &LlmRuntime,
    s: &DualAgentSession,
    win: &WritingWindow,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    // [morphling WriteHERE P1 2026-08-19] 递归原子化写稿：窗口带 sub_tasks DAG 时，
    // 按拓扑序逐任务生成（think 产出注入 write 的 prompt，write 产出拼接为正文）。
    // sub_tasks 为空 → 退化为旧单次写稿（零回归）。
    let dag_tasks = if !win.sub_tasks.is_empty() {
        win.sub_tasks.clone()
    } else {
        match try_decompose_window(prov_kind, llm, s, win).await {
            Some(t) => t,
            None => Vec::new(),
        }
    };
    if !dag_tasks.is_empty() {
        let ordered = topo_sort_tasks(&dag_tasks)?;
        let mut outputs: Vec<(String, String)> = Vec::new(); // (task_id, draft)
        let mut prose = String::new();
        for task in &ordered {
            let deps_ctx = deps_context(task, &ordered, &outputs);
            let user = build_task_user_prompt(s, win, task, &deps_ctx);
            let text = match chat_completion_dispatch(
                &llm.base_url,
                &llm.api_key,
                &llm.model,
                prov_kind,
                DANTE_SYS,
                &user,
                0.1, 16384, 90,
                &client,
            )
            .await
            {
                Ok(t) => strip_dante_cot(&t),
                Err(e) => {
                    return Err(format!(
                        "dante sub-task {} ({}) failed: {e}",
                        task.id, task.kind
                    ))
                }
            };
            outputs.push((task.id.clone(), text.clone()));
            if task.kind == "write" && !text.trim().is_empty() {
                prose.push_str(text.trim());
                prose.push_str("\n\n");
            }
        }
        let assembled = prose.trim_end().to_string();
        if assembled.is_empty() {
            tracing::warn!(session_id=%s.id, window_id=%win.id, "dante DAG produced empty prose");
        }
        return Ok(assembled);
    }

    let user = build_dante_user_prompt(s, win);
    let text = chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        prov_kind,
        DANTE_SYS,
        &user,
        0.1, 16384, 90,
        &client,
    )
    .await?;
    Ok(text)
}

/// [morphling WriteHERE P1 2026-08-19] 拓扑排序（Kahn）。返回依赖先行的任务序列；
/// 二环依赖 / 未声明依赖引用返回 Err（此时上层回退单次写稿）。
fn topo_sort_tasks(tasks: &[WindowTask]) -> Result<Vec<WindowTask>, String> {
    let ids: std::collections::HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
    for t in tasks {
        for d in &t.deps {
            if !ids.contains(d) {
                return Err(format!(
                    "sub-task {} declares unknown dependency {}",
                    t.id, d
                ));
            }
        }
    }
    // Kahn：反复取出「所有依赖已完成」的任务
    let mut remaining: Vec<WindowTask> = tasks.to_vec();
    let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut order: Vec<WindowTask> = Vec::with_capacity(tasks.len());
    while !remaining.is_empty() {
        let before = remaining.len();
        let mut next_round: Vec<WindowTask> = Vec::with_capacity(remaining.len());
        let mut progressed = false;
        for t in remaining {
            if t.deps.iter().all(|d| done.contains(d)) {
                done.insert(t.id.clone());
                order.push(t);
                progressed = true;
            } else {
                next_round.push(t);
            }
        }
        remaining = next_round;
        if remaining.len() == before && !progressed {
            return Err("sub-task DAG contains a cycle".to_string());
        }
    }
    Ok(order)
}

/// [morphling WriteHERE P1 2026-08-19] 构造某任务的依赖上下文：已完成依赖产出的拼接。
fn deps_context(
    task: &WindowTask,
    _ordered: &[WindowTask],
    outputs: &[(String, String)],
) -> String {
    if task.deps.is_empty() {
        return String::new();
    }
    let mut ctx = String::from("【依赖先行产出（本任务写作时须承接）】\n");
    for d in &task.deps {
        if let Some((_, txt)) = outputs.iter().rev().find(|(id, _)| id == d) {
            if !txt.trim().is_empty() {
                ctx.push_str(&format!("—— 依赖 {d} ——\n{txt}\n\n"));
            }
        }
    }
    ctx
}

/// [morphling WriteHERE P1 2026-08-19] 构造原子子任务的 user prompt。
/// think 任务产出分析；write 任务产出正文片段。均注入依赖产出与本章大纲目标。
fn build_task_user_prompt(
    s: &DualAgentSession,
    win: &WritingWindow,
    task: &WindowTask,
    deps_ctx: &str,
) -> String {
    let mut book = format!("书名《{}》\n", s.title);
    if let Some(ref p) = s.plan {
        if !p.direction.is_empty() {
            book.push_str(&format!("【创作方向】{}\n", p.direction));
        }
        if !p.current_arc.is_empty() {
            book.push_str(&format!("【当前篇章】{}\n", p.current_arc));
        }
    }
    let role = match task.kind.as_str() {
        "think" => "你是创作设计师。为【本章】产出精确的设计决策/节拍分析，不写正文。",
        _ => "你是正文执笔。承接依赖产出，按大纲目标写出连贯的叙事正文。",
    };
    format!(
        "请完成以下原子子任务「{id}」（类型：{kind}）。\n\
{book}\
【本章标题】{title}\n\
【本章大纲目标】{outline}\n\
{deps_ctx}\
【任务目标】{goal}\n\
目标篇幅约 {word} 字。{role}\n\
直接输出成果本身，不要复述任务，不要加思考过程、不要用 markdown 标题包裹。",
        id = task.id,
        kind = task.kind,
        title = win.title,
        outline = win.outline,
        goal = task.goal,
        word = task.word_target,
    )
}

/// P0：Dante 写稿带重试（LLM 失败自动重试 1 次，再失败回退启发式）。
/// 返回 (draft, note)。LLM 不可用时直接启发式（note 标记 ERROR 前缀，供上层区分占位）。
/// [fix 2026-08-16] 产出后做两道质量闸：
///   1) 剥离 CoT / 规划尾巴（strip_dante_cot）
///   2) 与既有已写窗口查重，重叠率 > 0.5 视为复制，触发一次「去重重写」；
///      重写后仍重复则保留并显式标注。
async fn dante_write_with_retry(
    prov_kind: &str,
    llm: &LlmRuntime,
    s: &DualAgentSession,
    win: &WritingWindow,
) -> (String, String) {
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return (
            heuristic_dante_draft(s, win),
            "ERROR: LLM 未配置，已生成启发式占位草稿（非正式正文，请先配置 LLM 后重试）".to_string(),
        );
    }
    // 首次写稿（失败重试 1 次）
    let first = match run_dante_write_iter(prov_kind, llm, s, win).await {
        Ok(text) => {
            let cleaned = strip_dante_cot(&text);
            if cleaned.is_empty() {
                (heuristic_dante_draft(s, win), "Dante 产出为空，已回退占位草稿".to_string())
            } else {
                (cleaned, String::new())
            }
        }
        Err(e1) => {
            tracing::warn!(session_id=%s.id, window_id=%win.id, error=%e1, "dante write LLM failed, retrying once");
            match run_dante_write_iter(prov_kind, llm, s, win).await {
                Ok(text) => {
                    let cleaned = strip_dante_cot(&text);
                    if cleaned.is_empty() {
                        (heuristic_dante_draft(s, win), "Dante 产出为空，已回退占位草稿".to_string())
                    } else {
                        (cleaned, String::new())
                    }
                }
                Err(e2) => (
                    heuristic_dante_draft(s, win),
                    format!("ERROR: Dante LLM 失败（已重试 1 次仍失败），回退启发式草稿：{e2}"),
                ),
            }
        }
    };
    let (mut draft, mut note) = first;

    // 查重闸：与既有已写窗口比对开头重叠率
    if let Some((rate, dup_id)) = dante_max_overlap(s, win, &draft) {
        if rate > 0.5 {
            tracing::warn!(
                session_id=%s.id,
                window_id=%win.id,
                dup_window=%dup_id,
                rate=%format!("{:.2}", rate),
                "dante draft duplicates existing window, triggering dedup rewrite"
            );
            // 去重重写：携带查重上下文，要求全新开头
            let ctx = build_dante_dedup_prompt(s, win, &dup_id, rate);
            let rewrite = run_dante_dedup_rewrite(prov_kind, llm, &ctx).await;
            match rewrite {
                Some(mut r2) => {
                    r2 = strip_dante_cot(&r2);
                    let rate2 = dante_max_overlap(s, win, &r2)
                        .map(|(r, _)| r)
                        .unwrap_or(0.0);
                    if rate2 > 0.5 {
                        draft = r2;
                        note = format!(
                            "Dante 写稿完成，但与「{}」重复（重叠 {:.0}%），去重重写后仍重复，请人工检查",
                            dup_id,
                            rate2 * 100.0
                        );
                    } else {
                        draft = r2;
                        note = format!(
                            "Dante 写稿完成（去重重写成功：原稿与「{}」重叠 {:.0}%）",
                            dup_id,
                            rate * 100.0
                        );
                    }
                }
                None => {
                    note = format!(
                        "Dante 写稿完成，但与「{}」重复（重叠 {:.0}%），去重重写失败，保留原稿",
                        dup_id,
                        rate * 100.0
                    );
                }
            }
        } else {
            note = if note.is_empty() {
                "Dante AI 写稿完成（LLM 产出）".to_string()
            } else {
                note
            };
        }
    } else {
        note = if note.is_empty() {
            "Dante AI 写稿完成（LLM 产出）".to_string()
        } else {
            note
        };
    }
    (draft, note)
}

/// 构造去重重写的 user prompt：明确告知模型与哪个窗口重复，要求用全新开头重写。
fn build_dante_dedup_prompt(
    s: &DualAgentSession,
    win: &WritingWindow,
    dup_id: &str,
    rate: f64,
) -> String {
    let base = build_dante_user_prompt(s, win);
    format!(
        "{}\n\n【去重警告】上一稿与已写窗口「{}」的开头重叠率高达 {:.0}%，属于内容复制，不合格。\
          \n请用与已写章节完全不同的全新场景、全新开头句式重写本窗口「{}」的正文，\
          \n严禁复用、模仿或重复任何已有章节的句子、段落或开头表达。直接输出全新正文，不要解释。",
        base, dup_id, rate * 100.0, win.title
    )
}

/// 去重重写调用（LLM 失败返回 None，不回退启发式）。
async fn run_dante_dedup_rewrite(
    prov_kind: &str,
    llm: &LlmRuntime,
    ctx: &str,
) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .ok()?;
    chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        prov_kind,
        DANTE_SYS,
        ctx,
        0.1, 16384, 2000,
        &client,
    )
    .await
    .ok()
}

/// P0：LLM 不可用时的启发式占位草稿（保证链路可走通）。
fn heuristic_dante_draft(s: &DualAgentSession, win: &WritingWindow) -> String {
    let direction = s
        .plan
        .as_ref()
        .map(|p| p.direction.as_str())
        .unwrap_or("故事待展开");
    format!(
        "（启发式占位草稿 · LLM 未配置）\n\n章节「{}」—— {}。\n\n大纲目标：{}。\n\n全书方向：{}。\n\n正文待 Dante（LLM）填充。",
        win.title, win.chapter_id, win.outline, direction
    )
}

/// 剥离 Dante 直出正文里混入的 CoT / 规划文本。
/// 现象：模型无视 DANTE_SYS「输出纯正文」约束，把「好的，我需要写第3章…结构规划…开始写正文」
/// 等思考尾巴原样吐进 draft（afb win-03 尾部、cba 第1章开头均复现）。
/// 策略：先尝试定位「正文起点」标记（开始写正文/以下是正文…），取其后的内容；
/// 否则在文本后半段定位规划标记（好的，我需要写/结构规划/字数控制在…），从该处截断。
fn strip_dante_cot(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    // 1) BODY 起点标记（模型提示词里的分隔词），取其后的正文。优先级最高。
    const BODY_MARKERS: &[&str] = &[
        "开始写正文",
        "以下是正文",
        "正文如下：",
        "正文如下",
        "以下为正文",
        "写正文如下",
        "正文：",
        "正文:",
    ];
    for m in BODY_MARKERS {
        if let Some(pos) = t.find(m) {
            let after = t[pos + m.len()..].trim().trim_start_matches('：').trim_start_matches(':').trim();
            if after.chars().count() > 20 {
                return after.to_string();
            }
        }
    }
    let bytes = t.as_bytes();
    let len = bytes.len() as f64;
    // 2) 规划起始弱信号：一旦出现且其后跟随规划特征词（章/规划/结构/分析/字数/大纲/结尾），
    //    判定为 CoT 起点并截断（不受 50% 位置限制——规划尾巴通常紧跟正文）。
    const PLAN_STARTS: &[&str] = &[
        "好的，我需要写",
        "好的，用户需要",
        "好的，我",
        "让我仔细分析",
        "让我先规划",
        "我需要写第",
        "我先规划",
    ];
    const PLAN_FEATURES: &[&str] = &[
        "章", "规划", "结构", "分析", "字数", "大纲", "结尾", "分镜", "场景",
    ];
    for m in PLAN_STARTS {
        if let Some(pos) = t.find(m) {
            let probe = &t[pos..t.len().min(pos + 40)];
            if PLAN_FEATURES.iter().any(|f| probe.contains(f)) {
                // 该位置之后整体视为规划尾巴
                return t[..pos].trim().to_string();
            }
        }
    }
    // 3) 强规划特征词（正文里几乎不会出现）——仅在后半段截断，避免误伤正常叙述。
    let mut s = t.to_string();
    for m in &[
        "字数控制在",
        "字数要求：",
        "结构规划：",
        "大纲目标：",
        "叙事视角是",
        "写作要求：",
        "开始写正文",
    ] {
        if let Some(pos) = s.find(m) {
            if pos as f64 > len * 0.5 {
                s.truncate(pos);
                break;
            }
        }
    }
    s.trim().to_string()
}

/// 计算新稿与既有稿的开头重叠率（0.0-1.0）。
/// 专门抓「开头一字不差复制」的 bug（afb win-04 整段照抄 win-03 开头）。
/// 取两稿前 300 字符逐字符比对连续相同的部分。
fn dante_overlap_rate(new: &str, existing: &str) -> f64 {
    let n: Vec<char> = new.trim().chars().collect();
    let e: Vec<char> = existing.trim().chars().collect();
    if n.is_empty() || e.is_empty() {
        return 0.0;
    }
    let limit = 300usize.min(n.len()).min(e.len());
    if limit == 0 {
        return 0.0;
    }
    let mut same = 0usize;
    for i in 0..limit {
        if n[i] == e[i] {
            same += 1;
        } else {
            break;
        }
    }
    same as f64 / limit as f64
}

/// 找出与某窗口草稿重叠率最高的既有已写窗口，返回 (重叠率, 该窗口id)。无则 None。
fn dante_max_overlap(
    s: &DualAgentSession,
    win: &WritingWindow,
    new: &str,
) -> Option<(f64, String)> {
    s.windows
        .iter()
        .filter(|w| w.status == "written" && w.id != win.id)
        .filter_map(|w| {
            w.draft
                .as_deref()
                .map(|d| (dante_overlap_rate(new, d), w.id.clone()))
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .filter(|(rate, _)| *rate > 0.0)
}

// ── P1 AI 审稿引擎 ──────────────────────────────────────────────────────

/// 审稿指令（REVIEWER_SYS）。
const REVIEWER_SYS: &str = "你是 Kaleido 的审稿编辑 Agent。\n\
你的职责：审阅已写章节，找出写作问题（情节矛盾、人物不一致、节奏问题、语言问题等）。\n\
输出格式：JSON 数组，每个元素为 {\"severity\":\"major\"|\"minor\",\"issue\":\"问题描述\",\"windowId\":\"对应窗口ID\"}。\n\
只输出 JSON 数组，不输出任何其他文字。\n\
severity 标准：\n\
- major：情节硬伤、人物性格前后矛盾、逻辑不通（必须修改）；\n\
- minor：语言可改进、节奏可优化、细节可丰富（建议修改）。";

/// P1：构造审稿 user prompt。
fn build_reviewer_user_prompt(s: &DualAgentSession) -> String {
    let mut parts = Vec::new();
    parts.push(format!("【作品】{}\n全书方向：{}", s.title,
        s.plan.as_ref().map(|p| p.direction.as_str()).unwrap_or("未定")));

    // 已写窗口的 draft（token 预算裁剪）
    let drafts: Vec<String> = s
        .windows
        .iter()
        .filter(|w| w.status == "written")
        .filter_map(|w| {
            w.draft.as_ref().map(|d| {
                format!("【{}（{}）】\n{}", w.title, w.id, d)
            })
        })
        .collect();
    if drafts.is_empty() {
        parts.push("（尚无已写章节，请输出空数组 []）".to_string());
    } else {
        let all = drafts.join("\n\n");
        let (fitted, dropped) = fit_text_to_token_budget(&all, 8000);
        let mut header = "【已写章节正文】".to_string();
        if dropped > 0 {
            header.push_str(&format!("（已裁剪约 {dropped} 字符）"));
        }
        parts.push(format!("{header}\n{fitted}"));
    }

    parts.push("请输出审稿结果 JSON 数组。".to_string());
    parts.join("\n\n")
}

/// P1：AI 审稿（调用 LLM，返回审稿条目列表）。
async fn run_review_iter(
    prov_kind: &str,
    llm: &LlmRuntime,
    s: &DualAgentSession,
) -> Result<Vec<ReviewItem>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let user = build_reviewer_user_prompt(s);
    let text = chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        prov_kind,
        REVIEWER_SYS,
        &user,
        0.1, 16384, 2000,
        &client,
    )
    .await?;
    let v = extract_json_value(&text).ok_or_else(|| "review response contains no JSON".to_string())?;
    let items: Vec<ReviewItem> =
        serde_json::from_value(v).map_err(|e| format!("review json mismatch: {e}"))?;
    Ok(items)
}

// ── P1 风格统一引擎 ─────────────────────────────────────────────────────

/// 风格统一指令。
const STYLING_SYS: &str = "你是 Kaleido 的风格润色 Agent。\n\
你的职责：对已写章节进行风格统一，使全书文风一致。\n\
要求：\n\
1. 保持原有剧情和人物不变；\n\
2. 统一叙事视角、用词风格、句式节奏；\n\
3. 输出完整润色后的章节正文（纯正文，不带标题和元注释）；\n\
4. 保持原有长度，不要大幅删减。";

/// P1：构造风格化 user prompt。
fn build_styling_user_prompt(s: &DualAgentSession, win: &WritingWindow) -> String {
    let mut parts = Vec::new();

    // 风格指令（从 settings 取）
    let mut style_directives = Vec::new();
    if let Some(p) = s.plan.as_ref() {
        for sv in &p.settings {
            let key = sv.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let val = sv.get("value").and_then(|v| v.as_str()).unwrap_or("");
            if key.contains("文风") || key.contains("风格") || key.contains("叙事") || key.contains("视角") {
                style_directives.push(format!("  - {key}：{val}"));
            }
        }
    }
    if style_directives.is_empty() {
        parts.push("【默认风格指令】\n网文长篇风格，叙事流畅，对话自然，节奏明快。".to_string());
    } else {
        parts.push(format!("【风格指令】\n{}", style_directives.join("\n")));
    }

    // 窗口 draft
    if let Some(draft) = &win.draft {
        parts.push(format!("【原文】\n章节：{}\n\n{}", win.title, draft));
    }

    parts.push("请输出风格统一后的完整正文。".to_string());
    parts.join("\n\n")
}

/// P1：LLM 不可用时的兜底（原样返回）。
#[allow(dead_code)] // [P7] 启发式样式兜底预留（LLM styling 主路径）
fn heuristic_styling(draft: &str) -> String {
    draft.to_string()
}

// ── P1 章节压缩引擎 ─────────────────────────────────────────────────────

/// 压缩指令。
const COMPRESS_SYS: &str = "你是 Kaleido 的摘要 Agent。\n\
你的职责：对章节正文生成简洁摘要（约 100-200 字），保留核心情节、人物行为和关键信息。\n\
输出格式：JSON {\"summary\":\"摘要内容\"}。\n\
只输出 JSON，不输出其他文字。";

/// P1：构造压缩 user prompt。
fn build_compress_user_prompt(_s: &DualAgentSession, win: &WritingWindow) -> String {
    let draft = win.draft.as_deref().unwrap_or("（无正文）");
    let (fitted, dropped) = fit_text_to_token_budget(draft, 4000);
    let mut text = format!("【章节】{}（{}）\n\n{}", win.title, win.chapter_id, fitted);
    if dropped > 0 {
        text.push_str(&format!("\n（已裁剪约 {dropped} 字符）"));
    }
    text.push_str("\n\n请生成简洁摘要 JSON。");
    text
}

/// P1：LLM 不可用时的兜底（截断前 200 字节，安全按字符边界，不 panic）。
fn heuristic_compress(draft: &str) -> String {
    let trimmed = draft.trim();
    if trimmed.len() <= 200 {
        return trimmed.to_string();
    }
    // 从 200 字节处回退到最近的字符边界（中文 3 字节，避免切半个字）
    let mut end = 200;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

fn agent_llm(state: &AppState) -> LlmRuntime {
    state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    )
}

/// F4: dual_agent 各迭代函数共用的 provider kind（managed protocol > env 默认）。
/// 迭代函数只拿 `&LlmRuntime`，kind 随调用点解析后透传 dispatch。
fn agent_provider_kind(state: &AppState) -> String {
    crate::llm_stream::runtime_provider_kind(&agent_llm(state), &state.provider_kind)
}

async fn plan_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<PlanBody>>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    let instruction = body
        .and_then(|Json(b)| b.instruction)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let iterating = instruction.is_some();

    // 已有规划且本轮无新增指示 → 幂等返回当前规划（U12 行为，避免重复消耗 LLM）。
    if s.plan.is_some() && !iterating {
        return ok_value(json!({
            "ok": true,
            "idempotent": true,
            "session": s.clone(),
            "plan": s.plan.clone(),
            "llmNote": s.llm_note,
            "pendingConfirmation": plan_pending_confirmation(&s),
        }));
    }

    if iterating {
        push_chat(&mut s, "user", instruction.as_deref().unwrap_or(""), "plan");
    }
    start_stage(&mut s, "context_assembly");
    if let Err(r) = save_session(&state, &s) {
        return r;
    }

    // ── context_assembly：装配真实上下文 ──
    let work_context = assemble_work_context(&state, &s);

    let llm = agent_llm(&state);
    let prov_kind = agent_provider_kind(&state);
    let (plan, note) = if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        (
            heuristic_plan_iter(&s, instruction.as_deref()),
            if iterating {
                "LLM 未配置，已生成启发式迭代规划（确认后即可交接写作）".to_string()
            } else {
                "LLM 未配置，已生成启发式占位规划（确认后即可交接写作）".to_string()
            },
        )
    } else {
        let inst = instruction.clone().unwrap_or_default();
        match run_goethe_plan_iter(&prov_kind, &llm, &s, &inst, &work_context).await {
            Ok(p) => (
                p,
                if iterating {
                    "Goethe 规划已按新增指示迭代（LLM 产出）".to_string()
                } else {
                    "Goethe 规划完成（LLM 产出）".to_string()
                },
            ),
            Err(e) => {
                tracing::warn!(session_id=%s.id, error=%e, "goethe plan LLM failed");
                (
                    heuristic_plan_iter(&s, instruction.as_deref()),
                    format!("Goethe LLM 失败，回退启发式规划：{e}"),
                )
            }
        }
    };

    let mut plan = plan;
    plan.state = "proposed".into();
    let step = if iterating { "plan_iteration" } else { "plan" };
    s.plan = Some(plan.clone());
    s.llm_note = note.clone();
    complete_stage(&mut s, "context_assembly", "规划输出：设定 / 大纲 / 伏笔清单（提案待确认）", None);
    let summary = format!(
        "Goethe 规划{}。方向：{}；大纲章数：{}；待写窗口：{}。提案待确认，确认后可交接 Dante。",
        if iterating { "已迭代" } else { "完成" },
        plan.direction,
        plan.outline.len(),
        plan.next_window.join("、")
    );
    s.transcript.push(AgentTurn {
        role: "assistant".into(),
        content: summary.clone(),
        at: now(),
    });
    push_chat(&mut s, "assistant", &summary, step);
    let o_chars = outline_chars(&s);
    let f_count = foreshadow_count(&s);
    ledger_push(&mut s, step, o_chars, f_count);
    // ── auto_confirm：自动确认规划提案 ──
    if s.auto_confirm {
        if let Some(p) = s.plan.as_mut() {
            if p.state != "confirmed" {
                p.state = "confirmed".into();
            }
        }
        push_chat(&mut s, "assistant", "规划提案已自动确认（auto_confirm=true）", "auto_confirm");
        s.llm_note.push_str("；已自动确认");
    }
    s.updated_at = now();
    let pending = plan_pending_confirmation(&s);
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "session": s.clone(),
        "plan": s.plan.clone(),
        "llmNote": s.llm_note.clone(),
        "pendingConfirmation": pending,
        "state": session_state_json(&s),
    }))
}

async fn get_plan_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    match s.plan {
        Some(p) => ok_value(json!({
            "ok": true,
            "sessionId": s.id,
            "plan": p,
            "llmNote": s.llm_note,
            "pendingConfirmation": p.state != "confirmed",
        })),
        None => not_found("DUAL_NO_PLAN", "plan not produced yet"),
    }
}

// ── handler：confirm-plan / chat（U12-A2 规划提案待确认 + 自然语言触发）────────

async fn confirm_plan_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    _body: Option<Json<Value>>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if s.plan.is_none() {
        return bad_request("DUAL_NO_PLAN", "尚无规划产出，请先 POST /plan");
    }
    let already = s.plan.as_ref().map(|p| p.state == "confirmed").unwrap_or(false);
    if !already {
        if let Some(p) = s.plan.as_mut() {
            p.state = "confirmed".into();
        }
        push_chat(&mut s, "assistant", "规划提案已确认（plan.state=confirmed），可执行交接写作。", "confirm");
        let o_chars = outline_chars(&s);
        let f_count = foreshadow_count(&s);
        ledger_push(&mut s, "confirm_plan", o_chars, f_count);
        s.updated_at = now();
        if let Err(r) = save_session(&state, &s) {
            return r;
        }
    }
    ok_value(json!({
        "ok": true,
        "confirmed": true,
        "idempotent": already,
        "session": s.clone(),
        "plan": s.plan.clone(),
        "pendingConfirmation": false,
        "state": session_state_json(&s),
    }))
}

// ── handler：auto-confirm ────────────────────────────────────────────────

async fn set_auto_confirm_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AutoConfirmBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    s.auto_confirm = body.enabled;
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "autoConfirm": s.auto_confirm,
        "state": session_state_json(&s),
    }))
}

// ── handler：chat（自然语言会话：显式确认 / handoff 触发，其余进入对话式规划迭代）──

/// 自然语言会话：显式确认 / handoff 触发，其余进入对话式规划迭代（A1）。
async fn chat_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChatBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return bad_request("DUAL_EMPTY_MESSAGE", "message is empty");
    }
    push_chat(&mut s, "user", &message, "chat");

    let explicit_confirm = looks_like_explicit_confirm(&message);
    let handoff_request = looks_like_handoff_request(&message);

    if explicit_confirm {
        let confirmed = s.plan.is_some();
        if let Some(p) = s.plan.as_mut() {
            if p.state != "confirmed" {
                p.state = "confirmed".into();
            }
        }
        push_chat(
            &mut s,
            "assistant",
            if confirmed {
                "已收到确认：规划提案置为 confirmed，可交接写作。"
            } else {
                "尚未生成规划提案，无可确认内容；请先描述规划需求。"
            },
            "confirm",
        );
        if confirmed {
            let o_chars = outline_chars(&s);
            let f_count = foreshadow_count(&s);
            ledger_push(&mut s, "confirm_plan", o_chars, f_count);
        }
        s.updated_at = now();
        if let Err(r) = save_session(&state, &s) {
            return r;
        }
        // 同句同时表达交接意图时，确认后继续交接（确认优先级参考 goethe.py）。
        if handoff_request {
            return run_handoff_common(&state, &mut s);
        }
        return ok_value(json!({
            "ok": true,
            "confirmed": confirmed,
            "pendingConfirmation": plan_pending_confirmation(&s),
            "session": s.clone(),
            "plan": s.plan.clone(),
            "chatTranscript": s.chat_transcript,
            "state": session_state_json(&s),
        }));
    }

    if handoff_request {
        s.updated_at = now();
        if let Err(r) = save_session(&state, &s) {
            return r;
        }
        return run_handoff_common(&state, &mut s);
    }

    // 其余输入 → 对话式规划迭代：喂回上一轮 PlanOutput + 本条用户消息。
    start_stage(&mut s, "context_assembly");
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    // ── context_assembly：装配真实上下文 ──
    let work_context = assemble_work_context(&state, &s);

    let llm = agent_llm(&state);
    let prov_kind = agent_provider_kind(&state);
    let (plan, note) = if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        (
            heuristic_plan_iter(&s, Some(&message)),
            "LLM 未配置，已生成启发式迭代规划".to_string(),
        )
    } else {
        match run_goethe_plan_iter(&prov_kind, &llm, &s, &message, &work_context).await {
            Ok(p) => (p, "Goethe 规划已按对话迭代（LLM 产出）".to_string()),
            Err(e) => {
                tracing::warn!(session_id=%s.id, error=%e, "goethe chat plan LLM failed");
                (
                    heuristic_plan_iter(&s, Some(&message)),
                    format!("Goethe LLM 失败，回退启发式规划：{e}"),
                )
            }
        }
    };
    let mut plan = plan;
    plan.state = "proposed".into();
    s.plan = Some(plan.clone());
    s.llm_note = note.clone();
    complete_stage(&mut s, "context_assembly", "规划迭代完成：设定 / 大纲 / 伏笔清单已更新", None);
    let summary = format!(
        "Goethe 规划已迭代。方向：{}；大纲章数：{}。当前提案为待确认状态，确认后即可交接。",
        plan.direction,
        plan.outline.len()
    );
    s.transcript.push(AgentTurn {
        role: "assistant".into(),
        content: summary.clone(),
        at: now(),
    });
    push_chat(&mut s, "assistant", &summary, "plan_iteration");
    let o_chars = outline_chars(&s);
    let f_count = foreshadow_count(&s);
    ledger_push(&mut s, "plan_iteration", o_chars, f_count);
    // ── auto_confirm：自动确认规划提案 ──
    if s.auto_confirm {
        if let Some(p) = s.plan.as_mut() {
            if p.state != "confirmed" {
                p.state = "confirmed".into();
            }
        }
        push_chat(&mut s, "assistant", "规划提案已自动确认（auto_confirm=true）", "auto_confirm");
        s.llm_note.push_str("；已自动确认");
    }
    s.updated_at = now();
    let pending = plan_pending_confirmation(&s);
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "session": s.clone(),
        "plan": s.plan.clone(),
        "llmNote": s.llm_note.clone(),
        "pendingConfirmation": pending,
        "chatTranscript": s.chat_transcript,
        "state": session_state_json(&s),
    }))
}

// ── handler：handoff / windows ────────────────────────────────────────────

fn str_get(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 由规划大纲生成写作窗口清单（含 placeholder 兜底）。
fn build_windows(s: &DualAgentSession) -> Vec<WritingWindow> {
    let mut windows = Vec::new();
    if let Some(p) = s.plan.as_ref() {
        let mut idx = 0usize;
        for item in &p.outline {
            idx += 1;
            let raw_chapter = str_get(item, "chapter");
            let raw_title = str_get(item, "title");
            let chapter_id = if raw_chapter.is_empty() {
                format!("ch{:02}", idx)
            } else {
                raw_chapter
            };
            let title = if raw_title.is_empty() {
                format!("第{idx}章")
            } else {
                raw_title
            };
            let goal = str_get(item, "goal");
            windows.push(WritingWindow {
                id: format!("win-{idx:02}"),
                chapter_id,
                title,
                status: "pending".into(),
                outline: goal.clone(),
                prompt: format!("Dante 请依据大纲目标撰写本章正文：{goal}"),
                word_target: 2000,
                assigned_role: None,
                draft: None,
                written_at: None,
                sub_tasks: vec![],
            });
        }
    }
    if windows.is_empty() {
        for i in 1..=2 {
            windows.push(WritingWindow {
                id: format!("win-{i:02}"),
                chapter_id: format!("ch{i:02}"),
                title: format!("第{i}章"),
                status: "pending".into(),
                outline: "待规划补充大纲目标".into(),
                prompt: "Dante 请撰写本章正文。".into(),
                word_target: 2000,
                assigned_role: None,
                draft: None,
                written_at: None,
                sub_tasks: vec![],
            });
        }
    }
    windows
}

/// A3：handoff 完整性校验协议 —— {ok, blocked, error, next_action, missing_items}。
struct HandoffProtocol {
    ok: bool,
    blocked: bool,
    error: String,
    next_action: String,
    missing_items: Vec<String>,
}

/// 校验是否满足交接条件：规划已产出 + 已确认 + 大纲/伏笔非空。
/// blocked=missing_items 非空（大纲 outline 或 foreshadowItems 为空）→
/// next_action="fill_outline" / "fill_foreshadow"；ok → next_action="start_writing"。
fn handoff_check(s: &DualAgentSession) -> HandoffProtocol {
    match s.plan {
        None => HandoffProtocol {
            ok: false,
            blocked: true,
            error: "plan not produced yet".into(),
            next_action: "fill_outline".into(),
            missing_items: vec!["outline".into(), "foreshadowItems".into()],
        },
        Some(ref p) if p.state != "confirmed" => HandoffProtocol {
            ok: false,
            blocked: true,
            error: "plan pending confirmation".into(),
            next_action: "confirm_plan".into(),
            missing_items: vec!["plan_confirmation".into()],
        },
        Some(ref p) => {
            let mut missing = Vec::new();
            if p.outline.is_empty() {
                missing.push("outline".into());
            }
            if p.foreshadow_items.is_empty() {
                missing.push("foreshadowItems".into());
            }
            if !missing.is_empty() {
                let next_action = if missing.iter().any(|m| m == "outline") {
                    "fill_outline"
                } else {
                    "fill_foreshadow"
                };
                HandoffProtocol {
                    ok: false,
                    blocked: true,
                    error: "handoff blocked: 必要规划章节缺失".into(),
                    next_action: next_action.into(),
                    missing_items: missing,
                }
            } else {
                HandoffProtocol {
                    ok: true,
                    blocked: false,
                    error: String::new(),
                    next_action: "start_writing".into(),
                    missing_items: vec![],
                }
            }
        }
    }
}

fn handoff_protocol_response(p: &HandoffProtocol, s: &DualAgentSession) -> Response {
    ok_value(json!({
        "ok": p.ok,
        "blocked": p.blocked,
        "error": p.error,
        "nextAction": p.next_action,
        "missingItems": p.missing_items,
        "session": s.clone(),
        "state": session_state_json(s),
    }))
}

/// 执行交接（已交接则幂等返回既有窗口；不满足条件返回 blocked 协议）。
fn run_handoff_common(state: &AppState, s: &mut DualAgentSession) -> Response {
    if s.handoff_ok {
        return ok_value(json!({
            "ok": true,
            "blocked": false,
            "error": "",
            "nextAction": "start_writing",
            "missingItems": [],
            "idempotent": true,
            "session": s.clone(),
            "windows": s.windows.clone(),
            "state": session_state_json(s),
        }));
    }
    let proto = handoff_check(s);
    if !proto.ok {
        return handoff_protocol_response(&proto, s);
    }

    s.windows = build_windows(s);
    s.handoff_ok = true;
    s.active_role = AGENT_WRITING.into();
    start_stage(s, "writing");
    let summary = format!(
        "Goethe 已完成交接：Dante 接管写作，共 {} 个写作窗口待接单。",
        s.windows.len()
    );
    s.transcript.push(AgentTurn {
        role: "assistant".into(),
        content: summary.clone(),
        at: now(),
    });
    push_chat(s, "assistant", &summary, "handoff");
    let o_chars = outline_chars(s);
    let f_count = foreshadow_count(s);
    ledger_push(s, "handoff", o_chars, f_count);
    s.updated_at = now();
    if let Err(r) = save_session(state, s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "blocked": false,
        "error": "",
        "nextAction": "start_writing",
        "missingItems": [],
        "handoff": true,
        "session": s.clone(),
        "windows": s.windows.clone(),
        "state": session_state_json(s),
    }))
}

async fn handoff_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    run_handoff_common(&state, &mut s)
}

async fn get_windows_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    let count = s.windows.len();
    ok_value(json!({
        "ok": true,
        "sessionId": s.id,
        "handoffDone": s.handoff_ok,
        "windows": s.windows,
        "count": count,
    }))
}

async fn write_window_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, window_id)): Path<(String, String)>,
    Json(body): Json<WriteWindowBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let content = body.content.trim().to_string();
    if content.is_empty() {
        return bad_request("DUAL_EMPTY_CONTENT", "content is empty");
    }
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    if !s.handoff_ok {
        return bad_request("DUAL_NO_HANDOFF", "尚未交接，请先 POST /handoff");
    }
    let wid = window_id.clone();
    let idx = match s.windows.iter().position(|w| w.id == wid) {
        Some(i) => i,
        None => {
            return not_found("DUAL_WINDOW_NOT_FOUND", format!("window not found: {window_id}"))
        }
    };
    {
        let win = &mut s.windows[idx];
        win.status = "written".into();
        win.draft = Some(content);
        win.assigned_role = Some(AGENT_WRITING.into());
        win.written_at = Some(now());
    }
    let win = s.windows[idx].clone();
    if s.windows.iter().all(|w| w.status == "written") {
        complete_stage(&mut s, "writing", "全部写作窗口已完成，进入审稿", None);
    }
    let f_count = foreshadow_count(&s);
    ledger_push(
        &mut s,
        "write_window",
        win.draft.as_ref().map(|d| d.chars().count()).unwrap_or(0),
        f_count,
    );
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    let state = session_state_json(&s);
    ok_value(json!({
        "ok": true,
        "window": win,
        "sessionId": s.id,
        "stage": s.stage,
        "state": state,
    }))
}

// ── P0 handler：AI 写稿（generate）────────────────────────────────────────

async fn generate_window_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, window_id)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    if !s.handoff_ok {
        return bad_request("DUAL_NO_HANDOFF", "尚未交接，请先 POST /handoff");
    }
    let wid = window_id.clone();
    let idx = match s.windows.iter().position(|w| w.id == wid) {
        Some(i) => i,
        None => {
            return not_found("DUAL_WINDOW_NOT_FOUND", format!("window not found: {window_id}"))
        }
    };
    // 标记窗口为 running
    {
        let win = &mut s.windows[idx];
        win.status = "running".into();
        win.assigned_role = Some(AGENT_WRITING.into());
    }
    s.llm_note = "Dante 正在写作…".into();
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }

    let llm = agent_llm(&state);
    let prov_kind = agent_provider_kind(&state);
    let win_snapshot = s.windows[idx].clone();
    let (draft, note) = dante_write_with_retry(&prov_kind, &llm, &s, &win_snapshot).await;

    // [fix 2026-08-16] 占位稿（note 以 ERROR 前缀标记）不再静默标 written：
    // 标为 placeholder，且不推进 writing 阶段完成，避免用户误以为已产出正式正文。
    let is_placeholder = note.starts_with("ERROR:");
    // 写入窗口
    {
        let win = &mut s.windows[idx];
        win.draft = Some(draft);
        win.status = if is_placeholder { "placeholder".into() } else { "written".into() };
        win.written_at = Some(now());
    }
    s.llm_note = note.clone();
    let draft_chars = s.windows[idx].draft.as_ref().map(|d| d.chars().count()).unwrap_or(0);
    let f_count = foreshadow_count(&s);
    ledger_push(&mut s, "write_window", draft_chars, f_count);
    // 全部窗口写完且无占位 → 自动完成 writing 阶段
    let all_written = s.windows.iter().all(|w| w.status == "written");
    if all_written && !is_placeholder {
        complete_stage(&mut s, "writing", "全部写作窗口已完成，进入审稿", None);
    }
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    let state_json = session_state_json(&s);
    ok_value(json!({
        "ok": true,
        "window": s.windows[idx],
        "llmNote": note,
        "sessionId": s.id,
        "stage": s.stage,
        "state": state_json,
    }))
}

// ── P1 handler：写稿落盘（draft → 作品章节 md）─────────────────────────────

/// 落盘文件名净化：只保留安全字符，防止路径逃逸。
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .filter(|c| {
            matches!(c,
                'A'..='Z' | 'a'..='z' | '0'..='9'
                | '_' | '-' | '.'
                | '\u{4E00}'..='\u{9FFF}'
            )
        })
        .collect::<String>()
        .trim_matches('.')
        .to_string()
}

/// 把 written 窗口的 draft 落盘为作品章节 md 文件。
async fn publish_window_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, window_id)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    let idx = match s.windows.iter().position(|w| w.id == window_id) {
        Some(i) => i,
        None => {
            return not_found("DUAL_WINDOW_NOT_FOUND", format!("window not found: {window_id}"))
        }
    };
    let win = &s.windows[idx];
    if win.status != "written" || win.draft.is_none() {
        return bad_request("DUAL_NOT_WRITTEN", "窗口未完成写作，无法落盘");
    }
    let draft = win.draft.clone().unwrap_or_default();

    // ── 落盘路径：data/works/{workspace_id}/{safe_work_id}/{safe_filename}.md ──
    let ws_root = match state.works.workspace_root(&s.workspace_id) {
        Ok(r) => r,
        Err(e) => {
            return internal("DUAL_WORKSPACE_ROOT", format!("workspace root: {e}"))
        }
    };
    let safe_work_id = sanitize_work_id(&s.work_id);
    let work_dir = if safe_work_id.is_empty() {
        ws_root.clone()
    } else {
        ws_root.join(&safe_work_id)
    };

    // 文件名：chapter_id 优先，其次 title，再其次 ch{序号}
    let raw_name = if !win.chapter_id.trim().is_empty() {
        win.chapter_id.clone()
    } else if !win.title.trim().is_empty() {
        win.title.clone()
    } else {
        format!("ch{}", idx + 1)
    };
    let mut file_name = sanitize_filename(&raw_name.trim());
    if file_name.is_empty() {
        file_name = format!("ch{}", idx + 1);
    }
    if !file_name.ends_with(".md") {
        file_name.push_str(".md");
    }

    // 创建 work_dir（不存在则建）
    if let Err(e) = std::fs::create_dir_all(&work_dir) {
        return internal("DUAL_CREATE_WORK_DIR", format!("create work dir: {e}"));
    }
    let target = work_dir.join(&file_name);
    // 双保险：确认 target 在 ws_root 内（sanitize 后 join 不应逃逸，但防御性检查）
    let canonical_ws = match std::fs::canonicalize(&ws_root) {
        Ok(p) => p,
        Err(_) => ws_root.clone(),
    };
    let target_abs = if target.exists() {
        std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone())
    } else {
        target.clone()
    };
    if !target_abs.starts_with(&canonical_ws) {
        return bad_request("DUAL_PATH_ESCAPE", "落盘路径越界，已拒绝");
    }

    // 内容：标题行 + draft
    let mut content = String::new();
    if !win.title.trim().is_empty() {
        content.push_str(&format!("# {}\n\n", win.title));
    }
    content.push_str(&draft);
    if let Err(e) = std::fs::write(&target, &content) {
        return internal("DUAL_WRITE_FILE", format!("write file: {e}"));
    }

    // 记账 + 返回
    let rel_path = format!("{}/{}", work_dir.file_name().unwrap_or_default().to_string_lossy(), file_name);
    ledger_push(&mut s, "publish", draft.chars().count(), 0);
    s.llm_note = format!("已落盘章节：{rel_path}");
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "path": rel_path,
        "fileName": file_name,
        "windowId": window_id,
        "llmNote": s.llm_note,
        "sessionId": s.id,
        "state": session_state_json(&s),
    }))
}

// ── P1 handler：AI 审稿 ──────────────────────────────────────────────────

async fn review_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    if !s.handoff_ok {
        return bad_request("DUAL_NO_HANDOFF", "尚未交接");
    }
    // 校验全部窗口已 written
    let pending: Vec<&str> = s.windows.iter().filter(|w| w.status != "written").map(|w| w.id.as_str()).collect();
    if !pending.is_empty() {
        return crate::error_codes::err_with_code(
            axum::http::StatusCode::BAD_REQUEST,
            "DUAL_NOT_WRITTEN",
            "尚有窗口未完成写作",
            serde_json::json!({ "pendingWindows": pending }),
        );
    }
    start_stage(&mut s, "review");
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }

    let llm = agent_llm(&state);
    let prov_kind = agent_provider_kind(&state);
    let (review_items, note) = if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        (
            vec![],
            "LLM 未配置，跳过审稿".to_string(),
        )
    } else {
        match run_review_iter(&prov_kind, &llm, &s).await {
            Ok(items) => (items, "AI 审稿完成（LLM 产出）".to_string()),
            Err(e) => {
                tracing::warn!(session_id=%s.id, error=%e, "review LLM failed");
                (vec![], format!("审稿 LLM 失败：{e}"))
            }
        }
    };

    let major_count = review_items.iter().filter(|r| r.severity == "major").count();
    let minor_count = review_items.iter().filter(|r| r.severity == "minor").count();
    s.review = review_items.clone();
    s.llm_note = note.clone();
    let msg = format!("审稿完成：{major_count} 个 major 问题，{minor_count} 个 minor 问题");
    complete_stage(&mut s, "review", &msg, Some(json!({"major": major_count, "minor": minor_count})));
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "review": review_items,
        "majorCount": major_count,
        "minorCount": minor_count,
        "llmNote": note,
        "sessionId": s.id,
        "stage": s.stage,
        "state": session_state_json(&s),
    }))
}

// ── P1 handler：风格统一 ─────────────────────────────────────────────────

async fn styling_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    start_stage(&mut s, "styling");
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }

    let llm = agent_llm(&state);
    let prov_kind = agent_provider_kind(&state);
    let mut styled = Vec::new();
    let mut note = String::new();
    let has_llm = !llm.base_url.trim().is_empty() && !llm.api_key.trim().is_empty();

    for win in &s.windows {
        if win.status != "written" || win.draft.is_none() {
            continue;
        }
        if has_llm {
            match run_styling_iter(&prov_kind, &llm, &s, win).await {
                Ok(styled_text) => {
                    styled.push(StyledWindow {
                        window_id: win.id.clone(),
                        styled_draft: styled_text,
                    });
                    note = "风格统一完成（LLM 产出）".into();
                }
                Err(e) => {
                    tracing::warn!(session_id=%s.id, window_id=%win.id, error=%e, "styling LLM failed");
                    // 兜底：原样返回
                    styled.push(StyledWindow {
                        window_id: win.id.clone(),
                        styled_draft: win.draft.clone().unwrap_or_default(),
                    });
                    note = format!("风格统一 LLM 失败（{e}），部分窗口原样保留");
                }
            }
        } else {
            // 无 LLM 时直接原样
            styled.push(StyledWindow {
                window_id: win.id.clone(),
                styled_draft: win.draft.clone().unwrap_or_default(),
            });
            note = "LLM 未配置，风格统一跳过（原样保留）".into();
        }
    }

    s.styled_windows = styled;
    s.llm_note = note.clone();
    complete_stage(&mut s, "styling", "风格统一完成", None);
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "styledWindows": s.styled_windows.len(),
        "llmNote": note,
        "sessionId": s.id,
        "stage": s.stage,
        "state": session_state_json(&s),
    }))
}

// ── P1 handler：章节压缩 ─────────────────────────────────────────────────

async fn compress_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    start_stage(&mut s, "compression");
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }

    let llm = agent_llm(&state);
    let prov_kind = agent_provider_kind(&state);
    let mut summaries = Vec::new();
    let mut note = String::new();
    let has_llm = !llm.base_url.trim().is_empty() && !llm.api_key.trim().is_empty();

    for win in &s.windows {
        if win.status != "written" || win.draft.is_none() {
            continue;
        }
        let summary_text = if has_llm {
            match run_compress_iter(&prov_kind, &llm, &s, win).await {
                Ok(text) => {
                    note = "章节摘要完成（LLM 产出）".into();
                    text
                }
                Err(e) => {
                    tracing::warn!(session_id=%s.id, window_id=%win.id, error=%e, "compress LLM failed");
                    note = format!("摘要 LLM 失败（{e}），使用截断兜底");
                    heuristic_compress(win.draft.as_deref().unwrap_or(""))
                }
            }
        } else {
            note = "LLM 未配置，使用截断兜底".into();
            heuristic_compress(win.draft.as_deref().unwrap_or(""))
        };
        summaries.push(ChapterSummary {
            window_id: win.id.clone(),
            chapter_id: win.chapter_id.clone(),
            summary: summary_text,
        });
    }

    s.summaries = summaries;
    s.llm_note = note.clone();
    complete_stage(&mut s, "compression", "章节压缩完成", None);
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }

    // Harness P3：章节压缩完成是干净的 auto-refine 触发点（默认关闭、后台、防御性）。
    {
        let st = state.clone();
        let sess = s.clone();
        tokio::spawn(async move {
            maybe_auto_refine(st, &sess).await;
        });
    }

    ok_value(json!({
        "ok": true,
        "summariesCount": s.summaries.len(),
        "llmNote": note,
        "sessionId": s.id,
        "stage": s.stage,
        "state": session_state_json(&s),
    }))
}

/// P1：风格统一（LLM 调用）。
async fn run_styling_iter(
    prov_kind: &str,
    llm: &LlmRuntime,
    s: &DualAgentSession,
    win: &WritingWindow,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let user = build_styling_user_prompt(s, win);
    chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        prov_kind,
        STYLING_SYS,
        &user,
        0.1, 16384, 2000,
        &client,
    )
    .await
}

/// P1：章节压缩（LLM 调用，返回摘要文本）。
async fn run_compress_iter(
    prov_kind: &str,
    llm: &LlmRuntime,
    s: &DualAgentSession,
    win: &WritingWindow,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let user = build_compress_user_prompt(s, win);
    let text = chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        prov_kind,
        COMPRESS_SYS,
        &user,
        0.1, 16384, 500,
        &client,
    )
    .await?;
    // 尝试从 JSON 提取 summary
    if let Some(v) = extract_json_value(&text) {
        if let Some(summary) = v.get("summary").and_then(|s| s.as_str()) {
            return Ok(summary.to_string());
        }
    }
    // fallback: 直接返回原文
    Ok(text)
}

// ── Harness P3 auto-refine 可选钩子 ─────────────────────────────────────
//
// 默认关闭。通过 env `HARNESS_AUTO_REFINE`（=1/true）开启。在章节压缩完成等
// 干净的阶段完成点触发：gate → should_refine 才 run_refine。任何 LLM/IO 错误
// 一律吞掉只打日志，绝不影响写作主流程。手动路径见 `harness_api::refine`。
//
// P4：gate/refine 的 guidance 对齐由 `harness_bridge`（load_state 后调
// guidance_summary）在构造 PlanContext/ReviewContext 时自动注入，此钩子无需
// 额外改动；默认关闭，零行为变化。
fn harness_auto_refine_enabled() -> bool {
    match std::env::var("HARNESS_AUTO_REFINE") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        }
        Err(_) => false,
    }
}

/// 从会话摘要近似构建 auto-refine 的 conversation_tail（无摘要则回退会话标题）。
fn session_tail(s: &DualAgentSession) -> String {
    if s.summaries.is_empty() {
        return s.title.clone();
    }
    let mut tail = String::new();
    for sum in s.summaries.iter().rev().take(5) {
        tail.push_str(&sum.chapter_id);
        tail.push_str(": ");
        tail.push_str(&sum.summary);
        tail.push('\n');
    }
    tail
}

/// 触发 auto-refine：env 开启 + provider 已配置 + gate 通过才真正 run_refine。
/// 防御性：任何错误只 log，返回不可见（后台跑，不阻塞/不改变主响应）。
async fn maybe_auto_refine(state: AppState, s: &DualAgentSession) {
    if !harness_auto_refine_enabled() {
        return;
    }
    let session = s.clone();
    let root = state.auth.data_root().root().to_path_buf();
    let Some(llm) = LlmClientImpl::from_env() else {
        tracing::warn!(session_id=%session.id, "HARNESS_AUTO_REFINE enabled but LLM provider not configured; skipping auto-refine");
        return;
    };
    let tail = session_tail(&session);
    match auto_refine_gate(&root, &llm, "compact", &tail).await {
        Ok(true) => {
            let _ = run_refine(&root, &llm, &tail, None, None).await.map_err(|e| {
                tracing::warn!(session_id=%session.id, error=%e, "harness auto-refine failed");
            });
        }
        Ok(false) => tracing::debug!(session_id=%session.id, "harness auto-refine gate declined"),
        Err(e) => tracing::warn!(session_id=%session.id, error=%e, "harness auto-refine gate error"),
    }
}

// ── handler：stage / resume / state ───────────────────────────────────────

async fn advance_stage_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<StageBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    let name = body.name.trim().to_string();
    if !STAGE_NAMES.contains(&name.as_str()) {
        return crate::error_codes::err_with_code(
            axum::http::StatusCode::BAD_REQUEST,
            "DUAL_BAD_STAGE",
            format!("invalid stage: {name}"),
            serde_json::json!({ "validStages": STAGE_NAMES }),
        );
    }
    match body.status.to_ascii_lowercase().as_str() {
        "start" => start_stage(&mut s, &name),
        "complete" | "completed" | "done" => {
            complete_stage(&mut s, &name, &body.message, body.data.clone())
        }
        "fail" | "failed" => fail_stage(&mut s, &name, &body.message),
        "skip" | "skipped" => skip_stage(&mut s, &name, &body.message),
        other => {
            return crate::error_codes::err_with_code(
                axum::http::StatusCode::BAD_REQUEST,
                "DUAL_BAD_STATUS",
                format!("invalid status: {other}"),
                serde_json::json!({ "valid": ["start", "complete", "fail", "skip"] }),
            );
        }
    }
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "session": s.clone(),
        "state": session_state_json(&s),
    }))
}

async fn resume_session_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    s.error = String::new();
    let o_chars = outline_chars(&s);
    let f_count = foreshadow_count(&s);
    ledger_push(&mut s, "resume", o_chars, f_count);
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    // ── 执行真正的 resume 续跑逻辑 ──
    let (actions, note) = run_resume_logic(&state, &mut s).await;
    if !note.is_empty() {
        s.llm_note = note;
    }
    s.updated_at = now();
    if let Err(r) = save_session(&state, &s) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "resumed": true,
        "actions": actions,
        "session": s.clone(),
        "state": session_state_json(&s),
        "nextAction": next_action(&s),
    }))
}

// ── resume 核心逻辑：按阶段分派续跑 ───────────────────────────────────

/// resume 时每个阶段最多连续执行的窗口数（避免一次 resume 消耗过多 LLM 额度）。
const RESUME_MAX_WINDOWS: usize = 2;

/// 执行真正的 resume 续跑逻辑，返回 (actions, llm_note)。
/// 按当前阶段分派：writing → review → styling → compression。
async fn run_resume_logic(state: &AppState, s: &mut DualAgentSession) -> (Vec<String>, String) {
    let mut actions = Vec::new();
    let mut note = String::new();

    // ── writing 阶段 ──
    if s.stage == "writing" && s.handoff_ok {
        // 先重置所有 running 卡住的窗口为 pending
        for win in s.windows.iter_mut() {
            if win.status == "running" {
                tracing::warn!(session_id=%s.id, window_id=%win.id, "resume: resetting stuck running window to pending");
                win.status = "pending".into();
            }
        }
        let llm = agent_llm(state);
        let prov_kind = agent_provider_kind(&state);
        let mut written_count = 0usize;
        for i in 0..s.windows.len() {
            if s.windows[i].status != "pending" {
                continue;
            }
            if written_count >= RESUME_MAX_WINDOWS {
                break; // 最多连续写 RESUME_MAX_WINDOWS 个窗口
            }
            // 标记为 running
            s.windows[i].status = "running".into();
            s.windows[i].assigned_role = Some(AGENT_WRITING.into());
            let win_snapshot = s.windows[i].clone();
            // 调用 Dante 写稿（带重试，LLM 失败回退启发式）
            let (draft, win_note) = dante_write_with_retry(&prov_kind, &llm, s, &win_snapshot).await;
            // 写入窗口
            s.windows[i].draft = Some(draft);
            s.windows[i].status = "written".into();
            s.windows[i].written_at = Some(now());
            note = win_note;
            actions.push("write_window".to_string());
            written_count += 1;
        }
        // 全部窗口写完自动完成 writing 阶段
        if s.windows.iter().all(|w| w.status == "written") {
            complete_stage(s, "writing", "全部写作窗口已完成，进入审稿", None);
            actions.push("complete_writing".to_string());
        }
        return (actions, note);
    }

    // ── review 阶段：所有窗口已 written 但 review 未完成 ──
    if s.windows.iter().all(|w| w.status == "written") && !stage_completed(s, "review") {
        start_stage(s, "review");
        let llm = agent_llm(state);
        let prov_kind = agent_provider_kind(&state);
        let has_llm = !llm.base_url.trim().is_empty() && !llm.api_key.trim().is_empty();
        let (review_items, review_note) = if !has_llm {
            (vec![], "LLM 未配置，跳过审稿".to_string())
        } else {
            match run_review_iter(&prov_kind, &llm, s).await {
                Ok(items) => (items, "AI 审稿完成（LLM 产出）".to_string()),
                Err(e) => {
                    tracing::warn!(session_id=%s.id, error=%e, "resume: review LLM failed");
                    (vec![], format!("审稿 LLM 失败：{e}"))
                }
            }
        };
        let major_count = review_items.iter().filter(|r| r.severity == "major").count();
        let minor_count = review_items.iter().filter(|r| r.severity == "minor").count();
        s.review = review_items;
        note = review_note;
        let msg = format!("审稿完成：{major_count} 个 major 问题，{minor_count} 个 minor 问题");
        complete_stage(s, "review", &msg, Some(json!({"major": major_count, "minor": minor_count})));
        actions.push("review".to_string());
        return (actions, note);
    }

    // ── styling 阶段：review 已完成但 styling 未完成 ──
    if stage_completed(s, "review") && !stage_completed(s, "styling") {
        start_stage(s, "styling");
        let llm = agent_llm(state);
        let prov_kind = agent_provider_kind(&state);
        let has_llm = !llm.base_url.trim().is_empty() && !llm.api_key.trim().is_empty();
        let mut styled = Vec::new();
        for win in &s.windows {
            if win.status != "written" || win.draft.is_none() {
                continue;
            }
            if has_llm {
                match run_styling_iter(&prov_kind, &llm, s, win).await {
                    Ok(styled_text) => {
                        styled.push(StyledWindow {
                            window_id: win.id.clone(),
                            styled_draft: styled_text,
                        });
                        note = "风格统一完成（LLM 产出）".into();
                    }
                    Err(e) => {
                        tracing::warn!(session_id=%s.id, window_id=%win.id, error=%e, "resume: styling LLM failed");
                        styled.push(StyledWindow {
                            window_id: win.id.clone(),
                            styled_draft: win.draft.clone().unwrap_or_default(),
                        });
                        note = format!("风格统一 LLM 失败（{e}），部分窗口原样保留");
                    }
                }
            } else {
                styled.push(StyledWindow {
                    window_id: win.id.clone(),
                    styled_draft: win.draft.clone().unwrap_or_default(),
                });
                note = "LLM 未配置，风格统一跳过（原样保留）".into();
            }
        }
        s.styled_windows = styled;
        complete_stage(s, "styling", "风格统一完成", None);
        actions.push("styling".to_string());
        return (actions, note);
    }

    // ── compression 阶段：styling 已完成但 compression 未完成 ──
    if stage_completed(s, "styling") && !stage_completed(s, "compression") {
        start_stage(s, "compression");
        let llm = agent_llm(state);
        let prov_kind = agent_provider_kind(&state);
        let has_llm = !llm.base_url.trim().is_empty() && !llm.api_key.trim().is_empty();
        let mut summaries = Vec::new();
        for win in &s.windows {
            if win.status != "written" || win.draft.is_none() {
                continue;
            }
            let summary_text = if has_llm {
                match run_compress_iter(&prov_kind, &llm, s, win).await {
                    Ok(text) => {
                        note = "章节摘要完成（LLM 产出）".into();
                        text
                    }
                    Err(e) => {
                        tracing::warn!(session_id=%s.id, window_id=%win.id, error=%e, "resume: compress LLM failed");
                        note = format!("摘要 LLM 失败（{e}），使用截断兜底");
                        heuristic_compress(win.draft.as_deref().unwrap_or(""))
                    }
                }
            } else {
                note = "LLM 未配置，使用截断兜底".into();
                heuristic_compress(win.draft.as_deref().unwrap_or(""))
            };
            summaries.push(ChapterSummary {
                window_id: win.id.clone(),
                chapter_id: win.chapter_id.clone(),
                summary: summary_text,
            });
        }
        s.summaries = summaries;
        complete_stage(s, "compression", "章节压缩完成", None);
        actions.push("compression".to_string());
        return (actions, note);
    }

    // ── context_assembly 阶段（规划中断）──
    if s.stage == "context_assembly" && s.plan.is_none() && !s.auto_confirm {
        // 规划需要用户输入，不自动重跑，仅清 error 返回 nextAction=run_plan
        note = "规划阶段中断，需要用户手动执行规划".into();
        return (actions, note);
    }

    (actions, note)
}

/// 判断指定阶段是否已完成（status == "completed"）。
fn stage_completed(s: &DualAgentSession, name: &str) -> bool {
    s.stages.iter().any(|st| st.name == name && st.status == "completed")
}

async fn get_state_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = require_workspace(&s, &session) {
        return r;
    }
    ok_value(json!({
        "ok": true,
        "session": s.clone(),
        "state": session_state_json(&s),
    }))
}

async fn get_ledger_h(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let s = match load_session(&state, &id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    ok_value(json!({
        "ok": true,
        "sessionId": s.id,
        "count": s.context_ledger.len(),
        "ledger": s.context_ledger,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_session() -> DualAgentSession {
        DualAgentSession {
            id: "dual-agent-test".into(),
            work_id: "w".into(),
            book_id: None,
            title: "t".into(),
            active_role: AGENT_PLANNING.into(),
            stage: STAGE_NAMES[0].into(),
            plan: None,
            windows: vec![],
            stages: STAGE_NAMES.iter().map(|n| StageRecord::new(n)).collect(),
            transcript: vec![],
            chat_transcript: vec![],
            context_ledger: vec![],
            handoff_ok: false,
            llm_note: String::new(),
            created_at: "x".into(),
            updated_at: "x".into(),
            error: String::new(),
            workspace_id: "ws".into(),
            review: vec![],
            summaries: vec![],
            styled_windows: vec![],
            auto_confirm: false,
        }
    }

    #[test]
    fn u12a_fit_text_under_budget_is_passthrough() {
        let (fitted, dropped) = fit_text_to_token_budget("short text", 1000);
        assert_eq!(fitted, "short text");
        assert_eq!(dropped, 0);
    }

    #[test]
    fn stage_completed_detects_completed() {
        let mut s = test_session();
        assert!(!stage_completed(&s, "writing"));
        complete_stage(&mut s, "writing", "done", None);
        assert!(stage_completed(&s, "writing"));
    }

    #[test]
    fn truncate_str_handles_utf8_and_short() {
        assert_eq!(truncate_str("短", 5), "短");
        let long = "字".repeat(300);
        let out = truncate_str(&long, 100);
        assert!(out.chars().count() <= 101); // 100 + 省略号
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_filename_blocks_traversal_and_spaces() {
        assert_eq!(sanitize_filename("../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_filename("第1章 醒来"), "第1章醒来");
        assert_eq!(sanitize_filename("a b/c"), "abc");
        assert_eq!(sanitize_filename("..."), "");
        assert_eq!(sanitize_filename("普通章节"), "普通章节");
    }

    #[test]
    fn dante_prompt_forbids_repeating_previous_drafts() {
        // 构造一个已写窗口 + 目标窗口，验证 prompt 含「不要重复」指令
        let mut s = test_session();
        s.plan = Some(PlanOutput {
            direction: "测试方向".into(),
            ..Default::default()
        });
        s.windows.push(WritingWindow {
            id: "win-01".into(),
            chapter_id: "第1章".into(),
            title: "已写章".into(),
            status: "written".into(),
            outline: String::new(),
            prompt: String::new(),
            word_target: 2000,
            assigned_role: Some(AGENT_WRITING.into()),
            draft: Some("这是已写章节的正文。".into()),
            written_at: Some(now()),
            sub_tasks: vec![],
        });
        s.windows.push(WritingWindow {
            id: "win-02".into(),
            chapter_id: "第2章".into(),
            title: "目标章".into(),
            status: "pending".into(),
            outline: String::new(),
            prompt: String::new(),
            word_target: 2000,
            assigned_role: None,
            draft: None,
            written_at: None,
            sub_tasks: vec![],
        });
        let prompt = build_dante_user_prompt(&s, &s.windows[1]);
        assert!(prompt.contains("不要重复已有章节"), "prompt 应禁止重复: {prompt}");
        assert!(prompt.contains("严禁复用"), "prompt 应含严禁复用: {prompt}");
        assert!(prompt.contains("已写章节的正文"), "prompt 应含已有章节内容");
    }

    #[test]
    fn u12a_fit_text_truncates_long_text_with_tail_summary() {
        let long = "第".repeat(5000) + "章";
        let (fitted, dropped) = fit_text_to_token_budget(&long, 100);
        assert!(dropped > 0, "expected chars dropped");
        assert!(fitted.len() < long.len());
        assert!(fitted.contains("已省略"));
        assert!(fitted.starts_with("第"));
    }

    #[test]
    fn u12a_fit_text_zero_budget_drops_all() {
        let (fitted, dropped) = fit_text_to_token_budget("abc", 0);
        assert_eq!(fitted, "");
        assert_eq!(dropped, 3);
    }

    #[test]
    fn u12a_confirm_detection_markers() {
        assert!(looks_like_explicit_confirm("没问题，就按这个吧"));
        assert!(looks_like_explicit_confirm("确认"));
        assert!(looks_like_explicit_confirm("OK"));
        assert!(looks_like_explicit_confirm("好的，确认应用这版"));
        assert!(!looks_like_explicit_confirm("先不确认，再讨论一下"));
        assert!(!looks_like_explicit_confirm("帮我看下大纲"));
    }

    #[test]
    fn u12a_handoff_detection_markers() {
        assert!(looks_like_handoff_request("交接给 Dante 吧"));
        assert!(looks_like_handoff_request("开始写正文"));
        assert!(looks_like_handoff_request("交给 Dante 来写"));
        assert!(!looks_like_handoff_request("确认大纲没问题"));
    }

    #[test]
    fn u12a_handoff_blocked_without_confirmation() {
        let mut s = test_session();
        s.plan = Some(PlanOutput {
            outline: vec![json!({"chapter": "第1章"})],
            ..Default::default()
        });
        let proto = handoff_check(&s);
        assert!(proto.blocked);
        assert_eq!(proto.next_action, "confirm_plan");
        assert!(proto.missing_items.contains(&"plan_confirmation".to_string()));
    }

    #[test]
    fn u12a_handoff_blocked_missing_outline_and_foreshadow() {
        let mut s = test_session();
        let mut p = PlanOutput::default();
        p.state = "confirmed".into();
        s.plan = Some(p);
        let proto = handoff_check(&s);
        assert!(proto.blocked);
        assert!(proto.missing_items.contains(&"outline".to_string()));
        assert!(proto.missing_items.contains(&"foreshadowItems".to_string()));
        assert_eq!(proto.next_action, "fill_outline");
    }

    #[test]
    fn u12a_handoff_blocked_no_plan() {
        let s = test_session();
        let proto = handoff_check(&s);
        assert!(proto.blocked);
        assert!(proto.missing_items.contains(&"outline".to_string()));
        assert_eq!(proto.next_action, "fill_outline");
    }

    #[test]
    fn u12a_handoff_ok_when_complete_and_confirmed() {
        let mut s = test_session();
        let mut p = PlanOutput::default();
        p.state = "confirmed".into();
        p.outline = vec![json!({"chapter": "第1章", "title": "开端", "goal": "g"})];
        p.foreshadow_items = vec![json!({"id": "f1", "desc": "d"})];
        s.plan = Some(p);
        let proto = handoff_check(&s);
        assert!(!proto.blocked);
        assert!(proto.ok);
        assert_eq!(proto.next_action, "start_writing");
        assert!(proto.missing_items.is_empty());
    }

    #[test]
    fn u12a_next_action_requires_confirmation() {
        let mut s = test_session();
        s.plan = Some(PlanOutput::default());
        assert_eq!(next_action(&s), "confirm_plan");
    }

    #[test]
    fn u12a_heuristic_plan_keeps_handoff_walkable() {
        // U12 承诺：LLM 未配置时启发式规划仍可端到端走通（confirm→handoff→windows）。
        let mut s = test_session();
        let mut p = heuristic_plan(&s);
        assert_eq!(p.foreshadow_items.len(), 1, "启发式规划需含占位伏笔");
        p.state = "confirmed".into();
        s.plan = Some(p);
        let proto = handoff_check(&s);
        assert!(!proto.blocked);
        assert_eq!(proto.next_action, "start_writing");
    }

    #[test]
    fn u12a_ledger_records_entries() {
        let mut s = test_session();
        s.plan = Some(PlanOutput {
            outline: vec![json!({"chapter": "第1章"})],
            foreshadow_items: vec![json!({"id": "f1"})],
            ..Default::default()
        });
        let o_chars = outline_chars(&s);
        let f_count = foreshadow_count(&s);
        ledger_push(&mut s, "plan", o_chars, f_count);
        ledger_push(&mut s, "handoff", o_chars, f_count);
        assert_eq!(s.context_ledger.len(), 2);
        assert_eq!(s.context_ledger[0].stage, "plan");
        assert_eq!(s.context_ledger[1].plan_hash, s.context_ledger[0].plan_hash);
        assert!(s.context_ledger[0].outline_chars > 0);
        assert_eq!(s.context_ledger[0].foreshadow_count, 1);
    }

    #[test]
    fn u12a_plan_proposed_by_default_on_load() {
        // 旧存档无 review/summaries/styled_windows 字段时 serde default 兼容
        let raw = serde_json::json!({
            "id": "dual-agent-test",
            "workId": "w",
            "title": "t",
            "activeRole": "planning",
            "stage": "context_assembly",
            "handoffOk": false,
            "createdAt": "x",
            "updatedAt": "x",
            "workspaceId": "ws",
        });
        let s: DualAgentSession = serde_json::from_value(raw).unwrap();
        assert!(s.review.is_empty());
        assert!(s.summaries.is_empty());
        assert!(s.styled_windows.is_empty());
    }

    #[test]
    fn dante_prompt_contains_outline_and_foreshadow() {
        let mut s = test_session();
        let mut p = PlanOutput::default();
        p.direction = "全书方向测试".into();
        p.settings = vec![serde_json::json!({"key": "世界观", "value": "奇幻大陆"})];
        p.outline = vec![serde_json::json!({"chapter": "第1章", "title": "开端", "goal": "引出主角", "characters": ["李明"]})];
        p.foreshadow_items = vec![serde_json::json!({"id": "f1", "desc": "神秘符文", "plantChapter": "第1章", "payoffChapter": "第5章"})];
        s.plan = Some(p);
        s.windows = vec![WritingWindow {
            id: "win-01".into(),
            chapter_id: "第1章".into(),
            title: "开端".into(),
            status: "pending".into(),
            outline: "引出主角与核心冲突".into(),
            prompt: String::new(),
            word_target: 2000,
            assigned_role: None,
            draft: None,
            written_at: None,
            sub_tasks: vec![],
        }];
        let prompt = build_dante_user_prompt(&s, &s.windows[0]);
        assert!(prompt.contains("全书方向测试"));
        assert!(prompt.contains("奇幻大陆"));
        assert!(prompt.contains("神秘符文"));
        assert!(prompt.contains("开端"));
    }

    #[test]
    fn review_item_serde_roundtrip() {
        let item = ReviewItem {
            severity: "major".into(),
            issue: "情节矛盾".into(),
            window_id: "win-01".into(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: ReviewItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.severity, "major");
        assert_eq!(back.issue, "情节矛盾");
    }

    #[test]
    fn chapter_summary_serde_roundtrip() {
        let s = ChapterSummary {
            window_id: "win-01".into(),
            chapter_id: "ch01".into(),
            summary: "主角出场".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ChapterSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.summary, "主角出场");
    }

    #[test]
    fn fit_text_to_token_budget_does_not_panic() {
        // 确保各种边界条件不 panic
        let (_, _) = fit_text_to_token_budget("", 100);
        let (_, _) = fit_text_to_token_budget("短文本", 0);
        let long = "章".repeat(10000);
        let (fitted, dropped) = fit_text_to_token_budget(&long, 100);
        assert!(dropped > 0);
        assert!(fitted.contains("已省略"));
    }

    #[test]
    fn heuristic_compress_basic() {
        assert_eq!(heuristic_compress(""), "");
        assert_eq!(heuristic_compress("短"), "短");
        let long = "字".repeat(500);
        let result = heuristic_compress(&long);
        assert!(result.len() <= 202); // 200 chars + ellipsis
        assert!(result.ends_with('…'));
    }

    #[test]
    fn heuristic_dante_draft_contains_context() {
        let mut s = test_session();
        let mut p = PlanOutput::default();
        p.direction = "测试方向".into();
        s.plan = Some(p);
        let win = WritingWindow {
            id: "win-01".into(),
            chapter_id: "ch01".into(),
            title: "第一章".into(),
            status: "pending".into(),
            outline: "测试大纲".into(),
            prompt: String::new(),
            word_target: 2000,
            assigned_role: None,
            draft: None,
            written_at: None,
            sub_tasks: vec![],
        };
        let draft = heuristic_dante_draft(&s, &win);
        assert!(draft.contains("第一章"));
        assert!(draft.contains("测试方向"));
    }

    #[test]
    fn serde_compatibility_old_archive_no_new_fields() {
        // 模拟旧存档（无 review/summaries/styled_windows）
        let raw = serde_json::json!({
            "id": "dual-agent-old",
            "workId": "w",
            "title": "old",
            "activeRole": "planning",
            "stage": "context_assembly",
            "handoffOk": false,
            "createdAt": "x",
            "updatedAt": "x",
            "workspaceId": "ws",
            "plan": {"direction": "d", "state": "confirmed", "outline": [{"chapter": "1"}], "foreshadowItems": [{"id": "f1"}]}
        });
        let s: DualAgentSession = serde_json::from_value(raw).unwrap();
        assert!(s.review.is_empty());
        assert!(s.summaries.is_empty());
        assert!(s.styled_windows.is_empty());
        // 确保 handoff check 仍然通过
        let proto = handoff_check(&s);
        assert!(proto.ok);
    }

    // ── 2026-08-16 修复回归测试：CoT 剥离 + 查重 gate ─────────────────────

    #[test]
    fn strip_dante_cot_removes_tail_planning() {
        // 复现 afb win-03：正文末尾混入 LLM 自我规划（CoT 泄漏）
        let input = "病房里的白炽灯管嗡嗡作响，父亲坐在床边，母亲端着一壶热水走进来。\n\
                     她总觉得，她在哭。\n\
                     好的，我需要写第3章「跌倒与尿尿」，先规划结构：开头写病房场景，\
                     中间写李昊跌倒，结尾写尿床。字数控制在2000字左右。要写出场景感、对话、人物情绪。开始写正文。";
        let out = strip_dante_cot(input);
        // 剥离后不应残留规划文本
        assert!(!out.contains("好的，我需要写"), "planning tail not stripped: {out}");
        assert!(!out.contains("结构规划"), "planning tail not stripped: {out}");
        assert!(!out.contains("字数控制"), "planning tail not stripped: {out}");
        assert!(!out.contains("开始写正文"), "planning tail not stripped: {out}");
        // 正文开头应保留
        assert!(out.contains("病房里的白炽灯管"), "body head lost: {out}");
        assert!(out.contains("她在哭"), "body tail lost: {out}");
    }

    #[test]
    fn strip_dante_cot_removes_head_planning() {
        // 复现 cba 第1章：正文开头混入思考（"好的，用户需要我写第1章…让我仔细分析"）
        let input = "好的，用户需要我写第1章「异常简历」，让我仔细分析一下要求。\
                     这是一篇关于程序员在体检中心发现异常的悬疑网文。\
                     开始写正文。\n\
                     窗外的雨敲打着玻璃，李默盯着屏幕上的体检报告，手指微微发颤。";
        let out = strip_dante_cot(input);
        assert!(!out.contains("好的，用户需要"), "head CoT not stripped: {out}");
        assert!(!out.contains("让我仔细分析"), "head CoT not stripped: {out}");
        assert!(out.contains("窗外的雨敲打"), "body lost: {out}");
    }

    #[test]
    fn strip_dante_cot_keeps_clean_text() {
        // 无 CoT 的干净正文应原样保留
        let clean = "夜色渐深，她关掉台灯，把手机放到枕边。今天的一切都像梦一样。";
        assert_eq!(strip_dante_cot(clean), clean);
    }

    #[test]
    fn extract_json_array_handles_fenced_and_nested() {
        // 纯 JSON 数组
        assert_eq!(extract_json_array("[{\"id\":1}]").unwrap(), "[{\"id\":1}]");
        // 前后有说明文字
        assert_eq!(
            extract_json_array("好的，分解如下：[{\"id\":\"t1\"}] 请接续。").unwrap(),
            "[{\"id\":\"t1\"}]"
        );
        // ```json 围栏 + 嵌套数组
        let fenced = "```json\n[{\"id\":\"t1\",\"deps\":[]},{\"id\":\"t2\",\"deps\":[\"t1\"]}]\n```";
        assert_eq!(
            extract_json_array(fenced).unwrap(),
            "[{\"id\":\"t1\",\"deps\":[]},{\"id\":\"t2\",\"deps\":[\"t1\"]}]"
        );
        // 字符串内出现 ] 不应提前截断
        let inner_bracket = "[{\"goal\":\"他（完档）」结束\"}]";
        assert_eq!(extract_json_array(inner_bracket).unwrap(), inner_bracket);
        // 无数组 → None
        assert!(extract_json_array("没有数组的回复").is_none());
    }

    #[test]
    fn extract_json_array_unbalanced_returns_none() {
        // 只有 `[` 没有配对 `]`
        assert!(extract_json_array("[{\"id\":1}").is_none());
    }

    #[test]
    fn overlap_rate_detects_copy_and_distinct() {
        // 复现 afb win-04 整段复制 win-03 开头
        let win3 = "病房里的白炽灯管嗡嗡作响，父亲坐在床边，母亲端着一壶热水走进来。\n她总觉得，她在哭。";
        let win4_copy = "病房里的白炽灯管嗡嗡作响，父亲坐在床边，母亲端着一壶热水走进来。\n但她总觉得，她在哭。\n门被推开。";
        let win4_orig = "清晨的走廊里，护士推着药车，铁轮碾过水磨石地面，发出刺耳的声响。";
        let rate_copy = dante_overlap_rate(win4_copy, win3);
        let rate_orig = dante_overlap_rate(win4_orig, win3);
        assert!(rate_copy > 0.5, "copy should be flagged, got {rate_copy}");
        assert!(rate_orig < 0.2, "distinct should pass, got {rate_orig}");
    }

    #[test]
    fn max_overlap_picks_written_window_only() {
        let mut s = test_session();
        s.windows = vec![
            WritingWindow {
                id: "w1".into(),
                chapter_id: "1".into(),
                title: "一".into(),
                status: "written".into(),
                outline: "".into(),
                prompt: String::new(),
                word_target: 2000,
                assigned_role: None,
                draft: Some("病房里的白炽灯管嗡嗡作响，父亲坐在床边。".into()),
                written_at: None,
                sub_tasks: vec![],
            },
            WritingWindow {
                id: "w2".into(),
                chapter_id: "2".into(),
                title: "二".into(),
                status: "running".into(), // 未写完的不参与查重
                outline: "".into(),
                prompt: String::new(),
                word_target: 2000,
                assigned_role: None,
                draft: Some("病房里的白炽灯管嗡嗡作响，父亲坐在床边。".into()),
                written_at: None,
                sub_tasks: vec![],
            },
            WritingWindow {
                id: "w3".into(),
                chapter_id: "3".into(),
                title: "三".into(),
                status: "pending".into(),
                outline: "".into(),
                prompt: String::new(),
                word_target: 2000,
                assigned_role: None,
                draft: None,
                written_at: None,
                sub_tasks: vec![],
            },
        ];
        let cur = WritingWindow {
            id: "w3".into(),
            chapter_id: "3".into(),
            title: "三".into(),
            status: "running".into(),
            outline: "".into(),
            prompt: String::new(),
            word_target: 2000,
            assigned_role: None,
            draft: Some("病房里的白炽灯管嗡嗡作响，父亲坐在床边。".into()),
            written_at: None,
            sub_tasks: vec![],
        };
        let res = dante_max_overlap(&s, &cur, "病房里的白炽灯管嗡嗡作响，父亲坐在床边。");
        let (rate, dup) = res.expect("should find a dup against written w1");
        assert!(rate > 0.5, "overlap rate too low: {rate}");
        assert_eq!(dup, "w1", "should only match the written window, got {dup}");
    }
}
