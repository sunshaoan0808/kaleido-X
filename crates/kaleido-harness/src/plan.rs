//! P2 LLM planning layer.
//!
//! `plan.rs` is the interface between the harness and an LLM: it lets the
//! harness "think" of a refinement proposal ([`plan_refinement`]) and decide
//! whether auto-refining is worthwhile at a given trigger
//! ([`review_auto_refine`]). It depends only on the [`LlmClient`] abstraction
//! defined here — no kaleido-server types, no HTTP. The crate stays unit
//! testable against mock LLMs.
//!
//! Implementation note: `LlmClient::complete` is declared as a regular method
//! returning a boxed future (`Pin<Box<dyn Future + Send + '_>>`) so the trait
//! remains object-safe and we avoid pulling in the `async-trait` dependency.

use std::future::Future;
use std::pin::Pin;

use crate::model::{RefinementEvent, RefinementProposal};
use crate::{
    extract_json_object, timestamp_17, validate_edit, HarnessError, HarnessState,
};

/// The `LlmError` produced when calling the harness LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmError {
    /// The upstream call timed out.
    Timeout,
    /// The upstream provider is rate-limited; back off and retry later.
    RateLimited,
    /// An arbitrary upstream failure (message for display/logging).
    Upstream(String),
    /// The upstream returned an empty / unusable response.
    Empty,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Timeout => write!(f, "LLM timeout"),
            LlmError::RateLimited => write!(f, "LLM rate limited"),
            LlmError::Upstream(msg) => write!(f, "LLM upstream error: {msg}"),
            LlmError::Empty => write!(f, "LLM returned an empty response"),
        }
    }
}

impl std::error::Error for LlmError {}

