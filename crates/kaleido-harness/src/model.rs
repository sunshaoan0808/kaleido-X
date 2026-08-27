//! P0 data layer: data structures for the self-evolving harness.
//!
//! All entities are serde-serializable so a full `HarnessState` can be
//! persisted to `data_root/harness/harness_state.json` and historical events
//! can be appended to `refinements.jsonl`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What kind of asset a harness entry refines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefinementKind {
    Prompt,
    Memory,
    Skill,
    Subagent,
}

/// The operation an edit performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefAction {
    Create,
    Update,
    Delete,
}

/// Where the entry applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessScope {
    Local,
    Global,
}

impl std::fmt::Display for RefinementKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RefinementKind::Prompt => "prompt",
            RefinementKind::Memory => "memory",
            RefinementKind::Skill => "skill",
            RefinementKind::Subagent => "subagent",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for RefinementKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "prompt" => Ok(RefinementKind::Prompt),
            "memory" => Ok(RefinementKind::Memory),
            "skill" => Ok(RefinementKind::Skill),
            "subagent" => Ok(RefinementKind::Subagent),
            other => Err(format!(
                "unknown refinement kind `{other}` (expected prompt|memory|skill|subagent)"
            )),
        }
    }
}

impl std::fmt::Display for RefAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RefAction::Create => "create",
            RefAction::Update => "update",
            RefAction::Delete => "delete",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for RefAction {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "create" => Ok(RefAction::Create),
            "update" => Ok(RefAction::Update),
            "delete" => Ok(RefAction::Delete),
            other => Err(format!(
                "unknown refine action `{other}` (expected create|update|delete)"
            )),
        }
    }
}

impl std::fmt::Display for HarnessScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            HarnessScope::Local => "local",
            HarnessScope::Global => "global",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for HarnessScope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "local" => Ok(HarnessScope::Local),
            "global" => Ok(HarnessScope::Global),
            other => Err(format!(
                "unknown harness scope `{other}` (expected local|global)"
            )),
        }
    }
}

/// A map of kind-string -> (entry id -> entry).
pub type EntriesByKind = std::collections::BTreeMap<String, std::collections::BTreeMap<String, HarnessEntry>>;

/// The full persisted harness state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessState {
    pub schema: u32,
    pub entries: EntriesByKind,
    pub refinements: Vec<RefinementEvent>,
    /// User-stated expectation entries (P4). `#[serde(default)]` keeps old
    /// state files (without the field) deserializable — schema is NOT bumped.
    #[serde(default)]
    pub guidances: Vec<Guidance>,
}

/// A user-stated expectation ("用户期望方向") that anchors self-evolution.
///
/// Persisted inside `HarnessState` (load/save with the whole state). `active`
/// allows soft-deactivation instead of hard deletion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guidance {
    /// `guid_<17位时间戳>`-style id.
    pub id: String,
    /// Short expectation title.
    pub title: String,
    /// Expectation content / desired direction description.
    pub description: String,
    /// `"user"` that the user declared it, `"discuss"` that it came out of a
    /// discuss round, or `"system"` for harness-internal entries.
    pub source: String,
    /// Whether the entry is still active (can be deactivated without deleting).
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Guidance {
    /// Convenience constructor using the current timestamp for id/created/updated.
    pub fn new(title: impl Into<String>, description: impl Into<String>, source: impl Into<String>) -> Self {
        let now = timestamp_17();
        let title = title.into();
        let description = description.into();
        let source = source.into();
        Guidance {
            id: format!("guid_{now}"),
            title,
            description,
            source,
            active: true,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Only the still-active entries (for context injection / alignment).
    pub fn active(guidances: &[Guidance]) -> Vec<Guidance> {
        guidances.iter().filter(|g| g.active).cloned().collect()
    }
}

/// An individual harness asset (prompt / memory / skill / subagent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessEntry {
    pub id: String,
    pub kind: RefinementKind,
    pub title: String,
    pub content: String,
    pub path: String,
    pub scope: HarnessScope,
    pub reference: Value,
    pub arguments: Value,
    pub metadata: Value,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub version: u32,
}

impl HarnessEntry {
    /// Convenience constructor filling all required-but-optional defaults.
    pub fn new(kind: RefinementKind, id: impl Into<String>, title: impl Into<String>) -> Self {
        HarnessEntry {
            id: id.into(),
            kind,
            title: title.into(),
            content: String::new(),
            path: "general".to_string(),
            scope: HarnessScope::Local,
            reference: Value::Null,
            arguments: Value::Null,
            metadata: Value::default(),
            source: "refine".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            version: 1,
        }
    }
}

/// A single proposed edit, as produced by a refine proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementEdit {
    pub action: RefAction,
    pub kind: RefinementKind,
    pub id: Option<String>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub path: Option<String>,
    pub reference: Option<Value>,
    pub arguments: Option<Value>,
    pub metadata: Option<Value>,
    pub reason: Option<String>,
}

/// A full refinement proposal (what the harness proposes to change).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementProposal {
    pub id: String,
    pub edits: Vec<RefinementEdit>,
    pub rationale: Option<String>,
    #[serde(default)]
    pub rollback_of: Option<String>,
}

/// An immutable event appended to the state's `refinements` log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementEvent {
    pub id: String,
    pub trigger: String,
    pub changes: Value,
    pub evidence: Value,
    pub outcome: String,
    /// [morphling EvoSkill P1 2026-08-19] 前置评估器结论（保持反馈历史可追溯）。
    #[serde(default)]
    pub evaluation: Option<ProposalEval>,
    pub created_at: String,
}

