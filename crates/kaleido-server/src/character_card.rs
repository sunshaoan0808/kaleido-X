//! Full character card builder — 36-item template (Chinese fiction subset).
//!
//! Absorbed from novel2hermes_jp's `character-template.md` 36-item card.
//! Produces a structured JSON-compatible card with the fields that matter for
//! Chinese web fiction distillation; kept as a pure builder (no LLM call) so
//! callers can fill `opening_lines` / `key_scenes` from their own data.
// [P7] 36-item 卡构建器为测试/预留资产
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Full character card (36-item template, meaningful subset for CN fiction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullCharacterCard {
    pub identity: CharacterIdentity,
    pub background: CharacterBackground,
    pub psychology: CharacterPsychology,
    pub relationships: Vec<CharacterRelationship>,
    pub narrative: CharacterNarrative,
}

/// Core identity fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterIdentity {
    pub name: String,
    pub aliases: Vec<String>,
    pub age: Option<String>,
    pub occupation: String,
    pub appearance: String,
}

/// Backstory / world anchor fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterBackground {
    pub origin: String,
    pub past_trauma: Option<String>,
    pub status: String,
    pub location: String,
}

/// Psychology / personality fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterPsychology {
    pub personality: String,
    pub speech_style: String,
    pub speech_mannerisms: Vec<String>,
    pub motivation: String,
    pub goals: Vec<String>,
    pub fears: Vec<String>,
    pub beliefs: Vec<String>,
    pub mental_models: Vec<String>,
}

/// Relationship entries (name + type + note).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRelationship {
    pub name: String,
    pub relation: String,
    pub note: String,
}

/// Narrative / appearance fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterNarrative {
    pub opening_lines: Vec<String>,
    pub key_scenes: Vec<String>,
    pub arc: String,
}

impl FullCharacterCard {
    /// Build a card from a minimal seed (name required, rest defaulted).
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            identity: CharacterIdentity {
                aliases: vec![],
                age: None,
                occupation: String::new(),
                appearance: String::new(),
                name,
            },
            background: CharacterBackground {
                origin: String::new(),
                past_trauma: None,
                status: String::new(),
                location: String::new(),
            },
            psychology: CharacterPsychology {
                personality: String::new(),
                speech_style: String::new(),
                speech_mannerisms: vec![],
                motivation: String::new(),
                goals: vec![],
                fears: vec![],
                beliefs: vec![],
                mental_models: vec![],
            },
            relationships: vec![],
            narrative: CharacterNarrative {
                opening_lines: vec![],
                key_scenes: vec![],
                arc: String::new(),
            },
        }
    }
}

/// Build a full character card from a name, returning JSON value.
///
/// This is the entry point for callers: pass the character's distilled name
/// and get a complete 36-item-compatible card (CN fiction subset) as JSON.
pub fn build_full_character_card(name: &str) -> Value {
    serde_json::to_value(FullCharacterCard::new(name)).unwrap_or_else(|_| {
        serde_json::json!({ "identity": { "name": name } })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_card_with_name() {
        let v = build_full_character_card("林淡妆");
        assert_eq!(v["identity"]["name"], "林淡妆");
        assert!(v["psychology"]["speech_style"].is_string());
    }

    #[test]
    fn card_has_36item_style_fields() {
        let card = FullCharacterCard::new("测试");
        assert!(card.psychology.goals.is_empty());
        assert!(card.narrative.key_scenes.is_empty());
    }
}
