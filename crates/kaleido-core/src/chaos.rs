//! Chaos Mode / Chance Time — pressure + event pool + pending injection.
//!
//! Port of Front Porch AI `lib/services/chat/chaos_mode_service.dart` (AGPL-3.0, reimplemented).
//! Pure: pressure 0-100, grows per turn, roll vs pressure+baseChance triggers event.
//! Event text with {{char}} replaced at apply time. Deterministic, no I/O.

use serde::{Deserialize, Serialize};

pub const CHAOS_BASE_CHANCE: i32 = 5;
pub const CHAOS_GROWTH_PER_TURN: i32 = 5;
pub const CHAOS_PRESSURE_CAP: i32 = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChaosState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub nsfw: bool,
    #[serde(default)]
    pub pressure: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_injection: Option<String>,
    #[serde(default)]
    pub delivered: bool,
}

impl Default for ChaosState {
    fn default() -> Self { Self { enabled: false, nsfw: false, pressure: 0, pending_injection: None, delivered: false } }
}

impl ChaosState {
    pub fn tick(&mut self) {
        if !self.enabled { return; }
        if self.pending_injection.is_some() && !self.delivered { return; }
        self.pressure = (self.pressure + CHAOS_GROWTH_PER_TURN).min(CHAOS_PRESSURE_CAP);
    }
    /// Roll 0-99 vs pressure+baseChance. Returns true if triggered.
    pub fn should_trigger(&self, roll: u32) -> bool {
        if !self.enabled { return false; }
        if self.pending_injection.is_some() && !self.delivered { return false; }
        let threshold = (self.pressure + CHAOS_BASE_CHANCE) as u32;
        roll % 100 < threshold
    }
    /// Pick random event from pool, set pending_injection with {{char}} replaced.
    pub fn arm_event(&mut self, char_name: &str, pool_idx: usize) {
        let pool = chance_pool();
        if pool.is_empty() { return; }
        let raw = pool[pool_idx % pool.len()];
        let text = raw.replace("{{char}}", char_name);
        self.pending_injection = Some(text);
        self.delivered = false;
    }
    pub fn mark_delivered(&mut self) { self.delivered = true; }
    pub fn has_pending(&self) -> bool { self.pending_injection.is_some() && !self.delivered }
    pub fn consume_pending(&mut self) -> Option<String> {
        if !self.has_pending() { return None; }
        self.delivered = true;
        self.pending_injection.clone()
    }
    /// Clear delivered pending on next user turn (regen window preserves).
    pub fn clear_delivered_if_any(&mut self) {
        if self.delivered { self.pending_injection = None; self.delivered = false; self.pressure = 0; }
    }
    pub fn prompt_injection(&self) -> Option<String> {
        if self.has_pending() { self.pending_injection.clone() } else { None }
    }
}

pub fn chance_pool() -> Vec<&'static str> {
    vec![
        "{{char}} just found something valuable they completely forgot they owned",
        "{{char}} stumbled into a crowd of admirers who are totally convinced they are famous",
        "{{char}} received a completely unexpected compliment that made their entire day",
        "An incredibly beautiful view has appeared right where {{char}} is standing",
        "{{char}} accidentally said the perfect thing at the perfect moment",
        "{{char}} urgently needs to use the restroom and there is no good option available",
        "{{char}} just stepped in something extremely unpleasant and is now tracking it everywhere",
        "{{char}} sneezed violently at the absolute worst possible moment",
        "{{char}} knocked something over in the loudest way possible",
        "{{char}} tripped, caught themselves, but everyone absolutely saw it",
        "A bird flew directly into the space {{char}} is in and refuses to leave",
        "A sudden and powerful gust of wind has created a chaotic situation involving {{char}}",
        "An extremely large insect has appeared and is refusing to be dealt with",
        "Something nearby fell over on its own for no apparent reason",
        "{{char}} is absolutely starving and trying very hard not to let it show",
        "{{char}} has a song stuck in their head that keeps making them move involuntarily",
        "{{char}} is desperately trying to stay awake and losing the battle",
        "{{char}} is trying very hard not to react to something extremely funny",
        "{{char}} just got completely soaked by something falling nearby",
        "{{char}} slipped on something wet and went down in slow motion in front of everyone",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn chaos_tick_grows() { let mut s = ChaosState { enabled: true, ..Default::default() }; s.tick(); assert_eq!(s.pressure, 5); }
    #[test] fn chaos_cap() { let mut s = ChaosState { enabled: true, pressure: 98, ..Default::default() }; s.tick(); assert_eq!(s.pressure, 100); }
    #[test] fn chaos_trigger_threshold() { let s = ChaosState { enabled: true, pressure: 10, ..Default::default() }; assert!(s.should_trigger(10)); assert!(!s.should_trigger(20)); }
    #[test] fn chaos_pending_blocks_tick() { let mut s = ChaosState { enabled: true, pending_injection: Some("evt".into()), delivered: false, ..Default::default() }; s.tick(); assert_eq!(s.pressure, 0); }
    #[test] fn chaos_arm_and_consume() { let mut s = ChaosState { enabled: true, ..Default::default() }; s.arm_event("Aria", 0); assert!(s.has_pending()); let t = s.consume_pending().unwrap(); assert!(t.contains("Aria")); assert!(!s.has_pending()); }
    #[test] fn chaos_clear_delivered() { let mut s = ChaosState { enabled: true, pending_injection: Some("evt".into()), delivered: true, pressure: 30, ..Default::default() }; s.clear_delivered_if_any(); assert!(s.pending_injection.is_none()); assert_eq!(s.pressure, 0); }
}