/// [morphling EvoSkill P1 2026-08-19] 提案前置评估结论（apply 前的确定性质量闸）。
/// 类比 EvoSkill 的 evaluator：退化/空/过大/自相矛盾的提案在落盘前被拦截。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalEval {
    /// pass | reject | flag
    pub verdict: String,
    /// 合计数分（0-100）；越高越可信。
    pub score: u8,
    /// 命中原因（如 "empty_content:edit#0"）。
    pub reasons: Vec<String>,
}

impl ProposalEval {
    /// 是否允许写入（仅 pass 直接放行；flag 可写但在事件里记录告警）。
    pub fn allowed(&self) -> bool {
        self.verdict != "reject"
    }
}

/// Result of applying a whole proposal; one record per edit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyResult {
    pub applied_edits: Vec<AppliedEdit>,
}

impl ApplyResult {
    /// The number of edits that applied successfully.
    pub fn success_count(&self) -> usize {
        self.applied_edits.iter().filter(|e| e.applied).count()
    }
}

/// Per-edit outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedEdit {
    pub id: String,
    pub applied: bool,
    pub error: Option<String>,
    pub before: Option<HarnessEntry>,
    pub after: Option<HarnessEntry>,
}

/// Timestamp helper shared across the crate: `"refine_<17位timestamp>"` style.
pub fn timestamp_17() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let s = nanos.to_string();
    // Pad on the left with zeros to 17 digits where needed.
    if s.len() >= 17 {
        s
    } else {
        format!("{:0>17}", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_fromstr_roundtrip() {
        for (k, s) in [
            (RefinementKind::Prompt, "prompt"),
            (RefinementKind::Memory, "memory"),
            (RefinementKind::Skill, "skill"),
            (RefinementKind::Subagent, "subagent"),
        ] {
            assert_eq!(k.to_string(), s);
            assert_eq!(s.parse::<RefinementKind>().unwrap(), k);
        }
        for (a, s) in [
            (RefAction::Create, "create"),
            (RefAction::Update, "update"),
            (RefAction::Delete, "delete"),
        ] {
            assert_eq!(a.to_string(), s);
            assert_eq!(s.parse::<RefAction>().unwrap(), a);
        }
        for (sc, s) in [
            (HarnessScope::Local, "local"),
            (HarnessScope::Global, "global"),
        ] {
            assert_eq!(sc.to_string(), s);
            assert_eq!(s.parse::<HarnessScope>().unwrap(), sc);
        }
    }

    #[test]
    fn kind_fromstr_rejects_unknown() {
        assert!("nope".parse::<RefinementKind>().is_err());
        assert!("".parse::<RefAction>().is_err());
        assert!("global ".parse::<HarnessScope>().is_err());
    }

    #[test]
    fn serde_rename_lowercase() {
        let k = RefinementKind::Prompt;
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"prompt\"");
        let a = RefAction::Update;
        assert_eq!(serde_json::to_string(&a).unwrap(), "\"update\"");
        let s = HarnessScope::Global;
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"global\"");
    }

    #[test]
    fn entry_new_defaults() {
        let e = HarnessEntry::new(RefinementKind::Skill, "s1", "My Skill");
        assert_eq!(e.id, "s1");
        assert_eq!(e.title, "My Skill");
        assert_eq!(e.path, "general");
        assert_eq!(e.scope, HarnessScope::Local);
        assert_eq!(e.source, "refine");
        assert_eq!(e.version, 1);
        assert_eq!(e.content, "");
        assert!(e.reference.is_null());
        assert!(e.arguments.is_null());
        assert_eq!(e.kind, RefinementKind::Skill);
    }

    #[test]
    fn default_state() {
        let s = HarnessState::default();
        assert_eq!(s.schema, 0);
        assert!(s.entries.is_empty());
        assert!(s.refinements.is_empty());
        assert!(s.guidances.is_empty());
    }

    #[test]
    fn guidance_new_defaults() {
        let g = Guidance::new("更统一的语气", "所有 prompt 输出应保持一致的简练语气", "user");
        assert!(g.id.starts_with("guid_"), "id = {}", g.id);
        assert!(g.id["guid_".len()..].len() >= 17);
        assert!(g.id["guid_".len()..].chars().all(|c| c.is_ascii_digit()));
        assert_eq!(g.title, "更统一的语气");
        assert!(!g.description.is_empty());
        assert_eq!(g.source, "user");
        assert!(g.active);
        assert!(!g.created_at.is_empty());
        assert!(!g.updated_at.is_empty());
    }

    #[test]
    fn guidance_active_filters() {
        let mut a = Guidance::new("a", "a description", "user");
        let b = Guidance::new("b", "b description", "discuss");
        a.active = false;
        let mixed = vec![a, b];
        let active = Guidance::active(&mixed);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].title, "b");
    }

    #[test]
    fn state_without_guidances_deserializes() {
        // 旧 state 文件（无 guidances 字段）必须能反序列化（serde default）。
        let old = r#"{"schema":0,"entries":{},"refinements":[]}"#;
        let s: HarnessState = serde_json::from_str(old).unwrap();
        assert!(s.guidances.is_empty());
        // 往返：guidances 序列化后能读回。
        let mut s2 = HarnessState::default();
        s2.guidances.push(Guidance::new("t", "d", "user"));
        let json = serde_json::to_string(&s2).unwrap();
        let back: HarnessState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.guidances.len(), 1);
        assert_eq!(back.guidances[0].title, "t");
    }
}