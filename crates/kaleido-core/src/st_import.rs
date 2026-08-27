//! SillyTavern character card → PartnerItem character_card mapping (JSON MVP).
//!
//! Supports:
//! - ST v2 (`spec: chara_card_v2`) / v3 (`spec: chara_card_v3`) envelopes with `data`
//! - Legacy flat cards (`name` / `description` / `personality` at top level)
//!
//! PNG tEXt / base64 `chara` chunk: see `st_png` module.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{compile_partner_markdown, PartnerItem};

/// Error message for bad ST payloads (surface as HTTP 400).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StImportError(pub String);

impl std::fmt::Display for StImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StImportError {}

/// Single V3 card asset / embedded image reference (avatar, emotion sprites,
/// backgrounds...). Serde-default compatible. 吞噬自 tavern-card-distiller
/// extract_card.py V3 `assets` handling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssetRef {
    pub name: String,
    /// "icon", "image", "emotion", "background", ...
    pub r#type: String,
    /// `data:image/<ext>;base64,<b64>` data URI or plain URL.
    pub uri: String,
    /// Inferred from uri: png / jpg / gif / webp.
    pub ext: String,
}

/// Parsed ST card fields used for mapping (normalized).
#[derive(Debug, Clone, Default)]
pub struct StCardData {
    pub name: String,
    pub description: String,
    pub personality: String,
    pub scenario: String,
    pub first_mes: String,
    pub mes_example: String,
    pub creator_notes: String,
    pub system_prompt: String,
    pub post_history_instructions: String,
    pub tags: Vec<String>,
    pub creator: String,
    pub character_version: String,
    pub spec: String,
    pub spec_version: String,
    /// Original data object (or whole card for legacy) for extensions / round-trip.
    pub raw_data: Value,
    /// Embedded lorebook (`data.character_book`) when present.
    pub character_book: Option<Value>,
    /// ST regex scripts from `data.extensions.regex_scripts` (and common aliases).
    pub regex_scripts: Vec<Value>,
    /// Card world book raw entries (`data.world_book.entries[]`), fed to the
    /// `import_card_world_book` pipeline (X6d). Serde-default compatible.
    pub world_book: Vec<Value>,
    /// V3 assets + V2 `extensions.embedded_images`, unified (X7a).
    pub assets: Vec<AssetRef>,
}

fn str_field(obj: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(s) = v.as_str() {
                let t = s.trim();
                if !t.is_empty() {
                    return t.to_string();
                }
            }
        }
    }
    String::new()
}

fn tags_field(obj: &Value) -> Vec<String> {
    obj.get("tags")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Infer image extension from a data URI or URL.
fn infer_ext_from_uri(uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("data:image/") {
        let mime = rest
            .split(|c| c == ';' || c == ',')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        return match mime.as_str() {
            "jpeg" | "jpg" => "jpg".into(),
            "gif" => "gif".into(),
            "webp" => "webp".into(),
            "avif" => "avif".into(),
            _ => "png".into(),
        };
    }
    let last = uri.rsplit('/').next().unwrap_or(uri);
    let lower = last.to_ascii_lowercase();
    for ext in ["png", "jpg", "jpeg", "gif", "webp", "avif"] {
        if lower.ends_with(&format!(".{ext}")) {
            return if ext == "jpeg" { "jpg".into() } else { ext.into() };
        }
    }
    "png".into()
}

/// Build one `AssetRef` from a V3 asset / embedded_images value
/// (object `{name, type/category, uri|url|data}` or bare string).
fn asset_from_value(v: &Value) -> Option<AssetRef> {
    let (name, typ, uri) = match v {
        Value::String(s) => {
            let uri = s.trim().to_string();
            (String::new(), "image".to_string(), uri)
        }
        Value::Object(m) => {
            let name = m
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let typ = m
                .get("type")
                .or_else(|| m.get("category"))
                .and_then(|x| x.as_str())
                .unwrap_or("image")
                .to_string();
            let uri = m
                .get("uri")
                .or_else(|| m.get("url"))
                .or_else(|| m.get("data"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            (name, typ, uri)
        }
        _ => return None,
    };
    if uri.is_empty() {
        return None;
    }
    let ext = infer_ext_from_uri(&uri);
    Some(AssetRef {
        name,
        r#type: typ,
        uri,
        ext,
    })
}

/// Gather V3 `data.assets[]` + V2 `extensions.embedded_images` into unified
/// `Vec<AssetRef>` (deduped by name+uri). 吞噬自 tavern-card-distiller
/// extract_card.py L538-544（V3 assets）.
fn extract_assets(data: &Value) -> Vec<AssetRef> {
    let mut out: Vec<AssetRef> = Vec::new();
    if let Some(arr) = data.get("assets").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(a) = asset_from_value(item) {
                out.push(a);
            }
        }
    }
    if let Some(ext) = data.get("extensions") {
        if let Some(arr) = ext.get("embedded_images").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(a) = asset_from_value(item) {
                    out.push(a);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|a| {
        let key = format!("{}|{}", a.name, a.uri);
        seen.insert(key)
    });
    out
}

/// Parse SillyTavern character card JSON (v2/v3 or legacy flat).
pub fn parse_st_character_card_json(raw: &str) -> Result<StCardData, StImportError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| StImportError(format!("invalid JSON: {e}")))?;
    parse_st_character_card_value(&value)
}

