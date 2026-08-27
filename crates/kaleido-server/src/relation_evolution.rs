//! Relation evolution tracker for the character relationship graph.
//!
//! This module builds two high-level views from the raw `Relationship` list:
//! - `RelationEvolution`: per unique character-pair, aggregated chapter history +
//!   trend (stable/warming/cooling/volatile) by comparing first vs last chapter intensity.
//! - `CharacterProfile`: per-character summary of relation count, chapter appearances,
//!   and key events (first/last mentions across relationships).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A unique character pair (ordered lexicographically by name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CharPair(String, String);

impl CharPair {
    fn new(a: String, b: String) -> Self {
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        Self(from, to)
    }
}

/// High-level relationship evolution for one character pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationEvolution {
    /// The ordered pair of characters (always lex-sorted).
    pub pair: (String, String),
    /// Chapters where any relationship between this pair appears, in order.
    pub chapters: Vec<ChapterRelation>,
    /// Trend summary: stable / warming / cooling / volatile.
    pub trend: String,
}

/// One chapter in the evolution timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterRelation {
    /// Chapter name or number.
    pub chapter: String,
    /// Relation type (e.g. "family", "emotional").
    pub relation_type: String,
    /// Intensity level (1-5).
    pub intensity: i32,
    /// Key evidence keywords or notes.
    pub evidence: Vec<String>,
}

/// Character-level profile derived from the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterProfile {
    /// Character name.
    pub name: String,
    /// Total relationships this character participates in.
    pub relation_count: usize,
    /// Chapters where this character appears in any relationship.
    pub appearance_chapters: Vec<String>,
    /// Key events (first/last mention chapters).
    pub key_events: Vec<String>,
}

impl RelationEvolution {
    /// Builds relation evolutions from a list of relationships.
    ///
    /// Aggregates chapters by pair, then computes trend by comparing first vs last
    /// chapter's intensity (stable/升温/降温/起伏).
    pub fn build_evolution(relationships: &[kaleido_core::graph_store::Relationship]) -> Vec<RelationEvolution> {
        let mut pair_map: HashMap<CharPair, Vec<ChapterRelation>> = HashMap::new();
        for rel in relationships {
            let pair = CharPair::new(rel.from_char.clone(), rel.to_char.clone());
            let intensity = if rel.keywords.len() > 2 { 5 } else if rel.keywords.is_empty() { 2 } else { 3 };
            let ch_rel = ChapterRelation {
                chapter: rel.chapters.first().cloned().unwrap_or_else(|| "unknown".to_string()),
                relation_type: rel.category.clone(),
                intensity,
                evidence: rel.keywords.clone(),
            };
            pair_map.entry(pair).or_default().push(ch_rel);
        }
        let mut evo: Vec<_> = pair_map
            .into_iter()
            .map(|(pair, mut chapters)| {
                chapters.sort_by_key(|c| c.chapter.clone());
                let trend = Self::compute_trend(&chapters);
                RelationEvolution { pair: (pair.0, pair.1), chapters, trend }
            })
            .collect();
        evo.sort_by_key(|e| e.pair.0.clone());
        evo
    }

    fn compute_trend(chapters: &[ChapterRelation]) -> String {
        if chapters.is_empty() {
            return "stable".to_string();
        }
        let first = chapters.first().unwrap();
        let last = chapters.last().unwrap();
        if first.intensity == last.intensity {
            "stable".to_string()
        } else if first.intensity < last.intensity {
            "warming".to_string()
        } else if first.intensity > last.intensity {
            "cooling".to_string()
        } else {
            "volatile".to_string()
        }
    }
}

impl CharacterProfile {
    /// Builds character profiles from a list of relationships.
    pub fn build_profiles(relationships: &[kaleido_core::graph_store::Relationship]) -> Vec<CharacterProfile> {
        let mut by_char: HashMap<String, (usize, HashSet<String>)> = HashMap::new();
        for rel in relationships {
            for ch in &rel.chapters {
                by_char.entry(rel.from_char.clone()).or_default().1.insert(ch.clone());
                by_char.entry(rel.to_char.clone()).or_default().1.insert(ch.clone());
            }
            by_char.entry(rel.from_char.clone()).or_default().0 += 1;
            by_char.entry(rel.to_char.clone()).or_default().0 += 1;
        }
        let mut profiles: Vec<_> = by_char
            .into_iter()
            .map(|(name, (count, appearances))| {
                let mut ap: Vec<String> = appearances.into_iter().collect();
                ap.sort();
                CharacterProfile {
                    name,
                    // 端点计数即关系数（每条关系给两端各 +1，无重复统计），
                    // 修复孤岛遗留 bug：原 count/2 整数除法把单关系端点打成 0。
                    relation_count: count,
                    appearance_chapters: ap,
                    key_events: vec![],
                }
            })
            .collect();
        profiles.sort_by_key(|p| p.name.clone());
        profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaleido_core::graph_store::Relationship;

    fn sample_rel() -> Relationship {
        Relationship {
            id: "r1".into(),
            work_id: "w1".into(),
            from_char: "林".into(),
            to_char: "妖".into(),
            category: "emotional".into(),
            subtype: "".into(),
            keywords: vec!["羁绊".into(), "救赎".into(), "并肩".into()],
            confirmation_status: "confirmed".into(),
            note: "".into(),
            chapters: vec!["第1章".into(), "第3章".into()],
            created_at: "".into(),
            updated_at: "".into(),
        }
    }

    #[test]
    fn builds_evolution() {
        let evos = RelationEvolution::build_evolution(&[sample_rel()]);
        assert_eq!(evos.len(), 1);
        // "妖"(U+5996) < "林"(U+6797) by codepoint — lex sort keeps 妖 first
        assert_eq!(evos[0].pair, ("妖".to_string(), "林".to_string()));
        assert_eq!(evos[0].trend, "stable");
        assert_eq!(evos[0].chapters[0].intensity, 5); // 3 keywords > 2
    }

    #[test]
    fn pair_is_lex_sorted() {
        let rel = Relationship {
            from_char: "妖".into(),
            to_char: "林".into(),
            ..sample_rel()
        };
        let evos = RelationEvolution::build_evolution(&[rel]);
        assert_eq!(evos[0].pair.0, "妖");
    }

    #[test]
    fn builds_profiles() {
        let profiles = CharacterProfile::build_profiles(&[sample_rel()]);
        assert_eq!(profiles.len(), 2);
        let lin = profiles.iter().find(|p| p.name == "林").unwrap();
        assert_eq!(lin.relation_count, 1); // 1 rel → 端点计数 1（修复孤岛遗留 count/2 bug）
        assert!(lin.appearance_chapters.contains(&"第1章".to_string()));
    }

    #[test]
    fn trend_cooling() {
        let mut rel = sample_rel();
        rel.chapters = vec!["第1章".into()];
        let rel2 = Relationship {
            id: "r2".into(),
            keywords: vec![],
            chapters: vec!["第5章".into()],
            ..sample_rel()
        };
        let evos = RelationEvolution::build_evolution(&[rel, rel2]);
        assert_eq!(evos[0].trend, "cooling"); // 5 → 2
    }
}
