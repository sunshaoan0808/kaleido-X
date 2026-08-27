//! 角色档案系统（吸收自 SoulLink——SillyTavern 角色扮演辅助插件的档案模块）。
//!
//! 对话驱动的增量档案维护：AI 每轮从近期对话分析角色变化，
//! 输出 fields 覆盖 + add/remove/update diff，本模块应用 diff 到存档。
//! 档案分两类：标量字段（name/age/gender/occupation，直接覆盖）
//! 与列表分节（personality/worldview/family/relationships/memory，增量维护）。
//!
//! 纯函数层：无 IO、无 LLM 调用，全部可单测。
//! 对齐 SoulLink `js/archive-analysis.js` 的 applyArchiveDiff / applyArchiveRefine。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 标量字段键（与 SoulLink ARCHIVE_SCALAR_FIELDS 对齐）。
pub const ARCHIVE_SCALAR_FIELDS: [&str; 4] = ["name", "age", "gender", "occupation"];

/// 标量字段中文标签（UI/日志用）。
pub const ARCHIVE_SCALAR_LABELS: [(&str, &str); 4] = [
    ("name", "姓名"),
    ("age", "年龄"),
    ("gender", "性别"),
    ("occupation", "职业"),
];

/// 列表分节（与 SoulLink ARCHIVE_SECTIONS 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArchiveSection {
    Personality,
    Worldview,
    Family,
    Relationships,
    Memory,
}

impl ArchiveSection {
    pub fn key(&self) -> &'static str {
        match self {
            ArchiveSection::Personality => "personality",
            ArchiveSection::Worldview => "worldview",
            ArchiveSection::Family => "family",
            ArchiveSection::Relationships => "relationships",
            ArchiveSection::Memory => "memory",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ArchiveSection::Personality => "性格",
            ArchiveSection::Worldview => "世界观",
            ArchiveSection::Family => "家庭背景",
            ArchiveSection::Relationships => "人际关系",
            ArchiveSection::Memory => "记忆",
        }
    }

    pub fn prefix(&self) -> &'static str {
        match self {
            ArchiveSection::Personality => "p",
            ArchiveSection::Worldview => "w",
            ArchiveSection::Family => "f",
            ArchiveSection::Relationships => "r",
            ArchiveSection::Memory => "m",
        }
    }

    pub const ALL: [ArchiveSection; 5] = [
        ArchiveSection::Personality,
        ArchiveSection::Worldview,
        ArchiveSection::Family,
        ArchiveSection::Relationships,
        ArchiveSection::Memory,
    ];

    pub fn from_key(key: &str) -> Option<ArchiveSection> {
        ArchiveSection::ALL.iter().copied().find(|s| s.key() == key)
    }
}

/// 档案条目（列表分节的一项；source = 来源楼层/消息 id，删楼溯源清理用）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// 角色完整档案。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CharacterArchive {
    /// 标量字段：name/age/gender/occupation（键为 ARCHIVE_SCALAR_FIELDS）。
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    #[serde(default)]
    pub personality: Vec<ArchiveEntry>,
    #[serde(default)]
    pub worldview: Vec<ArchiveEntry>,
    #[serde(default)]
    pub family: Vec<ArchiveEntry>,
    #[serde(default)]
    pub relationships: Vec<ArchiveEntry>,
    #[serde(default)]
    pub memory: Vec<ArchiveEntry>,
}

impl CharacterArchive {
    pub fn new() -> Self {
        Self::default()
    }

    /// 该分节的条目引用（可变）。
    pub fn section_mut(&mut self, section: ArchiveSection) -> &mut Vec<ArchiveEntry> {
        match section {
            ArchiveSection::Personality => &mut self.personality,
            ArchiveSection::Worldview => &mut self.worldview,
            ArchiveSection::Family => &mut self.family,
            ArchiveSection::Relationships => &mut self.relationships,
            ArchiveSection::Memory => &mut self.memory,
        }
    }

    pub fn section(&self, section: ArchiveSection) -> &Vec<ArchiveEntry> {
        match section {
            ArchiveSection::Personality => &self.personality,
            ArchiveSection::Worldview => &self.worldview,
            ArchiveSection::Family => &self.family,
            ArchiveSection::Relationships => &self.relationships,
            ArchiveSection::Memory => &self.memory,
        }
    }

