//! Harness P3 接线桥：把 `kaleido_harness` 的纯核心接到 kaleido-server 运行时。
//!
//! 职责：
//! - [`LlmClientImpl`]：把 `kaleido_harness::plan::LlmClient` 适配到
//!   `crate::llm_provider::LLMProvider`（复用现有 provider，不新造 LLM 客户端）。
//! - [`run_refine`]：plan(LLM) → apply(纯内存) 闭环；apply 前从
//!   `data_root/harness/harness_state.json` 重读，正确处理共享文件并发写。
//! - [`auto_refine_gate`]：auto-refine 触发前的评审门（LLM 判 should_refine）。
//!
//! 数据落 `data_root/harness/`（与 `kaleido_harness::store` 的布局一致）。
//! LLM 错误一律映射为 `LlmError::Upstream(String)`，不 panic。

use std::future::Future;
use std::pin::Pin;
use std::path::{Path, PathBuf};

use kaleido_harness::plan::{
    plan_refinement, review_auto_refine, LlmClient, LlmError, PlanContext, ReviewContext,
    HISTORY_WINDOW,
};
use kaleido_harness::{
    apply_refinement_proposal, load as hload, save as hsave, timestamp_17,
    ApplyResult, Guidance, HarnessState, RefinementEvent, RefinementProposal,
};

use crate::llm_provider::{
    create_provider, ChatMessage, ChatRequest, LLMProvider, ProviderConfig, ProviderKind,
};

/// `LLM_BASE_URL` / `LLM_API_KEY` / `LLM_MODEL` / `KALEIDO_LLM_PROVIDER` 与
/// `main.rs` 构建 `AppState` 时的一致。
const ENV_BASE_URL: &str = "LLM_BASE_URL";
const ENV_API_KEY: &str = "LLM_API_KEY";
const ENV_MODEL: &str = "LLM_MODEL";
const ENV_PROVIDER: &str = "KALEIDO_LLM_PROVIDER";
const DEFAULT_MODEL: &str = "deepseek-v4-flash-free";
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// 解析 provider 类型字符串（大小写不敏感；默认 OpenAI）。
/// 与 `llm_stream::parse_provider` 语义一致，此处保留本地副本避免改动旧模块。
pub fn parse_provider(kind: &str) -> ProviderKind {
    match kind.trim().to_lowercase().as_str() {
        "anthropic" => ProviderKind::Anthropic,
        "google" | "gemini" => ProviderKind::Google,
        _ => ProviderKind::OpenAI,
    }
}

/// `kaleido_harness::plan::LlmClient` 的 server 侧实现：持有 `ProviderConfig`，
/// `complete()` 内构造非流式 `ChatRequest` 调 `provider.chat()`。
#[derive(Debug, Clone)]
pub struct LlmClientImpl {
    config: ProviderConfig,
}

impl LlmClientImpl {
    /// 从环境变量读取 provider 配置。base_url/api_key 缺失时返回 None
    /// （调用方按「provider 未配置」处理）。
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var(ENV_BASE_URL).unwrap_or_default();
        let api_key = std::env::var(ENV_API_KEY).unwrap_or_default();
        if base_url.trim().is_empty() || api_key.trim().is_empty() {
            return None;
        }
        let model = std::env::var(ENV_MODEL).unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let kind = parse_provider(&std::env::var(ENV_PROVIDER).unwrap_or_default());
        Some(Self {
            config: ProviderConfig {
                kind,
                base_url,
                api_key,
                model,
                timeout_secs: DEFAULT_TIMEOUT_SECS,
            },
        })
    }
}

impl LlmClient for LlmClientImpl {
    fn complete(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + '_>> {
        let config = self.config.clone();
        // 先把借用转成自有 String，避免 async move 捕获多个 &'a str 的 lifetime 纠缠。
        let system = system.to_string();
        let user = user.to_string();
        Box::pin(async move {
            let provider = create_provider(&config);
            let req = ChatRequest {
                model: config.model.clone(),
                messages: vec![
                    ChatMessage {
                        role: "system".to_string(),
                        content: system,
                    },
                    ChatMessage {
                        role: "user".to_string(),
                        content: user,
                    },
                ],
                temperature: 0.2,
                max_tokens,
                timeout_secs: config.timeout_secs,
            };
            provider.chat(&req).await.map_err(LlmError::Upstream)
        })
    }
}

