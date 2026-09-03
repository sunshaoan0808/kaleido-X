//! Needs — Sims-style six-dimensional need simulation.
//!
//! Port of Front Porch AI `lib/services/chat/needs_simulation.dart` (AGPL-3.0, reimplemented).
//! Six needs drift each turn and colour how a character feels. Hunger-low characters
//! reach for food in their pockets first.
//!
//! Model (Front Porch `needKeys`/`needDefaults`/`needDecay`):
//! - hunger, energy, social, fun, hygiene, comfort (bladder omitted for narrative tone;
//!   its catastrophe text is too explicit for a prose-first Tavern).
//! - Per-turn decay + cross-need boost modifiers (low energy → faster hunger etc.) +
//!   optional weather boost (cold/rain → comfort drains faster).
//! - Catastrophe at ≤0 for hunger/energy/comfort/hygiene (pendingCatastrophe arms ONE
//!   event for prompt injection), with post-catastrophe floors.
//! - Scene deltas are model-proposed, capped at `sceneDepletionAt1x` per need (negative
//!   bound) — the prompt tells the eval to only report what the scene explicitly costs.
//!
//! Pure data, deterministic, no I/O.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── constants (Front Porch verbatim where applicable) ────────────────────────

pub const NEED_KEYS: &[&str] = &["hunger", "energy", "social", "fun", "hygiene", "comfort"];

/// Defaults (re-tuned: keep 65-75 range so prose-noticed urgency appears after a few ticks).
pub fn need_defaults() -> HashMap<String, i32> {
    [
        ("hunger", 75), ("energy", 80), ("social", 65),
        ("fun", 65), ("hygiene", 75), ("comfort", 70),
    ].into_iter().map(|(k,v)| (k.to_string(), v)).collect()
}

/// Per-turn ambient decay (int, 0-100 scale).
pub fn need_decay() -> HashMap<String, i32> {
    [
        ("hunger", 2), ("energy", 3), ("social", 2),
        ("fun", 2), ("hygiene", 1), ("comfort", 2),
    ].into_iter().map(|(k,v)| (k.to_string(), v)).collect()
}

pub const NEED_URGENT: i32 = 35;
pub const NEED_CRITICAL: i32 = 20;

/// Max depletion ONE described event may propose at 1x (negative bound, per-need).
pub fn scene_depletion_at_1x() -> HashMap<String, i32> {
    [
        ("hunger", 12), ("energy", 12), ("social", 10),
        ("fun", 10), ("hygiene", 15), ("comfort", 12),
    ].into_iter().map(|(k,v)| (k.to_string(), v)).collect()
}
pub const SCENE_DEPLETION_FALLBACK: i32 = 10;

pub fn scene_depletion_cap_for(key: &str, strength: i32) -> i32 {
    let base = scene_depletion_at_1x().get(key).copied().unwrap_or(SCENE_DEPLETION_FALLBACK);
    let s = strength.clamp(1, 5);
    base + (s - 1) * 2
}

// ── catastrophe ─────────────────────────────────────────────────────────────

pub fn need_catastrophe_text() -> HashMap<String, String> {
    [
        ("hunger", "Starvation buckles them — they sag, grey-faced and unsteady, barely able to stay upright."),
        ("energy", "Exhaustion drops them mid-action — knees buckle, they collapse and briefly black out, waking groggy."),
        ("hygiene", "They can smell themselves — grimy and sour, self-conscious and embarrassed; the stink just sits on them."),
        ("comfort", "The strain becomes unbearable — they have to shift, break contact, or otherwise ease it; they can't hold still."),
    ].into_iter().map(|(k,v)| (k.to_string(), v.to_string())).collect()
}

pub fn need_post_catastrophe_floor() -> HashMap<String, i32> {
    [("hunger", 70), ("energy", 65), ("comfort", 60)].into_iter().map(|(k,v)| (k.to_string(), v)).collect()
}

pub const CATASTROPHE_NEEDS: &[&str] = &["energy", "hunger", "comfort", "hygiene"];

// ── Needs (per-character vector) ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Needs {
    /// 0-100 each, key ∈ NEED_KEYS.
    pub vector: HashMap<String, i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_catastrophe: Option<String>,
    /// hygiene crisis acked set is session-global; kept here for serde of the per-char vector only.
    #[serde(default)]
    pub hygiene_crisis_acked: bool,
}

impl Default for Needs {
    fn default() -> Self {
        Self { vector: need_defaults(), pending_catastrophe: None, hygiene_crisis_acked: false }
    }
}

impl Needs {
    pub fn new_with_defaults(defaults: HashMap<String, i32>) -> Self {
        let mut v = need_defaults();
        for (k, val) in defaults { if NEED_KEYS.contains(&k.as_str()) { v.insert(k, val.clamp(0,100)); } }
        Self { vector: v, pending_catastrophe: None, hygiene_crisis_acked: false }
    }