/// The LLM abstraction `plan.rs` depends on.
///
/// Only the trait and its result type are defined in this crate; the real
/// adapter over `kaleido-server`'s `llm_provider::LLMProvider` is out of scope
/// for P2 (the caller provides a `&dyn LlmClient`).
///
/// `complete` returns a boxed future rather than being `async`, which keeps
/// the trait object-safe without the `async-trait` dependency.
pub trait LlmClient: Send + Sync {
    /// Non-streaming completion returning the full text (used to produce the
    /// JSON `RefinementProposal` / review outcome).
    fn complete(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + '_>>;
}

/// System prompt for [`plan_refinement`]. The LLM must output a single strict
/// JSON object matching `RefinementProposal`, with no fences, no surrounding
/// prose and no reasoning.
pub const REFINEMENT_SYSTEM_PROMPT: &str = concat!(
    "你是 Kaleido 自进化 harness 的精炼策划器。你的唯一输出必须是一个单一 JSON 对象，",
    "结构严格如下（RefinementProposal）：\n",
    "{ \"id\": \"refine_<17位数字时间戳>\", \"edits\": [ { \"action\": \"create|update|delete\", ",
    "\"kind\": \"prompt|memory|skill|subagent\", \"id\": \"<条目 id; update/delete 必填>\", ",
    "\"title\": \"<create/update 必填>\", \"content\": \"<create/update 必填>\", ",
    "\"path\": \"<可选>\", \"reference\": { \"type\": \"python\", \"import\": \"...\", ",
    "\"callable\": \"...\" }, \"arguments\": {...}, \"metadata\": {...}, ",
    "\"reason\": \"<为何要改，可选>\" } ], \"rationale\": \"<整体理由，可选>\", ",
    "\"rollback_of\": null }\n",
    "规则：\n- 只输出 JSON，严格单一对象；禁止输出任何 ``` / json 围栏、markdown、注释。\n",
    "- 禁止输出任何思考/推理过程，也不得输出围栏外的任何文字。\n",
    "- 不得修改 id 为 base_system_prompt 的条目。\n",
    "- skill 类型的编辑必须提供 reference（type=python 且含 import 与 callable/call_pattern）与 arguments。\n",
    "- 若当前没有值得的改进，edits 输出为空数组。"
);

/// System prompt for [`review_auto_refine`]. Strict single JSON object with a
/// `shouldRefine` boolean and optional `rationale` / `instructions`.
pub const REVIEW_SYSTEM_PROMPT: &str = concat!(
    "你是 Kaleido 自进化 harness 的评审门。根据给定的触发原因、harness 状态、精炼历史与对话尾部，",
    "判断是否值得自动发起一次精炼改进。\n",
    "唯一输出必须是一个单一 JSON 对象：\n",
    "{ \"shouldRefine\": true|false, \"rationale\": \"可选字符串或 null\", ",
    "\"instructions\": \"可选字符串或 null\" }\n",
    "- shouldRefine：布尔，是否应自动精炼。\n",
    "- rationale：给后续策划/用户的简短理由。\n",
    "- instructions：若 shouldRefine，给策划器的额外指令。\n",
    "规则：只输出 JSON，无围栏、无注释、无思考过程。"
);

/// Default max tokens for planning (align the caller with the model's
/// `maxTokens` cap before calling).
pub const PLAN_MAX_TOKENS: u32 = 32_000;
/// Default max tokens for the review gate.
pub const REVIEW_MAX_TOKENS: u32 = 2_000;
/// How many recent refinement events to feed into prompts.
pub const HISTORY_WINDOW: usize = 20;
/// Conversation-tail character budget for [`plan_refinement`].
pub const CONVERSATION_TAIL: usize = 80_000;
/// Conversation-tail character budget for [`review_auto_refine`].
pub const REVIEW_CONVERSATION_TAIL: usize = 40_000;

/// Errors returned by the planning / review layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The underlying LLM call failed.
    Llm(LlmError),
    /// The LLM output could not be extracted / parsed into the expected shape
    /// (carries a short excerpt of the upstream text for diagnosis).
    Parse(String),
    /// The (valid) proposal contained no edits.
    EmptyProposal,
    /// A proposal JSON object was structurally invalid.
    InvalidJson,
    /// One of the proposal edits failed harness validation.
    Harness(HarnessError),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Llm(e) => write!(f, "plan LLM call failed: {e}"),
            PlanError::Parse(msg) => write!(f, "plan output parse failed: {msg}"),
            PlanError::EmptyProposal => write!(f, "plan produced an empty proposal"),
            PlanError::InvalidJson => write!(f, "plan produced invalid JSON"),
            PlanError::Harness(e) => write!(f, "plan edit validation failed: {e}"),
        }
    }
}

impl std::error::Error for PlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlanError::Llm(e) => Some(e),
            PlanError::Harness(e) => Some(e),
            _ => None,
        }
    }
}

impl From<LlmError> for PlanError {
    fn from(e: LlmError) -> Self {
        PlanError::Llm(e)
    }
}

impl From<HarnessError> for PlanError {
    fn from(e: HarnessError) -> Self {
        PlanError::Harness(e)
    }
}

/// Inputs needed to produce one refinement proposal.
pub struct PlanContext<'a> {
    /// Current harness state (serialized into the user prompt).
    pub harness_state: &'a HarnessState,
    /// Recent refinement history (only the last [`HISTORY_WINDOW`] are used).
    pub refinement_history: &'a [RefinementEvent],
    /// Conversation tail (truncated to [`CONVERSATION_TAIL`] characters).
    pub conversation_tail: &'a str,
    /// Optional scope policy description.
    pub scope_policy: Option<&'a str>,
    /// Optional extra instructions for the LLM.
    pub instructions: Option<&'a str>,
    /// Formatted user-expectation anchors (active guidances) for alignment.
    pub guidance: Option<&'a str>,
}

/// Result of the auto-refine review gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOutcome {
    /// Whether auto-refining should be triggered.
    pub should_refine: bool,
    /// Optional rationale produced by the review.
    pub rationale: Option<String>,
    /// Optional instructions forwarded to the planner.
    pub instructions: Option<String>,
}

