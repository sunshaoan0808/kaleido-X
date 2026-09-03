//! Dreams + Episode Crumbs — night-crossing dreams + ordinary-day crumbs.
//!
//! Port of Front Porch AI `lib/services/chat/dream_service.dart` + `episode_crumbs.dart`
//! (AGPL-3.0, reimplemented). Pure, no LLM: rollover bookkeeping + crumb state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DreamState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session: Option<String>,
    #[serde(default)]
    pub last_day: i32,
    #[serde(default)]
    pub pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dream: Option<String>,
}

impl DreamState {
    pub fn check_rollover(&mut self, session_id: Option<&str>, day: i32, enabled: bool) {
        let Some(sid) = session_id else { return; };
        if self.last_session.as_deref() != Some(sid) {
            self.last_session = Some(sid.to_string());
            self.last_day = day;
            self.pending = false;
            return;
        }
        if day > self.last_day && enabled { self.pending = true; }
        if day != self.last_day { self.last_day = day; }
    }
    pub fn consume_pending(&mut self) -> bool {
        if self.pending { self.pending = false; true } else { false }
    }
    pub fn dream_prompt(&self, character: &str, fragments: &[String], emotion: &str, recap: &str) -> Option<String> {
        if fragments.is_empty() && emotion.is_empty() && recap.is_empty() { return None; }
        let frags = if fragments.is_empty() { "(no strong memories yet — keep it vague)".into() }
        else { fragments.iter().take(5).map(|m| format!("- {m}")).collect::<Vec<_>>().join("\n") };
        Some(format!(
            "Write the dream {character} had last night: brief hazy first-person 2-4 sentences.\nFragments:\n{frags}\nMood: {emotion}\nWhere story stands: {recap}\nRules: first-person as {character}, dreamlike associative, reference ONLY fragments above."
        ))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodeCrumb {
    pub id: String,
    pub kind: String, // "work" | "social" | "wander" etc
    pub content: String,
    pub created_at_turn: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodeStore {
    #[serde(default)]
    pub crumbs: Vec<EpisodeCrumb>,
}

impl EpisodeStore {
    pub fn push(&mut self, kind: impl Into<String>, content: impl Into<String>, turn: u32) {
        self.crumbs.push(EpisodeCrumb { id: uuid::Uuid::new_v4().to_string(), kind: kind.into(), content: content.into(), created_at_turn: turn });
        if self.crumbs.len() > 50 { self.crumbs.remove(0); }
    }
    pub fn recent_for_prompt(&self, n: usize) -> Vec<&EpisodeCrumb> { self.crumbs.iter().rev().take(n).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn dream_rollover_pending() {
        let mut d = DreamState::default();
        d.check_rollover(Some("s1"), 1, true);
        assert!(!d.pending);
        d.check_rollover(Some("s1"), 2, true);
        assert!(d.pending);
        assert!(d.consume_pending());
        assert!(!d.pending);
    }
    #[test] fn dream_no_pending_when_disabled() {
        let mut d = DreamState::default();
        d.check_rollover(Some("s1"), 1, true);
        d.check_rollover(Some("s1"), 2, false);
        assert!(!d.pending);
    }
    #[test] fn crumbs_cap() {
        let mut s = EpisodeStore::default();
        for i in 0..60 { s.push("work", format!("crumb {i}"), i); }
        assert_eq!(s.crumbs.len(), 50);
    }
}
