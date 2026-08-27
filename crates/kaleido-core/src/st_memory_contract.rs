//! st_memory_contract.rs — 记忆输出结构契约 + 写前校验 + 修复循环。
//!
//! 吸收自 OpenHanako (https://github.com/liliMozi/openhanako) 的
//! `lib/memory/rolling-summary-format.ts`（Apache 2.0）——「prompt 输出结构契约 +
//! 写前校验 + 修复重试 + 统一提取」设计。Kaleido 原状：记忆 LLM 输出解析失败一律
//! 静默降级（`apply_memory_patch_to_states` 返回 0 / 场记摘要无结构校验）。
//! 本模块提供：
//!   - 四类记忆补丁 JSON 契约校验（progress/character_state/world_state/foreshadowing）
//!   - 场记摘要五节 markdown 结构校验（前情提要/人物/承诺与伏笔/事实账/当前场景）
//!   - 修复 prompt/输入构造（失败原因 + 草稿 → LLM 原样重排，不增删改事实）
//!   - 通用 markdown 节提取（消费侧与校验侧共用同一份规则，防漂移）
//!
//! 修复上限 `MAX_FORMAT_REPAIRS = 1`（对齐 OpenHanako rolling-summary 上限）。

/// 记忆输出格式修复的最大重试次数（对齐 OpenHanako MAX_ROLLING_SUMMARY_FORMAT_REPAIRS）。
/// L4 情感层提炼补丁（2026-08-14 补写端）：JSON 契约 + 结构校验。
/// 输入 LLM 输出的 {affinity:{id:0-100}, secretsKnown:[], promises:[]}，
/// 返回 None 表示不合规（调用方降级，不阻塞回合）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct L4Patch {
    pub affinity: std::collections::HashMap<String, u8>,
    pub secrets_known: Vec<String>,
    pub promises: Vec<String>,
}

pub fn parse_l4_patch(raw: &str) -> Option<L4Patch> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let obj = v.as_object()?;
    let mut patch = L4Patch::default();
    // affinity 字段存在但非对象 → 契约不合规（降级）
    if let Some(v) = obj.get("affinity") {
        let Some(aff) = v.as_object() else {
            return None;
        };
        for (k, val) in aff {
            let score = match val {
                serde_json::Value::Number(n) => n.as_u64().map(|u| u.min(100) as u8),
                serde_json::Value::String(s) => s.trim().parse::<u8>().ok(),
                _ => None,
            };
            if let Some(s) = score {
                patch.affinity.insert(k.clone(), s.min(100));
            }
        }
    }
    for key in ["secretsKnown", "promises"] {
        if let Some(arr) = obj.get(key).and_then(|x| x.as_array()) {
            let out = if key == "secretsKnown" { &mut patch.secrets_known } else { &mut patch.promises };
            for item in arr {
                if let Some(s) = item.as_str() {
                    let t = s.trim();
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
            }
        }
    }
    Some(patch)
}

#[cfg(test)]
mod l4_patch_tests {
    use super::*;