    /// 序列化为 LLM 输入 JSON 的 profile 对象（对齐 SoulLink serializeArchiveForPrompt）：
    /// `{ "fields": {...}, "personality": [{id,content}], ... }`
    pub fn serialize_for_prompt(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        let mut fields = serde_json::Map::new();
        for key in ARCHIVE_SCALAR_FIELDS {
            fields.insert(
                key.to_string(),
                serde_json::Value::String(self.fields.get(key).cloned().unwrap_or_default()),
            );
        }
        obj.insert("fields".into(), serde_json::Value::Object(fields));
        for section in ArchiveSection::ALL {
            let items: Vec<serde_json::Value> = self
                .section(section)
                .iter()
                .map(|e| {
                    serde_json::json!({ "id": e.id, "content": e.content })
                })
                .collect();
            obj.insert(section.key().into(), serde_json::Value::Array(items));
        }
        serde_json::Value::Object(obj)
    }
}

/// 单条 add 输入（LLM 可能给 string 或 {id,content} 对象）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AddItem {
    Plain(String),
    Object { id: Option<String>, content: Option<String> },
}

impl AddItem {
    pub fn content(&self) -> String {
        match self {
            AddItem::Plain(s) => s.trim().to_string(),
            AddItem::Object { content, .. } => content.as_deref().unwrap_or("").trim().to_string(),
        }
    }

    pub fn id(&self) -> Option<String> {
        match self {
            AddItem::Plain(_) => None,
            AddItem::Object { id, .. } => id.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        }
    }
}

/// 单个分节的增量操作（对齐 SoulLink 输出契约的 add/remove/update）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SectionOps {
    #[serde(default)]
    pub add: Vec<AddItem>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub update: Vec<UpdateItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateItem {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

/// LLM 输出的档案 diff（对齐 SoulLink 输出契约）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ArchiveDiff {
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    #[serde(default)]
    pub personality: Option<SectionOps>,
    #[serde(default)]
    pub worldview: Option<SectionOps>,
    #[serde(default)]
    pub family: Option<SectionOps>,
    #[serde(default)]
    pub relationships: Option<SectionOps>,
    #[serde(default)]
    pub memory: Option<SectionOps>,
}

impl ArchiveDiff {
    pub fn section_ops(&self, section: ArchiveSection) -> Option<&SectionOps> {
        match section {
            ArchiveSection::Personality => self.personality.as_ref(),
            ArchiveSection::Worldview => self.worldview.as_ref(),
            ArchiveSection::Family => self.family.as_ref(),
            ArchiveSection::Relationships => self.relationships.as_ref(),
            ArchiveSection::Memory => self.memory.as_ref(),
        }
    }
}

/// 生成分节新条目 id（对齐 SoulLink nextSectionItemId：prefix + max数字+1）。
fn next_section_item_id(section: ArchiveSection, items: &[ArchiveEntry]) -> String {
    let mut max = 0usize;
    for item in items {
        if let Some(digits) = item.id.rsplit(|c: char| !c.is_ascii_digit()).next() {
            if !digits.is_empty() {
                if let Ok(n) = digits.parse::<usize>() {
                    max = max.max(n);
                }
            }
        }
    }
    format!("{}{}", section.prefix(), max + 1)
}

/// 应用 diff 到档案；返回变更描述列表（对齐 SoulLink applyArchiveDiff 的 changes）。
/// `source_floor`：本轮来源楼层/消息 id，新增条目记录溯源。
pub fn apply_diff(archive: &mut CharacterArchive, diff: &ArchiveDiff, source_floor: Option<&str>) -> Vec<String> {
    let mut changes = Vec::new();

    // 标量字段：只覆盖有值且不同的
    for (key, value) in &diff.fields {
        if !ARCHIVE_SCALAR_FIELDS.contains(&key.as_str()) {
            continue;
        }
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        let prev = archive.fields.get(key).cloned().unwrap_or_default();
        if prev != value {
            archive.fields.insert(key.clone(), value.clone());
            let label = ARCHIVE_SCALAR_LABELS
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, l)| *l)
                .unwrap_or(key);
            changes.push(format!("字段「{}」→ {}", label, value));
        }
    }

    // 列表分节
    for section in ArchiveSection::ALL {
        if let Some(ops) = diff.section_ops(section) {
            apply_section_ops(archive, section, ops, &mut changes, source_floor);
        }
    }

    changes
}

