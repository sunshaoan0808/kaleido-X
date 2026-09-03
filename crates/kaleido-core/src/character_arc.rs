//! Character arc tracking — cross-chapter growth/change records.
//!
//! Pure data layer (no LLM): consumes flat `ArcEntry` change records and
//! aggregates per-character arcs with a heuristic arc type. Absorbed from
//! novel2hermes_jp's revision-workflow "亲子记录" (cross-chapter tracking)
//! pattern.

use serde::{Deserialize, Serialize};

/// One recorded change for a character at a chapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArcEntry {
    pub character: String,
    pub chapter: String,
    pub field: String,
    pub from: String,
    pub to: String,
}

/// A single change in a character's arc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArcChange {
    pub chapter: String,
    pub field: String,
    pub from: String,
    pub to: String,
    pub note: String,
}

/// Aggregated arc for one character.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterArc {
    pub character: String,
    pub changes: Vec<ArcChange>,
    /// Heuristic arc type: 成长 / 黑化 / 回归 / 稳定.
    pub arc_type: String,
}

/// Fields that commonly indicate growth vs darkening.
const GROWTH_FIELDS: &[&str] = &["成长", "实力", "心态", "成熟", "领悟", "信任", "责任"];
const DARK_FIELDS: &[&str] = &["黑化", "仇恨", "堕落", "疯狂", "杀意", "偏执"];

/// Build per-character arcs from flat change entries.
pub fn build_character_arcs(entries: &[ArcEntry]) -> Vec<CharacterArc> {
    use std::collections::HashMap;
    let mut by_char: HashMap<String, Vec<ArcChange>> = HashMap::new();
    for e in entries {
        by_char
            .entry(e.character.clone())
            .or_default()
            .push(ArcChange {
                chapter: e.chapter.clone(),
                field: e.field.clone(),
                from: e.from.clone(),
                to: e.to.clone(),
                note: note_for(&e.field, &e.from, &e.to),
            });
    }
    let mut arcs: Vec<CharacterArc> = by_char
        .into_iter()
        .map(|(character, mut changes)| {
            changes.sort_by(|a, b| a.chapter.cmp(&b.chapter));
            let arc_type = classify(&changes);
            CharacterArc { character, changes, arc_type }
        })
        .collect();
    arcs.sort_by(|a, b| a.character.cmp(&b.character));
    arcs
}

/// Heuristic note for a single change.
fn note_for(field: &str, from: &str, to: &str) -> String {
    if from == to {
        format!("{field}保持不变")
    } else {
        format!("{field}: {from} → {to}")
    }
}

/// Classify arc type from the change sequence (heuristic).
fn classify(changes: &[ArcChange]) -> String {
    if changes.is_empty() {
        return "稳定".to_string();
    }
    let mut growth = 0usize;
    let mut dark = 0usize;
    let mut regress = 0usize;
    for c in changes {
        let field_lower = c.field.to_lowercase();
        if GROWTH_FIELDS.iter().any(|f| field_lower.contains(f)) {
            growth += 1;
        } else if DARK_FIELDS.iter().any(|f| field_lower.contains(f)) {
            dark += 1;
        }
        // Simple regression heuristic: non-empty "from" but empty/weaker "to".
        if !c.from.is_empty() && c.to.is_empty() {
            regress += 1;
        }
    }
    if dark > growth && dark >= 2 {
        "黑化".to_string()
    } else if growth > dark && growth >= 2 {
        "成长".to_string()
    } else if regress >= 2 {
        "回归".to_string()
    } else {
        "稳定".to_string()
    }
}

use std::collections::HashMap;

// ── Growth Rings — Front Porch AI growth-rings.md reimplemented ────────────
// A ring is a small, receipt-backed personality layer on top of the original card.
// Strong rings barely cool (flashbulb), weak rings fade in a few passes.

/// One growth ring on a character (per-session, per-character).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrowthRing {
    pub id: String,
    pub character: String,
    pub trigger_event: String,
    pub strength: f32, // 0.0-1.0, flashbulb resistance
    #[serde(default)]
    pub faded: bool,
    pub created_at_turn: u32,
}

impl GrowthRing {
    pub fn new(character: impl Into<String>, trigger_event: impl Into<String>, strength: f32, turn: u32) -> Self {
        Self { id: uuid::Uuid::new_v4().to_string(), character: character.into(), trigger_event: trigger_event.into(), strength: strength.clamp(0.0, 1.0), faded: false, created_at_turn: turn }
    }
    /// Flashbulb-style cooled strength after n passes.
    pub fn cooled_strength(&self, passes: u32) -> f32 {
        if self.faded { return 0.0; }
        let factor = if self.strength >= 0.7 { 0.15 } else if self.strength >= 0.4 { 0.5 } else { 1.0 };
        (self.strength - crate::journal_physics::K_BASE_DECAY_PER_PASS as f32 * factor * passes as f32).max(0.0)
    }
    pub fn is_active(&self) -> bool { !self.faded && self.strength > 0.05 }
}

