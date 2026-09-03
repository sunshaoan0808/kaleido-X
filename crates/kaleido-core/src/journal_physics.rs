//! Journal Physics — deterministic emotional heat/cold/mood/salience.
//!
//! Port of Front Porch AI `lib/services/chat/journal_physics.dart` (AGPL-3.0, reimplemented).
//! Pure constants + functions: heat cooling with flashbulb, cold threshold,
//! mood-congruent recall, salient-event detection. No LLM, no I/O.

use std::collections::HashSet;

// ── tunables (Front Porch verbatim) ─────────────────────────────────────────

pub const K_MAX_HEAT: f64 = 1.0;
pub const K_BASE_DECAY_PER_PASS: f64 = 0.15;
pub const K_MODERATE_DECAY_FACTOR: f64 = 0.5;
pub const K_STRONG_DECAY_FACTOR: f64 = 0.15;
pub const K_COLD_THRESHOLD: f64 = 0.35;
pub const K_REWARM_HEAT: f64 = 0.75;
pub const K_MOOD_BOOST: f64 = 0.25;
pub const K_MOOD_SIMILARITY_BONUS: f64 = 0.1;
pub const K_MIN_COLD_SIMILARITY: f64 = 0.35;
pub const K_COLD_RETRIEVAL_LIMIT: usize = 3;
pub const K_MIN_EXPAND_SIMILARITY: f64 = 0.45;
pub const K_MAX_EXPANDED_CARDS: usize = 1;
pub const K_EVENT_BOND_SWING: i32 = 12;
pub const K_EVENT_TRUST_SWING: i32 = 12;

// ── card model (minimal, for pure physics) ─────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct JournalCard {
    pub heat: f64,
    pub pinned: bool,
    pub intensity: String, // "mild" | "moderate" | "strong" | "pinned"
    pub kind: Option<String>, // "milestone" | "promise" | "item" | "episode" | None
    pub content: String,
    pub emotion_label: Option<String>,
    pub metadata_item: Option<String>,
    pub created_at: i64,
}

impl JournalCard {
    pub fn new(content: impl Into<String>) -> Self {
        Self { heat: K_MAX_HEAT, pinned: false, intensity: "mild".into(), kind: None, content: content.into(), emotion_label: None, metadata_item: None, created_at: 0 }
    }
    pub fn with_intensity(mut self, v: impl Into<String>) -> Self { self.intensity = v.into(); self }
    pub fn with_kind(mut self, v: impl Into<String>) -> Self { self.kind = Some(v.into()); self }
    pub fn with_heat(mut self, v: f64) -> Self { self.heat = v; self }
    pub fn pinned(mut self) -> Self { self.pinned = true; self }
}

// ── helpers ──────────────────────────────────────────────────────────────────

pub fn is_ledger_card(c: &JournalCard) -> bool {
    matches!(c.kind.as_deref(), Some("milestone") | Some("promise"))
}
pub fn is_item_card(c: &JournalCard) -> bool { c.kind.as_deref() == Some("item") }
pub fn is_episode_card(c: &JournalCard) -> bool { c.kind.as_deref() == Some("episode") }

pub fn cooled_heat(card: &JournalCard) -> f64 {
    if card.pinned || is_ledger_card(card) { return card.heat; }
    let factor = match card.intensity.as_str() {
        "strong" => K_STRONG_DECAY_FACTOR,
        "moderate" => K_MODERATE_DECAY_FACTOR,
        _ => 1.0,
    };
    (card.heat - K_BASE_DECAY_PER_PASS * factor).max(0.0)
}

pub fn is_hot(card: &JournalCard) -> bool {
    if is_ledger_card(card) { return false; }
    card.pinned || card.heat >= K_COLD_THRESHOLD
}

