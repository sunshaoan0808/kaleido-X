//! Promise Debt — open/kept/broken commitment tracking.
//!
//! Port of Front Porch AI `lib/services/chat/promise_debt_service.dart`
//! (AGPL-3.0, reimplemented). Pure structs, no LLM. Kept/broken resolves
//! via explicit API; trust/bond deltas applied by caller (relationship.rs).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Promise {
    pub id: String,
    pub text: String,
    /// 'user' | 'char'
    #[serde(default)]
    pub party: String,
    /// 'open' | 'kept' | 'broken'
    #[serde(default = "default_open")]
    pub status: String,
    #[serde(default)]
    pub character: String,
    pub created_at_turn: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at_turn: Option<u32>,
}

fn default_open() -> String { "open".into() }

impl Promise {
    pub fn new(character: impl Into<String>, party: impl Into<String>, text: impl Into<String>, turn: u32) -> Self {
        Self { id: uuid::Uuid::new_v4().to_string(), character: character.into(), party: party.into(), text: text.into(), status: "open".into(), created_at_turn: turn, resolved_at_turn: None }
    }
    pub fn is_open(&self) -> bool { self.status == "open" }
    pub fn resolve(&mut self, kept: bool, turn: u32) {
        self.status = if kept { "kept" } else { "broken" }.into();
        self.resolved_at_turn = Some(turn);
    }
    /// Trust/bond delta suggestion: kept +3/+5, broken -8/-6.
    pub fn resolve_deltas(kept: bool) -> (i32, i32) { if kept { (5, 3) } else { (-6, -8) } }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromiseStore {
    #[serde(default)]
    pub promises: Vec<Promise>,
}

impl PromiseStore {
    pub fn open(&self) -> Vec<&Promise> { self.promises.iter().filter(|p| p.is_open()).collect() }
    pub fn push(&mut self, p: Promise) { self.promises.push(p); if self.promises.len() > 50 { self.promises.remove(0); } }
    pub fn resolve(&mut self, id: &str, kept: bool, turn: u32) -> Option<(i32,i32)> {
        if let Some(p) = self.promises.iter_mut().find(|p| p.id==id && p.is_open()) {
            p.resolve(kept, turn);
            Some(Promise::resolve_deltas(kept))
        } else { None }
    }
    pub fn injection_block(&self) -> String {
        let open = self.open();
        if open.is_empty() { return String::new(); }
        let mut lines = vec!["## 未竟承诺（必须追踪兑现/失信）".to_string()];
        for p in open.iter().take(5) {
            lines.push(format!("- [{}·{}] {}", p.party, p.character, p.text));
        }
        lines.push("规则：正文中若兑现/违背任一承诺，后续由系统记账（kept/broken）；失信将扣 trust/bond。".into());
        lines.join("\n")
    }
}

/// Per-character preference lists for bond weighting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub likes: Vec<String>,
    #[serde(default)]
    pub dislikes: Vec<String>,
}

/// Scenario fade: strength 10→0 over user messages (6 per step, gone at 60).
/// Port of Front Porch AI `scenario_fade.dart` (AGPL-3.0, reimplemented).
pub fn scenario_strength(user_msg_count: usize) -> u8 {
    const PER_STEP: usize = 6;
    const MAX: i32 = 10;
    let dropped = (user_msg_count / PER_STEP) as i32;
    (MAX - dropped).clamp(0, MAX) as u8
}

pub fn wrap_scenario(scenario: &str, strength: u8) -> String {
    let text = scenario.trim();
    if text.is_empty() || strength == 0 { return String::new(); }
    if strength >= 8 { return format!("Scenario: {text}\n"); }
    // weaker: shorter reminder
    let short: String = text.chars().take(200).collect();
    format!("Scenario (faded {strength}/10): {short}\n")
}

/// Preference scoring: likes/dislikes weight bond deltas.
/// Port of Front Porch AI `preference_scoring.dart` (AGPL-3.0, reimplemented).
pub fn preference_weight(event_text: &str, likes: &[String], dislikes: &[String]) -> f64 {
    let low = event_text.to_lowercase();
    let hit_like = likes.iter().any(|l| !l.is_empty() && low.contains(&l.to_lowercase()));
    let hit_dislike = dislikes.iter().any(|d| !d.is_empty() && low.contains(&d.to_lowercase()));
    match (hit_like, hit_dislike) {
        (true, true) => 1.0,
        (true, false) => 1.5,
        (false, true) => 1.5,
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn promise_resolve() {
        let mut s = PromiseStore::default();
        let p = Promise::new("Aria","char","带她回家",0);
        let id = p.id.clone();
        s.push(p);
        assert_eq!(s.open().len(), 1);
        let d = s.resolve(&id, true, 5).unwrap();
        assert_eq!(d, (5,3));
        assert!(s.open().is_empty());
    }
    #[test] fn scenario_fade_steps() {
        assert_eq!(scenario_strength(0), 10);
        assert_eq!(scenario_strength(6), 9);
        assert_eq!(scenario_strength(60), 0);
        assert!(wrap_scenario("abc", 0).is_empty());
        assert!(wrap_scenario("abc", 10).contains("Scenario:"));
    }
    #[test] fn preference_weight_likes() {
        assert_eq!(preference_weight("she loves roses", &["roses".into()], &[]), 1.5);
        assert_eq!(preference_weight("plain day", &["roses".into()], &[]), 1.0);
    }
}
