//! Catbox.moe external illustration parsing from ST character cards.
//!
//! SillyTavern cards reference external images hosted on catbox.moe via
//! `<img>filename.png</img>` tags in world book entries / card text fields.
//! Filenames follow `描述性名称HASH.png` where HASH is the catbox file ID
//! (5-8 lowercase alphanumeric chars).
//!
//! 吞噬自 tavern-card-distiller scripts/generate_skill.py
//! `extract_catbox_illustrations` (L147-200).

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::StCardData;

/// One catbox illustration reference parsed from `<img>...</img>` tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CatboxIllustration {
    /// Descriptive scene name (filename minus trailing hash + `.png`).
    pub scene: String,
    /// Full filename, e.g. `神宫寺教室3cwenp.png`.
    pub file: String,
    /// Catbox file ID (hash portion).
    pub hash: String,
}

/// Case-insensitive `<img>(.*?)</img>` matcher.
fn img_regex() -> Regex {
    // `(?is)` — i = case-insensitive, s = dot-matches-newline so multi-line
    // world book entries are covered.
    Regex::new(r"(?is)<img>(.*?)</img>").expect("static img regex")
}

/// `描述性名称HASH.png` — lazy name prefix + greedy 5-8 char hash suffix.
fn hash_regex() -> Regex {
    Regex::new(r"(?i)^(.+?)([a-z0-9]{5,8})\.png$").expect("static hash regex")
}

/// Extract catbox illustration references from a card's world book entries and
/// text fields (`description` / `first_mes` / `mes_example` / `scenario`).
///
/// Source order (matters for `scene` naming): world book entries (entry
/// name/comment as scene context) → description → first_mes → mes_example →
/// scenario. Non-`.png` files and names without a trailing hash are skipped;
/// results deduped by hash.
pub fn extract_catbox_illustrations(card: &StCardData) -> Vec<CatboxIllustration> {
    let img = img_regex();
    let hash_pat = hash_regex();

    let mut sources: Vec<(String, String)> = Vec::new();
    for entry in &card.world_book {
        let content = entry
            .get("content")
            .or_else(|| entry.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if content.trim().is_empty() {
            continue;
        }
        let ctx = entry
            .get("name")
            .or_else(|| entry.get("comment"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        sources.push((ctx.to_string(), content.to_string()));
    }
    for (field, val) in [
        ("description", &card.description),
        ("first_mes", &card.first_mes),
        ("mes_example", &card.mes_example),
        ("scenario", &card.scenario),
    ] {
        if !val.trim().is_empty() {
            sources.push((field.to_string(), val.clone()));
        }
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_source, text) in &sources {
        for caps in img.captures_iter(text) {
            let filename = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if filename.is_empty() || !filename.to_ascii_lowercase().ends_with(".png") {
                continue;
            }
            let Some(hash_match) = hash_pat.captures(filename) else {
                continue;
            };
            let scene = hash_match.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            let hash = hash_match.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            if !seen.insert(hash.clone()) {
                continue;
            }
            out.push(CatboxIllustration {
                scene,
                file: filename.to_string(),
                hash,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn card_with(world_book: Vec<Value>, desc: &str, first_mes: &str) -> StCardData {
        StCardData {
            name: "X7B".into(),
            description: desc.into(),
            first_mes: first_mes.into(),
            world_book,
            ..Default::default()
        }
    }

    #[test]
    fn basic_img_tag() {
        let card = card_with(
            vec![json!({
                "name": "教室",
                "content": "窗外传来嘈杂声 <img>神宫寺教室3cwenp.png</img>"
            })],
            "",
            "",
        );
        let imgs = extract_catbox_illustrations(&card);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].scene, "神宫寺教室");
        assert_eq!(imgs[0].file, "神宫寺教室3cwenp.png");
        assert_eq!(imgs[0].hash, "3cwenp");
    }

    #[test]
    fn non_png_skipped() {
        let card = card_with(
            vec![json!({
                "content": "一张 <img>神宫寺教室3cwenp.jpg</img> 和 <img>乱码abc12345.webp</img>"
            })],
            "",
            "",
        );
        assert!(extract_catbox_illustrations(&card).is_empty());
    }

    #[test]
    fn no_hash_skipped() {
        let card = card_with(vec![json!({"content": "<img>纯描述名字.png</img>"})], "", "");
        assert!(extract_catbox_illustrations(&card).is_empty());
    }

    #[test]
    fn dedup_by_hash() {
        let card = card_with(
            vec![
                json!({"content": "<img>教室甲3cwenp.png</img>"}),
                json!({"content": "<img>教室乙3cwenp.png</img>"}),
            ],
            "",
            "",
        );
        let imgs = extract_catbox_illustrations(&card);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].file, "教室甲3cwenp.png");
    }

    #[test]
    fn empty_card_empty_result() {
        assert!(extract_catbox_illustrations(&StCardData::default()).is_empty());
    }

    #[test]
    fn multi_source_aggregation() {
        // world_book entry → description → first_mes all contribute.
        let card = StCardData {
            name: "X7B".into(),
            description: "<img>高坂樱月邂逅a1b2c3.png</img>".into(),
            first_mes: "*推开门* <img>后藤冬课堂q1w2e3.png</img>".into(),
            scenario: "<img>和琪由希神社x9y8z7.png</img>".into(),
            world_book: vec![json!({
                "comment": "豆咪",
                "content": "小巷里 <img>豆咪夜晚u7v8w9.png</img>"
            })],
            ..Default::default()
        };
        let imgs = extract_catbox_illustrations(&card);
        assert_eq!(imgs.len(), 4);
        let files: Vec<&str> = imgs.iter().map(|i| i.file.as_str()).collect();
        assert!(files.contains(&"高坂樱月邂逅a1b2c3.png"));
        assert!(files.contains(&"后藤冬课堂q1w2e3.png"));
        assert!(files.contains(&"和琪由希神社x9y8z7.png"));
        assert!(files.contains(&"豆咪夜晚u7v8w9.png"));
    }
}