/// 应用单个分节的 add/remove/update（对齐 SoulLink applySectionOps）。
fn apply_section_ops(
    archive: &mut CharacterArchive,
    section: ArchiveSection,
    ops: &SectionOps,
    changes: &mut Vec<String>,
    source_floor: Option<&str>,
) {
    let items = archive.section_mut(section);
    let label = section.label();

    // remove：按 id 删除
    let remove_ids: std::collections::HashSet<String> = ops.remove.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let removed_count = items.iter().filter(|it| remove_ids.contains(&it.id)).count();
    if removed_count > 0 {
        changes.push(format!("「{}」移除 {} 条", label, removed_count));
    }
    items.retain(|it| !remove_ids.contains(&it.id));

    // update：按 id 改 content
    let updates: BTreeMap<String, String> = ops
        .update
        .iter()
        .filter_map(|u| {
            let id = u.id.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
            let content = u.content.as_deref().unwrap_or("").trim().to_string();
            Some((id.to_string(), content))
        })
        .collect();
    for item in items.iter_mut() {
        if let Some(content) = updates.get(&item.id) {
            if !content.is_empty() && *content != item.content {
                changes.push(format!("「{}」更新 {}", label, item.id));
                item.content = content.clone();
            }
        }
    }

    // add：去重 content + 生成 id + 溯源
    let mut seen: std::collections::HashSet<String> = items.iter().map(|it| it.content.clone()).collect();
    let mut used_ids: std::collections::HashSet<String> = items.iter().map(|it| it.id.clone()).collect();
    for addition in &ops.add {
        let content = addition.content();
        if content.is_empty() || seen.contains(&content) {
            continue;
        }
        let mut id = None;
        if let Some(candidate) = addition.id() {
            if !used_ids.contains(&candidate) {
                id = Some(candidate);
            }
        }
        let id = id.unwrap_or_else(|| next_section_item_id(section, items));
        let preview: String = content.chars().take(24).collect();
        let mut entry = ArchiveEntry { id, content, source: source_floor.map(str::to_string) };
        // 防 id 冲突（fallback 生成仍撞就继续加后缀）
        let mut guard = 0;
        while used_ids.contains(&entry.id) && guard < 100 {
            entry.id = format!("{}{}", section.prefix(), next_section_item_id(section, items));
            guard += 1;
        }
        used_ids.insert(entry.id.clone());
        seen.insert(entry.content.clone());
        changes.push(format!("「{}」新增 {}", label, preview));
        items.push(entry);
    }
}

/// 应用「档案精编」结果：整体替换各分节（对齐 SoulLink applyArchiveRefine）。
/// 防御：分节缺失保留原内容；按 id 找回旧条目保留 source；合并沿用最早 id。
/// `refined` 为 LLM 返回的完整档案对象：`{ "fields": {...}, "personality": [{id,content}], ... }`
pub fn apply_refine(archive: &mut CharacterArchive, refined: &serde_json::Value) -> Vec<String> {
    let mut changes = Vec::new();
    if !refined.is_object() {
        return changes;
    }

    // fields：原样保留四标量（LLM 不应修改，仅当非空且不同时更新）
    if let Some(fields) = refined.get("fields").and_then(|v| v.as_object()) {
        for key in ARCHIVE_SCALAR_FIELDS {
            if let Some(v) = fields.get(key).and_then(|v| v.as_str()) {
                let value = v.trim().to_string();
                if value.is_empty() {
                    continue;
                }
                let prev = archive.fields.get(key).cloned().unwrap_or_default();
                if prev != value {
                    changes.push(format!("字段「{}」→ {}", key, value));
                    archive.fields.insert(key.to_string(), value);
                }
            }
        }
    }

    // 分节整体替换
    for section in ArchiveSection::ALL {
        let refined_items = refined.get(section.key());
        if refined_items.is_none() {
            continue; // 分节缺失：保留原内容
        }
        let Some(arr) = refined_items.and_then(|v| v.as_array()) else {
            continue;
        };
        let old_items = archive.section(section).clone();
        let old_by_id: BTreeMap<String, ArchiveEntry> =
            old_items.iter().map(|e| (e.id.clone(), e.clone())).collect();
        let mut next: Vec<ArchiveEntry> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut used_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut add_count = 0usize;

        for raw in arr {
            let Some(obj) = raw.as_object() else { continue };
            let content = obj
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if content.is_empty() || seen.contains(&content) {
                continue; // 空内容/重复丢弃
            }
            let mut id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            // 合法 id 且未被占：沿用；否则用旧条目匹配或新生成
            if id.is_empty() || used_ids.contains(&id) {
                id = String::new();
            }
            let mut entry = if id.is_empty() {
                // 按内容找旧条目沿用其 id（保留 source）
                let matched = old_by_id
                    .values()
                    .find(|e| e.content == content)
                    .cloned();
                match matched {
                    Some(mut e) => {
                        e.content = content.clone();
                        e
                    }
                    None => ArchiveEntry {
                        id: next_section_item_id(section, &next),
                        content: content.clone(),
                        source: None,
                    },
                }
            } else {
                // 显式 id：找回旧条目保留 source
                let mut entry = old_by_id.get(&id).cloned().unwrap_or(ArchiveEntry {
                    id: id.clone(),
                    content: content.clone(),
                    source: None,
                });
                entry.id = id.clone();
                entry.content = content.clone();
                entry
            };
            // 防冲突
            let mut guard = 0;
            while used_ids.contains(&entry.id) && guard < 100 {
                entry.id = format!("{}{}", section.prefix(), next_section_item_id(section, &next));
                guard += 1;
            }
            used_ids.insert(entry.id.clone());
            seen.insert(entry.content.clone());
            next.push(entry);
            add_count += 1;
        }
        if next != old_items {
            changes.push(format!("「{}」精编 → {} 条", section.label(), add_count));
            *archive.section_mut(section) = next;
        }
    }

    changes
}