/// Parse from already-deserialized JSON value.
pub fn parse_st_character_card_value(value: &Value) -> Result<StCardData, StImportError> {
    let obj = value
        .as_object()
        .ok_or_else(|| StImportError("ST card must be a JSON object".into()))?;

    let spec = str_field(value, &["spec"]);
    let spec_version = str_field(value, &["spec_version", "specVersion"]);

    // V1 flat keys (无 data / 无 spec 时的兜底分支，吞噬自 distiller normalize_card V1)
    let has_flat_keys = obj.contains_key("char_name")
        || obj.contains_key("char_persona")
        || obj.contains_key("world_scenario")
        || obj.contains_key("char_greeting")
        || obj.contains_key("example_dialogue");

    // v2/v3 envelope: { spec, spec_version, data: { name, ... } }
    let data_val = if let Some(data) = obj.get("data") {
        if !data.is_object() {
            return Err(StImportError("ST card `data` must be an object".into()));
        }
        data.clone()
    } else if obj.contains_key("name")
        || obj.contains_key("description")
        || obj.contains_key("personality")
        || has_flat_keys
    {
        // Legacy flat card or V1 flat card (spec 键存在时也走此分支 — 平铺字段仍可读)
        value.clone()
    } else {
        return Err(StImportError(
            "not a SillyTavern character card (need `data`, spec, or name/char_name)"
                .into(),
        ));
    };

    let name = str_field(&data_val, &["name", "char_name"]);
    if name.is_empty() {
        return Err(StImportError("character card missing name".into()));
    }

    let character_book = extract_character_book(&data_val);
    let regex_scripts = extract_regex_scripts(&data_val);
    let world_book = extract_world_book(&data_val);
    let assets = extract_assets(&data_val);
    let is_v1_flat = spec.is_empty() && (obj.contains_key("char_name") || obj.contains_key("char_persona"));
    let final_spec = if spec.is_empty() {
        if is_v1_flat {
            "v1".into()
        } else {
            "legacy".into()
        }
    } else {
        spec
    };

    Ok(StCardData {
        name,
        description: str_field(&data_val, &["description", "char_persona"]),
        personality: str_field(&data_val, &["personality"]),
        scenario: str_field(&data_val, &["scenario", "world_scenario"]),
        first_mes: str_field(&data_val, &["first_mes", "first_message", "firstMes", "char_greeting"]),
        mes_example: str_field(&data_val, &["mes_example", "example_dialogue", "mesExample"]),
        creator_notes: str_field(&data_val, &["creator_notes", "creatorNotes", "creatorcomment"]),
        system_prompt: str_field(&data_val, &["system_prompt", "systemPrompt"]),
        post_history_instructions: str_field(
            &data_val,
            &["post_history_instructions", "postHistoryInstructions"],
        ),
        tags: tags_field(&data_val),
        creator: str_field(&data_val, &["creator"]),
        character_version: str_field(&data_val, &["character_version", "characterVersion"]),
        spec: final_spec,
        spec_version,
        raw_data: data_val,
        character_book,
        regex_scripts,
        world_book,
        assets,
    })
}