/// Inputs needed to run the review gate.
pub struct ReviewContext<'a> {
    /// What triggered this review: "turn_interval" | "compact" | "manual".
    pub trigger: &'a str,
    /// Current harness state.
    pub harness_state: &'a HarnessState,
    /// Recent refinement history.
    pub refinement_history: &'a [RefinementEvent],
    /// Conversation tail (truncated to [`REVIEW_CONVERSATION_TAIL`]).
    pub conversation_tail: &'a str,
    /// Formatted user-expectation anchors (active guidances) for alignment.
    pub guidance: Option<&'a str>,
}

/// Generate a plan (a `RefinementProposal`) from the current state, history
/// and conversation tail via the given [`LlmClient`].
///
/// The proposal id is always generated locally as `refine_<17位时间戳>`
/// (aligning with the `RefinementEvent::id` convention) and overrides whatever
/// `id` the LLM echoed back.
pub async fn plan_refinement(
    llm: &dyn LlmClient,
    ctx: &PlanContext<'_>,
    max_tokens: Option<u32>,
) -> Result<RefinementProposal, PlanError> {
    let id = format!("refine_{}", timestamp_17());
    let tokens = max_tokens.unwrap_or(PLAN_MAX_TOKENS);

    let state = serde_json::to_string_pretty(ctx.harness_state)
        .unwrap_or_else(|_| "(failed to serialize harness state)".to_string());
    let recent = recent_history(ctx.refinement_history, HISTORY_WINDOW);
    let history = serde_json::to_string_pretty(&recent)
        .unwrap_or_else(|_| "[]".to_string());
    let tail = truncate_chars(ctx.conversation_tail, CONVERSATION_TAIL);

    let mut user = String::new();
    user.push_str("<current_harness_state>\n");
    user.push_str(&state);
    user.push_str("\n</current_harness_state>\n\n");
    user.push_str("<refinement_history>\n");
    user.push_str(&history);
    user.push_str("\n</refinement_history>\n\n");
    user.push_str("<conversation>\n");
    user.push_str(&tail);
    user.push_str("\n</conversation>\n\n");
    user.push_str("<scope_policy>\n");
    user.push_str(ctx.scope_policy.unwrap_or("none"));
    user.push_str("\n</scope_policy>\n\n");
    if let Some(instructions) = ctx.instructions {
        user.push_str("[instructions]\n");
        user.push_str(instructions);
        user.push_str("\n[/instructions]\n\n");
    }
    user.push_str("<user_expectations>\n");
    user.push_str(ctx.guidance.unwrap_or("无用户期望方向。"));
    user.push_str("\n</user_expectations>\n\n");
    user.push_str(
        "[alignment] 你产出的每条 edit 都必须朝着上述「用户期望方向」对齐；请在每条 edit 的 \
         reason 里说明该 edit 如何服务这些方向。若没有用户期望方向，则基于对话与历史自行推断用户意图。",
    );
    user.push('\n');

    let resp = llm.complete(REFINEMENT_SYSTEM_PROMPT, &user, tokens).await?;

    let value = extract_json_object(&resp)
        .map_err(|e| PlanError::Parse(non_json_message(&resp, &e.to_string())))?;
    let mut proposal: RefinementProposal = serde_json::from_value(value).map_err(|e| {
        PlanError::Parse(format!(
            "LLM JSON did not match RefinementProposal ({e}); upstream text: {}",
            excerpt(&resp)
        ))
    })?;
    proposal.id = id;

    if proposal.edits.is_empty() {
        return Err(PlanError::EmptyProposal);
    }
    for edit in &proposal.edits {
        validate_edit(edit)?;
    }

    Ok(proposal)
}

