//! # Memory Weaver — Narrative Context Compaction
//!
//! Automated context window management for story sessions. Inspired by
//! Liyuan's session tree compaction system, adapted for narrative prose.
//!
//! ## Core Concepts
//!
//! - **WeaverConfig** — Thresholds for automatic compaction
//! - **WeaveSegment** — A compacted narrative segment with summary
//! - **estimate_tokens** — Token estimation for story messages
//! - **should_weave** — Trigger detection when context exceeds limit
//! - **find_cut_point** — Find optimal split point preserving recent story
//! - **prepare_weave** — Prepare compaction without LLM call
//!
//! Integrates with the 4-layer memory: compacted segments become L3 memories.

use crate::story_tavern::{EngineTag, TavernMessage};
use serde::{Deserialize, Serialize};

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for automatic narrative compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeaverConfig {
    /// Enable automatic weaving.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Max estimated context tokens before weaving triggers.
    #[serde(default = "default_max_tokens")]
    pub max_context_tokens: usize,
    /// Token budget for recent context to preserve after weaving.
    #[serde(default = "default_keep_recent")]
    pub keep_recent_tokens: usize,
    /// Token budget reserved for summarization LLM call.
    #[serde(default = "default_reserve_tokens")]
    pub reserve_tokens: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_max_tokens() -> usize {
    64000
}
fn default_keep_recent() -> usize {
    16000
}
fn default_reserve_tokens() -> usize {
    16384
}

impl Default for WeaverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_context_tokens: 64000,
            keep_recent_tokens: 16000,
            reserve_tokens: 16384,
        }
    }
}

// ─── Types ───────────────────────────────────────────────────────────────────

/// Stats about a narrative message for compaction decisions.
#[derive(Debug, Clone)]
pub struct MessageStats {
    pub index: usize,
    pub role: String,
    pub char_count: usize,
    pub estimated_tokens: usize,
    pub engine_tag: Option<EngineTag>,
}

/// A compacted narrative segment stored with the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeaveSegment {
    /// Summary text replacing the compacted messages.
    pub summary: String,
    /// Index of the first retained message (after cut).
    pub first_kept_index: usize,
    /// Estimated tokens before compaction.
    pub tokens_before: usize,
    /// Estimated tokens after compaction.
    pub tokens_after: usize,
    /// Compression ratio.
    pub compression_ratio: f64,
    /// Timestamp of compaction.
    pub created_at: String,
}

/// Result of a weave preparation.
#[derive(Debug, Clone)]
pub struct WeaveResult {
    pub segment: WeaveSegment,
    /// Messages that should be replaced by the summary.
    pub compacted_range: std::ops::Range<usize>,
    /// Messages retained (recent context).
    pub retained_range: std::ops::Range<usize>,
    /// External ledger snapshot (from LedgerStore) injected into RP summary and weave result.
    /// No LLM call — pure data layer.
    pub ledger_snapshot: Option<String>,
}

// ─── Token Estimation ───────────────────────────────────────────────────────

/// Estimate token count for a single narrative message.
///
/// Uses ~3.5 chars/token for Chinese-dominant text, ~4 for English/mixed.
pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    let ratio = if text.chars().any(|c| {
        matches!(c as u32,
            0x4E00..=0x9FFF | 0x3400..=0x4DBF |
            0x2E80..=0x2EFF | 0x3000..=0x303F |
            0xFF00..=0xFFEF
        )
    }) {
        3.5
    } else {
        4.0
    };
    (chars as f64 / ratio).ceil() as usize
}

/// Estimate token count for a `TavernMessage` including role overhead.
pub fn estimate_message_tokens(msg: &TavernMessage) -> usize {
    let content_tokens = estimate_tokens(&msg.content);
    let role_overhead = msg.role.len().saturating_mul(2);
    content_tokens + role_overhead + 4 // padding for framing
}