/// harness 数据目录 → `data_root/harness`。
pub fn state_dir(data_root: &Path) -> PathBuf {
    data_root.join(kaleido_harness::store::HARNESS_DIR)
}

/// 加载 harness 状态（容错：缺失/损坏 → 默认态，绝不 panic）。
pub fn load_state(data_root: &Path) -> HarnessState {
    hload(&state_dir(data_root))
}

/// 保存 harness 状态（原子写；忽略返回路径）。
pub fn save_state(data_root: &Path, state: &HarnessState) -> std::io::Result<()> {
    hsave(&state_dir(data_root), state).map(|_| ())
}

/// 取最近 `HISTORY_WINDOW` 条精炼历史（短于窗口则全量，空历史安全）。
fn recent_history(state: &HarnessState) -> &[RefinementEvent] {
    let len = state.refinements.len();
    &state.refinements[len.saturating_sub(HISTORY_WINDOW)..]
}

/// 把 apply 后的状态重建出来。
///
/// `apply_refinement_proposal` 内部克隆 `before`、就地改那份克隆，且**不返回**
/// 变更后的 state（只返回 `ApplyResult`）。因此这里按 `AppliedEdit` 逐条把
/// `after`/`before` 重放回 `before` 的克隆，并复刻 apply.rs 的 RefinementEvent
/// 追加逻辑，得到与 apply 内部完全一致的后置状态，供 `save_state` 落盘。
fn reconstruct_applied_state(
    before: &HarnessState,
    proposal: &RefinementProposal,
    result: &ApplyResult,
) -> HarnessState {
    let mut out = before.clone();

    for ae in &result.applied_edits {
        if !ae.applied {
            continue;
        }
        match (&ae.before, &ae.after) {
            // create / update：写入 after 条目。
            (_, Some(after)) => {
                let kind_map = out.entries.entry(after.kind.to_string()).or_default();
                kind_map.insert(after.id.clone(), after.clone());
            }
            // delete：按 before 里的 kind/id 从 map 移除。
            (Some(before_entry), None) => {
                if let Some(kind_map) = out.entries.get_mut(&before_entry.kind.to_string()) {
                    kind_map.remove(&before_entry.id);
                }
            }
            // 防御：既无 before 也无 after（理论上不出现的空条目），跳过。
            (None, None) => {}
        }
    }

    if result.success_count() > 0 {
        let trigger = if proposal.rollback_of.is_some() {
            "rollback".to_string()
        } else {
            "summary".to_string()
        };
        let changes = serde_json::to_value(&proposal.edits).unwrap_or(serde_json::Value::Null);
        let evidence = proposal
            .rationale
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null);
        let event = RefinementEvent {
            id: proposal.id.clone(),
            trigger,
            changes,
            evidence,
            outcome: "applied".to_string(),
            evaluation: None,
            created_at: timestamp_17(),
        };
        out.refinements.push(event);
    }

    out
}

