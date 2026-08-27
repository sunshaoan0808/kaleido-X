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