/// Decode embedded `data:image/...;base64,<b64>` assets into bytes.
/// Returns `(asset name, decoded bytes)`; plain URLs / non-base64 URIs are
/// skipped. 吞噬自 tavern-card-distiller extract_card.py save_embedded_images L569-625.
pub fn extract_embedded_images(card: &StCardData) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for (i, asset) in card.assets.iter().enumerate() {
        let uri = asset.uri.trim();
        if !uri.starts_with("data:image/") {
            continue; // pure URL — skip (download is out of scope here)
        }
        let Some(comma) = uri.find(',') else {
            continue;
        };
        let b64 = &uri[comma + 1..];
        let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let Ok(bytes) = B64.decode(cleaned.as_bytes()) else {
            continue;
        };
        let name = if asset.name.trim().is_empty() {
            format!("image_{i}")
        } else {
            asset.name.clone()
        };
        out.push((name, bytes));
    }
    out
}

/// Raw card world book entries from `data.world_book` (object-with-entries or array).
fn extract_world_book(data: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for key in ["world_book", "worldBook"] {
        let Some(book) = data.get(key) else {
            continue;
        };
        if let Some(arr) = book.get("entries").and_then(|e| e.as_array()) {
            out.extend(arr.iter().cloned());
        } else if let Some(arr) = book.as_array() {
            out.extend(arr.iter().cloned());
        }
    }
    out
}