    #[test]
    fn parses_valid_patch() {
        let raw = r#"{"affinity":{"cc-xiao": 85, "cc-lin": "40"}, "secretsKnown":["她是卧底"], "promises":["不再隐瞒"]}"#;
        let p = parse_l4_patch(raw).expect("valid patch");
        assert_eq!(p.affinity.get("cc-xiao"), Some(&85));
        assert_eq!(p.affinity.get("cc-lin"), Some(&40));
        assert_eq!(p.secrets_known, vec!["她是卧底"]);
        assert_eq!(p.promises, vec!["不再隐瞒"]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_l4_patch("not json").is_none());
        assert!(parse_l4_patch(r#"{"affinity": 3}"#).is_none());
        assert!(parse_l4_patch("").is_none());
    }

    #[test]
    fn clamps_and_skips_bad() {
        let raw = r#"{"affinity":{"a": 999, "b": "NaN"}, "secretsKnown":[null, " ok "], "promises":[]}"#;
        let p = parse_l4_patch(raw).expect("tolerant patch");
        assert_eq!(p.affinity.get("a"), Some(&100)); // 999 → clamp 100
        assert!(!p.affinity.contains_key("b"));
        assert_eq!(p.secrets_known, vec!["ok"]);
    }
}

pub const MAX_FORMAT_REPAIRS: usize = 1;

/// 结构校验结果：ok=false 时 issues 列出全部破坏消费方提取假设的问题。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryFormatIssues {
    pub ok: bool,
    pub issues: Vec<String>,
}

impl MemoryFormatIssues {
    fn ok() -> Self {
        Self { ok: true, issues: Vec::new() }
    }
    fn err(issues: Vec<String>) -> Self {
        Self { ok: false, issues }
    }
}

// ─── 四类记忆补丁 JSON 契约 ────────────────────────────────────────────────

/// 四类记忆补丁的字段名（QUALITY_MEMORY_SYS 的 JSON 契约）。
pub const MEMORY_PATCH_FIELDS: [&str; 4] =
    ["progress", "character_state", "world_state", "foreshadowing"];

/// 校验记忆补丁 JSON：必须可解析为对象 + 含消费方硬依赖的 character_state。
/// progress/world_state/foreshadowing 缺失仅记 warning（不影响应用，与现行为兼容）。
pub fn validate_memory_patch(text: &str) -> MemoryFormatIssues {
    let mut trimmed = text.trim();
    // [fix 2026-08-15] 代码层兜底：模型常把 JSON 包进 ```json 围栏（memory.md 旧模板
    // 自伤所致），首字符反引号必然解析失败。先剥离常见围栏再解析，自愈后不再
    // 每次触发 repair 调用（日志 6/6 失败全为 expected value at line 1 column 1）。
    if trimmed.starts_with("```") {
        if let Some(rest) = trimmed.strip_prefix("```json").or_else(|| trimmed.strip_prefix("```")) {
            trimmed = rest.trim_start();
        }
        if let Some(end) = trimmed.rfind("```") {
            trimmed = trimmed[..end].trim();
        }
    }
    if trimmed.is_empty() {
        return MemoryFormatIssues::err(vec!["memory patch 为空".into()]);
    }
    let v: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
    let value = match v {
        Ok(v) => v,
        Err(e) => {
            return MemoryFormatIssues::err(vec![format!("JSON 解析失败: {}", e)]);
        }
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return MemoryFormatIssues::err(vec!["顶层必须是 JSON 对象".into()]),
    };
    let mut issues = Vec::new();
    if !obj.contains_key("character_state") {
        issues.push("缺少 character_state 字段（记忆补丁的消费方硬依赖）".into());
    }
    for f in ["progress", "world_state", "foreshadowing"] {
        if !obj.contains_key(f) {
            issues.push(format!("缺少 {} 字段（非阻断，warning）", f));
        }
    }
    if issues.is_empty() {
        MemoryFormatIssues::ok()
    } else {
        MemoryFormatIssues::err(issues)
    }
}

/// 记忆补丁修复器 system 指令（中文）：只修结构，不增删改事实。
pub fn build_memory_patch_repair_prompt() -> String {
    "你是记忆系统的 JSON 补丁修复器。上一步生成的记忆补丁不符合规定的 JSON 结构，\
     记忆系统无法应用。请把给定草稿中的信息原样重排进合法结构：不要新增、删除或改写\
     事实内容，不要解释，直接输出修复后的完整 JSON。\
     \n\n输出必须是合法 JSON 对象，字段：{\"progress\":\"...\",\"character_state\":{\"<characterId>\":\"当前状态摘要\"},\"world_state\":\"...\",\"foreshadowing\":\"...\"}。\
     \n只基于草稿中已发生内容，不臆造。标题之外不要输出前言、后记、XML 标签或代码块。"
        .to_string()
}

/// 记忆补丁修复调用输入：校验失败原因 + 待修复草稿。
pub fn build_memory_patch_repair_input(issues: &[String], draft: &str) -> String {
    let issue_lines: Vec<String> = issues
        .iter()
        .map(|i| format!("- {}", i.trim()))
        .filter(|l| l != "- ")
        .collect();
    format!(
        "## 校验失败原因\n\n{}\n\n## 待修复草稿\n\n<draft-patch>\n{}\n</draft-patch>",
        if issue_lines.is_empty() { "- 未知".to_string() } else { issue_lines.join("\n") },
        draft.trim()
    )
}

// ─── 场记摘要五节 markdown 契约 ────────────────────────────────────────────

/// 场记摘要五节标题（对齐 RP_SUMMARY_SYSTEM_PROMPT 结构；[0] 中文 [1] 英文）。
pub const RP_SUMMARY_SECTIONS: [[&str; 2]; 5] = [
    ["前情提要", "Synopsis"],
    ["人物", "Characters"],
    ["承诺与伏笔", "Promises & Foreshadowing"],
    ["事实账", "Facts"],
    ["当前场景", "Current Scene"],
];

/// 输出格式要求 prompt 块（追加到场记摘要 system prompt，或修复调用时单独使用）。
pub fn build_rp_summary_format_requirements() -> String {
    "## 输出格式\n最终答案必须按以下顺序包含五个二级标题，标题文本固定：\n\
     1. ## 前情提要\n2. ## 人物\n3. ## 承诺与伏笔\n4. ## 事实账\n5. ## 当前场景\n\n\
     每节正文都必须使用无序列表，列表项以 `- ` 开头。如果某节没有内容，也要输出一个列表项：`- 无`。\n\
     [P1B 2026-08-16 着装契约]「事实账」节必须包含每位出场角色的**着装状态**（当前穿着 + 已脱衣物去向）；\
     剧情中出现过脱衣/穿衣/换装事件时，「当前场景」节必须复述该角色此刻的着装。\
     不可因摘要压缩而丢弃任何着装状态信息——它决定后续剧情中角色的身体状态。\n\
     标题之外不要输出前言、后记、XML 标签或代码块。"
        .to_string()
}

/// 场记摘要修复器 system 指令（中文）。
pub fn build_rp_summary_repair_prompt() -> String {
    format!(
        "你是记忆系统场记摘要的格式修复器。上一步生成的摘要草稿不符合要求的固定结构，\
         记忆系统无法解析。请把给定草稿中的信息原样重排进规定结构：不要新增、删除或改写\
         事实内容，不要解释，直接输出修复后的摘要全文。\n\n{}",
        build_rp_summary_format_requirements()
    )
}

/// 场记摘要修复调用输入：校验失败原因 + 待修复草稿。
pub fn build_rp_summary_repair_input(issues: &[String], draft: &str) -> String {
    let issue_lines: Vec<String> = issues
        .iter()
        .map(|i| format!("- {}", i.trim()))
        .filter(|l| l != "- ")
        .collect();
    format!(
        "## 校验失败原因\n\n{}\n\n## 待修复草稿\n\n<draft-summary>\n{}\n</draft-summary>",
        if issue_lines.is_empty() { "- 未知".to_string() } else { issue_lines.join("\n") },
        draft.trim()
    )
}

/// 校验场记摘要五节结构（对齐 OpenHanako validateRollingSummaryFormat）：
/// 拦截四类破坏消费方提取假设的问题——缺节标题 / 节正文为空（契约要求空时显式 `- 无`）
/// / 后一节标题层级比前一节更深（前节无法收尾）。
/// [P9 2026-08-15] 检测 L1 场记摘要是否被 fix 元话语污染。
/// 实踩：压缩归档时摘要 LLM 把「让我分析审稿意见并修订正文。审稿意见共10项：…」
/// 当摘要写入 sceneSummary（仅 80 字符）。结构校验（validate_rp_summary）只查节标题
/// 存在与否，无法拦语义污染——这里用自指动作 + 审稿特征做启发式拦截。
pub fn summary_is_polluted(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    // 自指动作前缀（LLM 内部对话特征，干净摘要不会以此开头）
    const SELF_REF_PREFIXES: [&str; 8] = [
        "让我分析审稿意见",
        "让我检查",
        "让我重新组织",
        "让我重新设计",
        "让我起草",
        "让我看看",
        "让我输出最终版本",
        "好的，我需要根据审稿",
    ];
    for p in SELF_REF_PREFIXES {
        if t.starts_with(p) {
            return true;
        }
    }
    // 审稿清单特征：正文出现「审稿意见」+ 编号列表（摘要事实账不会逐条审稿）
    if t.contains("审稿意见") && (t.contains('✅') || t.contains('❌') || t.contains("问题") || t.contains('①')) {
        return true;
    }
    // 过长自检段：连续多行「让我/再检查」密集出现
    let self_ref_lines = t
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            l.starts_with("让我") || l.starts_with("再检查") || l.starts_with("好，让我")
        })
        .count();
    self_ref_lines >= 3
}