/// 删除来源楼层时清理该来源的档案条目（删楼联动清理）。
/// 返回移除的条目数。
pub fn purge_by_source(archive: &mut CharacterArchive, source: &str) -> usize {
    let mut removed = 0usize;
    for section in ArchiveSection::ALL {
        let items = archive.section_mut(section);
        let before = items.len();
        items.retain(|e| {
            !(e.source.as_deref() == Some(source) && e.source.is_some())
        });
        removed += before - items.len();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_diff(json: &str) -> ArchiveDiff {
        serde_json::from_str(json).expect("diff JSON 应可解析")
    }

    #[test]
    fn scalar_fields_override() {
        let mut a = CharacterArchive::new();
        a.fields.insert("age".into(), "25 岁".into());
        let diff = parse_diff(
            r#"{"fields": {"age": "26 岁", "gender": "男，17岁少年"}}"#,
        );
        let changes = apply_diff(&mut a, &diff, Some("f1"));
        assert_eq!(a.fields.get("age").unwrap(), "26 岁");
        assert_eq!(a.fields.get("gender").unwrap(), "男，17岁少年");
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn scalar_fields_ignore_unknown_and_empty() {
        let mut a = CharacterArchive::new();
        let diff = parse_diff(
            r#"{"fields": {"hobby": "钓鱼", "age": ""}}"#,
        );
        let changes = apply_diff(&mut a, &diff, None);
        assert!(changes.is_empty());
        assert!(a.fields.get("age").is_none());
    }

    #[test]
    fn section_add_dedup_and_id_generation() {
        let mut a = CharacterArchive::new();
        let diff = parse_diff(
            r#"{"memory": {"add": ["9月3日晚，主角当众维护了我。", "9月3日晚，主角当众维护了我。", "9月4日，我为他做了早餐。"]}}"#,
        );
        let changes = apply_diff(&mut a, &diff, Some("f1"));
        assert_eq!(a.memory.len(), 2, "重复内容应去重");
        assert_eq!(a.memory[0].id, "m1");
        assert_eq!(a.memory[1].id, "m2");
        assert_eq!(a.memory[0].source.as_deref(), Some("f1"), "新增条目应溯源");
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn section_remove_update_add_combined() {
        let mut a = CharacterArchive::new();
        a.memory = vec![
            ArchiveEntry { id: "m1".into(), content: "旧记忆1".into(), source: Some("f1".into()) },
            ArchiveEntry { id: "m2".into(), content: "旧记忆2".into(), source: None },
            ArchiveEntry { id: "m3".into(), content: "将被删除".into(), source: Some("f2".into()) },
        ];
        let diff = parse_diff(
            r#"{"memory": {
                "remove": ["m3"],
                "update": [{"id": "m1", "content": "旧记忆1（补充细节）"}],
                "add": ["新记忆3"]
            }}"#,
        );
        let changes = apply_diff(&mut a, &diff, Some("f5"));
        assert_eq!(a.memory.len(), 3);
        assert!(a.memory.iter().all(|e| e.content != "将被删除"), "旧 m3 内容应被删除");
        assert_eq!(a.memory[0].content, "旧记忆1（补充细节）");
        assert_eq!(a.memory[2].content, "新记忆3");
        assert_eq!(a.memory[2].source.as_deref(), Some("f5"));
        // 注：remove 后 add 会复用被删 id（m3）——与 SoulLink nextSectionItemId 行为一致
        assert!(changes.iter().any(|c| c.contains("移除") && c.contains("1 条")));
        assert!(changes.iter().any(|c| c.contains("更新")));
        assert!(changes.iter().any(|c| c.contains("新增")));
    }

    #[test]
    fn empty_diff_no_change() {
        let mut a = CharacterArchive::new();
        let diff = parse_diff(r#"{}"#);
        let changes = apply_diff(&mut a, &diff, None);
        assert!(changes.is_empty());
    }

    #[test]
    fn serialize_for_prompt_shape() {
        let mut a = CharacterArchive::new();
        a.fields.insert("gender".into(), "男性".into());
        a.memory.push(ArchiveEntry { id: "m1".into(), content: "记忆A".into(), source: None });
        let v = a.serialize_for_prompt();
        assert_eq!(v["fields"]["gender"], "男性");
        assert_eq!(v["memory"][0]["content"], "记忆A");
        assert_eq!(v["memory"][0]["id"], "m1");
        assert!(v["personality"].as_array().unwrap().is_empty());
    }

    #[test]
    fn refine_full_replace_preserves_source() {
        let mut a = CharacterArchive::new();
        a.fields.insert("name".into(), "林小宇".into());
        a.memory = vec![
            ArchiveEntry { id: "m1".into(), content: "重复事件A".into(), source: Some("f1".into()) },
            ArchiveEntry { id: "m2".into(), content: "重复事件A".into(), source: Some("f2".into()) },
        ];
        let refined = serde_json::json!({
            "fields": {"name": "林小宇", "gender": "男性"},
            "memory": [
                {"id": "m1", "content": "重复事件A（合并）"},
            ]
        });
        let changes = apply_refine(&mut a, &refined);
        assert_eq!(a.memory.len(), 1);
        assert_eq!(a.memory[0].id, "m1", "合并沿用最早 id");
        assert_eq!(a.memory[0].content, "重复事件A（合并）");
        assert_eq!(a.memory[0].source.as_deref(), Some("f1"), "id 保留则 source 保留");
        assert_eq!(a.fields.get("gender").unwrap(), "男性", "精编可补标量");
        assert!(!changes.is_empty());
    }

    #[test]
    fn refine_missing_section_keeps_original() {
        let mut a = CharacterArchive::new();
        a.relationships.push(ArchiveEntry { id: "r1".into(), content: "与母亲：疏离".into(), source: None });
        let refined = serde_json::json!({
            "fields": {},
            "personality": [{"id": "p1", "content": "INTP"}]
        });
        let _changes = apply_refine(&mut a, &refined);
        assert_eq!(a.relationships.len(), 1, "缺失分节保留原内容");
        assert_eq!(a.personality.len(), 1);
        assert_eq!(a.personality[0].id, "p1");
    }

    #[test]
    fn purge_by_source_removes_only_matching() {
        let mut a = CharacterArchive::new();
        a.memory = vec![
            ArchiveEntry { id: "m1".into(), content: "A".into(), source: Some("f1".into()) },
            ArchiveEntry { id: "m2".into(), content: "B".into(), source: Some("f2".into()) },
            ArchiveEntry { id: "m3".into(), content: "C".into(), source: None },
        ];
        let removed = purge_by_source(&mut a, "f1");
        assert_eq!(removed, 1);
        assert_eq!(a.memory.len(), 2);
        assert!(a.memory.iter().all(|e| e.source.as_deref() != Some("f1")));
    }

    #[test]
    fn next_id_skips_existing() {
        let mut a = CharacterArchive::new();
        a.memory.push(ArchiveEntry { id: "m10".into(), content: "x".into(), source: None });
        let diff = parse_diff(r#"{"memory": {"add": ["y"]}}"#);
        apply_diff(&mut a, &diff, None);
        assert_eq!(a.memory[1].id, "m11", "应从已有最大 id 后递增");
    }
}