    pub fn get(&self, key: &str) -> i32 { self.vector.get(key).copied().unwrap_or(70) }

    pub fn is_urgent(&self, key: &str) -> bool { self.get(key) <= NEED_URGENT }
    pub fn is_critical(&self, key: &str) -> bool { self.get(key) <= NEED_CRITICAL }
    pub fn is_bottomed(&self, key: &str) -> bool { self.get(key) <= 0 }

    /// Prompt helper: stepped prose, worst-first, pronoun-free (Front Porch `needSteppedText` trimmed).
    pub fn need_state_line(&self, key: &str) -> Option<String> {
        let v = self.get(key);
        if v > 45 { return None; }
        let band = if v <= 0 { 0 } else if v <= 15 { 1 } else if v <= 30 { 2 } else if v <= 45 { 3 } else { 4 };
        let lines: &[&str] = match key {
            "hunger" => &[
                "Doubled over starving — vision swimming, knees weak.",
                "Sharp hunger cramps; light-headed, thoughts drift to food.",
                "Stomach hollow — constant ache, short-tempered.",
                "Steady emptiness; occasionally distracted by hunger.",
            ],
            "energy" => &[
                "Body gives out — collapse, briefly unconscious from exhaustion.",
                "Barely awake — head nodding, speech slow.",
                "Heavy tiredness; every movement takes effort.",
                "Deep weariness — noticeably less animated.",
            ],
            "hygiene" => &[
                "Filthy and overwhelmed — grime strong enough to cause distress.",
                "Genuinely dirty, urge to pull away until a chance to wash.",
                "Persistent grimy feeling — self-conscious.",
                "Starting to feel unkempt — wants to freshen up.",
            ],
            "comfort" => &[
                "Strain unbearable — must shift or break contact immediately.",
                "Strong discomfort — visibly strained.",
                "Persistent discomfort — restless, seeking relief.",
                "Faint unease — slightly uncomfortable.",
            ],
            "social" => &[
                "Overwhelming loneliness — hollow, near breaking.",
                "Painfully isolated — unusually clingy or fragile.",
                "Deep ache for connection — casual chat feels hollow.",
                "Quiet craving for connection — a touch warmer than usual.",
            ],
            "fun" => &[
                "Torturous boredom — liable to do something reckless for stimulation.",
                "Deeply restless — ready to suggest almost anything.",
                "Heavy restlessness — everything feels dull.",
                "Noticeably bored — hoping for a change of pace.",
            ],
            _ => return None,
        };
        let idx = band.min(lines.len().saturating_sub(1));
        Some(lines[idx].to_string())
    }

    /// Injection block for LLM (wardrobeContext-style).
    pub fn needs_context(&self, char_name: &str) -> String {
        let mut parts: Vec<String> = vec![];
        for k in NEED_KEYS {
            if let Some(line) = self.need_state_line(k) {
                let label = match *k { "hunger"=> "Hunger", "energy"=> "Energy", "social"=> "Social", "fun"=> "Fun", "hygiene"=> "Hygiene", "comfort"=> "Comfort", _=>k };
                parts.push(format!("{label}: {line}"));
            }
        }
        if parts.is_empty() { return String::new(); }
        format!("How {char_name} feels right now (needs, 0-100; urgent ≤35, critical ≤20):\n{}\n", parts.join("\n"))
    }

    /// Single-key decay with modifier pipeline (deterministic, mirrors Front Porch `decayedValueFor`).
    pub fn decayed_value_for(&self, key: &str, current: i32, vector: &HashMap<String, i32>, weather_rough: bool, weather_clear: bool) -> i32 {
        let mut decay = need_decay().get(key).copied().unwrap_or(0) as f64;
        // cross-need boosts
        if key == "hunger" && vector.get("energy").copied().unwrap_or(50) <= 30 { decay *= 1.35; }
        if key == "comfort" && vector.get("energy").copied().unwrap_or(50) <= 25 { decay *= 1.25; }
        if key == "social" && vector.get("fun").copied().unwrap_or(50) <= 20 { decay *= 1.4; }
        if key == "comfort" && vector.get("hygiene").copied().unwrap_or(50) <= 20 { /* no, original is bladder<=20 -> comfort; we map hygiene */ decay *= 1.10; }
        // weather (Living Time tiny: comfort 1.25 in rough, fun 0.5 on clear)
        if key == "comfort" && weather_rough { decay *= 1.25; }
        if key == "fun" && weather_clear { decay *= 0.5; }
        let d = decay.round() as i32;
        (current - d).clamp(0, 100)
    }

