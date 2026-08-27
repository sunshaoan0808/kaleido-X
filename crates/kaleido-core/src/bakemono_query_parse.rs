//! 查询改写解析与指令行过滤（吸收自 SillyTavern-BakemonoMemory `src/vector/query-parser.js`）。
//!
//! 清除 LLM 输出的思考残渣/格式围栏，解析 INTENT/Q1-Q5 行或 JSON，过滤「复述任务」指令行
//! （如「以下/输出/要求/约束…」）——对症 Kaleido 提炼 LLM 输出复述任务而非 JSON 的污染。
//! 纯函数。测例翻译自 `tests/parser-modules.test.mjs`（向量查询解析部分）。

use regex::Regex;
use std::sync::OnceLock;

/// 清除 think/analysis/reasoning 块 + 代码围栏。
pub fn strip_reasoning_blocks(raw: &str) -> String {
    static RE_THINK: OnceLock<Regex> = OnceLock::new();
    static RE_ANALYSIS: OnceLock<Regex> = OnceLock::new();
    static RE_REASONING: OnceLock<Regex> = OnceLock::new();
    static RE_FENCE_OPEN: OnceLock<Regex> = OnceLock::new();
    static RE_FENCE_CLOSE: OnceLock<Regex> = OnceLock::new();
    let re_think = RE_THINK.get_or_init(|| Regex::new(r"(?is)<think\b[^>]*>[\s\S]*?</think>").unwrap());
    let re_analysis = RE_ANALYSIS.get_or_init(|| Regex::new(r"(?is)<analysis\b[^>]*>[\s\S]*?</analysis>").unwrap());
    let re_reasoning = RE_REASONING.get_or_init(|| Regex::new(r"(?is)<reasoning\b[^>]*>[\s\S]*?</reasoning>").unwrap());
    let re_fence_open = RE_FENCE_OPEN.get_or_init(|| Regex::new(r"```(?:json|text)?").unwrap());
    let re_fence_close = RE_FENCE_CLOSE.get_or_init(|| Regex::new(r"```").unwrap());
    let s = re_think.replace_all(raw, "");
    let s = re_analysis.replace_all(&s, "");
    let s = re_reasoning.replace_all(&s, "");
    let s = re_fence_open.replace_all(&s, "");
    let s = re_fence_close.replace_all(&s, "");
    s.trim().to_string()
}

/// 判断是否为 LLM 复述任务的指令行（非真实查询）。
pub fn is_vector_rewrite_instruction_line(text: &str) -> bool {
    let value = text.trim();
    if value.is_empty() {
        return true;
    }
    if value.chars().count() < 4 {
        return true;
    }
    // 英文指令前缀
    let en_patterns = [
        "thinking process",
        "analyze the request",
        "role:",
        "task:",
        "constraints:",
        "requirements:",
        "output:",
        "only output",
        "do not",
        "system:",
        "assistant:",
        "user:",
        "recent plot",
        "search queries:",
        "queries:",
        "intent:",
        "intent`",
        "intent'",
        "keep only facts",
        "one query per line",
        "no explanations",
        "language:",
        "convert recent plot",
        "clue",
        "query",
    ];
    let lower = value.to_lowercase();
    for p in en_patterns {
        if lower.starts_with(p) {
            return true;
        }
    }
    // 中文指令前缀
    let zh_patterns = [
        "以下",
        "输出",
        "检索",
        "要求",
        "约束",
        "任务",
        "角色",
        "输入",
        "目标",
        "规则",
        "格式",
        "最近剧情",
        "当前剧情",
        "检索意图",
        // [morphling 增强] Kaleido 提炼 LLM 复述任务实测高频开头（日志 2026-08-16 实证）
        "我们根据",
        "我们需要",
        "我们开始",
        "请根据",
        "根据要求",
        "根据用户",
        "根据输入",
        "根据规则",
        "注意格式",
        "注意：",
        "分析内容",
        "输出内容",
        "生成内容",
        "本次任务",
        "当前任务",
    ];
    for p in zh_patterns {
        if value.starts_with(p) {
            return true;
        }
    }
    // [morphling 增强] 中文复述任务特征片段
    let zh_fragments = [
        "需要输出json",
        "需要输出 json",
        "需要提取",
        "需要生成",
        "需要构建",
        "需要返回",
        "应输出json",
        "应输出 json",
        "输出json，",
        "输出 json，",
        "只输出json",
        "只输出 json",
        "包含events",
        "包含 event",
        "注意格式：",
        "注意：events",
        "输出格式为",
        "格式如下",
        "请按照",
        "请严格",
    ];
    let lower_zh = value.to_lowercase();
    for f in zh_fragments {
        if lower_zh.contains(f) {
            return true;
        }
    }
    // 英文指令片段
    let en_fragments = [
        "only return",
        "return json",
        "json array",
        "json object",
        "do not output",
        "must be in chinese",
        "must be specific",
        "specific questions",
        "searching old plot",
        "focus on what old memories",
        "current context",
        "determine the retrieval",
        "retrieval intent",
        "pain connection",
        "the text mentions",
        "the current scene",
        "old memories need to be recalled",
        "characters, relationships, locations",
        "unresolved foreshadowing",
        "不要解释", "不要输出步骤", "不要输出分析", "每行一条", "只返回", "只输出",
        "必须使用中文", "输出必须", "只能包含", "不要把最近剧情",
    ];
    for f in en_fragments {
        if lower.contains(f) {
            return true;
        }
    }
    // 中文标题式指令（**analyze** 等）
    if value.starts_with("**") && value.contains("**") {
        let mid = value[2..].to_lowercase();
        for p in ["analyze", "role", "task", "constraints", "output", "thinking", "goal", "input"] {
            if mid.starts_with(p) {
                return true;
            }
        }
    }
    // 英文段落式指令
    let en_line_starts = [
        "input", "goal", "analyze", "chapter", "recent plot chapters", "current context",
        "determine the retrieval", "the current scene",
    ];
    for p in en_line_starts {
        if lower.starts_with(p) {
            return true;
        }
    }
    false
}