/// Estimate total tokens for a slice of messages.
pub fn estimate_total_tokens(messages: &[TavernMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Build per-message stats for compaction analysis.
pub fn analyze_messages(messages: &[TavernMessage]) -> Vec<MessageStats> {
    messages
        .iter()
        .enumerate()
        .map(|(i, msg)| {
            let char_count = msg.content.len();
            let estimated_tokens = estimate_message_tokens(msg);
            MessageStats {
                index: i,
                role: msg.role.clone(),
                char_count,
                estimated_tokens,
                engine_tag: msg.engine_tag.clone(),
            }
        })
        .collect()
}

// ─── Compaction Triggers ────────────────────────────────────────────────────

/// Check whether total tokens exceed the configured threshold.
pub fn should_weave(total_tokens: usize, config: &WeaverConfig) -> bool {
    if !config.enabled {
        return false;
    }
    total_tokens > config.max_context_tokens
}

/// Find the optimal cut point for weaving.
///
/// Scans from the end to find a point that keeps approximately
/// `keep_recent_tokens` worth of recent context, preferring
/// natural turn boundaries (user messages).
pub fn find_cut_point(stats: &[MessageStats], keep_recent_tokens: usize) -> Option<usize> {
    if stats.len() < 4 {
        return None; // Too few messages to compact
    }

    let total = stats.len();
    let mut accumulated = 0usize;

    // Walk backwards from the end
    for i in (0..total).rev() {
        accumulated = accumulated.saturating_add(stats[i].estimated_tokens);
        if accumulated >= keep_recent_tokens {
            // Try to cut at a user-message boundary (turn boundary)
            let candidate = i.saturating_sub(2);
            for j in candidate..total {
                if stats[j].role == "user" || stats[j].role == "system" {
                    // Cut just before this turn (keep the user message)
                    return Some(j.saturating_sub(1));
                }
            }
            return Some(candidate);
        }
    }

    None
}

/// Prepare a session for weaving.
///
/// Returns `None` if compaction is not needed or not possible.
pub fn prepare_weave(messages: &[TavernMessage], config: &WeaverConfig) -> Option<WeaveResult> {
    if messages.is_empty() {
        return None;
    }

    let total_tokens = estimate_total_tokens(messages);

    if !should_weave(total_tokens, config) {
        return None;
    }

    let stats = analyze_messages(messages);
    let cut_index = find_cut_point(&stats, config.keep_recent_tokens)?;

    // Ensure we have messages to compact
    if cut_index < 2 {
        return None;
    }

    let compacted_range = 0..cut_index;
    let retained_range = cut_index..messages.len();

    let _compacted_tokens: usize = stats[..cut_index]
        .iter()
        .map(|s| s.estimated_tokens)
        .sum();
    let retained_tokens: usize = stats[cut_index..]
        .iter()
        .map(|s| s.estimated_tokens)
        .sum();

    let segment = WeaveSegment {
        summary: String::new(), // Filled in by external LLM call
        first_kept_index: cut_index,
        tokens_before: total_tokens,
        tokens_after: retained_tokens + 100, // Rough overhead for summary
        compression_ratio: if retained_tokens > 0 && total_tokens > 0 {
            format!("{:.2}", total_tokens as f64 / retained_tokens as f64)
                .parse()
                .unwrap_or(1.0)
        } else {
            1.0
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    Some(WeaveResult {
        segment,
        compacted_range,
        retained_range,
        ledger_snapshot: None,
    })
}

// ─── U13 M3: Parameterized Compaction Triggers (absorbed from Liyuan) ────────

/// Settings for parameterized compaction decisions, absorbed from Liyuan
/// compaction.ts `CompactionSettings` + additional granularity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    /// Enable automatic compaction decisions.
    pub enabled: bool,
    /// Context-window token capacity for the current model.
    pub context_window: usize,
    /// Threshold ratio (0.0–1.0) of context_window to trigger compaction.
    pub threshold: f64,
    /// Minimum token chunk size to consider for compaction.
    pub min_chunk_size: usize,
    /// Summarize age: minimum turns since last compaction before re-compacting.
    pub summarize_age: u32,
    /// Tokens reserved for summary prompt and output.
    pub reserve_tokens: usize,
    /// Approximate recent-context tokens to keep after compaction.
    pub keep_recent_tokens: usize,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            context_window: 128_000,
            threshold: 0.8,
            min_chunk_size: 2000,
            summarize_age: 3,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

/// Determine whether context compaction should be triggered.
/// Absorbed from Liyuan `shouldCompact(contextTokens, contextWindow, settings)`.
/// Parameters:
/// - `context_tokens`: estimated tokens in current context
/// - `context_window`: model's context window capacity
/// - `settings`: compaction settings with threshold, minChunkSize, summarizeAge
/// - `turns_since_last_compaction`: turns elapsed since last compaction
pub fn should_compact(
    context_tokens: usize,
    context_window: usize,
    settings: &CompactionSettings,
    turns_since_last_compaction: u32,
) -> bool {
    if !settings.enabled {
        return false;
    }
    // Enforce summarize age: don't re-compact too frequently
    if turns_since_last_compaction < settings.summarize_age {
        return false;
    }
    let threshold_tokens = (context_window as f64 * settings.threshold) as usize;
    context_tokens > threshold_tokens && context_tokens > settings.min_chunk_size
}

/// Find the turn-boundary-aware cut point for compaction.
/// Absorbed from Liyuan `findTurnStartIndex(entries, entryIndex, startIndex)`.
/// Finds the nearest turn boundary (user message) that doesn't break a
/// dual-agent turn. Returns the index of the first message to keep (retained range start).
///
/// - `messages`: the full message list
/// - `target_index`: ideal cut index (from token estimation)
/// - `min_keep`: minimum messages to keep from the end
pub fn find_turn_start_index(
    messages: &[TavernMessage],
    target_index: usize,
    min_keep: usize,
) -> usize {
    if messages.len() <= min_keep {
        return 0; // Nothing to cut
    }
    let min_keep = min_keep.max(2); // Always keep at least 2 messages
    let target_index = target_index.max(min_keep).min(messages.len().saturating_sub(min_keep));

    // Walk backwards from target_index looking for a turn boundary
    // (user message). This ensures we don't break mid-turn.
    let mut best = target_index;
    for i in (min_keep..=target_index).rev() {
        if i < messages.len() && messages[i].role == "user" {
            // Cut just before this user message (keep it as start of retained context)
            best = i;
            break;
        }
    }
    // Ensure we don't cut too aggressively
    best.max(min_keep)
}

/// Calculate context tokens from estimated message sizes.
/// Absorbed from Liyuan `estimateContextTokens(messages)`.
pub fn estimate_context_tokens(messages: &[TavernMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

// ─── Summarization Prompt Templates ─────────────────────────────────────────

/// System prompt for narrative context summarization.
pub const WEAVE_SYSTEM_PROMPT: &str =
    "You are a narrative context summarizer. Read the story chat and produce \
     a structured summary that preserves character states, plot developments, \
     and key decisions. Do NOT continue the story.";

/// User prompt template for narrative context summarization.
pub const WEAVE_USER_PROMPT: &str =
    "The messages above are a story session to summarize. Create a structured \
     context checkpoint that another AI will use to continue the narrative.

## Active Characters
- [Characters currently present and their state]

## Current Scene
- [Location, time, atmosphere]

## Plot Threads
- [Active plot lines and their status]

## Key Developments
- [Important events, discoveries, decisions]

## Open Questions
- [Unresolved plot threads, mysteries]

## Important Details
- [Specific facts the AI must remember to maintain consistency]

Keep each section concise (2-4 bullet points). Preserve character names exactly as used.";

/// Build the conversation text from a slice of messages for summarization.
pub fn build_conversation_text(messages: &[TavernMessage]) -> String {
    messages
        .iter()
        .map(|msg| {
            let role = &msg.role;
            let tag_str = msg
                .engine_tag
                .as_ref()
                .map(|t| format!(" [{}]", format!("{:?}", t)))
                .unwrap_or_default();
            format!("[{}]{}:\n{}", role, tag_str, msg.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

// ─── Liyuan-absorbed: RP 场记摘要（叙事过滤 + 结构化前情） ─────────────────────

/// 把会话消息序列化为摘要输入文本：只保留叙事正文（user/assistant），
/// 空消息与非叙事角色（system/tool/custom 等）一律不进入摘要输入。
/// 吸收自 Liyuan `serializeForSummary`（过程性内容在代码层被确定性剔除）。
/// 摘要序列化的分类省略统计（吸收自 OpenHanako lossy-local-compaction 的
/// OmissionCounts 设计）：区分工具块/推理/空消息/其他角色，审计「压缩丢了什么」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryOmissionStats {
    /// 跳过：程序卡/工具块（TavernMessage.program）
    pub skipped_program: usize,
    /// 跳过：推理/导演思考（TavernMessage.reasoning）
    pub skipped_reasoning: usize,
    /// 跳过：正文为空的消息
    pub skipped_empty: usize,
    /// 跳过：非 user/assistant 角色消息
    pub skipped_other: usize,
    /// 保留进摘要的消息数
    pub kept: usize,
}

/// serialize_for_summary 的统计版本：返回 (文本, 分类省略统计)。
pub fn serialize_for_summary_with_stats(
    messages: &[TavernMessage],
    user_label: &str,
    char_label: &str,
) -> (String, SummaryOmissionStats) {
    let mut lines: Vec<String> = Vec::new();
    let mut stats = SummaryOmissionStats::default();
    for m in messages {
        let text = m.content.trim();
        if text.is_empty() {
            if m.program.is_some() {
                stats.skipped_program += 1;
            } else if m.reasoning.is_some() {
                stats.skipped_reasoning += 1;
            } else {
                stats.skipped_empty += 1;
            }
            continue;
        }
        let label = match m.role.as_str() {
            "user" => user_label,
            "assistant" => char_label,
            _ => {
                stats.skipped_other += 1;
                continue;
            }
        };
        stats.kept += 1;
        lines.push(format!("{}：{}", label, text));
    }
    (lines.join("\n\n"), stats)
}

/// serialize_for_summary 兼容旧签名：丢弃统计（既有调用方零改动）。
pub fn serialize_for_summary(
    messages: &[TavernMessage],
    user_label: &str,
    char_label: &str,
) -> String {
    serialize_for_summary_with_stats(messages, user_label, char_label).0
}

/// 场记式接力摘要系统提示词（吸收自 Liyuan compaction.ts buildRpSummaryPrompt）。
/// 结构：前情提要（剧内时间刻度）→ 人物（关系温度演变）→ 承诺与伏笔（宁多勿漏）
/// → 事实账 → 当前场景（必须以最新为准，防剧情倒退）。
pub const RP_SUMMARY_SYSTEM_PROMPT: &str = "你是一场长篇角色扮演的场记。你的任务是为即将从上下文中裁掉的早期剧情写一份接力摘要——它将成为主演模型唯一能看到的「前情」，后续剧情将基于「本摘要 + 保留的最近对话」继续演出。

用中文输出，按以下结构：

## 前情提要
按时间顺序概述关键事件（谁做了什么、结果如何）。保留剧内时间刻度（如「第一天黄昏」「第三天清晨」）。

## 人物
每位出场人物：性格要点、说话习惯、与玩家的称呼、与玩家的关系温度及演变轨迹。

## 承诺与伏笔
逐条列出所有未兑现的约定、只被提过一次的线索、悬而未决的问题。这一节宁多勿漏——漏掉一条，后续剧情就永远丢失它。

## 事实账
物品归属（谁持有什么）、伤势与身体状态、重要数值、时间线（现在是剧内第几天）。
[P1B 2026-08-16 着装契约] **着装状态必须逐位记录**：每位出场角色当前穿着什么（上衣/下装/鞋袜等），以及剧情中**已脱掉/脱下的衣物去向**（在谁手里/扔在哪/是否穿回）。任何脱衣/穿衣/弄脏/换装事件都不得遗漏——宁可多写，不可省略。

## 当前场景
剧内此刻：第几天、什么时段、什么地点、谁在场、正在进行什么动作。必须以对话记录中**最新**的场景为准——这是续演点，写成更早的场景会导致剧情倒退。若此刻有角色着装与上文不同（已脱/已穿回），在此处复述其当前着装状态。

规则：只记录对话中实际发生的事；不虚构、不评论、不续写剧情；人名地名保持剧中写法。";

/// 构造场记摘要的 user 文本：叙事正文 +（可选）既有摘要合并 + 状态快照。
pub fn build_rp_summary_user_text(
    conversation_text: &str,
    state_snapshot: &str,
    previous_summary: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("<conversation>\n{}\n</conversation>", conversation_text));
    if let Some(prev) = previous_summary {
        if !prev.trim().is_empty() {
            parts.push(format!(
                "<previous-summary>\n{}\n</previous-summary>\n\n（上面是更早剧情的既有摘要：把它的内容合并进本次摘要，不要丢弃其中的承诺、伏笔与事实。）",
                prev
            ));
        }
    }
    if !state_snapshot.trim().is_empty() {
        parts.push(format!(
            "【工具账本快照】（辅助参考；记账可能滞后于正文，与对话记录冲突时以对话记录为准）\n{}",
            state_snapshot
        ));
    }
    parts.push("请按系统指令输出接力摘要。".into());
    parts.join("\n\n")
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story_tavern::TavernMessage;

    fn msg(role: &str, text: &str, tag: Option<EngineTag>) -> TavernMessage {
        TavernMessage {
            id: "test-id".into(),
            role: role.to_string(),
            content: text.to_string(),
            created_at: "2026-01-01T00:00:00Z".into(),
            options: vec![],
            swipes: vec![],
            swipe_index: 0,
            engine_tag: tag,
            program: None,
            reasoning: None,
            tokens: 0,
        }
    }

    #[test]
    fn test_estimate_tokens_cjk() {
        let tokens = estimate_tokens("你好，今天天气真不错。");
        // 12 chars / 3.5 ≈ 4
        assert!(tokens > 0 && tokens <= 6);
    }

    #[test]
    fn test_estimate_tokens_english() {
        let tokens = estimate_tokens("Hello, how are you today?");
        // 25 chars / 4 ≈ 7
        assert!(tokens > 0 && tokens <= 10);
    }

    #[test]
    fn test_estimate_message_tokens_with_role() {
        let m = msg("assistant", "Hello world", None);
        let tokens = estimate_message_tokens(&m);
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_total_tokens() {
        let msgs = vec![msg("user", "Hi", None), msg("assistant", "Hello!", None)];
        let total = estimate_total_tokens(&msgs);
        assert!(total > 0);
    }

    #[test]
    fn test_should_weave_trigger() {
        let config = WeaverConfig {
            max_context_tokens: 100,
            ..Default::default()
        };
        assert!(should_weave(150, &config));
        assert!(!should_weave(50, &config));
        assert!(!should_weave(100, &config));
    }

    #[test]
    fn test_should_weave_disabled() {
        let config = WeaverConfig {
            enabled: false,
            max_context_tokens: 100,
            ..Default::default()
        };
        assert!(!should_weave(999, &config));
    }

    #[test]
    fn test_find_cut_point_returns_none_for_short() {
        let stats = vec![
            MessageStats {
                index: 0,
                role: "user".into(),
                char_count: 50,
                estimated_tokens: 15,
                engine_tag: None,
            },
            MessageStats {
                index: 1,
                role: "assistant".into(),
                char_count: 100,
                estimated_tokens: 25,
                engine_tag: None,
            },
        ];
        assert!(find_cut_point(&stats, 1000).is_none());
    }

    #[test]
    fn test_analyze_messages() {
        let msgs = vec![
            msg("user", "Hello", None),
            msg("assistant", "Hi there!", None),
        ];
        let stats = analyze_messages(&msgs);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].role, "user");
        assert_eq!(stats[1].role, "assistant");
    }

    #[test]
    fn test_prepare_weave_noop_when_small() {
        let msgs = vec![
            msg("user", "Start", Some(EngineTag::Canon)),
            msg("assistant", "Story begins", Some(EngineTag::Canon)),
        ];
        let config = WeaverConfig::default();
        let result = prepare_weave(&msgs, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_prepare_weave_with_large_context() {
        // Generate enough messages to exceed max_context_tokens
        let mut msgs = Vec::new();
        for i in 0..20 {
            msgs.push(msg(
                if i % 2 == 0 { "user" } else { "assistant" },
                &"A".repeat(10000), // ~2500 tokens each
                Some(EngineTag::Canon),
            ));
        }
        let config = WeaverConfig {
            max_context_tokens: 5000,
            keep_recent_tokens: 10000,
            ..Default::default()
        };
        let result = prepare_weave(&msgs, &config);
        assert!(result.is_some());
        let wr = result.unwrap();
        assert!(wr.compacted_range.start < wr.compacted_range.end);
        assert!(wr.retained_range.start < wr.retained_range.end);
        assert_eq!(wr.compacted_range.end, wr.retained_range.start);
    }

    #[test]
    fn test_build_conversation_text() {
        let msgs = vec![
            msg("user", "Tell me a story", None),
            msg("assistant", "Once upon a time...", Some(EngineTag::Canon)),
        ];
        let text = build_conversation_text(&msgs);
        assert!(text.contains("[user]:"));
        assert!(text.contains("[assistant]"));
        assert!(text.contains("[Canon]"));
        assert!(text.contains("Once upon a time"));
    }

    #[test]
    fn test_weave_config_default() {
        let c = WeaverConfig::default();
        assert!(c.enabled);
        assert_eq!(c.max_context_tokens, 64000);
        assert_eq!(c.keep_recent_tokens, 16000);
    }

    // ── Liyuan 吸收测例（移植自 compaction.test.ts） ─────────────────────────

    #[test]
    fn test_serialize_for_summary_keeps_narrative_only() {
        let msgs = vec![
            // Kaleido 开场白以 assistant 角色落库（seed_opening_if_needed）
            msg("assistant", "【开场】*她守在你身边。*", None),
            msg("user", "我睁开眼。", None),
            msg(
                "assistant",
                "*她转过身来。*「你醒了。」",
                Some(EngineTag::Canon),
            ),
            msg("tool", "lorebook_search Gloomhound", None), // 非叙事角色应被剔除
            msg("system", "system prompt noise", None),
            msg("user", "", None), // 空消息应被剔除
        ];
        let text = serialize_for_summary(&msgs, "阿远", "青梧");
        assert!(text.contains("阿远：我睁开眼。"));
        assert!(text.contains("青梧：*她转过身来。*「你醒了。」"));
        assert!(text.contains("【开场】"));
        assert!(!text.contains("lorebook_search"), "工具调用不应进入摘要输入");
        assert!(!text.contains("system prompt"), "系统消息不应进入摘要输入");
        assert!(!text.contains("阿远：："), "空消息不应产生残行");
    }

    #[test]
    fn test_serialize_for_summary_labels() {
        let msgs = vec![
            msg("user", "你好", None),
            msg("assistant", "你好呀", None),
            msg("user", "继续", None),
        ];
        let text = serialize_for_summary(&msgs, "玩家", "旁白");
        assert_eq!(text, "玩家：你好\n\n旁白：你好呀\n\n玩家：继续");
    }

    #[test]
    fn test_rp_summary_prompt_structure() {
        // 系统提示词必须包含场记五段结构 + 当前场景最新守卫
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("## 前情提要"));
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("## 人物"));
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("## 承诺与伏笔"));
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("宁多勿漏"));
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("## 事实账"));
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("## 当前场景"));
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("最新"), "当前场景必须以最新为准");
        assert!(RP_SUMMARY_SYSTEM_PROMPT.contains("剧情倒退"));
    }

    #[test]
    fn test_rp_summary_user_text_merge_and_snapshot() {
        let user = build_rp_summary_user_text(
            "阿远：我睁开眼。\n\n青梧：*她转过身来。*",
            "时间：第三天清晨\n物品：黄铜怀表（阿远持有）",
            Some("第一天：相遇，约定明日同行。"),
        );
        assert!(user.contains("<conversation>"));
        assert!(user.contains("<previous-summary>"));
        assert!(user.contains("第一天：相遇，约定明日同行。"));
        assert!(user.contains("不要丢弃其中的承诺、伏笔与事实"));
        assert!(user.contains("黄铜怀表"));
        assert!(user.contains("请按系统指令输出接力摘要。"));
        // 无既有摘要时不输出 previous-summary 块
        let user2 = build_rp_summary_user_text("正文", "快照", None);
        assert!(!user2.contains("<previous-summary>"));
        assert!(user2.contains("<conversation>"));
    }

    // ── U13 M3: should_compact / find_turn_start_index tests ────────────────

    #[test]
    fn test_should_compact_below_threshold() {
        let settings = CompactionSettings::default();
        // 50k tokens < 128k * 0.8 = 102.4k threshold
        assert!(!should_compact(50_000, 128_000, &settings, 5));
    }

    #[test]
    fn test_should_compact_above_threshold() {
        let settings = CompactionSettings::default();
        // 110k tokens > 128k * 0.8 = 102.4k threshold
        assert!(should_compact(110_000, 128_000, &settings, 5));
    }

    #[test]
    fn test_should_compact_respects_summarize_age() {
        let settings = CompactionSettings {
            summarize_age: 5,
            ..Default::default()
        };
        // Above threshold but only 2 turns since last compaction (need >= 5)
        assert!(!should_compact(110_000, 128_000, &settings, 2));
        assert!(should_compact(110_000, 128_000, &settings, 5));
    }

    #[test]
    fn test_should_compact_disabled() {
        let settings = CompactionSettings {
            enabled: false,
            ..Default::default()
        };
        assert!(!should_compact(200_000, 128_000, &settings, 10));
    }

    #[test]
    fn test_should_compact_min_chunk_size() {
        let settings = CompactionSettings {
            min_chunk_size: 20000,
            threshold: 0.1,
            ..Default::default()
        };
        // threshold = 128k * 0.1 = 12.8k
        // 低于 threshold（12.8k）且低于 min_chunk_size（20k）→ false
        assert!(!should_compact(3_000, 128_000, &settings, 10));
        // 高于 threshold（12.8k）但低于 min_chunk_size（20k）→ 仍 false（min_chunk_size 拦截）
        assert!(!should_compact(15_000, 128_000, &settings, 10));
        // 同时高于 threshold 和 min_chunk_size → true
        assert!(should_compact(25_000, 128_000, &settings, 10));
    }

    #[test]
    fn test_find_turn_start_index_basic() {
        let msgs = vec![
            msg("user", "Hi", None),
            msg("assistant", "Hello", None),
            msg("user", "How are you?", None),
            msg("assistant", "Good", None),
            msg("user", "Let's go", None),
            msg("assistant", "Sure", None),
        ];
        // Target index 4, should find user message at index 4
        let cut = find_turn_start_index(&msgs, 4, 2);
        assert_eq!(cut, 4, "Should cut at user message boundary");
    }

    #[test]
    fn test_find_turn_start_index_fallback() {
        let msgs = vec![
            msg("assistant", "First", None),
            msg("assistant", "Second", None),
            msg("assistant", "Third", None),
            msg("assistant", "Fourth", None),
        ];
        // No user messages, should still return a valid index
        let cut = find_turn_start_index(&msgs, 2, 2);
        assert!(cut >= 2, "Should respect min_keep");
    }

    #[test]
    fn test_find_turn_start_index_short_list() {
        let msgs = vec![
            msg("user", "Hi", None),
            msg("assistant", "Hello", None),
        ];
        let cut = find_turn_start_index(&msgs, 1, 2);
        assert_eq!(cut, 0, "Too few messages to cut");
    }

    #[test]
    fn test_estimate_context_tokens_basic() {
        let msgs = vec![
            msg("user", "Hello world", None),
            msg("assistant", "Hi there!", None),
        ];
        let tokens = estimate_context_tokens(&msgs);
        assert!(tokens > 0, "Should estimate some tokens");
    }

    #[test]
    fn test_compaction_settings_default() {
        let s = CompactionSettings::default();
        assert!(s.enabled);
        assert_eq!(s.context_window, 128_000);
        assert!((s.threshold - 0.8).abs() < f64::EPSILON);
        assert_eq!(s.min_chunk_size, 2000);
        assert_eq!(s.summarize_age, 3);
    }
}