/// 校验场记摘要结构（RP_SUMMARY_SECTIONS 五节契约）。详见 validate_rp_summary。
pub fn validate_rp_summary(text: &str) -> MemoryFormatIssues {
    let lines: Vec<&str> = text.split('\n').collect();
    let headings: Vec<(usize, usize, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| parse_markdown_heading(l).map(|(level, title)| (i, level, title)))
        .collect();

    let mut issues = Vec::new();
    let mut prev: Option<(usize, usize)> = None; // (index, level)
    for (i, (idx, level, title)) in headings.iter().enumerate() {
        let Some(aliases) = RP_SUMMARY_SECTIONS.iter().find(|a| {
            a[0].eq_ignore_ascii_case(title) || a[1].eq_ignore_ascii_case(title)
        }) else {
            continue;
        };
        if let Some((p_idx, p_level)) = prev {
            if *idx > p_idx && *level > p_level {
                issues.push(format!(
                    "「{}」标题层级比前节更深，前节无法收尾（嵌套层级 {} > {}）",
                    aliases[0], level, p_level
                ));
            }
        }
        // 节正文为空检查：到下一个标题为止。显式 `- 无` 是有内容的标记（不算空）。
        let body: Vec<&str> = lines[*idx + 1..]
            .iter()
            .take_while(|l| parse_markdown_heading(l).is_none())
            .copied()
            .collect();
        if body.iter().all(|l| l.trim().is_empty()) {
            issues.push(format!(
                "「{}」节正文为空；没有内容时须显式写 `- 无`",
                aliases[0]
            ));
        }
        prev = Some((*idx, *level));
        let _ = i;
    }
    for a in RP_SUMMARY_SECTIONS.iter() {
        if !headings
            .iter()
            .any(|(_, _, t)| a[0].eq_ignore_ascii_case(t))
        {
            issues.push(format!("缺少「{}」节标题", a[0]));
        }
    }
    // [P1B 2026-08-16 着装契约弱校验]「事实账」节（或全文）须含着装/身体状态描述。
    // 弱校验——不强制每份摘要都写（无着装事件的摘要可用 `- 无`），但若正文大量
    // 提到衣裤/赤裸却摘要零着装词，视为事实账丢状态，进修复循环补写。
    let facts_text = extract_facts_section(text);
    let body = text.to_lowercase();
    let clothing_terms = ["内裤", "打底裤", "裤子", "裙子", "衣服", "上衣", "衬衫", "裙子",
        "鞋", "袜", "赤裸", "裸", "穿着", "脱", "穿回", "衣裤", "裙摆", "裤腰"];
    let body_has_clothing = clothing_terms.iter().any(|t| body.contains(t));
    if body_has_clothing && !facts_text.contains("着装") && !facts_text.contains("衣")
        && !facts_text.contains("裤") && !facts_text.contains("裙") && !facts_text.contains("鞋")
        && !facts_text.contains("袜") && !facts_text.contains("裸") && !facts_text.contains("脱")
    {
        issues.push(
            "「事实账」节缺失着装状态：正文出现衣裤/赤裸/脱衣等描述，但摘要未记录任何着装状态（当前穿着 + 已脱衣物去向）；请补写该节（无相关事件可写 `- 无`）".to_string(),
        );
    }
    if issues.is_empty() {
        MemoryFormatIssues::ok()
    } else {
        MemoryFormatIssues::err(issues)
    }
}