/// 中文/日文内容占比校验（查询必须主要是 CJK 或英文为主）。
pub fn has_vector_rewrite_query_language(text: &str) -> bool {
    let cjk_count = text.chars().filter(|c| {
        let u = *c as u32;
        (0x3400..=0x9fff).contains(&u) || (0x3040..=0x30ff).contains(&u)
    }).count();
    if cjk_count < 4 {
        return false;
    }
    let latin_count = text.chars().filter(|c| c.is_ascii_alphabetic()).count();
    if latin_count > 0 && (cjk_count as f64) / ((cjk_count + latin_count) as f64) < 0.32 {
        return false;
    }
    true
}

fn normalize_query_item(item: &str) -> String {
    let mut text = item.trim().to_string();
    if text.is_empty() {
        return String::new();
    }
    // 逐行合并 + 过滤指令行
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !is_vector_rewrite_instruction_line(l))
        .collect();
    text = lines.join(" ");
    // 剥前缀
    static RE_PREFIXES: OnceLock<Regex> = OnceLock::new();
    let re = RE_PREFIXES.get_or_init(|| {
        Regex::new(r"(?i)^(?:\s*(?:INTENT|Q\s*[1-5])\s*[:：]\s*|^\s*(?:Q\s*)?\d+\s*[.)、:：-]\s*|^\s*(?:clue|query)\s*\d+\s*(?:\([^)]*\))?\s*[:：*-]?\s*|^\s*(?:[-*]|\d+[.)、]|[（(]?\d+[）)])\s*|^\s*(?:query|查询|检索句|关键词|线索)\s*[:：]\s*)").unwrap()
    });
    text = re.replace_all(&text, "").to_string();
    let text = text.trim().trim_matches(|c| matches!(c, ' ' | '*' | '_' | '`' | '#' | '>')).trim_matches(|c| matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’')).trim().to_string();
    if !has_vector_rewrite_query_language(&text) || is_vector_rewrite_instruction_line(&text) {
        return String::new();
    }
    text
}

fn normalize_intent(item: &str) -> String {
    let text = normalize_query_item(item);
    if text.is_empty() {
        return String::new();
    }
    text.chars().take(220).collect()
}

/// 解析 INTENT / Q1-Q5 行格式。
pub fn parse_vector_query_rewrite_lines(source: &str) -> (String, Vec<String>) {
    static RE_INTENT: OnceLock<Regex> = OnceLock::new();
    static RE_Q: OnceLock<Regex> = OnceLock::new();
    let re_intent = RE_INTENT.get_or_init(|| Regex::new(r"(?i)^\s*INTENT\s*[:：]\s*(.+)$").unwrap());
    let re_q = RE_Q.get_or_init(|| Regex::new(r"(?i)^\s*Q\s*([1-5])\s*[:：]\s*(.+)$").unwrap());
    let mut intent = String::new();
    let mut queries: Vec<String> = Vec::new();
    for line in source.split('\n').map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if let Some(cap) = re_intent.captures(line) {
            let v = normalize_intent(&cap[1]);
            if !v.is_empty() {
                intent = v;
            }
            continue;
        }
        if let Some(cap) = re_q.captures(line) {
            let v = normalize_query_item(&cap[2]);
            if !v.is_empty() && !queries.contains(&v) {
                queries.push(v);
            }
        }
    }
    (intent, queries)
}