/// Run the auto-refine review gate: decide whether a refinement is worth
/// planning at this trigger.
pub async fn review_auto_refine(
    llm: &dyn LlmClient,
    ctx: &ReviewContext<'_>,
) -> Result<ReviewOutcome, PlanError> {
    let state = serde_json::to_string_pretty(ctx.harness_state)
        .unwrap_or_else(|_| "(failed to serialize harness state)".to_string());
    let recent = recent_history(ctx.refinement_history, HISTORY_WINDOW);
    let history = serde_json::to_string_pretty(&recent)
        .unwrap_or_else(|_| "[]".to_string());
    let tail = truncate_chars(ctx.conversation_tail, REVIEW_CONVERSATION_TAIL);

    let mut user = String::new();
    user.push_str("<trigger>\n");
    user.push_str(ctx.trigger);
    user.push_str("\n</trigger>\n\n");
    user.push_str("<harness_state>\n");
    user.push_str(&state);
    user.push_str("\n</harness_state>\n\n");
    user.push_str("<refinement_history>\n");
    user.push_str(&history);
    user.push_str("\n</refinement_history>\n\n");
    user.push_str("<conversation>\n");
    user.push_str(&tail);
    user.push_str("\n</conversation>\n\n");
    user.push_str("<user_expectations>\n");
    user.push_str(ctx.guidance.unwrap_or("无用户期望方向。"));
    user.push_str("\n</user_expectations>\n\n");
    user.push_str(
        "[alignment] 判断「要不要改」时，须考虑上述用户期望方向：当存在指向某改良的期望时更倾向 \
         shouldRefine=true；reviews 中说明如何向着该方向收敛。",
    );

    let resp = llm
        .complete(REVIEW_SYSTEM_PROMPT, &user, REVIEW_MAX_TOKENS)
        .await?;

    let value = extract_json_object(&resp)
        .map_err(|e| PlanError::Parse(non_json_message(&resp, &e.to_string())))?;
    let obj = value
        .as_object()
        .ok_or_else(|| PlanError::Parse("review outcome is not a JSON object".to_string()))?;
    let should_refine = obj
        .get("shouldRefine")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            PlanError::Parse("review outcome is missing boolean `shouldRefine`".to_string())
        })?;
    let rationale = obj.get("rationale").and_then(|v| v.as_str()).map(str::to_string);
    let instructions = obj
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(ReviewOutcome {
        should_refine,
        rationale,
        instructions,
    })
}

/// Return the tail `window` events of a history slice (never panics on short
/// histories).
fn recent_history<'a>(history: &'a [RefinementEvent], window: usize) -> &'a [RefinementEvent] {
    let len = history.len();
    &history[len.saturating_sub(window)..]
}

/// Truncate `s` to at most `max` characters at a char boundary.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// First 500 characters of `s`, for error messages.
fn excerpt(s: &str) -> String {
    s.chars().take(500).collect()
}