// ─── 通用 markdown 工具（校验侧与消费侧共用） ─────────────────────────────

/// 解析 markdown 标题行（1-6 级）。返回 (level, title)。
pub fn parse_markdown_heading(line: &str) -> Option<(usize, String)> {
    let line = line.trim_end();
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = line[hashes..].trim_start();
    if rest.is_empty() {
        return None;
    }
    // 去掉行尾闭合 #（如 `## 标题 ##`）
    let title = rest.trim_end_matches('#').trim();
    if title.is_empty() {
        return None;
    }
    Some((hashes, title.to_string()))
}

/// 提取 markdown 中第一个命中标题段的正文（到下一个同级或更高级标题为止）。
pub fn extract_markdown_section(markdown: &str, wanted_titles: &[&str]) -> String {
    if markdown.trim().is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = markdown.split('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let Some((level, title)) = parse_markdown_heading(line) else {
            continue;
        };
        if !wanted_titles.iter().any(|w| title.eq_ignore_ascii_case(w)) {
            continue;
        }
        let body: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|l| match parse_markdown_heading(l) {
                Some((next_level, _)) => next_level > level,
                None => true,
            })
            .copied()
            .collect();
        return body.join("\n").trim().to_string();
    }
    String::new()
}

/// 提取场记摘要「事实账」节正文（消费侧与校验侧共用）。
pub fn extract_facts_section(markdown: &str) -> String {
    extract_markdown_section(markdown, &[RP_SUMMARY_SECTIONS[3][0], RP_SUMMARY_SECTIONS[3][1]])
}