/// 完整解析：行格式优先，JSON 兜底，最后文本行。
pub fn parse_vector_query_rewrite_payload(raw: &str) -> (String, Vec<String>) {
    let source = strip_reasoning_blocks(raw);
    if source.is_empty() {
        return (String::new(), Vec::new());
    }
    let (intent, queries) = parse_vector_query_rewrite_lines(&source);
    if !queries.is_empty() || !intent.is_empty() {
        return (intent, queries);
    }
    // JSON 兜底
    static RE_JSON: OnceLock<Regex> = OnceLock::new();
    let re = RE_JSON.get_or_init(|| Regex::new(r"(\[[\s\S]*\]|\{[\s\S]*\})").unwrap());
    if let Some(cap) = re.captures(&source) {
        let candidate = cap[1].trim();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
            let mut qs: Vec<String> = Vec::new();
            if let Some(arr) = v.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let n = normalize_query_item(s);
                        if !n.is_empty() {
                            qs.push(n);
                        }
                    }
                }
                return (String::new(), qs);
            }
            if let Some(obj) = v.as_object() {
                let intent = obj
                    .get("intent")
                    .or_else(|| obj.get("searchIntent"))
                    .or_else(|| obj.get("goal"))
                    .and_then(|x| x.as_str())
                    .map(normalize_intent)
                    .unwrap_or_default();
                if let Some(qa) = obj.get("queries").and_then(|x| x.as_array()) {
                    for item in qa {
                        if let Some(s) = item.as_str() {
                            let n = normalize_query_item(s);
                            if !n.is_empty() {
                                qs.push(n);
                            }
                        }
                    }
                    return (intent, qs);
                }
                if let Some(qa) = obj.get("query").and_then(|x| x.as_array()) {
                    for item in qa {
                        if let Some(s) = item.as_str() {
                            let n = normalize_query_item(s);
                            if !n.is_empty() {
                                qs.push(n);
                            }
                        }
                    }
                    return (intent, qs);
                }
                if let Some(qs_str) = obj.get("query").and_then(|x| x.as_str()) {
                    let n = normalize_query_item(qs_str);
                    if !n.is_empty() {
                        qs.push(n);
                    }
                    return (intent, qs);
                }
            }
        }
    }
    // 文本行解析
    let mut qs: Vec<String> = Vec::new();
    for line in source.split('\n').map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let cleaned = line
            .trim_start_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace() || matches!(c, '-' | '*' | '•' | '·'))
            .trim_matches(|c| matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’'));
        let n = normalize_query_item(cleaned);
        if !n.is_empty() && !qs.contains(&n) {
            qs.push(n);
        }
    }
    (String::new(), qs)
}

/// 从 chat completion 响应中提取文本（兼容 content 数组 / reasoning）。
pub fn extract_chat_completion_text(data: &serde_json::Value) -> String {
    let choice = data
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .unwrap_or(data);
    let message = choice.get("message").unwrap_or(choice);
    let content = message
        .get("content")
        .or_else(|| choice.get("text"))
        .or_else(|| data.get("output_text"));
    let mut out = String::new();
    if let Some(c) = content {
        if let Some(s) = c.as_str() {
            out.push_str(s);
        } else if let Some(arr) = c.as_array() {
            for part in arr {
                if let Some(s) = part.as_str() {
                    out.push_str(s);
                } else if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                    out.push_str(t);
                } else if let Some(t) = part.get("content").and_then(|x| x.as_str()) {
                    out.push_str(t);
                }
            }
        }
    }
    let text = out.trim().to_string();
    if !text.is_empty() {
        return text;
    }
    message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .or_else(|| choice.get("reasoning_content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_query_parser_keeps_chinese_clues_and_removes_reasoning_residue() {
        let payload = r#"
        <think>Analyze the request and output queries.</think>
        INTENT: 找回鲸湾书房中遗失的钥匙
        Q1: Nana 在鲸湾书房交出黑曜石钥匙
        Q2: Kuroha 发现钥匙失踪后的反应
        Q3: Nana 在鲸湾书房交出黑曜石钥匙
        Output: no explanations
        "#;
        let (intent, queries) = parse_vector_query_rewrite_payload(payload);
        assert_eq!(intent, "找回鲸湾书房中遗失的钥匙");
        assert_eq!(
            queries,
            vec![
                "Nana 在鲸湾书房交出黑曜石钥匙".to_string(),
                "Kuroha 发现钥匙失踪后的反应".to_string(),
            ]
        );
    }

    #[test]
    fn chinese_instruction_lines_are_filtered() {
        // Kaleido 提炼 LLM 复述任务实例：指令行必须被过滤
        assert!(is_vector_rewrite_instruction_line("我们根据用户输入，需要输出JSON"));
        assert!(is_vector_rewrite_instruction_line("我们需要提取故事记忆。"));
        assert!(is_vector_rewrite_instruction_line("以下要求：只输出 JSON"));
        assert!(!is_vector_rewrite_instruction_line("向明初在画室画了一幅素描"));
        assert!(!is_vector_rewrite_instruction_line("庄眉看过那幅素描后没有说话"));
    }

    #[test]
    fn extract_chat_completion_text_handles_content_arrays() {
        let data = serde_json::json!({
            "choices": [{
                "message": {
                    "content": [
                        {"text": "第一行"},
                        {"content": "第二行"}
                    ]
                }
            }]
        });
        assert_eq!(extract_chat_completion_text(&data), "第一行第二行");
    }
}
