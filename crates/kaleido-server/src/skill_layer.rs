//! Skill 层（P4 写作 Skill）：运行时装载 novel lite/standard/heavy 三档 SKILL.md 与
//! 分档 stage 模板（plan/write/review/fix/gate/memory），供回合系统提示注入与
//! `run_quality_refine` 模板化 stage prompt 使用。
//!
//! 三层 scope（override 语义，仅显式覆盖）：
//!   1. workspace  `data_root/works/{workspace_id}/.denova/skills/writing/{tier}/`
//!   2. user       `data_root/skills/writing/{tier}/`
//!   3. builtin    `assets/skills/writing/{tier}/`（`include_str!` 内置只读，回退层）
//!
//! 回退链（保证与现状兼容）：
//!   1. 命中 `<layer>/templates/*.md` → 作为 run_quality_refine 的 stage prompt；
//!   2. 无模板 → 回退 `story_tavern.rs` 既有 `QUALITY_*_SYS` const（无模板即维持现状 = 零回归）。
//!
//! 5min 粒度 in-TTL 缓存，纯 std（无新依赖）。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::story_tavern::TurnQuality;

/// 写作子命名空间目录名（`skills/writing/`）。
pub const WRITING_NS: &str = "writing";

/// TTL：5 分钟。
const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 字段用于 scope 透出与 skill 元数据上报；测试读取
pub enum SkillScope {
    Workspace,
    User,
    Builtin,
}

impl SkillScope {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillScope::Workspace => "workspace",
            SkillScope::User => "user",
            SkillScope::Builtin => "builtin",
        }
    }
}

/// 单个写作档位的运行时 Skill 文档：SKILL.md 正文 + 原则级规则摘要 + 分档 stage 模板。
#[derive(Debug, Clone)]
#[allow(dead_code)] // 字段大部分供测试/CLI 检查读取；rules/templates 供生产注入
pub struct SkillDoc {
    pub name: String,
    pub tier: String,
    pub content: String,
    /// 原则级规则摘要（注入系统提示用，避免全文进 prompt）。
    pub rules: String,
    pub templates: SkillTemplates,
    pub scope: SkillScope,
}

/// 分档 stage 系统提示模板（plan/write/review/fix/gate/memory）。
#[derive(Debug, Clone, Default)]
pub struct SkillTemplates {
    pub plan: Option<String>,
    pub write: Option<String>,
    pub review: Option<String>,
    pub fix: Option<String>,
    pub gate: Option<String>,
    pub memory: Option<String>,
}

// ─── 内置只读层（builtin const 回退；assets 编译期嵌入）──────────────────────

const BUILTIN_LITE_SKILL: &str = include_str!("../assets/skills/writing/lite/SKILL.md");
const BUILTIN_LITE_WRITE: &str = include_str!("../assets/skills/writing/lite/templates/write.md");

const BUILTIN_STD_SKILL: &str = include_str!("../assets/skills/writing/standard/SKILL.md");
const BUILTIN_STD_WRITE: &str = include_str!("../assets/skills/writing/standard/templates/write.md");
const BUILTIN_STD_REVIEW: &str = include_str!("../assets/skills/writing/standard/templates/review.md");
const BUILTIN_STD_FIX: &str = include_str!("../assets/skills/writing/standard/templates/fix.md");

const BUILTIN_HEAVY_SKILL: &str = include_str!("../assets/skills/writing/heavy/SKILL.md");
const BUILTIN_HEAVY_PLAN: &str = include_str!("../assets/skills/writing/heavy/templates/plan.md");
const BUILTIN_HEAVY_WRITE: &str = include_str!("../assets/skills/writing/heavy/templates/write.md");
const BUILTIN_HEAVY_REVIEW: &str = include_str!("../assets/skills/writing/heavy/templates/review.md");
const BUILTIN_HEAVY_FIX: &str = include_str!("../assets/skills/writing/heavy/templates/fix.md");
const BUILTIN_HEAVY_GATE: &str = include_str!("../assets/skills/writing/heavy/templates/gate.md");
const BUILTIN_HEAVY_MEMORY: &str = include_str!("../assets/skills/writing/heavy/templates/memory.md");