/// 节正文是否是显式空标记（- 无 / - None / 空行）。
pub fn is_empty_section(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return true;
    }
    lines.iter().all(|l| {
        let item = l.trim_start_matches(['-', '*', '+']).trim().to_lowercase();
        item == "无" || item == "none"
    })
}

// ─── 测试（移植 OpenHanako rolling-summary-format 校验场景） ─────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_summary() -> String {
        "## 前情提要\n- 阿远抵达青梧城\n\n## 人物\n- 阿远：沉默寡言\n\n## 承诺与伏笔\n- 欠老板娘一顿酒\n\n## 事实账\n- 阿远持有青铜钥匙\n\n## 当前场景\n- 酒馆二楼，深夜".to_string()
    }

    #[test]
    fn valid_summary_passes() {
        let r = validate_rp_summary(&ok_summary());
        assert!(r.ok, "合规摘要应通过: {:?}", r.issues);
    }

    #[test]
    fn missing_section_fails() {
        let text = ok_summary().replace("## 承诺与伏笔\n- 欠老板娘一顿酒\n\n", "");
        let r = validate_rp_summary(&text);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.contains("承诺与伏笔")));
    }

    #[test]
    fn empty_section_without_marker_fails() {
        let text = ok_summary().replace("## 事实账\n- 阿远持有青铜钥匙", "## 事实账");
        let r = validate_rp_summary(&text);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.contains("事实账")));
    }

    #[test]
    fn explicit_empty_marker_passes() {
        let text = ok_summary().replace("## 事实账\n- 阿远持有青铜钥匙", "## 事实账\n- 无");
        let r = validate_rp_summary(&text);
        assert!(r.ok, "显式 - 无 应通过: {:?}", r.issues);
    }

    #[test]
    fn nested_later_heading_fails() {
        // 契约节之间嵌套：当前场景(level 3) 比 事实账(level 2) 更深 → 事实账无法收尾
        let text = ok_summary().replace(
            "## 当前场景\n- 酒馆二楼，深夜",
            "### 当前场景\n- 酒馆二楼，深夜",
        );
        let r = validate_rp_summary(&text);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.contains("收尾")));
    }

    #[test]
    fn heading_any_level_accepted() {
        let text = ok_summary()
            .replace("## 前情提要", "### 前情提要")
            .replace("## 人物", "### 人物")
            .replace("## 承诺与伏笔", "### 承诺与伏笔")
            .replace("## 事实账", "### 事实账")
            .replace("## 当前场景", "### 当前场景");
        let r = validate_rp_summary(&text);
        assert!(r.ok, "任意层级标题应通过: {:?}", r.issues);
    }

    #[test]
    fn memory_patch_valid_json_passes() {
        let patch = r#"{"progress":"已到青梧城","character_state":{"char-a":"疲惫但坚定"},"world_state":"夜晚","foreshadowing":"钥匙"}"#;
        let r = validate_memory_patch(patch);
        assert!(r.ok);
    }

    #[test]
    fn memory_patch_bad_json_fails() {
        let r = validate_memory_patch("not json");
        assert!(!r.ok);
        assert!(r.issues[0].contains("JSON 解析失败"));
    }

    #[test]
    fn memory_patch_fenced_code_block_self_heals() {
        // [fix 2026-08-15] 模型照抄模板 ```json 围栏（旧 memory.md 自伤），
        // 首字符反引号解析失败；代码层剥离围栏后应自愈通过。
        let patch = "```json\n{\"progress\":\"已到青梧城\",\"character_state\":{\"char-a\":\"疲惫但坚定\"},\"world_state\":\"夜晚\",\"foreshadowing\":\"钥匙\"}\n```";
        let r = validate_memory_patch(patch);
        assert!(r.ok, "带 ```json 围栏的补丁应剥离后通过: {:?}", r.issues);
    }

    #[test]
    fn memory_patch_fenced_no_lang_self_heals() {
        let patch = "```\n{\"progress\":\"p\",\"character_state\":{\"char-a\":\"s\"},\"world_state\":\"w\",\"foreshadowing\":\"f\"}\n```";
        let r = validate_memory_patch(patch);
        assert!(r.ok, "无语言标记的围栏也应剥离通过: {:?}", r.issues);
    }

    #[test]
    fn memory_patch_missing_character_state_fails() {
        let r = validate_memory_patch(r#"{"progress":"p","world_state":"w"}"#);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.contains("character_state")));
    }

    #[test]
    fn memory_patch_partial_fields_warns_only() {
        let r = validate_memory_patch(r#"{"character_state":{"a":"s"}}"#);
        assert!(!r.ok, "缺字段应报 issues");
        assert!(r.issues.iter().any(|i| i.contains("progress")));
    }

    #[test]
    fn repair_input_contains_issues_and_draft() {
        let input = build_memory_patch_repair_input(
            &["缺少 character_state 字段（记忆补丁的消费方硬依赖）".to_string()],
            "draft-here",
        );
        assert!(input.contains("校验失败原因"));
        assert!(input.contains("<draft-patch>"));
        assert!(input.contains("draft-here"));
    }

    #[test]
    fn extract_facts_shared_rule() {
        let facts = extract_facts_section(&ok_summary());
        assert_eq!(facts, "- 阿远持有青铜钥匙");
    }

    #[test]
    fn parse_heading_handles_closing_hashes() {
        let (level, title) = parse_markdown_heading("## 前情提要 ##").unwrap();
        assert_eq!(level, 2);
        assert_eq!(title, "前情提要");
    }
}
