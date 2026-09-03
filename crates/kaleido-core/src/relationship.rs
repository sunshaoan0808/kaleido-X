//! Relationship — Bond/Trust/WithUser + short decay / long check / fixation.
//!
//! Port of Front Porch AI `lib/services/chat/relationship_service.dart` dynamics
//! (AGPL-3.0, reimplemented). Pure, per-character (owner->char). TavernSession stores
//! a map of these; engine ticks them.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bond {
    pub score: i32,       // -300..300
    pub long_score: i32,  // -300..300
    pub trust: i32,       // -100..100
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with_user: Option<bool>,
    #[serde(default)]
    pub spatial_stance: String,
    #[serde(default)]
    pub fixation: String,
    #[serde(default)]
    pub fixation_life: i32,
    #[serde(default)]
    pub turns_since_long_check: i32,
    #[serde(default)]
    pub short_deltas_sum: i32,
    #[serde(default)]
    pub turns_since_decay: i32,
    #[serde(default)]
    pub pending_trust_repair: bool,
}

impl Default for Bond {
    fn default() -> Self { Self { score: 0, long_score: 0, trust: 0, with_user: None, spatial_stance: String::new(), fixation: String::new(), fixation_life: 0, turns_since_long_check: 0, short_deltas_sum: 0, turns_since_decay: 0, pending_trust_repair: false } }
}

impl Bond {
    pub fn apply_delta(&mut self, bond_d: i32, trust_d: i32) -> (Option<i32>, Option<i32>) {
        let prev_tier = crate::relationship_tiers::bond_tier_signed(self.score);
        let prev_trust_tier = crate::relationship_tiers::trust_tier(self.trust);
        self.score = (self.score + bond_d).clamp(-300, 300);
        self.trust = (self.trust + trust_d).clamp(-100, 100);
        self.short_deltas_sum += bond_d;
        self.turns_since_long_check += 1;
        if trust_d <= -20 { self.pending_trust_repair = true; }
        // fixation decay every turn the caller drives
        if self.fixation_life > 0 { self.fixation_life -= 1; if self.fixation_life==0 { self.fixation.clear(); } }
        let new_tier = crate::relationship_tiers::bond_tier_signed(self.score);
        let new_trust_tier = crate::relationship_tiers::trust_tier(self.trust);
        let bond_cross = if new_tier!=prev_tier { Some(new_tier) } else { None };
        let trust_cross = if new_trust_tier!=prev_trust_tier { Some(new_trust_tier) } else { None };
        (bond_cross, trust_cross)
    }
    /// Long check every 3 turns: fold short sum into long.
    pub fn maybe_long_check(&mut self) -> bool {
        if self.turns_since_long_check >= 3 {
            let delta = (self.short_deltas_sum / 3).clamp(-20, 20);
            self.long_score = (self.long_score + delta).clamp(-300, 300);
            self.turns_since_long_check = 0;
            self.short_deltas_sum = 0;
            true
        } else { false }
    }
    pub fn maybe_decay(&mut self) -> bool {
        self.turns_since_decay += 1;
        if self.turns_since_decay >= 10 {
            self.turns_since_decay = 0;
            if self.score != 0 {
                let step = if self.score > 0 { -1 } else { 1 };
                // decay toward neutral by 1
                self.score += step;
                return true;
            }
        }
        false
    }
    pub fn set_fixation(&mut self, topic: &str) {
        let t = topic.trim();
        if t.is_empty() || t.to_ascii_lowercase()=="none" { self.fixation.clear(); self.fixation_life=0; return; }
        if t != self.fixation { self.fixation = t.to_string(); self.fixation_life = 3; }
    }
    pub fn set_spatial(&mut self, v: &str) {
        let l = v.trim().to_ascii_lowercase();
        self.spatial_stance = if l=="none" || l.is_empty() { String::new() } else { v.trim().to_string() };
    }
    pub fn status_line(&self, char_name: &str) -> String {
        let bt = crate::relationship_tiers::bond_tier_label(crate::relationship_tiers::bond_tier_signed(self.score));
        let tt = crate::relationship_tiers::trust_tier_label(crate::relationship_tiers::trust_tier(self.trust));
        let fix = if self.fixation.is_empty() { String::new() } else { format!(" · fixation: {}", self.fixation) };
        let pos = if self.spatial_stance.is_empty() { String::new() } else { format!(" · stance: {}", self.spatial_stance) };
        format!("{char_name}: bond {} ({}) · trust {} ({}){}{}", self.score, bt, self.trust, tt, fix, pos)
    }
}

pub type RelationshipMap = HashMap<String, Bond>;

pub fn relationships_context(map: &RelationshipMap, pack_chars: &[String]) -> String {
    if map.is_empty() { return String::new(); }
    let mut lines = vec!["## 关系羁绊（权威状态）".to_string()];
    for (cid, b) in map {
        let name = pack_chars.iter().find(|n| *n==cid).map(|s| s.as_str()).unwrap_or(cid.as_str());
        lines.push(b.status_line(name));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn bond_clamp() { let mut b = Bond { score: 299, ..Default::default() }; b.apply_delta(10,0); assert_eq!(b.score,300); }
    #[test] fn long_check_folds() { let mut b = Bond { short_deltas_sum: 30, turns_since_long_check: 3, ..Default::default() }; assert!(b.maybe_long_check()); assert_eq!(b.long_score, 10); assert_eq!(b.short_deltas_sum,0); }
    #[test] fn fixation_life() { let mut b = Bond::default(); b.set_fixation("jealousy"); assert_eq!(b.fixation_life,3); b.apply_delta(0,0); assert_eq!(b.fixation_life,2); }
}