// ─── 缓存 ─────────────────────────────────────────────────────────────────────

type Cache = Mutex<HashMap<String, (Instant, SkillDoc)>>;

static SKILL_CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();

fn cache_lock() -> &'static Cache {
    SKILL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(data_root: &Path, workspace_id: Option<&str>, tier: &str) -> String {
    format!(
        "{}|{}|{}",
        data_root.display(),
        workspace_id.unwrap_or(""),
        tier
    )
}

fn cache_get(key: &str) -> Option<SkillDoc> {
    if let Ok(cache) = cache_lock().lock() {
        if let Some((at, doc)) = cache.get(key) {
            if at.elapsed() < CACHE_TTL {
                return Some(doc.clone());
            }
        }
    }
    None
}

fn cache_put(key: &str, doc: SkillDoc) {
    if let Ok(mut cache) = cache_lock().lock() {
        cache.insert(key.to_string(), (Instant::now(), doc));
    }
}

/// 测试辅助：清空装载缓存（force reload）。
#[allow(dead_code)]
pub fn clear_skill_cache() {
    if let Ok(mut cache) = cache_lock().lock() {
        cache.clear();
    }
}

// ─── 装载器 ──────────────────────────────────────────────────────────────────

/// TurnQuality → 档位目录名（lite|standard|heavy ↔ SKILL.md 子目录）。
pub fn resolve_tier_for_quality(quality: TurnQuality) -> &'static str {
    match quality {
        TurnQuality::Lite => "lite",
        TurnQuality::Standard => "standard",
        TurnQuality::Heavy => "heavy",
    }
}

/// 运行时装载某档写作 Skill（workspace → user → builtin 三层，遵循 override）。
/// 命中层有 SKILL.md 则连同其 templates/*.md 一并读取；无 SKILL.md 则继续下一层，
/// 最终回退内置 const。任何一层失败仅回退，不抛错。
pub fn load_writing_skill(
    data_root: &Path,
    workspace_id: Option<&str>,
    tier: &str,
) -> Option<SkillDoc> {
    let tier = resolve_tier(tier);
    let key = cache_key(data_root, workspace_id, tier);
    if let Some(doc) = cache_get(&key) {
        return Some(doc);
    }

    // 1. workspace 层
    if let Some(ws) = workspace_id {
        let dir = data_root
            .join("works")
            .join(ws)
            .join(".denova")
            .join("skills")
            .join(WRITING_NS)
            .join(tier);
        if let Some(doc) = load_from_dir(&dir, tier, SkillScope::Workspace) {
            cache_put(&key, doc.clone());
            return Some(doc);
        }
    }

    // 2. user 层
    let user_dir = data_root.join("skills").join(WRITING_NS).join(tier);
    if let Some(doc) = load_from_dir(&user_dir, tier, SkillScope::User) {
        cache_put(&key, doc.clone());
        return Some(doc);
    }

    // 3. builtin 层（内置 const 回退）
    let doc = builtin_doc(tier);
    cache_put(&key, doc.clone());
    Some(doc)
}

fn resolve_tier(tier: &str) -> &'static str {
    match tier.trim().to_ascii_lowercase().as_str() {
        "standard" => "standard",
        "heavy" => "heavy",
        _ => "lite",
    }
}

fn load_from_dir(dir: &Path, tier: &str, scope: SkillScope) -> Option<SkillDoc> {
    let md = dir.join("SKILL.md");
    let content = fs::read_to_string(&md).ok()?;
    let templates = load_templates(dir);
    let name = extract_frontmatter_value(&content, "name")
        .unwrap_or_else(|| format!("novel-{tier}"));
    Some(SkillDoc {
        name,
        tier: tier.to_string(),
        rules: principles(&content),
        content,
        templates,
        scope,
    })
}