fn emotion_family(label: &str) -> String {
    let l = label.trim().to_ascii_lowercase().replace(' ', "_");
    if l.is_empty() { return String::new(); }
    // tiny mapping of nuanced -> standard (Front Porch EmotionLabels.nuancedToStandard subset)
    match l.as_str() {
        "wistful" | "melancholy" | "blue" => "sadness".into(),
        "joyful" | "cheerful" | "elated" => "happiness".into(),
        "furious" | "enraged" => "anger".into(),
        "terrified" | "scared" => "fear".into(),
        _ => l,
    }
}

pub fn mood_congruent(card_emotion: Option<&str>, current: &str) -> bool {
    let cur = emotion_family(current);
    if cur.is_empty() { return false; }
    let card = card_emotion.map(emotion_family).unwrap_or_default();
    !card.is_empty() && card == cur
}

pub fn hot_sort_key(card: &JournalCard, current_emotion: &str) -> f64 {
    card.heat + if mood_congruent(card.emotion_label.as_deref(), current_emotion) { K_MOOD_BOOST } else { 0.0 }
}

pub fn item_name_tokens(s: &str) -> HashSet<String> {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_string())
        .collect()
}

pub fn item_card_mentioned(card: &JournalCard, query_tokens: &HashSet<String>) -> bool {
    if query_tokens.is_empty() || !is_item_card(card) { return false; }
    let name = match &card.metadata_item { Some(n) if !n.is_empty() => n, _ => return false };
    item_name_tokens(name).iter().any(|t| query_tokens.contains(t))
}

/// Top hot card content (hottest by hot_sort_key, tie-break newest).
pub fn top_hot_line(cards: &[JournalCard], current_emotion: &str) -> Option<String> {
    let mut best: Option<&JournalCard> = None;
    let mut best_key = f64::NEG_INFINITY;
    for c in cards {
        if !is_hot(c) { continue; }
        let k = hot_sort_key(c, current_emotion);
        if best.is_none() || k > best_key || (k == best_key && c.created_at > best.unwrap().created_at) {
            best = Some(c); best_key = k;
        }
    }
    let t = best?.content.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn cooled_heat_mild_decays_full() { let c = JournalCard::new("x"); assert!((cooled_heat(&c) - 0.85).abs() < 1e-9); }
    #[test] fn cooled_heat_strong_resists() { let c = JournalCard::new("x").with_intensity("strong"); assert!((cooled_heat(&c) - (1.0 - 0.15*0.15)).abs() < 1e-9); }
    #[test] fn cooled_heat_pinned_no_decay() { let c = JournalCard::new("x").pinned(); assert_eq!(cooled_heat(&c), 1.0); }
    #[test] fn cooled_heat_ledger_no_decay() { let c = JournalCard::new("x").with_kind("milestone"); assert_eq!(cooled_heat(&c), 1.0); }
    #[test] fn is_hot_threshold() { assert!(is_hot(&JournalCard::new("x").with_heat(0.35))); assert!(!is_hot(&JournalCard::new("x").with_heat(0.34))); }
    #[test] fn is_hot_ledger_never() { assert!(!is_hot(&JournalCard::new("x").with_kind("promise").with_heat(1.0))); }
    #[test] fn mood_congruent_family() { assert!(mood_congruent(Some("wistful"), "sadness")); assert!(!mood_congruent(Some("joyful"), "sadness")); }
    #[test] fn top_hot_picks_hottest() {
        let a = JournalCard::new("hot").with_heat(0.9);
        let b = JournalCard::new("cold").with_heat(0.4);
        assert_eq!(top_hot_line(&[a,b], ""), Some("hot".into()));
    }
    #[test] fn item_mentioned() {
        let c = JournalCard { heat: 1.0, pinned: false, intensity: "mild".into(), kind: Some("item".into()), content: "keys".into(), emotion_label: None, metadata_item: Some("car keys".into()), created_at: 0 };
        let mut q = HashSet::new(); q.insert("keys".into());
        assert!(item_card_mentioned(&c, &q));
    }
}
