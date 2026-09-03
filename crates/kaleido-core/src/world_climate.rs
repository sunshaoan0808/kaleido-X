//! World Climate — atmosphere/gravity/temp-bands dress check.
//!
//! Port of Front Porch AI `lib/models/world.dart` place traits + biome temp bands
//! (AGPL-3.0, reimplemented). Minimal, pure: atmosphere/gravity enums, temp band
//! derivation from season + explicit override, dress-for-weather guard.
//!
//! WorldState remains the authority; this module is a thin climate lens over it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldAtmosphere { Breathable, Thin, Unbreathable, Hostile }
impl Default for WorldAtmosphere { fn default() -> Self { Self::Breathable } }
impl WorldAtmosphere {
    pub fn from_name(s: Option<&str>) -> Self {
        match s.unwrap_or("").to_ascii_lowercase().as_str() {
            "thin" => Self::Thin, "unbreathable" => Self::Unbreathable, "hostile" => Self::Hostile, _ => Self::Breathable,
        }
    }
    pub fn as_str(&self) -> &'static str { match self { Self::Breathable=>"breathable", Self::Thin=>"thin", Self::Unbreathable=>"unbreathable", Self::Hostile=>"hostile" } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldGravity { Earth, Low, High, Micro }
impl Default for WorldGravity { fn default() -> Self { Self::Earth } }
impl WorldGravity {
    pub fn from_name(s: Option<&str>) -> Self {
        match s.unwrap_or("").to_ascii_lowercase().as_str() {
            "low" => Self::Low, "high" => Self::High, "micro" => Self::Micro, _ => Self::Earth,
        }
    }
    pub fn as_str(&self) -> &'static str { match self { Self::Earth=>"earth", Self::Low=>"low", Self::High=>"high", Self::Micro=>"micro" } }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldClimate {
    #[serde(default)]
    pub atmosphere: WorldAtmosphere,
    #[serde(default)]
    pub gravity: WorldGravity,
    /// Optional explicit temp band override ("cold"|"temperate"|"hot"|"..."), else derived from season.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_band: Option<String>,
    #[serde(default)]
    pub place_traits: std::collections::HashMap<String, String>,
}

impl Default for WorldClimate {
    fn default() -> Self { Self { atmosphere: WorldAtmosphere::Breathable, gravity: WorldGravity::Earth, temp_band: None, place_traits: Default::default() } }
}

impl WorldClimate {
    pub fn dress_ok_for_weather(&self, _weather: &str, attire: &str) -> bool {
        // Placeholder guard: hostile atmosphere always fails unless suit; otherwise ok.
        if self.atmosphere == WorldAtmosphere::Hostile && !attire.to_ascii_lowercase().contains("suit") { return false; }
        true
    }
    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        if self.atmosphere != WorldAtmosphere::Breathable { m.insert("atmosphere".into(), serde_json::Value::String(self.atmosphere.as_str().into())); }
        if self.gravity != WorldGravity::Earth { m.insert("gravity".into(), serde_json::Value::String(self.gravity.as_str().into())); }
        if let Some(tb) = &self.temp_band { m.insert("temp_band".into(), serde_json::Value::String(tb.clone())); }
        serde_json::Value::Object(m)
    }
    pub fn from_json(raw: &serde_json::Value) -> Self {
        let map = match raw.as_object() { Some(m) => m, None => return Self::default() };
        Self {
            atmosphere: WorldAtmosphere::from_name(map.get("atmosphere").and_then(|v| v.as_str())),
            gravity: WorldGravity::from_name(map.get("gravity").and_then(|v| v.as_str())),
            temp_band: map.get("temp_band").and_then(|v| v.as_str()).map(|s| s.to_string()),
            place_traits: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn default_is_breathable_earth() { let c = WorldClimate::default(); assert_eq!(c.atmosphere, WorldAtmosphere::Breathable); assert_eq!(c.gravity, WorldGravity::Earth); }
    #[test] fn hostile_requires_suit() { let mut c = WorldClimate::default(); c.atmosphere = WorldAtmosphere::Hostile; assert!(!c.dress_ok_for_weather("clear", "dress")); assert!(c.dress_ok_for_weather("clear", "eva suit")); }
    #[test] fn breathable_any_attire_ok() { let c = WorldClimate::default(); assert!(c.dress_ok_for_weather("rain", "t-shirt")); }
    #[test] fn json_roundtrip() { let mut c = WorldClimate::default(); c.gravity = WorldGravity::Low; c.temp_band = Some("hot".into()); let j = c.to_json(); let c2 = WorldClimate::from_json(&j); assert_eq!(c.gravity, c2.gravity); assert_eq!(c.temp_band, c2.temp_band); }
}