fn load_templates(dir: &Path) -> SkillTemplates {
    let tdir = dir.join("templates");
    let read = |name: &str| {
        fs::read_to_string(tdir.join(name))
            .ok()
            .filter(|s| !s.trim().is_empty())
    };
    SkillTemplates {
        plan: read("plan.md"),
        write: read("write.md"),
        review: read("review.md"),
        fix: read("fix.md"),
        gate: read("gate.md"),
        memory: read("memory.md"),
    }
}

fn builtin_doc(tier: &str) -> SkillDoc {
    match tier {
        "standard" => SkillDoc {
            name: "novel-standard".into(),
            tier: "standard".into(),
            rules: principles(BUILTIN_STD_SKILL),
            content: BUILTIN_STD_SKILL.to_string(),
            templates: SkillTemplates {
                write: Some(BUILTIN_STD_WRITE.to_string()),
                review: Some(BUILTIN_STD_REVIEW.to_string()),
                fix: Some(BUILTIN_STD_FIX.to_string()),
                ..Default::default()
            },
            scope: SkillScope::Builtin,
        },
        "heavy" => SkillDoc {
            name: "novel-heavy".into(),
            tier: "heavy".into(),
            rules: principles(BUILTIN_HEAVY_SKILL),
            content: BUILTIN_HEAVY_SKILL.to_string(),
            templates: SkillTemplates {
                plan: Some(BUILTIN_HEAVY_PLAN.to_string()),
                write: Some(BUILTIN_HEAVY_WRITE.to_string()),
                review: Some(BUILTIN_HEAVY_REVIEW.to_string()),
                fix: Some(BUILTIN_HEAVY_FIX.to_string()),
                gate: Some(BUILTIN_HEAVY_GATE.to_string()),
                memory: Some(BUILTIN_HEAVY_MEMORY.to_string()),
            },
            scope: SkillScope::Builtin,
        },
        _ => SkillDoc {
            name: "novel-lite".into(),
            tier: "lite".into(),
            rules: principles(BUILTIN_LITE_SKILL),
            content: BUILTIN_LITE_SKILL.to_string(),
            templates: SkillTemplates {
                write: Some(BUILTIN_LITE_WRITE.to_string()),
                ..Default::default()
            },
            scope: SkillScope::Builtin,
        },
    }
}

// ─── prompt 注入 helper ──────────────────────────────────────────────────────

/// 技能加载调用（模型在叙述中输出的【技能加载】独立块）。
#[derive(Debug, Clone, PartialEq)]
pub struct SkillLoadCall {
    /// 被剥离的整块原文（含标记行）。
    pub raw: String,
}