/// 核心闭环：plan(LLM) → apply(纯内存)。返回到位的 `ApplyResult`。
///
/// 并发安全要点：apply 前从盘上**重读** `harness_state.json` 作为 `before`，
/// 第一次读取的 `state` 作为冲突检测 `baseline`；apply 成功(≥1)后把 apply 后
/// 的状态写回。任何 LLM/解析错误以 `Err(String)` 返回，不 panic。
pub async fn run_refine(
    data_root: &Path,
    llm: &dyn LlmClient,
    conversation_tail: &str,
    instructions: Option<&str>,
    scope_policy: Option<&str>,
) -> Result<ApplyResult, String> {
    let st = load_state(data_root);
    let guidance = guidance_summary(&st);

    let ctx = PlanContext {
        harness_state: &st,
        refinement_history: recent_history(&st),
        conversation_tail,
        scope_policy,
        instructions,
        guidance: if guidance.is_empty() { None } else { Some(&guidance) },
    };
    let proposal = plan_refinement(llm, &ctx, None)
        .await
        .map_err(|e| e.to_string())?;

    // apply 前重读（并发安全）→ before = fresh，baseline = 第一次读的 state。
    let fresh = load_state(data_root);
    let result = apply_refinement_proposal(&fresh, &proposal, Some(&st));

    if result.applied_edits.iter().any(|e| e.applied) {
        let applied_state = reconstruct_applied_state(&fresh, &proposal, &result);
        save_state(data_root, &applied_state).map_err(|e| e.to_string())?;
    }

    Ok(result)
}

/// 纯 apply 并持久化：给 `/api/v1/harness/apply` 及调试用。
/// 直接 apply 一个完整 proposal（不走 LLM），成功(≥1)则重建并写回 post-apply 状态。
pub async fn apply_proposal_persist(
    data_root: &Path,
    proposal: &RefinementProposal,
) -> Result<ApplyResult, String> {
    let fresh = load_state(data_root);
    let result = apply_refinement_proposal(&fresh, proposal, None);
    if result.applied_edits.iter().any(|e| e.applied) {
        let applied_state = reconstruct_applied_state(&fresh, proposal, &result);
        save_state(data_root, &applied_state).map_err(|e| e.to_string())?;
    }
    Ok(result)
}

/// auto-refine 评审门：LLM 判断该 trigger 下是否值得自动精炼。
pub async fn auto_refine_gate(
    data_root: &Path,
    llm: &dyn LlmClient,
    trigger: &str,
    conversation_tail: &str,
) -> Result<bool, String> {
    let st = load_state(data_root);
    let guidance = guidance_summary(&st);
    let ctx = ReviewContext {
        trigger,
        harness_state: &st,
        refinement_history: recent_history(&st),
        conversation_tail,
        guidance: if guidance.is_empty() { None } else { Some(&guidance) },
    };
    let outcome = review_auto_refine(llm, &ctx).await.map_err(|e| e.to_string())?;
    Ok(outcome.should_refine)
}

// ── P4：Guidance 管理 + 需求讨论 ────────────────────────────────────────

/// 把 state 里 active 的 guidances 格式化成给 LLM 的文本块；无则返回空串。
pub fn guidance_summary(state: &HarnessState) -> String {
    let active: Vec<&Guidance> = state.guidances.iter().filter(|g| g.active).collect();
    if active.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (i, g) in active.iter().enumerate() {
        out.push_str(&format!("{}. [{}] {}：{}\n", i + 1, g.source, g.title, g.description));
    }
    out
}

/// 列出全部 guidance（含已停用）。
pub fn list_guidance(data_root: &Path) -> Vec<Guidance> {
    load_state(data_root).guidances
}

/// 新增一条 guidance 并持久化（id = `guid_<17位时间戳>`）。
pub fn add_guidance(
    data_root: &Path,
    title: &str,
    description: &str,
    source: &str,
) -> Result<Guidance, String> {
    let mut st = load_state(data_root);
    let g = Guidance::new(title, description, source);
    st.guidances.push(g.clone());
    save_state(data_root, &st).map_err(|e| e.to_string())?;
    Ok(g)
}

/// 软删除：把指定 id 的 guidance 置 active=false（找不到返回 Err）。
pub fn deactivate_guidance(data_root: &Path, id: &str) -> Result<(), String> {
    let mut st = load_state(data_root);
    let target = st
        .guidances
        .iter_mut()
        .find(|g| g.id == id)
        .ok_or_else(|| format!("guidance `{id}` not found"))?;
    if target.active {
        target.active = false;
        target.updated_at = timestamp_17();
    }
    save_state(data_root, &st).map_err(|e| e.to_string())?;
    Ok(())
}