/// Per-character ring store (session-scoped).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrowthStore {
    #[serde(default)]
    pub rings: Vec<GrowthRing>,
}

impl GrowthStore {
    pub fn active_for(&self, character: &str) -> Vec<&GrowthRing> {
        self.rings.iter().filter(|r| r.character == character && r.is_active()).collect()
    }
    pub fn active_for_all(&self) -> std::collections::HashMap<String, Vec<&GrowthRing>> {
        let mut m: std::collections::HashMap<String, Vec<&GrowthRing>> = std::collections::HashMap::new();
        for r in &self.rings { if r.is_active() { m.entry(r.character.clone()).or_default().push(r); } }
        m
    }
    /// GrowthPhysics thresholds: developing 0.35 / established 0.8.
    pub fn tier_of(strength: f32) -> &'static str {
        if strength >= 0.8 { "established" } else if strength >= 0.35 { "developing" } else { "fragile" }
    }
    /// Injection selection: top 8 by strength, reserve 2 fresh slots for strength<0.35.
    /// Returns (injected, reserved_fresh_count).
    pub fn injection_selection(&self, character: &str) -> Vec<&GrowthRing> {
        let mut act = self.active_for(character);
        act.sort_by(|a,b| b.strength.partial_cmp(&a.strength).unwrap_or(std::cmp::Ordering::Equal));
        const MAX_ACTIVE: usize = 12;
        const INJECTED: usize = 8;
        const FRESH_SLOTS: usize = 2;
        let act: Vec<&GrowthRing> = act.into_iter().take(MAX_ACTIVE).collect();
        let fresh: Vec<&GrowthRing> = act.iter().filter(|r| r.strength < 0.35).take(FRESH_SLOTS).cloned().collect();
        let mut out: Vec<&GrowthRing> = act.iter().take(INJECTED).cloned().collect();
        for f in fresh { if !out.iter().any(|r| r.id==f.id) { out.push(f); } }
        out.truncate(INJECTED + FRESH_SLOTS);
        out
    }
    pub fn strengthen(&mut self, character: &str, event: &str, delta: f32, turn: u32) {
        if let Some(r) = self.rings.iter_mut().find(|r| r.character == character && r.trigger_event == event && !r.faded) {
            r.strength = (r.strength + delta).clamp(0.0, 1.0);
        } else {
            self.rings.push(GrowthRing::new(character, event, delta.clamp(0.2, 1.0), turn));
        }
    }
    pub fn fade_old(&mut self, turn: u32, max_age_turns: u32) {
        for r in &mut self.rings { if turn.saturating_sub(r.created_at_turn) > max_age_turns { r.faded = true; } }
    }
    pub fn injection_block(&self, character: &str) -> String {
        let sel = self.injection_selection(character);
        if sel.is_empty() { return String::new(); }
        let mut lines = vec![format!("Character Growth for {character}:")];
        for r in sel { lines.push(format!("- {} ({}, {:.2})", r.trigger_event, Self::tier_of(r.strength), r.strength)); }
        lines.join("\n")
    }
    pub fn by_character(&self) -> HashMap<String, Vec<&GrowthRing>> {
        let mut m: HashMap<String, Vec<&GrowthRing>> = HashMap::new();
        for r in &self.rings { m.entry(r.character.clone()).or_default().push(r); }
        m
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;

    #[test]
    fn growth_ring_cooling() {
        let r = GrowthRing::new("Aria", "first kiss", 0.9, 0);
        assert!(r.cooled_strength(0) > 0.8);
        assert!(r.cooled_strength(10) > 0.6); // strong resists
        let weak = GrowthRing::new("Aria", "lost keys", 0.2, 0);
        assert!(weak.cooled_strength(5) < 0.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert!(build_character_arcs(&[]).is_empty());
    }

    #[test]
    fn growth_arc() {
        let arcs = build_character_arcs(&[
            ArcEntry { character: "林".into(), chapter: "c1".into(), field: "成长".into(), from: "懵懂".into(), to: "坚定".into() },
            ArcEntry { character: "林".into(), chapter: "c2".into(), field: "责任".into(), from: "逃避".into(), to: "承担".into() },
        ]);
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].arc_type, "成长");
        assert_eq!(arcs[0].changes.len(), 2);
    }

    #[test]
    fn dark_arc() {
        let arcs = build_character_arcs(&[
            ArcEntry { character: "妖".into(), chapter: "c1".into(), field: "黑化".into(), from: "克制".into(), to: "失控".into() },
            ArcEntry { character: "妖".into(), chapter: "c2".into(), field: "杀意".into(), from: "无".into(), to: "浓烈".into() },
        ]);
        assert_eq!(arcs[0].arc_type, "黑化");
    }
}