fn extract_character_book(data: &Value) -> Option<Value> {
    // ST v2/v3: data.character_book
    if let Some(book) = data.get("character_book").or_else(|| data.get("characterBook")) {
        if book.is_object() {
            // only keep if it has entries or name
            let has_entries = book
                .get("entries")
                .and_then(|e| e.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            let has_name = book
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if has_entries || has_name {
                return Some(book.clone());
            }
        }
    }
    // some cards nest under extensions.character_book
    if let Some(ext) = data.get("extensions") {
        if let Some(book) = ext.get("character_book").or_else(|| ext.get("characterBook")) {
            if book.is_object() {
                return Some(book.clone());
            }
        }
    }
    None
}

fn extract_regex_scripts(data: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let push_arr = |src: Option<&Value>, out: &mut Vec<Value>| {
        if let Some(arr) = src.and_then(|v| v.as_array()) {
            for item in arr {
                if item.is_object() {
                    out.push(item.clone());
                }
            }
        }
    };
    if let Some(ext) = data.get("extensions") {
        push_arr(
            ext.get("regex_scripts")
                .or_else(|| ext.get("regexScripts"))
                .or_else(|| ext.get("regex")),
            &mut out,
        );
        // Tavern Helper / common alt
        if let Some(regex) = ext.get("regex_scripts").or_else(|| ext.get("regexScripts")) {
            if regex.is_object() {
                // map form — take values
                if let Some(map) = regex.as_object() {
                    for (_k, v) in map {
                        if v.is_object() {
                            out.push(v.clone());
                        }
                    }
                }
            }
        }
    }
    // top-level rare
    push_arr(
        data.get("regex_scripts")
            .or_else(|| data.get("regexScripts")),
        &mut out,
    );
    // dedupe by scriptName+findRegex
    let mut seen = std::collections::HashSet::new();
    out.retain(|v| {
        let key = format!(
            "{}|{}",
            v.get("scriptName")
                .or_else(|| v.get("name"))
                .and_then(|x| x.as_str())
                .unwrap_or(""),
            v.get("findRegex")
                .or_else(|| v.get("find_regex"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
        );
        seen.insert(key)
    });
    out
}

/// Count enabled lore entries inside a character_book value.
pub fn character_book_entry_count(book: &Value) -> usize {
    book.get("entries")
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter(|e| {
                    // ST: enabled default true; disable/disabled false
                    let disabled = e
                        .get("disable")
                        .or_else(|| e.get("disabled"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let enabled = e
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    enabled && !disabled
                })
                .count()
        })
        .unwrap_or(0)
}

fn keys_of_entry(entry: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    for k in ["keys", "key"] {
        if let Some(v) = entry.get(k) {
            if let Some(a) = v.as_array() {
                for x in a {
                    if let Some(s) = x.as_str() {
                        let t = s.trim();
                        if !t.is_empty() {
                            keys.push(t.to_string());
                        }
                    }
                }
            } else if let Some(s) = v.as_str() {
                for part in s.split(',') {
                    let t = part.trim();
                    if !t.is_empty() {
                        keys.push(t.to_string());
                    }
                }
            }
        }
    }
    keys
}

fn entry_content(entry: &Value) -> String {
    str_field(entry, &["content", "entry", "text"])
}

fn entry_name(entry: &Value, idx: usize) -> String {
    let n = str_field(entry, &["name", "comment", "uid"]);
    if n.is_empty() {
        format!("entry-{idx}")
    } else {
        n
    }
}

/// Compile character_book → PartnerItem world_book (content = all enabled entries).
///
/// Kaleido currently injects full world-book markdown (no keyword scan yet), so we
/// materialize every enabled entry so lore actually appears in systemPrompt.
pub fn character_book_to_world_book(card: &StCardData) -> Option<PartnerItem> {
    // X6d: V3 卡片常用 `world_book.entries`；character_book 缺失时回退到它。
    // 两者都无 → 返回 None（无世界书）。
    let book = match &card.character_book {
        Some(b) => b.clone(),
        None => {
            if card.world_book.is_empty() {
                return None;
            }
            json!({ "name": format!("{} · 卡片世界书", card.name), "entries": card.world_book })
        }
    };
    let entries = book
        .get("entries")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() && str_field(&book, &["name"]).is_empty() {
        return None;
    }

    let book_name = {
        let n = str_field(&book, &["name"]);
        if n.is_empty() {
            format!("{} · 角色世界书", card.name)
        } else if n.contains(&card.name) {
            n
        } else {
            format!("{} · {}", card.name, n)
        }
    };

    let mut md = String::new();
    md.push_str(&format!("# {book_name}\n\n"));
    md.push_str(&format!(
        "> 自角色卡「{}」嵌入世界书导入（SillyTavern character_book）\n\n",
        card.name
    ));
    if let Some(desc) = book.get("description").and_then(|d| d.as_str()) {
        let d = desc.trim();
        if !d.is_empty() {
            md.push_str(&format!("## 简介\n{d}\n\n"));
        }
    }

    let mut simplified = Vec::new();
    let mut enabled_n = 0usize;
    for (i, entry) in entries.iter().enumerate() {
        let disabled = entry
            .get("disable")
            .or_else(|| entry.get("disabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let enabled = entry
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if disabled || !enabled {
            continue;
        }
        let content = entry_content(entry);
        if content.trim().is_empty() {
            continue;
        }
        enabled_n += 1;
        let ename = entry_name(entry, i);
        let keys = keys_of_entry(entry);
        let constant = entry
            .get("constant")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        md.push_str(&format!("## {ename}\n"));
        if !keys.is_empty() {
            md.push_str(&format!("- **关键词**: {}\n", keys.join(", ")));
        }
        if constant {
            md.push_str("- **常驻**: 是\n");
        }
        md.push_str(&format!("\n{content}\n\n"));
        simplified.push(json!({
            "name": ename,
            "keys": keys,
            "content": content,
            "constant": constant,
            "order": entry.get("insertion_order").or_else(|| entry.get("order")).cloned().unwrap_or(json!(i)),
        }));
    }

    if enabled_n == 0 && simplified.is_empty() {
        // book present but no usable entries — still create shell so UI shows link
        md.push_str("_（世界书无可用条目内容）_\n");
    }

    let fields = json!({
        "theme": book_name,
        "stCharacterBook": true,
        "stSourceCharacter": card.name,
        "stEntryCount": enabled_n,
        "stEntries": simplified,
        "stBookRaw": book,
        "geography": "",
        "keyScenes": "",
        "culturalFeatures": "",
        "history": "",
        "conflict": "",
    });

    Some(PartnerItem {
        id: String::new(),
        name: book_name,
        item_type: "world_book".into(),
        content: md,
        fields: Some(fields),
        world_book_id: None,
    })
}

/// Full import bundle: character card (+ optional embedded world book + regex meta).
#[derive(Debug, Clone)]
pub struct StImportBundle {
    pub card: StCardData,
    pub character: PartnerItem,
    pub world_book: Option<PartnerItem>,
    pub regex_count: usize,
    pub lore_entry_count: usize,
}

pub fn build_st_import_bundle(
    card: StCardData,
    world_book_id: Option<String>,
) -> StImportBundle {
    let world_book = if world_book_id.is_some() {
        // caller already chose an external book — still extract lore if present
        // but do not auto-create duplicate unless no link provided
        character_book_to_world_book(&card)
    } else {
        character_book_to_world_book(&card)
    };
    let lore_entry_count = card
        .character_book
        .as_ref()
        .map(character_book_entry_count)
        .unwrap_or(0)
        // X6d: 卡片 world_book.entries（V3 卡片常见）也计入 lore 条目
        + card.world_book.len();
    let regex_count = card.regex_scripts.len();
    // If external world_book_id given, prefer linking to it; else link to embedded book after save.
    let mut character = st_card_to_partner_item(&card, world_book_id);
    // stash flags for UI
    if let Some(fields) = character.fields.as_mut() {
        if let Some(obj) = fields.as_object_mut() {
            obj.insert("stHasCharacterBook".into(), json!(card.character_book.is_some()));
            obj.insert("stLoreEntryCount".into(), json!(lore_entry_count));
            obj.insert("stRegexCount".into(), json!(regex_count));
        }
    }
    StImportBundle {
        card,
        character,
        world_book,
        regex_count,
        lore_entry_count,
    }
}

pub fn import_st_character_card_bundle(
    raw: &str,
    world_book_id: Option<String>,
) -> Result<StImportBundle, StImportError> {
    let card = parse_st_character_card_json(raw)?;
    Ok(build_st_import_bundle(card, world_book_id))
}

/// Map ST card fields → partner character_card field bag (camelCase keys).
pub fn st_card_to_partner_fields(card: &StCardData) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(card.name));
    if !card.personality.is_empty() {
        m.insert("externalPersonality".into(), json!(card.personality));
    }
    if !card.description.is_empty() {
        m.insert("backgroundStory".into(), json!(card.description));
    }
    if !card.scenario.is_empty() {
        m.insert("userInteractionModel".into(), json!(card.scenario));
    }
    if !card.first_mes.is_empty() {
        m.insert("typicalReactions".into(), json!(card.first_mes));
    }
    if !card.mes_example.is_empty() {
        m.insert("speakingStyle".into(), json!(card.mes_example));
    }
    if !card.tags.is_empty() {
        m.insert("identityTags".into(), json!(card.tags.join(", ")));
    }
    // Preserve ST provenance for export / debugging
    m.insert(
        "stSource".into(),
        json!({
            "spec": card.spec,
            "specVersion": card.spec_version,
            "creator": card.creator,
            "characterVersion": card.character_version,
            "creatorNotes": card.creator_notes,
            "systemPrompt": card.system_prompt,
            "postHistoryInstructions": card.post_history_instructions,
        }),
    );
    if !card.system_prompt.is_empty() {
        m.insert("stSystemPrompt".into(), json!(card.system_prompt));
    }
    if !card.post_history_instructions.is_empty() {
        m.insert(
            "stPostHistoryInstructions".into(),
            json!(card.post_history_instructions),
        );
    }
    // Regex scripts — applied client-side on bubble render; kept for round-trip.
    if !card.regex_scripts.is_empty() {
        m.insert("stRegexScripts".into(), json!(card.regex_scripts));
        m.insert("stRegexCount".into(), json!(card.regex_scripts.len()));
    }
    if card.character_book.is_some() {
        m.insert("stHasCharacterBook".into(), json!(true));
        m.insert(
            "stLoreEntryCount".into(),
            json!(character_book_entry_count(card.character_book.as_ref().unwrap())),
        );
    }
    Value::Object(m)
}

/// Convert parsed ST card → PartnerItem ready for `PartnerStore::upsert_character_card`.
///
/// `id` empty → store assigns `cc-{uuid}`. Optional `world_book_id` link.
pub fn st_card_to_partner_item(
    card: &StCardData,
    world_book_id: Option<String>,
) -> PartnerItem {
    let fields = st_card_to_partner_fields(card);
    let mut content = compile_partner_markdown(&card.name, "character_card", &fields);
    if !card.system_prompt.trim().is_empty() {
        content.push_str(&format!(
            "\n\n## ST System Prompt\n{}\n",
            card.system_prompt.trim()
        ));
    }
    if !card.post_history_instructions.trim().is_empty() {
        content.push_str(&format!(
            "\n\n## ST Post-History Instructions\n{}\n",
            card.post_history_instructions.trim()
        ));
    }
    if !card.regex_scripts.is_empty() {
        content.push_str(&format!(
            "\n\n## ST Regex Scripts\n_已导入 {} 条正则（客户端渲染时应用）_\n",
            card.regex_scripts.len()
        ));
    }
    PartnerItem {
        id: String::new(),
        name: card.name.clone(),
        item_type: "character_card".into(),
        content,
        fields: Some(fields),
        world_book_id,
    }
}

/// One-shot: parse JSON string → PartnerItem.
pub fn import_st_character_card_json(
    raw: &str,
    world_book_id: Option<String>,
) -> Result<PartnerItem, StImportError> {
    let card = parse_st_character_card_json(raw)?;
    Ok(st_card_to_partner_item(&card, world_book_id))
}

/// Helper used by tests / future keygen of stable demo ids (not used by upsert).
#[allow(dead_code)]
pub fn new_character_card_id() -> String {
    format!("cc-{}", Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2_SAMPLE: &str = r#"{
      "spec": "chara_card_v2",
      "spec_version": "2.0",
      "data": {
        "name": "Aria Nightwind",
        "description": "A wandering mage from the northern isles.",
        "personality": "Calm, dry humor, fiercely loyal.",
        "scenario": "You meet in a storm-battered tavern.",
        "first_mes": "The door slams. *Aria shakes rain from her cloak.* \"Need a table?\"",
        "mes_example": "{{user}}: Hello\n{{char}}: *nods* Speak.",
        "tags": ["fantasy", "mage"],
        "creator": "kaleido-fixture",
        "character_version": "1.0",
        "creator_notes": "fixture for t7-st-import",
        "system_prompt": "",
        "post_history_instructions": "",
        "alternate_greetings": [],
        "extensions": {}
      }
    }"#;

    const LEGACY_SAMPLE: &str = r#"{
      "name": "Legacy Hero",
      "description": "Old format card",
      "personality": "Brave"
    }"#;

    #[test]
    fn parse_v2_card() {
        let card = parse_st_character_card_json(V2_SAMPLE).unwrap();
        assert_eq!(card.name, "Aria Nightwind");
        assert_eq!(card.spec, "chara_card_v2");
        assert!(card.personality.contains("Calm"));
        assert_eq!(card.tags, vec!["fantasy", "mage"]);
    }

    #[test]
    fn parse_legacy_card() {
        let card = parse_st_character_card_json(LEGACY_SAMPLE).unwrap();
        assert_eq!(card.name, "Legacy Hero");
        assert_eq!(card.spec, "legacy");
    }

    #[test]
    fn map_to_partner_item() {
        let item = import_st_character_card_json(V2_SAMPLE, None).unwrap();
        assert_eq!(item.item_type, "character_card");
        assert_eq!(item.name, "Aria Nightwind");
        assert!(item.id.is_empty());
        let fields = item.fields.as_ref().unwrap();
        assert_eq!(
            fields.get("externalPersonality").and_then(|v| v.as_str()),
            Some("Calm, dry humor, fiercely loyal.")
        );
        assert_eq!(
            fields.get("backgroundStory").and_then(|v| v.as_str()),
            Some("A wandering mage from the northern isles.")
        );
        assert!(item.content.contains("角色卡：Aria Nightwind"));
        assert!(item.content.contains("性格特征") || item.content.contains("外在性格"));
    }

    #[test]
    fn reject_empty_object() {
        let err = parse_st_character_card_json("{}").unwrap_err();
        assert!(err.0.contains("not a SillyTavern") || err.0.contains("missing"));
    }

    #[test]
    fn reject_missing_name() {
        let err = parse_st_character_card_json(
            r#"{"spec":"chara_card_v2","data":{"description":"x"}}"#,
        )
        .unwrap_err();
        assert!(err.0.contains("name"));
    }

    #[test]
    fn v3_envelope() {
        let raw = r#"{
          "spec": "chara_card_v3",
          "spec_version": "3.0",
          "data": { "name": "V3 Char", "personality": "witty" }
        }"#;
        let card = parse_st_character_card_json(raw).unwrap();
        assert_eq!(card.spec, "chara_card_v3");
        assert_eq!(card.name, "V3 Char");
    }

    #[test]
    fn extract_character_book_and_regex() {
        let raw = r#"{
          "spec": "chara_card_v2",
          "spec_version": "2.0",
          "data": {
            "name": "Lore Char",
            "description": "d",
            "character_book": {
              "name": "LoreBook",
              "entries": [
                {
                  "keys": ["storm", "rain"],
                  "content": "The Black Coast never dries.",
                  "enabled": true,
                  "constant": true,
                  "comment": "weather"
                },
                {
                  "keys": ["skip"],
                  "content": "disabled",
                  "disable": true
                }
              ]
            },
            "extensions": {
              "regex_scripts": [
                {
                  "id": "r1",
                  "scriptName": "hide ooc",
                  "findRegex": "/\\(OOC:.*?\\)/g",
                  "replaceString": "",
                  "placement": [2],
                  "disabled": false
                }
              ]
            }
          }
        }"#;
        let card = parse_st_character_card_json(raw).unwrap();
        assert!(card.character_book.is_some());
        assert_eq!(character_book_entry_count(card.character_book.as_ref().unwrap()), 1);
        assert_eq!(card.regex_scripts.len(), 1);
        let bundle = build_st_import_bundle(card, None);
        let wb = bundle.world_book.expect("world book");
        assert!(wb.content.contains("Black Coast"));
        assert!(wb.content.contains("关键词"));
        assert_eq!(bundle.lore_entry_count, 1);
        assert_eq!(bundle.regex_count, 1);
        let fields = bundle.character.fields.as_ref().unwrap();
        assert_eq!(fields.get("stRegexCount").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn parse_v1_flat_card() {
        let raw = r#"{
          "char_name": "V1 Flat",
          "char_persona": "A flat-era persona.",
          "world_scenario": "A dusty tavern.",
          "char_greeting": "*looks up* Oh, you again.",
          "example_dialogue": "{{char}}: Hello."
        }"#;
        let card = parse_st_character_card_json(raw).unwrap();
        assert_eq!(card.name, "V1 Flat");
        assert_eq!(card.spec, "v1");
        assert_eq!(card.description, "A flat-era persona.");
        assert_eq!(card.scenario, "A dusty tavern.");
        assert_eq!(card.first_mes, "*looks up* Oh, you again.");
        assert_eq!(card.mes_example, "{{char}}: Hello.");
    }

    #[test]
    fn v1_flat_with_spec_goes_spec_branch() {
        // spec 键存在 → 走现有 spec 分支；平铺字段仍映射到 data_val
        let raw = r#"{
          "spec": "chara_card_v2",
          "char_name": "V1+Spec",
          "char_persona": "Still flat, but spec-tagged."
        }"#;
        let card = parse_st_character_card_json(raw).unwrap();
        assert_eq!(card.name, "V1+Spec");
        assert_eq!(card.spec, "chara_card_v2");
        assert_eq!(card.description, "Still flat, but spec-tagged.");
    }

    #[test]
    fn parse_world_book_field() {
        let raw = r#"{
          "spec": "chara_card_v2",
          "data": {
            "name": "WB Char",
            "description": "d",
            "world_book": {
              "name": "CardWorldBook",
              "entries": [
                {
                  "keys": ["storm"],
                  "content": "The coast never dries.",
                  "enabled": true
                },
                {
                  "keys": ["skip"],
                  "content": "disabled entry",
                  "enabled": false
                }
              ]
            }
          }
        }"#;
        let card = parse_st_character_card_json(raw).unwrap();
        assert_eq!(card.world_book.len(), 2);
        assert_eq!(
            card.world_book[0].get("content").and_then(|v| v.as_str()),
            Some("The coast never dries.")
        );
    }

    #[test]
    fn parse_v3_assets() {
        let raw = r#"{
          "spec": "chara_card_v3",
          "spec_version": "3.0",
          "data": {
            "name": "Asset Char",
            "description": "d",
            "assets": [
              { "name": "avatar", "type": "icon", "uri": "data:image/png;base64,UE5H" },
              { "name": "angry", "type": "emotion", "uri": "data:image/webp;base64,UE5H" },
              { "name": "bg", "type": "background", "uri": "https://files.catbox.moe/abcde.png" }
            ]
          }
        }"#;
        let card = parse_st_character_card_json(raw).unwrap();
        assert_eq!(card.assets.len(), 3);
        assert_eq!(card.assets[0].name, "avatar");
        assert_eq!(card.assets[0].r#type, "icon");
        assert_eq!(card.assets[0].ext, "png");
        assert_eq!(card.assets[1].ext, "webp");
        assert_eq!(card.assets[2].ext, "png");
    }

    #[test]
    fn parse_v2_embedded_images_extensions() {
        let raw = r#"{
          "spec": "chara_card_v2",
          "spec_version": "2.0",
          "data": {
            "name": "Emb Char",
            "description": "d",
            "extensions": {
              "embedded_images": [
                { "name": "sprite", "uri": "data:image/gif;base64,UE5H" },
                "data:image/png;base64,UE5H"
              ]
            }
          }
        }"#;
        let card = parse_st_character_card_json(raw).unwrap();
        assert_eq!(card.assets.len(), 2);
        assert_eq!(card.assets[0].name, "sprite");
        assert_eq!(card.assets[0].ext, "gif");
        assert_eq!(card.assets[1].name, "");
        assert_eq!(card.assets[1].ext, "png");
    }

    #[test]
    fn extract_embedded_images_decodes_data_uri() {
        let card = StCardData {
            name: "X7A".into(),
            assets: vec![AssetRef {
                name: "avatar".into(),
                r#type: "icon".into(),
                uri: "data:image/png;base64,UE5H".into(),
                ext: "png".into(),
            }],
            ..Default::default()
        };
        let imgs = extract_embedded_images(&card);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].0, "avatar");
        assert_eq!(imgs[0].1, b"PNG");
    }

    #[test]
    fn extract_embedded_images_skips_pure_url() {
        let card = StCardData {
            name: "X7A".into(),
            assets: vec![
                AssetRef {
                    name: "url".into(),
                    r#type: "image".into(),
                    uri: "https://files.catbox.moe/abcde.png".into(),
                    ext: "png".into(),
                },
                AssetRef {
                    name: "b64".into(),
                    r#type: "image".into(),
                    uri: "not-a-data-uri".into(),
                    ext: "png".into(),
                },
            ],
            ..Default::default()
        };
        let imgs = extract_embedded_images(&card);
        assert!(imgs.is_empty());
    }

    #[test]
    fn no_assets_empty() {
        let card = parse_st_character_card_json(V2_SAMPLE).unwrap();
        assert!(card.assets.is_empty());
        assert!(extract_embedded_images(&card).is_empty());
    }
}