/// 讨论结果：LLM 的回复文本 + 可选的待固化建议条目。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DiscussResult {
    pub reply: String,
    #[serde(default)]
    pub suggested: Option<Guidance>,
}

/// 讨论系统 prompt。
const DISCUSS_SYSTEM_PROMPT: &str = "你是 Kaleido 自进化 harness 的需求对齐助手。\
 你的任务是与用户讨论/澄清其期望方向，最终目标是产出一条可固化的 Guidance。\
 规则：\
 - 先就模糊处提问或简述你对该需求的理解，不要急着固化。\
 - 若需求足够明确，在回复**末尾**附上一段由 <SUGGEST_GUIDANCE> 包裹的建议条目，格式为：\
   <SUGGEST_GUIDANCE>title|description</SUGGEST_GUIDANCE>（title 为简短期望标题，description 为期望内容描述）。\
 - 若需求仍不够明确，则只给澄清/讨论文本，不要输出 <SUGGEST_GUIDANCE>。";

/// 讨论/澄清接口。只讨论不自动固化；`auto_commit=true` 且 LLM 给出建议时才固化。
///
/// 解析 `<SUGGEST_GUIDANCE>title|description</SUGGEST_GUIDANCE>` 得到 `suggested`；
/// 没有该标记则 `suggested = None`。
pub async fn discuss(
    data_root: &Path,
    llm: &dyn LlmClient,
    user_message: &str,
    history_tail: &str,
    auto_commit: bool,
) -> Result<DiscussResult, String> {
    let st = load_state(data_root);
    let guidance_ctx = guidance_summary(&st);

    let mut user = String::new();
    user.push_str("<active_guidance>\n");
    user.push_str(if guidance_ctx.is_empty() { "（当前无已固化期望）" } else { &guidance_ctx });
    user.push_str("\n</active_guidance>\n\n");
    user.push_str("<recent_refinement_history>\n");
    let recent = recent_history(&st);
    let recent_json = serde_json::to_string_pretty(&recent)
        .unwrap_or_else(|_| "[]".to_string());
    user.push_str(&recent_json);
    user.push_str("\n</recent_refinement_history>\n\n");
    user.push_str("<user_message>\n");
    user.push_str(user_message);
    user.push_str("\n</user_message>\n\n");
    if !history_tail.trim().is_empty() {
        user.push_str("<conversation_tail>\n");
        user.push_str(history_tail);
        user.push_str("\n</conversation_tail>\n\n");
    }

    let resp = llm
        .complete(DISCUSS_SYSTEM_PROMPT, &user, 4000)
        .await
        .map_err(|e| e.to_string())?;

    let (reply, suggested) = parse_discuss_reply(&resp);

    match suggested {
        Some(g) if auto_commit => {
            let committed = add_guidance(data_root, &g.title, &g.description, "discuss")?;
            Ok(DiscussResult {
                reply,
                suggested: Some(committed),
            })
        }
        _ => Ok(DiscussResult { reply, suggested }),
    }
}