/// 从叙述文本提取【技能加载】调用块并剥离。块必须独立成段、行首 `【技能加载】`；
/// marker 行内带参数的旧格式（如「【技能加载】载入xxx」）行尾即块尾；marker 独占一行时
/// 继续吸收紧接其后的连续非空行（跳过行尾换行）作为块 payload，直到空行或 EOF。
/// 仿 tavern_mcp::split_mcp_calls_from_narrative 的剥离逻辑：循环 find MARKER，把块从
/// 文本移除（含块后的换行），其余原样保留。正文中出现的「技能加载」字样（非独立段）不误伤。
pub fn split_skill_load_calls_from_narrative(text: &str) -> (String, Vec<SkillLoadCall>) {
    const MARKER: &str = "【技能加载】";
    let mut calls = Vec::new();
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(MARKER) {
        // 独立块必须是行首：文本开头或前一字符为换行，否则视为正文原样保留。
        let line_start = pos == 0 || rest.as_bytes()[pos - 1] == b'\n';
        if !line_start {
            clean.push_str(&rest[..pos + MARKER.len()]);
            rest = &rest[pos + MARKER.len()..];
            continue;
        }
        // 块尾即行尾（旧格式：marker 行内直接带参数，不支持换行 payload）。
        let line_end = rest[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(rest.len());
        let marker_line = &rest[pos..line_end];
        let inline_payload = marker_line[MARKER.len()..].trim();
        let mut block_end = line_end;
        if inline_payload.is_empty() {
            // marker 独占一行：吸收紧接其后的"结构化 payload 行"作为块 payload，
            // 直到遇到空行/EOF 或普通叙述行（避免把块后正文吞掉）。
            let mut cursor = line_end;
            if cursor < rest.len() {
                cursor += 1; // 跳过 marker 行的行尾换行
            }
            let mut payload: Vec<&str> = Vec::new();
            while cursor < rest.len() {
                let line = &rest[cursor..];
                let nl = line.find('\n');
                let (content, next_cursor) = match nl {
                    Some(i) => (&line[..i], cursor + i + 1),
                    None => (line, rest.len()),
                };
                if content.trim().is_empty() {
                    break;
                }
                // 结构化 payload 特征：缩进 / 列表 / 引用 / 标题 / `key: value`（ASCII 冒号）。
                // 中文叙述行（如"他推开了门。"）不是 payload，终止吸收以保护正文。
                let t = content.trim_start();
                let structured = content.starts_with(' ') || content.starts_with('\t')
                    || t.starts_with('-') || t.starts_with('*') || t.starts_with('>')
                    || t.starts_with('#')
                    || t.split_once(':')
                        .map(|(k, _)| !k.is_empty() && k.len() <= 24)
                        .unwrap_or(false);
                if !structured {
                    break;
                }
                payload.push(content.trim());
                block_end = cursor + content.len();
                cursor = next_cursor;
            }
            let mut raw = marker_line.to_string();
            if !payload.is_empty() {
                raw.push('\n');
                raw.push_str(&payload.join("\n"));
            }
            calls.push(SkillLoadCall { raw });
        } else {
            calls.push(SkillLoadCall {
                raw: marker_line.to_string(),
            });
        }
        clean.push_str(&rest[..pos]);
        rest = &rest[block_end..];
        // 剥掉块尾紧邻的换行，避免正文留下空行。
        if rest.starts_with('\n') {
            rest = &rest[1..];
        }
    }
    clean.push_str(rest);
    (clean, calls)
}

/// 组装「完整 SKILL.md 注入文本」：doc.content（SKILL.md 全文）+ 各分档 stage 模板段。
/// 输出形如：
/// ```text
/// ## 完整写作 Skill（tier）
/// <content>
/// ## 模板
/// ### plan
/// <plan 或 (无)>
/// ### write / review / fix / gate / memory 同法
/// ```
pub fn skill_full_markdown(doc: &SkillDoc) -> String {
    let mut out = String::new();
    out.push_str(&format!("## 完整写作 Skill（{}）\n", doc.tier));
    out.push_str(doc.content.trim());
    out.push_str("\n\n## 模板\n");
    let mut push = |name: &str, tpl: &Option<String>| {
        out.push_str(&format!(
            "\n### {}\n{}\n",
            name,
            tpl.as_deref().unwrap_or("(无)").trim()
        ));
    };
    push("plan", &doc.templates.plan);
    push("write", &doc.templates.write);
    push("review", &doc.templates.review);
    push("fix", &doc.templates.fix);
    push("gate", &doc.templates.gate);
    push("memory", &doc.templates.memory);
    out
}

/// 追加一行「Writing Skill 按需加载提示」+（非 lite 且命中规则时）原则级写作规则。
/// 不预注入全文、控制 token；lite 只给提示不给规则（零回归）。
pub fn append_writing_skill_hint(
    system_prompt: &str,
    tier: &str,
    skill: Option<&SkillDoc>,
) -> String {
    let tier_label = match resolve_tier(tier) {
        "standard" => "standard（初稿 → 审稿 → 修订）",
        "heavy" => "heavy（规划 → 写作 → 审稿 → 修订 → 终检 → 状态补丁）",
        _ => "lite（单次直出，不审不改）",
    };
    let mut out = format!(
        "{}\n\n【写作 Skill】当前选中档位：{tier}（{tier_label}）。若本轮涉及正文创作，请遵循下方写作规则；standard/heavy 档在初稿后由后台子流程审稿修订，正文仍由主模型直接产出。",
        system_prompt,
    );
    if let Some(skill) = skill {
        if tier != "lite" && !skill.rules.trim().is_empty() {
            out.push_str("\n\n## 写作规则（原则级）\n");
            out.push_str(skill.rules.trim());
        }
    }
    // skill 按需加载提示：仅 standard/heavy（lite 不加载全文，忽略加载请求）
    if tier != "lite" {
        out.push_str(
            "\n\n若本轮涉正文创作且需要完整写作 Skill（全文/模板），请另起一段输出【技能加载】，服务端将注入完整 SKILL.md。",
        );
    }
    out
}

/// heavy MemoryPatch 产出（四类字段）解析后回写 actor_states。
/// 宽容解析：无法解析/无 character_state 时返回 0，不阻塞正文。
pub fn apply_memory_patch_to_states(
    states: &mut kaleido_core::ActorStateSystem,
    patch: &str,
) -> usize {
    let Some(v) = crate::llm_stream::extract_json_value(patch) else {
        return 0;
    };
    let Some(cs) = v.get("character_state") else {
        return 0;
    };
    let Some(map) = cs.as_object() else {
        return 0;
    };
    let mut updates: Vec<kaleido_core::ActorStateUpdate> = Vec::new();
    for (cid, val) in map {
        let summary = match val {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let mut fields: HashMap<String, Value> = HashMap::new();
        fields.insert("memory_summary".into(), json!(summary));
        updates.push(kaleido_core::ActorStateUpdate {
            character_id: cid.clone(),
            fields,
            ..Default::default()
        });
    }
    states.apply_updates(&updates)
}

// ─── 内部 helpers ────────────────────────────────────────────────────────────

/// 从 SKILL.md frontmatter 读取键值（兼容 skills.rs 同款语法）。
fn extract_frontmatter_value(body: &str, key: &str) -> Option<String> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = trimmed.trim_start_matches("---");
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    for line in fm.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key {
                let v = v
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim_matches('[')
                    .trim_matches(']');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// 原则级规则摘要：去掉 frontmatter 后取正文开头 ≤1500 字符。
fn principles(content: &str) -> String {
    let body = strip_frontmatter(content);
    body.chars().take(1500).collect()
}

fn strip_frontmatter(body: &str) -> &str {
    let trimmed = body.trim_start();
    if !trimmed.starts_with("---") {
        return body;
    }
    let rest = trimmed.trim_start_matches("---");
    if let Some(end) = rest.find("\n---") {
        let after = &rest[end + 4..];
        if after.starts_with('\n') {
            return &after[1..];
        }
        return after;
    }
    body
}

/// 生成测试/上报用的 Skill 元数据（与 skills.rs parse_skill_dir 对齐扩展键）。
#[allow(dead_code)]
pub fn skill_meta(doc: &SkillDoc) -> Value {
    json!({
        "name": doc.name,
        "tier": doc.tier,
        "scope": doc.scope.as_str(),
        "rulesChars": doc.rules.chars().count(),
        "hasTemplates": {
            "plan": doc.templates.plan.is_some(),
            "write": doc.templates.write.is_some(),
            "review": doc.templates.review.is_some(),
            "fix": doc.templates.fix.is_some(),
            "gate": doc.templates.gate.is_some(),
            "memory": doc.templates.memory.is_some(),
        },
    })
}

/// 工作区层路径解析（供测试/工具使用，不写盘）。
#[allow(dead_code)]
pub fn workspace_layer_dir(data_root: &Path, workspace_id: &str, tier: &str) -> PathBuf {
    data_root
        .join("works")
        .join(workspace_id)
        .join(".denova")
        .join("skills")
        .join(WRITING_NS)
        .join(tier)
}

/// user 层路径解析。
#[allow(dead_code)]
pub fn user_layer_dir(data_root: &Path, tier: &str) -> PathBuf {
    data_root.join("skills").join(WRITING_NS).join(tier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skill-layer-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_resolve_tier_mapping() {
        assert_eq!(resolve_tier_for_quality(TurnQuality::Lite), "lite");
        assert_eq!(resolve_tier_for_quality(TurnQuality::Standard), "standard");
        assert_eq!(resolve_tier_for_quality(TurnQuality::Heavy), "heavy");
        assert_eq!(resolve_tier("STANDARD"), "standard");
        assert_eq!(resolve_tier("unknown"), "lite");
    }

    #[test]
    fn test_builtin_fallback_loads_all_tiers() {
        let root = temp_root("builtin");
        let ws = "ws-test-builtin";
        for tier in ["lite", "standard", "heavy"] {
            let doc = load_writing_skill(&root, Some(ws), tier)
                .expect("builtin fallback must load");
            assert_eq!(doc.tier, tier);
            assert_eq!(doc.scope, SkillScope::Builtin);
            assert!(!doc.content.is_empty(), "SKILL.md content loaded");
            assert!(!doc.rules.is_empty(), "rules extract non-empty");
        }
        clear_skill_cache();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_user_layer_overrides_builtin() {
        let root = temp_root("user");
        let ws = "ws-test-user";
        let tier = "standard";
        // write user-layer SKILL.md + a review template
        let user_dir = user_layer_dir(&root, tier);
        fs::create_dir_all(user_dir.join("templates")).unwrap();
        fs::write(
            user_dir.join("SKILL.md"),
            "---\nname: my-standard\ndescription: custom\ntier: standard\nkind: writing\n---\n# custom standard skill\n用户自定义规则。",
        )
        .unwrap();
        fs::write(
            user_dir.join("templates").join("review.md"),
            "自定义审稿模板：只审不改，输出结构化问题（severity/dimension/problem/fix_instruction/keep）。",
        )
        .unwrap();
        clear_skill_cache();
        let doc = load_writing_skill(&root, Some(ws), tier).unwrap();
        assert_eq!(doc.name, "my-standard");
        assert_eq!(doc.scope, SkillScope::User);
        assert_eq!(doc.tier, "standard");
        assert!(
            doc.templates.review.as_deref().unwrap_or("").contains("自定义审稿模板"),
            "user 层 review 模板应命中"
        );
        clear_skill_cache();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_workspace_layer_overrides_user() {
        let root = temp_root("ws");
        let ws = "ws-123";
        let tier = "heavy";
        // user layer
        let user_dir = user_layer_dir(&root, tier);
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("SKILL.md"), "---\nname: user-heavy\n---\nuser 层规则").unwrap();
        // workspace layer
        let ws_dir = workspace_layer_dir(&root, ws, tier);
        fs::create_dir_all(&ws_dir).unwrap();
        fs::write(
            ws_dir.join("SKILL.md"),
            "---\nname: ws-heavy\n---\n工作区层规则",
        )
        .unwrap();
        clear_skill_cache();
        let doc = load_writing_skill(&root, Some(ws), tier).unwrap();
        assert_eq!(doc.name, "ws-heavy");
        assert_eq!(doc.scope, SkillScope::Workspace);
        // workspace 层没有 SKILL.md 时回落 user 层
        clear_skill_cache();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_append_hint_lite_no_rules() {
        let doc = builtin_doc("lite");
        let out = append_writing_skill_hint("base", "lite", Some(&doc));
        assert!(out.contains("lite"), "lite hint present");
        assert!(!out.contains("## 写作规则"), "lite 不注入规则全文");
        let heavy_doc = builtin_doc("heavy");
        let out2 = append_writing_skill_hint("base", "heavy", Some(&heavy_doc));
        assert!(out2.contains("## 写作规则"), "heavy 注入原则级规则");
        assert!(out2.contains("novel-heavy"), "heavy 档名透出");
    }

    #[test]
    fn test_append_hint_skill_load_line() {
        // heavy/standard 追加【技能加载】按需加载提示；lite 不加
        let out = append_writing_skill_hint("base", "heavy", Some(&builtin_doc("heavy")));
        assert!(
            out.contains("【技能加载】"),
            "heavy 须含按需加载提示：{out}"
        );
        assert!(out.contains("完整 SKILL.md"), "heavy 须提到完整 SKILL.md");
        let lite = append_writing_skill_hint("base", "lite", Some(&builtin_doc("lite")));
        assert!(
            !lite.contains("【技能加载】"),
            "lite 不提示按需加载（lite 不加载全文）"
        );
    }

    #[test]
    fn test_split_skill_load_calls_extracts_and_strips() {
        let text = "旁白：雨夜敲门。\n【技能加载】\n他推开了门。\n之后又提到技能加载，不算块。";
        let (clean, calls) = split_skill_load_calls_from_narrative(text);
        assert_eq!(calls.len(), 1, "独立段【技能加载】应解析为 1 个调用");
        assert!(calls[0].raw.contains("【技能加载】"));
        assert!(
            !clean.contains("【技能加载】"),
            "独立段块应被剥离干净：{clean}"
        );
        assert!(clean.contains("雨夜敲门"), "块前正文保留");
        assert!(clean.contains("他推开了门"), "块后正文保留");
        assert!(
            clean.contains("技能加载") && !clean.contains("【技能加载】"),
            "正文中非独立段的『技能加载』字样不误伤：{clean}"
        );
    }

    #[test]
    fn test_split_skill_load_calls_keeps_inline_prose() {
        // 行内出现（非行首）不剥离：正文原样保留，无调用。
        let text = "他说：其实我们不用【技能加载】也能写。";
        let (clean, calls) = split_skill_load_calls_from_narrative(text);
        assert!(calls.is_empty(), "行内出现不解析为调用");
        assert!(clean.contains("【技能加载】"), "行内原文保留");
        assert!(clean.contains("不用"));
        assert!(clean.contains("也能写"));
        // 无块时恒等
        let (c2, calls2) = split_skill_load_calls_from_narrative("普通正文。");
        assert!(calls2.is_empty());
        assert_eq!(c2, "普通正文。");
    }

    #[test]
    fn test_skill_full_markdown_shape() {
        let doc = builtin_doc("heavy");
        let md = skill_full_markdown(&doc);
        assert!(
            md.contains("# novel-heavy"),
            "SKILL.md 特征行应包含：{md}"
        );
        assert!(md.starts_with("## 完整写作 Skill（heavy）"));
        assert!(md.contains("## 模板"));
        for section in ["### plan", "### write", "### review", "### fix", "### gate", "### memory"]
        {
            assert!(md.contains(section), "缺少模板段 {section}");
        }
        // 有模板的档位输出内容、无模板的档位输出 (无)
        assert!(md.contains("### plan\n"), "heavy 有 plan 模板");
        assert!(!md.contains("### plan\n(无)"), "heavy plan 不应为 (无)");
        let lite = skill_full_markdown(&builtin_doc("lite"));
        assert!(lite.contains("### review\n(无)"), "lite 无 review 模板显示 (无)");
        assert!(lite.contains("## 完整写作 Skill（lite）"));
    }

    #[test]
    fn test_memory_patch_to_states() {
        let mut states = kaleido_core::ActorStateSystem::default();
        let patch = r#"{"progress":"p","character_state":{"苏晚":{"hp":80,"state":"受伤"}},"world_state":"w","foreshadowing":"f"}"#;
        let n = apply_memory_patch_to_states(&mut states, patch);
        assert_eq!(n, 1, "一个 character 写入");
        assert!(states.actors.contains_key("苏晚"));
        assert!(states.build_context_text().contains("memory_summary"));
        // 无法解析的 patch → 0 不 panic
        assert_eq!(apply_memory_patch_to_states(&mut states, "not json"), 0);
        assert_eq!(apply_memory_patch_to_states(&mut states, "{\"progress\":\"p\"}"), 0);
    }

    #[test]
    fn test_frontmatter_parse() {
        let body = "---\nname: novel-x\ntier: heavy\nparents: [\"tavern\"]\n---\nbody";
        assert_eq!(extract_frontmatter_value(body, "name").unwrap(), "novel-x");
        assert_eq!(extract_frontmatter_value(body, "tier").unwrap(), "heavy");
        // 列表字段取原始片段（引号/括号由更高层裁剪）
        assert!(extract_frontmatter_value(body, "parents").unwrap().contains("tavern"));
        assert_eq!(strip_frontmatter(body), "body");
    }
}
