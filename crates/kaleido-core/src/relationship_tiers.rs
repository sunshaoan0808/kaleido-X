//! Relationship tiers — bond/trust tier labels + progress + milestone triggers.
//!
//! Port of Front Porch AI `lib/services/chat/relationship_tiers.dart` +
//! `relationship_service` tier math (AGPL-3.0, reimplemented). Pure, no I/O.

use serde::{Deserialize, Serialize};

pub fn bond_tier(score: i32) -> i32 {
    let a = score.abs();
    if a < 15 { 0 } else if a < 30 { 1 } else if a < 50 { 2 } else if a < 80 { 3 }
    else if a < 120 { 4 } else if a < 160 { 5 } else if a < 200 { 6 }
    else if a < 250 { 7 } else if score >= 0 { 8 } else { -8 }
    // clamp to -10..10 via linear continue for negatives
}

pub fn bond_tier_signed(score: i32) -> i32 {
    let tier = if score >= 0 { bond_tier(score) } else { -bond_tier(-score) };
    tier.clamp(-10, 10)
}

pub fn bond_scale_percent(score: i32) -> f64 {
    let a = score.abs() as f64;
    let (base, target) = bond_band(a as i32);
    if target <= base { 1.0 } else { ((a - base as f64) / (target - base) as f64).clamp(0.0, 1.0) }
}

fn bond_band(abs: i32) -> (i32, i32) {
    if abs < 15 { (0,15) } else if abs < 30 { (15,30) } else if abs < 50 { (30,50) }
    else if abs < 80 { (50,80) } else if abs < 120 { (80,120) } else if abs < 160 { (120,160) }
    else if abs < 200 { (160,200) } else if abs < 250 { (200,250) } else { (250,300) }
}

pub fn bond_tier_label(tier: i32) -> &'static str {
    match tier {
        10 => "Devoted", 9 => "Enamored", 8 => "Inseparable", 7 => "Intimate", 6 => "Close",
        5 => "Amiable", 4 => "Friendly", 3 => "Warm", 2 => "Receptive", 1 => "Cordial",
        0 => "Neutral", -1 => "Reserved", -2 => "Cool", -3 => "Unimpressed", -4 => "Annoyed",
        -5 => "Disliked", -6 => "Hostile", -7 => "Adversarial", -8 => "Disdain", -9 => "Contempt", -10 => "Vitriolic", _ => "Unknown",
    }
}

pub fn trust_tier(level: i32) -> i32 {
    let a = level.abs();
    if a < 10 { 0 } else if a < 25 { 1 } else if a < 45 { 2 } else if a < 70 { 3 } else if a < 100 { 4 } else { 5 }
}

pub fn trust_scale_percent(level: i32) -> f64 {
    let a = level.abs() as f64;
    let (base, target) = trust_band(a as i32);
    if target <= base { 1.0 } else { ((a - base as f64) / (target - base) as f64).clamp(0.0, 1.0) }
}

fn trust_band(abs: i32) -> (i32, i32) {
    if abs < 10 { (0,10) } else if abs < 25 { (10,25) } else if abs < 45 { (25,45) } else if abs < 70 { (45,70) } else { (70,100) }
}

pub fn trust_tier_label(tier: i32) -> &'static str {
    match tier {
        7 => "Blind Trust", 6 => "Deep Trust", 5 => "Strong Trust", 4 => "Trusting", 3 => "Cautious Trust",
        2 => "Wary", 1 => "Guarded", 0 => "Neutral", -1 => "Distrustful", -2 => "Suspicious", _ => "Unknown",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub character: String,
    pub label: String,
    pub kind: String, // "bond" | "trust" | "custom"
    pub tier: i32,
    pub turn: u32,
}

pub fn check_milestone(character: &str, kind: &str, tier: i32, turn: u32, seen: &[Milestone]) -> Option<Milestone> {
    if tier == 0 { return None; }
    if seen.iter().any(|m| m.character == character && m.kind == kind && m.tier == tier) { return None; }
    Some(Milestone { id: uuid::Uuid::new_v4().to_string(), character: character.into(), label: if kind=="bond"{bond_tier_label(tier)} else {trust_tier_label(tier)}.into(), kind: kind.into(), tier, turn })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn bond_label() { assert_eq!(bond_tier_label(0), "Neutral"); assert_eq!(bond_tier_label(10), "Devoted"); }
    #[test] fn bond_scale_mid() { assert!(bond_scale_percent(40) > 0.0 && bond_scale_percent(40) < 1.0); }
    #[test] fn trust_label() { assert_eq!(trust_tier_label(0), "Neutral"); }
    #[test] fn milestone_once() {
        let m = check_milestone("Aria","bond", 3, 10, &[]).unwrap();
        assert!(check_milestone("Aria","bond", 3, 11, &[m]).is_none());
    }
}