/// 从 LLM 回复中提取 `<SUGGEST_GUIDANCE>title|description</SUGGEST_GUIDANCE>`。
fn parse_discuss_reply(raw: &str) -> (String, Option<Guidance>) {
    const OPEN: &str = "<SUGGEST_GUIDANCE>";
    const CLOSE: &str = "</SUGGEST_GUIDANCE>";

    let reply = raw.replace(OPEN, "").replace(CLOSE, "").trim().to_string();
    let Some(open_idx) = raw.find(OPEN) else {
        return (reply, None);
    };
    let Some(close_idx) = raw.find(CLOSE) else {
        return (reply, None);
    };
    if open_idx >= close_idx {
        return (reply, None);
    }
    let inner = &raw[open_idx + OPEN.len()..close_idx];
    let trimmed = inner.trim();
    let Some((title, description)) = trimmed.split_once('|') else {
        return (reply, None);
    };
    if title.trim().is_empty() || description.trim().is_empty() {
        return (reply, None);
    }
    let g = Guidance::new(title.trim(), description.trim(), "discuss");
    (reply, Some(g))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 方案 A 的 mock：返回固定文本；调用计数用于断言 gate 触发。
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
            let r = self.result.clone();
            Box::pin(async move { r })
        }
    }

    fn tmp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kaleido-server-harness-test-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn valid_create_proposal() -> String {
        r#"{
          "id": "refine_00000000000000000",
          "edits": [
            { "action": "create", "kind": "memory", "id": "m_note", "title": "note", "content": "remember this" }
          ],
          "rationale": "persist snippet"
        }"#
        .to_string()
    }

    #[tokio::test]
    async fn run_refine_applies_and_persists_state() {
        let root = tmp_root("run-refine-ok");
        let llm = MockLlm {
            result: Ok(valid_create_proposal()),
        };

        let res = run_refine(&root, &llm, "some conversation tail", None, None)
            .await
            .expect("run_refine should succeed");
        assert!(res.success_count() >= 1, "expected at least one applied edit");

        // harness_state.json 已落盘且 apply 后的状态可读回。
        let state_path = state_dir(&root).join(kaleido_harness::store::STATE_FILE);
        assert!(state_path.exists(), "harness_state.json missing");
        let loaded = load_state(&root);
        assert!(
            loaded.entries["memory"].contains_key("m_note"),
            "m_note should be persisted"
        );
        assert_eq!(loaded.entries["memory"]["m_note"].content, "remember this");
        // 成功≥1 → 写入了一条 RefinementEvent。
        assert_eq!(loaded.refinements.len(), 1);
        assert_eq!(loaded.refinements[0].trigger, "summary");

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_refine_garbage_returns_err_without_panic() {
        let root = tmp_root("run-refine-garbage");
        let llm = MockLlm {
            result: Ok("just prose, no json".to_string()),
        };

        let err = run_refine(&root, &llm, "tail", None, None).await.unwrap_err();
        assert!(err.contains("parse") || err.contains("LLM"), "unexpected err: {err}");

        // 不落盘任何错误产物。
        assert!(!state_dir(&root).join(kaleido_harness::store::STATE_FILE).exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_refine_empty_edits_returns_err() {
        let root = tmp_root("run-refine-empty");
        let llm = MockLlm {
            result: Ok(r#"{"id":"x","edits":[],"rationale":"nothing"}"#.to_string()),
        };

        let err = run_refine(&root, &llm, "tail", None, None).await.unwrap_err();
        assert!(err.contains("empty"), "expected empty proposal err, got {err}");

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_refine_llm_error_propagates() {
        let root = tmp_root("run-refine-llm-err");
        let llm = MockLlm {
            result: Err(LlmError::Upstream("boom".into())),
        };

        let err = run_refine(&root, &llm, "tail", None, None).await.unwrap_err();
        assert!(err.contains("boom"), "expected upstream message, got {err}");

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn auto_refine_gate_true_when_review_says_yes() {
        let root = tmp_root("gate-true");
        let llm = MockLlm {
            result: Ok(r#"{"shouldRefine":true,"rationale":"growing","instructions":"focus prompts"}"#.to_string()),
        };
        let yes = auto_refine_gate(&root, &llm, "turn_interval", "tail")
            .await
            .expect("gate should not error");
        assert!(yes);
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn auto_refine_gate_false_when_review_says_no() {
        let root = tmp_root("gate-false");
        let llm = MockLlm {
            result: Ok(r#"{"shouldRefine":false}"#.to_string()),
        };
        let no = auto_refine_gate(&root, &llm, "compact", "tail").await.unwrap();
        assert!(!no);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn add_guidance_persists_and_can_be_read_back() {
        let root = tmp_root("guidance-add");
        let g = add_guidance(&root, "更统一的语气", "prompt 输出应保持简练一致的语气", "user")
            .expect("add_guidance should succeed");
        assert!(g.id.starts_with("guid_"));
        assert!(g.active);

        let listed = list_guidance(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "更统一的语气");
        assert_eq!(listed[0].description, "prompt 输出应保持简练一致的语气");
        assert_eq!(listed[0].source, "user");
        assert!(listed[0].active);

        // 落盘后可读回（persistence via harness_state.json）。
        let reloaded = load_state(&root);
        assert_eq!(reloaded.guidances.len(), 1);
        assert_eq!(reloaded.guidances[0].id, g.id);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn deactivate_guidance_removes_from_active_list() {
        let root = tmp_root("guidance-deactivate");
        let g = add_guidance(&root, "增补记忆", "多记一条用户偏好", "user").unwrap();
        assert_eq!(list_guidance(&root).len(), 1);

        deactivate_guidance(&root, &g.id).expect("deactivate should succeed");
        let listed = list_guidance(&root);
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].active, "should be soft-deleted");

        // guidance_summary 只含 active → 空。
        let st = load_state(&root);
        assert!(guidance_summary(&st).is_empty());

        // 找不到 id → Err。
        assert!(deactivate_guidance(&root, "guid_missing_none").is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn guidance_summary_formats_active_only() {
        let root = tmp_root("guidance-summary");
        let g1 = add_guidance(&root, "保持简练", "输出要简短", "user").unwrap();
        let g2 = add_guidance(&root, "中文优先", "一律中文", "discuss").unwrap();
        deactivate_guidance(&root, &g1.id).unwrap();
        let st = load_state(&root);
        let sum = guidance_summary(&st);
        assert!(!sum.contains("保持简练"), "inactive guidance leaked into summary");
        assert!(sum.contains("中文优先"));
        assert!(sum.contains("discuss"));
        assert!(!g2.id.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn discuss_parses_reply_and_suggestion() {
        let root = tmp_root("discuss-suggest");
        let llm = MockLlm {
            result: Ok(
                "我觉得应该让 prompt 更一致。建议固化以下条目：\n\
                 <SUGGEST_GUIDANCE>统一语气|所有 prompt 输出应保持一致的简练语气</SUGGEST_GUIDANCE>"
                    .to_string(),
            ),
        };

        let res = discuss(&root, &llm, "我希望 prompt 语气更统一", "", false)
            .await
            .expect("discuss should succeed");
        assert!(
            res.reply.contains("让 prompt 更一致"),
            "reply = {:?}",
            res.reply
        );
        assert!(res.reply.contains("统一语气"), "reply should keep the lazy text");
        let suggested = res.suggested.expect("suggested should be present");
        assert_eq!(suggested.title, "统一语气");
        assert_eq!(suggested.description, "所有 prompt 输出应保持一致的简练语气");
        assert_eq!(suggested.source, "discuss");
        // 未 auto_commit → 不应落盘。
        assert!(list_guidance(&root).is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn discuss_without_suggestion_returns_none() {
        let root = tmp_root("discuss-none");
        let llm = MockLlm {
            result: Ok("好的，请再具体一点：是希望精简 prompt 本身，还是统一回复语气？".to_string()),
        };

        let res = discuss(&root, &llm, "想改进一下 prompt", "", false)
            .await
            .expect("discuss should succeed");
        assert!(res.suggested.is_none(), "no SUGGEST_GUIDANCE → None");
        assert!(!res.reply.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn discuss_auto_commit_persists_suggestion() {
        let root = tmp_root("discuss-commit");
        let llm = MockLlm {
            result: Ok(
                "已明确，固化如下。\n\
                 <SUGGEST_GUIDANCE>减少改动|尽量少动与用户目标无关的条目</SUGGEST_GUIDANCE>"
                    .to_string(),
            ),
        };

        let res = discuss(&root, &llm, "尽量少改无关的东西", "", true)
            .await
            .expect("discuss should succeed");
        let committed = res.suggested.expect("auto_commit should return committed guidance");
        assert!(!committed.id.is_empty());
        assert!(committed.active);

        let listed = list_guidance(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "减少改动");
        assert_eq!(listed[0].source, "discuss");

        let _ = fs::remove_dir_all(&root);
    }
}