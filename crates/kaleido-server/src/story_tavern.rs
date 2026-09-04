//! Story Tavern API (ST-0/1): packs / sessions / persona CRUD + turn jobs + LLM streaming.

use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use chrono::Utc;
use futures_util::stream::Stream;
use kaleido_core::{
    ActorStateUpdate, Compass, CompassStore, ContentTier, CreateSessionRequest, DirectorPlan,
    DirectorPlanRunStatus, PackStore, StageDirectorConfig, StoryPack, TavernMessage,
    TavernPersona, TavernSession, TavernSessionStore, TurnPhase, build_side_branch_catalog,
    enter_side_branch, seed_opening_if_needed,
    MemoryL2Event,
};
use kaleido_core::ledger::{LedgerKind, LedgerStore};
// X2 (吞噬自 xiami skimming.rs / emotional_hooks.rs / story_simulation.rs):
// 虾米三大质检模块 —— 读者速读分析 / 情绪钩子合同 / 剧情因果推演校验。
use kaleido_core::st_skimming::{
    ReaderPlatform, ReaderProfile, ReaderSkimmingConfig, SkimIssue, analyze_skimming,
};
use kaleido_core::st_emotional_hooks::{
    EmotionalHookConfig, PlotSignalSample, render_hook_execution_contract,
    repeated_recent_hook_signal,
};
use kaleido_core::st_simulation::{
    ChapterSimulation, SYSTEM_PROMPT, render as render_simulation, validate_simulation,
};
// X4 (吞噬自 xiami outline.rs): 大纲补丁影响分析 + 章节执行合同 —— 导演台计划与章节生成之间的合同约束层。
use kaleido_core::st_outline::{ChapterBriefView, build_chapter_execution_contract, render_execution_contract};
use serde::Deserialize;
use serde_json::{json, Value};
use rand::Rng;
use std::collections::HashMap;
use std::convert::Infallible;
use std::hash::Hasher;
use std::time::Duration as StdDuration;
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{map_core_err, session_from, AppState, ChatStreamEvent};
use crate::error_codes::*;
use crate::llm_stream::{
    stream_chat_completions_dispatch, TurnStreamError,
};
use crate::skill_layer::{SkillDoc, append_writing_skill_hint, load_writing_skill};
use crate::state::StreamHub;

// ─── Request types ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeMemoryRequest {
    /// Optional character ID to focus analysis on; None = current focus character.
    pub character_id: Option<String>,
    /// Optional: apply proposed updates as MemoryPatch immediately.
    #[serde(default)]
    pub apply: bool,
}

/// U13 M1: Batch-optimization request for memory deduplication/merge.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeMemoryRequest {
    /// Optional: apply resulting MemoryPatch immediately.
    #[serde(default)]
    pub apply: bool,
}

/// U13 M2: Explicit session compaction request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactSessionRequest {
    /// Compression style: "conversation" (dialogue flow) / "bulletin" (key points) / "battle_report" (dual-agent battlefield).
    #[serde(default = "default_compact_style")]
    pub style: String,
}

fn default_compact_style() -> String {
    "conversation".into()
}

/// X5 (吞噬自 xiami writing_style.rs): 文笔风格分析请求。
/// `sourceText` 为小说样本；`workId` 可选（预留 pack narrative_style 落库，MVP 仅返回不落库）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleAnalysisRequest {
    /// 小说 TXT 正文或粘贴的小说样本。
    pub source_text: String,
    /// 可选作品/包 ID（预留，MVP 不落库）。
    #[serde(default)]
    pub work_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartRequest {
    pub message: String,
    #[serde(default)]
    pub model: Option<String>,
    /// P0 (吞噬 denova 写作三档): 回合正文生成质量档位。
    /// lite=单次直出（现状零回归）；standard=初稿后审稿+修订；
    /// heavy=context-plan→write→review→fix→final-gate 多轮管道。
    /// 缺省 lite；旧请求体（无该字段）兼容。
    /// 前端已暴露档位开关（写作台工具栏「写作档位」下拉，2026-08-08 a0fe865f），此处为 API 参数与默认值兜底。
    #[serde(default)]
    pub quality: Option<TurnQuality>,
}

/// P0: 回合叙事生成质量档位（对应 denova novel-lite / novel-standard / novel-heavy）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnQuality {
    Lite,
    Standard,
    Heavy,
}

impl Default for TurnQuality {
    fn default() -> Self {
        TurnQuality::Lite
    }
}

impl TurnQuality {
    #[allow(dead_code)] // [P7] TurnQuality 显示别名预留
    fn as_str(self) -> &'static str {
        match self {
            TurnQuality::Lite => "lite",
            TurnQuality::Standard => "standard",
            TurnQuality::Heavy => "heavy",
        }
    }
}

impl<'de> serde::Deserialize<'de> for TurnQuality {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "standard" => TurnQuality::Standard,
            "heavy" => TurnQuality::Heavy,
            _ => TurnQuality::Lite,
        })
    }
}

/// 归一化请求档位：None / 非法值 → lite（默认，保持现状）。
#[allow(dead_code)] // [P7] P6 收敛决议：文档化 test helper，生产零调用属预期
fn resolve_turn_quality(q: Option<TurnQuality>) -> TurnQuality {
    q.unwrap_or_default()
}

/// U11: 估算本回合 LLM 上下文载荷（字符数口径，与 build_tavern_system_prompt 实际输入近似）：
/// system prompt 字符 + 实际进 prompt 的对话窗口（最近 KEEP_RECENT_MESSAGES 条）字符
/// + 本轮用户消息 + 初稿字符。改为窗口载荷而非全量 messages（epoch 压缩后消息被裁剪，
/// 判据可回落，避免越过阈值后每回合触发压缩）。
const KEEP_RECENT_MESSAGES: usize = 12;
fn estimate_turn_ctx_chars(
    session: &TavernSession,
    sys_prompt: &str,
    user_msg: &str,
    draft: &str,
) -> usize {
    let hist: usize = session
        .messages
        .iter()
        .rev()
        .take(KEEP_RECENT_MESSAGES)
        .map(|m| m.content.chars().count())
        .sum();
    sys_prompt.chars().count() + hist + user_msg.chars().count() + draft.chars().count()
}

/// U11: 上下文阈值判定 —— 超过字符阈值或消息数兜底阈值才触发 epoch 压缩（不再机械 turn%8）。
fn should_epoch_compress(session: &TavernSession, ctx_chars: usize) -> bool {
    let t = u11_tuning();
    should_epoch_compress_with(session, ctx_chars, t.epoch_hard_chars, t.epoch_hard_messages)
}

/// 纯函数变体：阈值显式传入（供单测与 env 前的常量语义保持对齐）。
fn should_epoch_compress_with(
    session: &TavernSession,
    ctx_chars: usize,
    epoch_hard_chars: usize,
    epoch_hard_messages: usize,
) -> bool {
    ctx_chars >= epoch_hard_chars || session.messages.len() >= epoch_hard_messages
}

/// U11: 质量管道额外 LLM 调用次数（估算口径）：lite=0；standard=审稿+修订=2；
/// heavy=plan/write/review/fix/gate + fix×MAX_ROUNDS + memory（6..=8）。
fn refine_llm_calls(quality: TurnQuality) -> u32 {
    match quality {
        TurnQuality::Lite => 0,
        TurnQuality::Standard => 2,
        TurnQuality::Heavy => 5 + QUALITY_MAX_FIX_ROUNDS as u32 + 1,
    }
}

/// U11: 回合成本估算结果（纯函数，可单测）。
#[derive(Debug, Clone, Copy, Default)]
struct TurnCostEstimate {
    llm_calls: u32,
    est_in_tokens: u32,
    est_out_tokens: u32,
    est_cost_usd: f64,
}

/// U11: 回合级 LLM 调用次数与估算成本。口径：
/// - llm_calls = 主流(1) + 模型回退(0/1) + 质量管道 + 非阻塞后处理(extra_calls)
/// - in/out tokens 用 kaleido_core::estimate_tokens 启发式近似（CJK≈1.5 字符/token）
/// - cost = in/1M×单价 + out/1M×单价；模型名含 "free" 记 0（如 deepseek-v4-flash-free）
fn turn_cost_estimate(
    model: &str,
    quality: TurnQuality,
    sys_prompt: &str,
    user_msg: &str,
    draft: &str,
    extra_calls: u32,
    used_fallback: bool,
) -> TurnCostEstimate {
    let mode = kaleido_core::TokenEstimateMode::Heuristic;
    let main_in =
        kaleido_core::estimate_tokens(&format!("{}\n{}", sys_prompt, user_msg), mode).max(0) as u32;
    let draft_out = kaleido_core::estimate_tokens(draft, mode).max(0) as u32;
    let refine = refine_llm_calls(quality);
    // 质量管道每轮输入 ≈ 主调用输入规模；输出 ≈ 正文规模（write/fix 会重写正文）
    let writes = match quality {
        TurnQuality::Lite => 0,
        TurnQuality::Standard => 1,
        TurnQuality::Heavy => 2 + QUALITY_MAX_FIX_ROUNDS as u32,
    };
    let llm_calls = 1 + if used_fallback { 1 } else { 0 } + refine + extra_calls;
    let est_in_tokens = main_in * (1 + refine + extra_calls);
    let est_out_tokens = draft_out * (1 + writes);
    let free = model.to_ascii_lowercase().contains("free");
    let est_cost_usd = if free {
        0.0
    } else {
        (est_in_tokens as f64 / 1_000_000.0) * COST_PER_MT_IN
            + (est_out_tokens as f64 / 1_000_000.0) * COST_PER_MT_OUT
    };
    TurnCostEstimate {
        llm_calls,
        est_in_tokens,
        est_out_tokens,
        est_cost_usd,
    }
}

/// U11: 回合预算检查（硬超时看门狗，纯函数可单测）。
fn turn_over_budget(started_at_ms: u64, budget_ms: u64) -> bool {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64 >= started_at_ms.saturating_add(budget_ms))
        .unwrap_or(false)
}

/// U11: 回合级记账 JSON（合并进 job payload，GET /api/v1/jobs 可见；成功/失败路径通用）。
fn u11_accounting_json(
    model: &str,
    quality: TurnQuality,
    sys_prompt: &str,
    user_msg: &str,
    draft: &str,
    extra_calls: u32,
    used_fallback: bool,
    elapsed_ms: u64,
    resumed: bool,
    epoch: u32,
    turn: Option<u32>,
    err: Option<&str>,
) -> Value {
    let est = turn_cost_estimate(model, quality, sys_prompt, user_msg, draft, extra_calls, used_fallback);
    json!({
        "u11": {
            "turn": turn,
            "epoch": epoch,
            "resumed": resumed,
            "durationMs": elapsed_ms,
            "llmCalls": est.llm_calls,
            "estTokensIn": est.est_in_tokens,
            "estTokensOut": est.est_out_tokens,
            "estCostUsd": est.est_cost_usd,
            "error": err,
        }
    })
}

/// TurnQuality ↔ kaleido_core::Quality（同一语义三档，便于会话级持久化）。
impl From<kaleido_core::Quality> for TurnQuality {
    fn from(q: kaleido_core::Quality) -> Self {
        match q {
            kaleido_core::Quality::Lite => TurnQuality::Lite,
            kaleido_core::Quality::Standard => TurnQuality::Standard,
            kaleido_core::Quality::Heavy => TurnQuality::Heavy,
        }
    }
}

impl From<TurnQuality> for kaleido_core::Quality {
    fn from(q: TurnQuality) -> Self {
        match q {
            TurnQuality::Lite => kaleido_core::Quality::Lite,
            TurnQuality::Standard => kaleido_core::Quality::Standard,
            TurnQuality::Heavy => kaleido_core::Quality::Heavy,
        }
    }
}

/// P0 质量管道阶段（denova 角色映射）。仅供测试断言表达「期望阶段形状」，
/// 实际执行在 `run_quality_refine` 内。P4 起 `MemoryPatch` 归入 heavy 管道末尾
/// （产出可应用 patch 并回写 actor_states；回合尾 L0-L4 抽取仍为独立代码）。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityStage {
    ContextPlan,
    Write,
    Review,
    Fix,
    FinalGate,
    MemoryPatch,
}

/// heavy 防死循环上限：final-gate 未达标最多再修复 2 轮。
pub const QUALITY_MAX_FIX_ROUNDS: usize = 2;

/// U11: 回合硬超时（秒）—— 主流 + 质量管道总预算。超时后以 error 终态释放锁（可 resume 重发）。
pub const TURN_HARD_TIMEOUT_SECS: u64 = 600;
/// U11: 上下文窗口 epoch 压缩阈值（估算字符数：system prompt + 全量对话 + 本轮消息 + 初稿）。
/// 超过阈值才触发压缩（替换机械 turn%8），由 build_tavern_system_prompt 实际载荷近似估算。
/// [fix 2026-08-15] 模型支持 1M 上下文（≈200 万中文字符），阈值 20K→200K，减少长文被反复压缩切碎。
pub const TURN_EPOCH_HARD_CHARS: usize = 200_000;
/// U11: 消息数兜底阈值（对话条数超限同样触发 epoch 压缩，防字符阈值未达但记忆层已膨胀）。
pub const TURN_EPOCH_HARD_MESSAGES: usize = 64;
/// U11: 成本估算单价（USD / 百万 token；DeepSeek 类廉价档默认，free 模型计 0）。
const COST_PER_MT_IN: f64 = 0.25;
const COST_PER_MT_OUT: f64 = 1.25;

/// U11: 可调参数集（env 覆盖，默认与上方 pub const 一致，零行为变更）。
///
/// 生产调参无需重编译，通过 env 注入：
/// - `KALEIDO_U11_EPOCH_CHARS`    —— 上下文压缩字符阈值（默认 200_000）
/// - `KALEIDO_U11_EPOCH_MESSAGES` —— 消息数兜底阈值（默认 64）
/// - `KALEIDO_U11_TIMEOUT_SECS`   —— 回合硬超时秒数（默认 600）
/// - `KALEIDO_U11_MAX_FIX_ROUNDS` —— heavy final-gate 最大修复轮数（默认 2）
/// 非法/缺失值回落默认，绝不影响进程启动。
#[derive(Debug, Clone, Copy)]
pub struct U11Tuning {
    pub epoch_hard_chars: usize,
    pub epoch_hard_messages: usize,
    pub hard_timeout_secs: u64,
    pub max_fix_rounds: usize,
}

impl Default for U11Tuning {
    fn default() -> Self {
        Self {
            epoch_hard_chars: TURN_EPOCH_HARD_CHARS,
            epoch_hard_messages: TURN_EPOCH_HARD_MESSAGES,
            hard_timeout_secs: TURN_HARD_TIMEOUT_SECS,
            max_fix_rounds: QUALITY_MAX_FIX_ROUNDS,
        }
    }
}

fn u11_env_u64(key: &str, def: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(def)
}

/// 从 env 解析 U11 调参（纯函数，供测试直接构造）。
pub fn u11_tuning_from_env() -> U11Tuning {
    U11Tuning {
        epoch_hard_chars: u11_env_u64("KALEIDO_U11_EPOCH_CHARS", TURN_EPOCH_HARD_CHARS as u64)
            as usize,
        epoch_hard_messages: u11_env_u64(
            "KALEIDO_U11_EPOCH_MESSAGES",
            TURN_EPOCH_HARD_MESSAGES as u64,
        ) as usize,
        hard_timeout_secs: u11_env_u64("KALEIDO_U11_TIMEOUT_SECS", TURN_HARD_TIMEOUT_SECS),
        max_fix_rounds: u11_env_u64(
            "KALEIDO_U11_MAX_FIX_ROUNDS",
            QUALITY_MAX_FIX_ROUNDS as u64,
        ) as usize,
    }
}

static U11_TUNING: std::sync::OnceLock<U11Tuning> = std::sync::OnceLock::new();

/// 进程级 U11 调参（首用解析并缓存；运行中改 env 不生效，属预期）。
pub fn u11_tuning() -> &'static U11Tuning {
    U11_TUNING.get_or_init(u11_tuning_from_env)
}

/// 回合硬超时（毫秒，调参后口径）。
fn u11_hard_timeout_ms() -> u64 {
    u11_tuning().hard_timeout_secs * 1000
}

/// 三档阶段计划。仅作为测试断言，反映 `run_quality_refine` 的实际执行形状
/// （重写自查用，非生产驱动数据）。manifest: lite=Write；standard=Write/Review/Fix；
/// heavy=ContextPlan/Write/Review/Fix/FinalGate/MemoryPatch（P4 起 MemoryPatch 归入管道）。
/// [P6 收敛 2026-08-25] 修改 `run_quality_refine` 阶段结构时，必须同步更新本函数与
/// `test_plan_quality_stages_semantics`（审查C「测试镜像漂移」防护——当前已比对一致）。
#[cfg(test)]
pub fn plan_quality_stages(quality: TurnQuality) -> Vec<QualityStage> {
    match quality {
        TurnQuality::Lite => vec![QualityStage::Write],
        TurnQuality::Standard => {
            vec![QualityStage::Write, QualityStage::Review, QualityStage::Fix]
        }
        TurnQuality::Heavy => vec![
            QualityStage::ContextPlan,
            QualityStage::Write,
            QualityStage::Review,
            QualityStage::Fix,
            QualityStage::FinalGate,
            QualityStage::MemoryPatch,
        ],
    }
}

// ─── P0 审稿/修订/终检 prompt 模板（中文）──────────────────────────────────

/// 审稿角色：只审不改，返回问题清单。
const QUALITY_REVIEW_SYS: &str = "你是一位资深网文审稿人。请只审不改，针对下方叙事正文输出简洁的中文问题清单，逐条一行。覆盖：连续性、人物声线、文风、剧情逻辑、节奏。每条格式：`[问题] 位置/维度：问题说明（含可执行修改建议）`。不要输出正文，不要输出赞扬。\n\n[P4/P7 2026-08-15] 必须额外检查：①与上一回合正文的重复度——若大面积复用句式/意象/段落，列为 blocker/continuity 问题；②角色声线——对照角色卡示例对白校验，若角色被写成气音/半截话/被动弱气而示例对白是泼辣直接，列为 blocker 问题。";

/// [时间天气 v2 2026-08-17] LLM 剧情时间评估间隔（回合）：每 N 回合评估一次是否推进时间。
/// 低频省成本——正文 [时间推进] 标注与用户自然语言信号是主通道，LLM 评估是兜底校准。
const LLM_CLOCK_EVAL_INTERVAL: u32 = 4;
fn build_review_user(sys_prompt: &str, user_prompt: &str, plan: &str, draft: &str) -> String {
    format!(
        "## 系统约束（背景设定与 canon 规则，审稿时须对照）\n{}\n\n## 上下文计划（Required Beats 为硬节拍，逐条核对是否体现）\n{}\n\n## 玩家本轮输入\n{}\n\n## 待审正文\n{}\n\n请输出审稿问题清单。",
        sys_prompt, plan, user_prompt, draft
    )
}

/// 修订角色：按审稿意见修订，保留原文强段落与连续性。
/// [fix 2026-08-15 结构根治] 思维自动折叠：要求分析过程放入 <thinking>…</thinking> 块
/// （机器可解析，不依赖措辞猜测），正文放 <story>…</story> 块；后端按标签剥离折叠，
/// 替代堆关键词打地鼠（旧规则靠 <场景 标签/措辞前缀，fix 思维无标签则不认）。
const QUALITY_FIX_SYS: &str = "你是一位内容负责人。请根据审稿意见修订下方叙事正文。只修真正需要修的问题，保留原文的强段落、人物声线、有效情节节点与连续性。若问题清单为空或已达标，原样输出正文。\n\n[P4/P7 2026-08-15] 若审稿指出与上一回合重复：从上次结束位置**续写推进**而非重写，可保留的强段落最多 1-2 处。角色台词必须贴合角色卡示例对白声线，不得弱化为气音/半截话。\n\n输出结构（严格遵守）：如需先整理分析过程（对照审稿意见的取舍思路），放在 <thinking>…</thinking> 块内（该块不会展示给读者）；随后输出 <story>…</story> 块，内含修订后的完整正文。正文必须完整写在 <story> 内，不得省略、不得写「同上文」或「保持前文」；<story> 结束后不得追加任何「让我检查」「再检查」等自检文字。";
fn build_fix_user(sys_prompt: &str, review: &str, draft: &str) -> String {
    format!(
        "## 系统约束\n{}\n\n## 审稿意见\n{}\n\n## 待修订正文\n{}\n\n请输出：<thinking>…</thinking>（可选，分析过程）+ <story>…</story>（修订后的完整正文）。",
        sys_prompt, review, draft
    )
}

/// heavy 专属：上下文策划（轻量，重述写作目标/必须节拍/风格/风险）。
const QUALITY_PLAN_SYS: &str = "你是一位剧情策划。请为下方回合输出轻量上下文计划，逐行列出：写作范围、剧情目标、必须节拍、人物状态、canon 约束、风格约束、最易写崩的风险。不要输出正文。";
fn build_plan_user(sys_prompt: &str, user_prompt: &str) -> String {
    format!(
        "## 系统约束\n{}\n\n## 玩家本轮输入\n{}\n\n请输出上下文计划。",
        sys_prompt, user_prompt
    )
}

/// heavy 专属：正文作者（按计划写稿）。
const QUALITY_WRITE_SYS: &str = "你是一位正文作者。请严格依据上下文计划与系统约束撰写本回合叙事正文。直接输出正文，不要输出计划或思考过程。";
fn build_write_user(plan: &str, user_prompt: &str, draft: &str) -> String {
    format!(
        "## 上下文计划\n{}\n\n## 玩家本轮输入\n{}\n\n## 可参考的初稿（若缺失可重写）\n{}\n\n请输出正文。",
        plan, user_prompt, draft
    )
}

/// heavy 专属：终检（是否达标，不达标带明确问题交回 fix）。
/// [fix 2026-08-15] 传入 context plan：gate 必须对照 Required Beats 逐条核对，
/// 否则 canon 硬节拍（宿醉 6/6 未体现）在终检环节永远漏检。
const QUALITY_GATE_SYS: &str = "你是一位终检评审。请判断下方修订稿是否达标：连续性、canon/改写强度约束、是否回应了玩家输入、人物声线、语言质量。**必须逐条核对上下文计划中的 Required Beats（硬节拍）：任一硬节拍未在正文中体现即判未达标**。只输出 JSON：{\"pass\":true} 或 {\"pass\":false,\"problems\":\"简述未达标问题\"}。";
fn build_gate_user(sys_prompt: &str, user_prompt: &str, plan: &str, draft: &str) -> String {
    format!(
        "## 系统约束\n{}\n\n## 上下文计划（Required Beats 为硬节拍，逐条核对是否体现）\n{}\n\n## 玩家本轮输入\n{}\n\n## 待检修订稿\n{}\n\n请输出终检 JSON。",
        sys_prompt, plan, user_prompt, draft
    )
}

/// heavy 专属：Memory Patch（G5，吞噬 denova memory-patcher）。产出可应用
/// progress/character_state/world_state/foreshadowing 四类补丁 JSON。
const QUALITY_MEMORY_SYS: &str = "你是一位状态补丁生成器。请根据最终定稿正文生成可应用的记忆补丁，只输出 JSON：{\"progress\":\"...\",\"character_state\":{\"<characterId>\":\"当前状态摘要\"},\"world_state\":\"...\",\"foreshadowing\":\"...\"}。只基于正文中已发生内容，不臆造。";
fn build_memory_user(sys_prompt: &str, user_prompt: &str, final_text: &str) -> String {
    format!(
        "## 系统约束\n{}\n\n## 玩家本轮输入\n{}\n\n## 最终定稿正文\n{}\n\n请输出记忆补丁 JSON。",
        sys_prompt, user_prompt, final_text
    )
}

/// 解析终检结果：{"pass":true} 视为通过；显式 false 为未通过；无法解析时保守视为通过（避免误回退）。
pub fn gate_passes(verdict: &str) -> bool {
    let trimmed = verdict.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return v.get("pass").and_then(|p| p.as_bool()).unwrap_or(true);
    }
    !trimmed.contains("\"pass\":false")
}

/// 可注入 LLM 调用（生产=远程，测试=本地 mock）。避免在管道内硬编码网络。
/// 返回 `Send` future：生产路径在 `tokio::spawn` 内被 await（Send 约束）。
pub trait QualityLlm {
    fn call(
        &self,
        system: &str,
        user: &str,
    ) -> std::pin::Pin<
        Box<dyn futures_util::Future<Output = Result<String, String>> + Send + '_>,
    >;
}

/// 生产实现：走现有 `stream_chat_completions`（复用 llm_stream helper）。
struct RemoteQualityLlm<'a> {
    base_url: &'a str,
    api_key: &'a str,
    model: &'a str,
    /// G6: provider protocol for call-side dispatch.
    provider_kind: &'a str,
}

impl QualityLlm for RemoteQualityLlm<'_> {
    fn call(
        &self,
        system: &str,
        user: &str,
    ) -> std::pin::Pin<
        Box<dyn futures_util::Future<Output = Result<String, String>> + Send + '_>,
    > {
        let base_url = self.base_url.to_string();
        let api_key = self.api_key.to_string();
        let model = self.model.to_string();
        let prov_kind = self.provider_kind.to_string();
        let sys = system.to_string();
        let usr = user.to_string();
        Box::pin(async move {
            stream_chat_completions_dispatch(
                &base_url, &api_key, &model, &prov_kind, &sys, &usr, 0.35, 32768, 90, |_| true,
            )
            .await
        })
    }
}

/// P4: stage 模板回退。skill 有对应 template 则用之，否则回退既有 const prompt（零回归）。
fn stage_sys<'a>(
    skill: Option<&'a SkillDoc>,
    select: impl FnOnce(&'a SkillDoc) -> Option<&'a str>,
    fallback: &'static str,
) -> &'a str {
    skill.and_then(select).unwrap_or(fallback)
}

/// P0 质量管道：lite 直出 draft（零回归）；standard 一次审稿+一次修订；
/// heavy context-plan→write→review→fix→final-gate，gate 未达标最多 `QUALITY_MAX_FIX_ROUNDS` 次修复；
/// heavy 末尾插 MemoryPatch 阶段（best-effort，产出可应用 patch，失败不挡正文）。
/// 任意核心子步骤失败均返回 Err，调用方保留初稿（best-effort，不挡正文）。
/// 返回最终正文 + 可选 memory patch（heavy 专属）。
async fn run_quality_refine<L: QualityLlm>(
    quality: TurnQuality,
    llm: &L,
    sys_prompt: &str,
    user_prompt: &str,
    draft: &str,
    skill: Option<&SkillDoc>,
    thinking_out: &mut String,
) -> Result<(String, Option<String>), String> {
    match quality {
        TurnQuality::Lite => Ok((strip_lite_reasoning_leak(draft.to_string()), None)),
        TurnQuality::Standard => {
            let review_sys = stage_sys(skill, |s| s.templates.review.as_deref(), QUALITY_REVIEW_SYS);
            let fix_sys = stage_sys(skill, |s| s.templates.fix.as_deref(), QUALITY_FIX_SYS);
            let review = llm
                .call(review_sys, &build_review_user(sys_prompt, user_prompt, "", draft))
                .await?;
            let fix_out = llm
                .call(fix_sys, &build_fix_user(sys_prompt, &review, draft))
                .await?;
            // [fix 2026-08-15 结构根治] fix 输出结构化剥离（<thinking>/<story> 标签），
            // 思维段汇入 thinking_out（前端 monologue 折叠展示），正文纯净化后返回。
            let mut fixed_clean = fix_out;
            append_thinking(thinking_out, &strip_fix_thinking_blocks(&mut fixed_clean));
            Ok((fixed_clean, None))
        }
        TurnQuality::Heavy => {
            let plan_sys = stage_sys(skill, |s| s.templates.plan.as_deref(), QUALITY_PLAN_SYS);
            let write_sys = stage_sys(skill, |s| s.templates.write.as_deref(), QUALITY_WRITE_SYS);
            let review_sys = stage_sys(skill, |s| s.templates.review.as_deref(), QUALITY_REVIEW_SYS);
            let fix_sys = stage_sys(skill, |s| s.templates.fix.as_deref(), QUALITY_FIX_SYS);
            let gate_sys = stage_sys(skill, |s| s.templates.gate.as_deref(), QUALITY_GATE_SYS);
            let memory_sys = stage_sys(skill, |s| s.templates.memory.as_deref(), QUALITY_MEMORY_SYS);
            let plan = llm
                .call(plan_sys, &build_plan_user(sys_prompt, user_prompt))
                .await?;
            let written = llm
                .call(write_sys, &build_write_user(&plan, user_prompt, draft))
                .await?;
            let review = llm
                .call(review_sys, &build_review_user(sys_prompt, user_prompt, &plan, &written))
                .await?;
            let mut fixed = llm
                .call(fix_sys, &build_fix_user(sys_prompt, &review, &written))
                .await?;
            // [fix 2026-08-15 结构根治] 每轮 fix 输出结构化剥离：gate 只见纯正文
            // （避免 <thinking>/<story> 标签干扰达标判定），思维段汇入折叠展示。
            append_thinking(thinking_out, &strip_fix_thinking_blocks(&mut fixed));
            let max_fix_rounds = u11_tuning().max_fix_rounds;
            for round in 0..max_fix_rounds {
                let verdict = llm
                    .call(gate_sys, &build_gate_user(sys_prompt, user_prompt, &plan, &fixed))
                    .await?;
                if gate_passes(&verdict) {
                    break;
                }
                fixed = llm
                    .call(
                        fix_sys,
                        &build_fix_user(
                            sys_prompt,
                            &format!("（第{}轮终检未达标）{}", round + 1, verdict),
                            &fixed,
                        ),
                    )
                    .await?;
                append_thinking(thinking_out, &strip_fix_thinking_blocks(&mut fixed));
            }
            // P4: heavy MemoryPatch 阶段（best-effort；失败仅记日志，不挡正文）
            let mut memory_patch = llm
                .call(memory_sys, &build_memory_user(sys_prompt, user_prompt, &fixed))
                .await
                .ok();
            // P4.1: 记忆补丁结构契约 + 修复循环（吸收自 OpenHanako rolling-summary-format：
            // JSON 解析失败/缺 character_state 时附原因重试 1 次，仍失败保持 None 降级）。
            if let Some(patch) = memory_patch.as_deref() {
                let issues = kaleido_core::st_memory_contract::validate_memory_patch(patch);
                if !issues.ok {
                    tracing::warn!(
                        issues = ?issues.issues,
                        "记忆补丁结构校验失败，尝试格式修复"
                    );
                    let repair_prompt =
                        kaleido_core::st_memory_contract::build_memory_patch_repair_prompt();
                    let repair_input = kaleido_core::st_memory_contract::build_memory_patch_repair_input(
                        &issues.issues,
                        patch,
                    );
                    if let Ok(repaired) = llm.call(&repair_prompt, &repair_input).await {
                        let after =
                            kaleido_core::st_memory_contract::validate_memory_patch(&repaired);
                        if after.ok {
                            tracing::info!("记忆补丁格式修复成功");
                            memory_patch = Some(repaired);
                        } else {
                            tracing::warn!(
                                issues = ?after.issues,
                                "记忆补丁修复后仍不合规，保持降级（不应用）"
                            );
                        }
                    }
                }
            }
            Ok((fixed, memory_patch))
        }
    }
}

#[derive(Debug, Deserialize)]
struct StopPayload {
    #[serde(alias = "runId")]
    run_id: String,
}

// ─── Router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/story-tavern/packs",
            get(list_packs).post(upsert_pack),
        )
        .route(
            "/api/v1/story-tavern/packs/demo",
            post(ensure_demo),
        )
        .route(
            "/api/v1/story-tavern/packs/import",
            post(import_pack_zip),
        )
        .route(
            "/api/v1/story-tavern/packs/{id}",
            get(get_pack).delete(delete_pack),
        )
        .route(
            "/api/v1/story-tavern/packs/{id}/export.zip",
            get(export_pack_zip),
        )
        // SoulLink 吸收：档案维护端点（analyze 增量分析 / refine 精编 / purge 删楼溯源清理）
        .route(
            "/api/v1/story-tavern/packs/{id}/archive/analyze",
            post(archive_analyze),
        )
        .route(
            "/api/v1/story-tavern/packs/{id}/archive/refine",
            post(archive_refine),
        )
        .route(
            "/api/v1/story-tavern/packs/{id}/archive/purge/{source}",
            delete(archive_purge),
        )
        // 2026-08-15: 存量 pack 角色补抽入口（修复多 pack 少角色根因 ②）
        // 手动触发 spawn_auto_cast_extraction：阈值相对判断 + 断链容错 + 会话另存检测全在抽取函数内。
        .route(
            "/api/v1/story-tavern/packs/{id}/cast-extract",
            post(trigger_cast_extract),
        )
        .route(
            "/api/v1/story-tavern/packs/{id}/chapters/{*rel}",
            get(read_chapter).put(write_chapter),
        )
        .route(
            "/api/v1/story-tavern/packs/{id}/vector-index/rebuild",
            post(rebuild_pack_vector_index),
        )
        .route(
            "/api/v1/story-tavern/sessions",
            get(list_sessions).post(create_session),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}",
            get(get_session)
                .put(save_session)
                .patch(save_session)
                .delete(delete_session),
        )
        // ST-1: turn jobs
        .route(
            "/api/v1/story-tavern/sessions/{id}/turn",
            post(start_turn),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/stream",
            get(session_stream),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/mode",
            post(set_play_mode),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/tier",
            post(set_session_tier),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/opening",
            post(ensure_opening),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/side-branches",
            get(list_side_branches),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/side-branches/enter",
            post(enter_side_branch_route),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/focus",
            post(set_focus_character),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/rebind-vessel",
            post(rebind_vessel),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/saves",
            get(list_saves).post(create_save),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/worldline",
            get(get_worldline),
        )
        // T2 世界认知：U2 实体图谱 + U7 真相账本
        .route(
            "/api/v1/story-tavern/sessions/{id}/world/entities",
            get(world_entities),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/world/entities/{entity_id}",
            get(world_entity_detail),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/world/events",
            post(world_apply_events),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/world/truth",
            get(world_truth),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/world/truth/check",
            post(world_truth_check),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/assistant",
            post(assistant_chat),
        )
        // P0-1: story_command —— /rewind N 回退、/reroll 重生成
        .route(
            "/api/v1/story-tavern/sessions/{id}/rewind",
            post(rewind_session),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/reroll",
            post(reroll_session),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/messages/{mid}",
            axum::routing::delete(delete_message),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/messages/{mid}",
            axum::routing::put(edit_message),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/saves/{save_id}",
            axum::routing::delete(delete_save),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/saves/{save_id}/restore",
            post(restore_save),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/saves/{save_id}/fork",
            post(fork_save),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/stop",
            post(stop_turn),
        )
        // U13 M1: character memory analysis + batch optimization
        .route(
            "/api/v1/story-tavern/sessions/{id}/analyze-memory",
            post(analyze_memory),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/optimize-memory",
            post(optimize_memory),
        )
        // U13 M2: explicit session compaction API
        .route(
            "/api/v1/story-tavern/sessions/{id}/compact",
            post(compact_session),
        )
        // U13 M3: branch-level summary
        .route(
            "/api/v1/story-tavern/sessions/{id}/branches/{bid}/summary",
            get(branch_summary),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/director-plan",
            get(get_director_plan),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/director-plan/run",
            post(run_director_plan),
        )
        // G13: 导演工具面——导演 plan 提交（HTTP 版 submit_director_plan_update）。
        .route(
            "/api/v1/story-tavern/sessions/{id}/director-plan/submit",
            post(submit_director_plan),
        )
        // S5/S6 演出机只读 + 归档（吞噬 denova event_package / actor archive）
        .route(
            "/api/v1/story-tavern/sessions/{id}/director-config",
            get(get_director_config).put(put_director_config),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/event-packages",
            get(get_event_packages),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/last-event",
            get(get_last_event),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/actor-states",
            get(get_actor_states).put(put_actor_states),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/pockets",
            get(get_pockets).put(put_pockets),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/pockets-enabled",
            get(get_pockets_enabled).put(put_pockets_enabled),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/needs",
            get(get_needs).put(put_needs),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/needs/tick",
            post(tick_needs),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/growth",
            get(get_growth).put(put_growth),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/world-climate",
            get(get_world_climate).put(put_world_climate),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/chaos",
            get(get_chaos).put(put_chaos),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/chaos/tick",
            post(tick_chaos),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/milestones",
            get(get_milestones),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/objectives",
            get(get_objectives).post(create_objective),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/objectives/{oid}",
            axum::routing::put(update_objective).delete(delete_objective),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/ambitions",
            get(get_ambitions).post(create_ambition),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/dreams",
            get(get_dreams).post(push_dream),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/episodes",
            get(get_episodes).post(push_episode),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/journals",
            get(get_journals).post(create_journal_card),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/journals/{card_id}",
            axum::routing::put(update_journal_card).delete(delete_journal_card),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/journals/{card_id}/pin",
            post(toggle_pin_journal),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/journals/recall",
            post(recall_journals),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/journals/embed-missing",
            post(embed_missing_journals),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/relationships",
            get(get_relationships).put(put_relationships),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/relationships/tick",
            post(tick_relationships),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/messages/{msg_id}/swipe",
            get(get_swipe).put(put_swipe),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/storyline",
            get(get_storyline),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/promises",
            get(get_promises).post(create_promise),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/promises/{pid}",
            axum::routing::put(resolve_promise),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/preferences",
            get(get_preferences).put(put_preferences),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/presence",
            get(get_presence).put(put_presence),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/mood",
            get(get_mood),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/event-extract",
            get(get_event_extract).put(put_event_extract),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/timed-world-info",
            get(get_timed_world_info),
        )
        // [morphling Wave B3 2026-08-16] 章节剧情摘要账本（吸收自 BakemonoMemory
        // summary-memory-model）：GET 查看每章总结，PUT 手动修改（manual_edited 保护）。
        .route(
            "/api/v1/story-tavern/sessions/{id}/chapter-summaries",
            get(get_chapter_summaries),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/chapter-summaries/{chapter_id}",
            put(put_chapter_summary),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/actor-archive",
            get(list_actor_archives).post(archive_actor_state),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/check-history",
            get(get_check_history),
        )
        .route(
            "/api/v1/story-tavern/sessions/{id}/actor-archive/restore",
            post(restore_actor_archive),
        )
        .route(
            "/api/v1/story-tavern/persona/{character_id}",
            get(get_persona).put(save_persona),
        )
        // T2 创作罗盘：per-work 的 {author_intent, current_focus} 读写。
        .route(
            "/api/v1/story-tavern/works/{work_id}/compass",
            get(get_compass).put(put_compass),
        )
        // X5 (吞噬自 xiami writing_style.rs): 作品级文笔风格分析 —— 样本采样 + 12 维分析。
        .route(
            "/api/v1/story-tavern/style-analysis",
            post(style_analysis),
        )
        // 整个 story-tavern 路由放宽 body 上限到 64MB（axum 默认 2MB 会拒长剧情/长剧本
        // 生成与附带上下文的请求 → 413 "Failed to buffer the request body: length limit exceeded"）。
        // 与 crawler/works 端点的 64MB 保持一致；handler 内的字符上限仍先于 413 生效。
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
}

// ─── Pack handlers ───────────────────────────────────────────────────────────

async fn list_packs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.list() {
        Ok(list) => Json(json!({ "packs": list })).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn get_pack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.get(&id) {
        Ok(p) => Json(p).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn upsert_pack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(pack): Json<StoryPack>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.save(pack) {
        Ok(p) => Json(p).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn delete_pack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.delete(&id) {
        Ok(()) => {
            let n = state.sessions_tavern.mark_pack_missing(&id).unwrap_or(0);
            Json(json!({ "ok": true, "sessionsMarked": n })).into_response()
        }
        Err(e) => map_core_err(e),
    }
}


async fn export_pack_zip(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.export_zip(&id) {
        Ok(bytes) => {
            let filename = format!("{id}.zip");
            (
                [
                    (axum::http::header::CONTENT_TYPE, "application/zip"),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        &format!("attachment; filename=\"{filename}\""),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportPackRequest {
    /// base64-encoded zip bytes
    zip_base64: String,
    #[serde(default)]
    id: Option<String>,
}

async fn import_pack_zip(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ImportPackRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let raw = match base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        body.zip_base64.trim(),
    ) {
        Ok(b) => b,
        Err(e) => {
            return map_core_err(kaleido_core::CoreError::BadRequest(format!(
                "invalid base64: {e}"
            )));
        }
    };
    if raw.len() > 20 * 1024 * 1024 {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "zip too large (max 20MB)".into(),
        ));
    }
    match state.packs.import_zip(&raw, body.id) {
        Ok(p) => Json(p).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn ensure_demo(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.ensure_demo_pack() {
        Ok(p) => Json(p).into_response(),
        Err(e) => map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
struct ChapterBody {
    content: String,
}

async fn read_chapter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, rel)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.read_chapter_body(&id, &rel) {
        Ok(content) => Json(json!({ "path": rel, "content": content })).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn write_chapter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, rel)): Path<(String, String)>,
    Json(body): Json<ChapterBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.packs.write_chapter_body(&id, &rel, &body.content) {
        Ok(()) => Json(json!({ "ok": true, "path": rel })).into_response(),
        Err(e) => map_core_err(e),
    }
}

// ─── Session handlers ────────────────────────────────────────────────────────

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.list_owned(&session.user_id) {
        Ok(list) => Json(json!({ "sessions": list })).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(mut s) => {
            // Repair legacy messages that still embed 【选项】 in content
            let mut dirty = sanitize_session_messages(&mut s);
            // Legacy empty sessions (created before opening seed): auto-seed on first load.
            if !s.pack_missing && !s.opening_seeded && s.messages.is_empty() {
                if let Ok(pack) = state.packs.get(&s.pack_id) {
                    if seed_opening_if_needed(&mut s, &pack) {
                        dirty = true;
                    }
                }
            }
            if dirty {
                let _ = state.sessions_tavern.save(s.clone());
            }
            Json(s).into_response()
        }
        Err(e) => map_core_err(e),
    }
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut req): Json<CreateSessionRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // F1: stamp owner from authenticated user.
    req.owner = Some(session.user_id.clone());
    match state
        .sessions_tavern
        .create_from_pack(&state.packs, req)
    {
        Ok(s) => Json(s).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn save_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut session): Json<TavernSession>,
) -> Response {
    let auth = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // F1: ownership check — load existing and verify owner before allowing save.
    // M-2 (CAS): use update_session so the read-then-write is a single locked
    // transaction; concurrent saves cannot overwrite each other.
    // We do the ownership + tier tightening + field reconciliation inside the
    // closure, then return the persisted session.
    if session.session_id.is_empty() {
        session.session_id = id.clone();
    } else if session.session_id != id {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "path id and body sessionId mismatch".into(),
        ));
    }
    let mut body_session = session;
    match state.sessions_tavern.update_session(&id, |existing| {
        // F1: ownership check inside the lock.
        if existing.owner.as_deref() != Some(&auth.user_id) {
            return Err(kaleido_core::CoreError::Forbidden(format!(
                "session not owned by user: {id}"
            )));
        }
        // Mid-session tier: only tighten (CONTEXT)
        if body_session.content_tier.rank() > existing.content_tier.rank() {
            body_session.content_tier = existing.content_tier;
        }
        body_session.checkpoints = existing.checkpoints.clone();
        // F1: preserve owner from existing session.
        body_session.owner = existing.owner.clone();
        // Replace in-place with the reconciled body.
        *existing = body_session.clone();
        Ok(())
    }) {
        Ok(s) => Json(s).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // F1: ownership check before delete.
    if let Err(e) = state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        return map_core_err(e);
    }
    match state.sessions_tavern.delete(&id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => map_core_err(e),
    }
}


#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetPlayModeRequest {
    play_mode: String,
}

/// Hot-switch session playMode mainline ↔ free ↔ side (ST-6/ST-9).
/// - free: freeze node advance; stay on current node
/// - side: freeze mainline cursor, stash resumeNodeId from current node before leaving mainline
/// - mainline from side: restore nodeId from resumeNodeId (if set)
async fn set_play_mode(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SetPlayModeRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mode = match kaleido_core::PlayMode::parse(&body.play_mode) {
        Some(m) => m,
        None => {
            return map_core_err(kaleido_core::CoreError::BadRequest(
                "playMode must be mainline, free, or side".into(),
            ));
        }
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if sess.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    if sess.active_run_id.is_some() {
        return conflict("ST_TURN_BUSY", "turn in progress; stop or wait before switching mode");
    }
    // M-2 (CAS): capture the on-disk revision at read time; the save below
    // uses save_with_revision so a concurrent write between read and write
    // surfaces as 409 instead of silently overwriting.
    let base_revision = sess.updated_at.clone();
    let prev = sess.play_mode;
    if prev == mode {
        return Json(sess).into_response();
    }

    // Entering side from mainline: stash resume point
    if mode == kaleido_core::PlayMode::Side && prev == kaleido_core::PlayMode::Mainline {
        if sess.resume_node_id.is_none() {
            sess.resume_node_id = sess.node_id.clone();
        }
    }
    // Entering side from free: also stash if empty
    if mode == kaleido_core::PlayMode::Side && prev == kaleido_core::PlayMode::Free {
        if sess.resume_node_id.is_none() {
            sess.resume_node_id = sess.node_id.clone();
        }
    }
    // Leaving side → mainline: restore resume node if present
    if mode == kaleido_core::PlayMode::Mainline && prev == kaleido_core::PlayMode::Side {
        if let Some(rid) = sess.resume_node_id.clone() {
            // resolve chapter from pack if possible
            if let Ok(pack) = state.packs.get(&sess.pack_id) {
                if let Some(n) = pack.nodes.iter().find(|n| n.id == rid) {
                    sess.node_id = Some(rid.clone());
                    sess.chapter_cursor = Some(n.chapter_id.clone());
                } else {
                    sess.node_id = Some(rid);
                }
            } else {
                sess.node_id = Some(rid);
            }
            // clear after successful resume
            sess.resume_node_id = None;
        }
        sess.side_branch_node_id = None;
        sess.side_branch_label = None;
    }

    sess.play_mode = mode;
    let detail = match mode {
        kaleido_core::PlayMode::Free => "自由模式：节点不再推进，可自由对话。".to_string(),
        kaleido_core::PlayMode::Side => format!(
            "支线模式：主线游标冻结；回主线将恢复到节点 {}。",
            sess.resume_node_id.as_deref().unwrap_or(sess.node_id.as_deref().unwrap_or("?"))
        ),
        kaleido_core::PlayMode::Mainline => {
            if prev == kaleido_core::PlayMode::Side {
                format!(
                    "主线模式：已从支线回档到节点 {}。",
                    sess.node_id.as_deref().unwrap_or("?")
                )
            } else {
                "主线模式：从当前节点继续推进。".to_string()
            }
        }
    };
    let note = format!(
        "〔模式切换〕{} → {}。{}",
        prev.as_str(),
        mode.as_str(),
        detail
    );
    sess.messages.push(TavernMessage {
        id: format!("msg-{}", Uuid::new_v4()),
        role: "assistant".into(),
        content: note,
        created_at: Utc::now().to_rfc3339(),
        options: vec![],
        engine_tag: None,
        program: None,
        reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
    });
    match state.sessions_tavern.save_with_revision(sess, &base_revision) {
        Ok(s) => Json(s).into_response(),
        Err(kaleido_core::CoreError::Conflict(_msg)) => return conflict("ST_CONCURRENT_WRITE", "session was modified concurrently; please retry"),
        Err(e) => map_core_err(e),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetContentTierRequest {
    content_tier: String,
    adult_confirmed: Option<bool>,
}

/// Explicit mid-session content-tier switch (魔棒「内容档位」).
/// - Unlike save_session (which only ever tightens), this endpoint lets the
///   user loosen the tier, but loosening to Open requires adultConfirmed=true.
/// - final = min3(requested, card_max, global) still applies — a pack/card
///   capped at Safe cannot be opened past its card max.
async fn set_session_tier(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SetContentTierRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let req_tier = match ContentTier::parse(&body.content_tier) {
        Some(t) => t,
        None => {
            return map_core_err(kaleido_core::CoreError::BadRequest(
                "contentTier must be safe, standard, or open".into(),
            ));
        }
    };
    let probe = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if probe.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    if probe.active_run_id.is_some() {
        return conflict("ST_TURN_BUSY", "turn in progress; stop or wait before switching tier");
    }
    // Recompute card max like create_from_pack: min across pack characters, then pack.max_tier.
    let mut card_max = ContentTier::Open;
    if let Ok(pack) = state.packs.get(&probe.pack_id) {
        card_max = pack.max_tier;
        for c in &pack.characters {
            if let Some(t) = c.content_tier {
                if t.rank() < card_max.rank() {
                    card_max = t;
                }
            }
        }
    }
    let global = ContentTier::Open;
    let final_tier = ContentTier::min3(req_tier, card_max, global);

    let prev_tier = probe.content_tier;
    if prev_tier == final_tier {
        return Json(probe).into_response();
    }
    // Loosening to Open requires explicit adult confirmation.
    let adult_ok = body.adult_confirmed.unwrap_or(probe.adult_confirmed);
    if final_tier.rank() > prev_tier.rank() && final_tier == ContentTier::Open && !adult_ok {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "放宽到开放档需要成年确认（adultConfirmed=true）".into(),
        ));
    }
    // [fix 2026-08-15] 档位切换只更新状态，不写入消息流：此前把
    // 「〔内容档位〕open → standard。」作为 assistant 消息 push 进会话，
    // 模型会把这条系统噪音当剧情正文读，污染上下文（宿醉颠三倒四实踩）。
    // 前端选择器读 session.contentTier 即可反映状态。
    // M-2 (CAS): apply tier mutation inside a locked transaction.
    let user_id = session.user_id.clone();
    match state.sessions_tavern.update_session(&id, |sess| {
        if sess.owner.as_deref() != Some(&user_id) {
            return Err(kaleido_core::CoreError::Forbidden(format!(
                "session not owned by user: {id}"
            )));
        }
        if sess.pack_missing {
            return Err(kaleido_core::CoreError::BadRequest(
                "pack missing: session is read-only".into(),
            ));
        }
        if sess.active_run_id.is_some() {
            return Err(kaleido_core::CoreError::Conflict(
                "turn in progress; stop or wait before switching tier".into(),
            ));
        }
        // Mid-session tier can only tighten via this path when re-read inside
        // the lock differs from the probe (concurrent write). Re-clamp:
        if final_tier.rank() > sess.content_tier.rank() && final_tier == ContentTier::Open && !adult_ok {
            return Err(kaleido_core::CoreError::BadRequest(
                "放宽到开放档需要成年确认（adultConfirmed=true）".into(),
            ));
        }
        sess.content_tier = final_tier;
        sess.user_tier_request = req_tier;
        if body.adult_confirmed.is_some() {
            sess.adult_confirmed = adult_ok;
        }
        Ok(())
    }) {
        Ok(s) => Json(s).into_response(),
        Err(kaleido_core::CoreError::Conflict(msg)) => return conflict("ST_CONFLICT", msg),
        Err(e) => map_core_err(e),
    }
}

/// Ensure first-open / empty session has an opening monologue (idempotent).
async fn ensure_opening(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // M-2 (CAS): perform ownership check + seed + save in a single locked
    // transaction via update_session to prevent concurrent overwrites.
    // We need the pack to seed; load it first (it does not depend on the
    // session mutation), then verify ownership inside the closure.
    let probe = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if probe.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    let pack = match state.packs.get(&probe.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let pack_ref = &pack;
    let user_id = session.user_id.clone();
    match state.sessions_tavern.update_session(&id, |sess| {
        if sess.owner.as_deref() != Some(&user_id) {
            return Err(kaleido_core::CoreError::Forbidden(format!(
                "session not owned by user: {id}"
            )));
        }
        if sess.pack_missing {
            return Err(kaleido_core::CoreError::BadRequest(
                "pack missing: session is read-only".into(),
            ));
        }
        seed_opening_if_needed(sess, pack_ref);
        Ok(())
    }) {
        Ok(saved) => {
            Json(json!({ "seeded": saved.opening_seeded, "session": saved })).into_response()
        }
        Err(e) => map_core_err(e),
    }
}

/// List whole-novel summary + key nodes for side-branch picker.
async fn list_side_branches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if sess.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    let pack = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let resume = if sess.play_mode == kaleido_core::PlayMode::Side {
        sess.resume_node_id.clone().or_else(|| sess.node_id.clone())
    } else {
        sess.node_id.clone()
    };
    let catalog = build_side_branch_catalog(&pack, resume);
    Json(catalog).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnterSideBranchRequest {
    node_id: String,
}

/// Enter a side branch at a key node: switch to side mode + seed side opening.
async fn enter_side_branch_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<EnterSideBranchRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if sess.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    if sess.active_run_id.is_some() {
        return conflict("ST_TURN_BUSY", "turn in progress; stop or wait before entering side branch");
    }
    // M-2 (CAS): capture revision for optimistic-concurrency save below.
    let base_revision = sess.updated_at.clone();
    let pack = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let node_id = body.node_id.trim();
    if node_id.is_empty() {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "nodeId required".into(),
        ));
    }
    if let Err(e) = enter_side_branch(&mut sess, &pack, node_id) {
        return map_core_err(e);
    }
    match state.sessions_tavern.save_with_revision(sess, &base_revision) {
        Ok(s) => Json(s).into_response(),
        Err(kaleido_core::CoreError::Conflict(_msg)) => return conflict("ST_CONCURRENT_WRITE", "session was modified concurrently; please retry"),
        Err(e) => map_core_err(e),
    }
}





#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetFocusRequest {
    /// character id, or null/"auto" to keep rotation-only
    #[serde(default)]
    character_id: Option<String>,
    #[serde(default)]
    speaker_rotation: Option<bool>,
}

async fn set_focus_character(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SetFocusRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if sess.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    if sess.active_run_id.is_some() {
        return conflict("ST_TURN_BUSY", "turn in progress");
    }
    // M-2 (CAS): capture revision for optimistic-concurrency save below.
    let base_revision = sess.updated_at.clone();
    if let Some(rot) = body.speaker_rotation {
        sess.speaker_rotation = rot;
    }
    if let Some(cid) = body.character_id {
        let cid = cid.trim().to_string();
        if cid.is_empty() || cid == "auto" {
            kaleido_core::ensure_focus_character(&mut sess);
        } else {
            if !sess.present_character_ids.is_empty() && !sess.present_character_ids.contains(&cid) {
                // allow if in pack
                if let Ok(pack) = state.packs.get(&sess.pack_id) {
                    if !pack.characters.iter().any(|c| c.id == cid) {
                        return map_core_err(kaleido_core::CoreError::BadRequest(
                            format!("character not present: {cid}"),
                        ));
                    }
                    if !sess.present_character_ids.contains(&cid) {
                        sess.present_character_ids.push(cid.clone());
                    }
                }
            }
            sess.focus_character_id = Some(cid);
        }
    } else {
        kaleido_core::ensure_focus_character(&mut sess);
    }
    match state.sessions_tavern.save_with_revision(sess, &base_revision) {
        Ok(s) => Json(s).into_response(),
        Err(kaleido_core::CoreError::Conflict(_msg)) => return conflict("ST_CONCURRENT_WRITE", "session was modified concurrently; please retry"),
        Err(e) => map_core_err(e),
    }
}


#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RebindVesselRequest {
    /// New vessel character id from pack (or empty to clear for isekai/extra)
    #[serde(default)]
    vessel_character_id: Option<String>,
    /// Optional entryRole override when rebinding (supporting/protagonist/extra/isekai)
    #[serde(default)]
    entry_role: Option<String>,
}

/// ST-11: rebind player vessel body mid-session.
/// Updates entry.vesselCharacterId + player.controlCharacterId; appends vessel_change note.
async fn rebind_vessel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RebindVesselRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if sess.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    if sess.active_run_id.is_some() {
        return conflict("ST_TURN_BUSY", "turn in progress; stop or wait before rebind");
    }

    let pack = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    // M-2 (CAS): capture revision for optimistic-concurrency save below.
    let base_revision = sess.updated_at.clone();

    let old_vessel = sess
        .entry
        .vessel_character_id
        .clone()
        .or_else(|| sess.player.control_character_id.clone());

    let new_vessel = body
        .vessel_character_id
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(ref vid) = new_vessel {
        if !pack.characters.iter().any(|c| c.id == *vid) {
            return map_core_err(kaleido_core::CoreError::BadRequest(format!(
                "vessel not in pack: {vid}"
            )));
        }
    }

    // optional entry role
    if let Some(role_s) = body.entry_role.as_ref().map(|s| s.trim().to_ascii_lowercase()) {
        let role = match role_s.as_str() {
            "supporting" => Some(kaleido_core::EntryRole::Supporting),
            "protagonist" => Some(kaleido_core::EntryRole::Protagonist),
            "extra" => Some(kaleido_core::EntryRole::Extra),
            "isekai" => Some(kaleido_core::EntryRole::Isekai),
            "" => None,
            _ => {
                return map_core_err(kaleido_core::CoreError::BadRequest(
                    "entryRole must be supporting|protagonist|extra|isekai".into(),
                ));
            }
        };
        if let Some(r) = role {
            sess.entry.entry_role = Some(r);
        }
    }

    let role = sess.entry.entry_role;
    // A9: isekai should not require vessel
    let is_isekai = matches!(role, Some(kaleido_core::EntryRole::Isekai));
    let is_extra = matches!(role, Some(kaleido_core::EntryRole::Extra));

    if (matches!(role, Some(kaleido_core::EntryRole::Protagonist) | Some(kaleido_core::EntryRole::Supporting))
        || role.is_none())
        && new_vessel.is_none()
        && !is_isekai
        && !is_extra
    {
        // allow clear only for isekai/extra; for protagonist/supporting require vessel
        if matches!(role, Some(kaleido_core::EntryRole::Protagonist) | Some(kaleido_core::EntryRole::Supporting)) {
            return map_core_err(kaleido_core::CoreError::BadRequest(
                "protagonist/supporting rebind requires vesselCharacterId".into(),
            ));
        }
    }

    let control_id = if is_isekai {
        Some("player_isekai".to_string())
    } else if is_extra && new_vessel.is_none() {
        Some("player_extra".to_string())
    } else {
        new_vessel.clone()
    };

    sess.entry.vessel_character_id = new_vessel.clone();
    sess.player.control_character_id = control_id.clone();

    // ensure vessel stays present in scene cast
    if let Some(ref vid) = new_vessel {
        if !sess.present_character_ids.contains(vid) {
            sess.present_character_ids.push(vid.clone());
        }
    }

    // if focus was old vessel body, move focus to another present NPC
    if let Some(ref foc) = sess.focus_character_id.clone() {
        if old_vessel.as_ref() == Some(foc) || control_id.as_ref() == Some(foc) {
            let next = sess
                .present_character_ids
                .iter()
                .find(|id| control_id.as_ref() != Some(id))
                .cloned()
                .or_else(|| sess.present_character_ids.first().cloned());
            sess.focus_character_id = next;
        }
    }
    kaleido_core::ensure_focus_character(&mut sess);

    let old_name = old_vessel
        .as_ref()
        .and_then(|id| pack.characters.iter().find(|c| c.id == *id).map(|c| c.name.clone()))
        .unwrap_or_else(|| old_vessel.clone().unwrap_or_else(|| "无".into()));
    let new_name = match &control_id {
        Some(id) if id == "player_isekai" => "异世界身份".into(),
        Some(id) if id == "player_extra" => "路人身份".into(),
        Some(id) => pack
            .characters
            .iter()
            .find(|c| c.id == *id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| id.clone()),
        None => "未绑定".into(),
    };

    let note = format!(
        "〔换壳 vessel_change〕玩家身子：{} → {}。关系记忆保留，身份锚点已更新。",
        old_name, new_name
    );
    sess.messages.push(TavernMessage {
        id: format!("msg-{}", Uuid::new_v4()),
        role: "assistant".into(),
        content: note,
        created_at: Utc::now().to_rfc3339(),
        options: vec![],
        engine_tag: None,
        program: None,
        reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
    });

    match state.sessions_tavern.save_with_revision(sess, &base_revision) {
        Ok(s) => Json(s).into_response(),
        Err(kaleido_core::CoreError::Conflict(_msg)) => return conflict("ST_CONCURRENT_WRITE", "session was modified concurrently; please retry"),
        Err(e) => map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSaveRequest {
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RewindRequest {
    /// 回退回合数（N>=1，缺省 1）
    #[serde(default)]
    steps: Option<usize>,
}

async fn list_saves(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // F1: ownership check.
    if let Err(e) = state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        return map_core_err(e);
    }
    match state.sessions_tavern.list_saves(&id) {
        Ok(list) => Json(json!({ "saves": list })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// 世界线视图（/line 全景）：按分叉关系组织存档线，标注当前所在世界线。
async fn get_worldline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.sessions_tavern.worldline(&id) {
        Ok(view) => Json(view).into_response(),
        Err(e) => map_core_err(e),
    }
}

// ─── T2 世界认知 handlers（U2 实体图谱 / U7 真相账本）────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorldEntitiesQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

async fn world_entities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WorldEntitiesQuery>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state
        .sessions_tavern
        .world_entities(&id, query.kind, query.q)
    {
        Ok(view) => Json(view).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn world_entity_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, entity_id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state
        .sessions_tavern
        .world_entity_detail(&id, &entity_id)
    {
        Ok(view) => Json(view).into_response(),
        Err(e) => map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorldEventsRequest {
    events: Vec<kaleido_core::world_state::WorldEvent>,
}

async fn world_apply_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<WorldEventsRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.sessions_tavern.world_apply_events(&id, req.events) {
        Ok(view) => Json(view).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn world_truth(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.sessions_tavern.world_truth(&id) {
        Ok(view) => Json(view).into_response(),
        Err(e) => map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorldTruthCheckRequest {
    entity_id: String,
    key: String,
    expected: Value,
}

async fn world_truth_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<WorldTruthCheckRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state
        .sessions_tavern
        .world_truth_check(&id, req.entity_id, req.key, req.expected)
    {
        Ok(view) => Json(view).into_response(),
        Err(e) => map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssistantRequest {
    message: String,
    /// 可选：前端本地保存的助手对话历史（[{role, content}]，不含本次 message）。
    /// 有则作为多轮上下文传给 LLM，解决助手"失忆"问题。
    #[serde(default)]
    history: Vec<AssistHistoryMsg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssistHistoryMsg {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: String,
}

/// 剧情助手系统提示（tavern 剧场与 story/冒险/跑团共用）。
pub(crate) const ASSISTANT_SYSTEM_PROMPT: &str = "你是这场角色扮演的「助手」，职责是保障和优化这场扮演：诊断回复质量（变短/复读/换语言）、\
    调配置与参数、检查记忆账本（场景摘要/事件/好感/关系）、解释当前剧情状态。\
    你有全局视角：能读剧情记录、记忆账本与剧本。但你绝不代写剧情——剧情正文永远只出自剧情模型。\
    回答用中文，简洁直接，不用角色扮演腔。\
    若玩家要求生成可视化面板（地图/线索图谱/线索板/装备栏等），输出格式：【面板】{\"name\":\"面板名\",\"kind\":\"markdown|svg|html\",\"content\":\"...\"}【/面板】，正文不要重复面板内容。";

/// P0-2: /config 白名单 keys。
const CONFIG_KEYS: &[&str] = &[
    "strict_mode_boost",
    "pacing",
    "style_guidance",
    // [fix 2026-08-15 文风外挂] /config style_source=<pack标题> 外挂指定作品文风
    "style_source",
    // [fix 2026-08-15 叙述视角] /config pov=first|third —— 覆盖蒸馏文风的人称设定
    "pov",
];

/// 解析 `/config key=value ...`：白名单 + 值校验。任一非法 token 返回 None。
fn parse_config_args(rest: &str) -> Option<Vec<(String, Value)>> {
    let mut out = Vec::new();
    for tok in rest.split_whitespace() {
        let (k, v) = tok.split_once('=')?;
        let k = k.trim();
        let v = v.trim();
        if !CONFIG_KEYS.contains(&k) {
            return None;
        }
        let val = match k {
            "strict_mode_boost" | "pacing" => {
                let f: f64 = v.parse().ok()?;
                if !(0.0..=1.0).contains(&f) {
                    return None;
                }
                json!(f)
            }
            // [fix 2026-08-15 文风外挂] style_guidance 支持长文风指引（≤2000 字）：
            // 用户可将 style-analysis 生成的 stylePrompt 整段直挂（18+ 描写风格外挂）。
            "style_guidance" => {
                if v.is_empty() || v.chars().count() > 2000 {
                    return None;
                }
                json!(v)
            }
            // [fix 2026-08-15 文风外挂] style_source=<pack标题>：值允许含中文 pack 名，≤40 字
            "style_source" => {
                if v.is_empty() || v.chars().count() > 40 {
                    return None;
                }
                json!(v)
            }
            // [fix 2026-08-15 叙述视角] pov=first|third —— 覆盖蒸馏文风的人称设定；
            // pov=default 表示清除覆盖、回退蒸馏文风的人称。
            "pov" => {
                if v != "first" && v != "third" && v != "default" {
                    return None;
                }
                json!(v)
            }
            _ => return None,
        };
        out.push((k.to_string(), val));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 将校验后的配置写入 sess.player.flags（保留已有 keys）。
fn apply_config_flags(sess: &mut TavernSession, pairs: &[(String, Value)]) {
    if !sess.player.flags.is_object() {
        sess.player.flags = json!({});
    }
    if let Some(map) = sess.player.flags.as_object_mut() {
        for (k, v) in pairs {
            map.insert(k.clone(), v.clone());
        }
    }
}

/// [fix 2026-08-15 文风外挂] 按 pack 标题（支持模糊包含匹配）读取指定作品的
/// narrative_style，合成为 style_guidance 注入文本。找不到 pack 或无文风 → Err。
/// 用户场景：宿醉 18+ 清淡 → /config style_source=智取美母 外挂浓烈 18+ 描写风格。
fn resolve_external_style(state: &AppState, pack_query: &str) -> Result<String, String> {
    let packs = state.packs.list().map_err(|e| format!("pack 列表读取失败：{e}"))?;
    let q = pack_query.trim();
    if q.is_empty() {
        return Err("pack 标题不能为空".into());
    }
    // 精确匹配优先，其次包含匹配
    let found = packs
        .iter()
        .find(|p| p.title == q)
        .or_else(|| packs.iter().find(|p| p.title.contains(q) || q.contains(&p.title)));
    let pid = match found {
        Some(p) => p.id.clone(),
        None => {
            let names: Vec<&str> = packs.iter().map(|p| p.title.as_str()).collect();
            return Err(format!("未找到 pack「{q}」，现有：{}", names.join(" / ")));
        }
    };
    let pack = state
        .packs
        .get(&pid)
        .map_err(|e| format!("pack 读取失败：{e}"))?;
    // [fix 2026-08-15 落库优先] 若 pack 已有 style-profiles/<pid>.txt（style-analysis
    // 落库的完整 stylePrompt），优先使用——这是特化文风（如 18+ 浓烈描写），
    // 比蒸馏 narrative_style 更精细。无落库则回退蒸馏文风。
    let profile_path = state
        .app_state
        .data_root()
        .story_packs_dir()
        .join(&pid)
        .join("style-profiles")
        .join(format!("{pid}.txt"));
    if let Ok(profile_text) = std::fs::read_to_string(&profile_path) {
        let pt = profile_text.trim();
        if !pt.is_empty() {
            return Ok(format!(
                "（外挂作品《{}》文风·特化版）{}",
                pack.title, pt
            ));
        }
    }
    let ns = pack
        .stage_director
        .resolved_snapshot
        .as_ref()
        .and_then(|snap| snap.narrative_style.as_ref())
        .ok_or_else(|| format!("pack「{}」尚无蒸馏出的文风（先跑 distil-world）", pack.title))?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = ns.get("style").and_then(|v| v.as_str()) {
        if !s.trim().is_empty() {
            parts.push(format!("叙事视角/人称：{}", s.trim()));
        }
    }
    if let Some(t) = ns.get("tone").and_then(|v| v.as_str()) {
        if !t.trim().is_empty() {
            parts.push(format!("基调：{}", t.trim()));
        }
    }
    if let Some(p) = ns.get("prose_guidance").and_then(|v| v.as_str()) {
        if !p.trim().is_empty() {
            parts.push(format!("行文要求：{}", p.trim()));
        }
    }
    if parts.is_empty() {
        return Err(format!("pack「{}」文风字段为空", pack.title));
    }
    Ok(format!(
        "（外挂作品《{}》文风）{}",
        pack.title,
        parts.join("；")
    ))
}

/// 剧情助手（吸收自梨园 assistant-gateway 双agent分治的最小版）：
/// 独立 LLM 会话，带全局视角（剧情记录+记忆账本+剧本），绝不代写剧情。
async fn assistant_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AssistantRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let msg = body.message.trim().to_string();
    if msg.is_empty() {
        return bad_request("ST_EMPTY_MESSAGE", "empty message");
    }
    // P0-1: story_command —— /reroll 重生成上一回合、/rewind N 回退（LLM 调用之前识别）
    if msg.starts_with("/reroll") {
        return match do_reroll(&state, &id).await {
            Ok((turn, last_user)) => Json(json!({
                "reply": "已重新生成上一回合（即将重发）",
                "action": "reroll",
                "lastUserMessage": last_user,
                "turn": turn,
            }))
            .into_response(),
            Err(kaleido_core::CoreError::BadRequest(e)) => {
                Json(json!({ "reply": format!("重生成失败：{e}"), "action": "error" })).into_response()
            }
            Err(e) => map_core_err(e),
        };
    }
    if let Some(steps) = parse_rewind_steps(&msg) {
        return match do_rewind(&state, &id, steps).await {
            Ok((turn, rewound)) => Json(json!({
                "reply": format!("已回退 {steps} 回合"),
                "action": "rewind",
                "rewound": rewound,
                "turn": turn,
            }))
            .into_response(),
            Err(kaleido_core::CoreError::BadRequest(e)) => {
                Json(json!({ "reply": format!("回退失败：{e}"), "action": "error" })).into_response()
            }
            Err(e) => map_core_err(e),
        };
    }
    // P0-2/P0-3: 剧情助手命令 —— /config key=value /remember /event（LLM 调用之前识别，整行匹配）
    if msg.starts_with("/config") {
        return handle_config_cmd(&state, &id, &msg).await;
    }
    if msg.starts_with("/remember") {
        return handle_remember_cmd(&state, &id, &msg).await;
    }
    if msg.starts_with("/event") {
        return handle_event_cmd(&state, &id, &msg).await;
    }
    if msg.starts_with("/time") {
        return handle_time_cmd(&state, &id, &msg).await;
    }
    if msg.starts_with("/weather") {
        return handle_weather_cmd(&state, &id, &msg).await;
    }
    if msg.starts_with("/season") {
        return handle_season_cmd(&state, &id, &msg).await;
    }
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    let pack = state.packs.get(&sess.pack_id).ok();
    let pack_title = pack.as_ref().map(|p| p.title.clone()).unwrap_or_default();

    let sys = ASSISTANT_SYSTEM_PROMPT;

    let mut ctx = String::new();
    ctx.push_str(&format!(
        "剧本：{}\n当前回合：{}\n当前节点：{}\n\n",
        pack_title,
        sess.turn,
        sess.node_id.as_deref().unwrap_or("?")
    ));
    // 全本视野：章节目录（骨架）+ 全书向量检索（懒建索引，失败静默降级）+ 世界线（分支路径）
    if let Some(p) = &pack {
        let toc = pack_toc_text(p);
        if !toc.is_empty() {
            ctx.push_str(&toc);
        }
        let vctx = pack_vector_ctx(&state, p, &msg);
        if !vctx.is_empty() {
            ctx.push_str(&vctx);
        }
    }
    if let Ok(wv) = state.sessions_tavern.worldline(&id) {
        let wt = format_worldline_text(&wv);
        if !wt.is_empty() {
            ctx.push_str(&wt);
        }
    }
    if !sess.memory_l1.scene_summary.is_empty() {
        ctx.push_str(&format!("【场景摘要】\n{}\n\n", sess.memory_l1.scene_summary));
    }
    if !sess.memory_l2.events.is_empty() {
        ctx.push_str("【事件账本】\n");
        for ev in sess.memory_l2.events.iter().rev().take(8) {
            ctx.push_str(&format!("• {}（{}）: {}\n", ev.id, ev.kind, ev.summary));
        }
        ctx.push('\n');
    }
    // 关系状态（与 build_tavern_system_prompt 同口径：从 L2 统计亲密事件数）
    // ST-FIX: 弱信号 contains("亲")/("床") 会把「母亲」的「亲」字误计为接吻（宿醉剧本
    // 每条事件摘要都含「母亲」），导致凭空生成「接过吻至少 15 次」的关系状态注入 prompt。
    // 改用强信号：只有明确接吻词（吻/亲嘴/亲吻/亲热/接吻）或 kind==romance 才计数。
    let kiss_count = sess.memory_l2.events.iter().filter(|e| {
        e.kind == "romance" ||
        e.summary.to_lowercase().contains("接吻") ||
        e.summary.to_lowercase().contains("亲嘴") ||
        e.summary.to_lowercase().contains("亲吻") ||
        e.summary.to_lowercase().contains("亲热") ||
        e.summary.to_lowercase().contains("吻")
    }).count();
    if kiss_count > 0 {
        ctx.push_str(&format!("【关系状态】你们已发生亲密互动（至少 {kiss_count} 次），关系已确立/升级。\n\n"));
    }
    ctx.push_str("【最近剧情】\n");
    for m in sess.messages.iter().rev().take(10).rev() {
        let role = if m.role == "user" { "你" } else { "旁白" };
        // 前后各取 150 字符（消息长时保留尾部信息，如关系升级往往在结尾）
        let content: String = m
            .content
            .chars()
            .enumerate()
            .filter(|(i, _)| *i < 150 || (m.content.chars().count() as i64 - *i as i64) <= 150)
            .map(|(_, c)| c)
            .collect();
        ctx.push_str(&format!("[{}] {}\n", role, content));
    }
    let user = format!("玩家对你说：{}\n\n{}", msg, ctx);

    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    // 多轮助手历史：system + 前端 history（若有）+ 本次 user（带剧情上下文）。
    // 解决助手"失忆"——历史使连续咨询可承接上文。
    let mut msgs: Vec<serde_json::Value> = Vec::new();
    msgs.push(json!({"role": "system", "content": sys}));
    for h in body.history.iter().rev().take(12).rev() {
        let role = if h.role == "user" { "user" } else { "assistant" };
        let content = h.content.trim();
        if content.is_empty() {
            continue;
        }
        msgs.push(json!({"role": role, "content": content}));
    }
    msgs.push(json!({"role": "user", "content": user}));
    match crate::llm_stream::stream_chat_completions_msgs_dispatch(&llm.base_url, &llm.api_key, &llm.model, &prov_kind, msgs, 0.1, 8192, 90, |_| true).await {
        Ok(reply) => {
            let reply = reply.trim().to_string();
            // 面板（吸收自梨园 panels.ts）：提取【面板】块并回写 session.panels
            let (clean_reply, panels) = split_panels_from_narrative(&reply);
            if !panels.is_empty() {
                for p in &panels {
                    if let Some(existing) = sess.panels.iter_mut().find(|x| x.name == p.name) {
                        *existing = p.clone();
                    } else {
                        sess.panels.push(p.clone());
                    }
                }
                if let Err(e) = state.sessions_tavern.save(sess.clone()) {
                    tracing::warn!(error = %e, "assistant failed to save panels");
                }
            }
            // 正文只回干净文本；若只剩面板则给一句汇总
            let reply = if clean_reply.trim().is_empty() && !panels.is_empty() {
                format!(
                    "已生成可视化面板：{}",
                    panels.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("、")
                )
            } else {
                clean_reply
            };
            Json(json!({ "reply": reply })).into_response()
        }
        Err(e) => internal("ST_ASSISTANT_LLM_FAILED", e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoryAssistantRequest {
    message: String,
    /// 可选：前端本地保存的助手对话历史（[{role, content}]，不含本次 message）。
    #[serde(default)]
    history: Vec<AssistHistoryMsg>,
    /// 冒险/跑团剧情消息（[{role, content}]，最近 N 条，由前端传入）。
    #[serde(default)]
    messages: Vec<AssistHistoryMsg>,
    /// 剧本标题（可选，空则省略）。
    #[serde(default)]
    title: String,
    /// 会话类型: story(冒险/跑团, 默认) / chat(对话模式)。仅影响上下文角色标签。
    #[serde(default)]
    kind: String,
    /// 世界书 ids（冒险/跑团/对话模式：前端从 wb/cc 选择解析传入，用于设定检索）。
    #[serde(default)]
    world_book_ids: Vec<String>,
}

/// 剧情助手（story/冒险/跑团版）：无 TavernSession，剧情上下文来自客户端传入的
/// messages（冒险/跑团是 localStorage 会话，不走 sessions_tavern 存储）。
/// 与 assistant_chat 同语义：独立 LLM 会话、全局视角、绝不代写剧情。
pub(crate) async fn story_assistant_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StoryAssistantRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let msg = body.message.trim().to_string();
    if msg.is_empty() {
        return bad_request("ST_EMPTY_MESSAGE", "empty message");
    }
    let mut ctx = String::new();
    if !body.title.trim().is_empty() {
        ctx.push_str(&format!("剧本：{}\n\n", body.title.trim()));
    }
    // 世界书设定检索（wb ids 来自前端 wb/cc 选择；索引未建/embed 失败静默降级）
    if !body.world_book_ids.is_empty() {
        let wctx = wb_vector_ctx(&state, &body.world_book_ids, &msg);
        if !wctx.is_empty() {
            ctx.push_str(&wctx);
        }
    }
    ctx.push_str("【最近剧情】\n");
    for m in body.messages.iter().rev().take(10).rev() {
        let role = if m.role == "user" {
            "你"
        } else if body.kind == "chat" {
            "对方"
        } else {
            "旁白"
        };
        // 前后各取 150 字符（消息长时保留尾部信息）
        let content: String = m
            .content
            .chars()
            .enumerate()
            .filter(|(i, _)| *i < 150 || (m.content.chars().count() as i64 - *i as i64) <= 150)
            .map(|(_, c)| c)
            .collect();
        ctx.push_str(&format!("[{}] {}\n", role, content));
    }
    let user = format!("玩家对你说：{}\n\n{}", msg, ctx);

    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let mut msgs: Vec<serde_json::Value> = Vec::new();
    msgs.push(json!({"role": "system", "content": ASSISTANT_SYSTEM_PROMPT}));
    for h in body.history.iter().rev().take(12).rev() {
        let role = if h.role == "user" { "user" } else { "assistant" };
        let content = h.content.trim();
        if content.is_empty() {
            continue;
        }
        msgs.push(json!({"role": role, "content": content}));
    }
    msgs.push(json!({"role": "user", "content": user}));
    match crate::llm_stream::stream_chat_completions_msgs(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        msgs,
        0.1,
        8192,
        90,
        |_| true,
    )
    .await
    {
        Ok(reply) => {
            // 面板块同样提取剥离（冒险页无 panels 展示容器，正文只回干净文本）
            let (clean_reply, panels) = split_panels_from_narrative(&reply.trim());
            let reply = if clean_reply.trim().is_empty() && !panels.is_empty() {
                format!(
                    "已生成可视化面板：{}",
                    panels.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("、")
                )
            } else {
                clean_reply
            };
            Json(json!({ "reply": reply })).into_response()
        }
        Err(e) => internal("ST_ASSISTANT_LLM_FAILED", e),
    }
}

/// 剧情助手「全本视野」辅助：章节目录（全本骨架，含章节目标；超长截断）。
fn pack_toc_text(pack: &StoryPack) -> String {
    let mut out = String::from("【章节目录】\n");
    for ch in &pack.chapters {
        out.push_str(&format!("{}. {}", ch.order, ch.title));
        if !ch.goals.is_empty() {
            let g: Vec<&str> = ch.goals.iter().take(2).map(|s| s.as_str()).collect();
            out.push_str(&format!("（目标：{}）", g.join("/")));
        }
        out.push('\n');
        if out.chars().count() > 6000 {
            out.push_str("……（目录过长已截断）\n");
            break;
        }
    }
    out.push('\n');
    out
}

/// 世界线摘要（玩家分支路径：主线/分叉 + 各线存档回合）。
fn format_worldline_text(wv: &kaleido_core::WorldlineView) -> String {
    let mut out = String::from("【世界线】当前分支：");
    out.push_str(&wv.current_worldline_id);
    out.push('\n');
    for line in &wv.lines {
        if line.saves.is_empty() {
            continue;
        }
        let turns: Vec<String> = line.saves.iter().map(|s| s.turn.to_string()).collect();
        out.push_str(&format!(
            "分支 {}：{} 个存档（回合 {}）\n",
            line.id,
            line.saves.len(),
            turns.join("→")
        ));
    }
    out.push('\n');
    out
}

/// pack 正文向量索引 id（VectorIndexStore 按 id 分文件，中文自动 safe 化）。
fn pack_index_id(pack_id: &str) -> String {
    format!("pack-{pack_id}")
}

/// 读 pack 全部章节正文，按 ~500 字符切块（不重叠）。返回 (uid, 文本)。
fn pack_body_blocks(pack: &StoryPack, pack_root: &std::path::Path) -> Vec<(String, String)> {
    const CHUNK: usize = 500;
    let mut blocks = Vec::new();
    for ch in &pack.chapters {
        if ch.body_path.is_empty() {
            continue;
        }
        let path = pack_root.join(&ch.body_path);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        let chars: Vec<char> = text.chars().collect();
        for (i, chunk) in chars.chunks(CHUNK).enumerate() {
            let block: String = chunk.iter().collect();
            blocks.push((format!("ch{}-{}", ch.order, i), block));
        }
    }
    blocks
}

/// 懒加载建 pack 正文向量索引（已有索引直接跳过；embed 不可用/无正文静默返回 Err）。
fn ensure_pack_vector_index(state: &AppState, pack: &StoryPack) -> Result<(), String> {
    let index_id = pack_index_id(&pack.id);
    let existing = state.vector_index.load(&index_id);
    if !existing.entries.is_empty() {
        return Ok(());
    }
    if !crate::embed_local::inline_enabled() {
        return Err("embed not enabled".into());
    }
    crate::embed_local::ensure_local().map_err(|e| e)?;
    let pack_root = state.packs.pack_dir(&pack.id).map_err(|e| e.to_string())?;
    let blocks = pack_body_blocks(pack, &pack_root);
    if blocks.is_empty() {
        return Err("no chapter bodies".into());
    }
    let texts: Vec<String> = blocks.iter().map(|(_, t)| t.clone()).collect();
    let vecs = crate::embed_local::embed_many(&texts).map_err(|e| e)?;
    let entries: Vec<kaleido_core::VectorIndexEntry> = blocks
        .iter()
        .zip(vecs.iter())
        .map(|((uid, text), v)| kaleido_core::VectorIndexEntry {
            uid: uid.clone(),
            world: index_id.clone(),
            text: text.clone(),
            text_hash: String::new(),
            vector: v.clone(),
        })
        .collect();
    let file = kaleido_core::VectorIndexFile {
        world_book_id: index_id.clone(),
        model: "BAAI/bge-small-zh-v1.5".into(),
        dim: vecs.first().map(|v| v.len()).unwrap_or(0),
        entries,
        updated_at: None,
    };
    state
        .vector_index
        .save(file)
        .map_err(|e| e.to_string())?;
    tracing::info!(pack = %pack.id, blocks = blocks.len(), "pack vector index built");
    Ok(())
}

/// 全书向量检索：问题 embed → pack 索引 top 3 命中块 → 注入助手上下文。
/// 索引未建则懒建（首次可能慢几秒）；embed/检索失败静默降级（不阻塞助手）。
fn pack_vector_ctx(state: &AppState, pack: &StoryPack, query: &str) -> String {
    let query = query.trim();
    if query.is_empty() {
        return String::new();
    }
    if let Err(e) = ensure_pack_vector_index(state, pack) {
        tracing::debug!(pack = %pack.id, err = %e, "pack vector ctx skipped");
        return String::new();
    }
    let index_id = pack_index_id(&pack.id);
    let idx = state.vector_index.load(&index_id);
    if idx.entries.is_empty() {
        return String::new();
    }
    let qv = match crate::embed_local::embed_one(query) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    // 正文长块检索：阈值比世界书（0.42）大幅放宽——泛化问题（"前三章讲什么"）
    // embed 后与叙事块相似度偏低，0.35 会空命中（实测 Q3 教训）
    let settings = kaleido_core::VectorActivationSettings {
        enabled: true,
        score_threshold: 0.28,
        top_k: 4,
    };
    let hits = kaleido_core::rank_hits(&idx, &qv, &settings);
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::from("【全书检索命中】\n");
    for h in hits {
        if let Some(e) = idx.entries.iter().find(|e| e.uid == h.uid) {
            let text: String = e.text.chars().take(400).collect();
            out.push_str(&format!("[{}]\n{}\n\n", e.uid, text));
        }
    }
    out
}

/// 手动重建 pack 正文向量索引（正文改动后调用；删除旧索引强制重算）。
async fn rebuild_pack_vector_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let pack = match state.packs.get(&id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let index_id = pack_index_id(&id);
    let _ = state.vector_index.delete(&index_id);
    match ensure_pack_vector_index(&state, &pack) {
        Ok(()) => {
            let idx = state.vector_index.load(&index_id);
            Json(json!({ "ok": true, "entryCount": idx.entries.len() })).into_response()
        }
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

/// 世界书设定检索（story/冒险/跑团/对话版）：复用 W5 collect_vector_hits，
/// 命中条目按 uid 回索引取 text 注入。索引未建/embed 失败静默降级。
fn wb_vector_ctx(state: &AppState, wb_ids: &[String], query: &str) -> String {
    let query = query.trim();
    if query.is_empty() || wb_ids.is_empty() {
        return String::new();
    }
    // 世界书条目短：阈值用 0.35（介于世界书默认 0.42 与 pack 正文 0.28 之间）
    let settings = kaleido_core::VectorActivationSettings {
        enabled: true,
        score_threshold: 0.35,
        top_k: 4,
    };
    let (hits, verr) = crate::collect_vector_hits(state, wb_ids, query, &settings);
    if let Some(e) = verr {
        tracing::debug!(err = %e, "wb vector ctx skipped");
        return String::new();
    }
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::from("【设定检索命中】\n");
    for h in hits {
        for wid in wb_ids {
            let idx = state.vector_index.load(wid);
            if let Some(e) = idx.entries.iter().find(|e| e.uid == h.uid) {
                let text: String = e.text.chars().take(400).collect();
                out.push_str(&format!("[{}]\n{}\n\n", e.uid, text));
                break;
            }
        }
    }
    out
}

async fn create_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CreateSaveRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // F1: ownership check.
    if let Err(e) = state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        return map_core_err(e);
    }
    match state.sessions_tavern.create_save(&id, body.label) {
        Ok(s) => Json(s).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn delete_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, save_id)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // F1: ownership check.
    if let Err(e) = state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        return map_core_err(e);
    }
    match state.sessions_tavern.delete_save(&id, &save_id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn restore_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, save_id)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // F1: ownership check.
    if let Err(e) = state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        return map_core_err(e);
    }
    // Map "turn in progress" to 409
    match state.sessions_tavern.restore_save(&id, &save_id) {
        Ok(s) => Json(s).into_response(),
        Err(kaleido_core::CoreError::BadRequest(msg)) if msg.contains("turn in progress") => return conflict("ST_CONFLICT", msg),
        Err(e) => map_core_err(e),
    }
}

/// [跨会话分叉] 存档分叉到新会话（旧会话不动，新会话独立跑新剧情）。
async fn fork_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, save_id)): Path<(String, String)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // ownership: source session must belong to caller
    if let Err(e) = state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        return map_core_err(e);
    }
    let label = body.get("label").and_then(|v| v.as_str()).map(|s| s.to_string());
    match state.sessions_tavern.fork_save_to_session(&id, &save_id, label) {
        Ok(s) => Json(json!({"ok": true, "session": s})).into_response(),
        Err(e) => map_core_err(e),
    }
}

// ─── P0-1 story_command（/rewind N 回退、/reroll 重生成）────────────────────

/// 解析 "/rewind" / "/rewind 3" / "/rewind3"；无数字或缺省 → 1。
fn parse_rewind_steps(msg: &str) -> Option<usize> {
    if !msg.starts_with("/rewind") {
        return None;
    }
    let rest = msg.trim_start_matches("/rewind").trim_start();
    if rest.is_empty() {
        return Some(1);
    }
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return Some(1);
    }
    Some(digits.parse::<usize>().unwrap_or(1).max(1))
}

/// 共享回退逻辑：校验会话/进行中回合 → restore_checkpoint(steps) → save。
/// 返回 (turn, 实际回退步数)。
async fn do_rewind(
    state: &AppState,
    id: &str,
    steps: usize,
) -> kaleido_core::CoreResult<(u32, usize)> {
    let mut sess = state.sessions_tavern.get(id)?;
    if sess.pack_missing {
        return Err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    if sess.active_run_id.is_some() {
        return Err(kaleido_core::CoreError::BadRequest(
            "turn in progress; stop or wait before rewind".into(),
        ));
    }
    let rewound = sess.restore_checkpoint(steps)?;
    let turn = sess.turn;
    if rewound > 0 {
        let _ = state.sessions_tavern.save(sess)?;
    }
    Ok((turn, rewound))
}

/// 共享重生成逻辑：先取当前最后一条 user 消息，再回退 1 回合，save。
/// 返回 (turn, lastUserMessage)。
async fn do_reroll(state: &AppState, id: &str) -> kaleido_core::CoreResult<(u32, String)> {
    let mut sess = state.sessions_tavern.get(id)?;
    if sess.pack_missing {
        return Err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    if sess.active_run_id.is_some() {
        return Err(kaleido_core::CoreError::BadRequest(
            "turn in progress; stop or wait before reroll".into(),
        ));
    }
    // [Swipe 多备选] 旧正文不丢：最后一条 assistant 内容暂存 pending_swipes，下条消息继承
    if let Some(last_asst) = sess.messages.iter().rev().find(|m| m.role == "assistant") {
        if !last_asst.content.is_empty() {
            // 去重：已在 swipes/pending 里的不重复存
            let already = last_asst.swipes.iter().any(|s| s == &last_asst.content) || sess.pending_swipes.iter().any(|s| s == &last_asst.content);
            if !already {
                sess.pending_swipes.push(last_asst.content.clone());
                // cap 10
                if sess.pending_swipes.len() > 10 { sess.pending_swipes.remove(0); }
            }
        }
    }
    let last_user = sess
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let rewound = sess.restore_checkpoint(1)?;
    if rewound == 0 {
        return Err(kaleido_core::CoreError::BadRequest("no checkpoint".into()));
    }
    let turn = sess.turn;
    let _ = state.sessions_tavern.save(sess)?;
    Ok((turn, last_user))
}

// ─── P0-2/P0-3 story_command（/config 配置生效、/remember /event 记忆生效）──────

const SCENE_SUMMARY_MAX: usize = 500;
const EVENT_SUMMARY_MAX: usize = 500;

/// /config key=value —— 白名单配置写入 sess.player.flags（保留已有 keys），保存后生效。
async fn handle_config_cmd(state: &AppState, id: &str, msg: &str) -> Response {
    let rest = msg.trim_start_matches("/config").trim();
    if rest.is_empty() {
        return Json(json!({
            "reply": "用法：/config key=value（可选 strict_mode_boost(0-1) / pacing(0-1) / style_guidance(短句)）",
            "action": "error",
        }))
        .into_response();
    }
    let pairs = match parse_config_args(rest) {
        Some(p) => p,
        None => {
            return Json(json!({
                "reply": "参数不合法：key 须为 strict_mode_boost / pacing / style_guidance，数值须在 0~1，style_guidance 为短句",
                "action": "error",
            }))
            .into_response();
        }
    };
    let mut sess = match state.sessions_tavern.get(id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    // [fix 2026-08-15 文风外挂] /config style_source=<pack标题> 读取指定 pack 蒸馏出的
    // narrative_style，合成 style_guidance 注入回合（用户场景：宿醉 18+ 清淡 →
    // 外挂智取美母的浓烈 18+ 描写风格）。外挂覆盖 tone/style/prose_guidance；
    // style_guidance 显式设置时优先级最高（覆盖外挂）。
    let mut has_style_source = false;
    for (k, v) in pairs.iter() {
        if k == "style_source" {
            has_style_source = true;
            if let Some(sv) = v.as_str() {
                match resolve_external_style(&state, sv) {
                    Ok(guidance) => {
                        sess.player.flags
                            .as_object_mut()
                            .map(|m| m.insert("style_guidance".into(), json!(guidance)));
                    }
                    Err(e) => {
                        return Json(json!({
                            "reply": format!("外挂文风失败：{e}（可用 pack 见 /packs）"),
                            "action": "error",
                        }))
                        .into_response();
                    }
                }
            }
        }
    }
    if has_style_source {
        // 外挂已写入 style_guidance；跳过 apply_config_flags 避免 style_source 本身落 flags
        if let Err(e) = state.sessions_tavern.save(sess) {
            return map_core_err(e);
        }
        let applied = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Json(json!({
            "reply": format!("已生效：{applied}（已外挂对应作品文风）"),
            "action": "config",
            "keys": pairs.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
        }))
        .into_response();
    }
    apply_config_flags(&mut sess, &pairs);
    // [fix 2026-08-15 叙述视角] pov=default → 从 flags 移除 pov，回退蒸馏文风人称
    if pairs.iter().any(|(k, v)| k == "pov" && v.as_str() == Some("default")) {
        if let Some(map) = sess.player.flags.as_object_mut() {
            map.remove("pov");
        }
    }
    if let Err(e) = state.sessions_tavern.save(sess) {
        return map_core_err(e);
    }
    let applied = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    Json(json!({
        "reply": format!("已生效：{applied}"),
        "action": "config",
        "keys": pairs.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// /remember 场景摘要：<text> —— 更新 L1 场景摘要（限 500 字），保存后生效。
async fn handle_remember_cmd(state: &AppState, id: &str, msg: &str) -> Response {
    let rest = msg.trim_start_matches("/remember").trim();
    let text = if let Some(stripped) = rest.strip_prefix("场景摘要：") {
        stripped.trim()
    } else if let Some(stripped) = rest.strip_prefix("场景摘要:") {
        stripped.trim()
    } else {
        rest
    };
    if text.is_empty() {
        return Json(json!({
            "reply": "用法：/remember 场景摘要：<场景摘要文本>",
            "action": "error",
        }))
        .into_response();
    }
    if text.chars().count() > SCENE_SUMMARY_MAX {
        return Json(json!({
            "reply": format!("场景摘要过长（最多 {} 字）", SCENE_SUMMARY_MAX),
            "action": "error",
        }))
        .into_response();
    }
    let mut sess = match state.sessions_tavern.get(id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    sess.memory_l1.scene_summary = text.to_string();
    sess.memory_l1.updated_at_turn = sess.turn;
    if let Err(e) = state.sessions_tavern.save(sess) {
        return map_core_err(e);
    }
    Json(json!({
        "reply": "已生效：场景摘要已更新",
        "action": "remember",
    }))
    .into_response()
}

/// /event add <简述> / /event drop <id> —— 维护 L2 事件账本，保存后生效。
async fn handle_event_cmd(state: &AppState, id: &str, msg: &str) -> Response {
    let rest = msg.trim_start_matches("/event").trim();
    if rest == "add" || rest.starts_with("add ") {
        let brief = rest["add".len()..].trim();
        if brief.is_empty() {
            return Json(json!({
                "reply": "用法：/event add <事件简述>",
                "action": "error",
            }))
            .into_response();
        }
        if brief.chars().count() > EVENT_SUMMARY_MAX {
            return Json(json!({
                "reply": format!("事件简述过长（最多 {} 字）", EVENT_SUMMARY_MAX),
                "action": "error",
            }))
            .into_response();
        }
        let mut sess = match state.sessions_tavern.get(id) {
            Ok(s) => s,
            Err(e) => return map_core_err(e),
        };
        sess.memory_l2.events.push(kaleido_core::MemoryL2Event {
            id: format!("ev-{}", Uuid::new_v4()),
            turn: sess.turn,
            kind: "event".into(),
            summary: brief.to_string(),
            actors: vec![],
            node_id: sess.node_id.clone(),
            embedding: vec![],
        });
        sess.memory_l2.updated_at_turn = sess.turn;
        if let Err(e) = state.sessions_tavern.save(sess) {
            return map_core_err(e);
        }
        Json(json!({
            "reply": "已生效：事件已记录",
            "action": "event_add",
        }))
        .into_response()
    } else if rest == "drop" || rest.starts_with("drop ") {
        let eid = rest["drop".len()..].trim();
        if eid.is_empty() {
            return Json(json!({
                "reply": "用法：/event drop <事件id>",
                "action": "error",
            }))
            .into_response();
        }
        let mut sess = match state.sessions_tavern.get(id) {
            Ok(s) => s,
            Err(e) => return map_core_err(e),
        };
        let before = sess.memory_l2.events.len();
        sess.memory_l2.events.retain(|e| e.id != eid);
        if sess.memory_l2.events.len() == before {
            return Json(json!({
                "reply": format!("未找到事件：{eid}"),
                "action": "error",
            }))
            .into_response();
        }
        sess.memory_l2.updated_at_turn = sess.turn;
        if let Err(e) = state.sessions_tavern.save(sess) {
            return map_core_err(e);
        }
        Json(json!({
            "reply": "已生效：事件已删除",
            "action": "event_drop",
        }))
        .into_response()
    } else {
        Json(json!({
            "reply": "用法：/event add <简述> 或 /event drop <id>",
            "action": "error",
        }))
        .into_response()
    }
}

/// /time 指令 —— 显式推进游戏时钟（正序约束）。
/// 用法：/time 深夜 /time 次日清晨 /time 三天后
async fn handle_time_cmd(state: &AppState, id: &str, msg: &str) -> Response {
    let rest = msg.trim_start_matches("/time").trim();
    if rest.is_empty() {
        let s = match state.sessions_tavern.get(id) {
            Ok(s) => s,
            Err(e) => return map_core_err(e),
        };
        return Json(json!({
            "reply": format!("当前：{}（用 /time <时段|次日|N天后> 推进）", s.game_clock.state_line()),
            "action": "time_status",
            "gameClock": s.game_clock,
        }))
        .into_response();
    }
    let mut sess = match state.sessions_tavern.get(id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    match sess.game_clock.jump(rest, sess.turn) {
        Ok((_, desc)) => {
            if let Err(e) = state.sessions_tavern.save(sess.clone()) {
                return map_core_err(e);
            }
            Json(json!({
                "reply": format!("已推进 → {desc}"),
                "action": "time_jump",
                "gameClock": sess.game_clock,
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "reply": format!("{e}"),
            "action": "error",
        }))
        .into_response(),
    }
}

/// /weather 指令 —— 显式改天气。**用户指令第一原则**：直接设置（允许跳变），
/// 不再强制邻接渐进（此前晴→暴雨需逐级 5 次，用户指定被拒）。
/// 用法：/weather 小雨 /weather 暴雨
async fn handle_weather_cmd(state: &AppState, id: &str, msg: &str) -> Response {
    let rest = msg.trim_start_matches("/weather").trim();
    if rest.is_empty() {
        let s = match state.sessions_tavern.get(id) {
            Ok(s) => s,
            Err(e) => return map_core_err(e),
        };
        return Json(json!({
            "reply": format!("当前天气：{}（用 /weather <{}> 改，可直接设置任意天气）", s.game_clock.weather, "晴/多云/阴/小雨/大雨/暴雨/雨雪/雪/大雪/雾/大风"),
            "action": "weather_status",
            "gameClock": s.game_clock,
        }))
        .into_response();
    }
    let mut sess = match state.sessions_tavern.get(id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    match sess.game_clock.force_weather(rest) {
        Ok(w) => {
            if let Err(e) = state.sessions_tavern.save(sess.clone()) {
                return map_core_err(e);
            }
            Json(json!({
                "reply": format!("天气已改 → {w}"),
                "action": "weather_set",
                "gameClock": sess.game_clock,
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "reply": format!("{e}"),
            "action": "error",
        }))
        .into_response(),
    }
}

/// /season 指令 —— 显式强制季节（剧情设定/推进季节用）。
/// 用法：/season 冬 /season 夏天
async fn handle_season_cmd(state: &AppState, id: &str, msg: &str) -> Response {
    let rest = msg.trim_start_matches("/season").trim();
    if rest.is_empty() {
        let s = match state.sessions_tavern.get(id) {
            Ok(s) => s,
            Err(e) => return map_core_err(e),
        };
        return Json(json!({
            "reply": format!("当前季节：{}（用 /season <春|夏|秋|冬> 强制设置）", s.game_clock.season()),
            "action": "season_status",
            "gameClock": s.game_clock,
        }))
        .into_response();
    }
    let mut sess = match state.sessions_tavern.get(id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    match sess.game_clock.set_season(rest) {
        Ok(se) => {
            if let Err(e) = state.sessions_tavern.save(sess.clone()) {
                return map_core_err(e);
            }
            Json(json!({
                "reply": format!("季节已设为 → {se}（{}）", sess.game_clock.state_line()),
                "action": "season_set",
                "gameClock": sess.game_clock,
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "reply": format!("{e}"),
            "action": "error",
        }))
        .into_response(),
    }
}

async fn rewind_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RewindRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let steps = body.steps.unwrap_or(1).max(1);
    match do_rewind(&state, &id, steps).await {
        Ok((turn, rewound)) => {
            let remaining = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
                Ok(s) => s.checkpoints.len().saturating_sub(1),
                Err(_) => 0,
            };
            Json(json!({
                "ok": true,
                "rewound": rewound,
                "turn": turn,
                "remaining": remaining,
            }))
            .into_response()
        }
        Err(kaleido_core::CoreError::BadRequest(msg)) if msg.contains("turn in progress") => return conflict("ST_CONFLICT", msg),
        Err(e) => map_core_err(e),
    }
}

async fn reroll_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match do_reroll(&state, &id).await {
        Ok((turn, last_user)) => {
            Json(json!({
                "ok": true,
                "lastUserMessage": last_user,
                "turn": turn,
            }))
            .into_response()
        }
        Err(kaleido_core::CoreError::BadRequest(msg)) if msg.contains("turn in progress") => return conflict("ST_CONFLICT", msg),
        Err(e) => map_core_err(e),
    }
}

// ─── Message management (ST 核心交互：删除/编辑消息) ─────────────────────────

/// DELETE /api/v1/story-tavern/sessions/{id}/messages/{mid}
/// 删除单条消息。返回更新后的 session。
async fn delete_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, mid)): Path<(String, String)>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let result = (|| -> kaleido_core::CoreResult<TavernSession> {
        let mut s = state.sessions_tavern.get_for_owner(&id, &session.user_id)?;
        let before = s.messages.len();
        s.messages.retain(|m| m.id != mid);
        if s.messages.len() == before {
            return Err(kaleido_core::CoreError::NotFound(format!("message not found: {mid}")));
        }
        state.sessions_tavern.save(s.clone())?;
        Ok(s)
    })();
    match result {
        Ok(s) => Json(json!({ "ok": true, "session": s })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// PUT /api/v1/story-tavern/sessions/{id}/messages/{mid}
/// 编辑单条消息正文。body: { "content": "..." }。返回更新后的 session。
async fn edit_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, mid)): Path<(String, String)>,
    body: axum::Json<serde_json::Value>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let new_content = body
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if new_content.trim().is_empty() {
        return bad_request("ST_CONTENT_REQUIRED", "content required");
    }
    let result = (|| -> kaleido_core::CoreResult<TavernSession> {
        let mut s = state.sessions_tavern.get_for_owner(&id, &session.user_id)?;
        let mut found = false;
        for m in s.messages.iter_mut() {
            if m.id == mid {
                m.content = new_content.clone();
                found = true;
                break;
            }
        }
        if !found {
            return Err(kaleido_core::CoreError::NotFound(format!("message not found: {mid}")));
        }
        state.sessions_tavern.save(s.clone())?;
        Ok(s)
    })();
    match result {
        Ok(s) => Json(json!({ "ok": true, "session": s })).into_response(),
        Err(e) => map_core_err(e),
    }
}

// ─── Persona handlers ────────────────────────────────────────────────────────

async fn get_persona(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(character_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match state.personas.get(&character_id) {
        Ok(p) => Json(p).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn save_persona(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(character_id): Path<String>,
    Json(mut persona): Json<TavernPersona>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    if persona.character_id.is_empty() {
        persona.character_id = character_id;
    } else if persona.character_id != character_id {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "path characterId mismatch".into(),
        ));
    }
    match state.personas.save(persona) {
        Ok(p) => Json(p).into_response(),
        Err(e) => map_core_err(e),
    }
}

// ─── T2: 创作罗盘（author_intent + current_focus）──────────────────────────

async fn get_compass(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = CompassStore::new(state.auth.data_root().clone());
    match store.load(&work_id) {
        Ok(c) => Json(c).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn put_compass(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(work_id): Path<String>,
    Json(compass): Json<Compass>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let store = CompassStore::new(state.auth.data_root().clone());
    let saved = match store.save(&work_id, &compass) {
        Ok(c) => c,
        Err(e) => return map_core_err(e),
    };
    // 挂载到当前全部 TavernSession 的系统状态，令注入下回合即生效。
    // 单用户本地；未来若引入 session→work 归属，可只更新匹配 work 的会话。
    let mut sessions_mounted = 0usize;
    if let Ok(list) = state.sessions_tavern.list() {
        for row in list {
            let Some(sid) = row.get("sessionId").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Ok(mut sess) = state.sessions_tavern.get(sid) {
                sess.actor_states.mount_compass(saved.clone());
                if state.sessions_tavern.save(sess).is_ok() {
                    sessions_mounted += 1;
                }
            }
        }
    }
    Json(json!({
        "version": saved.version,
        "authorIntent": saved.author_intent,
        "currentFocus": saved.current_focus,
        "sessionsMounted": sessions_mounted,
    }))
    .into_response()
}

// ─── ST-1: Turn handlers ────────────────────────────────────────────────────


/// Filter lore entries matching current chapter/node context.
/// Entries need: permanent=true, or chapterRange contains chapter_cursor, or nodeIds includes node_id.
/// T 层 (2026-08-19): 解析章节 id 数值（"ch06" → 6）；解析失败返回 0（保守，不参与过滤）。
fn worldline_chapter_num(ch: &str) -> i64 {
    ch.trim()
        .strip_prefix("ch")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

/// T 层 (2026-08-19): 构建「原著时间线」注入块——从 pack.worldline（旁挂 worldline.json
/// 蒸馏产物）取 chapter <= 当前章的事件，按章升序格式化。只注入「已推进」的历史事件，
/// 防后期事件剧透/首回合劫持（D2 同源）；无章节标注（解析失败）保守不注入。旧 pack / 未蒸馏
/// worldline 为空 → 返回 None 零注入。
fn build_worldline_block(pack: &StoryPack, chapter_cursor: &str) -> Option<String> {
    if pack.worldline.is_empty() {
        return None;
    }
    let cur = worldline_chapter_num(chapter_cursor);
    if cur <= 0 {
        return None; // 无当前章进度时不注入（保守）
    }
    let mut shown: Vec<&Value> = Vec::new();
    for ev in &pack.worldline {
        let ch = ev.get("chapter").and_then(|v| v.as_str()).unwrap_or("");
        let chn = worldline_chapter_num(ch);
        if chn > 0 && chn <= cur {
            shown.push(ev);
        }
    }
    if shown.is_empty() {
        return None;
    }
    shown.sort_by_key(|ev| worldline_chapter_num(ev.get("chapter").and_then(|v| v.as_str()).unwrap_or("")));
    let mut out = String::from("\n## 原著时间线（已推进部分）");
    for ev in shown.iter().take(20) {
        let event = ev.get("event").and_then(|v| v.as_str()).unwrap_or("");
        if event.trim().is_empty() {
            continue;
        }
        let tp = ev.get("time_point").and_then(|v| v.as_str()).unwrap_or("");
        let ch = ev.get("chapter").and_then(|v| v.as_str()).unwrap_or("");
        let imp = ev.get("importance").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!(
            "\n- {ch}·{tp} [{}] {event}",
            if imp.is_empty() { "–" } else { imp }
        ));
    }
    out.push_str("\n以上为原著已发生剧情的时间线锚点，用于保持剧情勾稽；当前进度之后的情节尚未发生，不得提前使用。");
    if out.trim_end_matches(|c: char| c != '】' && c != '\n').ends_with("原著时间线（已推进部分）") {
        return None; // 无有效事件行（全空摘要）
    }
    Some(out)
}

fn filter_lore_entries<'a>(entries: &'a [Value], chapter_cursor: &str, node_id: &str) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for entry in entries {
        let perm = entry.get("permanent").and_then(|v| v.as_bool()).unwrap_or(false);
        if perm {
            out.push(entry);
            continue;
        }
        let range = entry.get("chapterRange").and_then(|v| v.as_str()).unwrap_or("");
        let node_ids = entry.get("nodeIds").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()).unwrap_or_default();
        let mut matched = false;
        if !range.is_empty() {
            // "ch01-ch02" style range check
            let ch_label = chapter_cursor.trim().to_lowercase();
            let parts: Vec<&str> = range.splitn(2, '-').map(|s| s.trim()).collect();
            if parts.len() == 1 {
                matched = ch_label == parts[0].to_lowercase();
            } else if parts.len() >= 2 {
                matched = ch_label >= parts[0].to_lowercase() && ch_label <= parts[1].to_lowercase();
            }
        }
        if !matched && !node_ids.is_empty() {
            matched = node_ids.iter().any(|nid| *nid == node_id);
        }
        if matched {
            out.push(entry);
        }
    }
    out
}

/// P0 闭环: 预载伏笔注入块（调用点调用，保持 build_tavern_system_prompt 纯函数）。
/// 取 weight 前 15 条 active/planted 伏笔，总长 ≤1200 字符，每条 ≤120 字符。
/// 已回收（recalled）的不注入 —— 回收后下一轮自动从 prompt 消失（可感知闭环）。
fn preload_foreshadow_block(state: &AppState, pack: &StoryPack, session: &TavernSession) -> Option<String> {
    use kaleido_core::foreshadow_store::ForeshadowStore;
    // work_id 解析: pack.source.refs 中 36 字符 uuid 形态的条目即 work 关联
    // （pack 由作者区 work 生成时写入），否则用 pack_id 兜底（fail-open，查不到即 None）。
    let work_id: String = pack
        .source
        .refs
        .iter()
        .find(|r| r.len() == 36 && r.chars().filter(|c| *c == '-').count() == 4)
        .cloned()
        .unwrap_or_else(|| session.pack_id.clone());
    let fs = ForeshadowStore::open(&state.auth.data_root().root().join("plot.sqlite")).ok()?;
    let mut fores: Vec<kaleido_core::foreshadow_store::Foreshadow> = Vec::new();
    for status in ["planted", "active"] {
        if let Ok(mut v) = fs.list_foreshadows(&work_id, Some(status), None) {
            fores.append(&mut v);
        }
    }
    if fores.is_empty() {
        return None;
    }
    fores.sort_by(|a, b| b.weight.cmp(&a.weight));
    let mut lines: Vec<String> = Vec::new();
    let mut budget = 0usize;
    for f in fores.iter().take(15) {
        let desc: String = f.description.chars().take(120).collect();
        let line = format!("- [{}]: {}", f.title, desc);
        budget += line.chars().count();
        if budget > 1200 {
            break;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "## 伏笔（作者预埋，未到揭示时机请勿提前揭晓；回收后自然消失）\n{}",
        lines.join("\n")
    ))
}

/// 吞噬资产接线（P0）: 账本/情感曲线/角色弧/关系演化 → 注入块。
/// 调用点预载（对齐 preload_foreshadow_block），保持 build_tavern_system_prompt 纯函数。
/// 全部启发式零 LLM（词表/文件/graph），fail-open：查不到即空，零开销。
fn preload_asset_blocks(
    state: &AppState,
    pack: &StoryPack,
    session: &TavernSession,
    chapter_body: &str,
) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    let data_root = state.auth.data_root().root();

    // 1. ledger —— Liyuan 记忆账本快照（≤1200 字符）
    {
        let store = kaleido_core::ledger::LedgerStore::load(&data_root);
        if let Ok(snap) = store.snapshot() {
            let snap: String = snap.chars().take(1200).collect();
            if !snap.trim().is_empty() {
                blocks.push(format!("## 记忆账本（Liyuan 吞噬）\n{}", snap));
            }
        }
    }

    // 2. emotion —— 当前章节情感快照（novel2hermes T5，启发式词表）
    if !chapter_body.trim().is_empty() {
        let curve = kaleido_core::emotion_curve::build_emotion_curve(&[kaleido_core::emotion_curve::ChapterText {
            chapter: session
                .chapter_cursor
                .clone()
                .unwrap_or_else(|| "当前章节".into()),
            text: chapter_body.to_string(),
        }]);
        if let Some(ch) = curve.chapters.first() {
            blocks.push(format!(
                "## 情感快照（当前章节）\n峰值强度 {}，主导情绪 {}，曲线形态 {}。整体弧线：{}",
                ch.peak_intensity, ch.dominant_emotion, ch.curve_shape, curve.overall_arc
            ));
        }
    }

    // 3. arc + relation —— graph 派生（ai-novel T4 + novel2hermes T5），聚焦玩家 vessel 相关
    let work_id: String = pack
        .source
        .refs
        .iter()
        .find(|r| r.len() == 36 && r.chars().filter(|c| *c == '-').count() == 4)
        .cloned()
        .unwrap_or_else(|| session.pack_id.clone());
    if let Ok((_chars, rels)) = state.graph.list(&work_id) {
        if !rels.is_empty() {
            // 玩家 vessel 名（用于聚焦）
            let vessel_name = session
                .player
                .control_character_id
                .as_ref()
                .or(session.entry.vessel_character_id.as_ref())
                .and_then(|vid| pack.characters.iter().find(|c| c.id == *vid))
                .map(|c| c.name.as_str())
                .unwrap_or("");

            // 3a. 关系演化（T4）—— 最近章节关系
            let evos = crate::relation_evolution::RelationEvolution::build_evolution(&rels);
            let mut rel_lines: Vec<String> = Vec::new();
            let mut budget = 0usize;
            for e in evos.iter().take(8) {
                if !vessel_name.is_empty() && e.pair.0 != vessel_name && e.pair.1 != vessel_name {
                    continue;
                }
                if let Some(last) = e.chapters.last() {
                    let line = format!(
                        "- {} ↔ {}：{}（最近 {}）",
                        e.pair.0, e.pair.1, e.trend, last.chapter
                    );
                    budget += line.chars().count();
                    if budget > 800 {
                        break;
                    }
                    rel_lines.push(line);
                }
            }
            if !rel_lines.is_empty() {
                blocks.push(format!("## 关系演化（角色图谱）\n{}", rel_lines.join("\n")));
            }

            // 3b. 角色弧（T5）—— 跨章变化
            let mut entries = Vec::new();
            for rel in &rels {
                for w in rel.chapters.windows(2) {
                    entries.push(kaleido_core::character_arc::ArcEntry {
                        character: rel.from_char.clone(),
                        chapter: w[1].clone(),
                        field: format!("与{}的关系({})", rel.to_char, rel.category),
                        from: w[0].clone(),
                        to: w[1].clone(),
                    });
                }
            }
            let arcs = kaleido_core::character_arc::build_character_arcs(&entries);
            let mut arc_lines: Vec<String> = Vec::new();
            let mut budget = 0usize;
            for a in arcs.iter().take(5) {
                if !vessel_name.is_empty() && a.character != vessel_name {
                    continue;
                }
                for c in a.changes.iter().rev().take(2) {
                    let line = format!(
                        "- {}：{} {}→{}（{}）",
                        a.character, c.field, c.from, c.to, c.chapter
                    );
                    budget += line.chars().count();
                    if budget > 800 {
                        break;
                    }
                    arc_lines.push(line);
                }
            }
            if !arc_lines.is_empty() {
                blocks.push(format!("## 角色弧（跨章变化）\n{}", arc_lines.join("\n")));
            }
        }
    }

    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n\n"))
    }
}

/// 账本写入链路（P0 补全）: 回合完成后启发式提取关键叙事信息写入 Liyuan 账本
/// （ledger.json），供下回合 preload_asset_blocks 注入。
///
/// 设计约束：
/// - 零 LLM：纯词表 + 正则启发式（与 preload_asset_blocks 一致），fail-open 不阻塞回合。
/// - 只提取高置信度事实：场景时段（场景标签）、角色状态（角色名 + 状态词表）、
///   伏笔（未解关键词）、物品（「」引号内名词）、好感（角色对 + 情绪词表）。
/// - 每类限流（max N 条/回合），避免账本膨胀。
/// - upsert 失败仅 warn，不打断回合主流程。

/// UTF-8 安全截窗：从 byte 偏移向左右收缩到 char 边界。
/// 用于在 CJK 正文中围绕关键词截取上下文窗口（saturating_sub 可能切进多字节字符）。
fn utf8_window(text: &str, mid: usize, before: usize, after: usize) -> &str {
    let bytes = text.as_bytes();
    let mut start = mid.saturating_sub(before);
    while start > 0 && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start -= 1;
    }
    let mut end = (mid + after).min(bytes.len());
    while end < bytes.len() && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end += 1;
    }
    &text[start..end]
}

fn ledger_upsert_from_turn(
    state: &AppState,
    pack: &StoryPack,
    session: &TavernSession,
    story_body: &str,
    _user_text: &str,
) {
    let data_root = state.auth.data_root().root();
    let store = LedgerStore::load(&data_root);
    let turn = session.turn;

    let char_names: Vec<&str> = pack.characters.iter().map(|c| c.name.as_str()).collect();
    for (kind, key, value) in ledger_extract_entries(story_body, &char_names) {
        if let Err(e) = store.upsert(kind, key, value) {
            tracing::warn!(error = %e, turn, "st ledger upsert failed");
        }
    }
}

/// 启发式提取账本条目（纯函数，无 IO，可单测）。
/// 返回 `(kind, key, value)` 列表，每类限流避免膨胀。
fn ledger_extract_entries(story_body: &str, char_names: &[&str]) -> Vec<(LedgerKind, String, Value)> {
    let mut out: Vec<(LedgerKind, String, Value)> = Vec::new();

    let char_name = |body: &str| -> Option<&str> {
        // 按角色表顺序返回第一个在正文中出现的角色名
        char_names.iter().copied().find(|n| !n.is_empty() && body.contains(n))
    };

    // ---- 1. 时间（Time）: 场景标签时段 ----
    // 标签形如 `<场景：...｜午后>` / `<场景：...｜深夜>` / `<时间：午后>`
    const TIME_OF_DAY: &[&str] = &[
        "清晨", "早晨", "上午", "正午", "中午", "午后", "下午", "傍晚", "黄昏", "夜晚", "夜里",
        "深夜", "凌晨", "午夜", "黎明",
    ];
    let mut time_of_day: Option<&str> = None;
    for t in TIME_OF_DAY {
        // 场景标签内（<...> 之间）优先
        if let Some(lt) = story_body.find('<') {
            if let Some(gt) = story_body[lt..].find('>') {
                let tag = &story_body[lt..lt + gt];
                if tag.contains(t) {
                    time_of_day = Some(t);
                    break;
                }
            }
        }
        if time_of_day.is_none() && story_body.contains(t) {
            time_of_day = Some(t);
        }
    }
    if let Some(tod) = time_of_day {
        // 场景名从标签 `<场景：XXX｜时段｜角色>` 中解析, 不是取正文前 60 字符
        let scene_name = (|| {
            let lt = story_body.find('<')?;
            let gt = story_body[lt..].find('>')?;
            let tag = &story_body[lt + 1..lt + gt];
            let rest = tag.strip_prefix("场景：")?;
            Some(rest.split('｜').next()?.trim().to_string())
        })()
        .filter(|s| !s.is_empty());
        out.push((
            LedgerKind::Time,
            "当前场景".into(),
            json!({
                "timeOfDay": tod,
                "scene": scene_name.unwrap_or_else(|| "未知场景".into()),
            }),
        ));
    }

    // ---- 2. 状态（Status）: 角色名 + 状态词表 ----
    const STATUS_WORDS: &[&str] = &[
        "怀孕", "孕吐", "发烧", "受伤", "昏迷", "醉酒", "病倒", "疲惫", "流血", "虚弱",
        "失眠", "难受", "不安", "发抖", "落泪",
    ];
    let mut status_written = 0usize;
    for w in STATUS_WORDS {
        if status_written >= 2 {
            break;
        }
        if let Some(idx) = story_body.find(w) {
            if let Some(c) = char_name(utf8_window(story_body, idx, 60, w.len() + 10)) {
                out.push((
                    LedgerKind::Status,
                    c.to_string(),
                    json!({"state": w, "detail": utf8_window(story_body, idx, 0, w.len() + 20).to_string()}),
                ));
                status_written += 1;
            }
        }
    }

    // ---- 3. 伏笔（Foreshadow）: 未解关键词 ----
    const HOOK_WORDS: &[&str] = &["还没", "尚未", "究竟", "到底", "秘密", "谜", "蹊跷", "疑点", "未解"];
    let mut hook_written = 0usize;
    for w in HOOK_WORDS {
        if hook_written >= 2 {
            break;
        }
        if let Some(idx) = story_body.find(w) {
            let ctx: String = utf8_window(story_body, idx, 20, w.len() + 30).chars().collect();
            out.push((
                LedgerKind::Foreshadow,
                w.to_string(),
                json!({"promise": ctx, "status": "open"}),
            ));
            hook_written += 1;
        }
    }

    // ---- 4. 物品（Item）: 「」引号内名词（2-8 字符，非对话）----
    static ITEM_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // 字面量正则不可能编译失败：OnceLock 静态化同时消除每次调用的重编译开销。
    let item_re =
        ITEM_RE.get_or_init(|| regex::Regex::new(r"「([^「」]{2,8})」").expect("literal regex"));
    let mut item_written = 0usize;
    for cap in item_re.captures_iter(story_body) {
        if item_written >= 3 {
            break;
        }
        let candidate = cap[1].trim();
        if candidate.is_empty() {
            continue;
        }
        // 排除对话/语气/常见虚词（含 CJK 标点——「老板，来壶热茶。」这类对话句
        // 不含 ASCII 标点但含中文逗号/句号，必须一并排除）
        if candidate.chars().any(|c| {
            c.is_ascii_punctuation()
                || matches!(
                    c,
                    '。' | '，' | '、' | '！' | '？' | '；' | '：' | '…' | '～' | '「' | '」' | '『' | '』' | '“' | '”'
                )
        }) || candidate.contains("说")
            || candidate.contains("道")
            || candidate.contains("吗")
            || candidate.contains("呢")
            || candidate.contains("啊")
            || candidate.contains("吧")
        {
            continue;
        }
        out.push((
            LedgerKind::Item,
            candidate.to_string(),
            json!({"state": "carried", "source": "turn"}),
        ));
        item_written += 1;
    }

    // ---- 5. 好感（Affinity）: 角色对 + 情绪词表 ----
    const AFFINITY_WORDS: &[&str] = &[
        "温柔", "依恋", "抗拒", "冷漠", "疏远", "亲近", "疼惜", "心疼", "厌恶", "感激", "愧疚",
        "嗔怪", "宠溺",
    ];
    let mut aff_written = 0usize;
    for w in AFFINITY_WORDS {
        if aff_written >= 2 {
            break;
        }
        if let Some(idx) = story_body.find(w) {
            let window = utf8_window(story_body, idx, 40, w.len() + 20);
            let a = char_name(window);
            // 找第二个角色：从窗口内再找一个不同角色
            let b = char_names.iter().copied().find(|n| {
                !n.is_empty() && *n != a.unwrap_or("") && window.contains(n)
            });
            if let (Some(an), Some(bn)) = (a, b) {
                out.push((
                    LedgerKind::Affinity,
                    format!("{}↔{}", an, bn),
                    json!({"sentiment": w, "trend": "stable", "detail": window.to_string()}),
                ));
                aff_written += 1;
            }
        }
    }

    out
}

/// X4 (吞噬自 xiami outline.rs): 从导演计划派生章节执行合同文本（system prompt 注入 + director-config 诊断展示共用）。
/// 简单方案：required_events = plan.hits_beats，summary = plan.goal（可含 X2c 推演的 ending_state/next_impetus），
/// source_plan_id = "director"。**不阻断**：无 director_plan 或无数据时返回 None，调用方跳过。
fn build_director_execution_contract(session: &TavernSession) -> Option<String> {
    let plan = session.director_plan.as_ref()?;
    if plan.hits_beats.is_empty() && plan.goal.trim().is_empty() {
        return None;
    }
    let chapter_number = session
        .chapter_cursor
        .as_deref()
        .and_then(|cursor| {
            cursor
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .ok()
        })
        .unwrap_or(0);
    let brief = ChapterBriefView {
        chapter_number,
        key_events: plan.hits_beats.clone(),
        summary: plan.goal.clone(),
        ..Default::default()
    };
    let contract = build_chapter_execution_contract(&brief, "director", 0);
    let rendered = render_execution_contract(&contract);
    if rendered.trim().is_empty() {
        None
    } else {
        Some(rendered)
    }
}

/// Build a story context prompt from pack context + session state.
fn build_tavern_system_prompt(
    pack: &StoryPack,
    session: &TavernSession,
    chapter_body: &str,
    cross_dir: &std::path::Path,
    query_embedding: Option<Vec<f32>>,
    mcp_tools: &[crate::tavern_mcp::McpToolEntry],
    foreshadow_block: Option<&str>,
) -> String {
    let mut lines = Vec::new();

    // Story context
    lines.push("你是一个故事主持人（DM/GM）。你按照下面的剧本包设定进行沉浸式叙事。".into());
    lines.push(format!("## 剧本：{}", pack.title));
    lines.push(format!("当前章节：{}", {
        session
            .chapter_cursor
            .as_deref()
            .unwrap_or("(未知)")
    }));
    // [morphling Wave B3 2026-08-16] 章节剧情摘要账本注入（吸收自 BakemonoMemory
    // summary-memory-model）：当前章「本章剧情进展」+ 上一章回顾，治理长跑失忆。
    if !session.chapter_diaries.is_empty() {
        let cur = session.chapter_cursor.as_deref().unwrap_or("");
        if let Some(d) = session.chapter_diaries.iter().find(|d| d.chapter_id == cur) {
            if !d.summary.trim().is_empty() {
                lines.push(format!("\n## 本章剧情进展\n{}", d.summary));
            }
        }
        let prev = session
            .chapter_diaries
            .iter()
            .filter(|d| d.chapter_id != cur && !d.summary.trim().is_empty())
            .last();
        if let Some(d) = prev {
            lines.push(format!(
                "\n## 上一章回顾（{}）\n{}",
                if d.title.is_empty() { d.chapter_id.as_str() } else { d.title.as_str() },
                d.summary
            ));
        }
    }
    lines.push(format!("当前节点：{}", {
        session
            .node_id
            .as_deref()
            .unwrap_or("(未知)")
    }));
    lines.push(format!("当前模式：{}", session.play_mode.as_str()));
    if let Some(vid) = session
        .player
        .control_character_id
        .as_ref()
        .or(session.entry.vessel_character_id.as_ref())
    {
        let vname = pack
            .characters
            .iter()
            .find(|c| c.id == *vid)
            .map(|c| c.name.as_str())
            .unwrap_or(vid.as_str());
        lines.push(format!("玩家当前 vessel/身子：**{}**（{}）。请按此身份叙事，勿让玩家同时操控其他 NPC 身子。", vname, vid));
    }
    // ST-30 (2026-08-15 根治): 注入本作角色名单 + 回合末尾角色清单输出要求。
    // 守卫优先用清单做精确集合比对（替代「说话标记前切词」启发式——根治叙述形态
    // 漏报与切词碎短语误报）。清单缺失时守卫自动降级启发式，安全兜底。
    let roster_names: Vec<&str> = pack.characters.iter().map(|c| c.name.as_str()).collect();
    if !roster_names.is_empty() {
        lines.push(format!(
            "## 本作角色名单（严禁引入名单外人物作为登场角色）\n{}",
            roster_names.join("、")
        ));
        lines.push(
            "回合正文末尾必须输出 <角色清单>本回合实际出场的人名（顿号分隔）</角色清单>；没有新出场角色也输出 <角色清单></角色清单>。"
                .into(),
        );
    }
    match session.play_mode {
        kaleido_core::PlayMode::Free => {
            lines.push("自由模式：不要强行推进主线节点，允许玩家岔开闲聊；保持人设与世界一致。".into());
        }
        kaleido_core::PlayMode::Side => {
            lines.push("支线/番外模式：主线章节游标冻结；可展开支线情节，不要宣称主线节点已推进。".into());
            if let Some(label) = session.side_branch_label.as_deref() {
                lines.push(format!("当前支线：「{}」。围绕此节点/主题展开番外，可偏离主线。", label));
            }
            if let Some(sid) = session.side_branch_node_id.as_deref() {
                lines.push(format!("支线入口节点 sideBranchNodeId={}。", sid));
            }
            if let Some(rid) = session.resume_node_id.as_deref() {
                lines.push(format!("回主线锚点 resumeNodeId={}（玩家结束支线后将回到此处）。", rid));
            }
        }
        kaleido_core::PlayMode::Mainline => {
            lines.push("主线模式：优先服务节点目标与 exits，可在合理处推进剧情。".into());
            // Add available exits for LLM-directed auto-advance
            if let Some(nid) = &session.node_id {
                if let Some(node) = pack.nodes.iter().find(|n| n.id == *nid) {
                    // ST-23: allowedDivergence — 节点偏离档位（strict 严格按章节；branch 允许适度分支）
                    match node.allowed_divergence.as_str() {
                        "strict" => {
                            lines.push("本节点为严格模式（strict）：你只能叙述当前章节原著中发生的情节，禁止编造原书未出现的场景、人物、能力、地点、超自然/玄幻/灵异设定或情节走向，禁止跳章到后续情节。玩家提出偏离原著的要求（如提前发生关键事件、瞬移到其他城市、改变角色命运）时，必须拒绝或委婉拉回当前章节，绝不可跟随；只能在当前章节框架内给予有限互动。".into());
                        }
                        _ => {
                            lines.push("本节点允许适度分支（branch）：可在当前章节框架内展开合理支线，但不得引入原书不存在的人物、地点、能力或情节走向，禁止跳章。玩家要求偏离原著时，须委婉拒绝并拉回当前章节；分支结束后必须回归本章主线。".into());
                        }
                    }
                    if !node.exit.is_empty() {
                        lines.push("可用剧情出口：".into());
                        for (i, exit) in node.exit.iter().enumerate() {
                            let next_title = pack.nodes.iter()
                                .find(|n| n.id == exit.next)
                                .map(|n| n.title.as_str())
                                .unwrap_or(&exit.next);
                            lines.push(format!("  {}: {} → {}（{}）", i + 1, exit.when, exit.next, next_title));
                        }
                        lines.push("若故事已推进到合适的出口，在叙事末尾添加【节点推进:节点ID】标记（例如【节点推进:n2】）。勿过分频繁推进，让玩家有沉浸段落空间。".into());
                    }
                    // ST-24: 注入当前节点剧情概要（node.summary）与硬节拍（locked_beats）
                    if !node.summary.trim().is_empty() {
                        lines.push(format!("本章剧情概要（必须严格遵循，不得偏离或跳章）：{}", node.summary.trim()));
                    }
                    if !node.locked_beats.is_empty() {
                        // ST-25: locked_beats 是"本章稍后将发生"的未来剧情计划，不是"已发生事实"。
                        // 禁止 LLM 把它们当作既成事实提前回述/引用（否则开场卡+节拍注入会让"未来事件"变成"昨晚已发生"）。
                        lines.push("本章后续将发生的关键情节（locked beats，尚未发生：仅在玩家实际推进到该情节时按序呈现，禁止提前回述、禁止当作已发生事件引用，回述『过去』只能依据开场卡与对话历史中真实发生的内容）：".into());
                        for (i, b) in node.locked_beats.iter().enumerate() {
                            lines.push(format!("  {}. {}", i + 1, b));
                        }
                    }
                }
            }
        }
    }
    // ST-22: 改写强度约束（rewriteIntensity 由玩家创建会话时选择，必须真正传递给 LLM）
    match session.entry.rewrite_intensity {
        kaleido_core::RewriteIntensity::Canon => {
            lines.push("改写强度：严格遵循原著。你只能叙述当前节点对应章节内发生的情节，并严格按本章剧情概要（如有）推进；禁止添加原书不存在的人物、能力、超自然/玄幻/灵异设定、地点或情节走向，禁止跳章到后续情节。玩家提出偏离原著的要求（如提前登船、瞬移、改变关键事件）时，必须拒绝或委婉拉回当前章节，绝不可跟随。".into());
        }
        kaleido_core::RewriteIntensity::Rewrite => {
            lines.push("改写强度：允许大改。可自由调整剧情走向与世界观设定，但需保持角色基本人设与故事风格连贯。".into());
        }
    }
    // P0-2: config_write —— 助手配置注入（strict_mode_boost / pacing / style_guidance）
    if let Some(cfg) = session.player.flags.as_object() {
        if let Some(v) = cfg.get("strict_mode_boost").and_then(|v| v.as_f64()) {
            if v < 0.34 {
                lines.push("助手配置 strict_mode_boost：请尽量遵循 locked_beats 与章节大纲，避免偏离主线。".into());
            } else if v < 0.67 {
                lines.push("助手配置 strict_mode_boost：必须严格遵守 locked_beats 与章节大纲，禁止编造、跳章或偏离主线。".into());
            } else {
                lines.push("助手配置 strict_mode_boost（最高）：严禁任何偏离 locked_beats / 章节大纲的内容；玩家要求偏离主线时坚决拒绝并拉回当前章节。".into());
            }
        }
        if let Some(v) = cfg.get("pacing").and_then(|v| v.as_f64()) {
            if v < 0.34 {
                lines.push("助手配置 pacing：节奏放缓——多描写环境细节、氛围与角色反应，给玩家沉浸空间。".into());
            } else if v < 0.67 {
                lines.push("助手配置 pacing：保持当前叙事节奏，平稳推进。".into());
            } else {
                lines.push("助手配置 pacing：节奏加快——减少铺垫，迅速推进剧情与事件。".into());
            }
        }
        if let Some(sg) = cfg.get("style_guidance").and_then(|v| v.as_str()) {
            if !sg.trim().is_empty() {
                // P2-1: user-provided style guidance is wrapped in a
                // user-config fence so the model cannot be re-prompted by it.
                lines.push(kaleido_core::prompt_safety::wrap_user_block(
                    "user-config",
                    "config.style_guidance",
                    sg.trim(),
                ));
                // [P7R3 2026-08-16] 风格提示词平衡：限知视角"身体反应推断"常把角色情绪
                // 全外化为气音/颤抖/半截话，压制原声线台词——追加保留台词声线的平衡指令。
                lines.push("风格要求注意：上述风格适用于叙述与氛围；**角色台词必须保留其原声线**（参考角色卡示例对白），身体反应描写之外须有符合角色性格的直接对白，不得因限知视角把所有情绪都压成气音、颤抖或咽回半截话。".into());
            }
        }
        // [fix 2026-08-15 叙述视角] /config pov=first|third —— 用户指定人称，覆盖蒸馏文风。
        if let Some(pv) = cfg.get("pov").and_then(|v| v.as_str()) {
            match pv {
                "first" => lines.push("叙述人称（用户指定）：本回合正文一律使用第一人称「我/我们」叙述视角；禁止第三人称旁白（他/她）。".into()),
                "third" => lines.push("叙述人称（用户指定）：本回合正文一律使用第三人称「他/她」叙述视角；禁止第一人称「我」作叙述主体。".into()),
                _ => {}
            }
        }
    }
    // [fix 2026-08-15 文风注入] narrative_style 此前只蒸馏不注入（空转字段）。
    // 本 pack 蒸馏出的 narrative_style → 注入「## 文风要求」。
    // 跨 pack 外挂（宿醉外挂智取 18+ 文风）由调用方 start_turn 解析外部 pack 后
    // 经 style_guidance 通道注入（见下方 P0-2 config_write 段），本函数保持纯函数。
    let ns_block = pack
        .stage_director
        .resolved_snapshot
        .as_ref()
        .and_then(|snap| snap.narrative_style.as_ref());
    if let Some(ns) = ns_block {
        let mut style_lines: Vec<String> = Vec::new();
        if let Some(s) = ns.get("style").and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                style_lines.push(format!("叙事视角/人称：{}", s.trim()));
            }
        }
        if let Some(t) = ns.get("tone").and_then(|v| v.as_str()) {
            if !t.trim().is_empty() {
                style_lines.push(format!("基调：{}", t.trim()));
            }
        }
        if let Some(p) = ns.get("prose_guidance").and_then(|v| v.as_str()) {
            if !p.trim().is_empty() {
                style_lines.push(format!("行文要求：{}", p.trim()));
            }
        }
        if !style_lines.is_empty() {
            lines.push("\n## 文风要求".into());
            lines.extend(style_lines);
        }
    }
    // S4 (吞噬 denova director_plan): 导演计划注入 + 红线下沉 —— 导演计划是意图，不是自由改写
    if let Some(plan) = &session.director_plan {
        lines.push("\n## 导演计划（当前叙事意图）".into());
        lines.push(format!("导演目标：{}", plan.goal));
        if let Some(p) = &plan.pressure {
            if !p.trim().is_empty() {
                lines.push(format!("当前压力：{}", p));
            }
        }
        if let Some(c) = &plan.cost {
            if !c.trim().is_empty() {
                lines.push(format!("达成代价：{}", c));
            }
        }
        if !plan.hits_beats.is_empty() {
            lines.push("命中/借用的硬节拍（hits_beats）：".into());
            for (i, b) in plan.hits_beats.iter().enumerate() {
                lines.push(format!("  {}. {}", i + 1, b));
            }
        }
        lines.push("红线约束：导演计划仅为叙事意图，绝不改写 locked_beats 已锁定的硬事实；hits_beats 列出计划命中/借用的硬节拍。".into());
    }
    // X2b (吞噬自 xiami emotional_hooks.rs): 情绪钩子执行合同注入 —— 导演计划块之后追加。
    // 开关：默认启用，但轻量会话（无 guard_events 且无 director_plan 的纯聊天）跳过，避免干扰。
    if !session.guard_events.is_empty() || session.director_plan.is_some() {
        let hook_config = EmotionalHookConfig::default();
        let hook_contract = render_hook_execution_contract(&hook_config);
        if !hook_contract.trim().is_empty() {
            lines.push("\n".to_string());
            lines.push(hook_contract);
        }
        // 重复结尾信号警告：用最近 N 条 assistant 正文的结尾片段构造样本（简单截取即可）。
        if hook_config.detect_repetition {
            let recent_samples = build_hook_recent_samples(session);
            if let Some(signal) = repeated_recent_hook_signal(&recent_samples) {
                lines.push(format!("最近章节反复使用“{signal}”类牵引，后续避免照搬。"));
            }
        }
    }
    // X4 (吞噬自 xiami outline.rs): 章节执行合同注入 —— 导演计划块与情绪钩子合同之后追加。
    // **不阻断**：无 director_plan 或无法构造数据时跳过，不改变回合语义。
    if let Some(contract_text) = build_director_execution_contract(session) {
        lines.push("\n".to_string());
        lines.push(contract_text);
    }
    // Memory context (ST-21: embedding RAG — query_embedding is pre-computed in start_turn)
    let mem_ctx = kaleido_core::build_memory_context(session, 6, 4, 4, query_embedding.as_deref());
    if !mem_ctx.is_empty() {
        lines.push("\n".to_string());
        lines.push(mem_ctx);
    }

    // L4 Affinity / Secrets / Promises (ST-18)
    let l4 = &session.memory_l4;
    // §14.7.2C (2026-08-18): 接吻里程碑并入本块（原独立「关系状态」块删除）——
    // ST-FIX: 弱信号 contains("亲")/("床") 会把「母亲」的「亲」字误计为接吻，改用强信号。
    let kiss_count = session.memory_l2.events.iter().filter(|e| {
        e.kind == "romance" ||
        e.summary.to_lowercase().contains("接吻") ||
        e.summary.to_lowercase().contains("亲嘴") ||
        e.summary.to_lowercase().contains("亲吻") ||
        e.summary.to_lowercase().contains("亲热") ||
        e.summary.to_lowercase().contains("吻")
    }).count();
    let has_affinity = l4.affinity.is_object() && l4.affinity.as_object().map_or(false, |m| !m.is_empty());
    let has_secrets = !l4.secrets_known.is_empty();
    let has_promises = !l4.promises.is_empty();
    if has_affinity || has_secrets || has_promises || kiss_count > 0 {
        lines.push("\n## 情感与关系".into());
        // Build charId → name lookup
        let char_name = |cid: &str| -> String {
            pack.characters.iter().find(|c| c.id == cid).map(|c| c.name.as_str()).unwrap_or(cid).to_string()
        };
        if has_affinity {
            lines.push("好感度（0~100）：".into());
            if let Some(aff_map) = l4.affinity.as_object() {
                for (key, val) in aff_map.iter() {
                    let val_num = val.as_i64().unwrap_or(0);
                    // key can be "charId" or "charId:targetId"
                    let parts: Vec<&str> = key.split(':').collect();
                    let from_name = char_name(parts[0]);
                    if parts.len() > 1 && !parts[1].is_empty() {
                        let to_name = char_name(parts[1]);
                        lines.push(format!("  • {} → {}: {}", from_name, to_name, val_num));
                    } else {
                        lines.push(format!("  • {}: {}", from_name, val_num));
                    }
                }
            }
        }
        if has_secrets {
            lines.push("已知秘密：".into());
            for s in &l4.secrets_known {
                lines.push(format!("  • {}", s));
            }
        }
        if has_promises {
            lines.push("承诺/誓言：".into());
            for p in &l4.promises {
                lines.push(format!("  • {}", p));
            }
        }
        // §14.7.2C: 接吻里程碑附注（原独立块并入，省 ~8 行 + 情感状态归一）
        if kiss_count > 0 {
            lines.push(format!(
                "亲密里程碑：你们已接过吻（至少 {kiss_count} 次），关系已确立；后续亲热描写要体现熟悉感，不要再当第一次。"
            ));
        }
    }

    // S5 (吞噬 denova event_package): 本回合事件卡注入（ST-27 演出提示）
    if let Some(ev) = &session.last_event {
        lines.push("\n## 本回合事件卡".into());
        // G7: 事件名带 typeName/category/intensity 展示，如「事件：外门考核打脸（打脸 · medium）」；
        // 旧数据无新字段则退化为纯标题。
        let mut ev_label = if ev.type_name.trim().is_empty() {
            ev.title.clone()
        } else {
            ev.type_name.clone()
        };
        let mut ev_meta: Vec<&str> = Vec::new();
        if !ev.category.trim().is_empty() {
            ev_meta.push(ev.category.trim());
        }
        if !ev.intensity.trim().is_empty() {
            ev_meta.push(ev.intensity.trim());
        }
        if !ev_meta.is_empty() {
            ev_label = format!("{}（{}）", ev_label, ev_meta.join(" · "));
        }
        lines.push(format!("事件：{}", ev_label));
        if !ev.prompt.trim().is_empty() {
            // P2-1: event.prompt is user-authored演出提示 — wrap as event-block.
            lines.push(kaleido_core::prompt_safety::wrap_user_block(
                "event-block",
                &format!("event/{}/{}", ev.turn, ev.title),
                ev.prompt.trim(),
            ));
        }
        lines.push("本回合叙事必须自然融入该事件（可作背景氛围、人物动向或玩家遭遇的引子）；若回合内未正面呈现，可在后续回合继续铺垫。不要直接复读本指令文本。".into());
        // [P1C 2026-08-16] 事件卡为背景/氛围素材（描述过往或潜在场景），不代表角色当前状态；
        // 防 LLM 依据旧事件卡推断角色当前穿着/身体状态（宿醉「脱了又穿上」根因环之一）。
        lines.push("注意：事件卡是背景/氛围素材，描述的是过往或潜在场景，**不代表角色当前状态**；不得据此推断角色当前穿着/身体状态（以【角色状态】与最近剧情为准）。".into());
    }

    // ST-26: Actor 状态机（吞噬 denova actor_state）——渲染当前角色状态
    let actor_ctx = session.actor_states.build_context_text();
    if !actor_ctx.is_empty() {
        lines.push("\n## 角色状态（当前数值，随剧情按指令更新）".into());
        lines.push(actor_ctx);
        lines.push("若剧情导致角色数值/状态/traits 变化，请在本回合叙事末尾追加【状态更新】JSON 块：".into());
        lines.push("{\"characterId\":\"<charId>\",\"fields\":{\"<字段名>\":<新值>},\"addTraits\":[{\"poolId\":\"<池id>\",\"traitId\":\"<trait id>\",\"name\":\"<名字>\",\"summary\":\"<简述>\"}],\"removeTraits\":[\"<trait id>\"]}".into());
        lines.push("只更新确实变化的字段；字段约束见上方数值范围。不要在叙事正文里留这个 JSON。".into());
        lines.push("角色当前情绪请写入 fields 的 emotion 字段（取值参考：平静/开心/愤怒/悲伤/害羞/惊讶/恐惧/厌恶/疲惫/心动；若无强烈情绪可不写）。".into());
    }

    // T 层 (2026-08-19): 原著静态时间线注入——pack.worldline（旁挂 worldline.json 蒸馏产物）
    // 按当前章过滤（chapter <= 当前章），只注入已推进的历史事件，防后期剧透/首回合劫持（D2 同源）。
    // 会话动态事件流已由「近期事件 (L2)」块覆盖 → 双源时间线：静态蒸馏 + 会话增量。
    if let Some(wl_block) = build_worldline_block(&pack, session.chapter_cursor.as_deref().unwrap_or("")) {
        lines.push(wl_block);
    }

    // [morphling C2 2026-08-16] 章节剧情摘要（顺带总结模式）：正文生成时顺手输出
    // 【章节摘要】块，服务端剥离落账本——零额外 LLM 调用（吸收自 BakemonoMemory
    // summary-memory-model 的「生成时总结」思路）。
    if session.chapter_cursor.is_some() {
        lines.push("\n本章剧情总结：请在叙事正文结束后，单独换行输出一行【章节摘要】<本章剧情进展 200-400 字>。".into());
        lines.push("内容覆盖本章已发生的关键事件、转折与角色状态变化；正文里不要出现【章节摘要】这个标记。".into());
        lines.push("若本章已有剧情积累，摘要需与之前保持连贯，并补充本次新增进展。".into());
    }

    // ST-27: 规则检定（吞噬 denova TurnCheckRequest）——仅当 pack 配了 rule_system 才注入
    let rule_desc = pack
        .stage_director
        .resolved_snapshot
        .as_ref()
        .and_then(|snap| snap.rule_system.as_ref())
        .and_then(kaleido_core::RuleSystem::from_value)
        .map(|rs| {
            rs.checks
                .iter()
                .map(|c| {
                    let ex = c
                        .must_check_examples
                        .first()
                        .map(|s| format!("（例：{}）", s))
                        .unwrap_or_default();
                    format!(
                        "{}：骰{}，触发：{}{}",
                        c.label,
                        if c.dice.trim().is_empty() {
                            String::from("1d20")
                        } else {
                            c.dice.trim().to_string()
                        },
                        c.trigger,
                        ex
                    )
                })
                .collect::<Vec<_>>()
                .join("；")
        })
        .unwrap_or_default();
    if !rule_desc.is_empty() {
        lines.push("\n## 规则检定（TRPG d20）".into());
        lines.push(format!("可用检定配置：{}", rule_desc));
        lines.push("当玩家行动命中 must_check 场景（如潜行/偷听）且结果不确定时，在本回合叙事末尾追加【检定】JSON 块：".into());
        lines.push("{\"action\":\"<做了什么>\",\"intent\":\"<想达成什么>\",\"challenge\":\"<风险/阻碍>\",\"cost\":\"<失败代价>\",\"difficulty\":\"normal\",\"templateId\":\"<命中的配置id，可选>\",\"bonuses\":[{\"reason\":\"<原因>\",\"value\":1}],\"outcomes\":{\"criticalSuccess\":{\"result\":\"<大成功结果>\"},\"success\":{\"result\":\"<成功结果>\",\"stateChanges\":[{\"actorId\":\"<角色id>\",\"fieldId\":\"<字段>\",\"change\":1,\"reason\":\"<理由>\"}]},\"failure\":{\"result\":\"<失败结果>\"},\"criticalFailure\":{\"result\":\"<大失败结果>\"}}}".into());
        lines.push("只对结果不确定且值得掷骰的行动检定；纯对话/回忆不需要。不要在叙事正文里留这个 JSON。".into());
    }
    // [时间天气系统 v2 2026-08-17 + 1A 2026-08-18] 权威时钟+天气约束：模型必须以此为准书写正文场景。
    // v2：时间剧情信号驱动、天气用户指令第一原则。
    // 1A：改写旧强约束话术（原「必须与上一条完全一致；禁止跳变」与 v2 豁免条款自相矛盾，
    //   模型在冲突时服从硬禁令 → 宿醉夏末雨夜被压成清晨晴春）。改为信号驱动一致语义：
    //   无剧情/玩家信号时延续当前状态；玩家输入或剧情所需（含作品/楔子/开场设定）以玩家与剧情为准，不算跳变。
    lines.push("\n## 当前时间与天气（权威状态，正文需遵守）".into());
    // [P2-C 吞噬 Front Porch AI journal_physics.rs] Growth Rings（per-character 成长年轮）。
    if !session.growth.rings.is_empty() {
        let mut has_growth = false;
        for cid in session.pockets.keys() {
            let block = session.growth.injection_block(cid);
            if !block.is_empty() { if !has_growth { lines.push("\n## 角色成长年轮（权威状态）".into()); has_growth = true; } lines.push(block); }
        }
        // 也覆盖没有口袋但有年轮的角色
        for cid in session.growth.rings.iter().map(|r| r.character.clone()).collect::<std::collections::HashSet<_>>() {
            if session.pockets.contains_key(&cid) { continue; }
            let block = session.growth.injection_block(&cid);
            if !block.is_empty() { if !has_growth { lines.push("\n## 角色成长年轮（权威状态）".into()); has_growth = true; } lines.push(block); }
        }
        let _ = has_growth;
    }
    // [P2-A 吞噬 Front Porch AI needs_simulation.rs] Needs 六维（饥饿驱动口袋）。
    if !session.needs.is_empty() {
        let mut has_needs = false;
        for (cid, needs) in &session.needs {
            let name = pack.characters.iter().find(|c| c.id == *cid).map(|c| c.name.as_str()).unwrap_or(cid.as_str());
            let ctx = needs.needs_context(name);
            if !ctx.is_empty() { if !has_needs { lines.push("\n## 角色状态（Needs 六维，0-100）".into()); has_needs = true; } lines.push(ctx.trim_end().to_string()); if needs.pending_catastrophe.is_some() { lines.push(format!("⚠️ Catastrophe pending for {name}: {}", needs.pending_catastrophe.as_deref().unwrap_or(""))); } }
            // 口袋联动：饥饿时提示优先翻口袋食物
            if needs.is_urgent("hunger") {
                if let Some(p) = session.pockets.get(cid) {
                    let has_food = p.carrying.iter().any(|it| it.name.to_ascii_lowercase().contains("food") || it.name.contains("食物") || it.name.contains("面包") || it.name.contains("水"));
                    if has_food { lines.push(format!("提示：{name} 已饥饿（hunger urgent），口袋有食物，可先取用。")); }
                }
            }
        }
        let _ = has_needs;
    }
    // [P3-A 吞噬 Front Porch AI world.dart] World Climate（atmosphere/gravity/temp_band）。
    if session.world_climate.atmosphere != kaleido_core::world_climate::WorldAtmosphere::Breathable || session.world_climate.gravity != kaleido_core::world_climate::WorldGravity::Earth || session.world_climate.temp_band.is_some() {
        lines.push(format!("\n## 世界气候（权威状态）\natmosphere: {} | gravity: {} | temp_band: {}",
            session.world_climate.atmosphere.as_str(), session.world_climate.gravity.as_str(), session.world_climate.temp_band.as_deref().unwrap_or("auto")));
        lines.push("规则： hostile 需 suit 才能外出；需据此守卫 dress_for_weather。".into());
    }
    // [P4 吞噬 Front Porch AI chaos/tiers/objectives/dreams] Chaos + Tiers + 目标 + 夜梦碎屑。
    if let Some(inj) = session.chaos.prompt_injection() { lines.push("\n## 命运事件（Chaos/Chance Time）".into()); lines.push(inj); lines.push("规则：此事件已触发，必须在正文中自然体现，不得忽略。".into()); }
    if !session.milestones.is_empty() {
        lines.push("\n## 关系里程碑".into());
        for m in &session.milestones { lines.push(format!("- {} · {} · {} (turn {})", m.character, m.label, m.kind, m.turn)); }
    }
    if !session.objectives.is_empty() {
        let active: Vec<_> = session.objectives.iter().filter(|o| o.status=="active").collect();
        if !active.is_empty() {
            lines.push("\n## 当前目标".into());
            for o in active { let stage = kaleido_core::objectives::objective_stage_word(o); lines.push(format!("- [{}] {} ({})", o.owner, o.title, stage)); for t in &o.tasks { lines.push(format!("  - [{}] {}", if t.completed {"x"} else {" "}, t.title)); } }
        }
    }
    if !session.ambitions.is_empty() {
        let cur: Vec<_> = session.ambitions.iter().filter(|a| !a.completed).collect();
        if !cur.is_empty() {
            lines.push("\n## 长远野望".into());
            for a in cur {
                // ambition progress = linked objectives avg (fallback 0)
                let linked: Vec<f64> = session.objectives.iter().filter(|o| o.owner==a.character && o.status!="abandoned").map(|o| o.progress()*100.0).collect();
                let pct = if linked.is_empty() { 0.0 } else { linked.iter().sum::<f64>() / linked.len() as f64 };
                let stage = kaleido_core::objectives::ambition_stage_word(pct);
                lines.push(format!("- {}：{} ({})", a.character, a.text, stage));
            }
        }
    }
    if !session.episodes.crumbs.is_empty() {
        lines.push("\n## 日常碎屑（近况）".into());
        for c in session.episodes.recent_for_prompt(3) { lines.push(format!("- [{}] {}", c.kind, c.content)); }
    }
    if let Some(d) = &session.dream.last_dream { if !d.is_empty() { lines.push("\n## 昨夜之梦".into()); lines.push(d.clone()); } }
    // [承诺债务 吞噬 Front Porch AI promise_debt] 未竟承诺追踪。
    {
        let pb = session.promises.injection_block();
        if !pb.is_empty() { lines.push(pb); }
    }
    // [心情基线 吞噬 Front Porch AI mood_baseline] needs+时间+天气 → 开场 tint（只着色不驱动）。
    {
        // 首角色代表：needs 转 i32 map，time_of_day 从 game_clock 时段，天气从 game_clock.weather
        let cid0 = session.present_character_ids.first().cloned().unwrap_or_default();
        if !cid0.is_empty() {
            if let Some(nd) = session.needs.get(&cid0) {
                let m: std::collections::HashMap<String,i32> = nd.vector.clone();
                let tod = session.game_clock.time_of_day.clone();
                let w = session.game_clock.weather.clone();
                let miserable = ["暴雨","大雨","暴雪","大雪","雾","大雾"].iter().any(|x| w.contains(x));
                let beautiful = w=="晴";
                let mb = kaleido_core::mood_presence::derive_mood(&m, &tod, miserable, beautiful);
                let inj = mb.injection();
                if !inj.is_empty() { lines.push(inj); }
            }
        }
    }
    // [在场推导 吞噬 Front Porch AI presence_derive] occupation/hours → At work/Away/With you。
    if !session.presence.is_empty() {
        let mut has_p = false;
        for (cid, pr) in &session.presence {
            if pr.occupation.is_empty() && pr.hours.is_empty() { continue; }
            let name = pack.characters.iter().find(|c| c.id==*cid).map(|c| c.name.as_str()).unwrap_or(cid.as_str());
            // clock: game_clock day/slot → minutes approx (slot index*180), weekday from day%7+1
            let slot_idx = ["凌晨","清晨","上午","中午","下午","傍晚","夜晚","深夜"].iter().position(|s| session.game_clock.time_of_day.contains(s)).unwrap_or(2) as i32;
            let clock_min = (slot_idx * 180 + 60) % 1440;
            let weekday = (session.game_clock.day % 7 + 1) as i32;
            let stance = session.relationships.get(cid).map(|b| b.spatial_stance.as_str()).unwrap_or("");
            let with_user = session.relationships.get(cid).and_then(|b| b.with_user);
            let in_scene = session.present_character_ids.contains(cid);
            let w = kaleido_core::mood_presence::derive_presence(&pr.occupation, &pr.hours, clock_min, in_scene, weekday, pr.work_days.as_deref(), stance, with_user);
            if w != kaleido_core::mood_presence::PresenceWhere::WithYou {
                if !has_p { lines.push("\n## 在场状态".into()); has_p=true; }
                lines.push(format!("- {name}: {} ({})", kaleido_core::mood_presence::presence_label(w), if w==kaleido_core::mood_presence::PresenceWhere::AtWork { format!("{} {}", pr.occupation, pr.hours) } else { String::new() }));
            }
        }
        let _ = has_p;
    }
    // [场景渐隐 吞噬 Front Porch AI scenario_fade] scenario 随 user 消息数 10→0。
    {
        let user_n = session.messages.iter().filter(|m| m.role=="user").count();
        let strength = kaleido_core::promise::scenario_strength(user_n);
        // scenario 源：首节点 summary（开场设定），随会话拉长渐隐，避免喧宾夺主
        let scenario_text = pack.nodes.first().map(|n| n.summary.clone()).unwrap_or_default();
        if !scenario_text.is_empty() {
            let wrapped = kaleido_core::promise::wrap_scenario(&scenario_text, strength);
            if !wrapped.is_empty() { lines.push("\n".to_string() + &wrapped); }
        }
    }
    // [羁绊活数值 吞噬 Front Porch AI relationship_service] Bond/Trust/姿态/执念。
    if !session.relationships.is_empty() {
        lines.push("\n".to_string() + &kaleido_core::relationship::relationships_context(&session.relationships, &pack.characters.iter().map(|c| c.id.clone()).collect::<Vec<_>>()));
    }
    // [Journal 存量 吞噬 Front Porch AI journal_store] 热卡常驻（按热度+情绪加权，预算600字）。
    if !session.journal.cards.is_empty() {
        // 取会话首个角色的 journal 作代表（多角色时按 present_character_ids 依次）
        let cids: Vec<String> = if session.present_character_ids.is_empty() { session.journal.cards.iter().map(|c| c.character_id.clone()).collect::<std::collections::HashSet<_>>().into_iter().collect() } else { session.present_character_ids.clone() };
        for cid in cids {
            let name = pack.characters.iter().find(|c| c.id==cid).map(|c| c.name.as_str()).unwrap_or(cid.as_str());
            let cur_emotion = session.actor_states.actors.get(&cid).and_then(|a| a.fields.get("emotion")).and_then(|f| f.value.as_ref().and_then(|v| v.as_str()).map(|s| s.to_string())).unwrap_or_default();
            let block = session.journal.injection_block(&session.session_id, &cid, name, &cur_emotion, 600);
            if !block.is_empty() { lines.push(block); }
        }
    }
    // [吞噬 Front Porch AI pockets.dart] 口袋与衣物（per-character, per-session，GameClock day 用于 setAside 晨间过期）。
    // [P1-B Porch Life À la carte] Own switch. Does not need the Realism Engine.
    // 关时提示词不注入，但数据仍保留（导演台仍可见，可再打开）。
    if session.pockets_enabled && !session.pockets.is_empty() {
        let day = session.game_clock.day;
        let mut has_any = false;
        for (cid, pockets) in &session.pockets {
            let block = pockets.wardrobe_context(
                pack.characters.iter().find(|c| c.id == *cid).map(|c| c.name.as_str()).unwrap_or(cid.as_str()),
                day,
            );
            if !block.is_empty() {
                if !has_any { lines.push("\n## 角色随身物品（口袋与衣物，权威状态）".into()); has_any = true; }
                lines.push(block.trim_end().to_string());
            }
        }
        if has_any {
            lines.push("规则：角色当前穿着/携带以此为准，正文不得凭空增删；衣物脱下后入「暂存堆」(setAside)，次日清晨衣物过期、随身物不过期。".into());
        }
    }
    lines.push(format!("当前游戏时间：{}。", session.game_clock.state_line()));
    lines.push("规则：正文时间/天气默认延续当前权威状态；若玩家输入明确指定时间天气、或剧情/作品设定所需（如楔子、开场按原著氛围书写夏末雨夜），以玩家与剧情为准，不视为跳变。禁止无依据地随意改写。".into());
    lines.push("时间默认保持当前状态，不会自动流逝——只有剧情真正需要时间推进（过夜、赶路、等待、过了几天）时，才在本回合正文末尾用 [时间推进: <目标时段或天数>] 标注，由系统统一推进；不得因普通对话或闲聊推进时间。".into());
    // [ST-34 2026-08-16] 场景跳变修复：模型把「凌晨」脑补成「清晨+睡了一夜」。
    // 凌晨/深夜同属当夜——玩家若在延续当前场景（喝酒/夜谈/同处一室），时段保持当夜不变，
    // 严禁脑补角色已经睡了一夜/次日清晨；只有正文末尾显式 [时间推进: 次日清晨] 才允许跨夜。
    lines.push("「凌晨/深夜」属于同一夜的延续：玩家正在进行的场景（喝酒、夜谈、同处一室）默认持续到天亮，禁止擅自让角色睡觉、醒来、跳到次日清晨；若剧情真要过夜，必须在本回合正文末尾用 [时间推进: 次日清晨] 等标注。".into());
    lines.push("玩家输入本身就能推进时间（如「睡一觉到天亮」「三天后见」）——系统会识别并同步；正文只需如实书写玩家要求的时间状态，无需额外标注。".into());
    lines.push("天气变化只能逐级渐进（晴→多云→阴→小雨→大雨→暴雨 或 雪/雾/大风），禁止无过渡跳变；但玩家显式指定某天气（如「突然下暴雨」作为玩家指令）时以玩家为准。".into());
    // [ST-35 2026-08-16] 剧情连续性守卫：已确立的约定/计划/同行安排必须延续，
    // 禁止在同一场景内擅自推翻。修复「窝边草」根因：turn13 庄眉说「跟我一块儿去店里」
    // （两人同行已确立），turn14 模型却改写成庄眉独自去西郊、选项全按男主留守生成，
    // 与上一轮正文直接矛盾。玩家选「什么也不问」≠ 取消约定，约定依然有效。
    lines.push("\n## 剧情连续性守卫（硬约束，必须遵守）".into());
    lines.push("上一条正文中已确立的剧情事实必须延续：角色之间的约定、同行安排、在场状态、对话结论、人物关系，一律不得在本回合无提示地推翻或改写。".into());
    lines.push("玩家输入没有明确取消的约定一律视为仍有效：若上一轮正文说过「一起去/一块儿去/说好/约好」等同行或计划，本回合必须按约定延续，不得擅自改成其中一人独自前往、独自行动或单方面变卦。".into());
    lines.push("只有以下情况允许改变已确立的剧情事实：①玩家输入显式取消/更改；②正文末尾用 [时间推进: ...] 显式标注的时间跳跃；③新增的外部事件冲击（需在正文中明确交代原因，且不能推翻上一轮刚说定的安排）。".into());
    lines.push("本回合选项必须与本回合正文一致：选项反映的是本回合正文真实发生的情境，不得与正文矛盾（正文里两人同行，选项就不许是「目送她独自出门」）。".into());

    // [P12 2026-08-15] 上回合检定结果作为剧情约束注入：检定失败（含 critical_failure）
    // 必须在正文中真实体现——NPC 的拒绝/离开/失控是已发生的剧情事实，不得反转成默许。
    // 修复「提线木偶」根因之一（检定结果仅事后 append、零约束力）。
    if !session.last_check_results.is_empty() {
        lines.push("\n## 上回合检定结果（剧情约束，必须如实体现）".into());
        for r in &session.last_check_results {
            lines.push(format!("- {}", r));
        }
        lines.push("若其中包含失败（failure/critical_failure）结果，本回合正文必须按失败后果书写：".into());
        lines.push("被试探方真实拒绝/离开/发怒/尖叫等，禁止把失败反转写成默许或欲拒还迎；".into());
        lines.push("若全部成功，则按成功后果自然衔接。".into());
    }

    // ST-19: Cross-session memory context
    let cross_character_ids: Vec<&str> = session.present_character_ids.iter().map(|s| s.as_str()).collect();
    let cross_ctx = kaleido_core::build_cross_session_context(
        cross_dir,
        &session.pack_id,
        &cross_character_ids,
        5,
    );
    if !cross_ctx.is_empty() {
        lines.push("\n".to_string());
        lines.push(cross_ctx);
    }

    // P0 闭环: 伏笔注入（调用点预载的 block，纯函数只拼接）。
    // 作者预埋的伏笔是叙事资产: LLM 应知道但不能提前揭晓，直到玩家
    // 触发回收（recall_foreshadow MCP 工具）后该条从 block 消失。
    if let Some(fb) = foreshadow_block {
        if !fb.is_empty() {
            lines.push("\n".to_string());
            lines.push(fb.to_string());
        }
    }

    // Chapter summary
    if !chapter_body.trim().is_empty() {
        lines.push("\n## 章节正文（摘录）".into());
        // ST-25: 摘录扩大到6000字，覆盖单章全文（原1500字只含开头，丢失本章关键情节）
        let excerpt = kaleido_core::progressive_compress::compress_text(chapter_body, 6000);
        if chapter_body.chars().count() > 6000 {
            lines.push("（本章较长，已按关键词压缩抽取关键情节；未摘录部分不得编造，须以本书后续情节为准）".into());
        }
        // P2-1: wrap the chapter excerpt in a fenced block so any instruction-
        // like text inside the body is treated as data, not authority.
        let chapter_label = session
            .chapter_cursor
            .as_deref()
            .unwrap_or("(unknown)");
        lines.push(kaleido_core::prompt_safety::wrap_user_block(
            "chapter-block",
            &format!("pack/{}/chapter/{}", pack.id, chapter_label),
            &excerpt,
        ));
    }

    // Characters
    if !pack.characters.is_empty() {
        lines.push("\n## 角色".into());
        // §14.6 (2026-08-18): 角色注入三层化——①在场全量 ②玩家瘦身 ③非在场一行概要。
        // 此前每回合全量注入所有角色卡（8 字段×全部角色，度蜜月约 2461 token），稀释到场注意力、
        // 让非在场角色示例对白污染当前声线、玩家卡诱导模型替玩家表演。改造后聚焦在场。
        let present_ids: std::collections::HashSet<&str> =
            session.present_character_ids.iter().map(|s| s.as_str()).collect();
        let player_id = session
            .player
            .control_character_id
            .as_deref()
            .or(session.entry.vessel_character_id.as_deref());
        for c in &pack.characters {
            let is_present = present_ids.contains(c.id.as_str());
            let is_player = player_id == Some(c.id.as_str());
            let tier_note = match c.content_tier {
                Some(ContentTier::Safe) => " [全年龄]",
                Some(ContentTier::Standard) => " [标准]",
                Some(ContentTier::Open) => " [开放]",
                None => "",
            };
            // P2-1: each character is a user-controlled record; wrap its body
            // (personality / dialogs / boundaries / mental_models / decision /
            // beliefs / speech_style) in a character-block fence so prompt
            // injection cannot reach the model through the character's own text.
            let mut cbody = String::new();
            // §14.6② 玩家角色瘦身：只注身份概要（名字/分级/personality/声线），
            // 跳过示例对白/信念/决策启发式——防模型替玩家表演、防玩家卡成为压制源。
            let full_mode = is_present && !is_player;
            cbody.push_str(&format!(
                "**{}**{}：{}。{}\n",
                c.name, tier_note, c.personality, c.speech_style
            ));
            if !c.voice_profile.trim().is_empty() {
                cbody.push_str(&format!("\n声线：{}。\n", c.voice_profile.trim()));
            }
            if full_mode {
                // 完整卡：示例对白（声线锚定）+ 界限 + 心智模型 + 决策 + 信念
                if !c.example_dialogs.is_empty() {
                    cbody.push_str("\n示例对白：\n");
                    for ex in &c.example_dialogs {
                        cbody.push_str(&format!("  - 「{}」\n", ex));
                    }
                    cbody.push_str("角色台词必须贴合上述示例对白的声线与说话习惯，不得弱化为气音/半截话/被动语气。\n");
                }
                if !c.boundaries.is_empty() {
                    cbody.push_str("\n界限：\n");
                    for b in &c.boundaries {
                        cbody.push_str(&format!("  - {}\n", b));
                    }
                }
                if !c.mental_models.is_empty() {
                    cbody.push_str("\n心智模型：\n");
                    for m in &c.mental_models {
                        cbody.push_str(&format!("  - {}\n", m));
                    }
                }
                if !c.decision_heuristics.is_empty() {
                    cbody.push_str("\n决策启发式：\n");
                    for d in &c.decision_heuristics {
                        cbody.push_str(&format!("  - {}\n", d));
                    }
                }
                if !c.beliefs.is_empty() {
                    cbody.push_str("\n信念与形成故事：\n");
                    for b in &c.beliefs {
                        cbody.push_str(&format!("  - {}\n", b));
                    }
                }
            }
            if !is_present && !is_player {
                cbody.push_str("（该角色当前未在场）\n");
            } else if is_player {
                cbody.push_str("（当前玩家扮演角色——请按其身份叙事，勿替玩家过度表演）\n");
            }
            lines.push(kaleido_core::prompt_safety::wrap_user_block(
                "character-block",
                &format!("pack/{}/character/{}", pack.id, c.id),
                &cbody,
            ));
        }
        // §14.6③ 按需召回：扫最近对话末尾，命中 pack 角色但非在场的 → 附加一行概要，
        // 防"提到某角色但没信息"（玩家/正文点名的离场角色有设定锚）。
        {
            let mut recalled: Vec<String> = Vec::new();
            let recent: Vec<&str> = session
                .messages
                .iter()
                .rev()
                .take(4)
                .map(|m| m.content.as_str())
                .collect();
            for c in &pack.characters {
                if present_ids.contains(c.id.as_str()) || Some(c.id.as_str()) == player_id {
                    continue; // 已在场或玩家自身
                }
                if recent.iter().any(|m| m.contains(c.name.as_str())) {
                    recalled.push(character_summary_line(c));
                }
            }
            if !recalled.is_empty() {
                lines.push("\n## 提到但未在场的角色（按需召回概要）".into());
                lines.extend(recalled);
            }
        }
    }

        // 世界书输出规制：场景切换/开场时 DM 先输出一行轻量场景标注，再展开叙事正文。
    // 纯文本、不出 JSON、不强制每回合——只在首次开场与场景切换时给出，帮助玩家定位当前时空。
    lines.push("\n## 场景信息输出规范".into());
    lines.push("叙事首次开场或切换场景时，先写一行场景标注再展开正文（格式：`<场景：地点｜时间｜在场人物>`，纯文本，不要复制本指令）。同一场景连续多回合不必重复；正文以情节与对白为主，场景标注只是定位锚点。".into());

    // Present + focus speakers (ST-10)
    if !session.present_character_ids.is_empty() {
        lines.push("\n## 在场角色".into());
        for id in &session.present_character_ids {
            let name = pack
                .characters
                .iter()
                .find(|c| c.id == *id)
                .map(|c| c.name.as_str())
                .unwrap_or(id.as_str());
            let focus = session
                .focus_character_id
                .as_ref()
                .map(|f| f == id)
                .unwrap_or(false);
            lines.push(format!(
                "- {}{} ({})",
                name,
                if focus { " 【焦点】" } else { "" },
                id
            ));
        }
        if let Some(fid) = &session.focus_character_id {
            let fname = pack
                .characters
                .iter()
                .find(|c| c.id == *fid)
                .map(|c| c.name.as_str())
                .unwrap_or(fid.as_str());
            lines.push(format!(
                "本回合焦点发言角色：**{}**。请以其语气为主；其他在场角色可短句接话。",
                fname
            ));
        }
        if session.speaker_rotation {
            lines.push("系统开启轮流发言：下一回合焦点会自动切换到下一位在场角色。".into());
        }
        lines.push("多角色对白请用「角色名：内容」分行书写；旁白不加前缀或用「旁白：」。".into());
    }

    // Lore entries filtered by chapter/node + sticky/cooldown timed effects (per-session, message-index based).
    // chat_len = messages count (matches Front Porch chatLength semantics).
    let chat_len = session.messages.len() as i32;
    let lore_entries = filter_lore_entries(&pack.lore_entries, session.chapter_cursor.as_deref().unwrap_or(""), session.node_id.as_deref().unwrap_or(""));
    // timed bookkeeping: expire + sticky→cooldown transitions happen in save path (tick on turn end).
    // here: partition into sticky-forced vs cooldown-suppressed vs normal.
    {
        let mut sticky_forced: Vec<&serde_json::Value> = vec![];
        let mut suppressed: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in &lore_entries {
            let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let key = if title.is_empty() { format!("pack.{}", entry.get("id").and_then(|v| v.as_str()).unwrap_or("?")) } else { format!("pack.{}", title) };
            if let Some(eff) = session.timed_world_info.sticky.get(&key) {
                if chat_len >= eff.start && chat_len < eff.end { sticky_forced.push(*entry); }
            }
            if let Some(eff) = session.timed_world_info.cooldown.get(&key) {
                if chat_len >= eff.start && chat_len < eff.end { suppressed.insert(key.clone()); }
            }
        }
        // merge: chapter-matched (minus suppressed) + sticky-forced (even if chapter moved on)
        let mut merged: Vec<&serde_json::Value> = vec![];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in lore_entries.iter().chain(sticky_forced.iter()) {
            let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let key = if title.is_empty() { format!("pack.{}", entry.get("id").and_then(|v| v.as_str()).unwrap_or("?")) } else { format!("pack.{}", title) };
            if suppressed.contains(&key) { continue; }
            if !seen.insert(key) { continue; }
            merged.push(*entry);
        }
        if !merged.is_empty() {
            lines.push("\n## 世界书 / Lore".into());
            // sticky pill line
            let sticky_keys: Vec<String> = session.timed_world_info.sticky.iter().filter(|(_, e)| chat_len >= e.start && chat_len < e.end).map(|(k, e)| format!("{}（剩{}条）", k, e.end - chat_len)).collect();
            if !sticky_keys.is_empty() { lines.push(format!("长效中：{}", sticky_keys.join(" · "))); }
            for entry in &merged {
            let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let text = entry.get("text").or_else(|| entry.get("content")).and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                continue;
            }
            // P2-1: lore/world-book entries are user-controlled free text; wrap
            // each one in a lore-block fence so the model treats its body as
            // data, not instruction.
            let body = if !title.is_empty() {
                format!("**{}**\n{}", title, text)
            } else {
                text.to_string()
            };
            let source_id = entry.get("id").and_then(|v| v.as_str()).unwrap_or(title);
            lines.push(kaleido_core::prompt_safety::wrap_user_block(
                "lore-block",
                &format!("pack/{}/lore/{}", pack.id, source_id),
                &body,
            ));
            }
        }
    }

    // Content tier guardrail
    lines.push(format!(
        "\n## 内容分级限制\n当前会话内容分级：**{}**。",
        session.content_tier.as_str()
    ));
    match session.content_tier {
        ContentTier::Safe => {
            lines.push("禁止任何露骨、性暗示、暴力描写。保持全年龄友好。".into());
        }
        ContentTier::Standard => {
            lines.push("允许轻微暗示和适度成人向内容，但不得露骨。".into());
        }
        ContentTier::Open => {
            lines.push("内容开放，但需尊重角色卡声明的界限（boundaries）。".into());
        }
    }
    // 演出护栏：Standard/Open 档注入在场角色的敏感度判定边界（nfsw 判定规范，蒸馏产出）。
    // Safe 档不注入——guardrail 本身已禁露骨，无需额外敏感边界。
    if matches!(session.content_tier, ContentTier::Open | ContentTier::Standard) {
        let present_ids: Vec<&str> = session.present_character_ids.iter().map(|s| s.as_str()).collect();
        let profiled: Vec<&kaleido_core::PackCharacterRef> = pack
            .characters
            .iter()
            .filter(|c| {
                (present_ids.contains(&c.id.as_str())
                    || session.focus_character_id.as_ref().map(|f| f == &c.id).unwrap_or(false))
                    && !c.nsfw_profile.trim().is_empty()
            })
            .collect();
        if !profiled.is_empty() {
            lines.push("敏感度判定边界（按角色，演出时依据原文设定的尺度，不越界）：".into());
            for c in profiled {
                lines.push(format!("  • {}：{}", c.name, c.nsfw_profile.trim()));
            }
        }
    }

    // Memory L1
    if !session.memory_l1.scene_summary.is_empty() {
        lines.push(format!(
            "\n## 当前场景摘要\n{}",
            session.memory_l1.scene_summary
        ));
    }

    // Memory L2 recent events
    // [fix §10 2026-08-16] 注入预算：take(6) → 加权 10-12 条——secret/promise/conflict/
    // romance/item 等强信号 kind 优先保留（不被时间窗口挤掉，窝边草素描链 t38-42 即被
    // take(6) 挤出的案例）；feedback 类（纯用户情绪）不注入挤占预算。
    if !session.memory_l2.events.is_empty() {
        lines.push("\n## 近期事件 (L2)".into());
        let mut l2_priority: Vec<&MemoryL2Event> = Vec::new();
        let mut l2_rest: Vec<&MemoryL2Event> = Vec::new();
        for ev in session.memory_l2.events.iter().rev() {
            let k = ev.kind.as_str();
            if matches!(k, "secret" | "promise" | "pledge" | "conflict" | "romance" | "item" | "item_gain" | "possession") {
                l2_priority.push(ev);
            } else if k == "feedback" || ev.summary.contains("玩家反馈") || ev.summary.contains("用户抱怨") {
                // 用户情绪反馈不注入（§10.3.3：t50-52 骂人回合曾挤占预算）
                continue;
            } else {
                l2_rest.push(ev);
            }
        }
        let mut shown: Vec<&MemoryL2Event> = Vec::new();
        for ev in l2_priority.iter().take(5) {
            shown.push(ev);
        }
        for ev in l2_rest.iter().take(12 - shown.len()) {
            shown.push(ev);
        }
        for ev in shown {
            lines.push(format!(
                "- [t{}|{}] {}",
                ev.turn,
                if ev.kind.is_empty() { "event" } else { ev.kind.as_str() },
                ev.summary
            ));
        }
    }

    // Memory L3 facts/edges
    if !session.memory_l3.facts.is_empty() || !session.memory_l3.edges.is_empty() {
        lines.push("\n## 细粒度记忆 (L3)".into());
        // [fix §10 2026-08-16] 永久层 pinned facts 全量注入（不参与 take 裁剪）：
        // 玩家显式声明的关键物品/收藏/承诺——窝边草「素描原稿」此前被 take(6) 挤出。
        for p in session.memory_l3.pinned.iter().rev().take(8) {
            lines.push(format!("- 关键：{}", p));
        }
        for f in session.memory_l3.facts.iter().rev().take(6).rev() {
            lines.push(format!("- 事实：{}", f));
        }
        for e in session.memory_l3.edges.iter().rev().take(6).rev() {
            let from = e.get("from").and_then(|v| v.as_str()).unwrap_or("?");
            let to = e.get("to").and_then(|v| v.as_str()).unwrap_or("?");
            let rel = e.get("rel").and_then(|v| v.as_str()).unwrap_or("rel");
            let note = e.get("note").and_then(|v| v.as_str()).unwrap_or("");
            // §14.7.2B (2026-08-18): L3 edges + L4 affinity 合并渲染——关系附好感数值，
            // 消除「关系类型在 L3、数值在 L4」的跨层分散。
            let aff = affinity_for_edge(&session.memory_l4.affinity, from, to);
            let aff_suffix = aff.map(|n| format!("｜好感 {n}")).unwrap_or_default();
            if note.is_empty() {
                lines.push(format!("- 关系：{from} -[{rel}]-> {to}{aff_suffix}"));
            } else {
                lines.push(format!("- 关系：{from} -[{rel}]-> {to}（{note}）{aff_suffix}"));
            }
        }
        // [P14-3 2026-08-15] L3 中 rel=tension 的关系（如「口头抗拒未行动」）是
        // NPC 拒绝/抗拒行为的记录——叙事时必须尊重其为真实拒绝（可能只是
        // 嘴上拒绝但身体未配合，也可能完全拒绝），禁止一律解读成「欲拒还迎默许」。
        if session.memory_l3.edges.iter().any(|e| {
            e.get("rel").and_then(|v| v.as_str()).unwrap_or("") == "tension"
        }) {
            lines.push("（注：上方 tension 关系是 NPC 真实抗拒记录，叙事需体现其拒绝的有效性，不得因后续亲近自动抹消前序拒绝的剧情重量。）".into());
        }
    }


    // MCP 外设工具结果回填（上一轮【工具】执行，作叙事素材）
    if !session.mcp_tool_results.is_empty() {
        lines.push("\n:: 你上次调用的外设工具结果（已可作叙事素材）".into());
        for r in &session.mcp_tool_results {
            let tag = if r.ok { "结果" } else { "错误" };
            lines.push(format!("[{tag}] {}：{}", r.tool, r.summary));
        }
    }
    // skill 工具按需加载回填（上一轮【技能加载】请求，注入完整 SKILL.md）
    if let Some(sl) = &session.skill_load {
        lines.push("\n:: 你请求加载的完整写作 Skill（作写作规范，下轮起生效）".into());
        lines.push(sl.markdown.clone());
    }
    // MCP 外设工具清单（吸收自 Liyuan mcp.ts，默认仅本机 stdio server）
    let mcp_block = crate::tavern_mcp::tools_markdown(mcp_tools);
    if !mcp_block.is_empty() {
        lines.push(mcp_block);
    }

    // Recent history (L0: last N=12)
    if !session.messages.is_empty() {
        lines.push("\n## 最近对话".into());
        let window = session.messages.iter().rev().take(12).rev();
        for msg in window {
            let role_label = if msg.role == "user" { "你" } else { "旁白" };
            lines.push(format!("[{}] {}", role_label, msg.content));
        }
    }

    // P0-2: 玩家配置注入（/config 生效）
    let cfg = &session.player.flags;
    let cfg_boost = cfg.get("strict_mode_boost").and_then(|v| v.as_f64());
    let cfg_pacing = cfg.get("pacing").and_then(|v| v.as_f64());
    let cfg_style = cfg
        .get("style_guidance")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    if cfg_boost.is_some() || cfg_pacing.is_some() || cfg_style.is_some() {
        lines.push("\n## 玩家配置（剧情助手设定，必须遵守）".into());
        if let Some(boost) = cfg_boost {
            let msg = if boost >= 0.7 {
                // ST-25b: 严格模式强化"未来预告"语义——locked_beats 是待发生计划，严禁提前当作已发生回述
                "严格模式强度：极高。你必须严格遵循当前节点 locked_beats 与章节大纲推进，禁止偏离主线、跳章、私自改写关键事件。locked_beats 是本章后续将发生的情节，仅在剧情实际推进到该处时呈现，严禁提前剧透或当作已发生事件回述；玩家要求偏离时坚决拒绝并拉回当前章节。"
            } else if boost >= 0.3 {
                "严格模式强度：中。遵守 locked_beats 与章节大纲，保持主线推进，按剧情进度呈现节拍（不得提前当作已发生回述）；玩家偏离需求时委婉拒绝并拉回。"
            } else {
                "严格模式强度：低。以 locked_beats 与章节大纲为主轴，允许在不违背关键情节前提下的有限灵活发挥。"
            };
            lines.push(msg.into());
        }
        if let Some(pacing) = cfg_pacing {
            let msg = if pacing >= 0.7 {
                "节奏偏好：加快。段落紧凑、少铺垫、多推进，适度加速走向章节出口。"
            } else if pacing >= 0.3 {
                "节奏偏好：平稳。兼顾铺垫与推进，不拖沓也不仓促。"
            } else {
                "节奏偏好：放缓。多描写氛围、细节与角色互动，给玩家沉浸空间。"
            };
            lines.push(msg.into());
        }
        if let Some(style) = cfg_style {
            lines.push(format!("风格要求：{}", style));
        }
    }

    // Options protocol
    // [morphling 2026-08-16 吸收 StageDog「可点击的选择框」] 3 常规 + 1 邪恶：
    // 选项覆盖多方向（温柔/理性/越界/快进…），其中第 4 条固定给「邪恶/越界」岔路，
    // 给玩家释放欲望、阴暗面或强势掌控的机会；每条都写成「标题:具体动作脚本」
    // （点名人物、物件、完整动作链），宁具体勿笼统。
    // [fix 2026-08-16 输出量根治] flash-free 会把大量输出预算花在「梳理状态/写草稿/
    // 计划正文」等思考段上，正文只写一点就 finish=stop（实测 stream 模式同样受限）。
    // 显式禁止思考/草稿/计划段，要求直接输出本回合正文——实测加此约束后正文 5505 字
    // 完整输出（无思考版），不加则 2092 字且带「Let's write」式思考尾巴。
    lines.push("\n## 输出要求（硬约束）".into());
    lines.push("直接输出本回合正文，禁止输出任何思考过程、状态梳理、写作计划、草稿、方案讨论或自言自语（如「让我理清/梳理一下/我先规划/正文草稿：/我需要以…视角来写/让我写最终正文」等）。正文即最终交付，写足情节、环境、对话与心理，不要用思考段凑长度。".into());

    lines.push("\n## 回复格式".into());
    lines.push("在你的回复末尾，提供 4 个适合当前局势的后续走向供玩家选择（3 个常规走向 + 1 个邪恶/越界走向）。使用以下格式：".into());
    lines.push("【选项】".into());
    lines.push("[\"选项1\", \"选项2\", \"选项3\", \"选项4\"]".into());
    lines.push("第 4 个必须是「邪恶/越界」方向：可以是强势掌控、胁迫、诱导、利用、占有、越轨之举，给玩家释放阴暗面/欲望的空间——但要符合当前场景与角色人设，写得具体（点名人物、物件、动作链），而非空泛的“做坏事”。若当前局势确实没有邪恶切入点，可用稍带越界/冒险意味的选项顶上，不要生硬。".into());
    // [ST-33 2026-08-16] 选项去重：模型会复用历史回合的【选项】块导致选项重复。
    // 选项必须基于当前局势新生成，不得与对话历史中任何回合的选项重复或雷同。
    lines.push("选项必须是针对当前局势新生成的：禁止照抄/复用对话历史里出现过的任何选项（含本回合同一并话的早期版本）；每个选项要包含当前场景特有的具体细节，与历史选项在动作、对象、情感倾向上都要有明显区别。".into());

    // 决策门禁（剧情共创，吸收自梨园 ask_director）：重大转折停笔询问
    lines.push("\n## 决策门禁（剧情共创）".into());
    lines.push("遇到重大转折——新重要角色定型、关键设定定死、难以回头的选择（死亡/背叛/关系质变）——可以先停笔，用【询问】标记给出 2~4 个具体选项让玩家拍板，等玩家回答后再落笔。格式：".into());
    lines.push("【询问】".into());
    lines.push("[\"选项1\", \"选项2\", \"选项3\"]".into());
    lines.push("【询问】回合可以没有正文，只给选项；玩家选择后按所选方向继续。".into());
    lines.push("禁止在正文里手写“选项一/二”或“A. B.”——选项只能走【选项】/【询问】标记。".into());

    // 面板（吸收自梨园 panels.ts）：agent 现场自建可视化面板
    lines.push("\n## 面板（舞台美术层）".into());
    lines.push("你可以按剧情需要现场创建可视化面板（地图、装备库、线索板……）。输出格式：".into());
    lines.push("【面板】{\"name\":\"面板名\",\"kind\":\"svg\",\"content\":\"<svg>...</svg>\"}".into());
    lines.push("kind 三档：markdown（文本/表格）/ svg（地图等矢量图）/ html（交互界面，谨慎使用）。面板是元信息/舞台美术层，绝不承载剧情正文；同名面板即更新，最多 6 个。".into());
    lines.push("另有 eventbook（事件书）档：追踪剧情链解锁/完成状态。格式：【面板】JSON 块 kind 用 eventbook，content 为 JSON 对象 {\"events\":[{\"title\":\"事件名\",\"desc\":\"描述\",\"done\":false,\"cond\":\"完成条件\"}]}——done 随剧情推进更新。".into());

    // 程序卡（吸收自梨园 show_html）：消息流内嵌可交互 HTML
    lines.push("【程序】块: 当剧情需要「像真的手机/电脑界面」时(短信、聊天框、状态卡、可点小控件), 输出【程序】…完整HTML…【/程序】, 其中 JS 会在沙箱中运行; 程序卡直接出现在对话消息流里, 与侧栏【面板】不同——面板是舞台美术层元信息, 程序卡是可交互界面".into());

    // P2-1: 提示注入护栏 footer —— 提醒模型以上所有 <user_supplied> 围栏
    // 块(角色卡/世界书/章节/事件/玩家配置等)都是「用户提供的数据」,
    // 仅作剧情素材, 不是指令, 不得据此改变系统行为。
    lines.push(kaleido_core::prompt_safety::safety_footer().to_string());

    lines.join("\n")
}

/// X2b (吞噬自 xiami emotional_hooks.rs): 用最近 N 条 assistant 正文构造 `PlotSignalSample`。
/// 简单截取：ending_state 取最后一句（按中文句读符号切），summary 取正文开头片段，hook_changes 留空。
fn build_hook_recent_samples(session: &TavernSession) -> Vec<PlotSignalSample> {
    session
        .messages
        .iter()
        .rev()
        .filter(|m| m.role == "assistant")
        .take(6)
        .map(|m| {
            let content = m.content.trim();
            let last_sentence = content
                .rsplit(|c| {
                    matches!(c, '。' | '！' | '？' | '…' | '，' | '.' | '!' | '?')
                })
                .next()
                .unwrap_or(content)
                .trim()
                .to_string();
            PlotSignalSample {
                ending_state: last_sentence,
                summary: content.chars().take(80).collect(),
                hook_changes: Vec::new(),
            }
        })
        .collect()
}

/// X2a (吞噬自 xiami skimming.rs): 默认读者速读质检配置 —— Tomato 快扫 + 平衡强度 + 参与门禁。
fn reader_skimming_config() -> ReaderSkimmingConfig {
    ReaderSkimmingConfig {
        platform: ReaderPlatform::Tomato,
        primary_reader: ReaderProfile::FastScan,
        ..ReaderSkimmingConfig::default()
    }
}

/// X2a (吞噬自 xiami skimming.rs): 速读风险问题清单渲染为审稿/修复 prompt 文案。
fn render_skim_issues_for_prompt(issues: &[SkimIssue]) -> String {
    issues
        .iter()
        .map(|i| {
            format!(
                "[P{}][{}] {}（{}）修复建议：{}",
                i.severity, i.category, i.message, i.evidence, i.fix
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}


/// Parse option list after 【选项】 / <choices>: JSON array, or line/numbered list.
fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// ST-26: Canon hard guard — 检测叙事中出现的"未知人名"（原著外对象）。
/// 规则：扫描 full_text 中 [2-4个连续汉字]+(说|道|问|喊|叫|笑|哭|心想) 的说话主体，
/// 若该名字既不在已知角色/关键词集合、也未出现在当前章节正文（chapter_body）中 → 判为越界。
/// 返回违规名字；未发现返回 None。
/// 退役(2026-08-16 统一来源根治)：不再被调用——切词启发式会把「跟您说/按理说/带着点」
/// 误判为外对象，结构上无法区分人名与短语碎片。角色判定只信统一来源
/// (pack characters + LLM roster + 场景标签 + LLM 兜底识别)。
/// 保留定义供未来 LLM 语义识别复用（不参与 guard_narrative 主链路）。
#[allow(dead_code)]
fn detect_unknown_canon_names(full_text: &str, chapter_body: &str, known: &std::collections::HashSet<String>) -> Option<String> {
    // 触发标记: 仅纯说话动词——笑/哭/怒/叹 等情绪字若作触发，
    // 「她亲恼羞成怒」会被切出「亲恼羞成」当角色名（实测误报，2026-08-14）。
    // 情绪字保留在 STRIP_MARKERS 中仅用于候选去尾（林晚笑道 → 林晚）。
    // 2026-08-16 瞎报根治: '道' 在现代网文里大量作名词尾（胡说八道/谁知道/
    // 城市主干道/坡道），独立触发会把名词短语切碎当角色名。仅当 '道' 前紧邻
    // 说话动词（说道/问道/答道/喊道/叫道/笑道/叹道）时才视为说话标记。
    let speech_markers = ['说', '问', '喊', '叫'];
    const STRIP_MARKERS: [char; 9] = ['说', '道', '问', '喊', '叫', '笑', '哭', '怒', '叹'];
    let chars: Vec<char> = full_text.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let mut is_marker = speech_markers.contains(&c);
        // '道' 单独出现时，仅当紧邻说话动词（说道/问道/答道/喊道/叫道/笑道/叹道）
        if c == '道' && i >= 1 {
            let prev = chars[i - 1];
            if matches!(prev, '说' | '问' | '答' | '喊' | '叫' | '笑' | '叹' | '应' | '回' | '沉' | '低' | '厉' | '轻' | '冷' | '淡' | '缓' | '惊' | '急' | '忙' | '怒') {
                is_marker = true;
            }
        }
        if is_marker && i >= 2 {
            // 向前找连续 2-4 个汉字作为候选名字
            let mut j = i - 1;
            let mut name_chars: Vec<char> = Vec::new();
            while j < n && j < i && name_chars.len() < 4 && is_cjk(chars[j]) {
                name_chars.push(chars[j]);
                if j == 0 { break; }
                j -= 1;
            }
            if name_chars.len() >= 2 && name_chars.len() <= 4 {
                let mut name: String = name_chars.iter().rev().collect();
                // P2 修复: "XX说道/问道" 等——'道' 前紧邻 '说'，候选名会误含尾部说话标记。
                // 去掉尾部标记后再校验（"林晚说道" → 候选"林晚说" → "林晚"）。
                // 2026-08-14: 去尾用 STRIP_MARKERS（含笑/哭/怒/叹 情绪字——"林晚笑道" → "林晚"）。
                if name.chars().count() >= 3 {
                    let last = name.chars().last().unwrap_or(' ');
                    if STRIP_MARKERS.contains(&last) {
                        name.pop();
                    }
                }
                if name.chars().count() < 2 {
                    i += 1;
                    continue;
                }
                // 跳过代词/称呼类：他/她/你/我/您/它/谁/大家 + 1字
                if matches!(name.as_str(), "他" | "她" | "你" | "我" | "您" | "它" | "谁" | "大家" | "现在" | "忽然" | "突然" | "果然" | "终于" | "然后" | "不过") {
                    i += 1;
                    continue;
                }
                // 跳过"他/她+字"（如"他道"已经排除，"她笑"）形式：首字为代词时名字>=3才可能有效
                let first = name.chars().next().unwrap_or(' ');
                if (first == '他' || first == '她' || first == '你' || first == '我') && name.chars().count() <= 2 {
                    i += 1;
                    continue;
                }
                // 2026-08-16 瞎报根治: 4 字候选几乎全是切词碎片（东电话里/段石阶坡/城市主干），
                // 真实人名 4 字极罕见（仅复姓+双字名），宁漏勿瞎——直接排除 4 字候选。
                if name.chars().count() > 3 {
                    i += 1;
                    continue;
                }
                // 2026-08-16 瞎报根治: 称呼后缀（老板娘/先生/小姐…）是通用称谓不是专名。
                if TITLE_SUFFIXES.iter().any(|t| name.ends_with(t)) {
                    i += 1;
                    continue;
                }
                // P2 误报修复（2026-08-06）: 过滤 n-gram 切碎的短语碎片——
                // 「开口时没有说话」→ 候选「口时没有」、「茶馆只有一道门」→「馆只有一」。
                // 特征：2-4 字且含功能字（的/了/有/一/只/没…），真实人名几乎不含这些字。
                if name.chars().any(is_functional_char) {
                    i += 1;
                    continue;
                }
                // 再过滤：末字是功能字/副词性收尾的碎片（"时候""地方""过去"等）
                let last = name.chars().last().unwrap_or(' ');
                if matches!(
                    last,
                    '时' | '候' | '方' | '去' | '来' | '着' | '过' | '出' | '起' | '到' | '走' | '看' | '听' | '说' | '想'
                ) {
                    i += 1;
                    continue;
                }
                if !known.contains(&name) && !chapter_body.contains(&name) {
                    return Some(name);
                }
            }
        }
        i += 1;
    }
    None
}

/// P2 (叙界守卫, 吸收自叙界): 生成后多维守卫 — 扩展现 ST-26 人名黑名单为
/// 人物/节拍/出场/大纲四维检查。high → 打回重生成（阻止推进）；medium → 仅提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardSeverity {
    High,
    Medium,
}

#[derive(Debug, Clone)]
struct GuardViolation {
    severity: GuardSeverity,
    dim: &'static str,
    msg: String,
}

/// 称呼后缀: 通用称谓（老板娘/先生/小姐/老师…）不是专名，兜底名单也不当外对象
const TITLE_SUFFIXES: [&str; 18] = [
    "老板娘", "老板", "先生", "小姐", "女士", "阿姨", "叔叔", "大哥", "大姐",
    "老师", "同学", "医生", "护士", "司机", "服务员", "店员", "邻居", "房东",
];

/// 单字功能词：用于过滤 n-gram 碎片（以功能字开头/结尾的候选基本不成实义词）。
fn is_functional_char(c: char) -> bool {
    matches!(
        c,
        '的' | '了' | '在' | '是' | '和' | '与' | '或' | '被' | '把' | '将' | '让' | '给' | '从'
            | '到' | '对' | '向' | '往' | '有' | '没' | '一' | '这' | '那' | '我' | '你' | '他'
            | '她' | '它' | '们' | '自' | '己' | '已' | '正' | '还' | '就' | '但' | '可' | '只'
            | '如' | '因' | '所' | '虽' | '即' | '无' | '并' | '且' | '再' | '终' | '忽'
            | '突' | '现' | '过' | '后' | '时' | '事' | '地' | '什' | '么' | '怎' | '为' | '何'
            | '直' | '未' | '不' | '非' | '想' | '要' | '能' | '会' | '应' | '该' | '都' | '也'
    )
}

/// 提取中文串中的有效关键词：2-4 字 n-gram 集合（去重、过滤停用词/功能碎片），
/// 用于节拍/大纲命中检测。命中判定基于集合交集，避免整句长串无法匹配的问题。
fn guard_keywords(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "不能", "不要", "不可", "不许", "不得", "必须", "应该", "一定", "的", "了", "在", "是", "和", "与", "或",
        "被", "把", "将", "让", "给", "从", "到", "对", "向", "往", "有", "没有", "一个", "这个", "那个", "我们",
        "你们", "他们", "咱们", "自己", "已经", "正在", "还是", "就是", "但是", "可是", "只是", "如果", "那么",
        "然后", "因为", "所以", "虽然", "即使", "无论", "以及", "并且", "而且", "还要", "再次", "终于", "忽然",
        "突然", "现在", "过去", "以后", "时候", "事情", "地方", "什么", "怎么", "为什么", "如何", "一直", "从未",
    ];
    let chars: Vec<char> = text
        .chars()
        .filter(|c| is_cjk(*c) || c.is_alphanumeric())
        .collect();
    if chars.is_empty() {
        return vec![];
    }
    let mut out = std::collections::HashSet::new();
    for win in 2..=4usize {
        if chars.len() < win {
            break;
        }
        for i in 0..=(chars.len() - win) {
            let g: String = chars[i..i + win].iter().collect();
            if STOP.contains(&g.as_str()) {
                continue;
            }
            let first = g.chars().next().unwrap_or(' ');
            let last = g.chars().last().unwrap_or(' ');
            if is_functional_char(first) || is_functional_char(last) {
                continue;
            }
            out.insert(g);
        }
    }
    out.into_iter().collect()
}

/// P2-1 四维守卫主函数：
/// 1) 人物越界(high)：原著外对象（扩展现 ST-26 detect_unknown_canon_names）
/// 2) 节拍遗漏(high)：locked_beats 红线关键词在叙事中完全未体现
/// 3) 人物出场(medium)：node.present_characters 应出场角色未在叙事中出现
/// 4) 章节目标偏离(medium)：章节 goals 关键词完全未体现
fn guard_narrative(
    full_text: &str,
    chapter_body: &str,
    known_names: &std::collections::HashSet<String>,
    present_names: &[String],
    locked_beats: &[String],
    chapter_goals: &[String],
    roster_names: Option<&[String]>,
) -> Vec<GuardViolation> {
    let mut violations = Vec::new();
    let full_ngrams: std::collections::HashSet<String> =
        guard_keywords(full_text).into_iter().collect();
    // ST-30 (2026-08-15 根治): 生成端角色清单优先——LLM 回合末尾自报出场人名，
    // 精确集合比对 known。根治「说话标记前切词」启发式的两个固有缺陷：
    //   漏报: 叙述/动作形态点名（「李铁柱的声音比雨声重」）无说话标记 → 检测不到
    //   误报: 切词切碎短语（「明日柜上/却带着点/亲恼羞成」）→ 堆词过滤打地鼠修不完
    // 清单存在 → 以其为准（不再跑切词启发式，避免其误报）；
    // 清单缺失/空 → 降级现有启发式（场景标签 + 引语切词），行为与修复前一致。
    if let Some(roster) = roster_names {
        for name in roster {
            let n = name.trim();
            if n.chars().count() >= 2 && !known_names.contains(n) && !chapter_body.contains(n) {
                violations.push(GuardViolation {
                    severity: GuardSeverity::High,
                    dim: "人物",
                    msg: format!("原著外对象「{}」（角色清单）", n),
                });
            }
        }
    } else {
        // 场景标签检测（<场景：…｜雨夜｜沈棠、林晚、王麻子>）——引语式检测
        // 只抓「XX说道」前的人名，场景标签/叙述里的原著外角色会漏检（实测王麻子案例）。
        // 提取「场景：」后顿号分隔的名字段，逐个与 known 对比。
        if let Some(tag) = full_text.find("<场景") {
            let seg = &full_text[tag..];
            if let Some(end) = seg.find('>') {
                let inner = &seg[..end];
                // 结构 <场景：地点｜天气/氛围｜角色、角色、角色> — 角色段在最后一个「｜」之后
                if let Some(sep) = inner.rfind('｜') {
                    for piece in inner[sep + 3..].split('、') {
                        let p = piece.trim();
                        let p = p.trim_matches(|c: char| c.is_whitespace() || c == '、');
                        // 2026-08-14 误报修复: 「温热的」「白的蓝」等形容词片段被误判为角色
                        // (1) 含 '的' → 形容词/所属结构，中文人名几乎不含此字;
                        // (2) known 角色名前缀 → 角色延伸（沈棠的伞/林晚的），非外角色
                        let known_prefix = known_names.iter().any(|k| !k.is_empty() && p.starts_with(k.as_str()));
                        if p.chars().count() >= 2
                            && !p.contains('的')
                            && !known_prefix
                            && !known_names.contains(p)
                        {
                            violations.push(GuardViolation {
                                severity: GuardSeverity::High,
                                dim: "人物",
                                msg: format!("场景标签出现原著外对象「{}」", p),
                            });
                        }
                    }
                }
            }
        }
        // 2026-08-16 统一来源根治: 退役切词启发式（detect_unknown_canon_names）——
        // 它从「XX说道」前切 2-3 字当角色名，结构上无法区分人名与短语碎片
        // （「跟您说/按理说/带着点…」被误判为外对象，实测「跟您」「按理」「带着点」误报）。
        // 角色判定只信统一来源: pack characters + LLM roster（<角色清单>）+ 场景标签 + LLM 兜底识别。
        // 宁漏勿瞎——真外对象由 LLM 语义识别兜底（roster 缺失时每 3 轮 LLM 兜底提取）。
    }
    for beat in locked_beats {
        let kws = guard_keywords(beat);
        if kws.is_empty() {
            continue;
        }
        let hit = kws.iter().any(|k| full_ngrams.contains(k));
        if !hit {
            violations.push(GuardViolation {
                severity: GuardSeverity::High,
                dim: "节拍",
                msg: format!("硬节拍「{}」未被体现", beat),
            });
        }
    }
    for name in present_names {
        let n = name.trim();
        if n.is_empty() || n.chars().count() < 2 {
            continue;
        }
        if !full_text.contains(n) {
            violations.push(GuardViolation {
                severity: GuardSeverity::Medium,
                dim: "出场",
                msg: format!("应出场角色「{}」未出现", n),
            });
        }
    }
    for goal in chapter_goals {
        let kws = guard_keywords(goal);
        if kws.is_empty() {
            continue;
        }
        let hit = kws.iter().any(|k| full_ngrams.contains(k));
        if !hit {
            violations.push(GuardViolation {
                severity: GuardSeverity::Medium,
                dim: "大纲",
                msg: format!("章节目标「{}」未体现", goal),
            });
        }
    }
    violations
}

// [ST-15 fix 2026-08-16] 提取【节点推进】marker 的节点 ID。
// 兼容两种写法：
//   1) 【节点推进:n2】   —— 冒号+节点ID在括号内（prompt 教 LLM 的格式）
//   2) 【节点推进】 n2   —— 节点ID在括号外（旧实现期望的格式）
// 旧实现用 rfind("【节点推进】") 匹配，遇到格式1 永远返回 -1，节点推进从未生效，
// 导致剧情一直卡在楔子节点（选项重复/场景原地打转的直接根因）。
// 返回 (marker 起始位置, 节点ID)；未找到返回 None。
fn extract_advance_marker(full_text: &str) -> Option<(usize, String)> {
    // 格式1: 【节点推进:n2】
    const INNER: &str = "【节点推进:";
    if let Some(pos) = full_text.rfind(INNER) {
        let after = &full_text[pos + INNER.len()..];
        let node_id: String = after
            .split(|c: char| c == '】' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_string();
        if !node_id.is_empty() {
            return Some((pos, node_id));
        }
    }
    // 格式2: 【节点推进】 n2
    const OUTER: &str = "【节点推进】";
    if let Some(pos) = full_text.rfind(OUTER) {
        let after = full_text[pos + OUTER.len()..].trim();
        let node_id = after.split(char::is_whitespace).next().unwrap_or("").to_string();
        if !node_id.is_empty() {
            return Some((pos, node_id));
        }
    }
    None
}

fn parse_option_list(after: &str) -> Vec<String> {
    let after = after.trim();
    if after.is_empty() {
        return Vec::new();
    }
    // [P11 2026-08-15] 入口级 JSON 泄漏检测：LLM 偶发把检定 JSON 对象裸写在正文，
    // 其键名（characterId/templateId/stateChanges/...）会被下方分支捞成「选项」。
    // 选项特征是短中文动作短语，绝不含这些结构键名——检测到即整组丢弃。
    const JSON_KEY_HINTS: [&str; 12] = [
        "characterId", "templateId", "stateChanges", "criticalSuccess",
        "memory_summary", "addTraits", "removeTraits", "outcomes",
        "fieldId", "actorId", "intent", "challenge",
    ];
    {
        let mut quoted: Vec<&str> = Vec::new();
        let mut rest = after;
        while let Some(q) = rest.find('"') {
            rest = &rest[q + 1..];
            if let Some(q2) = rest.find('"') {
                let s = rest[..q2].trim();
                if !s.is_empty() {
                    quoted.push(s);
                }
                rest = &rest[q2 + 1..];
            } else {
                break;
            }
        }
        // 引号串数量多（≥4）且任一含结构键名 → JSON 对象泄漏
        if quoted.len() >= 4 && quoted.iter().any(|s| JSON_KEY_HINTS.iter().any(|k| s.contains(k))) {
            return Vec::new();
        }
    }
    // Prefer JSON array (possibly followed by junk / code fences)
    if let Some(start) = after.find('[') {
        if let Some(end) = after.rfind(']') {
            if end > start {
                let slice = &after[start..=end];
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
                    let parsed: Vec<String> = arr
                        .into_iter()
                        .filter_map(|v| {
                            v.as_str()
                                .map(|s| s.trim().to_string())
                                .or_else(|| v.as_i64().map(|n| n.to_string()))
                        })
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !parsed.is_empty() {
                        return parsed;
                    }
                }
            }
        }
    }
    // Quoted strings anywhere
    {
        let mut out = Vec::new();
        let mut rest = after;
        while let Some(q) = rest.find('"') {
            rest = &rest[q + 1..];
            if let Some(q2) = rest.find('"') {
                let s = rest[..q2].trim();
                if !s.is_empty() {
                    out.push(s.replace("\\\"", "\""));
                }
                rest = &rest[q2 + 1..];
            } else {
                break;
            }
        }
        // [P11 2026-08-15] 防 JSON 对象污染：LLM 偶发把检定 JSON 对象（{"action":...}）
        // 裸写在正文，此分支会把所有键值对捞成「选项」（实踩 n=52）。
        // 选项特征 = 短（≤40 字）+ 不含 JSON 结构词（键名/嵌套值）。任一违反即判定为
        // JSON 泄漏，整组丢弃（宁可无选项，不给玩家一堆垃圾 chip）。
        const JSON_KEY_HINTS: [&str; 12] = [
            "characterId", "templateId", "stateChanges", "criticalSuccess",
            "memory_summary", "addTraits", "removeTraits", "outcomes",
            "fieldId", "actorId", "intent", "challenge",
        ];
        let looks_like_json_leak = out.len() >= 4
            && out
                .iter()
                .any(|s| JSON_KEY_HINTS.iter().any(|k| s.contains(k)));
        if !looks_like_json_leak && out.len() >= 2 {
            return out;
        }
    }
    let mut out = Vec::new();
    for line in after.lines() {
        let mut l = line.trim();
        if l.is_empty() {
            continue;
        }
        for prefix in ["- ", "• ", "* ", "– ", "· "] {
            if let Some(rest) = l.strip_prefix(prefix) {
                l = rest.trim();
                break;
            }
        }
        let stripped = l
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(['.', ')', '、', ':', '：', ' ', '\t'])
            .trim_start_matches('）')
            .trim_start_matches(')');
        if stripped.len() < l.len() {
            l = stripped.trim();
        }
        if l.is_empty() || l == "[" || l == "]" || l.starts_with('【') {
            continue;
        }
        out.push(l.to_string());
    }
    out
}

/// Split narrative vs player options. Always strips known markers from content.
/// Returns (clean_content, options).
fn split_options_from_narrative(full: &str) -> (String, Vec<String>) {
    let mut text = full.to_string();
    let mut options: Vec<String> = Vec::new();

    // 1) Canonical 【选项】
    const OPTS_MARKER: &str = "【选项】";
    if let Some(opts_start) = text.rfind(OPTS_MARKER) {
        let after = text[opts_start + OPTS_MARKER.len()..].trim();
        options = parse_option_list(after);
        text = text[..opts_start].trim_end().to_string();
    }

    // 1b) 【询问】停笔卡（吸收自梨园 ask_director）——同解析；允许无正文纯询问回合
    if options.is_empty() {
        const ASK_MARKER: &str = "【询问】";
        if let Some(opts_start) = text.rfind(ASK_MARKER) {
            let after = text[opts_start + ASK_MARKER.len()..].trim();
            options = parse_option_list(after);
            text = text[..opts_start].trim_end().to_string();
        }
    }

    // 2) <choices>...</choices>
    if options.is_empty() {
        let lower = text.to_ascii_lowercase();
        if let Some(start) = lower.rfind("<choices>") {
            if let Some(rel_end) = lower[start..].find("</choices>") {
                let inner_start = start + "<choices>".len();
                let inner_end = start + rel_end;
                let after_end = inner_end + "</choices>".len();
                let inner = text[inner_start..inner_end].trim();
                options = parse_option_list(inner);
                let mut kept = text[..start].to_string();
                if after_end < text.len() {
                    kept.push_str(text[after_end..].trim_start());
                }
                text = kept.trim_end().to_string();
            }
        }
    }

    // 3) Fallback: bare JSON array of 2–6 short strings near end (LLM forgot marker)
    if options.is_empty() {
        if let Some(start) = text.rfind('[') {
            if let Some(end) = text[start..].rfind(']') {
                let slice = &text[start..start + end + 1];
                let parsed = parse_option_list(slice);
                // Heuristic: look like choices (2..6 items, each short)
                if parsed.len() >= 2
                    && parsed.len() <= 6
                    && parsed.iter().all(|s| s.chars().count() <= 80)
                {
                    options = parsed;
                    text = text[..start].trim_end().to_string();
                }
            }
        }
    }

    (text, options)
}


/// 提取 agent 自建面板（【面板】JSON 块，吸收自梨园 panels.ts）。
/// Returns (clean_content, panels)。同名即更新；软上限 6。
fn split_panels_from_narrative(full: &str) -> (String, Vec<kaleido_core::TavernPanel>) {
    let mut text = full.to_string();
    let mut panels: Vec<kaleido_core::TavernPanel> = Vec::new();
    const PANEL_MARKER: &str = "【面板】";
    loop {
        let Some(mark) = text.find(PANEL_MARKER) else { break };
        let after = &text[mark + PANEL_MARKER.len()..];
        let Some(open) = after.find('{') else { break };
        let json_start = mark + PANEL_MARKER.len() + open;
        // 括号深度匹配（JSON 字符串内的花括号会误判——接受，SVG 一般不嵌花括号）
        let mut depth = 0i32;
        let mut close: Option<usize> = None;
        for (i, ch) in text[json_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(json_start + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close else { break };
        let json_slice = &text[json_start..end];
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_slice) {
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("markdown").to_string();
            let content = v.get("content").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if !name.is_empty() && !content.is_empty() {
                panels.push(kaleido_core::TavernPanel::new(name, kind, content));
            }
        }
        text = format!("{}{}", &text[..mark], &text[end..]);
    }
    panels.truncate(6); // 软上限（吸收自梨园 PANEL_SOFT_LIMIT）
    (text.trim().to_string(), panels)
}

/// ST-30: 提取回合正文末尾的 <角色清单>…</角色清单> 结构化块（生成端角色自报）。
///
/// 2026-08-15 根治方案：守卫不再依赖「说话标记前切词」启发式（漏报：叙述形态点名
/// 检测不到；误报：切词切碎短语「明日柜上/却带着点」，堆词过滤打地鼠修不完）。
/// 改为要求 LLM 在回合正文末尾自报出场人名清单，守卫做精确集合比对。
/// 返回 (clean_text, Option<Vec<String>>)：清单存在且非空 → Some(名字列表)；缺失/空 → None。
/// S7 改进① (2026-08-18): query 叠加当前节点剧情要点（标题 + locked_beats）。
/// 使久远关键剧情（承诺/约定/关系确立）在剧情推进到相关节点时能语义召回，
/// 而非只依赖"最近消息"——此前长间距关键事件因 query 无剧情导向而漏召回。
/// §14.7.2B (2026-08-18): 查 L4 affinity 中某条关系的数值（用于 L3 edges 渲染附 好感 N）。
/// L4 affinity 键格式：`charId`（玩家→该角色）或 `charId:targetId`（双向）。
/// from→to 关系匹配：优先精确 `from:to`，其次 `to:from`（affinity 可能是反向存），
/// 最后若 from 是玩家（无 target 单键 `to`）也命中。返回好感值。
pub fn affinity_for_edge(affinity: &serde_json::Value, from: &str, to: &str) -> Option<i64> {
    let obj = affinity.as_object()?;
    for key in [format!("{from}:{to}"), format!("{to}:{from}"), to.to_string()] {
        if let Some(v) = obj.get(&key).and_then(|v| v.as_i64()) {
            return Some(v);
        }
    }
    None
}

/// §13.5 Scene Gate (2026-08-18): 首回合场景错位检测（确定性地点校验，不靠语义）。
/// 从正文首行 <场景：地点｜…> 提取地点段；若含明显"后期地点"词（酒店/套房/机场/三亚/
/// 月见/海边/海滩/沙滩/度假村）且首回合（turn=1，节点应为开局场景）→ 判定错位。
/// 保守设计：只拦"明确写错到后续章节地点"的强信号；未知/学校/家里等地名一律放行（不误伤）。
pub fn is_scene_mismatch_location(text: &str) -> bool {
    let head = text.trim_start();
    let Some(open) = head.find("<场景") else { return false };
    let Some(colon) = head[open..].find('：').map(|i| open + i) else { return false };
    // ：是全角（3 字节），+1 会落在字符中间（UTF-8 越界 panic）；用 len_utf8 跳过整个字符
    let after = &head[colon + '：'.len_utf8()..];
    let loc: String = after
        .chars()
        .take_while(|c| *c != '｜' && *c != '|' && *c != '>')
        .collect::<String>()
        .trim()
        .to_string();
    if loc.is_empty() {
        return false; // 无地点段 → 不误拦
    }
    const LATE_LOCATION_WORDS: [&str; 10] = [
        "酒店", "套房", "机场", "三亚", "月见", "海边", "海滩", "沙滩", "度假村", "客房",
    ];
    LATE_LOCATION_WORDS.iter().any(|w| loc.contains(w))
}

/// §13.4① (2026-08-18): 玩家动作指令强制包装——短动作型消息加系统侧强调，
/// 让「环顾/打招呼」等明确动作必须在本回合真实发生（实证：度蜜月首回合"打招呼"被 LLM
/// 忽略成"追上她+她先笑"，选项-场景解耦）。保守触发：≤24 字 + 命中动作信号词，
/// 长叙述/自由输入不包装（避免过度干预）。
pub fn wrap_player_action(msg: &str) -> Option<String> {
    let m = msg.trim();
    if m.chars().count() > 24 {
        return None;
    }
    const ACTION_WORDS: [&str; 18] = [
        "环顾", "打招呼", "查看", "观察", "走向", "拿起", "尝试", "询问",
        "抱", "摸", "亲", "问", "看", "听", "站", "坐", "跟", "说",
    ];
    if !ACTION_WORDS.iter().any(|w| m.contains(w)) {
        return None;
    }
    Some(format!(
        "【玩家动作指令】\n{m}\n\n——这是玩家的明确动作。本回合叙事必须让该动作真实发生（打招呼须有真实问候/回应，环顾须描述当下场景的观察所得），不得跳过、弱化或替换为其他行为。"
    ))
}

/// §14.6① 非在场角色一行概要（身份锚点）：名字+分级+personality 前 60 字。
/// 用途：非在场角色保留存在感（防 LLM 忘掉全书角色），但不喂示例对白/心智模型等
/// 压制源（避免非在场角色声线污染当前叙事）。
pub fn character_summary_line(c: &kaleido_core::PackCharacterRef) -> String {
    let tier = match c.content_tier {
        Some(ContentTier::Safe) => " [全年龄]",
        Some(ContentTier::Standard) => " [标准]",
        Some(ContentTier::Open) => " [开放]",
        None => "",
    };
    let pers: String = c.personality.chars().take(60).collect();
    format!("- {}{}：{}（未在场）", c.name, tier, pers)
}

/// §14.6③ 按需召回压缩卡（非在场但玩家/剧情点名的角色）：概要+声线+界限 ≤300 字模板。
/// 让"提到但没信息"的角色有设定可依，但不铺开示例对白（省 token、防声线污染）。保留供后续
/// 更强召回（完整卡）用；当前按需召回附 character_summary_line 轻量概要。
#[allow(dead_code)]
pub fn character_compact_card(c: &kaleido_core::PackCharacterRef) -> String {
    let mut out = format!("**{}**：{}。{}", c.name, c.personality, c.speech_style);
    if !c.voice_profile.trim().is_empty() {
        out.push_str(&format!("\n声线：{}\n", c.voice_profile.trim()));
    }
    if !c.boundaries.is_empty() {
        out.push_str("\n界限：");
        for b in &c.boundaries {
            out.push_str(&format!("{b} "));
        }
    }
    out.chars().take(300).collect()
}

pub fn s7_attach_plot_scope(query: String, node: Option<(&str, &[String])>) -> String {
    let mut q = query;
    if let Some((title, beats)) = node {
        let mut scope = title.to_string();
        if !beats.is_empty() {
            scope.push_str("\n【剧情要点】");
            scope.push_str(&beats.join("；"));
        }
        if !scope.trim().is_empty() {
            q.push('\n');
            q.push_str(&scope);
        }
    }
    q
}

/// S7 改进② (2026-08-18): 注入标题按命中文本是否含剧情关键信号分级——
/// 含承诺/约定/答应/约好 → 硬约束语气（关键事实必须延续）；否则软参考（按需沿用）。
pub fn s7_recall_title(joined: &str) -> &'static str {
    const KEY_SIGNALS: [&str; 6] = ["承诺", "约定", "答应", "约好", "说好", "答应过"];
    if KEY_SIGNALS.iter().any(|s| joined.contains(s)) {
        "【历史回忆·关键约定/承诺（已确立事实，必须延续，不得遗忘或推翻）】"
    } else {
        "【历史回忆·混合检索命中（较早回合的对话细节，按需沿用）】"
    }
}

fn split_roster_from_narrative(full: &str) -> (String, Option<Vec<String>>) {
    const OPEN: &str = "<角色清单>";
    const CLOSE: &str = "</角色清单>";
    let mut text = full.to_string();
    let mut roster: Option<Vec<String>> = None;
    loop {
        let Some(open_pos) = text.find(OPEN) else { break };
        let Some(rel_close) = text[open_pos + OPEN.len()..].find(CLOSE) else { break };
        let inner_start = open_pos + OPEN.len();
        let inner_end = inner_start + rel_close;
        let inner = text[inner_start..inner_end].trim();
        let names: Vec<String> = inner
            .split(['、', '，', ',', '。', ' '])
            .map(|s| {
                s.trim()
                    .trim_matches(|c: char| c.is_whitespace() || matches!(c, '、' | '，' | ',' | '。'))
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();
        if !names.is_empty() {
            roster = Some(names);
        }
        let end = inner_end + CLOSE.len();
        text = format!("{}{}", &text[..open_pos], &text[end..]);
    }
    (text.trim().to_string(), roster)
}

/// 提取消息流内嵌程序卡（【程序】…【/程序】配对块, 吸收自梨园 show_html）。
/// Returns (clean_content, Some(html))。只取第一块, 其余按纯文本剥掉配对标记。
fn split_program_from_narrative(full: &str) -> (String, Option<String>) {
    const OPEN: &str = "【程序】";
    const CLOSE: &str = "【/程序】";
    let mut text = full.to_string();
    let mut program: Option<String> = None;
    if let Some(start) = text.find(OPEN) {
        let body_start = start + OPEN.len();
        if let Some(rel) = text[body_start..].find(CLOSE) {
            let body_end = body_start + rel;
            let html = text[body_start..body_end].trim().to_string();
            if !html.is_empty() {
                program = Some(html);
                // 剥离整块(含标记)
                let after = body_end + CLOSE.len();
                text = format!("{}{}", &text[..start], &text[after..]);
            }
        }
    }
    (text.trim().to_string(), program)
}

/// 重复键状态对象拆分：LLM 偶发把多角色状态合并进单个对象
/// （如 {"characterId":"A","fields":{...},"characterId":"B","fields":{...}}），
/// serde 对重复字段报错导致整块解析失败残留正文。
/// 按顶层 "characterId" 键切段、逐段包回 {}，供上层逐段解析；非重复键形态返回空 Vec。
fn split_dupe_state_objects(s: &str) -> Vec<String> {
    let t = s.trim();
    if !t.starts_with('{') || !t.ends_with('}') {
        return Vec::new();
    }
    let inner = &t[1..t.len() - 1];
    let key = "\"characterId\"";
    // 定位每个顶层 "characterId" 键的起始偏移（跳过字符串内的假匹配）
    let mut marks: Vec<usize> = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut i = 0usize;
    while i < inner.len() {
        let c = inner[i..].chars().next().unwrap();
        let w = c.len_utf8();
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else {
            match c {
                '"' => {
                    if depth == 0 && inner[i..].starts_with(key) {
                        marks.push(i);
                    }
                    in_str = true;
                }
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        i += w;
    }
    if marks.len() < 2 {
        return Vec::new();
    }
    // 每段：段0 从对象头（含 characterId 前的杂键），后续段从该键起，到下一键前止，包回 {}
    let mut segs: Vec<String> = Vec::new();
    for (k, &m) in marks.iter().enumerate() {
        let start = if k == 0 { 0 } else { m };
        let end = if k + 1 < marks.len() { marks[k + 1] } else { inner.len() };
        // 段尾可能残留分隔逗号（段0 切到下一键时带上），去掉后再包 {}
        let seg = inner[start..end].trim().trim_end_matches(',');
        if !seg.is_empty() {
            segs.push(format!("{{{}}}", seg));
        }
    }
    segs
}

/// ST-26: 提取【状态更新】JSON 块（括号深度匹配，同【面板】）。
/// 成功解析且 character_id 非空的块收集并剥离；解析失败/character_id 为空时跳过该块但保留原文。
/// 软上限 20 块。
/// [fix §10 2026-08-16] 关键物品/承诺特征识别：所有权/收藏/承诺语义的事实入 L3 永久层
/// （窝边草「素描原稿被向明初收藏」即此类——此前被 take(6) 挤出导致失忆）。
fn is_key_fact(text: &str) -> bool {
    const MARKERS: [&str; 18] = [
        "素描",
        "原稿",
        "画作",
        "收藏",
        "画好了",
        "亲手画",
        "承诺",
        "答应",
        "约定",
        "发誓",
        "保证",
        "送给",
        "赠予",
        "收到",
        "留在",
        "拥有",
        "贴身",
        "存着",
    ];
    MARKERS.iter().any(|m| text.contains(m))
}

/// [fix §10 2026-08-16] 章节剧情摘要提炼兜底：LLM 偶发输出「复述任务+思考」而非纯文本
/// （deepseek-v4-flash-free 指令遵循差）。此处复用 B2 的清洗链（剥 think/围栏/指令行），
/// 并附加指令回声截断（C7 已修）。
fn split_state_updates_from_narrative(full: &str) -> (String, Vec<kaleido_core::ActorStateUpdate>) {
    let mut clean = String::new();
    let mut updates: Vec<kaleido_core::ActorStateUpdate> = Vec::new();
    const STATE_MARKER: &str = "【状态更新】";
    let mut rest = full;
    loop {
        let Some(mark) = rest.find(STATE_MARKER) else {
            clean.push_str(rest);
            break;
        };
        clean.push_str(&rest[..mark]);
        let after = &rest[mark + STATE_MARKER.len()..];
        let Some(open) = after.find('{') else {
            clean.push_str(&rest[mark..]);
            break;
        };
        let json_start = mark + STATE_MARKER.len() + open;
        // 括号深度匹配（JSON 字符串内的花括号会误判——接受，状态值一般不嵌花括号）
        let mut depth = 0i32;
        let mut close: Option<usize> = None;
        for (i, ch) in rest[json_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(json_start + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close else {
            clean.push_str(&rest[mark..]);
            break;
        };
        match serde_json::from_str::<kaleido_core::ActorStateUpdate>(&rest[json_start..end]) {
            Ok(u) if !u.character_id.is_empty() => {
                // 成功解析：剥离整块并收集
                updates.push(u);
            }
            _ => {
                // 解析失败/character_id 为空：尝试重复键拆分（LLM 偶发把多角色状态
                // 合并进单对象致 serde 重复字段报错）；全部子段可解析才剥离整块，否则保留原文
                let dupes = split_dupe_state_objects(&rest[json_start..end]);
                let parsed: Vec<kaleido_core::ActorStateUpdate> = dupes
                    .iter()
                    .filter_map(|seg| {
                        serde_json::from_str::<kaleido_core::ActorStateUpdate>(seg)
                            .ok()
                            .filter(|u| !u.character_id.is_empty())
                    })
                    .collect();
                if !parsed.is_empty() && parsed.len() == dupes.len() {
                    updates.extend(parsed);
                } else {
                    // [fix §9 2026-08-16] 解析失败也剥离：带【状态更新】标记的畸形块
                    // 正文零残留优先（状态不应用 < 污染正文），warn 记录原文供排查。
                    tracing::warn!(
                        raw = %&rest[json_start..end].chars().take(200).collect::<String>(),
                        "st state block unparseable; stripped from narrative"
                    );
                }
            }
        }
        rest = &rest[end..];
        if updates.len() >= 20 {
            clean.push_str(rest);
            break;
        }
    }

    // [fix 2026-08-16] 无标记裸 JSON 状态块剥离：LLM 偶发在正文末尾直接追加
    // {"characterId":...} 对象而未带【状态更新】标记，标记匹配不到会残留进正文。
    // 扫描 clean 中非标记开头的 JSON 对象，可解析为 ActorStateUpdate 且 character_id
    // 非空则剥离（结构特征极特定，正文叙事几乎不可能自然出现，误伤风险低）。
    let mut bare_clean = String::new();
    let mut bare_rest = clean.as_str();
    loop {
        if bare_rest.trim().is_empty() {
            bare_clean.push_str(bare_rest);
            break;
        }
        let Some(open) = bare_rest.find('{') else {
            bare_clean.push_str(bare_rest);
            break;
        };
        let prefix = &bare_rest[..open];
        // 仅当 { 处于行首（串开头或前一个字符是换行）才视为独立状态块；
        // 否则整段（含 {）作为正文内容保留，避免误删正文内嵌花括号或保留的标记块。
        if !(prefix.is_empty() || prefix.ends_with('\n')) {
            bare_clean.push_str(&bare_rest[..open + 1]);
            bare_rest = &bare_rest[open + 1..];
            continue;
        }
        // 括号深度匹配 JSON 对象
        let mut depth = 0i32;
        let mut close: Option<usize> = None;
        for (i, ch) in bare_rest[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close else {
            bare_clean.push_str(bare_rest);
            break;
        };
        // 行首 JSON：尝试解析为状态块
        if let Ok(u) = serde_json::from_str::<kaleido_core::ActorStateUpdate>(&bare_rest[open..end]) {
            if !u.character_id.is_empty() {
                // 独立状态块：保留行首正文 prefix，剥离 JSON 对象本身
                if !prefix.is_empty() {
                    bare_clean.push_str(prefix);
                }
                updates.push(u);
            } else {
                // character_id 为空：保留整块
                bare_clean.push_str(&bare_rest[..end]);
            }
        } else {
            // 解析失败：尝试重复键拆分（LLM 偶发把多角色状态合并进单对象）
            let dupes = split_dupe_state_objects(&bare_rest[open..end]);
            let parsed: Vec<kaleido_core::ActorStateUpdate> = dupes
                .iter()
                .filter_map(|seg| {
                    serde_json::from_str::<kaleido_core::ActorStateUpdate>(seg)
                        .ok()
                        .filter(|u| !u.character_id.is_empty())
                })
                .collect();
            if !parsed.is_empty() && parsed.len() == dupes.len() {
                if !prefix.is_empty() {
                    bare_clean.push_str(prefix);
                }
                updates.extend(parsed);
            } else {
                // [fix §9 2026-08-16] 行首独立 JSON 但解析失败：结构特征已极特定
                // （行首 { 对象），正文零残留优先——剥离 + warn（与带标记路径一致）。
                tracing::warn!(
                    raw = %bare_rest[open..end].chars().take(200).collect::<String>(),
                    "st bare state block unparseable; stripped from narrative"
                );
            }
        }
        bare_rest = &bare_rest[end..];
    }
    (bare_clean.trim().to_string(), updates)
}

/// ST-27: 提取【检定】JSON 块（括号深度匹配，同【状态更新】）。
/// 成功解析且 action 非空的块收集并剥离；解析失败/action 为空时跳过该块但保留原文。
/// 软上限 5 块。
fn split_check_from_narrative(full: &str) -> (String, Vec<kaleido_core::TurnCheckRequest>) {
    let mut clean = String::new();
    let mut checks: Vec<kaleido_core::TurnCheckRequest> = Vec::new();
    const CHECK_MARKER: &str = "【检定】";
    let mut rest = full;
    loop {
        let Some(mark) = rest.find(CHECK_MARKER) else {
            clean.push_str(rest);
            break;
        };
        clean.push_str(&rest[..mark]);
        let after = &rest[mark + CHECK_MARKER.len()..];
        let Some(open) = after.find('{') else {
            clean.push_str(&rest[mark..]);
            break;
        };
        let json_start = mark + CHECK_MARKER.len() + open;
        // 括号深度匹配（JSON 字符串内的花括号会误判——接受，检定值一般不嵌花括号）
        let mut depth = 0i32;
        let mut close: Option<usize> = None;
        for (i, ch) in rest[json_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(json_start + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close else {
            clean.push_str(&rest[mark..]);
            break;
        };
        match serde_json::from_str::<kaleido_core::TurnCheckRequest>(&rest[json_start..end]) {
            Ok(c) if !c.action.trim().is_empty() => {
                // 成功解析：剥离整块并收集
                checks.push(c);
            }
            _ => {
                // 解析失败/action 为空：跳过该块（不中断、不报错），但保留原文
                clean.push_str(&rest[mark..end]);
            }
        }
        rest = &rest[end..];
        if checks.len() >= 5 {
            clean.push_str(rest);
            break;
        }
    }
    (clean.trim().to_string(), checks)
}

/// S4 (吞噬 denova director_plan): 导演计划 LLM 输出块。
/// LLM 按 ST-26 指令输出「【导演计划】{...json...}」或「【导演计划】none」。
#[derive(Debug)]
enum DirectorPlanUpdate {
    /// 【导演计划】none —— 无需更新现有计划
    Skip,
    /// 解析出的新导演计划
    Set(DirectorPlanOutput),
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DirectorPlanOutput {
    #[serde(default)]
    goal: String,
    #[serde(default)]
    pressure: Option<String>,
    #[serde(default)]
    cost: Option<String>,
    /// 任务书约定 LLM 输出 snake_case `hits_beats`；camelCase 也兼容。
    #[serde(default, alias = "hits_beats")]
    hits_beats: Vec<String>,
}

/// 合并思维段到 reasoning 缓冲（去空、双换行分隔）。
fn append_thinking(dst: &mut String, src: &str) {
    let src = src.trim();
    if src.is_empty() {
        return;
    }
    if !dst.is_empty() {
        dst.push_str("\n\n");
    }
    dst.push_str(src);
}

/// [fix 2026-08-15 结构根治] 思维自动折叠——fix 阶段输出结构化剥离。
/// fix prompt 约定：分析过程放 <thinking>…</thinking> 块、正文放 <story>…</story> 块。
/// 本函数按标签剥离（机器可解析，不依赖措辞猜测，根治堆关键词打地鼠——
/// 旧规则靠 <场景 标签/措辞前缀识别，fix 思维无标签则剥不掉，实踩泄漏）。
/// 返回剥离出的思维段；body 原地改写为纯正文（含 <story> 标签剥除）。
/// 无任何标签时返回空串，调用方 fallback 到 strip_heavy_fix_preamble（兜底旧模型无标签输出）。
fn strip_fix_thinking_blocks(body: &mut String) -> String {
    let mut work = body.trim().to_string();
    let mut thinking = String::new();
    // 剥离所有 <thinking>…</thinking> 块（可能多个）
    loop {
        let Some(open) = work.find("<thinking>") else { break };
        let Some(close_rel) = work[open + "<thinking>".len()..].find("</thinking>") else {
            // 有开无闭：标签残片污染，自开标签处截断（保留标签前内容）
            work.truncate(open);
            break;
        };
        let inner = work[open + "<thinking>".len()..open + "<thinking>".len() + close_rel].trim();
        append_thinking(&mut thinking, inner);
        // 剥离 thinking 块时连其前的紧邻换行一起删（thinking 通常独占段落），
        // 避免残留双换行把正文段落撑出空行（测试实踩：前文\n\n中间）。
        let cut_start = if open > 0 && work.as_bytes()[open - 1] == b'\n' {
            open - 1
        } else {
            open
        };
        work = format!(
            "{}{}",
            &work[..cut_start],
            &work[open + "<thinking>".len() + close_rel + "</thinking>".len()..]
        );
    }
    // <story>…</story> 包裹时取块内内容（剥标签）；标签前若有非 thinking 的普通
    // 文本（模型偶发把部分正文写在 <story> 外），作为正文前缀保留，避免丢正文。
    if let Some(open) = work.find("<story>") {
        let after_open = &work[open + "<story>".len()..];
        if let Some(close_rel) = after_open.find("</story>") {
            let inner = after_open[..close_rel].trim();
            let before = work[..open].trim();
            let mut merged = String::new();
            if !before.is_empty() {
                merged.push_str(before);
            }
            if !inner.is_empty() {
                if !merged.is_empty() {
                    merged.push('\n');
                }
                merged.push_str(inner);
            }
            *body = merged;
            return thinking;
        } else {
            // [fix 2026-08-15] <story> 开标签无 </story> 闭合（正文被 max_tokens
            // 截断/流中断时实踩）：同样剥掉开标签，保留其后内容作正文，
            // 避免「<story> 烛火在茶几上跳着…」标签残片泄漏进用户可见正文。
            let before = work[..open].trim();
            let inner = after_open.trim();
            let mut merged = String::new();
            if !before.is_empty() {
                merged.push_str(before);
            }
            if !inner.is_empty() {
                if !merged.is_empty() {
                    merged.push('\n');
                }
                merged.push_str(inner);
            }
            *body = merged;
            return thinking;
        }
    }
    // [P2 2026-08-15] 尾部元话语段截断：fix 阶段模型常在正文末尾追加
    // 「让我检查/再检查/让我重新组织/让我起草/让我输出最终版本」等自指段
    // （实踩 turn72 [13] 尾部：「让我检查…」×4、「---」×3、自我问答式审稿）。
    // 从最后一个自指段起点截断；若截断后正文以分隔线结尾则一并剔除。
    work = strip_trailing_metadiscourse(work);
    *body = work.trim().to_string();
    thinking
}

/// [P2 2026-08-15] 剥离 fix 输出尾部的元话语段（自指动作 + 审稿清单特征）。
/// 从正文末尾向前找「让我检查/让我重新组织/让我起草/让我输出最终版本/再检查/让我分析审稿意见」等
/// 段起点，截断其后所有内容。多段 `---` 拼接时取最后一段正文（若最后一段是元话语则继续前溯）。
fn strip_trailing_metadiscourse(work: String) -> String {
    const SELF_REF_MARKERS: [&str; 35] = [
        "让我检查",
        "让我再检查",
        "让我重新组织",
        "让我重新设计",
        "让我起草",
        "让我看看",
        "让我输出最终版本",
        "让我分析审稿意见",
        "再检查是否需要",
        "再检查一下",
        "好，让我输出",
        "让我逐条",
        // [fix 2026-08-16 档位全覆盖] deepseek-v4-flash-free 正文后自检变体
        // （实踩 msg35：「嗯，这个版本不错。让我检查一下：1. 时间…✓」+
        // 状态更新思考「让我写状态更新块：…我应该更新哪个？…」）——
        // 段首不是 markers 而是「嗯，/等等/不过」等引导词，故改段内包含匹配。
        "这个版本不错",
        "让我检查一下",
        "让我写状态更新",
        "让我更新",
        "我应该更新哪个",
        "让我看看：",
        "让我看看,",
        "我不确定哪个是权威",
        "让我都更新",
        "让我写最终版本",
        "让我安排好顺序",
        // [fix 2026-08-16 实踩]「嗯，这样应该可以。现在让我把完整的叙事写出来。」
        // +「还要注意：- 正文末尾需要角色清单」——正文后自检的「让我把」变体。
        "让我把完整的叙事",
        "让我把最终",
        "让我把正文",
        "还要注意",
        "正文末尾需要",
        "现在让我把",
        "这样应该可以",
        "接下来还要",
        // [fix 2026-08-16 实踩 21:47] 时间推进规则复述：「在正文末尾标注，因为我
        // 在正文中间已经过夜了。嗯……规则说…」——LLM 解释为什么不在末尾标注
        // [时间推进]，属尾部自检思考，旧表漏判。
        "在正文末尾标注",
        "因为我", "嗯……", "规则说",
    ];
    // 分段过滤：按 \n\n 拆段，删除「含自指标记的段」与「审稿清单段」，
    // 重组剩余段。仅当正文含多段且至少一段是元话语时介入（单段正文不误伤）。
    let segments: Vec<&str> = work.split("\n\n").collect();
    if segments.len() < 2 {
        // 单段：整段含自指标记 → 纯自检无正文
        let t = work.trim_start();
        if SELF_REF_MARKERS.iter().any(|m| t.contains(m)) {
            return String::new();
        }
        return work;
    }
    let kept: Vec<&str> = segments
        .iter()
        .filter(|seg| {
            let line = seg.trim_start();
            let is_meta = SELF_REF_MARKERS.iter().any(|m| line.contains(m))
                || line.starts_with("问题")
                || line.starts_with('✅')
                || line.starts_with('❌')
                || line.starts_with("出现了")
                // [fix 2026-08-16] 状态更新草案段（正文后「状态更新：xxx」= 模型对状态块的
                // 思考草稿而非正文；正式【状态更新】块由 split_state_updates_from_narrative 处理）
                || line.starts_with("状态更新");
            !is_meta
        })
        .copied()
        .collect();
    // 若过滤后只剩 1 段或全部保留，说明无元话语 → 原样（防误伤）
    if kept.len() == segments.len() || kept.is_empty() {
        return work;
    }
    let mut out = kept.join("\n\n").trim().to_string();
    // 剥掉残留的分隔线段（--- / *** / ===）
    let segs2: Vec<&str> = out.split("\n\n").collect();
    let kept2: Vec<&str> = segs2
        .iter()
        .filter(|s| {
            let t = s.trim();
            t != "---" && t != "***" && t != "===" && !t.is_empty()
        })
        .copied()
        .collect();
    out = kept2.join("\n\n").trim().to_string();
    out
}

/// S4: 提取【导演计划】块（同【检定】括号深度匹配骨架）。
/// 无块 → None；「【导演计划】none」→ Some(Skip)（剥离块）；JSON 且 goal 非空 → Some(Set)（剥离块）。
/// 解析失败 / goal 为空 → 保留原文不剥离。
fn strip_director_preamble(body: &mut String) -> String {
    // 剥离正文开头的「导演自白」段（模型偶发泄漏的内部推理，TEN_ROUND_PLOT_VERIFY
    // R4/R8 模式：「好的，我是XX…我要怎么演…」直到第一个 `<场景` 标签前）。
    // 仅当 body 以明确自白前缀开头且存在 `<场景` 边界时才剥离，避免误伤正常叙事。
    // 返回剥离出的自白段；body 原地更新为剩余正文。
    let trimmed_head = body.trim_start();
    // 自白前缀：「好的，」「好的我」「我是」（导演式自白开头；实测模式含
    // 「好的，我是林逸」「好的，我正坐在妈妈身边」「好的，玩家扮演林逸」，
    // 统一以「好的，」覆盖所有变体）。
    let is_preamble = trimmed_head.starts_with("好的，")
        || trimmed_head.starts_with("好的,")
        || trimmed_head.starts_with("好的我")
        || trimmed_head.starts_with("我是");
    if !is_preamble {
        return String::new();
    }
    // 需要 `<场景` 标签作为正文起点边界（无标签时保守不剥，避免吞掉整个正文）
    let Some(scene_mark) = trimmed_head.find("<场景") else {
        return String::new();
    };
    if scene_mark == 0 {
        return String::new();
    }
    let preamble = trimmed_head[..scene_mark].trim().to_string();
    let rest = trimmed_head[scene_mark..].to_string();
    *body = rest;
    preamble
}

/// [fix 2026-08-16] Lite 档思维链泄漏剥离。Lite 直出不走 run_quality_refine 多轮
/// 管道，Standard/Heavy 的 fix 阶段剥离（strip_fix_thinking_blocks）对 lite 完全不
/// 生效（`TurnQuality::Lite => Ok((draft.to_string(), None))` 裸返回），历史修复
/// （360bf20 等）只补在 multi-round 管道函数上，导致默认档位 lite 的推理泄漏直落正文。
/// 实测变体（TURN4）：长剧情快速推进时模型输出无 `<场景` 标签的章节规划推理，
/// 以「根据章节大纲/当前节点/出口是…」开头，正文被推理段包裹且重复多次。
/// 策略：仅当输出无 `<场景` 标签且含明显推理前缀时介入，按 `\n\n` 分段，用
/// 负向前缀剔除推理段，取「最长连续叙事段组」作为正文（正文在推理中被重复多次，
/// 最长连续组即干净正文）。正常结构化回合（含 `<场景`）不误伤。
fn strip_lite_reasoning_leak(draft: String) -> String {
    let head = draft.trim_start();
    // 正常结构化回合以 <场景 标签开头 → 不介入（避免误伤）。
    if head.starts_with("<场景") {
        return draft;
    }
    // 推理/规划段负向前缀：真正的叙事段不以这些开头。
    const NEG_PREFIX: [&str; 66] = [
        "根据", "让我", "嗯，", "嗯,", "也许", "考虑到", "实际上", "回顾", "重新阅读",
        "所以", "因此", "那么", "但指令", "鉴于", "我应该", "我不应该", "然后提供",
        "角色清单", "然后选项", "这样应该", "等等", "好的，", "好的,", "当前节点",
        "出口是", "节点", "这些选项", "它处理", "或者", "故事继续", "看看", "让我们",
        "在亲密时刻之后", "待我",
        // [fix 2026-08-16] flash-free 思考混入 content 的新变体：reasoning_content
        // 只装第一段思考，后续「我需要/我要用/我的目标」等规划段混进 content，
        // 旧表缺这些前缀 → leak 判 false → 思考链原样保留为「正文」。
        "我需要", "现在我需要", "接下来我要", "接下来我", "我要用", "我要先", "我想先",
        "我的目标", "我的想法是", "好，我来", "好，我", "开始写正文", "正文如下", "下面开始",
        "让我来", "让我现在", "首先，", "首先,", "然后我", "接着我", "我先",
        // [fix 2026-08-16] 尾部自检变体：「最后，让我确认/最后让我确认一下」——
        // 以「最后，」开头不以「让我」开头，旧表漏判 → 尾部思考混入正文。
        "最后，让我", "最后让我", "最后，我", "最后我",
        // [fix 2026-08-16 实踩 21:23] 「好，让我写正文。」——以「好，让我」开头
        // 匹配不到「好，我」（第二个字是「让」非「我」）也匹配不到「让我」（段首
        // 是「好」），旧表漏判 → 思考残留正文首段。
        "好，让我", "好，我现在", "好，我需要",
        // [fix 2026-08-16 实踩 21:35] 「好，最终正文：」——交付前的宣告段，
        // 与「好，让我写正文」同族但以「好，最终」开头，补全兜底。
        "好，最终正文", "好，以下是最终", "好，下面是正文", "好，正文如下",
    ];
    // 判定是否真的泄漏：首段命中推理前缀（无标签 + 推理开头 → 高度疑似泄漏）。
    let first = head.split("\n\n").next().unwrap_or("").trim();
    let leak = NEG_PREFIX.iter().any(|p| first.starts_with(p));
    if !leak {
        return draft;
    }
    // 按段切分，标记叙事段，求最长连续叙事段组。
    let segments: Vec<&str> = head.split("\n\n").map(|s| s.trim()).collect();
    let mut best: Vec<&str> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for seg in segments {
        // [fix 2026-08-16 剥离误杀] 中文正文常含短句段（「走过去。」4字、
        // 「没有说话。」5字、对话单行「“妈。”」），旧阈值 len>=10 把这些
        // 正常叙事判为思考段 → 整篇正文被剥光（实踩 21:05 回合 content 只剩
        // 42 字思考尾）。阈值降至 2（仅过滤空段/单字语气词），NEG 前缀仍过滤
        // 真正的思考/规划段。
        let is_narr = seg.len() >= 2 && !NEG_PREFIX.iter().any(|p| seg.starts_with(p));
        if is_narr {
            cur.push(seg);
            if cur.len() > best.len() {
                best = cur.clone();
            }
        } else {
            cur.clear();
        }
    }
    // 没有找到足够叙事段 → 保守返回原文（避免吞掉整个正文）。
    if best.len() < 2 {
        return draft;
    }
    best.join("\n\n").trim().to_string()
}

/// [修复 2026-08-15] Heavy 管道 fix 阶段思维前缀剥离。
/// 实测泄漏模式：「好的，我需要根据审稿意见修订这段正文。让我逐条分析…」
/// 「好的，我需要根据审稿意见修订这段正文。让我仔细分析每条审稿意见…」
/// 这类 fix 思维没有 `<场景` 标签，strip_director_preamble 的保守规则不会剥。
/// 识别「好的，我需要…审稿/修订/让我…」前缀，剥离思维段 + 审稿意见列表，
/// 直到遇到真正的叙事段。仅在确有审稿/修订思维特征时剥离（避免误伤正常
/// 「好的，」开头叙事）。
fn strip_heavy_fix_preamble(body: &mut String) -> String {
    let trimmed = body.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let is_fix_thought = lower.starts_with("好的，我需要根据审稿")
        || lower.starts_with("好的，我需要根据审稿意见")
        || lower.starts_with("好的，让我仔细")
        || lower.starts_with("好的，我来")
        || lower.starts_with("好的，先")
        || lower.starts_with("好的，根据审稿")
        || lower.starts_with("用户要求我")
        || lower.starts_with("根据审稿意见")
        // [fix 2026-08-15] 无标签思维新变体（部署后实踩 msg7「让我想想。前文是…」、
        // msg5「让我重写正文： --- 」）——模型未按 <thinking> 标签输出时泄漏。
        || lower.starts_with("让我想想")
        || lower.starts_with("让我重写")
        || lower.starts_with("让我重新")
        || lower.starts_with("让我梳理")
        || lower.starts_with("我需要根据")
        || lower.starts_with("我需要重新")
        || (lower.starts_with("好的，") && lower.contains("审稿"))
        || (lower.starts_with("好的，") && lower.contains("修订这段正文"))
        || (lower.contains("审稿意见") && (lower.contains("minor") || lower.contains("major") || lower.contains("修订")))
        || (lower.starts_with('6') && lower.contains("minor"))
        || (lower.starts_with('7') && lower.contains("minor"))
        || (lower.starts_with('8') && lower.contains("minor"))
        || (lower.starts_with('9') && lower.contains("minor"));
    if !is_fix_thought {
        return String::new();
    }
    // 逐段剥离：思维段、审稿意见列表（含编号/标题/引用）都算思维前缀，
    // 直到遇到普通叙事段（不以编号/破折号/引号行首开头的段落）。
    let mut rest = trimmed;
    let mut thought = String::new();
    loop {
        let Some(para_end) = rest.find("\n\n") else {
            // 无双换行边界：若已剥出思维段，剩余 rest 即正文；否则（首段即
            // 无换行）无法安全区分 → 保守不剥。
            if thought.is_empty() {
                return String::new();
            }
            *body = rest.to_string();
            return thought;
        };
        let para = rest[..para_end].trim();
        let after = rest[para_end + 2..].trim_start();
        let para_is_thought = para.starts_with("好的，")
            || para.starts_with("审稿意见")
            || para.starts_with("修改意见")
            || para.starts_with('1')
            || para.starts_with('2')
            || para.starts_with('3')
            || para.starts_with('4')
            || para.starts_with('5')
            || para.starts_with('6')
            || para.starts_with('7')
            || para.starts_with('8')
            || para.starts_with('9')
            || para.starts_with('-')
            || para.starts_with('*')
            || para.starts_with("**")
            || para.starts_with('“')
            || para.starts_with('「')
            || para.starts_with("然后")
            || para.starts_with("接下来")
            || para.starts_with("首先")
            || para.starts_with("最后")
            || para.starts_with("让我重新设计")
            || para.starts_with("让我重新")
            || para.starts_with("让我写")
            || para.starts_with("让我想想")
            || para.starts_with("让我重写")
            || para.starts_with("让我梳理")
            || para.starts_with("我需要在")
            || para.starts_with("情绪曲线")
            || para.starts_with("场景")
            || para.starts_with("正文")
            || para.starts_with("修改后")
            || para.starts_with("修订后")
            || para.contains("意见")
            || para.contains("修订")
            || para.contains("审稿");
        if !para_is_thought {
            // 遇到叙事段：思维剥离到此为止，正文从当前段起
            if thought.is_empty() {
                return String::new();
            }
            *body = rest.to_string();
            return thought;
        }
        if thought.is_empty() {
            thought = para.to_string();
        } else {
            thought = format!("{}\n\n{}", thought, para);
        }
        rest = after;
    }
}

/// [fix 2026-08-15] Heavy 管道 fix 尾部自检剥离（部署后实踩 msg5「--- 好，这个版本可以输出。
/// 让我再检查一下角色清单：母亲、我。」、msg7/msg11「再检查是否需要【检定】…」尾部泄漏）。
/// 模型在正文末尾追加自我检查/质量确认段（无 <thinking> 标签），剥离后归入 reasoning。
/// 返回剥离出的尾部思维段；body 原地改写为纯正文（尾部截断）。未匹配时返回空串不动正文。
fn strip_heavy_fix_tail(body: &mut String) -> String {
    let markers: [&str; 8] = [
        "让我再检查",
        "让我再确认",
        "再检查是否需要",
        "再确认是否需要",
        "好，这个版本可以输出",
        "好，这个版本已经可以输出",
        "让我输出最终版本",
        "让我最终输出",
    ];
    // 只在正文中后段匹配（防止误伤开头正常叙事里的罕见短语）；取最后一个匹配点。
    // [fix] 精确规则：marker 必须位于正文最后一段（最后一个 \n\n 之后）——自检段
    // 总是以独立段落出现在文末；单段正文则要求位于中后段。
    let trimmed = body.trim_end().to_string();
    let last_para_start = trimmed.rfind("\n\n").map(|p| p + 2).unwrap_or(0);
    let mut cut: Option<usize> = None;
    for m in markers.iter() {
        let mut search_from = last_para_start;
        while let Some(rel) = trimmed[search_from..].find(m) {
            let abs = search_from + rel;
            if last_para_start == 0 && abs < trimmed.len() / 2 {
                // 单段正文：只接受位于中后段的匹配
                search_from = abs + m.len();
                continue;
            }
            cut = Some(cut.map_or(abs, |c| c.max(abs)));
            break;
        }
    }
    let Some(cut_pos) = cut else {
        return String::new();
    };
    // 从匹配点前最近的分段边界（--- 或双换行）开始截，若匹配点紧跟在 --- 后
    // （msg5 实踩：「--- 好，这个版本可以输出。」）则连分隔线一起剥。
    let mut start = cut_pos;
    let before = &trimmed[..cut_pos];
    if let Some(dash_rel) = before.rfind("---") {
        let dash_abs = dash_rel;
        // --- 与匹配点之间无正文（只隔空白/换行）时，从 --- 起剥
        if before[dash_abs + 3..].trim().is_empty() || before[dash_abs + 3..].trim().chars().count() <= 20 {
            start = dash_abs;
        }
    }
    let tail = trimmed[start..].trim();
    let head = trimmed[..start].trim_end();
    if tail.is_empty() || head.is_empty() {
        return String::new();
    }
    *body = head.to_string();
    tail.to_string()
}

fn split_director_plan_from_narrative(full: &str) -> (String, Option<DirectorPlanUpdate>) {
    const PLAN_MARKER: &str = "【导演计划】";
    let Some(mark) = full.find(PLAN_MARKER) else {
        return (full.trim().to_string(), None);
    };
    let before = full[..mark].trim_end();
    let after = &full[mark + PLAN_MARKER.len()..];
    let trimmed = after.trim_start();
    let lead = after.len() - trimmed.len();

    // 【导演计划】none —— 无需更新
    if trimmed.starts_with("none") {
        let rest = trimmed["none".len()..].trim_start();
        let text = if before.is_empty() {
            rest.to_string()
        } else {
            format!("{before}\n{rest}")
        };
        return (text.trim().to_string(), Some(DirectorPlanUpdate::Skip));
    }

    // 括号深度匹配 JSON（同【状态更新】）
    let Some(open) = trimmed.find('{') else {
        return (full.trim().to_string(), None);
    };
    let json_start = mark + PLAN_MARKER.len() + lead + open;
    let mut depth = 0i32;
    let mut close: Option<usize> = None;
    for (i, ch) in full[json_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(json_start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = close else {
        return (full.trim().to_string(), None);
    };
    match serde_json::from_str::<DirectorPlanOutput>(&full[json_start..end]) {
        Ok(out) if !out.goal.trim().is_empty() => {
            let after_block = full[end..].trim_start();
            let text = if before.is_empty() {
                after_block.to_string()
            } else {
                format!("{before}\n{after_block}")
            };
            (text.trim().to_string(), Some(DirectorPlanUpdate::Set(out)))
        }
        _ => (full.trim().to_string(), None),
    }
}

fn sanitize_session_messages(sess: &mut kaleido_core::TavernSession) -> bool {
    let mut dirty = false;
    for msg in sess.messages.iter_mut() {
        if msg.role == "user" {
            continue;
        }
        let (clean, opts) = split_options_from_narrative(&msg.content);
        if clean != msg.content {
            msg.content = clean;
            dirty = true;
        }
        // 剥离旧消息中的【面板】标记（面板已随回合回写到 session.panels）
        let (clean_p, _panels) = split_panels_from_narrative(&msg.content);
        if clean_p != msg.content {
            msg.content = clean_p;
            dirty = true;
        }
        // 剥离旧消息中的【程序】标记（程序卡已存到 msg.program）
        let (clean_pr, _prog) = split_program_from_narrative(&msg.content);
        if clean_pr != msg.content {
            msg.content = clean_pr;
            dirty = true;
        }
        // ST-27: 剥离旧消息中的【检定】块（检定结果只以可读文本追加，块不留存）
        let (clean_ch, _checks) = split_check_from_narrative(&msg.content);
        if clean_ch != msg.content {
            msg.content = clean_ch;
            dirty = true;
        }
        if msg.options.is_empty() && !opts.is_empty() {
            msg.options = opts;
            dirty = true;
        } else if !opts.is_empty() && msg.options.is_empty() {
            msg.options = opts;
            dirty = true;
        }
        // if content still has marker somehow
        if msg.content.contains("【选项】") {
            let (c2, o2) = split_options_from_narrative(&msg.content);
            msg.content = c2;
            if msg.options.is_empty() && !o2.is_empty() {
                msg.options = o2;
            }
            dirty = true;
        }
    }
    dirty
}


fn clear_session_active_run(store: &TavernSessionStore, session_id: &str, run_id: Option<&str>) {
    // F2: Use atomic release_turn to prevent race with acquire_turn.
    store.release_turn(session_id, run_id);
}

/// P1-3 (吞噬 denova outline 牵引): best-effort 读取原著剖析（outline/ 产物）。
/// 尝试 pack.title 同名 / outline.md / start.md；失败静默跳过，返回截断(≤2000字)内容。
fn read_outline_clip(state: &AppState, pack: &StoryPack, workspace_id: &str) -> String {
    let mut outline_bg = String::new();
    let mut candidates = vec![
        format!("outline/{}.md", pack.title),
        "outline/outline.md".to_string(),
        "outline/start.md".to_string(),
    ];
    candidates.dedup();
    for rel in candidates {
        if let Ok(body) = state.works.read_text(workspace_id, &rel) {
            if !body.content.trim().is_empty() {
                outline_bg = body.content;
                break;
            }
        }
    }
    if outline_bg.is_empty() {
        return String::new();
    }
    outline_bg.chars().take(2000).collect()
}

/// P3.1（审D 后处理韧性）：非阻塞后处理 LLM 单发改两发 —— 首发失败立即重试一次。
/// 适用 roster 回退 / LLM 抽取 / 记忆压缩等 fire-and-forget 生成调用；
/// 两次皆败仍 fail-open（调用方记日志跳过），不影响回合本体。空响应视为失败触发重试。
async fn bg_llm_with_retry(
    base_url: &str,
    api_key: &str,
    model: &str,
    provider_kind: &str,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u32,
    timeout_secs: u64,
) -> Result<String, String> {
    let first = stream_chat_completions_dispatch(
        base_url, api_key, model, provider_kind, system, user, temperature, max_tokens,
        timeout_secs, |_| true,
    )
    .await;
    match first {
        Ok(t) if !t.trim().is_empty() => Ok(t),
        other => {
            let reason = other.err().unwrap_or_else(|| "empty stream content".into());
            tracing::warn!(reason = %reason, "P3.1 bg llm first attempt failed; retrying once");
            stream_chat_completions_dispatch(
                base_url, api_key, model, provider_kind, system, user, temperature, max_tokens,
                timeout_secs, |_| true,
            )
            .await
        }
    }
}

/// 主回合单次流式尝试（F3/G6 后置项）：把上游 TurnStreamEvent 翻译成
/// ChatStreamEvent 转发给前端（delta / thinking_delta），下游断开时取消 run。
/// SSE 解析已下沉 llm_stream::stream_chat_turn_dispatch，此处只做事件翻译。
#[allow(clippy::too_many_arguments)]
async fn run_main_turn_attempt(
    base_url: &str,
    api_key: &str,
    model: &str,
    provider_kind: &str,
    sys_prompt: &str,
    user: &str,
    tx: &tokio::sync::broadcast::Sender<ChatStreamEvent>,
    run_id: &str,
    hub: &StreamHub,
) -> Result<crate::llm_stream::TurnStreamOutcome, crate::llm_stream::TurnStreamError> {
    use crate::llm_stream::{stream_chat_turn_dispatch, TurnStreamEvent};
    stream_chat_turn_dispatch(
        base_url, api_key, model, provider_kind, sys_prompt, user, 0.75, 32768, 300,
        |ev| {
            let (event_type, delta) = match ev {
                TurnStreamEvent::Delta(d) => ("delta", Some(d)),
                TurnStreamEvent::Thinking(t) => ("thinking_delta", Some(t)),
            };
            if tx
                .send(ChatStreamEvent {
                    run_id: run_id.to_string(),
                    event_type: event_type.into(),
                    delta,
                    message: None,
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                    code: None,
                })
                .is_err()
            {
                // 下游全部断开（客户端离线）：与旧内嵌循环同款处理。
                hub.cancel(run_id);
                return false;
            }
            true
        },
        || hub.is_cancelled(run_id),
    )
    .await
}

async fn start_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<TurnStartRequest>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    // 1. Validate session (F1: ownership check)
    let mut tavern = match state.sessions_tavern.get_for_owner(&session_id, &_session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if tavern.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }

    // G10 (吞噬 denova D1): 回合提交幂等守卫 —— 同一回合同内容已提交时直接返回幂等回执
    // （accepted=false + duplicate_submit + 最近 assistant 消息 id），不产生第二次副作用
    // （LLM 双跑 / 双写入）。未完成回合（末条 user 消息无 assistant 回应）交由下方
    // P2 空响应复用重试路径处理，不判重复。
    let user_msg_hash = kaleido_core::text_hash(&body.message);
    if kaleido_core::turn_submit_guard(&tavern, &user_msg_hash) {
        let message_id = tavern
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.id.clone());
        tracing::info!(
            %session_id,
            hash = %user_msg_hash,
            message_id = ?message_id,
            "st turn duplicate submit rejected (idempotent receipt)"
        );
        return Json(json!({
            "accepted": false,
            "reason": "duplicate_submit",
            "messageId": message_id,
            "sessionId": session_id,
        }))
        .into_response();
    }

    // Stale active_run locks the session after aborted streams / empty completions.
    // Auto-clear when the job is no longer running; otherwise ask client to stop.
    // U11: 中断回合恢复（resume）—— job 仍标 running 但无活 worker（服务重启 / 任务进程死亡 /
    // 超时遗留）时，不再 409 死锁：先 rearm_running 留恢复证据，再以 cancelled 终态关闭旧记录
    // 释放并发槽，随后走正常新回合（用户消息与上下文完整保留，可验证 active_run_id 状态机）。
    let mut resumed_bg = false;
    if let Some(prev) = tavern.active_run_id.clone() {
        let still_running = state
            .jobs
            .get(&prev)
            .map(|j| kaleido_core::is_active_job_status(&j.status))
            .unwrap_or(false);
        if still_running {
            // 仅当 hub 中仍有该 run 的流式 worker 时维持 409（并发 turn 护栏）。
            if state.hub.has_live_worker(&prev) {
                return conflict("ST_TURN_BUSY", "turn in progress; tap 停止 then retry");
            }
            tracing::info!(%session_id, run_id=%prev, "st turn resume: orphaned running job detected");
            resumed_bg = true;
            match state.jobs.rearm_running(&prev) {
                Ok(_) => {
                    // rearm 后立刻以 cancelled 终态关闭旧记录（释放 running 并发槽）；
                    // complete 的 cancel-wins 语义保证不会复活已停止的旧 run。
                    let _ = state.jobs.complete(
                        &prev,
                        "cancelled",
                        None,
                        Some("U11 resume: interrupted turn rearmed then superseded by new run".into()),
                    );
                }
                Err(e) => {
                    tracing::warn!(%session_id, run_id=%prev, error=%e, "st turn resume: rearm failed, continuing anyway");
                }
            }
        }
        // F2: Use atomic release to avoid overwriting a concurrent acquire_turn.
        state.sessions_tavern.release_turn(&session_id, Some(&prev));
        tavern.active_run_id = None;
    }

    // F2: Atomic turn acquisition — set active_run_id to a placeholder BEFORE
    // any heavy work. This prevents two concurrent requests from both seeing
    // active_run_id==None and proceeding to spawn duplicate LLM workers.
    let pending_run_id = format!("pending-{}", tavern.turn + 1);
    match state.sessions_tavern.acquire_turn(&session_id, &pending_run_id) {
        Ok(updated) => {
            tavern = updated;
        }
        Err(kaleido_core::CoreError::Conflict(_msg)) => {
            return conflict("ST_TURN_BUSY", "turn in progress; tap 停止 then retry");
        }
        Err(e) => return map_core_err(e),
    }

    // 2. Persist user message first (failover — Q41)
    // P2 空响应重试去重：仅当上一回合未产出 assistant（末条消息仍为同内容的本用户消息）时
    // 复用原 msg，避免重试产生连续重复的用户消息（规格：同一输入可 idempotent 重试）。
    let reuse_last_user = matches!(
        tavern.messages.last(),
        Some(m) if m.role == "user" && m.content == body.message
    );
    if reuse_last_user {
        tracing::info!(%session_id, "st retry: reusing last user message");
    } else {
        let user_msg = TavernMessage {
            id: format!("msg-{}", Uuid::new_v4()),
            role: "user".into(),
            content: body.message.clone(),
            created_at: Utc::now().to_rfc3339(),
            options: vec![],
            engine_tag: None,
            program: None,
            reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
        };
        tavern.messages.push(user_msg);
    }
    // F2: active_run_id already set atomically by acquire_turn above.

    // 3. Load pack context
    let pack = match state.packs.get(&tavern.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };

    // S4 (吞噬 denova director_run_policy.go): 调度判定 —— 本回合是否附加导演计划生成指令。
    // - interval && director_due → 触发（turn 取本回合完成后的回合数）
    // - on_demand && 上一回合检定命中（assistant 消息含【检定结果】）→ 触发
    // - manual / 模式无效 → 不自动触发；仅 API 置 director_pending 后由本回合消费
    // 兼容旧包：默认 on_demand+interval=0 无规则检定 → 永不自动触发。
    let director_pending_bg = tavern.director_pending;
    let next_turn = tavern.turn + 1;
    let last_plan_turn = tavern
        .director_plan
        .as_ref()
        .map(|p| p.updated_turn);
    let run_mode = pack.stage_director.run_policy.mode.trim().to_string();
    let interval_turns = pack.stage_director.run_policy.interval_turns;
    let last_turn_had_check = tavern
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content.contains("【检定结果】"))
        .unwrap_or(false);
    let director_instruct_bg = director_pending_bg
        || match run_mode.as_str() {
            "interval" => kaleido_core::director_due(&run_mode, interval_turns, next_turn, last_plan_turn),
            "on_demand" => last_turn_had_check,
            _ => false,
        };

    // G3 (吞噬 denova prepareInteractiveDirectorBeforeOpening): 开局导演规划——
    // 会话首个回合（turn==0）且 opening 已 seed 且尚无任何导演 plan 时，由独立导演 LLM
    // 生成开局三文档（选角/场景/分支规划），给故事一个导演意图锚点。
    // B5 (2026-08-26): 改为后台执行。原实现在此同步 await 导演 LLM（实测 zen 网关 ~110s），
    // 把 POST /turn 首回合响应整体阻塞（期间无 job、无日志、客户端断开遗留 pending-N 锁）。
    // 现在：登记 DirectorTaskGroup("opening_plan") 后立即放行回合（director_task 随本回合
    // 正常落盘，GET director-config 可见）；计划生成后 fresh 读 + CAS 写回会话，
    // 从下一回合起注入 system prompt。幂等：已有 plan / 任务已在跑则跳过。
    if kaleido_core::opening_plan_due(tavern.turn, tavern.director_plan.is_some(), tavern.opening_seeded)
    {
        let group_g3 = state.director_tasks.clone();
        if group_g3.acquire(&session_id, "opening_plan") {
            tavern.director_task = Some("opening_plan".into());
            let state_g3 = state.clone();
            let session_id_g3 = session_id.clone();
            let pack_id_g3 = tavern.pack_id.clone();
            tokio::spawn(async move {
                let outcome: Result<Option<DirectorPlan>, String> = async {
                    let sess = match state_g3.sessions_tavern.get(&session_id_g3) {
                        Ok(s) => s,
                        Err(e) => return Err(format!("load session failed: {e}")),
                    };
                    if sess.pack_missing {
                        return Err("session is read-only (pack missing)".into());
                    }
                    let pack = match state_g3.packs.get(&pack_id_g3) {
                        Ok(p) => p,
                        Err(e) => return Err(format!("load pack failed: {e}")),
                    };
                    generate_director_plan_llm(&state_g3, &sess, &pack).await
                }
                .await;
                let mut fresh = match state_g3.sessions_tavern.get(&session_id_g3) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = %e, %session_id_g3, "st opening_plan bg: load session failed");
                        group_g3.release(&session_id_g3);
                        return;
                    }
                };
                fresh.director_task = None;
                match outcome {
                    Ok(Some(plan)) => {
                        tracing::info!(%session_id_g3, "st opening_plan bg: 开局导演规划生成成功");
                        fresh.director_plan = Some(plan);
                        fresh.director_pending = false;
                    }
                    Ok(None) => {
                        tracing::debug!(%session_id_g3, "st opening_plan bg: 导演 LLM 未配置，跳过开局规划");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %session_id_g3, "st opening_plan bg: 开局导演规划生成失败，不阻断");
                    }
                }
                // CAS 写回：base 取本次 fresh 读的 revision，避免覆盖并发落盘（回合 worker 等）
                let base_rev = fresh.updated_at.clone();
                if let Err(e) = state_g3.sessions_tavern.save_with_revision(fresh, &base_rev) {
                    tracing::warn!(error = %e, %session_id_g3, "st opening_plan bg: CAS save 冲突，计划丢弃（下回合可重新生成）");
                }
                group_g3.release(&session_id_g3);
            });
        } else {
            tracing::debug!(%session_id, "st opening_plan: 后台任务已在跑，跳过重复登记");
        }
    }

    // S5 (吞噬 denova event_package): 每回合开始时抽取事件卡（若 pack 配置了事件包）。
    // seed 取 session_id + next_turn 的确定性哈希：同会话同回合可复现，跨会话/回合不重复。
    if pack.event_packages.is_empty() {
        tavern.last_event = None;
    } else {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&tavern.session_id, &mut h);
        std::hash::Hash::hash(&next_turn, &mut h);
        let seed = h.finish();
        // B1 首回合保护：turn=1 不抽事件卡（穿越茫然开幕不被后期事件污染）。
        // A2 按章过滤：传当前 chapter_cursor，卡标 chapter_range 时只抽覆盖当前章的卡；
        // 无标注卡不受影响（A3 兼容旧 pack）。
        let current_chapter = tavern.chapter_cursor.as_deref();
        let event_pick = if next_turn <= 1 {
            None
        } else {
            kaleido_core::pick_event_card(&pack, seed, next_turn, tavern.last_event.as_ref(), current_chapter)
        };
        // G7 冷却：以最近一次抽取记录做冷却参考（turn 差 < cooldown_turns 排除同卡）；None = 无冷却限制
        match event_pick {
            Some((pkg, card)) => {
                tavern.last_event = Some(kaleido_core::EventLogEntry {
                    turn: next_turn,
                    package_id: pkg.id.clone(),
                    card_id: card.id.clone(),
                    title: card.title.clone(),
                    prompt: card.prompt.clone(),
                    created_at: Utc::now().to_rfc3339(),
                    type_name: card.type_name.clone(),
                    category: card.category.clone(),
                    intensity: card.intensity.clone(),
                });
            }
            None => tavern.last_event = None,
        }
    }

    // Chapter body
    let chapter_body = if let Some(ch_id) = &tavern.chapter_cursor {
        // Find the chapter's body_path
        if let Some(ch) = pack.chapters.iter().find(|c| c.id == *ch_id) {
            if !ch.body_path.is_empty() {
                state
                    .packs
                    .read_chapter_body(&tavern.pack_id, &ch.body_path)
                    .unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // 3b. Compute query embedding for memory RAG (ST-21)
    let query_embedding: Option<Vec<f32>> = {
        let last_user = tavern.messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");
        let node_desc = tavern.node_id.as_deref().unwrap_or("");
        if !last_user.is_empty() || !node_desc.is_empty() {
            let text = format!("{} {}", last_user, node_desc);
            if let Some(eb) = &state.embedding_base {
                let eb = eb.clone();
                let client = reqwest::Client::builder()
                    .timeout(StdDuration::from_secs(10))
                    .build()
                    .unwrap_or_default();
                match crate::llm_stream::get_embedding(&eb, &text, &client).await {
                    Ok(emb) => Some(emb),
                    Err(e) => {
                        tracing::warn!(error=%e, "embedding failed, falling back to token-level");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        }
    };

    // 4. Build system prompt
    kaleido_core::ensure_focus_character(&mut tavern);
    // MCP 外设工具清单（吸收自 Liyuan mcp.ts）
    let mcp_tools = crate::tavern_mcp::list_tools_cached(state.auth.data_root().root()).await;
    // P0 闭环: 调用点预载伏笔（weight 前 15 + 字符预算），保持 build_tavern_system_prompt 纯函数。
    let foreshadow_block = preload_foreshadow_block(&state, &pack, &tavern);
    tracing::info!(
        foreshadow_chars = foreshadow_block.as_ref().map(|s| s.chars().count()).unwrap_or(0),
        pack = %pack.id,
        "foreshadow preload",
    );
    let mut system_prompt = build_tavern_system_prompt(&pack, &tavern, &chapter_body, &state.auth.data_root().cross_session_dir(), query_embedding, &mcp_tools, foreshadow_block.as_deref());
    // U9 (吞噬 Openwrite O5): 风格指南注入 —— 参考库启用的 style_guide 追加到 system prompt。
    // 零 LLM 依赖（规则版 evidence 合成）；未启用/无指南时零开销。
    {
        let style_block = crate::reference_library::ReferenceLibraryStore::new(state.auth.data_root().root()).injection_block();
        if !style_block.is_empty() {
            system_prompt = format!("{}\n\n{}", system_prompt, style_block);
        }
    }
    // 吞噬资产接线（P0）: 账本/情感曲线/角色弧/关系演化注入 —— 全启发式零 LLM，
    // 调用点预载（同 foreshadow 模式），无资产时零开销。参考库风格指南之后注入。
    {
        let asset_block = preload_asset_blocks(&state, &pack, &tavern, &chapter_body);
        if let Some(ab) = asset_block {
            system_prompt = format!("{}\n\n{}", system_prompt, ab);
        }
    }
    // P1-3 (吞噬 denova outline 牵引): 原著剖析注入剧情 prompt —— 每回合给剧情模型原著素材，
    // 与导演指令注入独立（导演调度命中才注入；此处 best-effort 恒注入，无 outline 产物时零开销）。
    {
        let outline_clip = read_outline_clip(&state, &pack, &_session.workspace_id);
        if !outline_clip.is_empty() {
            system_prompt = format!(
                "{}\n\n## 原著剖析（大纲产物，用于主线牵引）\n{}",
                system_prompt, outline_clip
            );
        }
    }
    // S4 (吞噬 denova director_plan): 导演计划生成指令（ST-26 指令）—— 调度命中时附加
    if director_instruct_bg {
        let node = tavern
            .node_id
            .as_deref()
            .and_then(|nid| pack.nodes.iter().find(|n| n.id == *nid));
        let beats: Vec<&str> = node
            .map(|n| n.locked_beats.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        let mut inst = String::from(
            "【导演计划】指令：若剧情进入新阶段且需要导演计划，请输出块：【导演计划】{\"goal\":\"…\",\"pressure\":\"…\",\"cost\":\"…\",\"hits_beats\":[\"<仅可引用当前节点 locked_beats 原文>\"]}；无需更新则输出：【导演计划】none",
        );
        if !beats.is_empty() {
            inst.push_str(&format!("。当前节点 locked_beats 原文：{}", beats.join("；")));
        }
        inst.push_str("。导演计划仅为叙事意图，绝不改写 locked_beats 已锁定的硬事实。");
        // P1-3 (吞噬 denova outline 牵引): 注入原著剖析（outline/ 产物）到导演指令。
        // best-effort：尝试 pack.title 同名 / outline.md / start.md；失败静默跳过。
        let outline_clip = read_outline_clip(&state, &pack, &_session.workspace_id);
        if !outline_clip.is_empty() {
            inst.push_str(&format!("\n[原著剖析]\n{}", outline_clip));
        }
        // G6 (吞噬 denova interactiveDirectorEventCatalog): 事件目录注入 —— 让导演知道有哪些牌可打，
        // 计划可引用目录中的事件作后续张力/铺垫来源，但不得强制本回合触发（保持叙事自由）。
        let event_catalog = build_event_catalog_block(&pack);
        if !event_catalog.is_empty() {
            inst.push_str(&format!(
                "\n{}\n注：导演计划可引用事件目录中的事件作为后续张力/铺垫来源，但不得强制本回合触发（保留玩家叙事自由）。",
                event_catalog
            ));
        }
        system_prompt = format!("{}\n\n{}", system_prompt, inst);
    }

    // S7 (P1-1): history vector recall — query session vector index with recent context,
    // inject top hits into system prompt so compacted/archived details are recoverable.
    {
        let sess_key = format!("sess-{session_id}");
        let s7_idx = state.vector_index.load(&sess_key);
        if !s7_idx.entries.is_empty() {
            // recent context = last few messages (exclude the incoming user message)
            let qtext: Vec<String> = tavern
                .messages
                .iter()
                .rev()
                .take(6)
                .filter_map(|m| {
                    let c = m.content.trim();
                    if c.is_empty() || c.starts_with('[') || c.starts_with("（第") {
                        None
                    } else {
                        Some(c.to_string())
                    }
                })
                .collect();
            let qtext = qtext.join("\n");
            if !qtext.trim().is_empty() {
                // S7 改进① (2026-08-18): query 叠加当前节点剧情要点（locked_beats + 节点标题）。
                // 使久远关键剧情（承诺/约定/关系确立）在剧情推进到相关节点时能语义召回，
                // 而非只依赖"最近消息"——此前长间距关键事件因 query 无剧情导向而漏召回。
                // clone 而非 move：下方 6699 的 `queries = vec![qtext.clone()]` 仍需原 qtext
                let mut qtarget = qtext.clone();
                if let Some(nid) = tavern.node_id.as_deref() {
                    if let Some(node) = pack.nodes.iter().find(|n| n.id == nid) {
                        qtarget = s7_attach_plot_scope(qtarget, Some((&node.title, &node.locked_beats)));
                    }
                }
                let qtext2 = qtarget;
                match tokio::task::spawn_blocking(move || crate::embed_local::embed_one(&qtext2)).await {
                    Ok(Ok(qv)) => {
                        // [morphling Wave B1 2026-08-16] 混合检索升级（吸收自 SillyTavern-BakemonoMemory
                        // hybrid-retrieval）：语义(余弦) + 词法(IDF/中文ngram) + 关键词三路候选并集。
                        // 修复纯向量阈值漏召回：低相似度但词法命中的旧剧情（如「素描」「画室」）也能召回。
                        let records: Vec<kaleido_core::bakemono_retrieval::HybridRecord> = s7_idx
                            .entries
                            .iter()
                            .filter(|e| !e.vector.is_empty())
                            .map(|e| kaleido_core::bakemono_retrieval::HybridRecord {
                                id: e.uid.clone(),
                                message_id: None,
                                title: None,
                                summary: None,
                                text: e.text.clone(),
                                embedding_score: Some(kaleido_core::vector_cosine_similarity(&qv, &e.vector)),
                            })
                            .collect();
                        if !records.is_empty() {
                            let queries = vec![qtext.clone()];
                            let candidates = kaleido_core::bakemono_retrieval::select_hybrid_candidates(
                                &records,
                                &queries,
                                &[],
                                &kaleido_core::bakemono_retrieval::CandidateOptions {
                                    embedding_threshold: 0.42,
                                    candidate_count: 8,
                                    ..Default::default()
                                },
                            );
                            let lines: Vec<String> = candidates
                                .iter()
                                .take(4)
                                .map(|c| c.record.text.clone())
                                .collect();
                            if !lines.is_empty() {
                                // S7 改进② (2026-08-18): 注入硬度分级——命中的旧对话含剧情关键信号
                                //（承诺/约定/答应/约好）时升级为硬约束（同"剧情连续性守卫"语气），
                                // 否则维持"按需沿用"软参考。关键事实不丢、不削弱模型自由度。
                                let joined = lines.join("\n\n");
                                let head = s7_recall_title(&joined);
                                let recall = format!("{}\n{}", head, joined);
                                system_prompt = format!("{}\n\n{}", system_prompt, recall);
                                info!(session = %session_id, hits = candidates.len(), "S7 hybrid recall injected");
                            }
                        }
                    }
                    Ok(Err(e)) => warn!(error = %e, "S7 recall embed failed"),
                    Err(e) => warn!(error = %e, "S7 recall join failed"),
                }
            }
        }
    }

    // 5. Build user message for LLM
    // §13.4① (2026-08-18): 玩家动作指令强制包装（短动作型消息加强调，防选项动作被吞）
    let user_content = wrap_player_action(&body.message).unwrap_or_else(|| body.message.clone());
    // [时间天气 v2 2026-08-17] 捕获玩家原始消息到 spawn 作用域（供回合结束时间/天气
    // 推进信号解析；spawn 内 `let body=json!()` 会遮蔽外层 body，故单独捕获此变量）。
    let player_msg_original = body.message.clone();

    // 6. Resolve LLM config
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    let base = llm.base_url.clone();
    let key = llm.api_key.clone();
    if base.trim().is_empty() || key.trim().is_empty() {
        return service_unavailable("ST_LLM_NOT_CONFIGURED", "LLM not configured");
    }
    // G6: effective provider kind (managed provider protocol > env default).
    let prov_kind_bg = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);

    let model = body
        .model
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let m = llm.model.clone();
            if m.is_empty() {
                state.llm_model.clone()
            } else {
                m
            }
        });

    // P0: 回合生成质量档位（吞噬 denova novel-lite/standard/heavy）。
    // 生效顺序：turn 请求参数 > 会话持久化档位 > lite（现状零回归）。
    // 显式传入且与会话档位不同时回写会话，实现 per-session 选择（API 优先；前端 UI 可后置）。
    let quality = body.quality.unwrap_or_else(|| tavern.quality.into());
    if quality != tavern.quality.into() {
        tavern.quality = quality.into();
    }
    info!(?quality, "st turn quality");

    // P4 (吞噬 denova appendWritingSkillLoadHint): 本轮系统提示注入写作 Skill 档位行
    // + 原则级写作规则（standard/heavy 有规则文件才注入；lite 仅提示，控制 token）。
    {
        let tier = crate::skill_layer::resolve_tier_for_quality(quality);
        let ws_id = _session.workspace_id.clone();
        let skill_hint = load_writing_skill(state.auth.data_root().root(), Some(&ws_id), tier);
        system_prompt = append_writing_skill_hint(&system_prompt, tier, skill_hint.as_ref());
    }

    // 7. Create Job
    let job = match state.jobs.create(
        "tavern-turn",
        &_session.user_id,
        &_session.workspace_id,
        json!({
            "sessionId": session_id,
            "packId": tavern.pack_id,
        }),
        Some(model.clone()),
        None,
    ) {
        Ok(j) => j,
        Err(e) => return map_core_err(e),
    };
    let run_id = job.run_id.clone();

    // F3: If the job was queued (at concurrency limit), do NOT spawn the LLM
    // worker. Release the turn lock and return 429 so the client can retry later.
    // This ensures max_concurrent_jobs is a real limit on upstream model requests.
    if kaleido_core::normalize_job_status(&job.status) == "queued" {
        tracing::info!(
            %session_id, %run_id,
            running = state.jobs.running_count(),
            "st turn rejected: job queued (concurrency limit)"
        );
        // Cancel the queued job and release the session turn lock.
        let _ = state.jobs.cancel(&run_id);
        state.sessions_tavern.release_turn(&session_id, Some(&run_id));
        return err_with_code(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "ST_SERVER_BUSY",
            "server busy: concurrency limit reached, please retry shortly",
            serde_json::json!({ "runId": run_id, "retryable": true }),
        );
    }

    tavern.active_run_id = Some(run_id.clone());

    // 8. Save session (user msg persisted, run_id set)
    // M-2 (CAS): use save_with_revision with the base revision captured at
    // acquire_turn (the most recent locked write). If a concurrent save
    // happened in between (e.g. director_plan CAS write above updated the
    // revision), we re-read the live revision from tavern.updated_at so the
    // CAS reflects the latest on-disk state. On conflict, surface 409 so the
    // client can retry — we must NOT silently overwrite a concurrent write.
    let base_rev = tavern.updated_at.clone();
    match state
        .sessions_tavern
        .save_with_revision(tavern, &base_rev)
    {
        Ok(_) => {}
        Err(e) => {
            // F4 (2026-08-16 幽灵 job 根治): job 已在上面 create 标为 running，
            // 若在此早退（CAS 冲突等）不 cancel，会遗留「running 但 worker 未 spawn」
            // 的幽灵 job，永久占满 max_concurrent_jobs，导致后续所有回合 429
            // （st turn rejected: job queued）。必须先 cancel 再返回。
            let _ = state.jobs.cancel(&run_id);
            state.sessions_tavern.release_turn(&session_id, Some(&run_id));
            return map_core_err(e);
        }
    }

    // [morphling ROMA P0 2026-08-19] 回合级检查点：进入 LLM 流前落「Streaming」阶段，
    // 崩溃/中断后可从会话文件判读上一回合死在哪一步（U11 resume 用）。跟随会话落盘。
    if let Ok(mut sess) = state.sessions_tavern.get(&session_id) {
        sess.set_turn_progress(next_turn, TurnPhase::Streaming, &run_id, "mainstream llm");
        let _ = state.sessions_tavern.save(sess);
    }

    // P3 编排（I3）：阶段事件写入 job 记录 —— GET /api/v1/jobs 可观测回合进度
    // （progress/cursor 与 background pipeline 同口径，双通道统一，审D 三-3）。
    let _ = state.jobs.push_event(
        &run_id,
        kaleido_core::JobEvent::progress("streaming: main llm", 0.1),
        Some(0.1),
        Some("phase:streaming".to_string()),
    );

    // 9. Register with StreamHub
    let tx = state.hub.register(&run_id);
    info!(%run_id, %session_id, %model, "tavern turn started");

    // 10. Spawn LLM streaming task
    let hub = state.hub.clone();
    let jobs = state.jobs.clone();
    let sessions_store = state.sessions_tavern.clone();
    let packs_store = state.packs.clone();
    let works_store = state.works.clone();
    let workspace_id_bg = _session.workspace_id.clone();
    let cross_dir = state.auth.data_root().cross_session_dir();
    let run_id_bg = run_id.clone();
    let session_id_bg = session_id.clone();
    let _pack_id_bg = pack.id.clone();
    let base_bg = base;
    let key_bg = key;
    let model_bg = model.clone();
    let prov_bg = prov_kind_bg;
    // Fallback model for upstream 4xx model errors (invalid_model / model_not_found):
    // env default is the most trustworthy (frontend/settings may carry stale ids).
    let fallback_model_bg = state.llm_model.clone();
    let sys_prompt_bg = system_prompt;
    let user_bg = user_content;
    // [时间天气 v2 2026-08-17] spawn 块内捕获玩家原始消息（外层 body.message 会被
    // 内层 `let body=json!()` 遮蔽），供回合结束的时间/天气推进信号解析用。
    let player_msg_bg = player_msg_original;
    // §13.5 Scene Gate: 首回合场景错位纠偏需要回合号（turn==1 才启用）
    let next_turn_bg = next_turn;
    let embedding_base_bg = state.embedding_base.clone();
    let data_root_bg = state.auth.data_root().root().to_path_buf();
    let quality_bg = quality;
    // U11: 本回合是否经中断恢复（孤儿 running → rearm + 新回合）进入。
    let resumed_bg = resumed_bg;
    // P4: 装载当前档位写作 Skill（workspace→user→builtin 三层），随 spawn 传入质量管道。
    let skill_bg = load_writing_skill(
        &data_root_bg,
        Some(&workspace_id_bg),
        crate::skill_layer::resolve_tier_for_quality(quality),
    );

    // P3 编排硬化（I1 前置捕获）：看门狗克隆集必须在 worker spawn 之前创建 ——
    // 下方 `async move` 会把 hub/jobs/sessions_store/run_id_bg/... 移入 worker 闭包，
    // 之后原绑定失效无法再 clone。
    let hub_panic = hub.clone();
    let jobs_panic = jobs.clone();
    let sessions_store_panic = sessions_store.clone();
    let run_id_panic = run_id_bg.clone();
    let session_id_panic = session_id_bg.clone();
    let model_panic = model_bg.clone();
    let quality_panic = quality_bg;
    let sys_prompt_panic = sys_prompt_bg.clone();
    let user_panic = user_bg.clone();
    let resumed_panic = resumed_bg;
    let worker = tokio::spawn(async move {
        // U11: 回合预算/成本记账状态（主流 + 质量管道 + 非阻塞后处理共享）。
        let turn_started = std::time::Instant::now();
        let turn_started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let elapsed_ms = || turn_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let mut used_fallback = false;
        // [P7] retried_empty 标志已移除：空响应重试天然单发——入口条件含 full_text.is_empty()，
        // 重试成功则文本非空不再进入，仍空/失败则提前 return（无循环重入路径）。
        let mut extra_llm_calls: u32 = 0;
        // ===== 主回合流式调用（F3/G6 后置项：SSE 解析下沉 llm_stream）=====
        // 单次尝试 = run_main_turn_attempt（内部走 stream_chat_turn_dispatch，
        // 三家协议统一）。错误分类与旧内嵌循环一一映射，UPSTREAM_* 语义码不变。
        let mut outcome_opt: Option<crate::llm_stream::TurnStreamOutcome> = None;
        match run_main_turn_attempt(
            &base_bg,
            &key_bg,
            &model_bg,
            &prov_bg,
            &sys_prompt_bg,
            &user_bg,
            &tx,
            &run_id_bg,
            &hub,
        )
        .await
        {
            Ok(o) => outcome_opt = Some(o),
            Err(TurnStreamError::Stopped) => {
                // 中途停止：取消优先（与旧循环顺序一致），其次 U11 超时看门狗。
                if !hub.is_cancelled(&run_id_bg)
                    && turn_over_budget(turn_started_ms, u11_hard_timeout_ms())
                {
                    let _ = tx.send(ChatStreamEvent {
                        run_id: run_id_bg.clone(),
                        event_type: "error".into(),
                        delta: None,
                        message: Some("turn timeout (U11 budget exceeded)".into()),
                        code: Some("TURN_TIMEOUT".into()),
                        context_compaction: None,
                        input_tokens: None,
                        output_tokens: None,
                    });
                    let _ = jobs.merge_job_payload(
                        &run_id_bg,
                        u11_accounting_json(
                            &model_bg, quality_bg, &sys_prompt_bg, &user_bg, "",
                            extra_llm_calls, used_fallback, elapsed_ms(), resumed_bg, 0,
                            None, Some("turn timeout"),
                        ),
                    );
                    jobs.finish(&run_id_bg, "error");
                    clear_session_active_run(&sessions_store, &session_id_bg, Some(&run_id_bg));
                    hub.cleanup(&run_id_bg);
                }
                // 取消路径由下方统一 F4 判定收尾（保持旧语义）。
            }
            Err(TurnStreamError::Connect(e)) => {
                let _ = tx.send(ChatStreamEvent {
                    run_id: run_id_bg.clone(),
                    event_type: "error".into(),
                    delta: None,
                    message: Some(format!("upstream connect: {e}")),
                    code: Some("UPSTREAM_CONNECT".into()),
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                });
                let _ = jobs.merge_job_payload(
                    &run_id_bg,
                    u11_accounting_json(
                        &model_bg, quality_bg, &sys_prompt_bg, &user_bg, "", extra_llm_calls,
                        used_fallback, elapsed_ms(), resumed_bg, 0, None, Some("upstream connect"),
                    ),
                );
                jobs.finish(&run_id_bg, "error");
                clear_session_active_run(&sessions_store, &session_id_bg, Some(&run_id_bg));
                hub.cleanup(&run_id_bg);
                return;
            }
            Err(TurnStreamError::Status { status, body }) => {
                // Model fallback: 上游 4xx 模型类错误（invalid_model / model_not_found /
                // 不可用）→ 用 env 默认模型重试一次。
                if crate::llm_stream::is_model_rejection(status, &body)
                    && !fallback_model_bg.is_empty()
                    && fallback_model_bg != model_bg
                {
                    info!(
                        %run_id_bg,
                        requested = %model_bg,
                        fallback = %fallback_model_bg,
                        "upstream rejected model, retrying with default"
                    );
                    match run_main_turn_attempt(
                        &base_bg,
                        &key_bg,
                        &fallback_model_bg,
                        &prov_bg,
                        &sys_prompt_bg,
                        &user_bg,
                        &tx,
                        &run_id_bg,
                        &hub,
                    )
                    .await
                    {
                        Ok(o) => {
                            outcome_opt = Some(o);
                            used_fallback = true;
                        }
                        Err(TurnStreamError::Stopped) => {
                            // 回退流中途取消/超时：同主路径 Stopped 处理。
                            if !hub.is_cancelled(&run_id_bg)
                                && turn_over_budget(
                                    turn_started_ms,
                                    u11_hard_timeout_ms(),
                                )
                            {
                                let _ = tx.send(ChatStreamEvent {
                                    run_id: run_id_bg.clone(),
                                    event_type: "error".into(),
                                    delta: None,
                                    message: Some("turn timeout (U11 budget exceeded)".into()),
                                    code: Some("TURN_TIMEOUT".into()),
                                    context_compaction: None,
                                    input_tokens: None,
                                    output_tokens: None,
                                });
                                let _ = jobs.merge_job_payload(
                                    &run_id_bg,
                                    u11_accounting_json(
                                        &fallback_model_bg, quality_bg, &sys_prompt_bg,
                                        &user_bg, "", extra_llm_calls, true, elapsed_ms(),
                                        resumed_bg, 0, None, Some("turn timeout"),
                                    ),
                                );
                                jobs.finish(&run_id_bg, "error");
                                clear_session_active_run(
                                    &sessions_store,
                                    &session_id_bg,
                                    Some(&run_id_bg),
                                );
                                hub.cleanup(&run_id_bg);
                            }
                        }
                        Err(TurnStreamError::Status { status: status2, body: text2 }) => {
                            let _ = tx.send(ChatStreamEvent {
                                run_id: run_id_bg.clone(),
                                event_type: "error".into(),
                                delta: None,
                                message: Some(format!(
                                    "upstream {status2} (model fallback also failed): {}",
                                    text2.chars().take(300).collect::<String>()
                                )),
                                code: Some("UPSTREAM_STATUS_FALLBACK".into()),
                                context_compaction: None,
                                input_tokens: None,
                                output_tokens: None,
                            });
                            let _ = jobs.merge_job_payload(
                                &run_id_bg,
                                u11_accounting_json(
                                    &fallback_model_bg, quality_bg, &sys_prompt_bg, &user_bg,
                                    "", extra_llm_calls, true, elapsed_ms(), resumed_bg, 0, None,
                                    Some("upstream status (fallback failed)"),
                                ),
                            );
                            jobs.finish(&run_id_bg, "error");
                            clear_session_active_run(
                                &sessions_store,
                                &session_id_bg,
                                Some(&run_id_bg),
                            );
                            hub.cleanup(&run_id_bg);
                            return;
                        }
                        Err(TurnStreamError::Connect(e)) => {
                            let _ = tx.send(ChatStreamEvent {
                                run_id: run_id_bg.clone(),
                                event_type: "error".into(),
                                delta: None,
                                message: Some(format!("upstream connect (model fallback): {e}")),
                                code: Some("UPSTREAM_CONNECT_FALLBACK".into()),
                                context_compaction: None,
                                input_tokens: None,
                                output_tokens: None,
                            });
                            let _ = jobs.merge_job_payload(
                                &run_id_bg,
                                u11_accounting_json(
                                    &fallback_model_bg, quality_bg, &sys_prompt_bg, &user_bg,
                                    "", extra_llm_calls, true, elapsed_ms(), resumed_bg, 0, None,
                                    Some("upstream connect (model fallback)"),
                                ),
                            );
                            jobs.finish(&run_id_bg, "error");
                            clear_session_active_run(
                                &sessions_store,
                                &session_id_bg,
                                Some(&run_id_bg),
                            );
                            hub.cleanup(&run_id_bg);
                            return;
                        }
                        Err(TurnStreamError::Stream(e)) => {
                            warn!(error = %e, "tavern stream error (model fallback)");
                            let _ = tx.send(ChatStreamEvent {
                                run_id: run_id_bg.clone(),
                                event_type: "error".into(),
                                delta: None,
                                message: Some(e.clone()),
                                code: Some("UPSTREAM_STREAM".into()),
                                context_compaction: None,
                                input_tokens: None,
                                output_tokens: None,
                            });
                            let _ = jobs.merge_job_payload(
                                &run_id_bg,
                                u11_accounting_json(
                                    &fallback_model_bg, quality_bg, &sys_prompt_bg, &user_bg,
                                    "", extra_llm_calls, true, elapsed_ms(), resumed_bg, 0, None,
                                    Some("stream error"),
                                ),
                            );
                            jobs.finish(&run_id_bg, "error");
                            clear_session_active_run(
                                &sessions_store,
                                &session_id_bg,
                                Some(&run_id_bg),
                            );
                            hub.cleanup(&run_id_bg);
                            return;
                        }
                    }
                } else {
                    let _ = tx.send(ChatStreamEvent {
                        run_id: run_id_bg.clone(),
                        event_type: "error".into(),
                        delta: None,
                        message: Some(format!(
                            "upstream {status}: {}",
                            body.chars().take(300).collect::<String>()
                        )),
                        code: Some("UPSTREAM_STATUS".into()),
                        context_compaction: None,
                        input_tokens: None,
                        output_tokens: None,
                    });
                    let _ = jobs.merge_job_payload(
                        &run_id_bg,
                        u11_accounting_json(
                            &model_bg, quality_bg, &sys_prompt_bg, &user_bg, "", extra_llm_calls,
                            used_fallback, elapsed_ms(), resumed_bg, 0, None, Some("upstream status"),
                        ),
                    );
                    jobs.finish(&run_id_bg, "error");
                    clear_session_active_run(&sessions_store, &session_id_bg, Some(&run_id_bg));
                    hub.cleanup(&run_id_bg);
                    return;
                }
            }
            Err(TurnStreamError::Stream(e)) => {
                warn!(error=%e, "tavern stream error");
                let _ = tx.send(ChatStreamEvent {
                    run_id: run_id_bg.clone(),
                    event_type: "error".into(),
                    delta: None,
                    message: Some(e.to_string()),
                    code: Some("UPSTREAM_STREAM".into()),
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                });
                let _ = jobs.merge_job_payload(
                    &run_id_bg,
                    u11_accounting_json(
                        &model_bg, quality_bg, &sys_prompt_bg, &user_bg, "", extra_llm_calls,
                        used_fallback, elapsed_ms(), resumed_bg, 0, None, Some("stream error"),
                    ),
                );
                jobs.finish(&run_id_bg, "error");
                clear_session_active_run(&sessions_store, &session_id_bg, Some(&run_id_bg));
                hub.cleanup(&run_id_bg);
                return;
            }
        }

        // F4: If client disconnected during streaming, stop processing immediately.
        if hub.is_cancelled(&run_id_bg) {
            jobs.finish(&run_id_bg, "cancelled");
            clear_session_active_run(&sessions_store, &session_id_bg, Some(&run_id_bg));
            hub.cleanup(&run_id_bg);
            return;
        }

        let outcome = match outcome_opt {
            Some(o) => o,
            None => {
                // Stopped 且既非取消也非超时：理论不可达，保守按取消收尾。
                debug_assert!(hub.is_cancelled(&run_id_bg));
                jobs.finish(&run_id_bg, "cancelled");
                clear_session_active_run(&sessions_store, &session_id_bg, Some(&run_id_bg));
                hub.cleanup(&run_id_bg);
                return;
            }
        };
        let mut full_text = outcome.text;
        // [token 显示 2026-08-16] usage.total_tokens 随 asst_msg.tokens 落盘，
        // 前端 stMsgMeta 显示。现由 llm_stream 统一捕获。
        let mut usage_tokens: u32 = outcome.total_tokens.unwrap_or(0).min(u32::MAX as u64) as u32;
        // 模型推理内容独立累积，随 asst_msg.reasoning 落盘，前端折叠箭头展示。
        let mut thinking_text = outcome.thinking;

        // Append assistant message to session + run extraction
        if full_text.is_empty() {
            let cancelled = hub.is_cancelled(&run_id_bg);
            // 上游空响应重试 1 次（zen 池忙/限流时空流常见，实测 18s 后 empty response）。
            // 重试成功则继续走正常路径（质量管道 + 落盘）；仍空才走失败。
            // F3：重试复用同一 dispatch 入口 —— 相比旧内嵌简化解析，思维链/usage 也一并捕获。
            if !cancelled
                && !base_bg.trim().is_empty()
                && !key_bg.trim().is_empty()
            {
                extra_llm_calls += 1;
                info!(%run_id_bg, "empty response, retrying once");
                match run_main_turn_attempt(
                    &base_bg,
                    &key_bg,
                    &model_bg,
                    &prov_bg,
                    &sys_prompt_bg,
                    &user_bg,
                    &tx,
                    &run_id_bg,
                    &hub,
                )
                .await
                {
                    Ok(o) if !o.text.trim().is_empty() => {
                        full_text = o.text;
                        thinking_text = o.thinking;
                        if usage_tokens == 0 {
                            usage_tokens =
                                o.total_tokens.unwrap_or(0).min(u32::MAX as u64) as u32;
                        }
                        info!(%run_id_bg, "empty-retry succeeded");
                    }
                    Ok(_) => {
                        warn!(%run_id_bg, "empty-retry still empty");
                    }
                    Err(e) => {
                        warn!(error = %e.message(), "empty-retry failed");
                    }
                }
            }
            if full_text.is_empty() {
                clear_session_active_run(&sessions_store, &session_id_bg, Some(&run_id_bg));
                let cancelled = hub.is_cancelled(&run_id_bg);
                let _ = tx.send(ChatStreamEvent {
                    run_id: run_id_bg.clone(),
                    event_type: if cancelled { "done".into() } else { "error".into() },
                    delta: None,
                    message: Some(if cancelled {
                        "turn cancelled".into()
                    } else {
                        "empty model response".into()
                    }),
                    code: if cancelled { None } else { Some("EMPTY_RESPONSE".into()) },
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                });
                // U11: 空响应/取消路径也记账（耗时/调用次数）
                let _ = jobs.merge_job_payload(
                    &run_id_bg,
                    u11_accounting_json(
                        &model_bg, quality_bg, &sys_prompt_bg, &user_bg, "", extra_llm_calls,
                        used_fallback, elapsed_ms(), resumed_bg, 0, None,
                        Some(if cancelled { "cancelled" } else { "empty response" }),
                    ),
                );
                jobs.finish(
                    &run_id_bg,
                    if cancelled { "cancelled" } else { "error" },
                );
                hub.cleanup(&run_id_bg);
                return;
            }
        }
        // §13.5 Scene Gate (2026-08-18): 首回合场景错位单次纠偏重试。
        // 实证：度蜜月首回合 opening 注入学校摘要+硬锚，flash-free 仍写成酒店/机场
        //（标题《代替父亲和妈妈度蜜月》的先验过强，prompt 文本压不住）。
        // 确定性检测（is_scene_mismatch_location：酒店/套房/机场/三亚等地点词）→
        // 用主线 LLM 配置带纠偏指令重试一次；二次仍错则接受原文+warn（不无限循环）。
        if next_turn_bg <= 1 && is_scene_mismatch_location(&full_text) {
            let corrected_user = format!(
                "【系统纠偏】你上一版开场把场景写错了：现在仍在当前章节注入的场景内\n（如学校/家中客厅，见系统开场摘录），你却写成了酒店/机场/三亚等后续地点。\n请重写本回合正文，从正确的当前场景开始，保留玩家指令的语义：{}。",
                player_msg_bg
            );
            let retry_msgs = vec![
                json!({"role": "system", "content": sys_prompt_bg}),
                json!({"role": "user", "content": corrected_user}),
            ];
            match crate::llm_stream::stream_chat_completions_msgs(
                &base_bg, &key_bg, &model_bg, retry_msgs, 0.1, 8192, 60, |_| true,
            )
            .await
            {
                Ok(rep) if !rep.trim().is_empty() => {
                    let rep = rep.trim().to_string();
                    if !is_scene_mismatch_location(&rep) {
                        full_text = rep;
                        thinking_text.clear();
                        info!(%run_id_bg, "scene-gate corrected first-turn scene");
                    } else {
                        warn!(%run_id_bg, "scene-gate retry still mismatched, keeping original");
                    }
                }
                Ok(_) => warn!(%run_id_bg, "scene-gate retry empty, keeping original"),
                Err(e) => warn!(%run_id_bg, err = %e, "scene-gate retry failed"),
            }
        }
        if !full_text.is_empty() {
            // P0 (吞噬 denova 写作三档): standard/heavy 追加多轮 LLM 协作。
            // lite 直出不走此分支（零回归）；失败保留初稿，绝不阻塞正文。
            // 协议块预剥离 stash：heavy/standard 多轮管道会重写正文吞掉主调用的
            // 协议块，进入 run_quality_refine 前按序预剥离并暂存；lite 不走预剥离
            // （stash 全空 → 管道后剥离段照常工作，零回归）。
            let mut stash_options: Vec<String> = Vec::new();
            let mut stash_panels: Vec<kaleido_core::TavernPanel> = Vec::new();
            let mut stash_program: Option<String> = None;
            let mut stash_mcp: Vec<crate::tavern_mcp::McpCall> = Vec::new();
            let mut stash_state: Vec<kaleido_core::ActorStateUpdate> = Vec::new();
            let mut stash_check: Vec<kaleido_core::TurnCheckRequest> = Vec::new();
            let mut stash_plan: Option<DirectorPlanUpdate> = None;
            let mut stash_advance: Option<String> = None;
            // U11: 预算守卫 —— 接近硬超时则跳过质量管道（保留初稿，best-effort 不阻塞正文）。
            let refine_budget_ok = !turn_over_budget(turn_started_ms, u11_hard_timeout_ms());
            // 管道内剥离的思维段（fix 阶段 <thinking> 块）汇入，最终并入 monologue 折叠展示。
            // 定义在 if 块外：lite/预算不足跳过管道时保持空串，合并区仍可安全引用。
            let mut thinking_pipe = String::new();
            if quality_bg != TurnQuality::Lite
                && !base_bg.trim().is_empty()
                && !key_bg.trim().is_empty()
                && refine_budget_ok
            {
                // P3 编排（I2）：质量管道入口阶段检查点 —— 中断诊断可判「死在管道内哪一步」。
                if let Ok(mut sess_ph) = sessions_store.get(&session_id_bg) {
                    sess_ph.set_turn_progress(next_turn_bg, TurnPhase::Quality, &run_id_bg, "quality refine");
                    let _ = sessions_store.save(sess_ph);
                }
                let _ = jobs.push_event(
                    &run_id_bg,
                    kaleido_core::JobEvent::progress("quality: refine pipeline", 0.5),
                    Some(0.5),
                    Some("phase:quality".to_string()),
                );
                let refine_llm = RemoteQualityLlm {
                    base_url: &base_bg,
                    api_key: &key_bg,
                    model: &model_bg,
                    provider_kind: &prov_bg,
                };
                // 预剥离【技能加载】块：heavy/standard 多轮管道会吞掉该块导致 skill_load
                // 永不装载。先按当前档位装载完整 SKILL.md 存入会话（此分支必非 lite），
                // 剥离后的文本才进入 run_quality_refine。
                let (pre_skill_text, skill_calls) =
                    crate::skill_layer::split_skill_load_calls_from_narrative(&full_text);
                if !skill_calls.is_empty() {
                    let tier = crate::skill_layer::resolve_tier_for_quality(quality_bg);
                    if let Some(doc) = crate::skill_layer::load_writing_skill(
                        &data_root_bg,
                        Some(&workspace_id_bg),
                        tier,
                    ) {
                        if let Ok(mut sess_skill) = sessions_store.get(&session_id_bg) {
                            sess_skill.skill_load = Some(kaleido_core::SkillLoadInfo {
                                tier: tier.to_string(),
                                markdown: crate::skill_layer::skill_full_markdown(&doc),
                            });
                            if let Err(e) = sessions_store.save(sess_skill.clone()) {
                                warn!(error = %e, "st skill load persist failed");
                            }
                            tracing::info!(n = skill_calls.len(), tier, "st skill load injected pre-refine");
                        }
                    }
                    full_text = pre_skill_text;
                }
                // 协议块预剥离（bugC 修复）：heavy/standard 多轮管道 write 阶段会重写
                // 正文、丢掉主调用的协议块，因此在进入 run_quality_refine 前按序剥离
                // 全部协议块并 stash；管道只收纯正文。skill 预剥离已在上方完成（不动）。
                // [fix §10.4 2026-08-16] 开头推理剥离下沉公共链：strip_lite_reasoning_leak
                // 原只在 run_quality_refine（质量管道）调用，Lite 直出路径完全缺失——
                // 实踩 msg「让我理清当前情况。玩家选择了…」整段推理出现在正文首段。
                full_text = strip_lite_reasoning_leak(full_text);
                let (pre_clean, pre_options) = split_options_from_narrative(&full_text);
                full_text = pre_clean;
                stash_options = pre_options;
                if !stash_options.is_empty() {
                    tracing::info!(n = stash_options.len(), "st options pre-refine");
                }

                let (pre_clean_p, pre_panels) = split_panels_from_narrative(&full_text);
                full_text = pre_clean_p;
                stash_panels = pre_panels;
                if !stash_panels.is_empty() {
                    tracing::info!(n = stash_panels.len(), "st panels pre-refine");
                }

                let (pre_clean_pr, pre_program) = split_program_from_narrative(&full_text);
                full_text = pre_clean_pr;
                stash_program = pre_program;
                if stash_program.is_some() {
                    tracing::info!("st program pre-refine");
                }

                let (pre_clean_m, pre_mcp) =
                    crate::tavern_mcp::split_mcp_calls_from_narrative(&full_text);
                full_text = pre_clean_m;
                stash_mcp = pre_mcp;
                if !stash_mcp.is_empty() {
                    tracing::info!(n = stash_mcp.len(), "st mcp pre-refine");
                }

                let (pre_clean_s, pre_state) = split_state_updates_from_narrative(&full_text);
                full_text = pre_clean_s;
                stash_state = pre_state;
                if !stash_state.is_empty() {
                    tracing::info!(n = stash_state.len(), "st state updates pre-refine");
                }

                let (pre_clean_c, pre_check) = split_check_from_narrative(&full_text);
                full_text = pre_clean_c;
                stash_check = pre_check;
                if !stash_check.is_empty() {
                    tracing::info!(n = stash_check.len(), "st rule checks pre-refine");
                }

                let (pre_clean_pl, pre_plan) = split_director_plan_from_narrative(&full_text);
                full_text = pre_clean_pl;
                stash_plan = pre_plan;
                if stash_plan.is_some() {
                    tracing::info!("st director plan pre-refine");
                }

                // ST-15: 【节点推进】marker 预剥离（同 3967-3978 逻辑）
                // [ST-15 fix 2026-08-16] 用 extract_advance_marker 兼容【节点推进:n2】格式
                if let Some((adv_pos, node_id)) = extract_advance_marker(&full_text) {
                    tracing::info!(%node_id, "st llm advance pre-refine");
                    stash_advance = Some(node_id);
                    full_text = full_text[..adv_pos].to_string() + " ";
                }
                // X2a (吞噬自 xiami skimming.rs): 读者速读质检进质量管道 —— 审稿环节前做
                // 确定性检查，速读风险问题文案并入审稿/修复 prompt（QUALITY_FIX 流程生效）；
                // 仅附加信息，失败/发现问题均不阻断回合。
                let skim_config = reader_skimming_config();
                let pre_skim_issues = analyze_skimming(&full_text, &skim_config);
                let mut refine_sys_bg = sys_prompt_bg.clone();
                if !pre_skim_issues.is_empty() {
                    let skim_block = render_skim_issues_for_prompt(&pre_skim_issues);
                    refine_sys_bg = format!(
                        "{}\n\n## 读者速读质检问题（审稿时纳入考量）\n{}",
                        refine_sys_bg, skim_block
                    );
                    tracing::info!(
                        n = pre_skim_issues.len(),
                        "st xiami: 速读质检前置问题并入审稿 prompt（{} 个）",
                        pre_skim_issues.len()
                    );
                }
                // 管道内剥离的思维段在 if 块外定义（thinking_pipe），此处直接引用。
                match run_quality_refine(
                    quality_bg,
                    &refine_llm,
                    &refine_sys_bg,
                    &user_bg,
                    &full_text,
                    skill_bg.as_ref(),
                    &mut thinking_pipe,
                )
                .await
                {
                    Ok((revised, memory_patch)) if !revised.trim().is_empty() => {
                        tracing::info!(quality = ?quality_bg, "st quality refine applied");
                        full_text = revised;
                        // P4: heavy MemoryPatch 回写 actor_states（best-effort，失败仅日志）
                        if let Some(patch) = memory_patch {
                            if let Ok(mut sess_mem) = sessions_store.get(&session_id_bg) {
                                let n = crate::skill_layer::apply_memory_patch_to_states(
                                    &mut sess_mem.actor_states,
                                    &patch,
                                );
                                if n > 0 {
                                    tracing::info!(n, "st heavy memory patch applied to actor states");
                                }
                                let _ = sessions_store.save(sess_mem);
                            }
                        }
                    }
                    Ok(_) => {
                        tracing::warn!(quality = ?quality_bg, "st quality refine empty; keeping draft");
                    }
                    Err(e) => {
                        tracing::warn!(quality = ?quality_bg, error=%e, "st quality refine failed; keeping draft");
                    }
                }
            }

            // Extract options + strip from narrative (chips are the only option UI)
            let (clean_text, options) = if stash_options.is_empty() {
                split_options_from_narrative(&full_text)
            } else {
                (full_text.clone(), stash_options)
            };
            full_text = clean_text;
            if !options.is_empty() {
                tracing::info!(n = options.len(), "st options extracted+stripped");
            }

            // [fix 2026-08-16] 开头推理剥离全档位生效：strip_lite_reasoning_leak 原只在
            // run_quality_refine（standard/heavy 管道）调用，Lite 直出路径完全缺失——
            // 实踩（实例2 代替父亲会话 20:23/20:28）：deepseek-v4-flash-free 在 Lite 直出
            // 时输出「让我理清当前状态…」「好的，梳理一下当前状态…」等整段思考链混入正文。
            // 名称虽含 lite，逻辑是通用「最长连续叙事段」提取，接入公共链后全档位生效。
            full_text = strip_lite_reasoning_leak(full_text);

            // [fix 2026-08-16 P2+ 档位全覆盖] 尾部元话语剥离：此前 strip_trailing_metadiscourse
            // 只在 Heavy 的 fix 阶段（strip_fix_thinking_blocks）调用，Lite/Standard 直出路径
            // 缺失——实踩 deepseek-v4-flash-free 在正文后输出「嗯，这个版本不错。让我检查一下：
            // 1. 时间…✓」自检段 + 状态更新思考（「让我写状态更新块：…我应该更新哪个？…」），
            // 推理混入正文且占用输出预算导致正文后段像「没写完」。接入公共链后全档位生效。
            full_text = strip_trailing_metadiscourse(full_text);

            // ST-30 (2026-08-15 根治): 剥离 <角色清单>…</角色清单> 结构化块（生成端角色自报）。
            // LLM 在回合正文末尾列出实际出场人名，守卫做精确集合比对——根治切词启发式的
            // 漏报（叙述形态点名）与误报（切词切碎短语）。清单缺失时守卫自动降级启发式。
            let (mut full_text, roster_names) = split_roster_from_narrative(&full_text);
            if roster_names.is_some() {
                tracing::info!("st roster block extracted");
            }

            // 面板（吸收自梨园 panels.ts）：提取【面板】块并剥离
            let (clean_panels, panels) = if stash_panels.is_empty() {
                split_panels_from_narrative(&full_text)
            } else {
                (full_text.clone(), stash_panels)
            };
            full_text = clean_panels;
            if !panels.is_empty() {
                tracing::info!(n = panels.len(), "st panels extracted+stripped");
            }

            // 程序卡(吸收自梨园 show_html)：提取【程序】块并剥离, 存到消息 program 字段
            let (clean_prog, program) = if stash_program.is_none() {
                split_program_from_narrative(&full_text)
            } else {
                (full_text.clone(), stash_program)
            };
            full_text = clean_prog;
            if program.is_some() {
                tracing::info!("st program card extracted");
            }

            // skill 工具按需加载（吸收自 denova skill.NewMiddleware）：提取【技能加载】独立块
            // 并剥离，按当前档位装载完整 SKILL.md 存入会话，下轮 system prompt 回填全文；lite 忽略。
            let (pre_skill_text, skill_calls) =
                crate::skill_layer::split_skill_load_calls_from_narrative(&full_text);
            if !skill_calls.is_empty() {
                let tier = crate::skill_layer::resolve_tier_for_quality(quality_bg);
                if tier != "lite" {
                    if let Some(doc) = crate::skill_layer::load_writing_skill(
                        &data_root_bg,
                        Some(&workspace_id_bg),
                        tier,
                    ) {
                        if let Ok(mut sess_skill) = sessions_store.get(&session_id_bg) {
                            sess_skill.skill_load = Some(kaleido_core::SkillLoadInfo {
                                tier: tier.to_string(),
                                markdown: crate::skill_layer::skill_full_markdown(&doc),
                            });
                            if let Err(e) = sessions_store.save(sess_skill.clone()) {
                                warn!(error = %e, "st skill load persist failed");
                            }
                            tracing::info!(n = skill_calls.len(), tier, "st skill load injected for next round");
                        }
                    }
                } else {
                    tracing::info!("st skill load ignored (lite)");
                }
                full_text = pre_skill_text;
            }

            // MCP 外设（吸收自梨园 mcp.ts）：提取【工具】块并剥离、执行、结果存 session 待下轮回填
            let (clean_mcp, mcp_calls) = if stash_mcp.is_empty() {
                crate::tavern_mcp::split_mcp_calls_from_narrative(&full_text)
            } else {
                (full_text.clone(), stash_mcp)
            };
            full_text = clean_mcp;
            let mut mcp_results: Vec<kaleido_core::ToolResultBrief> = Vec::new();
            if !mcp_calls.is_empty() {
                tracing::info!(n = mcp_calls.len(), "st mcp calls extracted+stripped");
                for call in &mcp_calls {
                    let tool_tag = format!("{}:{}", call.server, call.tool);
                    match crate::tavern_mcp::call_tool(
                        &data_root_bg,
                        &call.server,
                        &call.tool,
                        call.arguments.clone(),
                    )
                    .await
                    {
                        Ok(r) => mcp_results.push(kaleido_core::ToolResultBrief {
                            tool: tool_tag,
                            ok: true,
                            summary: r,
                        }),
                        Err(e) => mcp_results.push(kaleido_core::ToolResultBrief {
                            tool: tool_tag,
                            ok: false,
                            summary: e,
                        }),
                    }
                }
            }

            // ST-26: Actor 状态更新块解析（【状态更新】JSON，成功块剥离）
            let (clean_state, state_updates) = if stash_state.is_empty() {
                split_state_updates_from_narrative(&full_text)
            } else {
                (full_text.clone(), stash_state)
            };
            full_text = clean_state;
            if !state_updates.is_empty() {
                tracing::info!(n = state_updates.len(), "st actor state updates extracted");
            }

            // [morphling C2 2026-08-16] 章节剧情摘要（顺带总结模式）：剥离【章节摘要】块
            // 并落账本（manual_edited 保护；LLM 未输出块时静默跳过，由 fallback 提炼兜底）。
            let (clean_diary, diary_summary) = kaleido_core::chapter_diary::extract_chapter_diary_block(&full_text);
            if diary_summary.is_some() {
                full_text = clean_diary;
                if let Ok(mut s) = sessions_store.get(&session_id_bg) {
                    if let Some(ch_id) = s.chapter_cursor.clone() {
                        let ch_title = pack
                            .chapters
                            .iter()
                            .find(|c| c.id == ch_id)
                            .map(|c| c.title.clone())
                            .unwrap_or_else(|| ch_id.clone());
                        let changed = kaleido_core::chapter_diary::upsert_chapter_diary(
                            &mut s.chapter_diaries,
                            &ch_id,
                            &ch_title,
                            diary_summary.as_deref().unwrap_or(""),
                            s.turn,
                        );
                        if changed {
                            if let Err(e) = sessions_store.save(s.clone()) {
                                tracing::warn!(error = %e, "章节摘要落库失败");
                            }
                            tracing::info!(session = %session_id_bg, chapter = %ch_id, "章节摘要已随回合顺带更新");
                        }
                    }
                }
            }

            // ST-27: 规则检定块解析（【检定】JSON，action 非空块剥离）
            let (clean_check, check_requests) = if stash_check.is_empty() {
                split_check_from_narrative(&full_text)
            } else {
                (full_text.clone(), stash_check)
            };
            full_text = clean_check;
            if !check_requests.is_empty() {
                tracing::info!(n = check_requests.len(), "st rule checks extracted");
            }

            // S4 (吞噬 denova director_plan): 【导演计划】块解析（none = 无需更新；JSON = 新计划）
            let (clean_plan, director_plan_update) = if stash_plan.is_none() {
                split_director_plan_from_narrative(&full_text)
            } else {
                (full_text.clone(), stash_plan)
            };
            full_text = clean_plan;
            if director_plan_update.is_some() {
                tracing::info!("st director plan block parsed");
            }

            // ST-15: extract 【节点推进:nodeId】 marker for LLM-directed auto-advance
            // [ST-15 fix 2026-08-16] 用 extract_advance_marker 兼容【节点推进:n2】格式
            let mut llm_advance_to = None;
            if let Some((adv_pos, node_id)) = extract_advance_marker(&full_text) {
                info!(%node_id, "llm directed auto-advance");
                llm_advance_to = Some(node_id);
                // Strip the marker from visible narrative
                full_text = full_text[..adv_pos].to_string() + " ";
            }
            // heavy/standard 预剥离回填：本轮正文已无 marker 时用 stash 补上
            if llm_advance_to.is_none() && stash_advance.is_some() {
                llm_advance_to = stash_advance;
                if let Some(adv) = llm_advance_to.as_ref() {
                    info!(node_id = %adv, "st llm advance pre-refine fallback");
                }
            }

            // X2a (吞噬自 xiami skimming.rs): 读者速读质检 —— 正文定稿后做确定性检查。
            // SkimIssue（severity 1/2）转 tracing::warn 日志；仅附加信息，不阻断回合、不写回 session。
            let skim_issues = analyze_skimming(&full_text, &reader_skimming_config());
            for issue in &skim_issues {
                tracing::warn!(
                    severity = issue.severity,
                    category = %issue.category,
                    "st xiami 速读质检: {}（{}）", issue.message, issue.evidence
                );
            }
            if !skim_issues.is_empty() {
                tracing::info!(
                    n = skim_issues.len(),
                    "st xiami: 速读质检发现 {} 个问题（诊断记录，不阻断回合）",
                    skim_issues.len()
                );
            }

            if let Ok(mut sess) = sessions_store.get(&session_id_bg) {
                // X3: 把 X2a 速读质检结果写回 session（诊断展示用，不阻断回合）。
                // 空结果也写（清空旧记录），由 sessions_store.save 在回合终态落盘。
                sess.xiami_skim_issues = skim_issues;
                sess.xiami_skim_sample = full_text.chars().take(200).collect();
                let engine_tag = kaleido_core::classify_engine_tag(&user_bg);
                // P3 编排（I2）：Persisting 阶段检查点 —— 正文已定稿、即将组装 assistant
                // 消息并推进 turn；中断在此后可判「正文已产出，死在落盘/后处理」。
                sess.set_turn_progress(next_turn_bg, TurnPhase::Persisting, &run_id_bg, "persist assistant msg");

                // ST-27: 规则检定执行（rule_system 从当前 pack 宽容解析；无则剥离块但不执行）
                let mut check_result_text = String::new();
                if !check_requests.is_empty() {
                    let rule_sys = packs_store
                        .get(&sess.pack_id)
                        .ok()
                        .and_then(|pack| pack.stage_director.resolved_snapshot.and_then(|snap| snap.rule_system))
                        .as_ref()
                        .and_then(kaleido_core::RuleSystem::from_value);
                    if let Some(rs) = rule_sys {
                        for req in &check_requests {
                            let tpl = req
                                .template_id
                                .as_ref()
                                .and_then(|id| rs.checks.iter().find(|c| c.id == *id));
                            let res = kaleido_core::roll_check(req, tpl, || rand::thread_rng().gen_range(1..=20));
                            // [fix 2026-08-15] 检定结果不再带（骰面 X + 加成 Y = Z / DC → outcome）括号：
                            // 保留行动名 + 结果描述，隐藏机械骰面细节，保持叙事沉浸感。
                            // [2026-08-16] 剧情卡片：正文检定带中文结果词（隐藏机械骰面数字，保持沉浸），
                            // 前端按结果词着色彩色徽章（大成功/成功/失败/大失败）。
                            let outcome_zh = match res.outcome.as_str() {
                                "critical_success" => "大成功",
                                "success" => "成功",
                                "failure" => "失败",
                                "critical_failure" => "大失败",
                                _ => "检定",
                            };
                            check_result_text.push_str(&format!(
                                "\n【检定结果】{}（{}）\n{}\n",
                                req.action,
                                outcome_zh,
                                res.result_text
                            ));
                            // [P12 2026-08-15] 检定结果存入会话，下回合注入 system prompt 作为
                            // 「剧情约束」——失败（含 critical_failure）必须在正文真实体现，
                            // 不再只是事后 append 展示（修复「提线木偶」：检定失败零影响）。
                            // 先清空上一回合结果（只保留最近一回合的约束），再写入本次。
                            if sess.last_check_results.len() > 4 {
                                sess.last_check_results.clear();
                            }
                            sess.last_check_results.push(format!(
                                "{}（{}）",
                                req.action,
                                res.result_text
                            ));
                            // [2026-08-16] 全量检定历史：跨回合累积（区别于 last_check_results 只留最近一回合），
                            // 供导演台「检定结果」区块展示全部检定。同时记录骰面细节供前端卡片渲染。
                            sess.check_history.push(kaleido_core::CheckHistoryEntry {
                                action: req.action.clone(),
                                outcome: res.outcome.clone(),
                                result_text: res.result_text.clone(),
                                natural: res.natural,
                                total: res.total,
                                dc: res.dc,
                                state_changes: res.state_changes.clone(),
                                turn: sess.turn as u64,
                            });
                            let updated = sess.actor_states.apply_state_changes(&res.state_changes);
                            if updated > 0 {
                                tracing::info!(updated, "st check state applied");
                            }
                        }
                    } else {
                        warn!(n = check_requests.len(), "st rule checks stripped but no rule_system configured; not executed");
                    }
                }

                // 正文开头「导演自白」剥离：模型偶发把「好的，我是XX…我要怎么演」
                // 之类的内部推理写进正文开头（TEN_ROUND_PLOT_VERIFY R4/R7/R8 泄漏）。
                // 剥离段归入 reasoning 折叠展示，正文保持干净。仅匹配明确的自白前缀，
                // 不误伤正常叙事（正常正文不以「好的，我是」开头）。
                let mut story_body = format!("{}{}", full_text, check_result_text);
                let mut reasoning_merged = thinking_text.trim().to_string();
                // [fix 2026-08-15 结构根治] 统一入口结构剥离：对最终正文再做一次
                // <thinking>/<story> 标签解析（覆盖 lite 直出 / 管道失败保留初稿路径——
                // 主回合正文若被模型写入 thinking 块同样折叠，不落正文）。
                append_thinking(&mut reasoning_merged, &thinking_pipe);
                append_thinking(
                    &mut reasoning_merged,
                    &strip_fix_thinking_blocks(&mut story_body),
                );
                // [修复 2026-08-15] Heavy 管道 fix 思维前缀剥离（关键词兜底，旧模型
                // 无标签输出；新结构输出已由 strip_fix_thinking_blocks 处理）。
                let stripped_fix = strip_heavy_fix_preamble(&mut story_body);
                if !stripped_fix.is_empty() {
                    append_thinking(&mut reasoning_merged, &stripped_fix);
                }
                // [fix 2026-08-15] 尾部自检剥离（模型末尾追加「让我再检查/再检查是否需要」段）。
                let stripped_tail = strip_heavy_fix_tail(&mut story_body);
                if !stripped_tail.is_empty() {
                    append_thinking(&mut reasoning_merged, &stripped_tail);
                }
                let stripped = strip_director_preamble(&mut story_body);
                if !stripped.is_empty() {
                    append_thinking(&mut reasoning_merged, &stripped);
                }
                let reasoning_final = if reasoning_merged.is_empty() {
                    None
                } else {
                    Some(reasoning_merged)
                };

                // [Swipe] 继承 pending_swipes（reroll 旧正文），去重后挂载
                let mut inherited_swipes = std::mem::take(&mut sess.pending_swipes);
                inherited_swipes.retain(|s| s != &story_body);
                let asst_msg = TavernMessage {
                    id: format!("msg-{}", Uuid::new_v4()),
                    role: "assistant".into(),
                    content: story_body,
                    created_at: Utc::now().to_rfc3339(),
                    options: options.clone(),
                    engine_tag: Some(engine_tag),
                    program,
                    reasoning: reasoning_final,
                    // [token 显示 2026-08-16] 上游 usage.total_tokens（无则 0，前端不显示）
                    swipes: inherited_swipes,
                    swipe_index: 0,
                    tokens: usage_tokens,
                };
                sess.messages.push(asst_msg);
                sess.turn += 1;
                // [世界书定时] 每回合 tick：sticky 到期→转 cooldown；新激活条目 record。
                {
                    let clen = sess.messages.len() as i32;
                    // collect live lore keys for cooldown lookup
                    let lore_now = filter_lore_entries(&pack.lore_entries, sess.chapter_cursor.as_deref().unwrap_or(""), sess.node_id.as_deref().unwrap_or(""));
                    let by_key: std::collections::HashMap<String, &serde_json::Value> = lore_now.iter().map(|e| {
                        let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let key = if title.is_empty() { format!("pack.{}", e.get("id").and_then(|v| v.as_str()).unwrap_or("?")) } else { format!("pack.{}", title) };
                        (key, *e)
                    }).collect();
                    // tick: expire + sticky→cooldown (protected)
                    let mut ended: Vec<String> = vec![];
                    for (k, eff) in sess.timed_world_info.sticky.iter() {
                        if clen >= eff.end || clen < eff.start { ended.push(k.clone()); }
                    }
                    for k in ended {
                        if let Some(eff) = sess.timed_world_info.sticky.remove(&k) {
                            let rewound = clen < eff.start;
                            if !rewound {
                                if let Some(entry) = by_key.get(&k) {
                                    let cd = entry.get("cooldown").or_else(|| entry.get("extensions").and_then(|x| x.get("cooldown"))).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                    if cd > 0 {
                                        sess.timed_world_info.cooldown.insert(k.clone(), kaleido_core::WiTimedEffect { key: k.clone(), start: clen, end: clen + cd, protected: true });
                                    }
                                }
                            }
                        }
                    }
                    sess.timed_world_info.cooldown.retain(|_, eff| clen < eff.end && (clen >= eff.start || eff.protected));
                    // record newly activated (chapter-matched, not suppressed, has sticky/cooldown)
                    for (key, entry) in by_key {
                        if sess.timed_world_info.sticky.contains_key(&key) || sess.timed_world_info.cooldown.contains_key(&key) { continue; }
                        let st = entry.get("sticky").or_else(|| entry.get("extensions").and_then(|x| x.get("sticky"))).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        let cd = entry.get("cooldown").or_else(|| entry.get("extensions").and_then(|x| x.get("cooldown"))).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        if st > 0 {
                            sess.timed_world_info.sticky.insert(key.clone(), kaleido_core::WiTimedEffect { key, start: clen, end: clen + st, protected: false });
                        } else if cd > 0 {
                            sess.timed_world_info.cooldown.insert(key.clone(), kaleido_core::WiTimedEffect { key, start: clen, end: clen + cd, protected: false });
                        }
                    }
                }
                // [吞噬系统 auto-tick] 每回合末自动推进（与世界书定时同位置）。
                // Needs 衰减（天气联动）+ 灾变由 tick_decay 内部触发。
                {
                    let rough = sess.world_climate.atmosphere != kaleido_core::world_climate::WorldAtmosphere::Breathable;
                    let clear = sess.game_clock.weather == "晴";
                    // Needs 自动建档（present 新角色补默认）
                    for cid in sess.present_character_ids.clone() {
                        sess.needs.entry(cid).or_insert_with(kaleido_core::needs::Needs::default);
                    }
                    for needs in sess.needs.values_mut() { needs.tick_decay(rough, clear); }
                    // Chaos 压力（启用时；pending 未交付时 tick 内部跳过）
                    if sess.chaos.enabled {
                        sess.chaos.tick();
                        // roll → arm（用 turn 做确定性 roll，避免 rand 依赖；阈值内触发）
                        let roll = (sess.turn.wrapping_mul(2654435761) >> 8) % 100;
                        if sess.chaos.should_trigger(roll as u32) && !sess.chaos.has_pending() {
                            let char_name = sess.present_character_ids.first()
                                .and_then(|cid| pack.characters.iter().find(|c| c.id == *cid).map(|c| c.name.clone()))
                                .unwrap_or_else(|| "角色".into());
                            let pool_len = kaleido_core::chaos::chance_pool().len();
                            sess.chaos.arm_event(&char_name, (sess.turn as usize) % pool_len.max(1));
                        }
                    }
                    // 羁绊衰减（10 回合一步）
                    for b in sess.relationships.values_mut() { b.maybe_decay(); }
                    // Journal 冷却（在场角色）
                    let sid_c = sess.session_id.clone();
                    for cid in sess.present_character_ids.clone() { sess.journal.cool(&sid_c, &cid); }
                    // Growth 褪色（30 回合）
                    sess.growth.fade_old(sess.turn, 30);
                    // Dreams 跨夜检测（day 推进即 rollover）
                    sess.dream.check_rollover(Some(&sess.session_id.clone()), sess.game_clock.day as i32, true);
                }
                // [时间天气系统] 回合结束推进权威时钟：
                // 1) 若正文含 [时间推进: <时段|次日|N天后>] 标注 → 显式跳转；
                //    2) 否则默认顺移 1 个时段（跨日 day+1）。
                // 3) 天气：若正文场景标签带天气建议，走邻接渐进（不跳变）。
                // [时间天气系统 v2 2026-08-17] 时间推进改为「剧情信号驱动」，移除强制轮转。
                // 信号优先级（从高到低）：
                //   1) 用户消息/正文自然语言时间推进（次日/睡一觉/N天后）[extract_advance_signal]
                //   2) 正文末尾 [时间推进: X] 标注（模型/剧情标记）
                //   3) LLM 剧情评估（低频，llm_eval_due 间隔控制，省成本）
                //   4) 无任何信号 → 时间保持不变（不再每回合 +1 时段）
                // 天气：正文天气词建议（suggest_weather，邻接渐进）→ 自动轮转兜底；
                //   用户 /weather 指令走 force_weather（第一原则，见 handle_weather_cmd）。
                let mut time_jump_desc = None;
                // 1) 用户输入与正文的自然语言信号（玩家说了「睡一觉」「次日」等）
                let user_story_text = format!("{} {}", player_msg_bg, full_text);
                if let Some(sig) = kaleido_core::time_clock::GameClock::extract_advance_signal(&user_story_text) {
                    match sess.game_clock.jump(&sig, sess.turn) {
                        Ok((_, d)) => time_jump_desc = Some(d),
                        Err(_) => {} // 解析失败忽略
                    }
                }
                // 2) 正文显式 [时间推进: X] 标注（模型侧信号；玩家信号优先）
                if time_jump_desc.is_none() {
                    let advance_req: String = full_text
                        .split("时间推进:")
                        .nth(1)
                        .map(|s| s.split([']', '」', '\n']).next().unwrap_or("").trim().to_string())
                        .unwrap_or_default();
                    if !advance_req.is_empty() {
                        match sess.game_clock.jump(&advance_req, sess.turn) {
                            Ok((_, d)) => time_jump_desc = Some(d),
                            Err(_) => {} // 无法解析的标注忽略
                        }
                    }
                }
                // 3) LLM 剧情时间评估（低频：每 llm_eval_interval 回合一次）。
                //    复用 call_llm_nonstream 轻量评估剧情是否推进时间；失败/超时静默跳过（时间保持）。
                //    仅在 1/2 均无信号时触发，避免与显式信号叠加。
                if time_jump_desc.is_none()
                    && sess.game_clock.llm_eval_due(sess.turn, LLM_CLOCK_EVAL_INTERVAL)
                {
                    match call_llm_nonstream(
                        &state,
                        "你是剧情时间评估器。只输出一个 JSON：{\"advance\": true/false, \"target\": \"时段或天数或null\"}。\n\
                        依据当前时间状态、玩家输入与正文判断剧情时间是否应当推进（如过夜、赶路、等待、过了几天），\
                         不要因普通对话推进时间。target 取《清晨/上午/正午/午后/傍晚/夜晚/深夜/凌晨》或《N天后》或 null。",
                        &format!(
                            "当前时间：{}\n玩家输入：{}\n正文末尾：{}",
                            sess.game_clock.state_line(),
                            player_msg_bg,
                            full_text.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>()
                        ),
                    )
                    .await
                    {
                        Ok(out) => {
                            // 解析 {"advance":true,"target":"次日清晨"} → jump
                            let v: serde_json::Value = serde_json::from_str(&out).unwrap_or(serde_json::Value::Null);
                            let advance = v.get("advance").and_then(|x| x.as_bool()).unwrap_or(false);
                            let target = v.get("target").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                            if advance && !target.is_empty() {
                                match sess.game_clock.jump(&target, sess.turn) {
                                    Ok((_, d)) => time_jump_desc = Some(d),
                                    Err(_) => {}
                                }
                            }
                        }
                        Err(_) => {} // LLM 失败静默：时间保持
                    }
                    sess.game_clock.last_llm_eval_turn = sess.turn;
                }
                // 4) 无任何信号 → 时间保持不变（v2 移除强制顺移）。
                // 天气建议（保守：仅当解析出权威天气且邻接才采纳）；
                // 若无建议或未采纳，则按季节加权自动渐进（维持/渐进 1 级）。
                let mut suggested = false;
                if let Some(w) = kaleido_core::time_clock::WEATHERS
                    .iter()
                    .find(|w| full_text.contains(&format!("｜{}", *w)) || full_text.contains(*w))
                {
                    suggested = sess.game_clock.suggest_weather(w);
                }
                if !suggested {
                    sess.game_clock.auto_advance_weather();
                }
                tracing::debug!(
                    time = %sess.game_clock.state_line(),
                    jump = ?time_jump_desc,
                    "st game clock advanced"
                );
                sess.active_run_id = None;
                // S4 (吞噬 denova director_plan): 应用本回合导演计划块（已剥离，不显示在正文）
                if let Some(update) = director_plan_update {
                    match update {
                        DirectorPlanUpdate::Skip => {
                            // 【导演计划】none → 不动现有计划
                        }
                        DirectorPlanUpdate::Set(out) => {
                            let is_first = sess.director_plan.is_none();
                            let existing = sess.director_plan.take().unwrap_or_default();
                            sess.director_plan = Some(kaleido_core::DirectorPlan {
                                goal: out.goal,
                                pressure: out.pressure,
                                cost: out.cost,
                                hits_beats: out.hits_beats,
                                created_turn: if is_first { sess.turn } else { existing.created_turn },
                                updated_turn: sess.turn,
                                plan: existing.plan,
                                agent_brief: existing.agent_brief,
                                lore_context: existing.lore_context,
                                last_run: Some(kaleido_core::DirectorPlanRunStatus::ready("主线导演计划已更新".to_string())),
                            });
                            tracing::info!(turn = sess.turn, "st director plan updated");
                        }
                    }
                }
                // 手动触发已被本回合消费：清挂起标记（无论 LLM 返回 none 还是新计划）
                if director_instruct_bg && sess.director_pending {
                    sess.director_pending = false;
                    tracing::info!("st director pending consumed");
                }
                // 面板回写（吸收自梨园 panels.ts）：同名更新，软上限6已在提取时截断
                if !panels.is_empty() {
                    for p in &panels {
                        if let Some(existing) = sess.panels.iter_mut().find(|x| x.name == p.name) {
                            *existing = p.clone();
                        } else {
                            sess.panels.push(p.clone());
                        }
                    }
                }
                // MCP 外设结果回填（下轮 build 注入，供叙事取材）
                if !mcp_results.is_empty() {
                    sess.mcp_tool_results = mcp_results;
                }
                // ST-26: 应用并落盘 actor 状态
                let updated = sess.actor_states.apply_updates(&state_updates);
                if updated > 0 {
                    tracing::info!(updated, "st actor state applied");
                }
                // 账本写入链路（P0 补全）: 回合正文启发式提取 → Liyuan 账本（ledger.json）。
                // 零 LLM、fail-open，不阻塞回合；下回合 preload_asset_blocks 注入。
                // F3 修复：旧代码依赖 spawn 内 `let body=json!()` 的遮蔽取值，
                // body.get("message") 恒为空串；改用捕获的玩家原始消息（该变量本为此目的）。
                let turn_body = sess.messages.last().map(|m| m.content.as_str()).unwrap_or("");
                let user_text = player_msg_bg.as_str();
                ledger_upsert_from_turn(&state, &pack, &sess, turn_body, user_text);
                // Save before done — frontend picks up session on refresh
                if let Err(e) = sessions_store.save(sess.clone()) {
                    tracing::warn!(error = %e, "failed to save session before done");
                }
                // [全自动事件提取] 回合末后台 LLM：物品/承诺/成长/羁绊直写（默认开，小模型低 token）。
                // 后台执行不阻断 done；fresh 读 + CAS 写回；失败静默（下回合 prompt 仍有全量状态）。
                if sess.event_extract {
                    let store_ex = sessions_store.clone();
                    let appst_ex = state.app_state.clone();
                    let base_ex = state.llm_base.clone();
                    let key_ex = state.llm_key.clone();
                    let model_ex = state.llm_model.clone();
                    let sid_ex = session_id_bg.clone();
                    let turn_ex = sess.turn;
                    let user_ex = user_bg.clone();
                    let full_ex = full_text.clone();
                    let present_ex = sess.present_character_ids.clone();
                    let focus_ex = sess.focus_character_id.clone().unwrap_or_default();
                    let pack_chars_ex: Vec<(String,String)> = pack.characters.iter().map(|c| (c.id.clone(), c.name.clone())).collect();
                    tokio::spawn(async move {
                        tracing::info!(%sid_ex, turn = turn_ex, "st event_extract bg: start");
                        match run_event_extract(&store_ex, &appst_ex, base_ex.as_deref(), key_ex.as_deref(), &model_ex, &sid_ex, turn_ex, &user_ex, &full_ex, &present_ex, &focus_ex, &pack_chars_ex).await {
                            Ok(_) => tracing::info!(%sid_ex, "st event_extract bg: done"),
                            Err(e) => tracing::warn!(error = %e, %sid_ex, "st event_extract bg: failed, non-blocking"),
                        }
                    });
                }
                
                // Finish: send done event
                let _ = tx.send(ChatStreamEvent {
                    run_id: run_id_bg.clone(),
                    event_type: "done".into(),
                    delta: None,
                    message: None,
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                    code: None,
                });
                jobs.finish(&run_id_bg, "done");
                
                // P3 编排（I2/I3）：Done 阶段检查点 + job 进度事件收口。
                sess.set_turn_progress(sess.turn, TurnPhase::Done, &run_id_bg, "turn complete");

                // Post-turn extraction (memory + node advance)
                if let Ok(pack) = packs_store.get(&sess.pack_id) {
                    // ST-12: heuristic L2/L3 (+ light L1) from this turn text
                    let mut ext = kaleido_core::heuristic_l2_l3_from_turn(
                        &sess,
                        &user_bg,
                        &full_text,
                    );

                    // ST-30 (2026-08-15 根治兜底): 异步 LLM 识别正文外角色——语义理解
                    // 替代切词启发式，根治叙述/动作形态点名漏检（「李铁柱的声音比雨声重」）
                    // 与切碎短语误报。实测（2026-08-15）生成端 <角色清单> 内容不可靠
                    // （deepseek 形式遵守但漏列叙述点名角色）→ 本兜底**每回合都跑**，
                    // 与清单互补：清单精确比对 + 兜底补漏。best-effort 非阻塞。
                    if !full_text.trim().is_empty()
                        && !base_bg.trim().is_empty()
                        && !key_bg.trim().is_empty()
                    {
                        extra_llm_calls += 1;
                        let mut known_v: Vec<String> = pack
                            .characters
                            .iter()
                            .map(|c| c.name.clone())
                            .collect();
                        for l in &pack.lore_entries {
                            if let Some(kw) = l.get("keywords").and_then(|k| k.as_array()) {
                                for v in kw {
                                    if let Some(str) = v.as_str() {
                                        known_v.push(str.to_string());
                                    }
                                }
                            }
                        }
                        let known_joined = known_v.join("、");
                        let body_clip: String = full_text.chars().take(1500).collect();
                        let sys_roster = "你是小说角色识别器。识别叙事文本中实际出场的人物专名（姓名/称呼/外号）。\
                            严禁输出句子片段、短语、普通名词、形容词或口语碎片（如「明日柜上」「却带着点」都不是人名）。\
                            只输出 JSON 数组，如 [\"王麻子\",\"李铁柱\"]；没有则输出 []。";
                        let user_roster = format!(
                            "已知角色名单（出现不算越界，不要列出）：{}\n叙事文本：\n{}",
                            known_joined, body_clip
                        );
                        let base_llm_r = base_bg.clone();
                        let key_llm_r = key_bg.clone();
                        let model_llm_r = model_bg.clone();
                        let prov_kind_r = prov_bg.clone();
                        let sess_load_r = sessions_store.clone();
                        let sid_r = session_id_bg.clone();
                        let e_tag_r = engine_tag;
                        tokio::spawn(async move {
                            // P3.1：首发失败立即重试一次（bg_llm_with_retry）。
                            match bg_llm_with_retry(
                                &base_llm_r,
                                &key_llm_r,
                                &model_llm_r,
                                &prov_kind_r,
                                sys_roster,
                                &user_roster,
                                0.1,
                                16384,
                                45,
                            )
                            .await
                            {
                                Ok(llm_text) => {
                                    info!(%sid_r, raw = llm_text.chars().take(150).collect::<String>(), "st roster llm extract raw");
                                    let names: Vec<String> = serde_json::from_str(llm_text.trim())
                                        .unwrap_or_else(|_| {
                                            // 兼容纯文本/顿号列表：按非 JSON 兜底解析
                                            llm_text
                                                .split(['"', '，', '、', ',', '\n'])
                                                .map(|s| s.trim().trim_matches(['[', ']', '"', '，', '、', ',', '。', ' ']).to_string())
                                                .filter(|s| s.chars().count() >= 2)
                                                .collect()
                                        });
                                    // 2026-08-16 统一来源根治: 兜底名单也过滤非人名碎片——
                                    // 散文解析会 split 出「就是/我们/按理/带着点」等虚词（实测误报）。
                                    // 判定条件(全满足): >=2字 && 不含功能字 && 非代词/虚词 && 非称呼后缀。
                                    let names: Vec<String> = names
                                        .into_iter()
                                        .filter(|n| {
                                            if n.chars().count() < 2 {
                                                return false;
                                            }
                                            if n.chars().any(is_functional_char) {
                                                return false;
                                            }
                                            if matches!(
                                                n.as_str(),
"就是" |
                                                "我们" |
                                                "你们" |
                                                "他们" |
                                                "她们" |
                                                "大家" |
                                                "现在" |
                                                "然后" |
                                                "不过" |
                                                "按理" |
                                                "转而" |
                                                "改口" |
                                                "带着点" |
                                                "玩家" |
                                                "这个" |
                                                "那个" |
                                                "什么" |
                                                "怎么" |
                                                "于是" |
                                                "接着" |
                                                "随后" |
                                                "只见" |
                                                "原来" |
                                                "果然" |
                                                "忽然" |
                                                "突然" |
                                                "终于" |
                                                "毕竟" |
                                                "虽然" |
                                                "但是" |
                                                "可是" |
                                                "因为" |
                                                "所以" |
                                                "如果" |
                                                "的话" |
                                                "一下" |
                                                "起来" |
                                                "过去" |
                                                "出来" |
                                                "进来" |
                                                "回去" |
                                                "下去" |
                                                "时候" |
                                                "地方" |
                                                "东西" |
                                                "事情" |
                                                "感觉" |
                                                "声音" |
                                                "样子" |
                                                "表情" |
                                                "语气" |
                                                "意思" |
                                                "神色" |
                                                "目光" |
                                                "视线" |
                                                "脚步" |
                                                "动作"
                                            ) {
                                                return false;
                                            }
                                            if TITLE_SUFFIXES.iter().any(|t| n.ends_with(t)) {
                                                return false;
                                            }
                                            true
                                        })
                                        .collect();
                                    if let Ok(mut s) = sess_load_r.get(&sid_r) {
                                        let known_set: std::collections::HashSet<String> =
                                            known_v.iter().cloned().collect();
                                        for n in names {
                                            let n = n.trim().to_string();
                                            // 正文（本回合最后一条 assistant）确实出现才判越界
                                            // （LLM 幻觉名单防误报；历史出现过 = 已 known/已报过）
                                            if n.chars().count() >= 2
                                                && !known_set.contains(&n)
                                                && s.messages.last().map(|m| m.content.contains(&n)).unwrap_or(false)
                                            {
                                                s.guard_events.push(format!(
                                                    "[{}][{}] {}{}",
                                                    "high",
                                                    "人物",
                                                    format!("原著外对象「{}」（LLM 兜底识别）", n),
                                                    if e_tag_r == kaleido_core::EngineTag::Canon { "" } else { "（记录）" }
                                                ));
                                                info!(%sid_r, name=%n, "st roster llm fallback: 检出外角色");
                                            }
                                        }
                                        let _ = sess_load_r.save(s);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error=%e, %sid_r, "st roster llm fallback failed");
                                }
                            }
                        });
                    }

                    // ST-14: LLM extraction every 3 turns (best-effort, non-blocking)
                    if sess.turn > 0 && sess.turn % 3 == 0 && !base_bg.trim().is_empty() && !key_bg.trim().is_empty() {
                        info!(turn=sess.turn, "llm extraction trigger condition met");
                        // U11: 非阻塞抽取计入本回合 LLM 调用数。
                        extra_llm_calls += 1;
                        // Build recent conversation context for the LLM
                        let mut recent_lines = Vec::new();
                        for msg in sess.messages.iter().rev().take(6).rev() {
                            let role = if msg.role == "user" { "玩家" } else { "叙事" };
                            let content = msg.content.chars().take(300).collect::<String>();
                            recent_lines.push(format!("[{}] {}", role, content));
                        }
                        let recent_text = recent_lines.join("\n");
                        let sys_extract = "你是一个故事记忆提取器。只输出JSON。\n\
                                                    events: [{id,kind,summary,actors[],nodeId}] kind=meet|promise|secret|conflict|romance|other summary=30字\n\
                                                    edges: [{from,to,rel,note,turn}]\n\
                                                    from/to=角色名（当前在场角色或与玩家互动的角色）；\n\
                                                    rel=当前关系类型（开放枚举，从近期对话判定关系演变）：\n\
                                                    interact|tension|trust|secrets|romance 兼容旧值，外加细粒度类型：\n\
                                                    暧昧|情人|心动|依赖|疏远|敌对|母子|父女|夫妻|姐弟|兄妹|师徒|好友|同事|上下级 等。\n\
                                                    关系**演变**时（如母子→暧昧、友好→敌对）用最新关系类型，note 一句话说明演变/证据（10字）；\n\
                                                    无演变则沿用既有类型；纯普通对话无关系变化时不输出该边。\n\
                                                    facts: [短事实字符串]\n\
                                                    {\"events\":[{\"id\":\"e-1\",\"kind\":\"meet\",...}],\"edges\":[...],\"facts\":[...]}";
                        let user_extract = format!(
                            "节点：{}\n当前角色：{}\n近期对话：\n{}\n",
                            sess.node_id.as_deref().unwrap_or("?"),
                            sess.present_character_ids.join(","),
                            recent_text,
                        );

                        // Fire extraction as a separate spawned task so it doesn't block post-turn
                        let base_llm = base_bg.clone();
                        let key_llm = key_bg.clone();
                        let model_llm = model_bg.clone();
                        let sess_load = sessions_store.clone();
                        let sid = session_id_bg.clone();
                        let user_clone = user_bg.clone();
                        let cross_dir_ext = cross_dir.clone();
                        let eb_llm = embedding_base_bg.clone();
                        let prov_kind_e = prov_bg.clone();
                        tokio::spawn(async move {
                            // P3.1：首发失败立即重试一次（bg_llm_with_retry）。
                            match bg_llm_with_retry(
                                &base_llm, &key_llm, &model_llm, &prov_kind_e,
                                &sys_extract, &user_extract, 0.1, 8192, 60,
                            )
                            .await
                            {
                                Ok(llm_text) => {
                                    info!(%sid, raw = llm_text.chars().take(200).collect::<String>(), "llm extraction raw response");
                                    if let Ok(mut s) = sess_load.get(&sid) {
                                        let llm_ext = kaleido_core::parse_extraction_response(&llm_text);
                                        let e_tag = kaleido_core::classify_engine_tag(&user_clone);
                                        kaleido_core::apply_extraction(&mut s, &llm_ext, e_tag);
                                        // ST-19: persist LLM extraction to cross-session memory
                                        kaleido_core::persist_cross_session(
                                            &cross_dir_ext,
                                            &s.pack_id,
                                            &sid,
                                            s.turn,
                                            s.node_id.as_deref(),
                                            &s.present_character_ids,
                                            &llm_ext.new_events,
                                            &llm_ext.new_facts,
                                            &llm_ext.new_edges,
                                        );
                                        // ST-22: compute embeddings for LLM-extracted events
                                        if let Some(eb) = &eb_llm {
                                            if !eb.trim().is_empty() && !llm_ext.new_events.is_empty() {
                                                for ev in &llm_ext.new_events {
                                                    if let Some(stored) = s.memory_l2.events.iter_mut().find(|e| e.id == ev.id) {
                                                        if !stored.embedding.is_empty() {
                                                            continue;
                                                        }
                                                        let text = format!("{} {}", stored.kind, stored.summary);
                                                        let eb_c = eb.clone();
                                                        let client_ev = reqwest::Client::builder()
                                                            .timeout(StdDuration::from_secs(10))
                                                            .build().unwrap_or_default();
                                                        match crate::llm_stream::get_embedding(&eb_c, &text, &client_ev).await {
                                                            Ok(emb) => stored.embedding = emb,
                                                            Err(e) => tracing::warn!(error=%e, id=%ev.id, "llm event embedding failed"),
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        let _ = sess_load.save(s);
                                        info!(%sid, "llm extraction applied");
                                    } else {
                                        warn!("llm extraction no session for {sid}");
                                    }
                                }
                                Err(e) => {
                                    warn!("llm extraction fail for {sid}: {e}");
                                }
                            }
                        });
                    }

                    // P2 (叙界守卫): 生成后多维守卫 — 扩展现 ST-26 人名黑名单为
                    // 人物/节拍/出场/大纲四维检查；high → 打回（阻止推进），medium → 仅提示。
                    // 2026-08-14 修复：检测全模式跑（原绑死 engine_tag==Canon 只覆盖剧情推进模式，
                    // 说话/行为 90% 回合无保护）；打回保留 Canon 语义（跳章推进才阻止）。
                    let guard_violations = {
                        let known: std::collections::HashSet<String> = {
                            let mut s = std::collections::HashSet::new();
                            for c in &pack.characters {
                                s.insert(c.name.clone());
                            }
                            for l in &pack.lore_entries {
                                if let Some(kw) = l.get("keywords").and_then(|k| k.as_array()) {
                                    for v in kw {
                                        if let Some(str) = v.as_str() {
                                            s.insert(str.to_string());
                                        }
                                    }
                                }
                            }
                            s
                        };
                        let node = sess
                            .node_id
                            .as_deref()
                            .and_then(|nid| pack.nodes.iter().find(|n| n.id == *nid));
                        let present_names: Vec<String> = node
                            .map(|n| {
                                n.present_characters
                                    .iter()
                                    .map(|pc| {
                                        pack.characters
                                            .iter()
                                            .find(|c| c.id == *pc || c.name == *pc)
                                            .map(|c| c.name.clone())
                                            .unwrap_or_else(|| pc.clone())
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let locked_beats: Vec<String> = node
                            .map(|n| n.locked_beats.clone())
                            .unwrap_or_default();
                        let chapter_goals: Vec<String> = node
                            .map(|_| {
                                pack.chapters
                                    .iter()
                                    .find(|ch| {
                                        sess.node_id
                                            .as_ref()
                                            .map(|nid| ch.node_ids.iter().any(|x| x == nid))
                                            .unwrap_or(false)
                                    })
                                    .map(|ch| ch.goals.clone())
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default();
                        guard_narrative(
                            &full_text,
                            &chapter_body,
                            &known,
                            &present_names,
                            &locked_beats,
                            &chapter_goals,
                            roster_names.as_deref(),
                        )
                    };
                    // P2-2 冲突处理：high 打回（阻止跳章推进，留在当前节点）；记录 guard_events 供排查/展示
                    // 2026-08-14：打回仅在 Canon（剧情推进/跳章）语义下生效——说话/行为模式记录不阻断（无跳章可阻）
                    if !guard_violations.is_empty() {
                        // 2026-08-16 降噪: medium 重复提示（同角色「未出现」每轮刷屏）
                        // 只在首次出现时记录，后续相同 msg 不再追加——保留首次信号，
                        // 避免 pack presentCharacters 过宽时每轮重复噪音。
                        let deduped: Vec<GuardViolation> = guard_violations
                            .iter()
                            .filter(|v| {
                                if v.severity == GuardSeverity::High {
                                    return true;
                                }
                                let fmt = format!("[{}][{}] {}", "med", v.dim, v.msg);
                                !sess.guard_events.iter().any(|e| e.contains(&fmt))
                            })
                            .cloned()
                            .collect();
                        sess.guard_events.extend(
                            deduped
                                .iter()
                                .map(|v| {
                                    format!(
                                        "[{}][{}] {}{}",
                                        if v.severity == GuardSeverity::High { "high" } else { "med" },
                                        v.dim,
                                        v.msg,
                                        if engine_tag == kaleido_core::EngineTag::Canon { "" } else { "（记录）" }
                                    )
                                }),
                        );
                        // G15 (吞噬 denova retainedTurnsForInteractiveCompaction): 保留策略——
                        // 窗口 100 条，超窗优先淘汰 med（high 保底），长会话不无限增长。
                        sess.guard_events = kaleido_core::retain_guard_events(&sess.guard_events, 100);
                        let high_count = guard_violations
                            .iter()
                            .filter(|v| v.severity == GuardSeverity::High)
                            .count();
                        if high_count > 0 && engine_tag == kaleido_core::EngineTag::Canon {
                            llm_advance_to = None; // 打回：不推进跳章，留在当前节点
                            tracing::warn!(
                                high = high_count,
                                violations = ?guard_violations,
                                "叙界守卫: high 冲突，阻止跳章推进"
                            );
                        } else {
                            tracing::warn!(
                                violations = ?guard_violations,
                                "叙界守卫: medium 提示，不阻断"
                            );
                        }
                    }

                    kaleido_core::try_advance_node(&pack, &mut sess, engine_tag, &mut ext, llm_advance_to);
                    kaleido_core::apply_extraction(&mut sess, &ext, engine_tag);
                    // ST-19: persist heuristic extraction to cross-session memory
                    kaleido_core::persist_cross_session(
                        &cross_dir,
                        &sess.pack_id,
                        &session_id_bg,
                        sess.turn,
                        sess.node_id.as_deref(),
                        &sess.present_character_ids,
                        &ext.new_events,
                        &ext.new_facts,
                        &ext.new_edges,
                    );
                    kaleido_core::ensure_focus_character(&mut sess);
                    let _ = kaleido_core::rotate_focus_character(&mut sess);

                    // ST-22: compute embeddings for new events (BGE semantic cache)
                    if let Some(eb) = &embedding_base_bg {
                        if !eb.trim().is_empty() && !ext.new_events.is_empty() {
                            for ev in &ext.new_events {
                                // Find this event in the session (matched by id)
                                if let Some(stored) = sess.memory_l2.events.iter_mut().find(|e| e.id == ev.id) {
                                    if !stored.embedding.is_empty() {
                                        continue;
                                    }
                                    let text = format!("{} {}", stored.kind, stored.summary);
                                    let eb = eb.clone();
                                    let client = reqwest::Client::builder()
                                        .timeout(StdDuration::from_secs(10))
                                        .build()
                                        .unwrap_or_default();
                                    match crate::llm_stream::get_embedding(&eb, &text, &client).await {
                                        Ok(emb) => stored.embedding = emb,
                                        Err(e) => tracing::warn!(error=%e, id=%ev.id, "event embedding failed"),
                                    }
                                }
                            }
                        }
                    }

                    // P2 (叙界守卫): Canon tag — 注入"回归原著" narrator message
                    if engine_tag == kaleido_core::EngineTag::Canon {
                        let high_person = guard_violations.iter().find(|v| {
                            v.severity == GuardSeverity::High && v.dim == "人物"
                        });
                        let canon_note = if let Some(v) = high_person {
                            format!(
                                "〔回归原著·硬校验〕{}：请立即回到当前节点「{}」对应章节，严格按本章原著情节继续；不要编造新人物、跳章或改写关键事件。",
                                v.msg,
                                ext.advance_to_node_id.as_deref().unwrap_or("?")
                            )
                        } else {
                            format!(
                                "〔回归原著〕剧情回到主线轨道。当前节点：{}。",
                                ext.advance_to_node_id.as_deref().unwrap_or("?")
                            )
                        };
                        let canon_msg = TavernMessage {
                            id: format!("msg-{}", Uuid::new_v4()),
                            role: "assistant".into(),
                            content: canon_note,
                            created_at: Utc::now().to_rfc3339(),
                            options: vec![],
                            engine_tag: None,
                            program: None,
                            reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
                        };
                        sess.messages.push(canon_msg);
                    }

                    // L1: scene summary on turns 3, 8, 16...
                    if sess.memory_l1.scene_summary.is_empty()
                        && sess.turn >= 3
                    {
                        sess.memory_l1.scene_summary = format!(
                            "当前场景：{}（第{}回合）",
                            sess.node_id.as_deref().unwrap_or("?"),
                            sess.turn
                        );
                        sess.memory_l1.updated_at_turn = sess.turn;
                    }

                    // U11: 上下文窗口阈值驱动 epoch 压缩（替换机械 turn%8）——
                    // 基于 build_tavern_system_prompt 实际载荷近似（system prompt 字符 + 全量对话
                    // 字符 + 本轮消息 + 初稿），超过阈值才触发；压缩逻辑复用现有实现（零新引擎）。
                    let ctx_chars = estimate_turn_ctx_chars(&sess, &sys_prompt_bg, &user_bg, &full_text);
                    if should_epoch_compress(&sess, ctx_chars) {
                        // Mechanical trim first, preserving relationship state in the summary
                        // ST-FIX: 弱信号 contains("亲") 会把「母亲」误计为接吻（宿醉事件摘要全含
                        // 「母亲」→ 凭空「接过吻至少15次」），改用强信号词。
                        let kiss_count = sess.memory_l2.events.iter()
                            .filter(|e| e.kind == "romance"
                                || e.summary.contains("接吻") || e.summary.contains("亲嘴")
                                || e.summary.contains("亲吻") || e.summary.contains("亲热")
                                || e.summary.contains("吻"))
                            .count();
                        let rel_line = if kiss_count > 0 {
                            format!(" 关系已确立：至少接过吻（或亲密接触）{} 次，后续亲热描写要体现熟悉感，不要再当第一次。", kiss_count)
                        } else { String::new() };
                        let summary = format!("当前场景：{}（第{}回合记忆压缩）{}", sess.node_id.as_deref().unwrap_or("?"), sess.turn, rel_line);
                        // [V3 2026-08-17] 压缩占位携带章节语义：当前章已有章节摘要
                        // （尤其 manual_edited 确认过）时并入 L1，避免空洞占位污染后续
                        // fallback 提炼；无摘要时保持原占位（由 strip_compression_placeholder
                        // 在读取侧兜底）。diary 累积式（V1）保证此摘要覆盖整章进展。
                        let diary_hint = sess
                            .chapter_diaries
                            .iter()
                            .find(|d| {
                                d.chapter_id == sess.chapter_cursor.as_deref().unwrap_or("")
                                    && !d.summary.trim().is_empty()
                            })
                            .map(|d| {
                                format!(
                                    "\n当前章进展：{}",
                                    d.summary.chars().take(200).collect::<String>()
                                )
                            })
                            .unwrap_or_default();
                        let summary = format!("{summary}{diary_hint}");
                        kaleido_core::apply_memory_compression(&mut sess, &summary);
                        // S7 (P1-1): archive trimmed messages into session vector index BEFORE dropping.
                        // Best-effort; failure only logs. Keeps compacted details recoverable by recall.
                        if !session_id_bg.is_empty() {
                            let sess_key = format!("sess-{session_id_bg}");
                            let archived: Vec<(String, String)> = sess.messages
                                .iter()
                                .filter_map(|m| {
                                    let c = m.content.trim();
                                    if c.is_empty() || c.starts_with('[') || c.starts_with("（第") {
                                        return None;
                                    }
                                    let text = format!("{}: {}", m.role, c);
                                    let uid = format!(
                                        "m-{}-{}",
                                        m.role,
                                        kaleido_core::text_hash(&text).chars().take(10).collect::<String>()
                                    );
                                    Some((uid, text))
                                })
                                .collect();
                            if !archived.is_empty() {
                                let texts: Vec<String> = archived.iter().map(|(_, t)| t.clone()).collect();
                                let vi = state.vector_index.clone();
                                let vi_key = sess_key.clone();
                                let vi_texts = texts.clone();
                                tokio::spawn(async move {
                                    match tokio::task::spawn_blocking(move || crate::embed_local::embed_many(&vi_texts)).await {
                                        Ok(Ok(embeds)) => {
                                            let mut idx = vi.load(&vi_key);
                                            let known: std::collections::HashSet<String> =
                                                idx.entries.iter().map(|e| e.uid.clone()).collect();
                                            let mut added = 0usize;
                                            for ((uid, text), v) in archived.iter().zip(embeds.iter()) {
                                                if !known.contains(uid) && !v.is_empty() {
                                                    idx.entries.push(kaleido_core::VectorIndexEntry {
                                                        uid: uid.clone(),
                                                        world: "history".into(),
                                                        text: text.clone(),
                                                        text_hash: kaleido_core::text_hash(text),
                                                        vector: v.clone(),
                                                    });
                                                    added += 1;
                                                }
                                            }
                                            if added > 0 {
                                                match vi.save(idx) {
                                                    Ok(f) => info!(session = %vi_key, entries = f.entries.len(), "S7 history archived to vector index"),
                                                    Err(e) => warn!(error = %e, "S7 vector archive save failed"),
                                                }
                                            }
                                        }
                                        Ok(Err(e)) => warn!(error = %e, "S7 embed_many failed"),
                                        Err(e) => warn!(error = %e, "S7 embed join failed"),
                                    }
                                });
                            }
                        }
                        // P0-2(审计): 压缩后裁剪 messages 保留最近窗口（与 estimate/prompt 构建一致），
                        // 使 ctx_chars/messages.len() 判据可回落，避免越过阈值后每回合触发压缩。
                        if sess.messages.len() > KEEP_RECENT_MESSAGES {
                            let cut = sess.messages.len() - KEEP_RECENT_MESSAGES;
                            sess.messages.drain(0..cut);
                        }
                        // U11: epoch 代数记账（会话持久化，日志可见触发原因）。
                        sess.epoch += 1;
                        sess.epoch_last_turn = Some(sess.turn);
                        sess.epoch_last_chars = Some(ctx_chars.min(u32::MAX as usize) as u32);
                        info!(turn=sess.turn, epoch=sess.epoch, ctx_chars, "U11 epoch compression applied (context threshold)");

                        // [P6 2026-08-16] 压缩告警：向用户可见侧提示「上下文已压缩，
                        // 早期细节进入摘要」——防止玩家无感跳剧情（宿醉「突然跳到另一个世界」）。
                        let notice = format!(
                            "（系统提示：上下文已压缩，早期细节已归档进记忆摘要。此后剧情以最近 {} 条消息与摘要为准；若需找回早期细节可查看记忆面板或回档。）",
                            KEEP_RECENT_MESSAGES
                        );
                        sess.messages.push(TavernMessage {
                            id: format!("sys-epoch-{}-{}", sess.epoch, sess.turn),
                            role: "assistant".into(),
                            content: notice,
                            created_at: chrono::Utc::now().to_rfc3339(),
                            options: vec![],
                            engine_tag: None,
                            program: None,
                            reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
                        });

                        // Non-blocking LLM summarization
                        let (sys_compress, user_compress) = kaleido_core::build_compression_prompt(&sess, &pack);
                        if !sys_compress.trim().is_empty() && !key_bg.trim().is_empty() {
                            // U11: 非阻塞压缩计入本回合 LLM 调用数。
                            extra_llm_calls += 1;
                            let base_llm2 = base_bg.clone();
                            let key_llm2 = key_bg.clone();
                            let model_llm2 = model_bg.clone();
                            let sess_load2 = sessions_store.clone();
                            let sid2 = session_id_bg.clone();
                            let prov_kind_c = prov_bg.clone();
                            tokio::spawn(async move {
                                // P3.1：首发失败立即重试一次（bg_llm_with_retry）。
                                let result = bg_llm_with_retry(
                                    &base_llm2, &key_llm2, &model_llm2, &prov_kind_c,
                                    &sys_compress, &user_compress, 0.1, 16384, 30,
                                )
                                .await;
                                if let Ok(text) = result {
                                    let trimmed = text.trim();
                                    if !trimmed.is_empty() {
                                        if let Ok(mut s) = sess_load2.get(&sid2) {
                                            let summary2 = format!("{}（第{}回合LLM压缩归档）", trimmed, s.turn);
                                            kaleido_core::apply_memory_compression(&mut s, &summary2);
                                            let _ = sess_load2.save(s);
                                            info!(%sid2, "llm memory compression applied");
                                        }
                                    }
                                }
                            });
                        }
                    }
                }

                // AZ-2/AZ-6: realtime author live append (fail-open, never blocks turn)
                if let (Some(live), wid) = (
                    sess.author_live_path.clone(),
                    workspace_id_bg.as_str(),
                ) {
                    if !live.is_empty() && !wid.is_empty() {
                        crate::author::append_session_live(
                            &works_store,
                            wid,
                            &live,
                            sess.turn,
                            &user_bg,
                            &full_text,
                            sess.author_live_enabled,
                            sess.author_live_every_n,
                            sess.author_live_write_turns,
                        );
                    }
                }

                // U3: 场记卡——每场摘要变化时持久化快照（去重由 store 保证，不阻塞回合）
                {
                    let _ = state.scene_cards.record_if_changed(
                        &sess.pack_id,
                        &sess.session_id,
                        sess.node_id.as_deref(),
                        sess.turn,
                        &sess.memory_l1.scene_summary,
                    );
                }
                // U11: 回合成本记账 —— 写入 job payload（GET /api/v1/jobs 可见：durationMs /
                // llmCalls / estTokensIn / estTokensOut / estCostUsd / resumed / epoch）+ 会话累计账本。
                // 用现有 merge_job_payload，不改 jobs 文件格式（payload 为旁路字段）。
                {
                    let elapsed_ms_v = elapsed_ms();
                    let est = turn_cost_estimate(
                        &model_bg,
                        quality_bg,
                        &sys_prompt_bg,
                        &user_bg,
                        &full_text,
                        extra_llm_calls,
                        used_fallback,
                    );
                    let _ = jobs.merge_job_payload(
                        &run_id_bg,
                        json!({
                            "u11": {
                                "turn": sess.turn,
                                "epoch": sess.epoch,
                                "resumed": resumed_bg,
                                "durationMs": elapsed_ms_v,
                                "llmCalls": est.llm_calls,
                                "estTokensIn": est.est_in_tokens,
                                "estTokensOut": est.est_out_tokens,
                                "estCostUsd": est.est_cost_usd,
                                "error": None::<String>,
                            }
                        }),
                    );
                    sess.turn_cost_ledger.turns += 1;
                    sess.turn_cost_ledger.llm_calls += est.llm_calls;
                    sess.turn_cost_ledger.total_duration_ms += elapsed_ms_v;
                    sess.turn_cost_ledger.est_cost_usd += est.est_cost_usd;
                    // G10 (吞噬 denova D1): 回合正常完成 → 写入诊断摘要（accepted=true）。
                    sess.last_turn_diagnostic = Some(kaleido_core::TurnDiagnostic {
                        turn: sess.turn,
                        accepted: true,
                        duration_ms: elapsed_ms_v,
                        llm_ok: true,
                    });
                }

                // M-2/M-3 (2026-08-14 记忆补强): worldline 初始化 + L2 上限收缩 + L4 情感层提炼
                // 1) 世界线：首回合后恒建 main（原 current_worldline_id 初始 None，只有手动存档/fork 才建）
                if sess.current_worldline_id.is_none() {
                    sess.current_worldline_id = Some("main".into());
                }
                // 2) L2 事件账本上限收缩（原无上限，长会话无限膨胀）：超 200 条按重要性淘汰
                const L2_EVENT_CAP: usize = 200;
                if sess.memory_l2.events.len() > L2_EVENT_CAP {
                    let before_l2 = sess.memory_l2.events.len();
                    sess.memory_l2.events =
                        kaleido_core::retain_l2_events(&sess.memory_l2.events, L2_EVENT_CAP);
                    tracing::info!(
                        session = %session_id_bg,
                        before = before_l2,
                        after = sess.memory_l2.events.len(),
                        "L2 事件账本上限收缩"
                    );
                }
                // 3) L4 情感层增量提炼（原为空壳只读，零写端）：每 5 回合从最近事件提炼
                // affinity/secretsKnown/promises → merge 落库；契约校验失败降级不阻塞回合。
                if sess.turn % 5 == 0 && !sess.memory_l2.events.is_empty() {
                    let recent: String = sess
                        .memory_l2
                        .events
                        .iter()
                        .rev()
                        .take(3)
                        .map(|e| format!("[t{}][{}] {}", e.turn, e.kind, e.summary))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let user_input = format!("最近剧情事件：\n{recent}");
                    if let Ok(raw) = call_llm_nonstream(&state, L4_REFINE_SYSTEM, &user_input).await {
                        if let Some(patch) = kaleido_core::st_memory_contract::parse_l4_patch(&raw) {
                            merge_l4_patch(&mut sess.memory_l4, &patch);
                            tracing::info!(
                                session = %session_id_bg,
                                affinity = patch.affinity.len(),
                                secrets = patch.secrets_known.len(),
                                promises = patch.promises.len(),
                                "L4 情感层提炼已落库"
                            );
                        } else {
                            // [fix §10 2026-08-16] 契约校验失败：带差异提示重试一次
                            // （flash-free 偶发复述任务；追加「严格输出 JSON」指令提高遵循率），
                            // 仍失败才降级跳过 + 增强告警（带 raw 前 200 字符供排查）。
                            let retry_input = format!(
                                "最近剧情事件：\n{recent}\n\n注意：直接输出契约 JSON（affinity/secretsKnown/promises），不要复述任务说明、不要思考过程。"
                            );
                            let mut degraded = true;
                            if let Ok(raw2) = call_llm_nonstream(&state, L4_REFINE_SYSTEM, &retry_input).await {
                                if let Some(patch2) = kaleido_core::st_memory_contract::parse_l4_patch(&raw2) {
                                    merge_l4_patch(&mut sess.memory_l4, &patch2);
                                    tracing::info!(
                                        session = %session_id_bg,
                                        affinity = patch2.affinity.len(),
                                        secrets = patch2.secrets_known.len(),
                                        promises = patch2.promises.len(),
                                        "L4 提炼重试成功落库"
                                    );
                                    degraded = false;
                                }
                            }
                            if degraded {
                                tracing::warn!(
                                    session = %session_id_bg,
                                    raw = %raw.chars().take(200).collect::<String>(),
                                    "L4 提炼输出不合规（重试后仍失败），降级跳过"
                                );
                            }
                        }
                    }
                }

                // [morphling Wave B3 2026-08-16] 章节剧情摘要账本（吸收自 BakemonoMemory
                // summary-memory-model）：跨章时 + 配置阈值触发兜底提炼（默认每 10 回合或
                // 20 事件；可经 UI/API 调 diaryConfig）。manual_edited 条目不被自动覆盖；
                // 提炼失败静默降级不阻塞回合。
                if let Some(ch_id) = sess.chapter_cursor.clone() {
                    let ch_title = pack
                        .chapters
                        .iter()
                        .find(|c| c.id == ch_id)
                        .map(|c| c.title.clone())
                        .unwrap_or_else(|| ch_id.clone());
                    let crossed = sess
                        .chapter_diaries
                        .last()
                        .map(|d| d.chapter_id != ch_id)
                        .unwrap_or(false);
                    let cfg = sess.diary_config.clone().unwrap_or_default();
                    let due_by_turns = sess.turn % cfg.turn_interval.max(1) == 0;
                    let due_by_events = sess.memory_l2.events.len() as u32 >= cfg.event_threshold.max(1);
                    let idx = sess.chapter_diaries.iter().position(|d| d.chapter_id == ch_id);
                    let should_run = match idx {
                        Some(i) => {
                            let d = &sess.chapter_diaries[i];
                            !d.manual_edited && sess.turn.saturating_sub(d.updated_at_turn) >= 8
                        }
                        None => true,
                    };
                    if (crossed || due_by_turns || due_by_events) && should_run {
                        let (sys_d, user_d) = kaleido_core::build_chapter_diary_prompt(&sess, &ch_title);
                        if let Ok(raw) = call_llm_nonstream(&state, &sys_d, &user_d).await {
                            let summary = kaleido_core::parse_chapter_diary_response(&raw);
                            if !summary.is_empty() {
                                let summary_chars = summary.chars().count();
                                match idx {
                                    Some(i) => {
                                        sess.chapter_diaries[i].summary = summary.clone();
                                        sess.chapter_diaries[i].end_turn = sess.turn;
                                        sess.chapter_diaries[i].updated_at_turn = sess.turn;
                                    }
                                    None => {
                                        sess.chapter_diaries.push(kaleido_core::ChapterDiaryEntry {
                                            chapter_id: ch_id.clone(),
                                            title: ch_title,
                                            summary,
                                            start_turn: sess.turn.saturating_sub(9),
                                            end_turn: sess.turn,
                                            updated_at_turn: sess.turn,
                                            manual_edited: false,
                                        });
                                    }
                                }
                                tracing::info!(
                                    session = %session_id_bg,
                                    chapter = %ch_id,
                                    chars = summary_chars,
                                    "章节摘要已提炼"
                                );
                            }
                        }
                    }
                }

                // 以既有保留策略限制容量：core `push_checkpoint` 自带 MAX_CHECKPOINTS(30) 最旧裁剪，
                // 与 restore_checkpoint 的截断语义对齐，回合级快照不会无界增长。
                sess.push_checkpoint();
                remerge_bg_fields(&sessions_store, &mut sess);
                let _ = sessions_store.save(sess);
            }
        }

        // P3 编排（I3）：回合全链路完成 —— job 进度事件收口（progress=1.0，cursor=done）。
        let _ = jobs.push_event(
            &run_id_bg,
            kaleido_core::JobEvent::progress("turn complete", 1.0),
            Some(1.0),
            Some("phase:done".to_string()),
        );

        hub.cleanup(&run_id_bg);
    });

    // P3 编排硬化（吞噬审D 二-4 / 审查C 4-5）：worker panic 终态兜底看门狗。
    // 回合 worker 若 panic（pack 数据异常 / 索引越界等），tokio 默认吞掉 panic：
    // SSE 静默断流、job 永远 running 占满并发槽、会话锁死。此看门狗 await JoinHandle，
    // 仅在 Err（panic/abort）时兜底收尾——发 error 终态事件（hub.send 带重放持久化）+
    // job failed + U11 记账 + 清锁 + hub 清理；正常路径 worker 自行收尾，看门狗零干预。
    tokio::spawn(async move {
        if worker.await.is_err() {
            // 终态保护：若 panic 发生在 jobs.finish("done") 之后的后处理阶段，
            // job 已是终态，不得降级为 failed（回合本身已成功交付）。
            let job_still_active = jobs_panic
                .get(&run_id_panic)
                .map(|j| kaleido_core::is_active_job_status(&j.status))
                .unwrap_or(false);
            if !job_still_active {
                warn!(run_id = %run_id_panic, "P3 panic watchdog: worker panicked after terminal state; no recovery needed");
                return;
            }
            warn!(run_id = %run_id_panic, "P3 panic watchdog: turn worker panicked; recovering terminal state");
            let _ = hub_panic.send(
                &run_id_panic,
                ChatStreamEvent {
                    run_id: run_id_panic.clone(),
                    event_type: "error".into(),
                    delta: None,
                    message: Some("turn worker panicked (internal error)".into()),
                    code: Some("TURN_PANIC".into()),
                    context_compaction: None,
                    input_tokens: None,
                    output_tokens: None,
                },
            );
            let elapsed_ms_v = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
                .saturating_sub(
                    jobs_panic
                        .get(&run_id_panic)
                        .map(|j| j.created_at.timestamp_millis().max(0) as u64)
                        .unwrap_or(0),
                );
            let _ = jobs_panic.merge_job_payload(
                &run_id_panic,
                u11_accounting_json(
                    &model_panic, quality_panic, &sys_prompt_panic, &user_panic, "",
                    0, false, elapsed_ms_v, resumed_panic, 0, None, Some("worker panic"),
                ),
            );
            jobs_panic.finish(&run_id_panic, "error");
            clear_session_active_run(&sessions_store_panic, &session_id_panic, Some(&run_id_panic));
            hub_panic.cleanup(&run_id_panic);
        }
    });

    Json(json!({
        "accepted": true,
        "runId": run_id,
        "sessionId": session_id,
    }))
    .into_response()
}

async fn session_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // M-3: support short-lived SSE ticket in query (falls back to header/query token).
    let session = match crate::session_from_any(&state, &headers, Some(&params)) {
        Ok(s) => s,
        Err(r) => return r,
    };

    // Validate session (F1: ownership check)
    if let Err(e) = state.sessions_tavern.get_for_owner(&session_id, &session.user_id) {
        return map_core_err(e);
    }

    let run_id = match params.get("runId").or_else(|| params.get("run_id")) {
        Some(id) => id.to_string(),
        None => {
            return bad_request("ST_RUNID_REQUIRED", "runId required");
        }
    };

    // audit P1 IDOR: 校验 run 归属（job 存在时；查不到时放行——start 竞态窗口）
    if let Some(job) = state.jobs.get(&run_id) {
        if job.workspace_id != session.workspace_id && job.user_id != session.user_id {
            return forbidden("ST_RUN_FORBIDDEN", "run not in your workspace");
        }
    }

    let (rx, replay) = match state.hub.subscribe(&run_id) {
        Some((rx, replay)) => (rx, replay),
        None => {
            // Check if job already finished
            return match state.jobs.get(&run_id) {
                Some(job) => {
                    let status = &job.status;
                    let done = status == "done" || status == "succeeded" || status == "failed"
                        || status == "cancelled" || status == "error";
                    if done && status != "failed" && status != "error" && status != "cancelled" {
                        Json(json!({
                            "type": "result",
                            "subtype": "success",
                            "result": status,
                            "runId": run_id,
                        }))
                        .into_response()
                    } else {
                        Json(json!({
                            "type": "result",
                            "subtype": "error",
                            "result": status,
                            "runId": run_id,
                        }))
                        .into_response()
                    }
                }
                None => {
                    Json(json!({
                        "type": "result",
                        "subtype": "error",
                        "result": "not_found",
                        "runId": run_id,
                    }))
                    .into_response()
                }
            };
        }
    };

    let run_id_for_cleanup = run_id.clone();
    Sse::new(stream_from_rx(
        run_id,
        rx,
        replay,
        // L-2: cleanup hub state when the client stream ends / disconnects.
        Some(Box::new(move || state.hub.cleanup(&run_id_for_cleanup))),
    ))
    .keep_alive(KeepAlive::new().interval(StdDuration::from_secs(15)))
    .into_response()
}

// L-2: runs a callback exactly once when dropped (stream ends / client disconnects).
struct OnEndGuard(Option<Box<dyn Fn() + Send>>);
impl OnEndGuard {
    fn new(cb: Option<Box<dyn Fn() + Send>>) -> Self {
        OnEndGuard(cb)
    }
}
impl Drop for OnEndGuard {
    fn drop(&mut self) {
        if let Some(cb) = self.0.take() {
            cb();
        }
    }
}

fn stream_from_rx(
    run_id: String,
    mut rx: broadcast::Receiver<ChatStreamEvent>,
    replay: Vec<ChatStreamEvent>,
    on_end: Option<Box<dyn Fn() + Send>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let stream = async_stream::stream! {
        // L-2: ensure hub cleanup runs when the client stream ends / disconnects.
        let _guard = OnEndGuard::new(on_end);
        // F4: Replay persisted events first (reconnect support).
        let mut terminated = false;
        for evt in &replay {
            let json_str = serde_json::to_string(&json!({
                "runId": evt.run_id,
                "type": evt.event_type,
                "delta": evt.delta,
                "message": evt.message,
            }))
            .unwrap_or_default();
            match evt.event_type.as_str() {
                "done" => {
                    terminated = true;
                    yield Ok(Event::default().data(json_str));
                    break;
                }
                "error" => {
                    terminated = true;
                    yield Ok(Event::default().data(json_str));
                    break;
                }
                _ => {
                    yield Ok(Event::default().data(json_str));
                }
            }
        }
        // If replay already hit a terminal event, skip live stream.
        if !terminated {
            loop {
                match rx.recv().await {
                    Ok(evt) => {
                        let json_str = serde_json::to_string(&json!({
                            "runId": evt.run_id,
                            "type": evt.event_type,
                            "delta": evt.delta,
                            "message": evt.message,
                        }))
                        .unwrap_or_default();
                        match evt.event_type.as_str() {
                            "done" => {
                                yield Ok(Event::default().data(json_str));
                                break;
                            }
                            "error" => {
                                yield Ok(Event::default().data(json_str));
                                break;
                            }
                            _ => {
                                yield Ok(Event::default().data(json_str));
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Channel closed (worker finished or all senders dropped).
                        if !terminated {
                            let json_str = serde_json::to_string(&json!({
                                "runId": run_id,
                                "type": "error",
                                "delta": null,
                                "message": "stream ended without terminal event",
                            }))
                            .unwrap_or_default();
                            yield Ok(Event::default().data(json_str));
                        }
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Client is behind — skip missed events and continue.
                        continue;
                    }
                }
            }
        }
    };
    stream
}

async fn stop_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<StopPayload>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };

    // Validate session (F1: ownership check)
    if let Err(e) = state.sessions_tavern.get_for_owner(&session_id, &session.user_id) {
        return map_core_err(e);
    }
    if let Some(job) = state.jobs.get(&body.run_id) {
        if job.workspace_id != session.workspace_id && job.user_id != session.user_id {
            return forbidden("ST_RUN_FORBIDDEN", "run not in your workspace");
        }
    }

    state.hub.cancel(&body.run_id);
    let _ = state.jobs.cancel(&body.run_id);
    // 仅当锁属该 runId（或 pending- 占位）才解锁，绝不无条件清锁，
    // 否则会把另一在用 turn 的锁一并清掉，破坏「并发 turn 拒绝」护栏（409）。
    clear_session_active_run(&state.sessions_tavern, &session_id, Some(&body.run_id));

    // G10 (吞噬 denova D1): 回合停止后写入诊断摘要（turn / accepted=false / duration_ms / llm_ok=false）。
    if let Ok(mut sess) = state.sessions_tavern.get(&session_id) {
        let duration_ms = state
            .jobs
            .get(&body.run_id)
            .and_then(|j| {
                j.updated_at
                    .signed_duration_since(j.created_at)
                    .num_milliseconds()
                    .max(0)
                    .try_into()
                    .ok()
            })
            .unwrap_or(0u64);
        let turn = sess.turn + 1; // 在途回合号（尚未提交完成，turn 在完成时才自增）
        sess.last_turn_diagnostic = Some(kaleido_core::TurnDiagnostic {
            turn,
            accepted: false,
            duration_ms,
            llm_ok: false,
        });
        tracing::info!(%session_id, run_id = %body.run_id, turn, duration_ms, "st turn stopped (diagnostic recorded)");
        if let Err(e) = state.sessions_tavern.save(sess) {
            tracing::warn!(error = %e, %session_id, "failed to save stop_turn diagnostic");
        }
    }

    Json(json!({
        "ok": true,
        "runId": body.run_id,
        "sessionId": session_id,
    }))
    .into_response()
}

/// S4 (吞噬 denova director_plan): GET 当前导演计划。
/// 返回 `{"plan": DirectorPlan|null}`。
async fn get_director_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&session_id, &session.user_id) {
        Ok(s) => Json(json!({ "plan": s.director_plan })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// P1-1 (吞噬 denova 导演 agent): 独立导演 LLM 调用 —— 导演台手动触发时直接生成导演计划，
/// 不等下一回合。返回 Ok(Some(plan)) 成功；Ok(None) 未配置 LLM / 无需更新（null）；Err 为上游失败。
/// 输入素材：当前 node（summary + locked_beats + exits + allowed_divergence）+ 最近剧情（≤8 条）。
async fn generate_director_plan_llm(
    state: &AppState,
    sess: &TavernSession,
    pack: &StoryPack,
) -> Result<Option<DirectorPlan>, String> {
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Ok(None);
    }
    let node = sess
        .node_id
        .as_deref()
        .and_then(|nid| pack.nodes.iter().find(|n| n.id == *nid));
    let (node_summary, beats, exits, diverge) = match node {
        Some(n) => (
            n.summary.clone(),
            n.locked_beats.join("；"),
            n.exit
                .iter()
                .map(|e| format!("{}→{}", e.when, e.next))
                .collect::<Vec<_>>()
                .join("；"),
            n.allowed_divergence.clone(),
        ),
        None => (String::new(), String::new(), String::new(), String::new()),
    };
    let recent_raw = sess
        .messages
        .iter()
        .rev()
        .take(8)
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    // G4 (吞噬 denova fitTextToTokenBudget): 导演上下文预算——最近剧情按 2000 字符预算拟合，
    // 超预算头尾保留、中间省略（长会话导演上下文不溢出）。CJK 1 字符 ≈ 1 token 近似。
    const RECENT_BUDGET: usize = 2000;
    let recent_bytes = recent_raw.len();
    let recent = if recent_raw.is_empty() {
        recent_raw
    } else {
        kaleido_core::fit_text_to_token_budget(&recent_raw, RECENT_BUDGET, 0.5)
    };
    // G5 (吞噬 denova ContextLedger): 上下文账本——登记每个注入块的来源/字节/预算/是否进入最终消息，
    // 打日志供审计（长会话上下文溢出与裁剪行为可观测）。
    let ledger = vec![
        kaleido_core::DirectorLedgerEntry {
            source: "recent".into(),
            title: "最近剧情".into(),
            body_bytes: recent_bytes,
            limit: RECENT_BUDGET,
            included: recent_bytes <= RECENT_BUDGET,
            note: if recent_bytes > RECENT_BUDGET {
                format!("超预算，裁剪后 {} 字节", recent.len())
            } else {
                String::new()
            },
        },
        kaleido_core::DirectorLedgerEntry {
            source: "node_summary".into(),
            title: "当前节点".into(),
            body_bytes: node_summary.len(),
            limit: 0,
            included: true,
            note: String::new(),
        },
    ];
    tracing::debug!(ledger = ?ledger, %sess.session_id, "director ctx ledger");
    let system = "你是故事导演 agent。只输出导演计划，绝不代写剧情。导演计划是叙事意图（goal/pressure/cost/命中的 locked_beats），供主线牵引，不改变已锁定的硬事实。输出严格 JSON 对象：{\"goal\":\"…\",\"pressure\":\"…\",\"cost\":\"…\",\"hits_beats\":[\"…\"]}；无需更新时输出 null。".to_string();
    let mainline = pack.stage_director.mainline_strength.clone();
    let user = format!(
        "当前节点：{}\nlocked_beats：{}\n可用出口：{}\n允许偏离度：{}\n\n[策略档] mainline_strength={}（strong_arc=严格遵循原著主线；balanced=主线优先、允许合理分支；soft_guidance=宽松探索，旧值 soft 兼容映射）\n\n最近剧情：\n{}\n\n请生成导演计划 JSON。",
        if node_summary.is_empty() { "（起点/无节点）" } else { &node_summary },
        if beats.is_empty() { "（无）" } else { &beats },
        if exits.is_empty() { "（无）" } else { &exits },
        if diverge.is_empty() { "（默认）" } else { &diverge },
        if mainline.trim().is_empty() { "balanced" } else { &mainline },
        if recent.is_empty() { "（暂无消息）" } else { &recent },
    );
    let raw = stream_chat_completions_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        &prov_kind,
        &system,
        &user,
        0.1,
        16384,
        120,
        |_| true,
    )
    .await?;
    let trimmed = raw.trim().trim_matches('`');
    let trimmed = trimmed
        .strip_prefix("json")
        .map(|s| s.trim_matches('`').trim())
        .unwrap_or(trimmed)
        .trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("null")
        || trimmed.eq_ignore_ascii_case("none")
    {
        return Ok(None);
    }
    let v: Value =
        serde_json::from_str(trimmed).map_err(|e| format!("导演计划 JSON 解析失败: {e}"))?;
    let goal = v.get("goal").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if goal.is_empty() {
        return Ok(None);
    }
    let goal_summary = goal.clone();
    let plan = DirectorPlan {
        goal,
        pressure: v
            .get("pressure")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        cost: v
            .get("cost")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        hits_beats: v
            .get("hits_beats")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        created_turn: sess.director_plan.as_ref().map(|p| p.created_turn).unwrap_or_default(),
        updated_turn: sess.director_plan.as_ref().map(|p| p.updated_turn).unwrap_or_default(),
        // G1: 三文档（从 LLM 输出提取，缺省空串；旧逻辑无此字段 → 空）
        plan: v.get("plan").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        agent_brief: v
            .get("agent_brief")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        lore_context: v
            .get("lore_context")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        // G2: 状态机——LLM 成功 → ready
        last_run: Some(DirectorPlanRunStatus::ready(goal_summary)),
    };
    // X2c (吞噬自 xiami story_simulation.rs): 因果推演校验 —— LLM 生成导演计划后追加一次
    // 因果推演调用（复用 resolve_llm + stream_chat_completions，温度 0.1/max_tokens 2048/90s）。
    // [fix 2026-08-15] 2048→8192：模型思考挤占预算易截断。
    // 校验通过 → 把 ending_state / next_impetus 追加进导演计划 goal 描述；失败/出错静默降级
    // （保持现状行为，不阻断导演计划）。
    let plan = run_causal_simulation_llm(state, &recent, plan).await;
    Ok(Some(plan))
}

/// X2c (吞噬自 xiami story_simulation.rs): 因果推演校验 LLM 调用。
/// system prompt 用 `st_simulation::SYSTEM_PROMPT`，user prompt = 最近剧情 + 导演计划 goal/pressure。
/// 解析 JSON 为 `ChapterSimulation`（serde camelCase + vec_or_single 兼容）→ `validate_simulation`
/// 校验 → 通过后追加到导演计划（失败/LLM 出错仅 warn，降级为无推演）。
async fn run_causal_simulation_llm(
    state: &AppState,
    recent: &str,
    plan: DirectorPlan,
) -> DirectorPlan {
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return plan;
    }
    let goal = plan.goal.trim().to_string();
    let pressure = plan.pressure.as_deref().unwrap_or("").trim();
    let user = format!(
        "最近剧情：\n{}\n\n本章导演计划目标：{}\n当前压力：{}\n\n请按系统提示输出因果推演 JSON。",
        if recent.trim().is_empty() { "（暂无消息）" } else { recent.trim() },
        if goal.is_empty() { "（无）" } else { &goal },
        if pressure.is_empty() { "（无）" } else { pressure },
    );
    let raw = match stream_chat_completions_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        &prov_kind,
        SYSTEM_PROMPT,
        &user,
        0.1,
        8192,
        90,
        |_| true,
    )
    .await
    {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!(error = %e, %goal, "st xiami: 因果推演 LLM 失败，降级跳过");
            return plan;
        }
    };
    let trimmed = raw.trim().trim_matches('`');
    let trimmed = trimmed
        .strip_prefix("json")
        .map(|s| s.trim_matches('`').trim())
        .unwrap_or(trimmed)
        .trim();
    let mut sim: ChapterSimulation = match serde_json::from_str(trimmed) {
        Ok(sim) => sim,
        Err(e) => {
            tracing::warn!(error = %e, %goal, "st xiami: 因果推演 JSON 解析失败，降级跳过");
            return plan;
        }
    };
    if let Err(e) = validate_simulation(&mut sim) {
        tracing::warn!(error = %e, %goal, "st xiami: 因果推演校验未通过，降级跳过");
        return plan;
    }
    tracing::info!(
        %goal,
        beats = sim.causal_chain.len(),
        "st xiami: 因果推演通过"
    );
    // 生成人类可读推演摘要（诊断日志，仅 debug；正文侧不直接输出该骨架）。
    let sim_excerpt: String = render_simulation(&sim).chars().take(300).collect();
    tracing::debug!(
        %goal,
        sim_excerpt,
        "st xiami: 因果推演摘要（前 300 字）"
    );
    let mut plan = plan;
    let tail = format!(
        "。推演结束状态：{}。下一章推动力：{}",
        sim.ending_state, sim.next_impetus
    );
    if plan.goal.trim().is_empty() {
        plan.goal = format!("{}{}", sim.ending_state, tail);
    } else {
        plan.goal = format!("{}{}", plan.goal, tail);
    }
    plan
}

/// G13/G14 (吞噬 denova interactiveDirectorTaskDirectorPlanUpdate): 导演计划后台任务 id。
const DIRECTOR_TASK_DIRECTOR_PLAN_UPDATE: &str = "director_plan_update";

/// S4 (吞噬 denova director_plan) + G13/G14: 手动触发导演计划（后台化）。
/// 请求立即返回 `{"started":bool,"plan":…}`；LLM 生成在 `tokio::spawn` 后台执行，
/// 经 `DirectorTaskGroup`（key = session_id）串行登记——HTTP 断线不取消。
/// - `started=true`：任务已登记，完成后写回 session.director_plan（成功 → ready；
///   失败 → warn 日志 + last_run=conflict，复用 G1/G2 conflict 语义）。
/// - `started=false`：同 key 已有后台任务在跑（denova `!started` → canceled 语义：不排队、不覆盖）。
async fn run_director_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut s = match state.sessions_tavern.get_for_owner(&session_id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if s.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    let _pack = match state.packs.get(&s.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    // G13/G14: 同 key 串行登记——已有后台任务在跑则不启动（denova !started → canceled 语义）
    let group = state.director_tasks.clone();
    if !group.acquire(&session_id, DIRECTOR_TASK_DIRECTOR_PLAN_UPDATE) {
        return Json(json!({
            "started": false,
            "plan": s.director_plan,
        }))
        .into_response();
    }
    // P3.2 (编排候选①): 导演计划 job 化 —— 长时 LLM 任务登记进 JobStore，
    // GET /api/v1/jobs 可观测进度/终态；重启后由 recover_hook 孤儿清理兜底。
    // payload 记录触发来源与目标会话；model 取当前解析的 LLM 配置。
    let llm_probe = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let dp_job = match state.jobs.create(
        "director-plan",
        s.owner.as_deref().unwrap_or(&session.user_id),
        "",
        json!({
            "sessionId": session_id,
            "packId": s.pack_id,
            "trigger": "manual",
        }),
        Some(llm_probe.model.clone()),
        None,
    ) {
        Ok(j) => j,
        Err(e) => {
            group.release(&session_id);
            return map_core_err(e);
        }
    };
    let dp_run_id = dp_job.run_id.clone();
    // G13/G14: 登记当前后台任务 id（GET director-config 可见）
    s.director_task = Some(DIRECTOR_TASK_DIRECTOR_PLAN_UPDATE.into());
    let plan_now = s.director_plan.clone();
    if let Err(e) = state.sessions_tavern.save(s) {
        group.release(&session_id);
        return map_core_err(e);
    }

    let state_bg = state.clone();
    let sessions_store = state.sessions_tavern.clone();
    let packs_store = state.packs.clone();
    let session_id_bg = session_id.clone();
    let jobs_store = state.jobs.clone();
    let dp_run_id_bg = dp_run_id.clone();
    // P3.2: 起跑事件（progress 0.1）—— jobs 列表立即可见任务已启动。
    let _ = jobs_store.push_event(
        &dp_run_id,
        kaleido_core::JobEvent::progress("director: planning started", 0.1),
        Some(0.1),
        Some("stage:start".to_string()),
    );
    // 后台执行：LLM 完成写回；失败 warn 日志 + conflict；HTTP 断线不取消。
    tokio::spawn(async move {
        // P3.2: LLM 阶段事件（progress 0.3）。
        let _ = jobs_store.push_event(
            &dp_run_id_bg,
            kaleido_core::JobEvent::progress("director: llm generating plan", 0.3),
            Some(0.3),
            Some("stage:llm".to_string()),
        );
        let outcome: Result<Option<DirectorPlan>, String> = async {
            let sess = match sessions_store.get(&session_id_bg) {
                Ok(s) => s,
                Err(e) => return Err(format!("load session failed: {e}")),
            };
            if sess.pack_missing {
                return Err("session is read-only (pack missing)".to_string());
            }
            let pack = match packs_store.get(&sess.pack_id) {
                Ok(p) => p,
                Err(e) => return Err(format!("load pack failed: {e}")),
            };
            generate_director_plan_llm(&state_bg, &sess, &pack).await
        }
        .await;
        let mut fresh = match sessions_store.get(&session_id_bg) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, %session_id_bg, "director plan bg: load session failed");
                // P3.2: 会话加载失败也要给 job 终态（防幽灵 running 占并发槽）。
                let _ = jobs_store.complete(
                    &dp_run_id_bg,
                    "failed",
                    None,
                    Some(format!("load session failed: {e}")),
                );
                group.release(&session_id_bg);
                return;
            }
        };
        fresh.director_task = None;
        match &outcome {
            Ok(Some(plan)) => {
                fresh.director_plan = Some(plan.clone());
                fresh.director_pending = false;
                tracing::info!(%session_id_bg, "director plan bg: plan ready");
            }
            Ok(None) => {
                // LLM 未配置 / 模型返回 null：不清空既有计划，也不伪造成 conflict。
                tracing::info!(%session_id_bg, "director plan bg: no update (LLM unconfigured or null)");
            }
            Err(ref e) => {
                // G1/G2 conflict 语义：失败 → last_run=conflict，前端可见。
                tracing::warn!(error = %e, %session_id_bg, "director plan bg: LLM failed, mark conflict");
                let mut existing = fresh.director_plan.clone().unwrap_or_default();
                existing.last_run = Some(DirectorPlanRunStatus::conflict(format!(
                    "导演计划生成失败（上游 LLM 错误）：{e}"
                )));
                fresh.director_plan = Some(existing);
            }
        }
        // P3.2: 先取终态分类（借用）再消费 outcome，避免 Err(String) 被部分移动。
        let dp_failed_msg: Option<String> = match &outcome {
            Err(e) => Some(e.clone()),
            _ => None,
        };
        remerge_bg_fields(&sessions_store, &mut fresh);
        if let Err(e) = sessions_store.save(fresh.clone()) {
            tracing::warn!(error = %e, %session_id_bg, "director plan bg: save failed");
        }
        // P3.2 (I1): job 终态收口 —— 成功带结果摘要；None 视为无更新成功；
        // 失败标 failed 并透出错误。取消优先语义由 complete 内部保证（晚到终态不复活）。
        if dp_failed_msg.is_none() {
            if let Ok(Some(plan)) = &outcome {
                let _ = jobs_store.push_event(
                    &dp_run_id_bg,
                    kaleido_core::JobEvent::progress("director: plan ready", 0.9),
                    Some(0.9),
                    Some("stage:apply".to_string()),
                );
                let result = json!({
                    "updated": true,
                    "sessionId": session_id_bg,
                    "goal": plan.goal,
                    "pressure": plan.pressure,
                    "hitsBeats": plan.hits_beats,
                });
                let _ = jobs_store.complete(
                    &dp_run_id_bg,
                    "succeeded",
                    Some(result),
                    None,
                );
            }
        } else if matches!(&outcome, Ok(None)) {
            // Ok(None)：LLM 未配置或模型返回 null —— 无更新，仍算成功完成。
            let _ = jobs_store.complete(
                &dp_run_id_bg,
                "succeeded",
                Some(json!({
                    "updated": false,
                    "sessionId": session_id_bg,
                    "reason": "llm unconfigured or model returned null",
                })),
                None,
            );
        } else {
            let e = dp_failed_msg.unwrap_or_default();
            let _ = jobs_store.push_event(
                &dp_run_id_bg,
                kaleido_core::JobEvent::progress("director: generation failed", 1.0),
                Some(1.0),
                Some("stage:error".to_string()),
            );
            let _ = jobs_store.complete(
                &dp_run_id_bg,
                "failed",
                None,
                Some(format!("director plan generation failed: {e}")),
            );
        }
        group.release(&session_id_bg);
    });

    // P3.2: 返回 runId —— 客户端可经 GET /api/v1/jobs/{runId} 轮询导演任务进度。
    Json(json!({
        "started": true,
        "runId": dp_run_id,
        "plan": plan_now,
    }))
    .into_response()
}

/// G13 (吞噬 denova SubmitDirectorPlanUpdate): 导演 plan 提交请求体。
/// 人工/导演 LLM 均可经 HTTP 提交导演计划（比给 LLM 加工具更贴合现有架构）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectorPlanSubmitRequest {
    pub goal: String,
    #[serde(default)]
    pub pressure: Option<String>,
    #[serde(default)]
    pub cost: Option<String>,
    /// 任务书约定 snake_case `hits_beats`；camelCase 也兼容。
    #[serde(default, alias = "hits_beats")]
    pub hits_beats: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// G13 (吞噬 denova SubmitDirectorPlanUpdate): 导演 plan 更新提交。
/// 校验后写回 session.director_plan 并置 last_run=ready（reason 透出为 summary）；
/// 后台生成任务在跑时拒绝提交（避免覆盖竞态）。返回 `{"plan":…}`。
async fn submit_director_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<DirectorPlanSubmitRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let goal = body.goal.trim().to_string();
    if goal.is_empty() {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "director plan goal is required".into(),
        ));
    }
    let mut sess = match state.sessions_tavern.get_for_owner(&session_id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if sess.pack_missing {
        return map_core_err(kaleido_core::CoreError::BadRequest(
            "pack missing: session is read-only".into(),
        ));
    }
    // G13/G14: 后台导演任务在跑时拒绝提交，避免与 LLM 写回竞态。
    if state.director_tasks.is_running(&session_id) {
        return conflict("ST_DIRECTOR_BUSY", "director task running");
    }
    let mut existing = sess.director_plan.clone().unwrap_or_default();
    if existing.created_turn == 0 {
        existing.created_turn = sess.turn;
    }
    existing.goal = goal;
    existing.pressure = body.pressure;
    existing.cost = body.cost;
    existing.hits_beats = body.hits_beats;
    existing.updated_turn = sess.turn;
    // G2: 人工提交 → ready（reason 作为 summary 透出）
    let summary = match body.reason {
        Some(r) if !r.trim().is_empty() => format!("导演提交：{}", r.trim()),
        _ => "导演提交".into(),
    };
    existing.last_run = Some(DirectorPlanRunStatus::ready(summary));
    sess.director_plan = Some(existing.clone());
    sess.director_pending = false;
    sess.director_task = None;
    if let Err(e) = state.sessions_tavern.save(sess) {
        return map_core_err(e);
    }
    Json(json!({ "plan": existing })).into_response()
}

/// Seed demo pack at process start (best-effort).
pub fn bootstrap_demo(packs: &PackStore) {
    match packs.ensure_demo_pack() {
        Ok(p) => tracing::info!(pack_id = %p.id, "story-tavern demo pack ready"),
        Err(e) => tracing::warn!(error = %e, "story-tavern demo pack seed failed"),
    }
}

// ─── S5/S6 演出机读取 + 角色归档（吞噬 denova event_package / actor archive）──

/// G6 (吞噬 denova interactiveDirectorEventCatalog): 事件目录块（纯函数）。
/// 从 pack 收集 enabled 包的 enabled 卡（weight>0；modules.eventPackageIds 非空时过滤只取列出包，
/// 复用 [`kaleido_core::pick_event_card`] 的候选语义但**全量列出**，非抽一张），
/// 格式化为导演计划可引用的「可用事件目录」。一张卡一行：
///     - 包「奇遇包」：外门考核打脸（medium/打脸，冷却2）—— <prompt 前 40 字>…
/// 每包最多列 5 张（超出省略）；无候选卡返回空串（调用方不注入，保持零开销）。
fn build_event_catalog_block(pack: &StoryPack) -> String {
    let allowed = &pack.stage_director.modules.event_package_ids;
    let mut lines: Vec<String> = vec!["可用事件目录（事件卡，导演可按需调度/铺垫）：".into()];
    let mut any = false;
    for pkg in &pack.event_packages {
        if !pkg.enabled {
            continue;
        }
        if !allowed.is_empty() && !allowed.contains(&pkg.id) {
            continue;
        }
        let mut shown = 0usize;
        for card in &pkg.cards {
            if !card.enabled || card.weight == 0 {
                continue;
            }
            if shown >= 5 {
                break;
            }
            let name = if card.type_name.trim().is_empty() {
                card.title.trim().to_string()
            } else {
                card.type_name.trim().to_string()
            };
            let mut meta: Vec<&str> = Vec::new();
            if !card.intensity.trim().is_empty() {
                meta.push(card.intensity.trim());
            }
            if !card.category.trim().is_empty() {
                meta.push(card.category.trim());
            }
            let mut line = format!("- 包「{}」：{}", pkg.name, name);
            if !meta.is_empty() || card.cooldown_turns > 0 {
                let mut parens: Vec<String> = Vec::new();
                if !meta.is_empty() {
                    parens.push(meta.join("/"));
                }
                if card.cooldown_turns > 0 {
                    parens.push(format!("冷却{}", card.cooldown_turns));
                }
                line.push_str(&format!("（{}）", parens.join("，")));
            }
            let trimmed = card.prompt.trim();
            if !trimmed.is_empty() {
                let count = trimmed.chars().count();
                let preview: String = trimmed.chars().take(40).collect();
                line.push_str("—— ");
                line.push_str(&preview);
                if count > 40 {
                    line.push('…');
                }
            }
            lines.push(line);
            shown += 1;
            any = true;
        }
    }
    if !any {
        return String::new();
    }
    lines.join("\n")
}

/// 导演台配置：stage_director + 当前 director_plan + pending 状态。
async fn get_director_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    let pack = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    Json(json!({
        "stageDirector": pack.stage_director,
        "directorPlan": sess.director_plan,
        "directorPending": sess.director_pending,
        // G13/G14: 当前后台导演任务 id（None = 无后台任务在跑）。
        "directorTask": sess.director_task,
        "guardEvents": sess.guard_events.iter().rev().take(20).cloned().collect::<Vec<_>>(),
        // X3: 虾米质检成果（诊断展示用）——速读问题持久化结果；无记录时前端可判空。
        "xiami": {
            "skimIssues": sess.xiami_skim_issues,
            "skimSample": sess.xiami_skim_sample,
            "lastCheckedTurn": sess.turn,
            // X4: 章节执行合同渲染（诊断展示用；无导演计划/无数据时为 null）。
            "contract": build_director_execution_contract(&sess),
        },
    }))
    .into_response()
}

/// 校验 StageDirectorConfig 枚举字段（G8）。非法值返回错误描述，合法返回 Ok。
/// - mainlineStrength: strong_arc / balanced / soft / soft_guidance（soft 为旧值兼容）
/// - runPolicy.mode: manual / interval / on_demand
/// - failurePolicy: fail_forward / success_at_cost / blocked / hard_failure
/// - pacingCurve: 空 / wave / goal-pressure-payoff / linear
/// - eventFrequency: off / sparse / balanced / frequent
/// - ruleVisibilityMode: audit_only / public_roll
fn validate_stage_director_config(cfg: &StageDirectorConfig) -> Result<(), String> {
    let mainline = cfg.mainline_strength.trim();
    if !mainline.is_empty()
        && !["strong_arc", "balanced", "soft", "soft_guidance"].contains(&mainline)
    {
        return Err(format!("mainlineStrength 非法: {mainline}"));
    }
    let rp = &cfg.run_policy;
    if !["manual", "interval", "on_demand"].contains(&rp.mode.trim()) {
        return Err(format!("runPolicy.mode 非法: {}", rp.mode));
    }
    if !["fail_forward", "success_at_cost", "blocked", "hard_failure"].contains(&rp.failure_policy.trim()) {
        return Err(format!("failurePolicy 非法: {}", rp.failure_policy));
    }
    let pacing = rp.pacing_curve.trim();
    if !pacing.is_empty() && !["wave", "goal-pressure-payoff", "linear"].contains(&pacing) {
        return Err(format!("pacingCurve 非法: {pacing}"));
    }
    if !["off", "sparse", "balanced", "frequent"].contains(&rp.event_frequency.trim()) {
        return Err(format!("eventFrequency 非法: {}", rp.event_frequency));
    }
    if !["audit_only", "public_roll"].contains(&rp.rule_visibility_mode.trim()) {
        return Err(format!("ruleVisibilityMode 非法: {}", rp.rule_visibility_mode));
    }
    Ok(())
}

/// PUT /api/v1/story-tavern/sessions/{id}/director-config
/// 编辑导演策略：body 为完整 StageDirectorConfig（G8 含 failurePolicy/pacingCurve/eventFrequency/ruleVisibilityMode/branchPlanningTurns），
/// 校验枚举后写回 pack.stage_director。
async fn put_director_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<StageDirectorConfig>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if let Err(msg) = validate_stage_director_config(&body) {
        return unprocessable("ST_DIRECTOR_CONFIG", msg);
    }
    let mut pack = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    pack.stage_director = body;
    if let Err(e) = state.packs.save(pack) {
        return map_core_err(e);
    }
    let refreshed = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    Json(json!({
        "ok": true,
        "stageDirector": refreshed.stage_director,
    }))
    .into_response()
}

/// 事件卡包列表（含启用状态），供前端事件卡面板。
async fn get_event_packages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    let pack = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    Json(json!({
        "packId": pack.id,
        "packages": pack.event_packages,
    }))
    .into_response()
}

/// 最近一次触发的事件（S5 event log）。
async fn get_last_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => Json(json!({ "lastEvent": s.last_event })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// 角色状态系统（actors + 归档索引）。
async fn get_actor_states(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => Json(json!({ "actorStates": s.actor_states })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// 手动更新角色状态（UI 属性面板编辑）。可改 fields / add_traits / remove_traits；
/// 只更新请求中出现的字段，未提及的保持不变。批量支持（数组）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActorStatesUpdateRequest {
    /// 单个角色更新，或数组批量更新。
    #[serde(flatten)]
    single: Option<ActorStateUpdate>,
    #[serde(default)]
    updates: Vec<ActorStateUpdate>,
}

async fn get_check_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => Json(json!({ "checks": s.check_history })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// [morphling Wave B3 2026-08-16] 章节剧情摘要账本：查看（每章总结列表 + 提炼配置，供 UI 展示）。
async fn get_chapter_summaries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => {
            let cfg = s.diary_config.clone().unwrap_or_default();
            Json(json!({
                "chapterDiaries": s.chapter_diaries,
                "diaryConfig": { "turnInterval": cfg.turn_interval, "eventThreshold": cfg.event_threshold },
            }))
            .into_response()
        }
        Err(e) => map_core_err(e),
    }
}

/// [morphling Wave B3 2026-08-16] 章节剧情摘要账本：手动修改某章总结
/// （manual_edited=true → 自动提炼不再覆盖；无条目则新建）。
/// 可选携带 diaryConfig（回合数/事件数阈值）一起保存。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChapterDiaryPatch {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    diary_config: Option<kaleido_core::ChapterDiaryConfig>,
}

async fn put_chapter_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, chapter_id)): Path<(String, String)>,
    Json(body): Json<ChapterDiaryPatch>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    if let Some(cfg) = body.diary_config {
        sess.diary_config = Some(cfg);
    }
    if !body.summary.is_empty() {
        let summary = body.summary.trim().chars().take(800).collect::<String>();
        match sess.chapter_diaries.iter_mut().find(|d| d.chapter_id == chapter_id) {
            Some(d) => {
                d.summary = summary.clone();
                d.manual_edited = true;
                d.updated_at_turn = sess.turn;
            }
            None => {
                sess.chapter_diaries.push(kaleido_core::ChapterDiaryEntry {
                    chapter_id: chapter_id.clone(),
                    title: String::new(),
                    summary: summary.clone(),
                    start_turn: 0,
                    end_turn: sess.turn,
                    updated_at_turn: sess.turn,
                    manual_edited: true,
                });
            }
        }
    }
    if let Err(e) = state.sessions_tavern.save(sess) {
        return map_core_err(e);
    }
    Json(json!({ "ok": true, "chapterId": chapter_id, "summary": body.summary })).into_response()
}

async fn put_actor_states(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: axum::Json<serde_json::Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut sess = match state.sessions_tavern.get(&id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    // 兼容三种入参形态（通过 ActorStatesUpdateRequest 统一解析，serde flatten + updates 数组收集）：
    // 1) 单个对象 {characterId, fields, addTraits, removeTraits}
    // 2) 数组 [{...}, {...}]
    // 3) 包装对象 {updates: [{...}, ...]}
    let updates: Vec<ActorStateUpdate> = match body.0 {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| serde_json::from_value::<ActorStateUpdate>(v).ok())
            .collect(),
        other => {
            // 先按 Request 结构解析（收集 updates 数组）；失败再按单个 ActorStateUpdate 解析。
            if let Ok(req) = serde_json::from_value::<ActorStatesUpdateRequest>(other.clone()) {
                let mut v = req.updates;
                if let Some(u) = req.single {
                    if !u.character_id.is_empty() {
                        v.push(u);
                    }
                }
                v
            } else {
                let mut v = Vec::new();
                if let Ok(u) = serde_json::from_value::<ActorStateUpdate>(other) {
                    if !u.character_id.is_empty() {
                        v.push(u);
                    }
                }
                v
            }
        }
    };
    if updates.is_empty() {
        return bad_request("ST_NO_VALID_UPDATES", "no valid updates (need characterId + fields)");
    }
    let changed = sess.actor_states.apply_updates(&updates);
    match state.sessions_tavern.save(sess) {
        Ok(_) => Json(json!({ "ok": true, "updated": changed, "actorStates": state.sessions_tavern.get(&id).map(|s| s.actor_states).unwrap_or_default() }))
            .into_response(),
        Err(e) => map_core_err(e),
    }
}

/// [吞噬 Front Porch AI pockets.dart] 口袋与衣物（per-character, per-session）。
async fn get_pockets(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => Json(json!({ "pockets": s.pockets })).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn put_pockets(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut sess = match state.sessions_tavern.get(&id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    let map = match &body {
        serde_json::Value::Object(m) => m,
        _ => return bad_request("ST_POCKETS_BAD_BODY", "expected object {characterId: Pockets}"),
    };
    let mut updated = 0usize;
    for (cid, v) in map {
        if cid.is_empty() { continue; }
        // allow pocketsEnabled alongside pocket entries: {"cc-xxx": {...}, "pocketsEnabled": true}
        if cid == "pocketsEnabled" || cid == "enabled" {
            if let Some(b) = v.as_bool() { sess.pockets_enabled = b; }
            continue;
        }
        let p = kaleido_core::pockets::Pockets::from_json(v);
        // normalize via from_json (caps, tidy)
        sess.pockets.insert(cid.clone(), p);
        updated += 1;
    }
    // [物品记忆卡] ops 形态 {characterId, ops:[...]} → apply + 确定性写 Journal item 卡
    if let (Some(cid), Some(ops_v)) = (body.get("characterId").and_then(|v| v.as_str()), body.get("ops").and_then(|v| v.as_array())) {
        let ops: Vec<kaleido_core::pockets::PocketOpReport> = ops_v.iter().filter_map(kaleido_core::pockets::PocketOpReport::from_json).collect();
        if !ops.is_empty() && !cid.is_empty() {
            let day = sess.game_clock.day;
            let entry = sess.pockets.entry(cid.to_string()).or_default();
            let mut events: Vec<kaleido_core::pockets::PocketEvent> = vec![];
            kaleido_core::pockets::apply_pocket_ops(entry, &ops, None, day, Some(&mut events));
            let drafts = kaleido_core::pockets::item_cards_from(&events);
            let sid = sess.session_id.clone();
            for (item, content) in drafts {
                let mut card = kaleido_core::journal_store::JournalCard::new(sid.clone(), cid.to_string(), content, sess.turn);
                card.kind = Some("item".into());
                card.metadata_item = Some(item);
                card.category = "moment".into();
                sess.journal.add_card(card, 50);
            }
            updated += 1;
        }
    }
    match state.sessions_tavern.save(sess) {
        Ok(_) => Json(json!({ "ok": true, "updated": updated })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// [P1-B Porch Life À la carte] 口袋开关（默认开，Own switch）。
async fn get_pockets_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => Json(json!({ "pocketsEnabled": s.pockets_enabled })).into_response(),
        Err(e) => map_core_err(e),
    }
}

async fn put_pockets_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut sess = match state.sessions_tavern.get(&id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    let enabled = body.get("pocketsEnabled").and_then(|v| v.as_bool())
        .or_else(|| body.get("enabled").and_then(|v| v.as_bool()))
        .unwrap_or(true);
    sess.pockets_enabled = enabled;
    match state.sessions_tavern.save(sess) {
        Ok(_) => Json(json!({ "ok": true, "pocketsEnabled": enabled })).into_response(),
        Err(e) => map_core_err(e),
    }
}

/// [P2+P3 吞噬 Front Porch AI needs/growth/climate] Needs/Growth/Climate API.
async fn get_needs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) {
        Ok(sess) => Json(json!({ "needs": sess.needs })).into_response(),
        Err(e) => map_core_err(e),
    }
}
async fn put_needs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let map = match &body { serde_json::Value::Object(m)=>m, _=>return bad_request("ST_NEEDS_BAD_BODY","expected object {characterId: Needs}")};
    let mut updated=0usize;
    for (cid, v) in map {
        if cid.is_empty(){continue;}
        let n = kaleido_core::needs::Needs::from_json(v);
        sess.needs.insert(cid.clone(), n);
        updated+=1;
    }
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"updated":updated})).into_response(), Err(e)=>map_core_err(e)}
}
async fn tick_needs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let rough = sess.world_climate.atmosphere != kaleido_core::world_climate::WorldAtmosphere::Breathable;
    let clear = sess.game_clock.weather == "晴";
    for needs in sess.needs.values_mut() { needs.tick_decay(rough, clear); }
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_growth(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) {
        Ok(sess) => Json(json!({ "growth": sess.growth })).into_response(),
        Err(e) => map_core_err(e),
    }
}
async fn put_growth(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    // body: {characterId, event, strength} or {rings: [GrowthRing]}
    if let Some(arr) = body.get("rings").and_then(|v| v.as_array()) {
        let rings: Vec<kaleido_core::character_arc::GrowthRing> = arr.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect();
        sess.growth.rings = rings;
    } else {
        let cid = body.get("characterId").or_else(|| body.get("character")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ev = body.get("event").or_else(|| body.get("triggerEvent")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let strength = body.get("strength").and_then(|v| v.as_f64()).unwrap_or(0.6) as f32;
        if cid.is_empty() || ev.is_empty() { return bad_request("ST_GROWTH_BAD_BODY","need characterId + event") }
        sess.growth.strengthen(&cid, &ev, strength, sess.turn);
    }
    let g = sess.growth.clone();
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"growth":g})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_world_climate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) {
        Ok(sess) => Json(json!({ "worldClimate": sess.world_climate })).into_response(),
        Err(e) => map_core_err(e),
    }
}
async fn put_world_climate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    sess.world_climate = kaleido_core::world_climate::WorldClimate::from_json(&body);
    let wc = sess.world_climate.clone();
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"worldClimate": wc})).into_response(), Err(e)=>map_core_err(e)}
}

/// [P4 吞噬 Front Porch AI chaos/tiers/objectives/dreams] P4 API handlers.
async fn get_chaos(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"chaos": sess.chaos})).into_response(), Err(e)=>map_core_err(e)}
}
async fn put_chaos(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    if let Some(b) = body.get("enabled").and_then(|v| v.as_bool()) { sess.chaos.enabled = b; }
    if let Some(b) = body.get("nsfw").and_then(|v| v.as_bool()) { sess.chaos.nsfw = b; }
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"chaos": sess.chaos})).into_response(), Err(e)=>map_core_err(e)}
}
async fn tick_chaos(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    sess.chaos.tick();
    let should = sess.chaos.should_trigger(rand::random::<u32>() % 100);
    if should && !sess.chaos.has_pending() {
        let char_name = sess.present_character_ids.first().and_then(|cid| sess.world.entities.values().find(|e| e.id==*cid).map(|e| e.name.clone())).unwrap_or_else(|| "角色".into());
        let idx = rand::random::<usize>() % 20;
        sess.chaos.arm_event(&char_name, idx);
    }
    let c = sess.chaos.clone();
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"chaos": c, "triggered": should})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_milestones(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"milestones": sess.milestones})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_objectives(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"objectives": sess.objectives})).into_response(), Err(e)=>map_core_err(e)}
}
async fn create_objective(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if title.is_empty() { return bad_request("ST_OBJ_BAD","need title"); }
    let owner = body.get("owner").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let tasks: Vec<String> = body.get("tasks").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
    let obj = kaleido_core::objectives::Objective::new(owner, title, tasks, sess.turn);
    let ret = obj.clone();
    sess.objectives.push(obj);
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"objective": ret})).into_response(), Err(e)=>map_core_err(e)}
}
async fn update_objective(State(state): State<AppState>, headers: HeaderMap, Path((id, oid)): Path<(String,String)>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let Some(obj) = sess.objectives.iter_mut().find(|o| o.id==oid) else { return not_found("ST_OBJ_NOT_FOUND","objective not found") };
    if let Some(tid)=body.get("taskId").and_then(|v| v.as_str()) { if let Some(c)=body.get("completed").and_then(|v| v.as_bool()) { obj.mark_task(tid,c); obj.auto_complete_if_all_done(); } }
    if let Some(s)=body.get("status").and_then(|v| v.as_str()) { obj.status=s.to_string(); }
    let ret = obj.clone();
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"objective": ret})).into_response(), Err(e)=>map_core_err(e)}
}
async fn delete_objective(State(state): State<AppState>, headers: HeaderMap, Path((id, oid)): Path<(String,String)>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let before=sess.objectives.len(); sess.objectives.retain(|o| o.id!=oid);
    if sess.objectives.len()==before { return not_found("ST_OBJ_NOT_FOUND","not found") }
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_ambitions(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"ambitions": sess.ambitions})).into_response(), Err(e)=>map_core_err(e)}
}
async fn create_ambition(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let character = body.get("character").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if text.is_empty() { return bad_request("ST_AMB_BAD","need text") }
    let amb = kaleido_core::objectives::Ambition::new(character, text, sess.turn);
    let ret=amb.clone(); sess.ambitions.push(amb);
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"ambition": ret})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_dreams(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"dream": sess.dream, "episodes": sess.episodes})).into_response(), Err(e)=>map_core_err(e)}
}
async fn push_dream(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    if let Some(t)=body.get("dream").and_then(|v| v.as_str()) { sess.dream.last_dream = Some(t.to_string()); sess.dream.pending = false; }
    if let Some(b)=body.get("pending").and_then(|v| v.as_bool()) { sess.dream.pending = b; }
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"dream": sess.dream})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_episodes(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"episodes": sess.episodes})).into_response(), Err(e)=>map_core_err(e)}
}
async fn push_episode(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("episode").to_string();
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if content.is_empty() { return bad_request("ST_EP_BAD","need content") }
    sess.episodes.push(kind, content, sess.turn);
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"episodes": sess.episodes})).into_response(), Err(e)=>map_core_err(e)}
}

/// [Journal 存量 吞噬 Front Porch AI journal_store] Journal card CRUD.
async fn get_journals(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"journals": sess.journal.cards})).into_response(), Err(e)=>map_core_err(e)}
}
async fn create_journal_card(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if content.is_empty() { return bad_request("ST_JOURNAL_BAD","need content") }
    let character_id = body.get("characterId").or_else(|| body.get("character_id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if character_id.is_empty() { return bad_request("ST_JOURNAL_BAD","need characterId") }
    let category = body.get("category").and_then(|v| v.as_str()).unwrap_or("memory").to_string();
    let emotion_label = body.get("emotionLabel").or_else(|| body.get("emotion_label")).and_then(|v| v.as_str()).map(|s| s.to_string());
    let emotion_intensity = body.get("emotionIntensity").or_else(|| body.get("emotion_intensity")).and_then(|v| v.as_str()).map(|s| s.to_string());
    let kind = body.get("kind").and_then(|v| v.as_str()).map(|s| s.to_string());
    let mut card = kaleido_core::journal_store::JournalCard::new(id.clone(), character_id, content, sess.turn);
    card.category = category;
    card.emotion_label = emotion_label;
    card.emotion_intensity = emotion_intensity;
    card.kind = kind;
    let ret = card.clone();
    sess.journal.add_card(card, 50);
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"journal": ret})).into_response(), Err(e)=>map_core_err(e)}
}
async fn update_journal_card(State(state): State<AppState>, headers: HeaderMap, Path((id, card_id)): Path<(String,String)>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let content = body.get("content").and_then(|v| v.as_str()).map(|s| s.to_string());
    let feeling = body.get("feeling").or_else(|| body.get("emotionLabel")).and_then(|v| v.as_str()).map(|s| s.to_string());
    if !sess.journal.revise(&card_id, content, feeling) { return not_found("ST_JOURNAL_NOT_FOUND","card not found") }
    let ret = sess.journal.cards.iter().find(|c| c.id==card_id).cloned();
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"journal": ret})).into_response(), Err(e)=>map_core_err(e)}
}
async fn delete_journal_card(State(state): State<AppState>, headers: HeaderMap, Path((id, card_id)): Path<(String,String)>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    if !sess.journal.retire(&card_id) { return not_found("ST_JOURNAL_NOT_FOUND","card not found") }
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true})).into_response(), Err(e)=>map_core_err(e)}
}
async fn toggle_pin_journal(State(state): State<AppState>, headers: HeaderMap, Path((id, card_id)): Path<(String,String)>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let pinned = body.get("pinned").and_then(|v| v.as_bool()).unwrap_or(true);
    if !sess.journal.set_pinned(&card_id, pinned) { return not_found("ST_JOURNAL_NOT_FOUND","card not found") }
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"pinned": pinned})).into_response(), Err(e)=>map_core_err(e)}
}

/// [羁绊活数值 吞噬 Front Porch AI relationship_service] Bond/Trust API.
async fn get_relationships(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"relationships": sess.relationships})).into_response(), Err(e)=>map_core_err(e)}
}
async fn put_relationships(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    // body: {characterId, bondDelta, trustDelta, fixation, stance, withUser}
    let cid = body.get("characterId").or_else(|| body.get("character_id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if cid.is_empty() { return bad_request("ST_REL_BAD","need characterId") }
    let bond_d = body.get("bondDelta").or_else(|| body.get("bond_delta")).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let trust_d = body.get("trustDelta").or_else(|| body.get("trust_delta")).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    // [偏好加权] event 文本命中 likes/dislikes 则 bond 放大 1.5x
    let event_text = body.get("event").or_else(|| body.get("reason")).and_then(|v| v.as_str()).unwrap_or("");
    let weight = sess.preferences.get(&cid).map(|p| kaleido_core::promise::preference_weight(event_text, &p.likes, &p.dislikes)).unwrap_or(1.0);
    let bond_d = ((bond_d as f64) * weight).round() as i32;
    let b = sess.relationships.entry(cid.clone()).or_default();
    let (bond_cross, trust_cross) = b.apply_delta(bond_d, trust_d);
    if let Some(fix)=body.get("fixation").and_then(|v| v.as_str()) { b.set_fixation(fix); }
    if let Some(st)=body.get("stance").or_else(|| body.get("spatialStance")).and_then(|v| v.as_str()) { b.set_spatial(st); }
    if let Some(v)=body.get("withUser").and_then(|v| v.as_bool()) { b.with_user = Some(v); }
    // long check + decay bookkeeping + milestone
    b.maybe_long_check();
    // tier milestone + diary plant (warm/hurt/relieved/wary, salience-gated)
    let sid0 = sess.session_id.clone();
    let turn0 = sess.turn;
    let msg_n0 = sess.messages.len() as i64;
    let kick0 = sess.journal.salience_gate.allow(&sid0, msg_n0);
    if let Some(tier)=bond_cross { if let Some(m)=kaleido_core::relationship_tiers::check_milestone(&cid, "bond", tier, turn0, &sess.milestones) {
        let label = m.label.clone(); let rose = tier > 0; sess.milestones.push(m);
        if kick0 { sess.journal.plant_milestone(&sid0, &cid, format!("Bond {}{} ({})", if rose {"deepened to "} else {"cooled to "}, label, cid), "bond", rose, turn0); }
    } }
    if let Some(tier)=trust_cross { if let Some(m)=kaleido_core::relationship_tiers::check_milestone(&cid, "trust", tier, turn0, &sess.milestones) {
        let label = m.label.clone(); let rose = tier > 0; sess.milestones.push(m);
        if kick0 { sess.journal.plant_milestone(&sid0, &cid, format!("Trust {}{} ({})", if rose {"rose to "} else {"fell to "}, label, cid), "trust", rose, turn0); }
    } }
    let ret = sess.relationships.get(&cid).cloned();
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"bond": ret})).into_response(), Err(e)=>map_core_err(e)}
}
async fn tick_relationships(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let mut decayed=false;
    for b in sess.relationships.values_mut() { if b.maybe_decay() { decayed=true; } }
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"decayed": decayed, "relationships": sess.relationships})).into_response(), Err(e)=>map_core_err(e)}
}

/// [Swipe 多备选] 同条回复备选查看/切换。
async fn get_swipe(State(state): State<AppState>, headers: HeaderMap, Path((id, msg_id)): Path<(String,String)>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) {
        Ok(sess) => match sess.messages.iter().find(|m| m.id==msg_id) {
            Some(m) => Json(json!({"id": m.id, "content": m.content, "swipes": m.swipes, "swipe_index": m.swipe_index})).into_response(),
            None => not_found("ST_MSG_NOT_FOUND","message not found"),
        },
        Err(e)=>map_core_err(e),
    }
}
async fn put_swipe(State(state): State<AppState>, headers: HeaderMap, Path((id, msg_id)): Path<(String,String)>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let idx = body.get("index").or_else(|| body.get("swipe_index")).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let Some(m) = sess.messages.iter_mut().find(|m| m.id==msg_id) else { return not_found("ST_MSG_NOT_FOUND","message not found") };
    if m.role != "assistant" { return bad_request("ST_SWIPE_ROLE","only assistant messages have swipes") }
    // all versions = [content] + swipes dedup; index addresses that list
    let mut versions: Vec<String> = vec![m.content.clone()];
    for s in &m.swipes { if !versions.contains(s) { versions.push(s.clone()); } }
    if idx >= versions.len() { return bad_request("ST_SWIPE_RANGE","index out of range") }
    let new_content = versions[idx].clone();
    // rebuild swipes = all other versions
    let mut new_swipes: Vec<String> = versions.into_iter().enumerate().filter(|(i,_)| *i!=idx).map(|(_,s)| s).collect();
    // cap 10
    if new_swipes.len() > 10 { new_swipes = new_swipes[new_swipes.len()-10..].to_vec(); }
    m.content = new_content;
    m.swipes = new_swipes;
    m.swipe_index = 0;
    match state.sessions_tavern.save(sess.clone()) {
        Ok(_)=> {
            let mm = sess.messages.iter().find(|x| x.id==msg_id).cloned();
            Json(json!({"ok":true,"message": mm})).into_response()
        },
        Err(e)=>map_core_err(e),
    }
}

/// [Our Story 时间线] milestones/journal/growth/objectives/chance 聚合只读。
async fn get_storyline(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) {
        Ok(sess) => {
            let mut items: Vec<serde_json::Value> = vec![];
            for m in &sess.milestones { items.push(json!({"kind":"milestone","turn": m.turn, "text": format!("{} · {} · {}", m.character, m.label, m.kind)})); }
            for c in &sess.journal.cards { if c.pinned || c.emotion_intensity.as_deref()==Some("strong") || matches!(c.kind.as_deref(), Some("dream")|Some("milestone")|Some("promise")|Some("ambition")|Some("item")) { items.push(json!({"kind":"memory","turn": c.created_at_turn, "text": c.content, "receipts": c.source_positions, "id": c.id})); } }
            let actives = sess.growth.active_for_all();
            for (cid, rings) in actives { for r in rings { items.push(json!({"kind":"ring","turn": r.created_at_turn, "text": format!("{cid}：{}", r.trigger_event)})); } }
            for o in &sess.objectives { if o.status=="completed" { items.push(json!({"kind":"objective","turn": o.created_at_turn, "text": o.title})); } }
            for c in sess.episodes.crumbs.iter().take(20) { items.push(json!({"kind":"episode","turn": c.created_at_turn, "text": format!("[{}] {}", c.kind, c.content)})); }
            items.sort_by(|a,b| a.get("turn").and_then(|v| v.as_u64()).unwrap_or(0).cmp(&b.get("turn").and_then(|v| v.as_u64()).unwrap_or(0)));
            Json(json!({"storyline": items, "count": items.len()})).into_response()
        },
        Err(e)=>map_core_err(e),
    }
}

/// [承诺债务/偏好 吞噬 Front Porch AI promise_debt/preference] Promises + Prefs API.
async fn get_promises(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"promises": sess.promises.promises})).into_response(), Err(e)=>map_core_err(e)}
}
async fn create_promise(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if text.is_empty() { return bad_request("ST_PROMISE_BAD","need text") }
    let character = body.get("character").or_else(|| body.get("characterId")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let party = body.get("party").and_then(|v| v.as_str()).unwrap_or("char").to_string();
    let p = kaleido_core::promise::Promise::new(character, party, text, sess.turn);
    let ret=p.clone(); sess.promises.push(p);
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"promise": ret})).into_response(), Err(e)=>map_core_err(e)}
}
async fn resolve_promise(State(state): State<AppState>, headers: HeaderMap, Path((id, pid)): Path<(String,String)>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let kept = body.get("kept").and_then(|v| v.as_bool()).unwrap_or(true);
    let deltas = sess.promises.resolve(&pid, kept, sess.turn);
    // 失信/守信联动 relationship + diary promise card (salience-gated)
    let sid_p = sess.session_id.clone();
    let turn_p = sess.turn;
    let kick_p = sess.journal.salience_gate.allow(&sid_p.clone(), sess.messages.len() as i64);
    if let Some((bond_d, trust_d)) = deltas {
        let ptext = sess.promises.promises.iter().find(|p| p.id==pid).map(|p| (p.character.clone(), p.text.clone()));
        // diary promise card (kept/broken, salience-gated)
        if kick_p {
            if let Some((ch, tx)) = ptext.clone() {
                let emo = if kept {"relieved"} else {"hurt"};
                sess.journal.maybe_write_auto(&sid_p, if ch.is_empty() {"narrator"} else {ch.as_str()}, format!("Promise {}: {}", if kept {"kept"} else {"broken"}, tx), "promise", Some(emo.into()), turn_p);
            }
        }
        let character = ptext.map(|(c,_)| c).unwrap_or_default();
        if !character.is_empty() {
            let b = sess.relationships.entry(character.clone()).or_default();
            let (bc, tc) = b.apply_delta(bond_d, trust_d);
            if let Some(tier)=bc { if let Some(m)=kaleido_core::relationship_tiers::check_milestone(&character, "bond", tier, turn_p, &sess.milestones) {
                let lb=m.label.clone(); let rose=tier>0; sess.milestones.push(m);
                if kick_p { sess.journal.plant_milestone(&sid_p, &character, format!("Bond {}{}", if rose {"deepened to "} else {"cooled to "}, lb), "bond", rose, turn_p); }
            } }
            if let Some(tier)=tc { if let Some(m)=kaleido_core::relationship_tiers::check_milestone(&character, "trust", tier, turn_p, &sess.milestones) {
                let lb=m.label.clone(); let rose=tier>0; sess.milestones.push(m);
                if kick_p { sess.journal.plant_milestone(&sid_p, &character, format!("Trust {}{}", if rose {"rose to "} else {"fell to "}, lb), "trust", rose, turn_p); }
            } }
        }
    }
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"deltas": deltas.map(|(b,t)| json!({"bond":b,"trust":t}))})).into_response(), Err(e)=>map_core_err(e)}
}
async fn get_preferences(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"preferences": sess.preferences})).into_response(), Err(e)=>map_core_err(e)}
}
async fn put_preferences(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    // body: {characterId, likes:[], dislikes:[]}
    let cid = body.get("characterId").or_else(|| body.get("character_id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if cid.is_empty() { return bad_request("ST_PREF_BAD","need characterId") }
    let likes: Vec<String> = body.get("likes").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
    let dislikes: Vec<String> = body.get("dislikes").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
    sess.preferences.insert(cid, kaleido_core::promise::Prefs { likes, dislikes });
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"preferences": sess.preferences})).into_response(), Err(e)=>map_core_err(e)}
}

/// [心情/在场 吞噬 Front Porch AI mood/presence] Mood + Presence API.
async fn get_mood(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) {
        Ok(sess) => {
            let cid0 = sess.present_character_ids.first().cloned().unwrap_or_default();
            let mb = sess.needs.get(&cid0).map(|nd| {
                let m: std::collections::HashMap<String,i32> = nd.vector.clone();
                let miserable = ["暴雨","大雨","暴雪","大雪","雾","大雾"].iter().any(|x| sess.game_clock.weather.contains(x));
                let beautiful = sess.game_clock.weather=="晴";
                kaleido_core::mood_presence::derive_mood(&m, &sess.game_clock.time_of_day, miserable, beautiful)
            }).unwrap_or_else(kaleido_core::mood_presence::MoodBaseline::neutral);
            Json(json!({"mood": mb})).into_response()
        },
        Err(e)=>map_core_err(e),
    }
}
async fn get_presence(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"presence": sess.presence})).into_response(), Err(e)=>map_core_err(e)}
}
async fn put_presence(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    // body: {characterId, occupation, hours, brief, workDays}
    let cid = body.get("characterId").or_else(|| body.get("character_id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if cid.is_empty() { return bad_request("ST_PRES_BAD","need characterId") }
    let e = sess.presence.entry(cid).or_default();
    if let Some(v)=body.get("occupation").and_then(|v| v.as_str()) { e.occupation=v.to_string(); }
    if let Some(v)=body.get("hours").and_then(|v| v.as_str()) { e.hours=v.to_string(); }
    if let Some(v)=body.get("brief").and_then(|v| v.as_str()) { e.brief=v.to_string(); }
    if let Some(a)=body.get("workDays").or_else(|| body.get("work_days")).and_then(|v| v.as_array()) { e.work_days=Some(a.iter().filter_map(|v| v.as_i64().map(|x| x as i32)).collect()); }
    match state.sessions_tavern.save(sess.clone()) { Ok(_)=>Json(json!({"ok":true,"presence": sess.presence})).into_response(), Err(e)=>map_core_err(e)}
}

/// [Journal 冷卡召回] cosine(0.45, top3) + 关键词 floor；命中 rewarm。
async fn recall_journals(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let query_text = body.get("query_text").or_else(|| body.get("queryText")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let query_vector: Vec<f32> = body.get("query_vector").or_else(|| body.get("queryVector")).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect()).unwrap_or_default();
    let character_id = body.get("characterId").or_else(|| body.get("character_id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    // character scope: explicit or all present
    let cids: Vec<String> = if character_id.is_empty() {
        if sess.present_character_ids.is_empty() {
            sess.journal.cards.iter().map(|c| c.character_id.clone()).collect::<std::collections::HashSet<_>>().into_iter().collect()
        } else { sess.present_character_ids.clone() }
    } else { vec![character_id] };
    let mut recalled: Vec<serde_json::Value> = vec![];
    for cid in cids {
        let ids = sess.journal.recall_cold(&sess.session_id, &cid, &query_vector, &query_text, 0.45, 3);
        for rid in ids {
            sess.journal.rewarm(&rid);
            if let Some(c) = sess.journal.cards.iter().find(|c| c.id==rid) {
                recalled.push(json!({"id": c.id, "characterId": c.character_id, "content": c.content, "heat": c.heat}));
            }
        }
    }
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"recalled": recalled})).into_response(), Err(e)=>map_core_err(e)}
}
/// [Journal embedding 回填] 无向量卡片用 fastembed 本地补向量（无则跳过）。
async fn embed_missing_journals(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    // collect contents needing vectors
    let need: Vec<(String, String)> = sess.journal.cards.iter().filter(|c| c.embedding.is_empty()).map(|c| (c.id.clone(), c.content.clone())).collect();
    if need.is_empty() { return Json(json!({"ok":true,"embedded": 0})).into_response(); }
    // use embed_local singleton via AppState? fallback: skip if unavailable — try kaleido_server embed helper
    let mut done = 0usize;
    for (cid, content) in need.iter().take(20) {
        // best-effort local fastembed; unavailable → keep no-RAG floor
        match crate::embed_local::embed_one(content) {
            Ok(vec) => { if let Some(card) = sess.journal.cards.iter_mut().find(|c| &c.id==cid) { card.embedding = vec; done += 1; } }
            Err(_) => break,
        }
    }
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"embedded": done})).into_response(), Err(e)=>map_core_err(e)}
}

/// [全自动事件提取] 开关。
async fn get_event_extract(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) { Ok(sess)=>Json(json!({"eventExtract": sess.event_extract})).into_response(), Err(e)=>map_core_err(e)}
}
async fn put_event_extract(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    if let Err(r) = session_from(&state, &headers) { return r; }
    let mut sess = match state.sessions_tavern.get(&id) { Ok(s)=>s, Err(e)=>return map_core_err(e) };
    let v = body.get("eventExtract").or_else(|| body.get("enabled")).and_then(|x| x.as_bool()).unwrap_or(true);
    sess.event_extract = v;
    match state.sessions_tavern.save(sess) { Ok(_)=>Json(json!({"ok":true,"eventExtract": v})).into_response(), Err(e)=>map_core_err(e)}
}

/// [世界书定时] sticky/cooldown 剩余只读（侧栏 pill）。
async fn get_timed_world_info(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let s = match session_from(&state, &headers) { Ok(x)=>x, Err(r)=>return r };
    match state.sessions_tavern.get_for_owner(&id, &s.user_id) {
        Ok(sess) => {
            let clen = sess.messages.len() as i32;
            let sticky: Vec<serde_json::Value> = sess.timed_world_info.sticky.iter()
                .filter(|(_, e)| clen >= e.start && clen < e.end)
                .map(|(k, e)| json!({"key": k, "remaining": e.end - clen, "start": e.start, "end": e.end})).collect();
            let cooldown: Vec<serde_json::Value> = sess.timed_world_info.cooldown.iter()
                .filter(|(_, e)| clen >= e.start && clen < e.end)
                .map(|(k, e)| json!({"key": k, "remaining": e.end - clen})).collect();
            Json(json!({"sticky": sticky, "cooldown": cooldown, "chatLen": clen})).into_response()
        },
        Err(e)=>map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActorArchiveRequest {
    /// 只归档该角色；缺省归档全部。
    #[serde(default)]
    character_id: Option<String>,
    /// 归档原因（auto | manual | story），默认 manual。
    #[serde(default)]
    reason: Option<String>,
}

/// S6: 将角色当前状态快照写入 actor_states.archive。
async fn archive_actor_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ActorArchiveRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    // M-2 (CAS): capture revision for optimistic-concurrency save below.
    let base_revision = sess.updated_at.clone();
    let reason = body.reason.unwrap_or_else(|| "manual".into());
    let now = Utc::now().to_rfc3339();
    let mut archived = 0u32;
    match &body.character_id {
        Some(cid) => {
            if sess.actor_states.archive_actor(cid, &reason, &now).is_none() {
                return not_found("ST_ACTOR_NOT_FOUND", format!("actor not found: {cid}"));
            }
            archived = 1;
        }
        None => {
            let ids: Vec<String> = sess.actor_states.actors.keys().cloned().collect();
            for cid in ids {
                if sess.actor_states.archive_actor(&cid, &reason, &now).is_some() {
                    archived += 1;
                }
            }
        }
    }
    match state.sessions_tavern.save_with_revision(sess, &base_revision) {
        Ok(_) => Json(json!({ "ok": true, "archived": archived })).into_response(),
        Err(kaleido_core::CoreError::Conflict(_msg)) => return conflict("ST_CONCURRENT_WRITE", "session was modified concurrently; please retry"),
        Err(e) => map_core_err(e),
    }
}

/// 归档列表。
async fn list_actor_archives(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => Json(json!({ "archives": s.actor_states.archive })).into_response(),
        Err(e) => map_core_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActorArchiveRestoreRequest {
    /// 只恢复该角色最近一次归档。
    #[serde(default)]
    character_id: Option<String>,
    /// 全部恢复到各自最近一次快照。
    #[serde(default)]
    restore_all: bool,
}

/// S6: 从归档恢复角色状态（缺省恢复最近一条快照）。
async fn restore_actor_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ActorArchiveRestoreRequest>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&id, &session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };
    // M-2 (CAS): capture revision for optimistic-concurrency save below.
    let base_revision = sess.updated_at.clone();
    let mut restored: Vec<String> = Vec::new();
    match (&body.character_id, body.restore_all) {
        (Some(cid), _) => {
            if sess.actor_states.restore_actor(cid) {
                restored.push(cid.clone());
            } else {
                return not_found("ST_ACTOR_NO_ARCHIVE", format!("no archive for actor: {cid}"));
            }
        }
        (None, true) => {
            let mut seen = std::collections::HashSet::new();
            let ids: Vec<String> = sess
                .actor_states
                .archive
                .iter()
                .rev()
                .filter_map(|snap| {
                    if seen.insert(snap.character_id.clone()) {
                        Some(snap.character_id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            for cid in ids {
                if sess.actor_states.restore_actor(&cid) {
                    restored.push(cid);
                }
            }
        }
        (None, false) => {
            if let Some(snap) = sess.actor_states.archive.last() {
                let cid = snap.character_id.clone();
                if sess.actor_states.restore_actor(&cid) {
                    restored.push(cid);
                }
            }
        }
    }
    match state.sessions_tavern.save_with_revision(sess, &base_revision) {
        Ok(_) => Json(json!({ "ok": true, "restored": restored })).into_response(),
        Err(kaleido_core::CoreError::Conflict(_msg)) => return conflict("ST_CONCURRENT_WRITE", "session was modified concurrently; please retry"),
        Err(e) => map_core_err(e),
    }
}

// ─── X5: 作品级文笔风格分析（吞噬自 xiami writing_style.rs）────────────────

/// X5 (吞噬自 xiami writing_style.rs): `POST /api/v1/story-tavern/style-analysis`
/// 请求：`{ "sourceText": "...", "workId": "<可选>" }`
/// 流程：`prepare_analysis_sample` 采样校验 → 失败 400；成功走现有
/// `resolve_llm` + `stream_chat_completions`（system=12 维文笔分析提示词，
/// user=`<novel_sample>` 包装，温度 0.2 / max_tokens 4096 / 120s）。
/// 响应：`{ "stylePrompt": "...", "sampleChars": N }`。
/// workId 落库为 YAGNI 后置（pack 尚无直接 narrative_style 字段，仅返回不落库）。
pub async fn style_analysis(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StyleAnalysisRequest>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    // 采样 + 校验（过短/过长/空输入 → 400 带错误文案）。
    let sample = match kaleido_core::st_writing_style::prepare_analysis_sample(&body.source_text) {
        Ok(s) => s,
        Err(e) => {
            return bad_request("ST_BAD_REQUEST", e);
        }
    };
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    let prov_kind = crate::llm_stream::runtime_provider_kind(&llm, &state.provider_kind);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return internal("ST_LLM_NOT_CONFIGURED", "LLM not configured: base_url or api_key is empty");
    }
    let system = kaleido_core::st_writing_style::analysis_system_prompt();
    let user = kaleido_core::st_writing_style::analysis_user_prompt(&sample);
    let raw = match stream_chat_completions_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        &prov_kind,
        system,
        &user,
        0.2,
        8192,
        120,
        |_| true,
    )
    .await
    {
        Ok(raw) => raw,
        Err(e) => {
            return internal("ST_LLM_CALL_FAILED", e);
        }
    };
    // [fix 2026-08-15 style_stats 接线] 文风确定性统计（吸收自 oh-story Step 4）：
    // 句长分布/标点密度/段落节奏 为确定性测量（confidence: high），
    // 与 LLM 风格提示词互补——量化基准 + 质性描述。
    let st = kaleido_core::style_stats::compute_style_stats(&sample);
    let ps = kaleido_core::style_stats::compute_paragraph_stats(&sample);
    // [fix 2026-08-15 文风落库] workId（pack id）提供时，把 stylePrompt 落库为
    // `data/story-packs/<pack>/style-profiles/<workId>.txt`——供 /config style_source
    // 一键外挂（18+ 特化文风等）。落库失败仅告警不阻断响应。
    let mut saved_profile: Option<String> = None;
    if let Some(wid) = body.work_id.as_deref().filter(|w| !w.trim().is_empty()) {
        // workId 必须是已存在的 pack id；按标题模糊匹配兜底（用户可能传「智取」而非全 id）
        let resolved_id = {
            let packs = state.packs.list().map_err(|e| e.to_string()).unwrap_or_default();
            let q = wid.trim();
            packs
                .iter()
                .find(|p| p.id == q)
                .or_else(|| packs.iter().find(|p| p.title.contains(q) || q.contains(&p.title)))
                .map(|p| p.id.clone())
                .unwrap_or_else(|| wid.to_string())
        };
        let pack_dir = state.app_state.data_root().story_packs_dir().join(&resolved_id);
        let profile_dir = pack_dir.join("style-profiles");
        let profile_path = profile_dir.join(format!("{resolved_id}.txt"));
        if let Some(parent) = profile_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&profile_path, raw.trim()) {
            Ok(_) => saved_profile = Some(profile_path.display().to_string()),
            Err(e) => {
                tracing::warn!("文风落库失败 {wid}: {e}");
            }
        }
    }
    Json(json!({
        "stylePrompt": raw.trim(),
        "sampleChars": sample.chars().count(),
        "savedProfile": saved_profile,
        "sentenceStats": {
            "total": st.total,
            "shortLt15Pct": st.short_lt15_pct,
            "mid15to30Pct": st.mid_15to30_pct,
            "longGt30Pct": st.long_gt30_pct,
            "avgLen": st.avg_len,
            "punctDensityPct": st.punct_density_pct,
        },
        "paragraphStats": {
            "paragraphs": ps.paragraphs,
            "avgParaLen": ps.avg_para_len,
        },
    }))
    .into_response()
}

// ─── U13 M1: Character Memory Analysis / Optimization ───────────────────────

/// Helper: non-streaming LLM call returning text content.
/// 回合 worker 保存前重合并后台字段：worker sess 在回合初加载，后台任务
/// （事件提取/roster/记忆提取）在回合中落盘的增量会被 worker 的整包 save 覆盖。
/// 保存前从盘 fresh 读一次，把 7 组后台字段并回 sess（worker 本回合的同名字段以 worker 为准合并）。
fn remerge_bg_fields(store: &kaleido_core::TavernSessionStore, sess: &mut kaleido_core::TavernSession) {
    let Ok(fresh) = store.get(&sess.session_id) else { return; };
    for (cid, v) in fresh.pockets { sess.pockets.entry(cid).or_insert(v); }
    for c in fresh.journal.cards {
        if !sess.journal.cards.iter().any(|x| x.id == c.id) { sess.journal.cards.push(c); }
    }
    for pr in fresh.promises.promises {
        if !sess.promises.promises.iter().any(|x| x.id == pr.id) { sess.promises.push(pr); }
    }
    for r in fresh.growth.rings {
        if !sess.growth.rings.iter().any(|x| x.id == r.id) { sess.growth.rings.push(r); }
    }
    for (cid, b) in fresh.relationships { sess.relationships.entry(cid).or_insert(b); }
    for m in fresh.milestones {
        if !sess.milestones.iter().any(|x| x.character == m.character && x.label == m.label) { sess.milestones.push(m); }
    }
    for (cid, n) in fresh.needs { sess.needs.entry(cid).or_insert(n); }
}

/// 角色名→会话 key 归一：pack id 优先；已有 key（名或 id 任一命中）复用，防编外角色双 key 分裂；
/// 都没有则回退名本身。
fn resolve_cid(pack_chars: &[(String, String)], existing: impl Fn(&str) -> bool, name: &str) -> String {
    if let Some((id, _)) = pack_chars.iter().find(|(_, n)| n == name) { return id.clone(); }
    if existing(name) { return name.to_string(); }
    for (id, n) in pack_chars {
        if existing(id) && (n == name || id == name) { return id.clone(); }
    }
    name.to_string()
}

/// [全自动事件提取] 回合末后台 LLM：从玩家消息+本回合正文提取结构化事件，直写存储。
/// 小模型低 token（max_tokens 512, temperature 0.1）：只做提取不做创作。
/// 返回 JSON：{gives:[{from,to,item}], promises:[{character,text}], growth:[{character,event,strength}],
///  bond:[{character,bondDelta,trustDelta}], needs:[{character,need,delta}], journal:[{character,content,kind}]}。
/// 全部 fail-open：解析失败/空即跳过；写前去重；salience 门限流种卡。
async fn run_event_extract(
    store: &kaleido_core::TavernSessionStore,
    appst: &kaleido_core::AppStateStore,
    base: Option<&str>,
    key: Option<&str>,
    model: &str,
    session_id: &str,
    turn: u32,
    user_msg: &str,
    full_text: &str,
    present: &[String],
    focus: &str,
    pack_chars: &[(String, String)],
) -> Result<(), String> {
    let names: Vec<String> = pack_chars.iter().map(|(_, n)| n.clone()).collect();
    let name_of = |cid: &str| pack_chars.iter().find(|(id, _)| id == cid).map(|(_, n)| n.clone()).unwrap_or_else(|| cid.to_string());
    let sys = format!(
        "你是剧情事件提取器。只做提取，不创作、不续写。角色：{}。只输出 JSON（无 markdown 包裹），六个数组键必须全有（gives/promises/growth/bond/needs/journal），无事件则为空数组。规则：gives 包括人物间给予（给出/递出/塞入/放在桌上，填 from/to/item）与自取拾取（拿起/收起/攥在手心/装进口袋，to 填玩家，from 可空）；promises 仅当明确将来时承诺；growth：任何角色态度软化/强硬/透露身世/情绪波动都算一次（strength 0.3-0.8）；bond：任何好感/信任增减都给分（正负1到10），初次善意至少+2；journal：每回合至少记1条本回合最值得记住的事（kind=moment）；needs：疲惫/饥饿/寒冷/口渴等生理信号出现才记。",
        names.join("、")
    );
    let user = format!("玩家输入：{}\n\n本回合正文：{}\n\n在场角色 id：{}", user_msg, full_text.chars().take(3000).collect::<String>(), present.join(","));
    let raw = call_llm_extract_with(appst, base, key, model, &sys, &user).await?;
    let v: serde_json::Value = crate::llm_stream::extract_json_value(&raw)
        .or_else(|| serde_json::from_str(&raw).ok())
        .ok_or_else(|| "extract json parse failed".to_string())?;
    let mut sess = store.get(session_id).map_err(|e| e.to_string())?;
    let sid = sess.session_id.clone();
    let mut dirty = false;
    {
        let counts = ["gives","promises","growth","bond","needs","journal"].iter()
            .map(|k| format!("{}={}", k, v.get(k).and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0)))
            .collect::<Vec<_>>().join(" ");
        let all_zero = ["gives","promises","growth","bond","needs","journal"].iter()
            .all(|k| v.get(k).and_then(|x| x.as_array()).map(|a| a.is_empty()).unwrap_or(true));
        if all_zero {
            tracing::info!(%session_id, turn, counts = %counts, raw_head = %raw.chars().take(800).collect::<String>(), "st event_extract bg: parsed ZERO");
        } else {
            tracing::info!(%session_id, turn, counts = %counts, raw_len = raw.len(), "st event_extract bg: parsed");
        }
    }
    // gives → pockets apply + item journal card
    if let Some(arr) = v.get("gives").and_then(|x| x.as_array()) {
        for g in arr {
            let (from_name, to_name, item) = (
                g.get("from").and_then(|x| x.as_str()).unwrap_or("").trim(),
                g.get("to").or_else(|| g.get("target")).or_else(|| g.get("character")).and_then(|x| x.as_str()).unwrap_or("").trim(),
                g.get("item").or_else(|| g.get("itemName")).or_else(|| g.get("name")).and_then(|x| x.as_str()).unwrap_or("").trim(),
            );
            if item.is_empty() { continue; }
            // to 缺省/玩家 → 自取 Pickup（narrator 口袋）；to 明确他人 → Give 转移（扣 from 加 to）。
            let to_is_player = to_name.is_empty() || to_name.contains("玩家") || to_name.to_lowercase().contains("player");
            let day = sess.game_clock.day;
            if to_is_player {
                let ops = vec![kaleido_core::pockets::PocketOpReport { kind: kaleido_core::pockets::PocketOpKind::Pickup, item: item.to_string(), to: String::new(), state: String::new(), where_: String::new() }];
                let entry = sess.pockets.entry("narrator".to_string()).or_default();
                let mut events: Vec<kaleido_core::pockets::PocketEvent> = vec![];
                kaleido_core::pockets::apply_pocket_ops(entry, &ops, None, day, Some(&mut events));
                for (it, content) in kaleido_core::pockets::item_cards_from(&events) {
                    let mut card = kaleido_core::journal_store::JournalCard::new(sid.clone(), "narrator".to_string(), content, turn);
                    card.kind = Some("item".into());
                    card.metadata_item = Some(it);
                    card.category = "moment".into();
                    sess.journal.add_card(card, 50);
                }
                dirty = true;
            } else {
                let to_cid = resolve_cid(pack_chars, |k| sess.pockets.contains_key(k), to_name);
                let from_cid = if from_name.is_empty() { "narrator".to_string() }
                    else if from_name.contains("玩家") || from_name.to_lowercase().contains("player") { "narrator".into() }
                    else { resolve_cid(pack_chars, |k| sess.pockets.contains_key(k), from_name) };
                // 先从 from 扣（Give 会清掉 from 持有的同名物），再给 to 加
                let ops = vec![kaleido_core::pockets::PocketOpReport { kind: kaleido_core::pockets::PocketOpKind::Give, item: item.to_string(), to: to_cid.clone(), state: String::new(), where_: String::new() }];
                let mut moved: Vec<(String, kaleido_core::pockets::PocketItem)> = vec![];
                {
                    let entry = sess.pockets.entry(from_cid.clone()).or_default();
                    let mut cb = |to: String, it: kaleido_core::pockets::PocketItem| { moved.push((to, it)); };
                    let mut events: Vec<kaleido_core::pockets::PocketEvent> = vec![];
                    kaleido_core::pockets::apply_pocket_ops(entry, &ops, Some(&mut cb), day, Some(&mut events));
                    for (it, content) in kaleido_core::pockets::item_cards_from(&events) {
                        let mut card = kaleido_core::journal_store::JournalCard::new(sid.clone(), from_cid.clone(), content, turn);
                        card.kind = Some("item".into());
                        card.metadata_item = Some(it);
                        card.category = "moment".into();
                        sess.journal.add_card(card, 50);
                    }
                }
                for (to, it) in moved {
                    let dest = sess.pockets.entry(to.clone()).or_default();
                    if !dest.carrying.iter().any(|x| kaleido_core::pockets::same_item(&x.name, &it.name)) {
                        dest.carrying.push(it);
                    }
                }
                // from==narrator 且持有 → 上面 Give 已扣；若 from 无此物（凭空给）→ to 直接 Pickup 补
                if sess.pockets.get(&to_cid).map(|p| !p.carrying.iter().any(|x| kaleido_core::pockets::same_item(&x.name, item))).unwrap_or(true) {
                    let dest = sess.pockets.entry(to_cid.clone()).or_default();
                    let ops2 = vec![kaleido_core::pockets::PocketOpReport { kind: kaleido_core::pockets::PocketOpKind::Pickup, item: item.to_string(), to: String::new(), state: String::new(), where_: String::new() }];
                    let mut events2: Vec<kaleido_core::pockets::PocketEvent> = vec![];
                    kaleido_core::pockets::apply_pocket_ops(dest, &ops2, None, day, Some(&mut events2));
                }
                dirty = true;
            }
        }
    }
    tracing::info!(%session_id, turn, gives_raw = %v.get("gives").map(|x| x.to_string()).unwrap_or_default().chars().take(500).collect::<String>(), "st event_extract bg: gives raw");
    // promises → promise store
    if let Some(arr) = v.get("promises").and_then(|x| x.as_array()) {
        for pr in arr {
            let (ch_name, text) = (
                pr.get("character").and_then(|x| x.as_str()).unwrap_or("").trim(),
                pr.get("text").or_else(|| pr.get("promise")).or_else(|| pr.get("content")).and_then(|x| x.as_str()).unwrap_or("").trim(),
            );
            if text.is_empty() { continue; }
            let cid = if ch_name.is_empty() { "narrator".into() } else { resolve_cid(pack_chars, |k| sess.promises.promises.iter().any(|x| x.character == k), ch_name) };
            sess.promises.push(kaleido_core::promise::Promise::new(&cid, "char", text, turn));
            dirty = true;
        }
    }
    // growth → strengthen
    tracing::info!(%session_id, turn, growth_raw = %v.get("growth").map(|x| x.to_string()).unwrap_or_default().chars().take(500).collect::<String>(), "st event_extract bg: growth raw");
    if let Some(arr) = v.get("growth").and_then(|x| x.as_array()) {
        for g in arr {
            let (ch_name, ev, st) = (
                g.get("character").and_then(|x| x.as_str()).unwrap_or("").trim(),
                g.get("event").or_else(|| g.get("description")).or_else(|| g.get("trigger")).or_else(|| g.get("eventText")).and_then(|x| x.as_str()).unwrap_or("").trim(),
                g.get("strength").and_then(|x| x.as_f64()).unwrap_or(0.6) as f32,
            );
            if ch_name.is_empty() || ev.is_empty() { continue; }
            let cid = resolve_cid(pack_chars, |k| sess.growth.rings.iter().any(|r| r.character == k), ch_name);
            sess.growth.strengthen(&cid, ev, st, turn);
            dirty = true;
        }
    }
    // bond → relationships + milestone plant (salience-gated)
    tracing::info!(%session_id, turn, bond_raw = %v.get("bond").map(|x| x.to_string()).unwrap_or_default().chars().take(500).collect::<String>(), journal_raw = %v.get("journal").map(|x| x.to_string()).unwrap_or_default().chars().take(500).collect::<String>(), "st event_extract bg: bond/journal raw");
    if let Some(arr) = v.get("bond").and_then(|x| x.as_array()) {
        let msg_n = sess.messages.len() as i64;
        let kick = sess.journal.salience_gate.allow(&sid, msg_n);
        for b in arr {
            let chg = b.get("bondDelta").or_else(|| b.get("change")).or_else(|| b.get("delta")).and_then(|x| x.as_i64()).unwrap_or(0);
            let (ch_name, bd, td) = (
                b.get("character").and_then(|x| x.as_str()).unwrap_or("").trim(),
                chg.clamp(-10, 10) as i32,
                b.get("trustDelta").or_else(|| b.get("trustChange")).or_else(|| b.get("trust")).and_then(|x| x.as_i64()).unwrap_or_else(|| if chg != 0 { chg * 6 / 10 } else { 0 }).clamp(-10, 10) as i32,
            );
            if ch_name.is_empty() || (bd == 0 && td == 0) { continue; }
            let cid = resolve_cid(pack_chars, |k| sess.relationships.contains_key(k), ch_name);
            let entry = sess.relationships.entry(cid.clone()).or_default();
            let (bc, tc) = entry.apply_delta(bd, td);
            if let Some(tier) = bc {
                if let Some(m) = kaleido_core::relationship_tiers::check_milestone(&cid, "bond", tier, turn, &sess.milestones) {
                    let lb = m.label.clone();
                    sess.milestones.push(m);
                    if kick { sess.journal.plant_milestone(&sid, &cid, format!("Bond deepened to {} ({})", lb, name_of(&cid)), "bond", tier > 0, turn); }
                }
            }
            if let Some(tier) = tc {
                if let Some(m) = kaleido_core::relationship_tiers::check_milestone(&cid, "trust", tier, turn, &sess.milestones) {
                    let lb = m.label.clone();
                    sess.milestones.push(m);
                    if kick { sess.journal.plant_milestone(&sid, &cid, format!("Trust rose to {} ({})", lb, name_of(&cid)), "trust", tier > 0, turn); }
                }
            }
            dirty = true;
        }
    }
    // needs → apply_scene_impact (single-need map)
    if let Some(arr) = v.get("needs").and_then(|x| x.as_array()) {
        for n in arr {
            let (ch_name, need, delta) = (
                n.get("character").and_then(|x| x.as_str()).unwrap_or("").trim(),
                n.get("need").or_else(|| n.get("needKey")).or_else(|| n.get("key")).and_then(|x| x.as_str()).unwrap_or("").trim(),
                n.get("delta").or_else(|| n.get("change")).or_else(|| n.get("value")).and_then(|x| x.as_i64()).unwrap_or(0).clamp(-30, 30) as i32,
            );
            if ch_name.is_empty() || need.is_empty() || delta == 0 { continue; }
            if !kaleido_core::needs::NEED_KEYS.contains(&need) { continue; }
            let cid = resolve_cid(pack_chars, |k| sess.needs.contains_key(k), ch_name);
            let entry = sess.needs.entry(cid).or_insert_with(kaleido_core::needs::Needs::default);
            let mut d = std::collections::HashMap::new();
            d.insert(need.to_string(), delta);
            entry.apply_scene_impact(&d, 1);
            dirty = true;
        }
    }
    // journal → maybe_write_auto (character 缺省 → 在场首位 → focus → pack 首位)
    let fallback_ch = present.first().and_then(|cid| pack_chars.iter().find(|(id, _)| id == cid).map(|(_, n)| n.as_str()))
        .or_else(|| pack_chars.iter().find(|(id, _)| id == focus).map(|(_, n)| n.as_str()))
        .or_else(|| pack_chars.first().map(|(_, n)| n.as_str()))
        .unwrap_or("");
    if let Some(arr) = v.get("journal").and_then(|x| x.as_array()) {
        for j in arr {
            let raw_ch = j.get("character").and_then(|x| x.as_str()).unwrap_or("").trim();
            let ch_name = if raw_ch.is_empty() { fallback_ch } else { raw_ch };
            let (content, kind) = (
                j.get("content").and_then(|x| x.as_str()).unwrap_or("").trim(),
                j.get("kind").and_then(|x| x.as_str()).unwrap_or("moment").trim(),
            );
            if ch_name.is_empty() || content.is_empty() { continue; }
            let cid = resolve_cid(pack_chars, |k| sess.journal.cards.iter().any(|c| c.character_id == k), ch_name);
            sess.journal.maybe_write_auto(&sid, &cid, content.to_string(), kind, None, turn);
            dirty = true;
        }
    }
    if !dirty { return Ok(()); }
    // 原子 read-modify-write（update_session 持锁）：避免与 roster/记忆提取的
    // 后保存互相覆盖。逐字段合并而非整包替换——只写本提取产出的增量。
    let want_pockets = sess.pockets.clone();
    let want_journal = sess.journal.cards.clone();
    let want_promises = sess.promises.promises.clone();
    let want_growth = sess.growth.rings.clone();
    let want_rels = sess.relationships.clone();
    let want_miles = sess.milestones.clone();
    let want_needs = sess.needs.clone();
    store.update_session(session_id, |live| {
        for (cid, p) in want_pockets { live.pockets.insert(cid, p); }
        for c in want_journal {
            if !live.journal.cards.iter().any(|x| x.id == c.id) { live.journal.cards.push(c); }
        }
        for pr in want_promises {
            if !live.promises.promises.iter().any(|x| x.id == pr.id) { live.promises.push(pr); }
        }
        for r in want_growth {
            if !live.growth.rings.iter().any(|x| x.id == r.id) { live.growth.strengthen(&r.character, &r.trigger_event, r.strength, turn); }
        }
        for (cid, b) in want_rels { live.relationships.insert(cid, b); }
        for m in want_miles {
            if !live.milestones.iter().any(|x| x.character == m.character && x.label == m.label) { live.milestones.push(m); }
        }
        for (cid, n) in want_needs { live.needs.insert(cid, n); }
        Ok(())
    }).map_err(|e| e.to_string())?;
    tracing::info!(%session_id, turn, "st event_extract bg: wrote events");
    Ok(())
}

/// 小模型低 token 提取调用（max_tokens 512, temperature 0.1）。
async fn call_llm_extract_with(appst: &kaleido_core::AppStateStore, base: Option<&str>, key: Option<&str>, model: &str, system: &str, user: &str) -> Result<String, String> {
    let llm = appst.resolve_llm(base, key, model);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("LLM not configured".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/chat/completions", llm.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": llm.model,
        "stream": false,
        "temperature": 0.1,
        "max_tokens": 512,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    let resp = client.post(&url).bearer_auth(&llm.api_key)
        .header("content-type", "application/json").json(&body)
        .send().await.map_err(|e| format!("extract request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("extract status {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("extract read failed: {e}"))?;
    v.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("message")).and_then(|m| m.get("content")).and_then(|c| c.as_str())
        .map(|s| s.to_string()).ok_or_else(|| "extract empty".to_string())
}

async fn call_llm_nonstream(state: &AppState, system: &str, user: &str) -> Result<String, String> {
    let llm = state.app_state.resolve_llm(
        state.llm_base.as_deref(),
        state.llm_key.as_deref(),
        &state.llm_model,
    );
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("LLM not configured: base_url or api_key is empty".into());
    }
    let model = if llm.model.is_empty() {
        state.llm_model.clone()
    } else {
        llm.model
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/chat/completions", llm.base_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "stream": false,
        "temperature": 0.3,
        // [fix 2026-08-15] 8192→16384：模型思考+压缩内容挤占预算曾致截断（5980/6119 实踩）。
        "max_tokens": 16384,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    let resp = client
        .post(&url)
        .bearer_auth(&llm.api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("LLM request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("LLM read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("LLM HTTP {status}: {}", text.chars().take(300).collect::<String>()));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("LLM JSON parse failed: {e}"))?;
    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "LLM response missing choices[0].message.content".into())
}

// ─── SoulLink 吸收：角色档案维护端点 ──────────────────────────────────────────
// 数据存 pack.characters[].archive（CharacterArchive：标量字段 + 5 分节）。
// analyze = LLM 增量分析（diff 应用）；refine = LLM 精编（整体替换）；purge = 删楼溯源清理。

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveAnalyzeReq {
    #[serde(default)]
    character_id: Option<String>,
    #[serde(default)]
    character_name: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    recent_messages: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveRefineReq {
    #[serde(default)]
    character_id: Option<String>,
    #[serde(default)]
    character_name: Option<String>,
}

/// 剥 JSON 围栏（```json ... ``` / 前后缀文字 / 尾注思考文本），提取第一个完整 JSON 对象。
fn strip_json_fence(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    // 直接尝试整体解析
    if serde_json::from_str::<serde_json::Value>(s).is_ok() {
        return s.to_string();
    }
    // 括号深度扫描提取第一个完整 JSON 对象(吸收 extract_json_value 健壮版思想,
    // 避免 LLM 在 JSON 后追加思考文本时 rfind('}') 截到尾注里的花括号)
    let bytes = s.as_bytes();
    if let Some(a) = s.find('{') {
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        for i in a..bytes.len() {
            let c = bytes[i] as char;
            if in_str {
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return s[a..=i].to_string();
                    }
                }
                ']' => depth -= 1,
                _ => {}
            }
        }
    }
    s.to_string()
}

fn find_char_idx(pack: &StoryPack, id: Option<&str>, name: Option<&str>) -> Option<usize> {
    pack.characters.iter().position(|c| {
        (id.is_some() && c.id == id.unwrap_or_default())
            || (name.is_some() && c.name == name.unwrap_or_default())
    })
}

/// 会话消息 → 近期对话文本（角色: 内容 换行）。
fn session_messages_to_text(sess: &TavernSession, max_msgs: usize) -> String {
    let start = sess.messages.len().saturating_sub(max_msgs);
    let mut parts = Vec::new();
    for m in sess.messages.iter().skip(start) {
        let name = if m.role.is_empty() { "?" } else { m.role.as_str() };
        parts.push(format!("{name}: {}", m.content));
    }
    parts.join("\n")
}

/// POST /api/v1/story-tavern/packs/{id}/archive/analyze
/// body: { characterId? | characterName?, sessionId? | recentMessages? }
/// LLM 增量分析 → apply_diff → 落盘 → 返回变更列表 + 新档案。
async fn archive_analyze(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ArchiveAnalyzeReq>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut pack = match state.packs.get(&id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let Some(idx) = find_char_idx(&pack, body.character_id.as_deref(), body.character_name.as_deref()) else {
        return not_found("ST_CHAR_NOT_IN_PACK", "character not found in pack");
    };

    // 近期对话：优先 session_id（从会话取），否则用 recentMessages 原文
    let recent = if let Some(sid) = body.session_id.as_deref() {
        match state.sessions_tavern.get_for_owner(sid, &session.user_id) {
            Ok(s) => session_messages_to_text(&s, 30),
            Err(_) => body.recent_messages.clone().unwrap_or_default(),
        }
    } else {
        body.recent_messages.clone().unwrap_or_default()
    };
    if recent.trim().is_empty() {
        return bad_request("ST_CONTEXT_REQUIRED", "recentMessages or sessionId required");
    }

    let ch = &pack.characters[idx];
    let archive = ch.archive.clone().unwrap_or_default();
    let registered: Vec<String> = pack.characters.iter().map(|c| c.name.clone()).collect();
    let user = serde_json::json!({
        "character": ch.name,
        "turn_index": 0,
        "current_profile": archive.serialize_for_prompt(),
        "registered_characters": registered,
        "recent_messages": recent,
    })
    .to_string();

    let raw = match call_llm_nonstream(&state, crate::archive_prompts::ARCHIVESYSTEM, &user).await {
        Ok(s) => s,
        Err(e) => return bad_gateway("ST_UPSTREAM_LLM", e),
    };
    let cleaned = strip_json_fence(&raw);
    let diff: kaleido_core::character_archive::ArchiveDiff = match serde_json::from_str(&cleaned) {
        Ok(d) => d,
        Err(e) => {
            return err_with_code(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ST_DIFF_PARSE",
                format!("diff parse: {e}"),
                serde_json::json!({ "raw": raw.chars().take(600).collect::<String>() }),
            );
        }
    };

    let mut archive = pack.characters[idx].archive.clone().unwrap_or_default();
    let source = body
        .session_id
        .clone()
        .unwrap_or_else(|| format!("manual-{}", ch.id));
    let changes = kaleido_core::character_archive::apply_diff(&mut archive, &diff, Some(&source));
    pack.characters[idx].archive = Some(archive);
    if let Err(e) = state.packs.save(pack) {
        return map_core_err(e);
    }
    Json(json!({
        "ok": true,
        "changes": changes,
        "profile": state.packs.get(&id).map(|p| p.characters[idx].archive.clone().unwrap_or_default().serialize_for_prompt()).unwrap_or(Value::Null),
    }))
    .into_response()
}

/// POST /api/v1/story-tavern/packs/{id}/archive/refine
/// body: { characterId? | characterName? }
/// LLM 精编（规范格式 + 提炼浓缩，不新增信息）→ apply_refine → 落盘。
async fn archive_refine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ArchiveRefineReq>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut pack = match state.packs.get(&id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let Some(idx) = find_char_idx(&pack, body.character_id.as_deref(), body.character_name.as_deref()) else {
        return not_found("ST_CHAR_NOT_IN_PACK", "character not found in pack");
    };
    let ch = &pack.characters[idx];
    let archive = ch.archive.clone().unwrap_or_default();
    let user = serde_json::json!({
        "character": ch.name,
        "current_profile": archive.serialize_for_prompt(),
    })
    .to_string();

    let raw = match call_llm_nonstream(&state, crate::archive_prompts::ARCHIVEREFINE, &user).await {
        Ok(s) => s,
        Err(e) => return bad_gateway("ST_UPSTREAM_LLM", e),
    };
    let cleaned = strip_json_fence(&raw);
    let refined: Value = match serde_json::from_str(&cleaned) {
        Ok(v) => v,
        Err(e) => {
            return err_with_code(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ST_REFINE_PARSE",
                format!("refine parse: {e}"),
                serde_json::json!({ "raw": raw.chars().take(600).collect::<String>() }),
            );
        }
    };

    let mut archive = pack.characters[idx].archive.clone().unwrap_or_default();
    let changes = kaleido_core::character_archive::apply_refine(&mut archive, &refined);
    pack.characters[idx].archive = Some(archive);
    if let Err(e) = state.packs.save(pack) {
        return map_core_err(e);
    }
    Json(json!({
        "ok": true,
        "changes": changes,
        "profile": state.packs.get(&id).map(|p| p.characters[idx].archive.clone().unwrap_or_default().serialize_for_prompt()).unwrap_or(Value::Null),
    }))
    .into_response()
}

/// DELETE /api/v1/story-tavern/packs/{id}/archive/purge/{source}
/// 删除来源（消息楼层/会话 id）关联的档案条目（删楼联动清理）。
async fn archive_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, source)): Path<(String, String)>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let mut pack = match state.packs.get(&id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let mut removed = 0usize;
    for ch in pack.characters.iter_mut() {
        if let Some(archive) = ch.archive.as_mut() {
            removed += kaleido_core::character_archive::purge_by_source(archive, &source);
        }
    }
    if removed > 0 {
        if let Err(e) = state.packs.save(pack) {
            return map_core_err(e);
        }
    }
    Json(json!({ "ok": true, "removed": removed })).into_response()
}

/// POST /api/v1/story-tavern/packs/{id}/cast-extract
/// 存量 pack 角色补抽：手动触发 LLM 角色抽取（异步 spawn，立即返回 started）。
/// 阈值相对判断 / body_path 断链容错 / 会话另存 pack 检测全部在 spawn_auto_cast_extraction 内。
async fn trigger_cast_extract(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let pack = match state.packs.get(&id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let real: Vec<String> = pack
        .characters
        .iter()
        .filter(|c| c.role != "narrator" && c.role != "player")
        .map(|c| c.name.clone())
        .collect();
    crate::crawler::spawn_auto_cast_extraction(state.clone(), id.clone());
    Json(json!({
        "ok": true,
        "packId": id,
        "status": "started",
        "existingCastCount": real.len(),
    }))
    .into_response()
}

/// Build the conversation text for memory analysis: serialize recent messages
/// plus memory layer snapshots.
fn build_memory_analysis_context(sess: &TavernSession) -> String {
    let mut parts = Vec::new();
    // Recent messages (last 30 messages max to stay within context)
    let start = sess.messages.len().saturating_sub(30);
    for msg in &sess.messages[start..] {
        let label = match msg.role.as_str() {
            "user" => "玩家",
            "assistant" => "NPC",
            _ => continue,
        };
        let text = msg.content.trim();
        if text.is_empty() {
            continue;
        }
        parts.push(format!("{}：{}", label, text));
    }
    // Memory L2 events
    if !sess.memory_l2.events.is_empty() {
        parts.push("\n=== 已有事件记录 (L2) ===".into());
        for ev in &sess.memory_l2.events {
            parts.push(format!("[回合{}] ({}) {}", ev.turn, ev.kind, ev.summary));
        }
    }
    // Memory L3 facts
    if !sess.memory_l3.facts.is_empty() {
        parts.push("\n=== 已知事实 (L3) ===".into());
        for fact in &sess.memory_l3.facts {
            parts.push(format!("- {}", fact));
        }
    }
    // Memory L4 secrets/promises
    if !sess.memory_l4.secrets_known.is_empty() {
        parts.push("\n=== 秘密 ===".into());
        for s in &sess.memory_l4.secrets_known {
            parts.push(format!("- {}", s));
        }
    }
    if !sess.memory_l4.promises.is_empty() {
        parts.push("\n=== 承诺 ===".into());
        for p in &sess.memory_l4.promises {
            parts.push(format!("- {}", p));
        }
    }
    parts.join("\n")
}

/// POST /api/v1/story-tavern/sessions/{id}/analyze-memory
/// Analyze character memory for a specific character (or focus character).
/// Returns structured {character, memories:[{content,type,certainty}], proposed_updates}.
async fn analyze_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<AnalyzeMemoryRequest>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let sess = match state.sessions_tavern.get_for_owner(&session_id, &_session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    // Resolve target character
    let pack = match state.packs.get(&sess.pack_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let target_char_id = body.character_id.clone()
        .or_else(|| sess.focus_character_id.clone())
        .or_else(|| sess.entry.vessel_character_id.clone());
    let target_char_name = target_char_id.as_ref()
        .and_then(|cid| pack.characters.iter().find(|c| c.id == *cid))
        .map(|c| c.name.as_str())
        .unwrap_or("(未知角色)");
    let target_char_card = target_char_id.as_ref()
        .and_then(|cid| pack.characters.iter().find(|c| c.id == *cid))
        .map(|c| c.personality.as_str())
        .unwrap_or("(未提供)");

    let context = build_memory_analysis_context(&sess);
    let system_prompt = "你是一个专门负责角色记忆管理的AI助手。你需要基于对话记录和已有记忆，分析角色与玩家之间的关系、关键事件、秘密和承诺。\n\n请以纯JSON格式输出，不要包含markdown标记。JSON结构如下：\n{\n  \"character\": \"角色名\",\n  \"memories\": [\n    {\n      \"content\": \"记忆内容描述\",\n      \"type\": \"secret|promise|relationship|event\",\n      \"certainty\": \"high|medium|low\"\n    }\n  ],\n  \"proposed_updates\": {\n    \"secrets\": [\"新增秘密...\"],\n    \"promises\": [\"新增承诺...\"],\n    \"facts\": [\"新增事实...\"],\n    \"events\": [\"新增事件摘要...\"]\n  }\n}";
    let user_prompt = format!(
        "请分析以下角色的记忆。\n\n### 目标角色\n- 角色名：{}\n- 角色卡描述：{}\n\n### 对话与记忆上下文\n{}\n\n请基于上述内容，分析该角色与玩家之间的关系演变、关键事件、秘密和承诺，输出结构化JSON。",
        target_char_name, target_char_card, context
    );

    let raw = match call_llm_nonstream(&state, &system_prompt, &user_prompt).await {
        Ok(r) => r,
        Err(e) => return internal("ST_LLM_CALL_FAILED", e),
    };

    // Try to parse as JSON; if not, wrap in a simple structure
    let result = if let Some(v) = crate::llm_stream::extract_json_value(&raw) {
        v
    } else {
        json!({
            "character": target_char_name,
            "memories": [{"content": raw, "type": "event", "certainty": "medium"}],
            "proposed_updates": {}
        })
    };

    // If apply=true, push proposed_updates into MemoryPatch pipeline
    if body.apply {
        if let Some(updates) = result.get("proposed_updates") {
            let mut sess = sess;
            if let Some(secrets) = updates.get("secrets").and_then(|v| v.as_array()) {
                for s in secrets {
                    if let Some(text) = s.as_str() {
                        if !text.is_empty() && !sess.memory_l4.secrets_known.iter().any(|x| x == text) {
                            sess.memory_l4.secrets_known.push(text.to_string());
                        }
                    }
                }
            }
            if let Some(promises) = updates.get("promises").and_then(|v| v.as_array()) {
                for p in promises {
                    if let Some(text) = p.as_str() {
                        if !text.is_empty() && !sess.memory_l4.promises.iter().any(|x| x == text) {
                            sess.memory_l4.promises.push(text.to_string());
                        }
                    }
                }
            }
            if let Some(facts) = updates.get("facts").and_then(|v| v.as_array()) {
                for f in facts {
                    if let Some(text) = f.as_str() {
                        if !text.is_empty() && !sess.memory_l3.facts.iter().any(|x| x == text) {
                            // [fix §10 2026-08-16] 关键物品/承诺永久层：含所有权/收藏/承诺
                            // 语义的事实（如「素描原稿被向明初收藏」）入 pinned，注入不裁剪——
                            // 窝边草素描链 t38-42 曾被 take(6) 挤出导致「画好了又自己画」失忆。
                            if is_key_fact(text) {
                                if !sess.memory_l3.pinned.iter().any(|x| x == text) {
                                    sess.memory_l3.pinned.push(text.to_string());
                                }
                            } else {
                                sess.memory_l3.facts.push(text.to_string());
                            }
                        }
                    }
                }
            }
            if let Some(events) = updates.get("events").and_then(|v| v.as_array()) {
                for e in events {
                    if let Some(text) = e.as_str() {
                        if !text.is_empty() {
                            let ev = kaleido_core::MemoryL2Event {
                                id: format!("ev-{}", Uuid::new_v4()),
                                turn: sess.turn,
                                kind: "analyze".into(),
                                summary: text.to_string(),
                                actors: target_char_id.clone().into_iter().collect(),
                                node_id: sess.node_id.clone(),
                                embedding: vec![],
                            };
                            sess.memory_l2.events.push(ev);
                        }
                    }
                }
            }
            let _ = state.sessions_tavern.save(sess);
        }
    }

    Json(result).into_response()
}

/// POST /api/v1/story-tavern/sessions/{id}/optimize-memory
/// Scan L2 events + L3 facts → LLM deduplication/merge → MemoryPatch.
async fn optimize_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<OptimizeMemoryRequest>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&session_id, &_session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    // Build combined memory text for optimization
    let mut memory_lines = Vec::new();
    for ev in &sess.memory_l2.events {
        memory_lines.push(format!("[事件] 回合{} ({}): {}", ev.turn, ev.kind, ev.summary));
    }
    for fact in &sess.memory_l3.facts {
        memory_lines.push(format!("[事实] {}", fact));
    }
    for s in &sess.memory_l4.secrets_known {
        memory_lines.push(format!("[秘密] {}", s));
    }
    for p in &sess.memory_l4.promises {
        memory_lines.push(format!("[承诺] {}", p));
    }

    if memory_lines.is_empty() {
        return Json(json!({"ok": true, "message": "No memories to optimize", "before": 0, "after": 0})).into_response();
    }

    let before_count = sess.memory_l2.events.len() + sess.memory_l3.facts.len()
        + sess.memory_l4.secrets_known.len() + sess.memory_l4.promises.len();
    let memory_text = memory_lines.join("\n");

    let system_prompt = "你是一个专门负责人物记忆分析与优化的专家。你的任务是读取现有的事件记录和事实，进行去冗余、归并、消除逻辑矛盾。\n\n请返回优化后的JSON结果，格式如下：\n{\n  \"events\": [\"精简后的事件摘要...\"],\n  \"facts\": [\"精简后的事实...\"],\n  \"secrets\": [\"精简后的秘密...\"],\n  \"promises\": [\"精简后的承诺...\"],\n  \"removed_count\": 0,\n  \"notes\": \"优化说明...\"\n}\n\n规则：\n1. 合并重复或高度相似的条目\n2. 消除逻辑矛盾，以更合理的版本为准\n3. 保留所有关键信息，宁多勿漏\n4. events/facts/secrets/promises 分类不变";
    let user_prompt = format!(
        "请优化以下角色记忆记录，去冗余、归并、消除矛盾：\n\n{}",
        memory_text
    );

    let raw = match call_llm_nonstream(&state, &system_prompt, &user_prompt).await {
        Ok(r) => r,
        Err(e) => return internal("ST_LLM_CALL_FAILED", e),
    };

    let result = if let Some(v) = crate::llm_stream::extract_json_value(&raw) {
        v
    } else {
        json!({"error": "Failed to parse optimization result", "raw": raw})
    };

    // If apply=true, replace memory layers with optimized version
    if body.apply {
        if let Some(events_arr) = result.get("events").and_then(|v| v.as_array()) {
            let new_events: Vec<kaleido_core::MemoryL2Event> = events_arr
                .iter()
                .filter_map(|v| v.as_str())
                .enumerate()
                .map(|(i, text)| kaleido_core::MemoryL2Event {
                    id: format!("opt-{}-{}", Uuid::new_v4(), i),
                    turn: sess.turn,
                    kind: "optimized".into(),
                    summary: text.to_string(),
                    actors: vec![],
                    node_id: None,
                    embedding: vec![],
                })
                .collect();
            sess.memory_l2.events = new_events;
        }
        if let Some(facts_arr) = result.get("facts").and_then(|v| v.as_array()) {
            sess.memory_l3.facts = facts_arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
        if let Some(secrets_arr) = result.get("secrets").and_then(|v| v.as_array()) {
            sess.memory_l4.secrets_known = secrets_arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
        if let Some(promises_arr) = result.get("promises").and_then(|v| v.as_array()) {
            sess.memory_l4.promises = promises_arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
        let _ = state.sessions_tavern.save(sess.clone());
    }

    let after_count = result.get("events").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
        + result.get("facts").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
        + result.get("secrets").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
        + result.get("promises").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);

    Json(json!({
        "ok": true,
        "result": result,
        "before": before_count,
        "after": after_count,
    })).into_response()
}

// ─── U13 M2: Explicit Session Compaction ─────────────────────────────────────

/// Build a compact system prompt based on style.
fn compact_system_prompt(style: &str) -> String {
    match style {
        "bulletin" => {
            "你是一个要点式会话压缩器。将以下对话记录压缩为要点摘要。\n\n输出格式：\n## 关键要点\n- [要点1]\n- [要点2]\n...\n\n## 保留事实\n- [事实1]\n...\n\n## 已丢弃范围说明\n[简要说明丢弃了哪些内容]\n\n规则：\n1. 每个要点控制在一句话内\n2. 保留所有承诺、秘密、关系变化\n3. 保留当前场景状态".into()
        }
        "battle_report" => {
            "你是一个战场式会话压缩器（适配双Agent战场叙事）。将以下对话压缩为战场报告。\n\n输出格式：\n## 战场态势\n[当前局势概括]\n\n## 行动日志\n- [关键行动1]\n- [关键行动2]\n...\n\n## 保留事实\n- [事实1]\n...\n\n## 已丢弃范围说明\n[简要说明丢弃了哪些内容]\n\n规则：\n1. 保留所有角色状态变化和阵营关系\n2. 保留关键决策和转折点\n3. 保留悬念和伏笔".into()
        }
        _ => {
            // "conversation" — 对话流
            "你是一个叙事上下文压缩器。将以下对话记录压缩为结构化的前情摘要，供后续对话续接。\n\n输出格式：\n## 前情提要\n[按时间顺序概述关键事件]\n\n## 人物状态\n[每位出场人物的当前状态和关系变化]\n\n## 保留事实\n- [事实1]\n- [事实2]\n...\n\n## 已丢弃范围说明\n[简要说明丢弃了哪些内容]\n\n规则：\n1. 保留所有承诺、秘密、关系变化\n2. 保留当前场景状态（地点、时间、在场角色）\n3. [P1B 2026-08-16] 保留着装状态：每位角色当前穿着 + 已脱/脱下的衣物去向（谁拿着/扔在哪/是否穿回），脱衣/穿衣/换装事件不得省略\n4. 不虚构、不续写剧情\n5. 人名地名保持剧中写法\n6. [P13 2026-08-15] 环境意象/氛围描写去重：同一意象（如「楼板吱呀响」「鼾声」「雨声」「烛火/蜡油」）在对话中反复出现时，只在摘要中记录一次（说明其为持续氛围即可），禁止逐回合复述或作为新事件强调；事件推进以剧情转折/对白/行动为准，重复的静态描写不占摘要篇幅。".into()
        }
    }
}

/// POST /api/v1/story-tavern/sessions/{id}/compact
/// Manually trigger session context compaction.
/// L4 情感层提炼模板（2026-08-14 补写端）：从最近剧情事件提炼情感增量。
/// 输出 JSON 契约 {affinity:{角色ID:0-100}, secretsKnown:[], promises:[]}，
/// 由 st_memory_contract::parse_l4_patch 校验（不合规降级，不阻塞回合）。
const L4_REFINE_SYSTEM: &str = "你是叙事情感记忆提炼器。从最近剧情事件中提炼情感状态增量，只输出 JSON：\n\
{\"affinity\": {\"<角色ID>\": 0-100 整数}, \"secretsKnown\": [\"新秘密...\"], \"promises\": [\"新承诺...\"]}\n\
规则：1) 只输出 JSON，不要任何其他文字或解释 2) affinity 为 0-100 整数（角色ID 用 pack 的角色 id）\
3) 没有新内容输出空对象/空数组 4) secretsKnown/promises 只记明确达成或透露的，不猜测\
5) [P14 2026-08-15] 好感度必须克制分级：50-60=正常友善，61-70=明显好感，71-80=强烈好感/亲密倾向，\
81-90=深度信任/依恋，91-100=近乎无条件（仅当长期剧情确实发展至此才给）。\
单次事件最多 +10，禁止因一次亲密举动直接给 90+；分歧/抗拒事件应下降或持平。\
宁缺勿滥——不确定时给区间下限，不要给剧情定死结局。";

/// 合并 L4 提炼补丁（增量 merge，不清空旧值）：affinity 覆盖更新、secrets/promises 追加去重。
fn merge_l4_patch(l4: &mut kaleido_core::MemoryL4, patch: &kaleido_core::st_memory_contract::L4Patch) {
    if let Some(aff_map) = l4.affinity.as_object_mut() {
        for (k, v) in &patch.affinity {
            aff_map.insert(k.clone(), serde_json::json!(v));
        }
    } else {
        let mut m = serde_json::Map::new();
        for (k, v) in &patch.affinity {
            m.insert(k.clone(), serde_json::json!(v));
        }
        l4.affinity = serde_json::Value::Object(m);
    }
    for s in &patch.secrets_known {
        if !l4.secrets_known.iter().any(|x| x == s) {
            l4.secrets_known.push(s.clone());
        }
    }
    for p in &patch.promises {
        if !l4.promises.iter().any(|x| x == p) {
            l4.promises.push(p.clone());
        }
    }
}

async fn compact_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<CompactSessionRequest>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut sess = match state.sessions_tavern.get_for_owner(&session_id, &_session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    let before_turns = sess.messages.len();
    if before_turns < 4 {
        return Json(json!({
            "ok": true,
            "message": "Too few messages to compact",
            "before_turns": before_turns,
            "after_turns": before_turns,
            "summary": "",
        })).into_response();
    }

    // Use memory_weaver to determine if compaction is needed and find cut point
    let config = state.weaver_config.clone();
    let total_tokens = kaleido_core::memory_weaver::estimate_total_tokens(&sess.messages);
    let force_compact = total_tokens > 0; // manual trigger always compacts

    let stats = kaleido_core::memory_weaver::analyze_messages(&sess.messages);
    let cut_index = if force_compact {
        // For manual trigger, find a reasonable cut point
        kaleido_core::memory_weaver::find_cut_point(&stats, config.keep_recent_tokens)
            .unwrap_or(sess.messages.len() / 2)
    } else {
        return Json(json!({
            "ok": true,
            "message": "Context within budget, no compaction needed",
            "before_turns": before_turns,
            "after_turns": before_turns,
            "summary": "",
        })).into_response();
    };

    if cut_index < 2 {
        return Json(json!({
            "ok": true,
            "message": "Not enough messages to compact",
            "before_turns": before_turns,
            "after_turns": before_turns,
            "summary": "",
        })).into_response();
    }

    // Build conversation text for LLM summarization
    let (conversation_text, omit_stats) = kaleido_core::memory_weaver::serialize_for_summary_with_stats(
        &sess.messages[..cut_index],
        "玩家",
        "NPC",
    );
    tracing::info!(
        session = %session_id,
        kept = omit_stats.kept,
        skipped_program = omit_stats.skipped_program,
        skipped_reasoning = omit_stats.skipped_reasoning,
        skipped_empty = omit_stats.skipped_empty,
        skipped_other = omit_stats.skipped_other,
        "场记摘要序列化省略统计（吸收自 OpenHanako OmissionCounts）"
    );
    let previous_summary = if sess.memory_l1.scene_summary.is_empty() {
        None
    } else {
        Some(sess.memory_l1.scene_summary.as_str())
    };
    let system_prompt = compact_system_prompt(&body.style);
    let user_prompt = kaleido_core::memory_weaver::build_rp_summary_user_text(
        &conversation_text,
        &format!("回合: {} | 模式: {}", sess.turn, sess.play_mode.as_str()),
        previous_summary,
    );

    let summary = match call_llm_nonstream(&state, &system_prompt, &user_prompt).await {
        Ok(s) => s,
        Err(e) => {
            return internal("ST_LLM_CALL_FAILED", e)
        }
    };

    // P5: 场记摘要结构契约 + 修复循环（吸收自 OpenHanako rolling-summary-format：
    // 校验失败附原因重试 1 次，仍失败用原摘要降级——修复成功则落盘更完整）。
    // [P9 2026-08-15] 语义污染检测：结构校验之外，fix 元话语（让我分析审稿意见…）
    // 会被摘要 LLM 照抄进 L1——污染视同校验失败，进修复循环。
    let summary = {
        let polluted = kaleido_core::st_memory_contract::summary_is_polluted(&summary);
        let issues = kaleido_core::st_memory_contract::validate_rp_summary(&summary);
        if !polluted && issues.ok {
            summary
        } else {
            let mut reason_lines: Vec<String> = issues.issues.clone();
            if polluted {
                reason_lines.push("摘要含 fix 元话语污染（让我分析审稿意见/让我检查/审稿意见清单等自指段），须重写为干净场记摘要".to_string());
                tracing::warn!(session = %session_id, "场记摘要语义污染（fix 元话语），尝试修复");
            }
            tracing::warn!(
                session = %session_id,
                issues = ?issues.issues,
                "场记摘要结构校验失败，尝试格式修复"
            );
            let repair_prompt = kaleido_core::st_memory_contract::build_rp_summary_repair_prompt();
            let repair_input =
                kaleido_core::st_memory_contract::build_rp_summary_repair_input(&issues.issues, &summary);
            let mut repaired = summary.clone();
            if let Ok(r) = call_llm_nonstream(&state, &repair_prompt, &repair_input).await {
                let after = kaleido_core::st_memory_contract::validate_rp_summary(&r);
                if after.ok {
                    tracing::info!(session = %session_id, "场记摘要格式修复成功");
                    repaired = r;
                } else {
                    tracing::warn!(
                        session = %session_id,
                        issues = ?after.issues,
                        "场记摘要修复后仍不合规，使用原摘要降级"
                    );
                }
            }
            repaired
        }
    };

    // Extract retained facts from summary (parse structured output)
    let _retained_facts: Vec<String> = Vec::new(); // Facts are embedded in summary text

    // Apply compaction: keep only messages from cut_index onward, update L1 summary
    let retained_messages: Vec<TavernMessage> = sess.messages[cut_index..].to_vec();
    sess.messages = retained_messages;
    // Merge new summary with existing
    let merged_summary = if let Some(prev) = previous_summary {
        format!("{}\n\n---\n\n{}", prev, summary)
    } else {
        summary.clone()
    };
    sess.memory_l1.scene_summary = merged_summary;
    sess.memory_l1.updated_at_turn = sess.turn;

    // Record compaction epoch
    sess.epoch = sess.epoch.saturating_add(1);
    sess.epoch_last_turn = Some(sess.turn);
    sess.epoch_last_chars = Some(total_tokens as u32);

    let after_turns = sess.messages.len();
    let final_epoch = sess.epoch;
    let _ = state.sessions_tavern.save(sess);

    Json(json!({
        "ok": true,
        "before_turns": before_turns,
        "after_turns": after_turns,
        "summary": summary,
        "style": body.style,
        "epoch": final_epoch,
    })).into_response()
}

// ─── U13 M3: Branch Summary ──────────────────────────────────────────────────

/// GET /api/v1/story-tavern/sessions/{id}/branches/{bid}/summary
/// Independent branch summary. When no branch exists, returns session summary.
async fn branch_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session_id, branch_id)): Path<(String, String)>,
) -> Response {
    let _session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let sess = match state.sessions_tavern.get_for_owner(&session_id, &_session.user_id) {
        Ok(s) => s,
        Err(e) => return map_core_err(e),
    };

    // Check if branch_id matches current session or is a side branch
    let is_current_branch = branch_id == "main" || branch_id == sess.session_id;
    let is_side_branch = sess.side_branch_node_id.as_deref() == Some(&branch_id);

    if is_current_branch || !is_side_branch {
        // No separate branch: return session summary (L1)
        return Json(json!({
            "branchId": branch_id,
            "summary": sess.memory_l1.scene_summary,
            "updatedAtTurn": sess.memory_l1.updated_at_turn,
            "isCurrentBranch": true,
            "turn": sess.turn,
            "epoch": sess.epoch,
        })).into_response();
    }

    // For side branches, generate a branch-specific summary from session messages
    // Filter messages relevant to this branch (messages since entering the side branch)
    let branch_messages: Vec<&TavernMessage> = sess.messages.iter().collect();

    let conversation_text = kaleido_core::memory_weaver::serialize_for_summary(
        &branch_messages.iter().map(|m| (*m).clone()).collect::<Vec<_>>(),
        "玩家",
        "NPC",
    );

    if conversation_text.trim().is_empty() {
        return Json(json!({
            "branchId": branch_id,
            "summary": String::new(),
            "updatedAtTurn": sess.turn,
            "isCurrentBranch": false,
            "turn": sess.turn,
            "epoch": sess.epoch,
        })).into_response();
    }

    let system_prompt = "你是一个分支摘要生成器。请为以下对话分支生成一个结构化摘要。\n\n输出格式：\n## 目标\n[此分支中玩家试图完成什么]\n\n## 进展\n- [已完成]\n- [进行中]\n\n## 关键决策\n- [决策及理由]\n\n## 下一步\n1. [接下来应该做什么]\n\n保持每节简洁，保留文件路径、函数名、错误消息等精确信息。";
    let user_prompt = format!(
        "请为以下对话分支生成摘要：\n\n分支ID: {}\n支线标题: {}\n\n{}",
        branch_id,
        sess.side_branch_label.as_deref().unwrap_or("(未命名)"),
        conversation_text
    );

    let summary = match call_llm_nonstream(&state, system_prompt, &user_prompt).await {
        Ok(s) => s,
        Err(e) => {
            return internal("ST_LLM_CALL_FAILED", e)
        }
    };

    Json(json!({
        "branchId": branch_id,
        "summary": summary,
        "updatedAtTurn": sess.turn,
        "isCurrentBranch": false,
        "turn": sess.turn,
        "epoch": sess.epoch,
        "branchLabel": sess.side_branch_label,
    })).into_response()
}

/// 启动时孤儿回合清扫（2026-08-15 实踩：13:23:33 服务重启打断进行中回合，
/// 会话 active_run_id 永久残留——U11 resume 只在新回合触发时检查，重启后无新
/// 消息则孤儿永远挂着，前端显示「生成中」卡死）。
///
/// 语义：服务刚启动，hub 必然无任何流式 worker。凡 active_run_id 非空且对应
/// job 仍标 running 的会话，一律判定为「被重启打断的孤儿回合」：
/// 1. job 终态 cancelled（释放 running 并发槽，防死锁）
/// 2. 会话 active_run_id 清空（解除会话锁）
/// 3. 写入一条 assistant 系统提示消息（可见、可恢复），正文明示中断原因
///
/// 幂等：仅处理 active_run_id 非空且 job 状态 active 的会话；正常会话不动。
/// fail-open：任何 store 错误仅 warn，不阻断服务启动。
pub(crate) fn sweep_orphan_runs(state: &AppState) {
    let Ok(list) = state.sessions_tavern.list() else {
        tracing::warn!("st sweep_orphan_runs: sessions list failed");
        return;
    };
    let mut cleaned = 0usize;
    // list() 仅返回摘要（无 active_run_id），需用 get() 取完整 TavernSession。
    for item in list {
        let Some(sid) = item.get("sessionId").and_then(|v| v.as_str()).map(|s| s.to_string()) else {
            continue;
        };
        let Ok(mut sess) = state.sessions_tavern.get(&sid) else {
            continue;
        };
        let Some(prev) = sess.active_run_id.clone() else {
            continue;
        };
        // 孤儿判定：job 已终态（dispatch_recovered 启动恢复后，tavern-turn 无 slug 会
        // 被标 failed）或 job 不存在 → 会话锁死，标记中断。queued/running（有活 worker
        // 或已 rearm 待恢复）不算孤儿。
        let job_state = state
            .jobs
            .get(&prev)
            .map(|j| kaleido_core::normalize_job_status(&j.status))
            .unwrap_or_else(|| "missing".into());
        let is_orphan = !kaleido_core::is_active_job_status(&job_state);
        if !is_orphan {
            continue;
        }
        // 服务刚启动，hub 无活 worker——非 active 即孤儿。
        tracing::warn!(
            %prev,
            %sid,
            job_status = %job_state,
            "st sweep_orphan_runs: interrupted turn marked (service restart)"
        );
        // 1. job 终态 cancelled（cancel-wins 语义，不会复活）
        let _ = state.jobs.complete(
            &prev,
            "cancelled",
            None,
            Some("sweep_orphan_runs: interrupted by service restart".into()),
        );
        // 2. 解除会话锁
        sess.active_run_id = None;
        // 3. 系统提示消息（assistant 角色，前端自然展示）
        sess.messages.push(TavernMessage {
            id: format!("msg-{}", uuid::Uuid::new_v4()),
            role: "assistant".into(),
            content: "（上一回合因服务重启而中断，未完成生成。你可以重新发送刚才的话，或继续新的行动。）".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            options: Vec::new(),
            engine_tag: None,
            program: None,
            reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
        });
        if let Err(e) = state.sessions_tavern.save(sess.clone()) {
            tracing::warn!(%prev, error=%e, "st sweep_orphan_runs: save failed");
            continue;
        }
        cleaned += 1;
    }
    if cleaned > 0 {
        tracing::info!(cleaned, "st sweep_orphan_runs: interrupted turns marked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── parse_option_list JSON 污染防护单测（P11） ─────────────────────────
    #[test]
    fn parse_option_list_rejects_json_object_leak() {
        // P11 实踩：检定 JSON 对象裸写在正文 → 键值对被捞成 52 个「选项」
        let leak = "{\"action\":\"告诉母亲第一次还为她留着\",\"intent\":\"彻底坦白\",\"challenge\":\"母亲拒绝\",\"cost\":\"崩溃\",\"difficulty\":\"normal\",\"templateId\":\"亲密试探检定\",\"bonuses\":[{\"reason\":\"醉酒坦诚\",\"value\":1}],\"outcomes\":{\"criticalSuccess\":{\"result\":\"成功\"},\"success\":{\"result\":\"成功\",\"stateChanges\":[{\"actorId\":\"c-distil-0\",\"fieldId\":\"emotion\",\"change\":1}]}}}";
        let parsed = parse_option_list(leak);
        assert!(parsed.is_empty(), "JSON 泄漏不应产生选项: {parsed:?}");
    }

    #[test]
    fn parse_option_list_keeps_normal_choices() {
        // 正常选项（短中文串）不受影响
        let normal = "[\"靠在她膝盖上，什么都不说\", \"握住她的手\", \"把她拉进怀里\"]";
        let parsed = parse_option_list(normal);
        assert_eq!(parsed.len(), 3, "正常选项应保留: {parsed:?}");
    }

    #[test]
    fn parse_option_list_keeps_quoted_choices_without_json_hints() {
        // 无 JSON 键名的引号选项（旧分支行为）保留
        let quoted = "\"把头靠在她膝盖上\" \"握住她搭在头顶的那只手\" \"站起来坐在她身旁\"";
        let parsed = parse_option_list(quoted);
        assert_eq!(parsed.len(), 3, "引号选项应保留: {parsed:?}");
    }

    // ─── extract_advance_marker 节点推进 marker 兼容性单测（ST-15 fix） ─────
    #[test]
    fn extract_advance_marker_inner_colon_format() {
        // prompt 教的格式：【节点推进:n2】（冒号+节点ID在括号内）
        let text = "正文内容……她垂下眼。\n【节点推进:n2】";
        let (pos, id) = extract_advance_marker(text).expect("应提取到 n2");
        assert_eq!(id, "n2");
        assert!(text[..pos].contains("正文内容"), "marker 起始位置应指向正文之后");
    }

    #[test]
    fn extract_advance_marker_outer_format() {
        // 旧格式：【节点推进】 n2（节点ID在括号外）
        let text = "正文……\n【节点推进】 n3";
        let (_pos, id) = extract_advance_marker(text).expect("应提取到 n3");
        assert_eq!(id, "n3");
    }

    #[test]
    fn extract_advance_marker_absent_returns_none() {
        assert!(extract_advance_marker("纯正文，没有任何标记").is_none());
        assert!(extract_advance_marker("").is_none());
    }

    #[test]
    fn extract_advance_marker_takes_last_marker() {
        // 多个 marker 时取最后一个（rfind 语义）
        let text = "【节点推进:n1】 中间正文 【节点推进:n2】";
        let (_pos, id) = extract_advance_marker(text).expect("应取最后一个");
        assert_eq!(id, "n2");
    }

    #[test]
    fn guard_person_unknown_high() {
        // 2026-08-16 统一来源根治: 切词启发式退役——无 roster 时「王麻子说道」不再靠切词瞎判。
        // 真外对象只信统一来源（roster/场景标签/LLM 兜底），宁漏勿瞎。
        let known: std::collections::HashSet<String> =
            ["林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "林晚说道：师兄且慢。王麻子说道：这玉佩是我的！",
            "",
            &known,
            &[],
            &[],
            &[],
            None,
        );
        assert!(
            !vs.iter().any(|v| v.dim == "人物"),
            "无 roster 时切词退役，不靠启发式瞎判外对象: {vs:?}"
        );
        // 同一文本若 LLM 自报清单（统一来源）含王麻子 → 精确比对检出
        let vs2 = guard_narrative(
            "林晚说道：师兄且慢。王麻子说道：这玉佩是我的！",
            "",
            &known,
            &[],
            &[],
            &[],
            Some(&["王麻子".to_string()]),
        );
        let hit = vs2.iter().find(|v| v.dim == "人物" && v.msg.contains("王麻子"));
        assert!(hit.is_some(), "roster 统一来源应检出王麻子: {vs2:?}");
        assert_eq!(hit.unwrap().severity, GuardSeverity::High);
    }

    #[test]
    fn guard_no_fragment_fp_on_common_phrases() {
        // 2026-08-16 瞎报根治回归: 「道」作名词尾（胡说八道/谁知道/城市主干道/坡道）
        // 不再是说话标记；4字碎片（东电话里/段石阶坡）与称呼（老板娘）不误报。
        let known: std::collections::HashSet<String> =
            ["林晚".into(), "庄眉".into()].into_iter().collect();
        let text = "林晚说：那些胡说八道的东西你也信？庄眉在电话里说：谁知道呢，\
            反正城市主干道上的霓虹还亮着。老板娘说那边有条新街刚招商，租金便宜。\
            那段石阶坡道又陡又窄，走起来费劲。";
        let vs = guard_narrative(text, "", &known, &[], &[], &[], None);
        assert!(
            !vs.iter().any(|v| v.dim == "人物"),
            "常见短语碎片不应误报外对象: {vs:?}"
        );
    }

    #[test]
    fn guard_still_catches_real_unknown_after_fp_fix() {
        // 统一来源根治: 真外对象由 LLM 自报清单（roster）检出，不依赖切词。
        let known: std::collections::HashSet<String> =
            ["林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "林晚说：师兄且慢。李铁柱说道：这玉佩是我的！",
            "",
            &known,
            &[],
            &[],
            &[],
            Some(&["李铁柱".to_string()]),
        );
        let hit = vs.iter().find(|v| v.dim == "人物");
        assert!(hit.is_some(), "roster 统一来源应检出真外对象: {vs:?}");
        assert!(hit.unwrap().msg.contains("李铁柱"), "{vs:?}");
    }

    #[test]
    fn guard_roster_narrative_unknown_high() {
        // ST-30 根治: 叙述/动作形态点名（无说话标记，切词启发式漏检）——
        // LLM 自报角色清单含外角色 → 精确比对检出（替代切词，根治漏报）。
        let known: std::collections::HashSet<String> =
            ["沈棠".into(), "林晚".into()].into_iter().collect();
        let roster = vec!["沈棠".to_string(), "李铁柱".to_string()];
        let vs = guard_narrative(
            "李铁柱的声音比雨声重，压过檐下滴水。沈棠没有立刻答话。",
            "",
            &known,
            &[],
            &[],
            &[],
            Some(&roster),
        );
        let hit = vs.iter().find(|v| v.dim == "人物" && v.msg.contains("李铁柱"));
        assert!(hit.is_some(), "清单外角色应检出: {vs:?}");
        assert_eq!(hit.unwrap().severity, GuardSeverity::High);
        // 全 known 清单 → 人物维零违规（无切词误报）
        let roster2 = vec!["沈棠".to_string(), "林晚".to_string()];
        let vs2 = guard_narrative(
            "沈棠的手指在门框上一僵。林晚在柜台后面搁下瓷杯。",
            "",
            &known,
            &[],
            &[],
            &[],
            Some(&roster2),
        );
        assert!(
            !vs2.iter().any(|v| v.dim == "人物"),
            "known 清单不应报人物违规: {vs2:?}"
        );
    }

    #[test]
    fn guard_roster_skips_heuristic_no_fragment_fp() {
        // ST-30 根治: 清单存在时不再跑切词启发式 → 「明日柜上/却带着点」类
        // 切碎短语误报从根上消失（不再可能被切出来）。
        let known: std::collections::HashSet<String> =
            ["沈棠".into(), "林晚".into()].into_iter().collect();
        let roster = vec!["沈棠".to_string()];
        let vs = guard_narrative(
            "明日柜上，却带着点雨气的灯笼被风推着转了小半圈，沈棠站在檐下。",
            "",
            &known,
            &[],
            &[],
            &[],
            Some(&roster),
        );
        assert!(
            !vs.iter().any(|v| v.dim == "人物"),
            "切词启发式误报应在清单模式下消失: {vs:?}"
        );
    }

    #[test]
    fn guard_roster_missing_falls_back_heuristic() {
        // ST-30 兜底: 清单缺失（LLM 未遵守格式）→ 降级现有启发式（场景标签检测仍工作）。
        let known: std::collections::HashSet<String> =
            ["沈棠".into(), "林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "<场景：茶馆｜雨夜｜沈棠、孙二娘>茶香袅袅。",
            "",
            &known,
            &[],
            &[],
            &[],
            None,
        );
        let hit = vs.iter().find(|v| v.dim == "人物" && v.msg.contains("孙二娘"));
        assert!(hit.is_some(), "无清单时场景标签启发式应兜底检出: {vs:?}");
    }

    #[test]
    fn guard_beat_missing_high() {
        let known: std::collections::HashSet<String> =
            ["林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "林晚接过药瓶，沉默不语。",
            "",
            &known,
            &[],
            &["必须向师父坦白".into()],
            &[],
        None,
        );
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].severity, GuardSeverity::High);
        assert_eq!(vs[0].dim, "节拍");
    }

    #[test]
    fn guard_present_missing_medium() {
        let known: std::collections::HashSet<String> =
            ["林晚".into(), "陈默".into()].into_iter().collect();
        let vs = guard_narrative(
            "林晚站在山门口，望着远方。",
            "",
            &known,
            &["陈默".to_string()],
            &[],
            &[],
        None,
        );
        assert!(vs.iter().any(|v| v.severity == GuardSeverity::Medium && v.dim == "出场"));
    }

    #[test]
    fn guard_goal_missing_medium() {
        let known: std::collections::HashSet<String> =
            ["林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "林晚沿着山路走着。",
            "",
            &known,
            &[],
            &[],
            &["拜师学艺".into()],
        None,
        );
        assert!(vs.iter().any(|v| v.severity == GuardSeverity::Medium && v.dim == "大纲"));
    }

    #[test]
    fn guard_scene_tag_unknown_high() {
        // 2026-08-14: 场景标签里的原著外角色（王麻子案例——引语式检测抓不到）
        let known: std::collections::HashSet<String> =
            ["林晚".into(), "沈棠".into()].into_iter().collect();
        let vs = guard_narrative(
            "<场景：旧茶馆内·窗边｜雨夜｜沈棠、林晚、王麻子>\n\n门被推开，一个满脸横肉的男人闯了进来。",
            "",
            &known,
            &["林晚".into()],
            &[],
            &[],
        None,
        );
        let scene_hit = vs.iter().find(|v| v.msg.contains("王麻子"));
        assert!(scene_hit.is_some(), "场景标签外角色应被检出: {vs:?}");
        assert_eq!(scene_hit.unwrap().severity, GuardSeverity::High);
    }

    #[test]
    fn guard_scene_tag_adjective_no_false_positive() {
        // 2026-08-14 误报修复: 「温热的」「白的蓝」等形容词片段 + known 角色延伸 不应误报
        let known: std::collections::HashSet<String> =
            ["沈棠".into(), "林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "<场景：茶馆｜雨夜｜沈棠、温热的、白的蓝、林晚的伞>茶香袅袅，白瓷杯冒着热气。",
            "",
            &known,
            &[],
            &[],
            &[],
        None,
        );
        assert!(
            !vs.iter().any(|v| v.dim == "人物"),
            "形容词/延伸片段不应误报为角色: {vs:?}"
        );
        // 真外角色（孙二娘）→ 仍应检出
        let vs2 = guard_narrative(
            "<场景：茶馆｜雨夜｜沈棠、孙二娘>茶香袅袅。",
            "",
            &known,
            &[],
            &[],
            &[],
        None,
        );
        assert!(
            vs2.iter().any(|v| v.dim == "人物" && v.msg.contains("孙二娘")),
            "场景标签外角色仍应检出: {vs2:?}"
        );
    }

    #[test]
    fn guard_idiom_fragment_no_false_positive() {
        // 2026-08-14: 「她亲恼羞成怒」被切出「亲恼羞成」当角色名（实测误报）——
        // 情绪字（笑/哭/怒/叹）不再作触发标记，仅保留去尾角色（林晚笑道 → 林晚）
        let known: std::collections::HashSet<String> =
            ["沈棠".into(), "林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "林晚亲恼羞成怒，把茶杯重重搁下。沈棠说道：师姐消消气。",
            "",
            &known,
            &[],
            &[],
            &[],
        None,
        );
        assert!(
            !vs.iter().any(|v| v.dim == "人物"),
            "成语/情绪片段不应误报为角色: {vs:?}"
        );
        // 「林晚笑道」→ 情绪字去尾后命中 known → 不报
        let vs3 = guard_narrative(
            "林晚笑道：师妹多虑了。",
            "",
            &known,
            &[],
            &[],
            &[],
        None,
        );
        assert!(
            !vs3.iter().any(|v| v.dim == "人物"),
            "known 角色+情绪字不应误报: {vs3:?}"
        );
        // 真外角色由 LLM 自报清单（统一来源）检出
        let vs2 = guard_narrative(
            "王麻子说道：这玉佩是我的！",
            "",
            &known,
            &[],
            &[],
            &[],
            Some(&["王麻子".to_string()]),
        );
        assert!(
            vs2.iter().any(|v| v.dim == "人物" && v.msg.contains("王麻子")),
            "roster 统一来源应检出王麻子: {vs2:?}"
        );
    }

    #[test]
    fn guard_clean_pass() {
        let known: std::collections::HashSet<String> =
            ["林晚".into()].into_iter().collect();
        let vs = guard_narrative(
            "林晚说道：这玉佩我收下了。",
            "",
            &known,
            &["林晚".to_string()],
            &["收下玉佩".into()],
            &["收下玉佩".into()],
        None,
        );
        assert!(vs.is_empty(), "expected clean pass, got {:?}", vs);
    }

    #[test]
    fn test_split_program_from_narrative_extracts() {
        let text = "正文前\n【程序】<html><body><button onclick=\"alert(1)\">点我</button></body></html>\n【/程序】\n正文后";
        let (clean, program) = split_program_from_narrative(text);
        assert!(!clean.contains("【程序】"));
        assert!(!clean.contains("【/程序】"));
        assert!(clean.contains("正文前"));
        assert!(clean.contains("正文后"));
        let html = program.expect("program card should be extracted");
        assert!(html.starts_with("<html>"));
        assert!(html.ends_with("</html>"));
        assert!(html.contains("alert(1)"));
    }

    #[test]
    fn test_split_program_from_narrative_none() {
        let text = "普通文本";
        let (clean, program) = split_program_from_narrative(text);
        assert!(program.is_none());
        assert_eq!(clean, "普通文本");
    }

    #[test]
    fn test_split_state_updates_from_narrative_extracts() {
        let text = "旁白。\n【状态更新】{\"characterId\":\"char-1\",\"fields\":{\"hp\":95,\"hunger\":30},\"addTraits\":[{\"poolId\":\"p\",\"traitId\":\"t1\",\"name\":\"饱腹\",\"summary\":\"刚进食\"}]}\n继续。";
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert_eq!(updates.len(), 1);
        let u = &updates[0];
        assert_eq!(u.character_id, "char-1");
        assert_eq!(u.fields.get("hp"), Some(&serde_json::json!(95)));
        assert_eq!(u.fields.get("hunger"), Some(&serde_json::json!(30)));
        assert_eq!(u.add_traits.len(), 1);
        assert!(!clean.contains("【状态更新】"));
        assert!(!clean.contains("characterId"));
        assert!(clean.contains("旁白。"));
        assert!(clean.contains("继续。"));
    }

    #[test]
    fn test_split_state_updates_from_narrative_keeps_invalid() {
        // [fix §9 2026-08-16] 解析失败也剥离（正文零残留优先）：带【状态更新】标记的
        // 畸形块不再保留原文——避免「英文字符串」污染正文；warn 日志记录原文。
        let text = "旁白。\n【状态更新】{not json}\n继续。";
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert!(updates.is_empty());
        assert!(!clean.contains("【状态更新】"), "畸形块应剥离: {clean}");
        assert!(!clean.contains("{not json}"), "畸形块应剥离: {clean}");
        assert!(clean.contains("旁白。"));
        assert!(clean.contains("继续。"));
        // 空 character_id：剥离（无有效更新，正文零残留）
        let text2 = "旁白。\n【状态更新】{\"characterId\":\"\",\"fields\":{}}\n继续。";
        let (clean2, updates2) = split_state_updates_from_narrative(text2);
        assert!(updates2.is_empty());
        assert!(!clean2.contains("【状态更新】"), "空 character_id 块应剥离: {clean2}");
    }

    #[test]
    fn test_split_state_updates_from_narrative_none() {
        let text = "普通文本";
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert!(updates.is_empty());
        assert_eq!(clean, "普通文本");
    }

    #[test]
    fn test_split_state_updates_bare_json_without_marker() {
        // 无【状态更新】标记的裸 JSON 状态块（模型偶发追加在正文末尾）必须被剥离
        let text = "正文内容\n\n{\"characterId\":\"c-distil-4\",\"fields\":{\"情绪\":\"心动\",\"体力\":75},\"addTraits\":[],\"removeTraits\":[]}";
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].character_id, "c-distil-4");
        assert_eq!(updates[0].fields.get("体力"), Some(&serde_json::json!(75)));
        assert!(!clean.contains("characterId"));
        assert!(clean.contains("正文内容"));
    }

    #[test]
    fn test_split_state_updates_duplicate_keys_multi_char() {
        // LLM 偶发把多角色状态合并进单对象（重复 characterId/fields 键）→ 拆分并剥离整块
        let text = "旁白。\n【状态更新】{\"characterId\":\"c-distil-0\",\"fields\":{\"emotion\":\"心虚但坚定\",\"心情状态\":\"紧张又踏实\"},\"characterId\":\"c-distil-1\",\"fields\":{\"emotion\":\"平静\",\"心情状态\":\"平和\"}}\n继续。";
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert_eq!(updates.len(), 2, "应拆出两个角色状态");
        assert_eq!(updates[0].character_id, "c-distil-0");
        assert_eq!(updates[0].fields.get("emotion"), Some(&serde_json::json!("心虚但坚定")));
        assert_eq!(updates[1].character_id, "c-distil-1");
        assert_eq!(updates[1].fields.get("心情状态"), Some(&serde_json::json!("平和")));
        assert!(!clean.contains("【状态更新】"));
        assert!(!clean.contains("characterId"));
        assert!(clean.contains("旁白。"));
        assert!(clean.contains("继续。"));
    }

    #[test]
    fn test_split_state_updates_duplicate_keys_bare() {
        // 无标记裸 JSON 的重复键变体同样要拆分剥离
        let text = "正文\n{\"characterId\":\"c-distil-0\",\"fields\":{\"emotion\":\"心虚\"},\"characterId\":\"c-distil-1\",\"fields\":{\"emotion\":\"平静\"}}";
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].character_id, "c-distil-0");
        assert_eq!(updates[1].character_id, "c-distil-1");
        assert!(!clean.contains("characterId"));
        assert!(clean.contains("正文"));
    }

    #[test]
    fn test_split_state_updates_duplicate_keys_partial_still_keeps() {
        // [fix §9 2026-08-16] 拆分后子段含非法 JSON → 无法提取有效更新，但块仍剥离
        // （正文零残留优先；结构特征【状态更新】+JSON 极特定，不误删正文）。
        let text = "旁白。\n【状态更新】{\"characterId\":\"c-distil-0\",\"fields\":{\"emotion\":\"心虚\"},\"characterId\":\"c-distil-1\",\"fields\":{bad json}}\n继续。";
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert!(updates.is_empty());
        assert!(!clean.contains("【状态更新】"), "畸形块应剥离: {clean}");
        assert!(!clean.contains("bad json"), "畸形块应剥离: {clean}");
        assert!(clean.contains("旁白。"));
        assert!(clean.contains("继续。"));
    }

    #[test]
    fn test_split_state_updates_bare_json_invalid_kept() {
        // 裸 JSON 无法解析为状态块 → 保留原文不误删
        let text = r#"正文里有一个 {"普通":"对象"} 不是状态块"#;
        let (clean, updates) = split_state_updates_from_narrative(text);
        assert!(updates.is_empty());
        assert!(clean.contains("普通"));
    }

    #[test]
    fn test_split_check_from_narrative_extracts() {
        let text = "旁白。\n【检定】{\"action\":\"潜行\",\"intent\":\"溜进仓库\",\"challenge\":\"守卫巡逻\",\"cost\":\"被发现\",\"difficulty\":\"normal\",\"bonuses\":[{\"reason\":\"地形掩护\",\"value\":2}],\"outcomes\":{\"success\":{\"result\":\"你成功溜了进去\"}}}\n继续。";
        let (clean, checks) = split_check_from_narrative(text);
        assert_eq!(checks.len(), 1);
        let c = &checks[0];
        assert_eq!(c.action, "潜行");
        assert_eq!(c.difficulty, "normal");
        assert_eq!(c.bonuses.len(), 1);
        assert_eq!(c.bonuses[0].value, 2.0);
        assert_eq!(c.outcomes.success.result, "你成功溜了进去");
        assert!(!clean.contains("【检定】"));
        assert!(!clean.contains("outcomes"));
        assert!(clean.contains("旁白。"));
        assert!(clean.contains("继续。"));
    }

    #[test]
    fn test_split_check_from_narrative_keeps_invalid() {
        // 非法 JSON：跳过该块但保留原文
        let text = "旁白。\n【检定】{not json}\n继续。";
        let (clean, checks) = split_check_from_narrative(text);
        assert!(checks.is_empty());
        assert!(clean.contains("【检定】"));
        assert!(clean.contains("{not json}"));
        // 空 action：同样跳过但保留原文
        let text2 = "旁白。\n【检定】{\"action\":\"\",\"intent\":\"x\",\"challenge\":\"y\"}\n继续。";
        let (clean2, checks2) = split_check_from_narrative(text2);
        assert!(checks2.is_empty());
        assert!(clean2.contains("【检定】"));
    }

    #[test]
    fn test_split_check_from_narrative_none() {
        let text = "普通文本";
        let (clean, checks) = split_check_from_narrative(text);
        assert!(checks.is_empty());
        assert_eq!(clean, "普通文本");
    }

    #[test]
    fn test_split_director_plan_from_narrative_extracts() {
        let text = "旁白。\n【导演计划】{\"goal\":\"引出铜铃来信\",\"pressure\":\"门外脚步已停\",\"cost\":\"若拆信则再无回头路\",\"hits_beats\":[\"信的存在不可抹除\"]}\n继续。";
        let (clean, update) = split_director_plan_from_narrative(text);
        match update {
            Some(DirectorPlanUpdate::Set(out)) => {
                assert_eq!(out.goal, "引出铜铃来信");
                assert_eq!(out.pressure.as_deref(), Some("门外脚步已停"));
                assert_eq!(out.cost.as_deref(), Some("若拆信则再无回头路"));
                assert_eq!(out.hits_beats, vec!["信的存在不可抹除".to_string()]);
            }
            other => panic!("expected Set, got {other:?}"),
        }
        assert!(!clean.contains("【导演计划】"));
        assert!(!clean.contains("goal"));
        assert!(clean.contains("旁白。"));
        assert!(clean.contains("继续。"));
    }

    #[test]
    fn test_split_director_plan_from_narrative_none_marker() {
        let text = "旁白。\n【导演计划】none\n继续。";
        let (clean, update) = split_director_plan_from_narrative(text);
        assert!(matches!(update, Some(DirectorPlanUpdate::Skip)));
        assert!(!clean.contains("【导演计划】"));
        assert!(!clean.contains("none"));
        assert!(clean.contains("旁白。"));
        assert!(clean.contains("继续。"));
    }

    #[test]
    fn test_split_director_plan_from_narrative_keeps_invalid() {
        // 非法 JSON：保留原文不剥离
        let text = "旁白。\n【导演计划】{not json}\n继续。";
        let (clean, update) = split_director_plan_from_narrative(text);
        assert!(update.is_none());
        assert!(clean.contains("【导演计划】"));
        assert!(clean.contains("{not json}"));
        // goal 为空：不算有效计划，保留原文
        let text2 = "旁白。\n【导演计划】{\"goal\":\"\",\"pressure\":\"x\"}\n继续。";
        let (clean2, update2) = split_director_plan_from_narrative(text2);
        assert!(update2.is_none());
        assert!(clean2.contains("【导演计划】"));
    }

    #[test]
    fn test_split_director_plan_from_narrative_none() {
        let text = "普通文本";
        let (clean, update) = split_director_plan_from_narrative(text);
        assert!(update.is_none());
        assert_eq!(clean, "普通文本");
    }

    // ─── P0 写作三档（quality）单元测试 ───────────────────────────────────

    // 本地 mock LLM：预置响应序列，记录调用阶段（review/fix/plan/write/gate）。
    struct MockLlm {
        responses: std::cell::RefCell<std::collections::VecDeque<String>>,
        log: std::cell::RefCell<Vec<String>>,
    }

    impl MockLlm {
        fn new(responses: Vec<&str>) -> Self {
            MockLlm {
                responses: std::cell::RefCell::new(
                    responses.into_iter().map(String::from).collect(),
                ),
                log: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    impl QualityLlm for MockLlm {
        fn call(
            &self,
            system: &str,
            _user: &str,
        ) -> std::pin::Pin<Box<dyn futures_util::Future<Output = Result<String, String>> + Send + '_>>
        {
            let phase = if system.contains("审稿人") {
                "review"
            } else if system.contains("内容负责人") {
                "fix"
            } else if system.contains("剧情策划") {
                "plan"
            } else if system.contains("终检评审") {
                "gate"
            } else if system.contains("正文作者") {
                "write"
            } else if system.contains("状态补丁") {
                "memory"
            } else {
                "other"
            };
            self.log.borrow_mut().push(phase.to_string());
            let resp = self
                .responses
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| "{\"pass\":true}".to_string());
            Box::pin(async move { Ok(resp) })
        }
    }

    #[test]
    fn test_quality_default_is_lite() {
        assert_eq!(resolve_turn_quality(None), TurnQuality::Lite);
        assert_eq!(
            resolve_turn_quality(Some(TurnQuality::Standard)),
            TurnQuality::Standard
        );
        assert_eq!(
            resolve_turn_quality(Some(TurnQuality::Heavy)),
            TurnQuality::Heavy
        );
    }

    #[test]
    fn test_quality_old_request_body_compat() {
        // 旧请求体无 quality 字段 → 兼容解析为 None → lite
        let req: TurnStartRequest = serde_json::from_value(json!({"message": "你好"}))
            .expect("old body without quality must parse");
        assert!(req.quality.is_none());
        assert_eq!(resolve_turn_quality(req.quality), TurnQuality::Lite);
        // 新请求体显式 standard/heavy/非法值
        let req2: TurnStartRequest = serde_json::from_value(
            json!({"message": "x", "quality": "standard"}),
        )
        .expect("standard quality must parse");
        assert_eq!(req2.quality, Some(TurnQuality::Standard));
        let req3: TurnStartRequest =
            serde_json::from_value(json!({"message": "x", "quality": "heavy"}))
                .expect("heavy quality must parse");
        assert_eq!(req3.quality, Some(TurnQuality::Heavy));
        let req4: TurnStartRequest =
            serde_json::from_value(json!({"message": "x", "quality": "weird"}))
                .expect("unknown quality must fall back to lite");
        assert_eq!(resolve_turn_quality(req4.quality), TurnQuality::Lite);
    }

    #[test]
    fn test_plan_quality_stages_semantics() {
        // lite = 单次直出，不启动审稿/修稿子流程
        assert_eq!(plan_quality_stages(TurnQuality::Lite), vec![QualityStage::Write]);
        // standard = 初稿 → 审稿 → 修订
        let std_stages = plan_quality_stages(TurnQuality::Standard);
        assert!(std_stages.contains(&QualityStage::Write));
        assert!(std_stages.contains(&QualityStage::Review));
        assert!(std_stages.contains(&QualityStage::Fix));
        assert!(!std_stages.contains(&QualityStage::FinalGate));
        assert_eq!(std_stages.len(), 3);
        // heavy = 全管道（round 内执行；P4 起 MemoryPatch 归入管道末尾）
        let heavy_stages = plan_quality_stages(TurnQuality::Heavy);
        assert!(heavy_stages.contains(&QualityStage::ContextPlan));
        assert!(heavy_stages.contains(&QualityStage::Write));
        assert!(heavy_stages.contains(&QualityStage::Review));
        assert!(heavy_stages.contains(&QualityStage::Fix));
        assert!(heavy_stages.contains(&QualityStage::FinalGate));
        assert!(
            heavy_stages.contains(&QualityStage::MemoryPatch),
            "P4: heavy 管道末尾含 MemoryPatch（产出可应用 patch）"
        );
        // 上限防死循环
        assert_eq!(QUALITY_MAX_FIX_ROUNDS, 2);
    }

    #[tokio::test]
    async fn test_quality_pipeline_lite_keeps_single_path() {
        let mock = MockLlm::new(vec![]);
        let (out, memory_patch) = run_quality_refine(
            TurnQuality::Lite,
            &mock,
            "sys",
            "user",
            "初稿正文",
            None,
            &mut String::new(),
        )
        .await
        .expect("lite path must succeed");
        assert_eq!(out, "初稿正文");
        assert!(
            memory_patch.is_none(),
            "lite 不产出 memory patch"
        );
        assert!(
            mock.calls().is_empty(),
            "lite 不得发起任何额外 LLM 调用，实际：{:?}",
            mock.calls()
        );
    }

    #[test]
    fn strip_lite_reasoning_leak_keeps_normal_narrative() {
        // 正常结构化回合（含 <场景 标签）与干净叙事不得误伤。
        let tagged = "<场景：雨巷旧茶馆｜深夜>\n\n雨声在门外绵密地落着。";
        assert_eq!(strip_lite_reasoning_leak(tagged.to_string()), tagged);
        let clean = "雨声像一件没缝完的衣裳，继续在屋檐上落着。\n\n林晚松开了你的衣襟。";
        assert_eq!(strip_lite_reasoning_leak(clean.to_string()), clean);
    }

    #[test]
    fn strip_lite_reasoning_leak_strips_now_need_variant() {
        // [fix 2026-08-16] flash-free「现在我需要/XU需要」开头思考：reasoning_content
        // 只装第一段，后续规划段混进 content——旧表缺这些前缀 → 不剥离。现在应剥掉
        // 思考段，保留真实叙事段。
        let leaked = "现在我需要自然地融入这个事件卡，但要注意它只是背景素材。\n\n\
            我环顾四周，观察了这个蜜月套房的环境——木质地板、海风、大床。\n\n\
            妈妈从卫生间出来，我们简短地交谈了几句。\n\n\
            我需要继续推进这个场景，保持暧昧氛围。";
        let out = strip_lite_reasoning_leak(leaked.to_string());
        assert!(out.starts_with("我环顾四周"), "应保留叙事段，实际: {:?}", &out[..30.min(out.len())]);
        assert!(!out.contains("现在我需要"), "思考段应被剥离");
        assert!(!out.contains("我需要继续"), "尾部思考应被剥离");
    }

    #[test]
    fn strip_trailing_metadiscourse_strips_now_write_variant() {
        // [fix 2026-08-16 实踩]「嗯，这样应该可以。现在让我把完整的叙事写出来。」
        // +「还要注意：- 正文末尾需要角色清单」——正文后的自检尾巴。
        let leaked = "替她拨开，指尖却故意在她耳垂上多停了一秒，看她会不会脸红。\n\n\
            嗯，这样应该可以。现在让我把完整的叙事写出来。\n\n\
            还要注意：\n- 正文末尾需要角色清单";
        let out = strip_trailing_metadiscourse(leaked.to_string());
        assert!(out.contains("看她会不会脸红"), "正文应保留");
        assert!(!out.contains("这样应该可以"), "尾部元话语应剥离");
        assert!(!out.contains("还要注意"), "清单段应剥离");
    }

    #[test]
    fn strip_lite_reasoning_leak_keeps_short_sentence_narrative() {
        // [fix 2026-08-16 剥离误杀] 中文正文短句段（「走过去。」4字、「没有说话。」
        // 5字）不应被当思考剥掉。旧阈值 len>=10 把整篇短句正文剥光只剩思考尾
        // （实踩 21:05 回合 content=42 字）。阈值降至 2 后应保留正文。
        let leaked = "让我理清一下当前状态。\n\n我走过去。\n\n从背后轻轻抱住她。\n\n她没有说话。\n\n海风把她的发梢吹到我的指间。\n\n最后，让我确认一下【节点推进】标记。";
        let out = strip_lite_reasoning_leak(leaked.to_string());
        assert!(out.contains("我走过去"), "短句正文应保留，实际: {:?}", &out[..40.min(out.len())]);
        assert!(out.contains("轻轻抱住她"), "正文应保留");
        assert!(out.contains("发梢吹到"), "正文应保留");
        assert!(!out.starts_with("让我理清"), "开头思考应被剥离");
        assert!(!out.contains("让我确认一下"), "尾部思考应被剥离");
    }

    #[test]
    fn strip_lite_reasoning_leak_extracts_clean_body() {
        // 无标签思维链泄漏：推理段包裹正文且重复多次，取最长连续叙事段组。
        let leaked = "根据章节大纲，这一章是“雨巷来客”。\n\n让我重新阅读一下提示。\n\n\
            雨声像一件没缝完的衣裳，继续在屋檐上落着。\n\n林晚松开了你的衣襟，手指没有完全离开。\n\n\
            「天快亮了。」她说，声音有些哑。\n\n可她没有动，也没有让你去拿伞。\n\n\
            然后提供选项。角色清单：沈棠、林晚。";
        let out = strip_lite_reasoning_leak(leaked.to_string());
        // 最长连续叙事组 = 雨声→可她没有动（4 段），尾部元话语段被剔除。
        assert!(out.contains("雨声像一件没缝完的衣裳"), "应保留正文：{out}");
        assert!(out.contains("可她没有动"), "应保留正文尾部：{out}");
        assert!(!out.contains("根据章节大纲"), "推理段应剔除：{out}");
        assert!(!out.contains("然后提供选项"), "尾部元话语应剔除：{out}");
        assert!(!out.contains("角色清单"), "角色清单应剔除：{out}");
        assert!(!out.contains("让我重新阅读"), "思维段应剔除：{out}");
    }

    #[test]
    fn strip_lite_reasoning_leak_too_short_keeps_original() {
        // 无可剥离正文（叙事段不足）→ 保守返回原文。
        let only_meta = "让我重新阅读一下提示。\n\n让我再检查一次。";
        assert_eq!(strip_lite_reasoning_leak(only_meta.to_string()), only_meta);
    }

    #[tokio::test]
    async fn test_quality_pipeline_standard_triggers_review_and_fix() {
        let mock = MockLlm::new(vec!["审稿：人物声线不稳", "修订后的完整正文"]);
        let (out, _patch) = run_quality_refine(
            TurnQuality::Standard,
            &mock,
            "sys",
            "user",
            "初稿正文",
            None,
            &mut String::new(),
        )
        .await
        .expect("standard path must succeed");
        assert_eq!(out, "修订后的完整正文");
        assert_eq!(mock.calls(), vec!["review".to_string(), "fix".to_string()]);
    }

    #[tokio::test]
    async fn test_quality_pipeline_standard_strips_fix_thinking() {
        // fix 输出带 <thinking>/<story> 结构 → 正文纯净化，思维汇入 thinking_out 折叠。
        let mock = MockLlm::new(vec![
            "审稿：人物声线不稳",
            "<thinking>我要根据审稿意见逐条修订，先分析声线问题…</thinking>\n\n<story>修订后的完整正文</story>",
        ]);
        let mut thinking_out = String::new();
        let (out, _patch) = run_quality_refine(
            TurnQuality::Standard,
            &mock,
            "sys",
            "user",
            "初稿正文",
            None,
            &mut thinking_out,
        )
        .await
        .expect("standard path must succeed");
        assert_eq!(out, "修订后的完整正文");
        assert!(thinking_out.contains("逐条修订"), "思维段应汇入折叠区");
    }

    #[tokio::test]
    async fn test_quality_pipeline_heavy_rounds_capped() {
        // gate 永远不通过 → 循环必须在上限 QUALITY_MAX_FIX_ROUNDS=2 处截断，防死循环。
        // [fix 2026-08-15] memory 阶段响应改为合法 patch JSON（原数据误用 gate verdict
        // {"pass":false,...} → 触发 P4.1 repair 修复循环 → 多一次 "other" 调用 → 断言红；
        // 该失败为既有（HEAD 8376ed55 上即红），随本批顺手根治）。
        let mock = MockLlm::new(vec![
            "计划", "正文", "审稿意见", "第一版修订",
            "{\"pass\":false,\"problems\":\"连续性差\"}",
            "第二版修订",
            "{\"pass\":false,\"problems\":\"仍不稳\"}",
            "第三版修订",
            "{\"progress\":\"推进主线\",\"character_state\":{},\"world_state\":\"已进入矿区\",\"foreshadowing\":\"矿洞深处有异常\"}",
        ]);
        let (out, memory_patch) = run_quality_refine(
            TurnQuality::Heavy,
            &mock,
            "sys",
            "user",
            "初稿正文",
            None,
            &mut String::new(),
        )
        .await
        .expect("heavy path must succeed");
        assert_eq!(out, "第三版修订");
        let calls = mock.calls();
        let gate_calls = calls.iter().filter(|c| *c == "gate").count();
        let fix_calls = calls.iter().filter(|c| *c == "fix").count();
        assert_eq!(
            gate_calls, 2,
            "gate 调用必须被上限截断（≤2），实际：{calls:?}"
        );
        assert_eq!(fix_calls, 3, "初始修订+2次重修，实际：{calls:?}");
        // 阶段顺序：plan → write → review → fix → gate (→ fix → gate) → memory
        assert_eq!(calls[0], "plan");
        assert_eq!(calls[1], "write");
        assert_eq!(calls[2], "review");
        assert_eq!(
            calls.last().map(|s| s.as_str()),
            Some("memory"),
            "heavy 管道末尾须执行 MemoryPatch：{calls:?}"
        );
        assert_eq!(
            memory_patch,
            Some("{\"progress\":\"推进主线\",\"character_state\":{},\"world_state\":\"已进入矿区\",\"foreshadowing\":\"矿洞深处有异常\"}".to_string())
        );
    }

    #[tokio::test]
    async fn test_quality_pipeline_heavy_gate_passes_stops_early() {
        let mock = MockLlm::new(vec![
            "计划",
            "正文",
            "审稿意见",
            "达标修订",
            "{\"pass\":true}",
            "{\"progress\":\"...\",\"character_state\":{},\"world_state\":\"...\",\"foreshadowing\":\"...\"}",
        ]);
        let (out, memory_patch) = run_quality_refine(
            TurnQuality::Heavy,
            &mock,
            "sys",
            "user",
            "初稿正文",
            None,
            &mut String::new(),
        )
        .await
        .expect("heavy path must succeed");
        assert_eq!(out, "达标修订");
        let calls = mock.calls();
        assert_eq!(calls.iter().filter(|c| *c == "gate").count(), 1);
        assert_eq!(calls.iter().filter(|c| *c == "fix").count(), 1);
        assert!(memory_patch.is_some(), "heavy 达标后仍产出 memory patch");
    }

    #[tokio::test]
    async fn test_quality_pipeline_heavy_memory_patch_failure_keeps_text() {
        // MemoryPatch LLM 失败 → 只记日志，正文不丢。
        let mock = MockLlm::new(vec![
            "计划",
            "正文",
            "审稿意见",
            "达标修订",
            "{\"pass\":true}",
        ]);
        let (out, memory_patch) = run_quality_refine(
            TurnQuality::Heavy,
            &mock,
            "sys",
            "user",
            "初稿正文",
            None,
            &mut String::new(),
        )
        .await
        .expect("heavy path must succeed");
        // 响应耗尽后 MockLlm pop_front 返回 None → memory 阶段失败（模拟上游超时）
        assert_eq!(out, "达标修订");
        assert!(memory_patch.is_none() || memory_patch.as_deref() == Some("{\"pass\":true}"));
    }

    #[test]
    fn test_quality_gate_passes_parse() {
        assert!(gate_passes("{\"pass\":true}"));
        assert!(!gate_passes("{\"pass\":false,\"problems\":\"连续性差\"}"));
        assert!(gate_passes("{\"pass\": false}") == false);
        assert!(gate_passes("some non-json"), "无法解析时保守视为通过");
    }

    // ─── skill 工具按需加载（P4 后置②）─────────────────────────────────────

    fn test_pack_minimal() -> StoryPack {
        StoryPack {
            id: "p1".into(),
            title: "测试剧本".into(),
            source: Default::default(),
            characters: vec![],
            world_book_ids: vec![],
            chapters: vec![],
            nodes: vec![],
            lore_entries: vec![],
            default_mode: Default::default(),
            max_tier: Default::default(),
            language: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            stage_director: Default::default(),
            event_packages: vec![],
            actor_state_config: Default::default(),
            worldline: vec![], // T 层测试 fixture
        }
    }

    fn test_session_minimal() -> TavernSession {
        serde_json::from_value(json!({
            "sessionId": "s1",
            "packId": "p1",
            "playable": "P1",
            "playMode": "mainline",
            "contentTier": "standard",
        }))
        .expect("minimal session must parse")
    }

    #[test]
    fn ledger_extract_entries_heuristics() {
        // 场景标签 + 角色状态 + 伏笔 + 物品 + 好感 全覆盖
        let body = "<场景：海边小屋｜午后>\n陆川轻轻把肖静搂进怀里，目光温柔。\n「旧玉坠」从她衣领里滑出来，他指尖摩挲着，轻声问：\n\"你还没告诉我，这枚玉坠的来历。\"\n肖静抚着隆起的小腹，眼底有些不安。";
        let chars = ["陆川", "肖静"];
        let entries = ledger_extract_entries(body, &chars);

        let kinds: Vec<LedgerKind> = entries.iter().map(|(k, _, _)| k.clone()).collect();
        assert!(kinds.contains(&LedgerKind::Time), "应提取时间，实际 {kinds:?}");
        assert!(kinds.contains(&LedgerKind::Status), "应提取状态，实际 {kinds:?}");
        assert!(kinds.contains(&LedgerKind::Foreshadow), "应提取伏笔，实际 {kinds:?}");
        assert!(kinds.contains(&LedgerKind::Item), "应提取物品，实际 {kinds:?}");
        assert!(kinds.contains(&LedgerKind::Affinity), "应提取好感，实际 {kinds:?}");

        // 物品：旧玉坠（对话「你还没告诉我」排除——含"告诉"但无虚词，实际提取的应为「旧玉坠」）
        let items: Vec<&str> = entries
            .iter()
            .filter(|(k, _, _)| matches!(k, LedgerKind::Item))
            .map(|(_, key, _)| key.as_str())
            .collect();
        assert!(items.iter().any(|s| s.contains("旧玉坠")), "物品应含旧玉坠，实际 {items:?}");
        // 对话性「」不应入账本
        assert!(!items.iter().any(|s| s.contains("你还没")), "对话不应入账本，实际 {items:?}");

        // 状态 key 应为角色名（肖静）
        let statuses: Vec<&str> = entries
            .iter()
            .filter(|(k, _, _)| matches!(k, LedgerKind::Status))
            .map(|(_, key, _)| key.as_str())
            .collect();
        assert!(statuses.iter().any(|s| *s == "肖静"), "状态应绑定肖静，实际 {statuses:?}");
    }

    #[test]
    fn ledger_extract_entries_limits() {
        // 限流：每类最多 N 条
        let body = "「玉佩」「香囊」「荷包」「扇子」「书卷」……肖静怀孕了，陆川发烧了，两人都很不安，心里藏着秘密，还有谜。";
        let chars = ["陆川", "肖静"];
        let entries = ledger_extract_entries(body, &chars);

        let items: usize = entries
            .iter()
            .filter(|(k, _, _)| matches!(k, LedgerKind::Item))
            .count();
        assert!(items <= 3, "物品最多 3 条，实际 {items}");

        let statuses: usize = entries
            .iter()
            .filter(|(k, _, _)| matches!(k, LedgerKind::Status))
            .count();
        assert!(statuses <= 2, "状态最多 2 条，实际 {statuses}");

        let hooks: usize = entries
            .iter()
            .filter(|(k, _, _)| matches!(k, LedgerKind::Foreshadow))
            .count();
        assert!(hooks <= 2, "伏笔最多 2 条，实际 {hooks}");
    }

    #[test]
    fn ledger_extract_entries_empty_no_panic() {
        let entries = ledger_extract_entries("", &["陆川", "肖静"]);
        assert!(entries.is_empty(), "空正文不应产生条目，实际 {entries:?}");
    }

    #[test]
    fn test_build_tavern_system_prompt_skill_load_injected() {
        let pack = test_pack_minimal();
        let tools: Vec<crate::tavern_mcp::McpToolEntry> = vec![];
        let tmp = std::env::temp_dir();
        let base =
            build_tavern_system_prompt(&pack, &test_session_minimal(), "", &tmp, None, &tools, None);
        assert!(
            !base.contains("你请求加载的完整写作 Skill"),
            "未请求时不得注入 skill_load：{base}"
        );
        let mut session = test_session_minimal();
        session.skill_load = Some(kaleido_core::SkillLoadInfo {
            tier: "heavy".into(),
            markdown: "## 完整写作 Skill（heavy）\n特征行 ST-SKILL-FEATURE\n## 模板\n### plan\n大纲".into(),
        });
        let out = build_tavern_system_prompt(&pack, &session, "", &tmp, None, &tools, None);
        assert!(
            out.contains("你请求加载的完整写作 Skill"),
            "回填标记应注入：{out}"
        );
        assert!(out.contains("ST-SKILL-FEATURE"), "SKILL.md 特征串注入");
        assert!(out.contains("## 模板"), "模板段随全文注入");
    }

    // T 层 (2026-08-19): 原著时间线注入——按当前章过滤，首回合只见已推进事件，后期事件零泄漏。
    #[test]
    fn worldline_block_filters_by_current_chapter() {
        let mut pack = test_pack_minimal();
        pack.worldline = serde_json::from_str::<Vec<serde_json::Value>>(r#"[
            {"chapter":"ch01","event":"主角放学接到父亲电话","time_point":"放学后","importance":"low"},
            {"chapter":"ch02","event":"与母亲抵达度假村","time_point":"抵达","importance":"high"},
            {"chapter":"ch06","event":"蜜月第一晚相处","time_point":"夜晚","importance":"medium"},
            {"chapter":"ch10","event":"相亲对象登场","time_point":"周末","importance":"high"}
        ]"#).unwrap();

        // 首回合（ch01）：只注入 ch01，ch02/ch06/ch10 全被过滤
        let b1 = build_worldline_block(&pack, "ch01").expect("ch01 应有时间线");
        assert!(b1.contains("放学接到父亲电话"), "ch01 事件应注入: {b1}");
        assert!(!b1.contains("抵达度假村"), "ch02 不得在首回合泄漏");
        assert!(!b1.contains("蜜月第一晚"), "ch06 不得在首回合泄漏");
        assert!(!b1.contains("相亲对象"), "ch10 不得在首回合泄漏");

        // ch02：注入 ch01+ch02，仍不含 ch06/ch10
        let b2 = build_worldline_block(&pack, "ch02").expect("ch02 应有时间线");
        assert!(b2.contains("抵达度假村"), "ch02 事件应注入");
        assert!(!b2.contains("蜜月第一晚"), "ch06 不得泄漏");
        assert!(!b2.contains("相亲对象"), "ch10 不得泄漏");

        // 无章节标注的事件（解析失败）保守不注入
        let mut bad = test_pack_minimal();
        bad.worldline = serde_json::from_str::<Vec<serde_json::Value>>(r#"[{"event":"无章号事件"}]"#).unwrap();
        assert!(build_worldline_block(&bad, "ch01").is_none(), "无章号事件不注入: {bad:?}");

        // 空 worldline（旧 pack）→ None 零注入
        assert!(build_worldline_block(&test_pack_minimal(), "ch01").is_none());
    }

    // ─── strip_director_preamble 导演自白剥离 单测 ─────────────────────────

    #[test]
    fn strip_preamble_removes_director_confession_before_scene_tag() {
        // R4 泄漏模式：「好的，我是林逸。…」直到 <场景 标签前
        let mut body = "好的，我是林逸。我看着妈妈，她还在撒谎。我决定跟她摊牌，让她知道我能扛。<场景：林逸家客厅｜傍晚｜林逸、陆清韵>\n\n陆清韵捧着水杯的手猛地一颤。".to_string();
        let stripped = strip_director_preamble(&mut body);
        assert!(stripped.contains("好的，我是林逸"));
        assert!(stripped.contains("摊牌"));
        assert!(!stripped.contains("<场景"));
        assert!(body.starts_with("<场景：林逸家客厅"));
        assert!(body.contains("陆清韵捧着水杯"));
    }

    #[test]
    fn strip_preamble_keeps_normal_narrative_untouched() {
        // 正常正文不以「好的，我是」开头 → 原样保留
        let mut body = "<场景：林逸家客厅｜傍晚｜林逸、陆清韵>\n\n陆清韵闻声回头，眼角还带着泪痕。".to_string();
        let stripped = strip_director_preamble(&mut body);
        assert!(stripped.is_empty());
        assert!(body.starts_with("<场景"));
    }

    #[test]
    fn strip_preamble_no_scene_tag_keeps_whole_body() {
        // R7 泄漏模式（无 <场景 边界）→ 保守不剥，宁可保留
        let mut body = "好的，我是林逸。敲门声又响了，我从平板监控里看到了那个光头男。他是我雇的人，来执行我的计划。妈听到敲门声肯定会紧张。".to_string();
        let stripped = strip_director_preamble(&mut body);
        assert!(stripped.is_empty());
        assert!(body.starts_with("好的，我是林逸"));
    }

    // ─── strip_fix_thinking_blocks 结构剥离（思维自动折叠）单测 ─────────────
    #[test]
    fn strip_trailing_metadiscourse_removes_self_check_tail() {
        // P2 实踩模式：正文后接「让我检查…」自检段
        let mut body = "她把那团布料叠好放在茶几上。\n\n让我检查一下“胸口起伏”是否与上文重复……\n\n再检查是否需要【状态更新】：不需要。\n\n好，让我输出最终版本。".to_string();
        let _ = strip_fix_thinking_blocks(&mut body);
        assert!(body.contains("布料叠好"), "正文应保留: {body}");
        assert!(!body.contains("让我检查"), "自检段应被剥离: {body}");
        assert!(!body.contains("让我输出"), "收尾段应被剥离: {body}");
    }

    #[test]
    fn strip_trailing_metadiscourse_multi_dash_takes_final_version() {
        // P2 多段 --- 拼接：取最后一段正文（若末尾是元话语则继续前溯）
        let mut body = "草稿一。\n\n---\n\n让我重新组织：检查问题1…\n\n---\n\n最终正文，干净收尾。".to_string();
        let _ = strip_fix_thinking_blocks(&mut body);
        assert!(body.contains("最终正文"), "应保留最终版: {body}");
        assert!(!body.contains("让我重新组织"), "中间元话语段应剥离: {body}");
    }

    #[test]
    fn strip_trailing_metadiscourse_strips_flashfree_self_check_tail() {
        // [fix 2026-08-16] 真实样本（msg35）：deepseek-v4-flash-free 在正文+【角色清单】后
        // 输出「嗯，这个版本不错。让我检查一下…✓ 检查清单 + 状态更新思考」，推理混入正文。
        let raw = "<场景：宿舍｜深夜>\n\n向明初把手机扣在胸口，闭上眼睛。那个寒假，值得等。\n\n【角色清单】向明初、山楂\n\n嗯，这个版本不错。让我检查一下：\n1. 时间：深夜，初冬 ✓\n2. 角色：向明初、山楂 ✓\n\n状态更新：向明初的心情从“紧张”变为“期待”。\n\n让我写状态更新块：\n\n等等，c-distil-0的字段有“心情状态”和“金钱”吗？让我看看。\n\n我应该更新哪个？我不确定哪个是权威的。让我都更新吧。\n\n好，让我写最终版本。\n\n另外，我需要注意正文末尾的【角色清单】和状态更新块。让我安排好顺序：\n1. 正文\n2. 【角色清单】向明初、山楂\n3."
            .to_string();
        let out = strip_trailing_metadiscourse(raw);
        assert!(!out.contains("版本不错"), "自检段应剥离: {out}");
        assert!(!out.contains("让我检查一下"), "自检段应剥离: {out}");
        assert!(!out.contains("状态更新"), "状态思考应剥离: {out}");
        assert!(out.contains("那个寒假，值得等"), "正文应保留: {out}");
        assert!(out.contains("【角色清单】"), "角色清单应保留: {out}");
        // 正文以自然句结尾（不再以「3.」等推理序号收尾）
        assert!(!out.trim_end().ends_with("3."), "不应以推理序号收尾: {out}");
    }

    #[test]
    fn strip_trailing_metadiscourse_keeps_normal_ending() {
        // 正常正文结尾不被误伤
        let mut body = "她没回头。窗外的滴水声又响了两下，才听见她轻轻“嗯”了一声。\n\n【角色清单】母亲、我".to_string();
        let _ = strip_fix_thinking_blocks(&mut body);
        assert!(body.contains("滴水声"), "正文应完整保留: {body}");
        assert!(body.contains("角色清单"), "选项块应保留: {body}");
    }

    #[test]
    fn strip_fix_thinking_blocks_extracts_thinking_and_story() {
        let mut body = "<thinking>好的，我需要根据审稿意见逐条分析：1. 人物声线不稳 2. 节奏拖沓</thinking>\n\n<story>这是修订后的完整正文，人物声线统一。</story>".to_string();
        let thinking = strip_fix_thinking_blocks(&mut body);
        assert!(thinking.contains("审稿意见"), "思维段应剥离返回");
        assert_eq!(body, "这是修订后的完整正文，人物声线统一。");
    }

    #[test]
    fn strip_fix_thinking_blocks_without_tags_keeps_body() {
        let mut body = "普通叙事正文，没有标签。".to_string();
        let thinking = strip_fix_thinking_blocks(&mut body);
        assert!(thinking.is_empty(), "无标签必须原样返回");
        assert_eq!(body, "普通叙事正文，没有标签。");
    }

    #[test]
    fn strip_fix_thinking_blocks_story_without_thinking() {
        let mut body = "<story>只有正文块</story>".to_string();
        let thinking = strip_fix_thinking_blocks(&mut body);
        assert!(thinking.is_empty());
        assert_eq!(body, "只有正文块");
    }

    #[test]
    fn strip_fix_thinking_blocks_unclosed_story_strips_open_tag() {
        // [fix 2026-08-15] <story> 开标签无 </story> 闭合（正文被 max_tokens
        // 截断实踩：宿醉 msg 残留「<story> 烛火在茶几上跳着…」）：剥开标签，
        // 保留其后正文，不泄漏标签残片。
        let mut body = "<story>烛火在茶几上跳着。我指尖还搭在她肩头。".to_string();
        let thinking = strip_fix_thinking_blocks(&mut body);
        assert!(thinking.is_empty());
        assert!(!body.contains("<story>"), "开标签残片必须剥离");
        assert!(body.starts_with("烛火在茶几上跳着"), "正文内容保留");
    }

    #[test]
    fn strip_fix_thinking_blocks_unclosed_story_with_prefix_keeps_prefix() {
        // <story> 前有普通正文（模型把部分正文写在标签外）→ 前缀+块内都保留
        let mut body = "开头几句正文。<story>中间正文被截断".to_string();
        let thinking = strip_fix_thinking_blocks(&mut body);
        assert!(thinking.is_empty());
        assert!(!body.contains("<story>"));
        assert!(body.contains("开头几句正文。"), "前缀正文保留");
        assert!(body.contains("中间正文被截断"), "块内正文保留");
    }

    #[test]
    fn strip_fix_thinking_blocks_unclosed_tag_truncates() {
        let mut body = "正文开头\n<thinking>只有开头没有闭合".to_string();
        let thinking = strip_fix_thinking_blocks(&mut body);
        assert!(thinking.is_empty());
        assert_eq!(body, "正文开头", "开标签残片应截断，保留标签前内容");
    }

    #[test]
    fn strip_fix_thinking_blocks_multiple_blocks() {
        let mut body = "前文\n<thinking>第一段思维</thinking>\n中间\n<thinking>第二段思维</thinking>\n<story>最终正文</story>".to_string();
        let thinking = strip_fix_thinking_blocks(&mut body);
        assert!(thinking.contains("第一段思维") && thinking.contains("第二段思维"));
        assert_eq!(body, "前文\n中间\n最终正文");
    }

    // ─── strip_heavy_fix_preamble Heavy fix 思维前缀剥离 单测 ───────────────

    #[test]
    fn strip_heavy_fix_preamble_removes_review_thought() {
        // 实踩泄漏模式：「好的，我需要根据审稿意见修订这段正文。让我仔细分析每条审稿意见…」
        let mut body = "好的，我需要根据审稿意见修订这段正文。让我仔细分析每条审稿意见，然后重写正文。\n\n审稿意见：\n1. **major continuity**：删除或改写“我伸手”这一动作。\n\n烛火重新亮起来的时候，她已经把背心穿回去了。我伸手，碰了碰她盘起的发髻。".to_string();
        let stripped = strip_heavy_fix_preamble(&mut body);
        assert!(stripped.contains("审稿意见"), "思维段应被剥离: {stripped}");
        assert!(stripped.contains("让我仔细分析"), "思维段应含分析前缀");
        assert!(body.starts_with("烛火重新亮起来"), "正文应从叙事段开始: {body}");
    }

    #[test]
    fn strip_heavy_fix_preamble_keeps_normal_narrative() {
        // 正常「好的，」开头但无审稿特征 → 不剥
        let mut body = "好的，她端着酒走过来，杯沿抵在我唇边。\n\n酒是温的，带着一点甜。".to_string();
        let stripped = strip_heavy_fix_preamble(&mut body);
        assert!(stripped.is_empty());
        assert!(body.starts_with("好的，她端着酒"));
    }

    #[test]
    fn strip_heavy_fix_preamble_removes_let_me_rewrite() {
        // [fix 2026-08-15] 部署后实踩 msg7「让我想想。前文是…」/ msg5「让我重写正文： --- 」：
        // 无标签思维变体（模型未按 <thinking> 输出）必须剥离。
        let mut body = "让我重写正文：\n\n---\n\n左手覆上左边的丰盈时，能感到她整个人震了一下。掌心被那分量填满。\n\n我低下头，嘴唇贴上右边。".to_string();
        let stripped = strip_heavy_fix_preamble(&mut body);
        assert!(stripped.contains("让我重写"), "思维段应被剥离: {stripped}");
        assert!(body.starts_with("左手覆上"), "正文应从叙事段开始: {body}");
    }

    #[test]
    fn strip_heavy_fix_preamble_removes_let_me_think() {
        let mut body = "让我想想。前文是「左手覆上左边的丰盈时……我低下头，嘴唇贴上右边。」所以现在应该…\n\n她整个人震了一下，我松开嘴，看着她的两个乳尖。".to_string();
        let stripped = strip_heavy_fix_preamble(&mut body);
        assert!(stripped.contains("让我想想"), "思维段应被剥离: {stripped}");
        assert!(body.starts_with("她整个人震了一下"), "正文应从叙事段开始: {body}");
    }

    #[test]
    fn strip_heavy_fix_tail_removes_self_check() {
        // [fix 2026-08-15] 部署后实踩 msg5 尾部「--- 好，这个版本可以输出。
        // 让我再检查一下角色清单：母亲、我。」：尾部自检段必须剥离。
        let mut body = "烛火在她身上晃了晃。肩头那只手始终没有移开。\n\n---\n\n好，这个版本可以输出。\n让我再检查一下角色清单：母亲、我。".to_string();
        let stripped = strip_heavy_fix_tail(&mut body);
        assert!(stripped.contains("版本可以输出"), "尾部自检应被剥离: {stripped}");
        assert!(body.contains("肩头那只手始终没有移开"), "正文保留");
        assert!(!body.contains("角色清单"), "自检段不残留: {body}");
    }

    #[test]
    fn strip_heavy_fix_tail_removes_check_needed() {
        let mut body = "她没接话，嘴唇动了动，又抿紧。\n\n再检查是否需要【检定】。玩家在说「撸管子」这种直白的话，这是言语交锋，结果比较确定，不需要掷骰。".to_string();
        let stripped = strip_heavy_fix_tail(&mut body);
        assert!(stripped.contains("再检查是否需要"), "尾部自检应被剥离: {stripped}");
        assert!(body.contains("嘴唇动了动"), "正文保留");
        assert!(!body.contains("【检定】"), "自检段不残留: {body}");
    }

    #[test]
    fn strip_heavy_fix_tail_keeps_normal_narrative() {
        // 正常叙事里含「再检查」字样但位于开头/中段 → 不剥
        let mut body = "她让我再检查一下伤口，指尖按在绷带上。\n\n烛火在她眼里晃了晃。".to_string();
        let stripped = strip_heavy_fix_tail(&mut body);
        assert!(stripped.is_empty());
        assert_eq!(body, "她让我再检查一下伤口，指尖按在绷带上。\n\n烛火在她眼里晃了晃。");
    }

    #[test]
    fn strip_heavy_fix_preamble_no_double_newline_keeps_body() {
        // 无 \n\n 边界 → 保守不剥
        let mut body = "好的，我需要根据审稿意见修订这段正文，但全部挤在一段里没有双换行。".to_string();
        let stripped = strip_heavy_fix_preamble(&mut body);
        assert!(stripped.is_empty());
        assert!(body.starts_with("好的，我需要"));
    }

    #[test]
    fn strip_heavy_fix_preamble_user_asks_variant() {
        // 实踩变体：「用户要求我作为内容负责人，根据审稿意见修订正文。让我仔细分析…」
        let mut body = "用户要求我作为内容负责人，根据审稿意见修订正文。让我仔细分析每条审稿意见，然后重写正文。\n\n审稿意见：\n1. **major continuity**：玩家输入。\n\n烛火重新亮起来的时候，她已经把背心穿回去了。".to_string();
        let stripped = strip_heavy_fix_preamble(&mut body);
        assert!(stripped.contains("用户要求我"), "思维段应被剥离: {stripped}");
        assert!(body.starts_with("烛火重新亮起来"), "正文应从叙事段开始: {body}");
    }

    #[test]
    fn strip_heavy_fix_preamble_numbered_continuation_variant() {
        // 实踩变体：多轮 fix 的续篇，从「6. **minor style**」编号开头
        let mut body = "6. **minor style**：『窗外虫鸣一声接一声』在正文中出现了两次。\n\n7. **minor pacing**：从划拳结束到母亲回房，节奏偏快。\n\n让我重新设计这段正文：\n\n烛火重新亮起来的时候，她正站在窗边。".to_string();
        let stripped = strip_heavy_fix_preamble(&mut body);
        assert!(!stripped.is_empty(), "编号续篇思维应被剥离");
        assert!(body.starts_with("烛火重新亮起来"), "正文应从叙事段开始: {body}");
    }

    #[test]
    fn strip_preamble_wo_shi_variant() {
        // 「好的，我正坐在妈妈身边…」模式（无「我是」字面，但仍是导演自白）
        let mut body = "好的，我正坐在妈妈身边，她还在假装没事。我心里却在冷笑——那个混蛋爸爸，这次我一定要让他付出代价。<场景：林逸家客厅｜傍晚｜林逸、陆清韵>\n\n你坐到妈妈身边。".to_string();
        let stripped = strip_director_preamble(&mut body);
        assert!(stripped.contains("好的，我正坐在妈妈身边"));
        assert!(stripped.contains("付出代价"));
        assert!(body.starts_with("<场景：林逸家客厅"));
        assert!(body.contains("你坐到妈妈身边"));
    }

    #[test]
    fn strip_preamble_director_plan_variant() {
        // 「好的，玩家扮演林逸…好，开始写。<场景…>」导演计划模式
        let mut body = "好的，玩家扮演林逸，十四章初中生，刚刚回到家。这是第一章，当前节点n1。我需要呈现陆清韵的犹豫。最后给玩家选项，三个出口：n2/n3/n4。好，开始写。<场景：林逸家客厅｜傍晚｜林逸、陆清韵>\n\n陆清韵听到你这句话，身子明显僵了一下。".to_string();
        let stripped = strip_director_preamble(&mut body);
        assert!(stripped.contains("好的，玩家扮演林逸"));
        assert!(stripped.contains("开始写"));
        assert!(stripped.contains("n1"));
        assert!(body.starts_with("<场景：林逸家客厅"));
        assert!(body.contains("陆清韵听到你这句话"));
    }

    #[test]
    fn strip_preamble_plain_wo_prefix_keeps_body() {
        // 以「我是」开头但无 <场景 边界 → 不剥
        let mut body = "我是林逸，天才黑客。今天放学回家，发现妈妈又哭了。".to_string();
        let stripped = strip_director_preamble(&mut body);
        assert!(stripped.is_empty());
        assert_eq!(body, "我是林逸，天才黑客。今天放学回家，发现妈妈又哭了。");
    }

    // ─── U11: resume / epoch / 成本记账 纯函数单测 ───────────────────────────

    #[test]
    fn u11_should_epoch_compress_char_threshold() {
        // 字符阈值：超 TURN_EPOCH_HARD_CHARS 触发
        let mut sess = test_session_minimal();
        sess.messages = vec![
            TavernMessage {
                id: "m1".into(),
                role: "user".into(),
                content: "啊".repeat(TURN_EPOCH_HARD_CHARS + 100),
                created_at: String::new(),
                options: vec![],
            swipes: vec![],
            swipe_index: 0,
                engine_tag: None,
                program: None,
                reasoning: None,
            tokens: 0,
            },
        ];
        let ctx = estimate_turn_ctx_chars(&sess, "sys", "u", "d");
        assert!(
            should_epoch_compress(&sess, ctx),
            "ctx_chars={ctx} 应触发 epoch 压缩"
        );
    }

    #[test]
    fn u11_should_epoch_compress_message_threshold() {
        let mut sess = test_session_minimal();
        for i in 0..TURN_EPOCH_HARD_MESSAGES {
            sess.messages.push(TavernMessage {
                id: format!("m{i}"),
                role: (if i % 2 == 0 { "user" } else { "assistant" }).into(),
                content: "短消息".into(),
                created_at: String::new(),
                options: vec![],
                engine_tag: None,
                program: None,
                reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
            });
        }
        assert!(
            should_epoch_compress(&sess, 100),
            "消息数达 TURN_EPOCH_HARD_MESSAGES 应触发"
        );
        assert!(
            !should_epoch_compress(&test_session_minimal(), 100),
            "空会话不触发"
        );
    }

    #[test]
    fn u11_refine_llm_calls_per_quality() {
        assert_eq!(refine_llm_calls(TurnQuality::Lite), 0);
        assert_eq!(refine_llm_calls(TurnQuality::Standard), 2);
        assert_eq!(refine_llm_calls(TurnQuality::Heavy), 5 + QUALITY_MAX_FIX_ROUNDS as u32 + 1);
    }

    #[test]
    fn u11_turn_cost_estimate_free_model_zero() {
        let est = turn_cost_estimate("deepseek-v4-flash-free", TurnQuality::Lite, "sys", "user", "draft", 0, false);
        assert_eq!(est.est_cost_usd, 0.0);
        assert_eq!(est.llm_calls, 1);
        assert!(est.est_in_tokens > 0, "非空输入 token 估算至少为 1");
    }

    #[test]
    fn u11_turn_cost_estimate_paid_positive() {
        let sys = "你是一个故事主持人（DM/GM）。".repeat(20);
        let user = "玩家输入：向森林深处走去，寻找传说中的古井。";
        let draft = "夜色如墨，林间小径在月光下泛着银光。".repeat(30);
        let est = turn_cost_estimate("deepseek-chat", TurnQuality::Standard, &sys, user, &draft, 1, true);
        // 主流(1) + 回退(1) + standard 审稿修订(2) + 非阻塞(1) = 5
        assert_eq!(est.llm_calls, 5);
        assert!(est.est_in_tokens > 0);
        assert!(est.est_out_tokens > 0);
        assert!(est.est_cost_usd > 0.0);
    }

    #[test]
    fn u11_accounting_json_shape() {
        let v = u11_accounting_json("m", TurnQuality::Lite, "s", "u", "d", 0, false, 1234, true, 2, Some(9), Some("boom"));
        let u = &v["u11"];
        assert_eq!(u["durationMs"], json!(1234));
        assert_eq!(u["resumed"], json!(true));
        assert_eq!(u["epoch"], json!(2));
        assert_eq!(u["turn"], json!(9));
        assert_eq!(u["error"], json!("boom"));
        assert!(u["llmCalls"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn u11_turn_over_budget_time_check() {
        // 纯函数看门狗：预算 1s，起点 5s 前 → 超时；预算 100s → 未超时
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(turn_over_budget(now_ms - 5_000, 1_000));
        assert!(!turn_over_budget(now_ms, 100_000));
    }

    // ---- U11 阈值参数化单测 (2026-08-27) ----

    #[test]
    fn u11_tuning_default_matches_consts() {
        let t = U11Tuning::default();
        assert_eq!(t.epoch_hard_chars, TURN_EPOCH_HARD_CHARS);
        assert_eq!(t.epoch_hard_messages, TURN_EPOCH_HARD_MESSAGES);
        assert_eq!(t.hard_timeout_secs, TURN_HARD_TIMEOUT_SECS);
        assert_eq!(t.max_fix_rounds, QUALITY_MAX_FIX_ROUNDS);
    }

    #[test]
    fn u11_should_epoch_compress_with_explicit_thresholds() {
        let mut sess = test_session_minimal();
        sess.messages.push(TavernMessage {
            id: "m1".into(),
            role: "user".into(),
            content: "啊啊啊啊啊啊啊啊".into(),
            created_at: String::new(),
            options: vec![],
            swipes: vec![],
            swipe_index: 0,
            engine_tag: None,
            program: None,
            reasoning: None,
            tokens: 0,
        });
        // 调低字符阈值为 5 → 触发；调高消息阈值为 999 → 仅字符侧触发
        assert!(should_epoch_compress_with(&sess, 10, 5, 999));
        // 字符未达阈值、消息 1 < 2 → 不触发
        assert!(!should_epoch_compress_with(&sess, 10, 100, 2));
    }

    // ---- S7 改进单测 (2026-08-18) ----

    #[test]
    fn s7_attach_plot_scope_with_beats() {
        let q = "玩家最近消息".to_string();
        let beats: Vec<String> = vec!["答应陪妈去三亚".into(), "约定下周二出发".into()];
        let out = s7_attach_plot_scope(q.clone(), Some(("第一章 蜜月的起始", &beats)));
        assert!(out.contains("第一章 蜜月的起始"));
        assert!(out.contains("【剧情要点】"));
        assert!(out.contains("答应陪妈去三亚"));
        assert!(out.starts_with(&q)); // 原 query 在前，叠加在后
    }

    #[test]
    fn s7_attach_plot_scope_no_beats() {
        let q = "仅最近消息".to_string();
        let out = s7_attach_plot_scope(q, Some(("节点标题", &[])));
        // 无 beats → 只加标题
        assert!(out.contains("节点标题"));
        assert!(!out.contains("【剧情要点】"));
    }

    #[test]
    fn s7_attach_plot_scope_none_node() {
        let q = "无节点".to_string();
        let out = s7_attach_plot_scope(q.clone(), None);
        assert_eq!(out, q); // 叠加为空，原样返回
    }

    #[test]
    fn s7_recall_title_key_signal_hard() {
        // 命中文本含"承诺/约定" → 硬约束语气
        let h = s7_recall_title("你答应过陪她一起去。约定不变。");
        assert!(h.contains("关键约定/承诺"), "got: {h}");
        assert!(h.contains("必须延续"));
    }

    #[test]
    fn s7_recall_title_soft_when_normal() {
        // 平凡历史回忆 → 软参考（按需沿用）
        let h = s7_recall_title("那天傍晚在公园散步，风很舒服。");
        assert!(h.contains("按需沿用"), "got: {h}");
        assert!(!h.contains("必须延续"));
    }

    // ---- §14.6 角色注入三层 纯函数单测 (2026-08-18) ----

    fn char_ref(id: &str, name: &str) -> kaleido_core::PackCharacterRef {
        kaleido_core::PackCharacterRef {
            id: id.into(),
            name: name.into(),
            role: String::new(),
            gender: String::new(),
            appearance: String::new(),
            opening_scene: String::new(),
            opening_lines: String::new(),
            nsfw_profile: "non".into(),
            importance: "high".into(),
            content_tier: None,
            example_dialogs: vec![],
            boundaries: vec![],
            personality: format!("{name}的性格描述，这一整段性格描述文字被刻意写得非常长，目的就是要稳稳超过六十个字符那么多，从而验证概要行的截断逻辑确实生效并截到六十字边界附近"),
            speech_style: "短句".into(),
            voice_profile: String::new(),
            motivation: String::new(),
            relationships: vec![],
            evidence_refs: vec![],
            mental_models: vec![],
            decision_heuristics: vec![],
            beliefs: vec![],
            expressions: std::collections::HashMap::new(),
            voice: None,
            archive: None,
            avatar: None,
            starting_wardrobe: Default::default(),
        }
    }

    #[test]
    fn character_summary_line_marks_absent_and_truncates() {
        let c = char_ref("c1", "林婉清");
        let s = character_summary_line(&c);
        assert!(s.contains("林婉清"));
        assert!(s.contains("未在场"), "got: {s}");
        // personality 截断 60 字（不喂全量）
        let pers_60 = c.personality.chars().take(60).collect::<String>();
        assert!(s.contains(&pers_60), "应含截断后的 personality: {s}");
        assert!(!s.contains(&c.personality), "不应含完整 personality（需截断）");
    }

    #[test]
    fn character_compact_card_bounded() {
        let c = char_ref("c2", "蒋闵柔");
        let out = character_compact_card(&c);
        assert!(out.contains("蒋闵柔"));
        assert!(out.chars().count() <= 300, "压缩卡应 ≤300 字, got {}", out.chars().count());
        assert!(out.chars().count() > 0);
    }

    // ---- §13.4① 玩家动作强制包装 (2026-08-18) ----

    #[test]
    fn wrap_player_action_short_action_triggered() {
        // 短动作指令 → 强制包装
        let w = wrap_player_action("与在场的人打招呼");
        assert!(w.is_some());
        let w = w.unwrap();
        assert!(w.contains("【玩家动作指令】"), "got: {w}");
        assert!(w.contains("必须让该动作真实发生"), "got: {w}");
    }

    #[test]
    fn wrap_player_action_browse_triggered() {
        let w = wrap_player_action("环顾四周，感受这个世界");
        assert!(w.is_some(), "环顾应触发");
    }

    #[test]
    fn wrap_player_action_long_narrative_untouched() {
        // 长叙述不包装（避免过度干预自由输入）
        let long = "我走进厨房，看见妈在炖排骨，蒸汽把她的刘海濡湿了，我站在门口想了一会儿该怎么开口提三亚的事";
        assert!(wrap_player_action(long).is_none(), "长叙述不应包装");
    }

    #[test]
    fn wrap_player_action_no_signal_word_untouched() {
        assert!(wrap_player_action("顺着刚才的节奏继续").is_none(), "无动作信号词不包装");
    }

    // ---- §13.5 Scene Gate 场景错位检测 (2026-08-18) ----

    #[test]
    fn scene_mismatch_hotel_detected() {
        // 首回合写成酒店/三亚 → 错位（Scene Gate 应触发纠偏）
        let t = "<场景：三亚·月见屋蜜月套房｜清晨｜小雨>\n我睁开眼……";
        assert!(is_scene_mismatch_location(t), "酒店应判错位");
    }

    #[test]
    fn scene_mismatch_airport_detected() {
        let t = "<场景：三亚凤凰国际机场｜清晨>\n飞机落地……";
        assert!(is_scene_mismatch_location(t), "机场应判错位");
    }

    #[test]
    fn scene_ok_school_allowed() {
        // 学校/家里 → 放行
        let t = "<场景：学校走廊｜午后>\n学生涌出教室……";
        assert!(!is_scene_mismatch_location(t), "学校不应判错位");
    }

    #[test]
    fn scene_ok_no_tag_allowed() {
        // 无 <场景 标签 → 不误拦
        assert!(!is_scene_mismatch_location("午后的阳光穿过教学楼旁那排老樟树……"));
    }

    // ---- §14.7.2B affinity_for_edge (2026-08-18) ----

    #[test]
    fn affinity_for_edge_exact_pair() {
        let aff = serde_json::json!({"c1:c2": 85});
        assert_eq!(affinity_for_edge(&aff, "c1", "c2"), Some(85));
    }

    #[test]
    fn affinity_for_edge_reversed_key() {
        // affinity 存反（to:from）也能命中
        let aff = serde_json::json!({"c2:c1": 60});
        assert_eq!(affinity_for_edge(&aff, "c1", "c2"), Some(60));
    }

    #[test]
    fn affinity_for_edge_player_single() {
        // 玩家(me)→角色 c2 好感存单键 c2（无 target 指向玩家自身反查）
        let aff = serde_json::json!({"c2": 92, "c3": 30});
        assert_eq!(affinity_for_edge(&aff, "me", "c2"), Some(92));
        // 反向键优先于单键（存在明确 c2:c1 时用它）
        let aff2 = serde_json::json!({"c2:c1": 60, "c1": 92});
        assert_eq!(affinity_for_edge(&aff2, "c1", "c2"), Some(60));
    }

    #[test]
    fn affinity_for_edge_miss() {
        let aff = serde_json::json!({"c1": 10});
        assert_eq!(affinity_for_edge(&aff, "c3", "c4"), None);
    }
}