/// Build the "not parseable" parse error, carrying the raw text excerpt.
fn non_json_message(raw: &str, kind: &str) -> String {
    format!(
        "LLM output was not a JSON object ({kind}); upstream text: {}",
        excerpt(raw)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RefAction, RefinementEvent, RefinementKind};
    use std::pin::Pin;

    /// A mock [`LlmClient`] returning a canned result.
    struct MockLlm {
        result: Result<String, LlmError>,
    }

    impl LlmClient for MockLlm {
        fn complete(
            &self,
            _system: &str,
            _user: &str,
            _max_tokens: u32,
        ) -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + '_>> {
            Box::pin(async move { self.result.clone() })
        }
    }

    fn empty_plan_ctx<'a>(
        state: &'a HarnessState,
        history: &'a [RefinementEvent],
    ) -> PlanContext<'a> {
        PlanContext {
            harness_state: state,
            refinement_history: history,
            conversation_tail: "",
            scope_policy: None,
            instructions: None,
            guidance: None,
        }
    }

    fn review_ctx<'a>(
        trigger: &'a str,
        state: &'a HarnessState,
        history: &'a [RefinementEvent],
    ) -> ReviewContext<'a> {
        ReviewContext {
            trigger,
            harness_state: state,
            refinement_history: history,
            conversation_tail: "",
            guidance: None,
        }
    }

    fn valid_proposal_json() -> String {
        r#"{
          "id": "refine_00000000000000000",
          "edits": [
            { "action": "update", "kind": "prompt", "id": "p_greeting", "title": "greeting", "content": "new greeting content", "reason": "more concise" },
            { "action": "create", "kind": "memory", "id": "m_note", "title": "note", "content": "remember this" }
          ],
          "rationale": "polish the greeting prompt"
        }"#
        .to_string()
    }

    #[tokio::test]
    async fn plan_valid_json_produces_proposal() {
        let llm = MockLlm {
            result: Ok(valid_proposal_json()),
        };
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = empty_plan_ctx(&state, &history);

        let p = plan_refinement(&llm, &ctx, None).await.unwrap();

        assert!(p.id.starts_with("refine_"), "id = {}", p.id);
        let stamp = &p.id["refine_".len()..];
        // `timestamp_17` pads to at least 17 digits; the bare nanos can exceed
        // 17 once the clock passes 2015, so only the lower bound is asserted.
        assert!(stamp.len() >= 17, "stamp = {stamp}");
        assert!(stamp.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(p.edits.len(), 2);
        assert_eq!(p.edits[0].action, RefAction::Update);
        assert_eq!(p.edits[0].kind, RefinementKind::Prompt);
        assert_eq!(p.edits[0].id.as_deref(), Some("p_greeting"));
        assert_eq!(p.edits[1].action, RefAction::Create);
    }

    #[tokio::test]
    async fn plan_fenced_json_is_tolerated() {
        let body = valid_proposal_json();
        let fenced = format!("Here is the plan:\n```json\n{body}\n```\nthanks");
        let llm = MockLlm { result: Ok(fenced) };
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = empty_plan_ctx(&state, &history);

        let p = plan_refinement(&llm, &ctx, Some(1000)).await.unwrap();
        assert_eq!(p.edits.len(), 2);
        assert_eq!(p.edits[0].reason.as_deref(), Some("more concise"));
    }

    #[tokio::test]
    async fn plan_garbage_or_empty_is_parse_error() {
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = empty_plan_ctx(&state, &history);

        for bad in ["", "just prose, no json here", "askjh sdjkahk asd"] {
            let llm = MockLlm {
                result: Ok(bad.to_string()),
            };
            let err = plan_refinement(&llm, &ctx, None).await.err().unwrap();
            assert!(matches!(err, PlanError::Parse(_)), "got {err:?} for {bad:?}");
        }
    }

    #[tokio::test]
    async fn plan_truncated_json_is_parse_error() {
        let truncated = r#"{"id":"x","edits":[{"action":"update","kind":"prompt","id":"p","title":"t""#;
        let llm = MockLlm {
            result: Ok(truncated.to_string()),
        };
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = empty_plan_ctx(&state, &history);

        let err = plan_refinement(&llm, &ctx, None).await.err().unwrap();
        assert!(matches!(err, PlanError::Parse(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn plan_empty_edits_is_empty_proposal() {
        let empty = r#"{"id":"x","edits":[],"rationale":"nothing to do"}"#;
        let llm = MockLlm {
            result: Ok(empty.to_string()),
        };
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = empty_plan_ctx(&state, &history);

        let err = plan_refinement(&llm, &ctx, None).await.err().unwrap();
        assert!(matches!(err, PlanError::EmptyProposal), "got {err:?}");
    }

    #[tokio::test]
    async fn review_true_with_rationale_parses() {
        let body = r#"{"shouldRefine":true,"rationale":"history is growing","instructions":"focus on prompts"}"#;
        let llm = MockLlm {
            result: Ok(body.to_string()),
        };
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = review_ctx("turn_interval", &state, &history);

        let oc = review_auto_refine(&llm, &ctx).await.unwrap();
        assert!(oc.should_refine);
        assert_eq!(oc.rationale.as_deref(), Some("history is growing"));
        assert_eq!(oc.instructions.as_deref(), Some("focus on prompts"));
    }

    #[tokio::test]
    async fn review_missing_should_refine_is_parse_error() {
        let body = r#"{"rationale":"hmm"}"#;
        let llm = MockLlm {
            result: Ok(body.to_string()),
        };
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = review_ctx("manual", &state, &history);

        let err = review_auto_refine(&llm, &ctx).await.err().unwrap();
        assert!(matches!(err, PlanError::Parse(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn llm_error_propagates_as_plan_error() {
        let llm = MockLlm {
            result: Err(LlmError::RateLimited),
        };
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = empty_plan_ctx(&state, &history);

        let err = plan_refinement(&llm, &ctx, None).await.err().unwrap();
        assert_eq!(err, PlanError::Llm(LlmError::RateLimited));

        let ctx = review_ctx("compact", &state, &history);
        let err = review_auto_refine(&llm, &ctx).await.err().unwrap();
        assert!(matches!(err, PlanError::Llm(_)), "got {err:?}");
    }

    #[test]
    fn recent_history_windows_and_never_panics() {
        let mk = || RefinementEvent {
            id: "refine_00000000000000000".into(),
            trigger: "manual".into(),
            changes: serde_json::json!({}),
            evidence: serde_json::json!({}),
            outcome: "applied".into(),
            evaluation: None,
            created_at: ".".to_string(),
        };
        let history: Vec<RefinementEvent> = (0..3).map(|_| mk()).collect();

        // Shorter than the window -> everything.
        assert_eq!(recent_history(&history, HISTORY_WINDOW).len(), 3);
        // Longer than the window -> only the tail.
        let long: Vec<RefinementEvent> = (0..(HISTORY_WINDOW + 5)).map(|_| mk()).collect();
        assert_eq!(recent_history(&long, HISTORY_WINDOW).len(), HISTORY_WINDOW);
        // Empty history -> empty slice, no panic.
        let empty: Vec<RefinementEvent> = Vec::new();
        assert!(recent_history(&empty, HISTORY_WINDOW).is_empty());
    }

    #[test]
    fn truncate_chars_respects_char_boundary() {
        assert_eq!(truncate_chars("abcde", 10), "abcde");
        assert_eq!(truncate_chars("abcde", 3), "abc");
        assert_eq!(truncate_chars("中文测试", 2), "中文");
        assert_eq!(truncate_chars("", 5), "");
    }

    /// Mock that captures the user prompt and echoes a canned result.
    struct CapturingLlm {
        current_user: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }

    impl CapturingLlm {
        fn new() -> Self {
            Self {
                current_user: std::sync::Arc::new(std::sync::Mutex::new(None)),
            }
        }
    }

    impl LlmClient for CapturingLlm {
        fn complete(
            &self,
            _system: &str,
            user: &str,
            _max_tokens: u32,
        ) -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + '_>> {
            let buf = self.current_user.clone();
            let user = user.to_string();
            Box::pin(async move {
                *buf.lock().unwrap() = Some(user);
                Ok(valid_proposal_json())
            })
        }
    }

    #[tokio::test]
    async fn guidance_is_injected_into_prompt() {
        let llm = CapturingLlm::new();
        let state = HarnessState::default();
        let history: Vec<RefinementEvent> = Vec::new();
        let ctx = PlanContext {
            harness_state: &state,
            refinement_history: &history,
            conversation_tail: "",
            scope_policy: None,
            instructions: None,
            guidance: Some("保持简练语气；prompt 输出中文"),
        };

        plan_refinement(&llm, &ctx, None).await.unwrap();
        let user = llm.current_user.lock().unwrap().clone().unwrap();
        assert!(user.contains("user_expectations"), "missing expectations block");
        assert!(
            user.contains("保持简练语气；prompt 输出中文"),
            "guidance text missing"
        );
        assert!(user.contains("这些方向"), "alignment hint missing");
    }
}