    /// Tick all needs once (ambient drift). No-ops if `enabled` false handled by caller.
    pub fn tick_decay(&mut self, weather_rough: bool, weather_clear: bool) {
        let snapshot = self.vector.clone();
        for k in NEED_KEYS {
            let cur = snapshot.get(*k).copied().unwrap_or(70);
            let next = self.decayed_value_for(k, cur, &snapshot, weather_rough, weather_clear);
            self.vector.insert(k.to_string(), next);
        }
        self.apply_catastrophe_if_needed();
    }

    /// Apply scene deltas (model-proposed), clamped per-need to depletion cap when negative.
    pub fn apply_scene_impact(&mut self, deltas: &HashMap<String, i32>, strength: i32) {
        for (k, v) in deltas {
            if !NEED_KEYS.contains(&k.as_str()) { continue; }
            let mut dv = *v;
            if dv < 0 {
                let cap = scene_depletion_cap_for(k, strength);
                if dv < -cap { dv = -cap; }
            }
            let cur = self.vector.get(k).copied().unwrap_or(70);
            self.vector.insert(k.clone(), (cur + dv).clamp(0, 100));
        }
        self.apply_catastrophe_if_needed();
    }

    pub fn apply_catastrophe_if_needed(&mut self) {
        if self.pending_catastrophe.is_some() { return; }
        // hygiene ack: if hygiene >0, clear acked
        if self.get("hygiene") > 0 { self.hygiene_crisis_acked = false; }
        let texts = need_catastrophe_text();
        let floors = need_post_catastrophe_floor();
        let mut worst: Option<String> = None;
        let mut worst_val = 1;
        for k in CATASTROPHE_NEEDS {
            if *k == "hygiene" && self.hygiene_crisis_acked { continue; }
            let v = self.get(k);
            if v <= 0 && v < worst_val { worst_val = v; worst = Some(k.to_string()); }
        }
        if let Some(k) = worst {
            self.pending_catastrophe = texts.get(&k).cloned();
            if k == "hygiene" { self.hygiene_crisis_acked = true; }
            if let Some(floor) = floors.get(&k).copied() { self.vector.insert(k, floor); }
        }
    }

    pub fn consume_pending_catastrophe(&mut self) -> Option<String> { self.pending_catastrophe.take() }

    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("vector".into(), serde_json::to_value(&self.vector).unwrap_or_default());
        if let Some(cat) = &self.pending_catastrophe { m.insert("pendingCatastrophe".into(), serde_json::Value::String(cat.clone())); }
        serde_json::Value::Object(m)
    }

    pub fn from_json(raw: &serde_json::Value) -> Self {
        let map = match raw.as_object() { Some(m) => m, None => return Self::default() };
        let vector: HashMap<String, i32> = map.get("vector").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_else(need_defaults);
        let pending_catastrophe = map.get("pendingCatastrophe").and_then(|v| v.as_str()).map(|s| s.to_string());
        Self { vector, pending_catastrophe, hygiene_crisis_acked: false }
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn needs_default_in_range() { let n = Needs::default(); for k in NEED_KEYS { assert!((0..=100).contains(&n.get(k))); } }
    #[test] fn needs_tick_decay_monotonic() { let mut n = Needs::default(); let before = n.get("hunger"); n.tick_decay(false,false); assert!(n.get("hunger") <= before); }
    #[test] fn needs_weather_rough_boosts_comfort() { let mut a = Needs::default(); let mut b = Needs::default(); a.vector.insert("comfort".into(), 70); b.vector.insert("comfort".into(), 70); a.tick_decay(true,false); b.tick_decay(false,false); assert!(a.get("comfort") <= b.get("comfort")); }
    #[test] fn needs_scene_depletion_capped() { let mut n = Needs::default(); n.vector.insert("hunger".into(), 80); let mut d = HashMap::new(); d.insert("hunger".into(), -100); n.apply_scene_impact(&d, 1); assert!(n.get("hunger") >= 68); }
    #[test] fn needs_catastrophe_arms_and_floors() { let mut n = Needs::default(); n.vector.insert("hunger".into(), 0); n.apply_catastrophe_if_needed(); assert!(n.pending_catastrophe.is_some()); assert!(n.get("hunger") >= 60); }
    #[test] fn needs_hygiene_acked_once() { let mut n = Needs::default(); n.vector.insert("hygiene".into(), 0); n.apply_catastrophe_if_needed(); assert!(n.pending_catastrophe.is_some()); n.consume_pending_catastrophe(); n.apply_catastrophe_if_needed(); assert!(n.pending_catastrophe.is_none()); }
    #[test] fn needs_context_empty_when_healthy() { let n = Needs::default(); assert!(n.needs_context("Aria").is_empty()); }
    #[test] fn needs_json_roundtrip() { let n = Needs::default(); let j = n.to_json(); let n2 = Needs::from_json(&j); assert_eq!(n.vector, n2.vector); }
    #[test] fn needs_scene_strength_scales_cap() { assert!(scene_depletion_cap_for("hunger", 5) > scene_depletion_cap_for("hunger", 1)); }
}
