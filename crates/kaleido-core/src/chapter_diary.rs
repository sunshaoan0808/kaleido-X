//! 章节剧情摘要账本（吸收自 SillyTavern-BakemonoMemory summary-memory-model）。
//!
//! [morphling C2 2026-08-16] 顺带总结模式：主生成响应剥离【章节摘要】块——
//! LLM 写正文时顺手输出本章剧情进展，服务端提取后落账本，零额外 LLM 调用。

/// 从生成响应中提取【章节摘要】块。
///
/// 返回 (clean, summary)：
/// - clean：剥离块后的正文（正文中所有【章节摘要】标记块被移除）
/// - summary：最后一个摘要块的内容（无块则为 None）
///
/// 匹配规则：`【章节摘要】` 标记后直到行尾（\n 或文本结尾）的文本；多个块取最后一个。
pub fn extract_chapter_diary_block(full: &str) -> (String, Option<String>) {
    const MARKER: &str = "【章节摘要】";
    let mut clean = String::new();
    let mut summary: Option<String> = None;
    let mut rest = full;
    loop {
        let Some(mark) = rest.find(MARKER) else {
            clean.push_str(rest);
            break;
        };
        // 标记前的正文保留
        clean.push_str(&rest[..mark]);
        let after = &rest[mark + MARKER.len()..];
        // 块内容 = 标记后到行尾（含 \r\n 与 \n）；若其后是其他【标记行则只取到该行前
        let line_end = after.find('\n').unwrap_or(after.len());
        let content = after[..line_end].trim();
        // 跳过标记后立刻换行（空内容）的情况
        if !content.is_empty() {
            summary = Some(content.to_string());
        }
        // 换行符保留（正文段落结构不破坏）
        if line_end < after.len() {
            clean.push('\n');
        }
        rest = &after[line_end..];
        if summary.is_some() && rest.trim().is_empty() {
            break;
        }
    }
    (clean, summary)
}

/// 章节摘要写入账本（manual_edited 保护：用户手动改过的章不覆盖）。
/// [V1 2026-08-17 修复] 累积式：同章多次摘要不再整段覆盖——旧摘要与新摘要
/// 融合（互含则取较长者，否则拼接），受 [`SUMMARY_BUDGET`] 字符预算约束，
/// 超限保留尾部（越新越重要）。修复「每章摘要实为每回合摘要」的整章丢失。
/// 返回是否写入/更新。
pub const SUMMARY_BUDGET: usize = 800;

fn merge_chapter_summaries(old: &str, new: &str) -> String {
    let old = old.trim();
    let new = new.trim();
    if old.is_empty() {
        return new.to_string();
    }
    if new.is_empty() {
        return old.to_string();
    }
    // 互含 → 取较长者（同一进展的复述/精炼，避免重复累积）
    let old_head: String = old.chars().take(20).collect();
    let new_head: String = new.chars().take(20).collect();
    if old.contains(new) || new.contains(old) || old_head == new_head {
        return if new.chars().count() >= old.chars().count() { new.to_string() } else { old.to_string() };
    }
    // 拼接：旧摘要去尾标点 + 新摘要，受预算约束（保尾 = 保最新）
    let joined = format!(
        "{}{}",
        old.trim_end_matches(['。', '；', ';', '，', ',']),
        new
    );
    joined.chars().rev().take(SUMMARY_BUDGET).collect::<String>().chars().rev().collect()
}

pub fn upsert_chapter_diary(
    diaries: &mut Vec<crate::ChapterDiaryEntry>,
    chapter_id: &str,
    title: &str,
    summary: &str,
    turn: u32,
) -> bool {
    if chapter_id.is_empty() || summary.trim().is_empty() {
        return false;
    }
    match diaries.iter_mut().find(|d| d.chapter_id == chapter_id) {
        Some(d) => {
            if d.manual_edited {
                return false;
            }
            d.summary = merge_chapter_summaries(&d.summary, summary);
            d.end_turn = turn;
            d.updated_at_turn = turn;
        }
        None => {
            diaries.push(crate::ChapterDiaryEntry {
                chapter_id: chapter_id.to_string(),
                title: title.to_string(),
                summary: summary.to_string(),
                start_turn: 0,
                end_turn: turn,
                updated_at_turn: turn,
                manual_edited: false,
            });
        }
    }
    true
}

/// [V2 2026-08-17 修复] epoch/weaver 压缩写入 L1 的占位文本识别：
/// 「当前场景：X（第N回合记忆压缩）」或「第N回合压缩归档」类空洞占位。
/// fallback 提炼输入须先过滤这类占位，避免章节摘要吸收压缩噪声。
/// 规则：行整体是压缩占位 → 丢整行；行内含占位片段 → 只删片段。
pub fn strip_compression_placeholder(text: &str) -> String {
    // 占位片段形态：「（第N回合记忆压缩）」「（第N回合压缩归档）」「第N回合记忆压缩」
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let is_pure_placeholder = t.contains("记忆压缩") || t.contains("压缩归档");
        if !is_pure_placeholder {
            out.push(t.to_string());
            continue;
        }
        // 剥占位片段：「（第N回合记忆压缩）」/「（第N回合压缩归档）」→ 空；行内残余再判
        let cleaned = t.replacen("（", "（", 1);
        let cleaned = strip_marker(&cleaned, "记忆压缩");
        let cleaned = strip_marker(&cleaned, "压缩归档");
        let cleaned = cleaned.trim().trim_matches(['（', '）', '(', ')', ' ']).to_string();
        if !cleaned.is_empty() {
            out.push(cleaned);
        }
    }
    // 占位片段可能跨行无括号：兜底全文剥「第N回合记忆压缩/压缩归档」词
    out.join("\n")
        .replacen("记忆压缩", "", 20)
        .replacen("压缩归档", "", 20)
        .trim()
        .to_string()
}

/// 从文本中剥掉「第N回合<tag>」片段（含可能的前置"（"与后置"）"），返回残余。
fn strip_marker(text: &str, tag: &str) -> String {
    let Some(idx) = text.find(tag) else {
        return text.to_string();
    };
    // 向前找起点：最近的中文「（」或英文 "(" 或行首
    let mut start = idx;
    for (i, c) in text[..idx].char_indices().rev() {
        if c == '（' || c == '(' {
            start = i;
            break;
        }
        if c == '｜' || c == '|' || c == '\n' || c == '：' || c == ':' {
            start = i + c.len_utf8();
            break;
        }
        start = i;
    }
    let tag_end = idx + tag.len();
    let mut end = tag_end;
    let after = &text[tag_end..];
    // 向后找结束：最近的「）」或 ")"、换行
    for (i, c) in after.char_indices() {
        if c == '）' || c == ')' || c == '\n' || c == '。' || c == '；' {
            end = tag_end + i + c.len_utf8();
            break;
        }
        end = tag_end + i + c.len_utf8();
    }
    let mut out = String::new();
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_trailing_summary_block() {
        let text = "庄眉推门进来，看见画架上摊开的素描，指尖顿在纸边。\n【章节摘要】向明初在画室为庄眉画了裸体素描，庄眉看过并离开。";
        let (clean, summary) = extract_chapter_diary_block(text);
        assert_eq!(
            summary.as_deref(),
            Some("向明初在画室为庄眉画了裸体素描，庄眉看过并离开。")
        );
        assert_eq!(clean.trim(), "庄眉推门进来，看见画架上摊开的素描，指尖顿在纸边。");
    }

    #[test]
    fn block_mid_text_removed_clean() {
        let text = "正文第一段。\n【章节摘要】第一版摘要内容。\n正文第二段继续。";
        let (clean, summary) = extract_chapter_diary_block(text);
        assert_eq!(summary.as_deref(), Some("第一版摘要内容。"));
        assert!(!clean.contains("第一版摘要内容"));
        assert!(clean.contains("正文第一段"));
        assert!(clean.contains("正文第二段继续"));
    }

    #[test]
    fn multiple_blocks_take_last() {
        let text = "【章节摘要】早期摘要。\n剧情继续。\n【章节摘要】最终摘要：本章以画室相遇收尾。";
        let (clean, summary) = extract_chapter_diary_block(text);
        assert_eq!(summary.as_deref(), Some("最终摘要：本章以画室相遇收尾。"));
        assert!(!clean.contains("早期摘要"));
        assert!(!clean.contains("最终摘要"));
        assert!(clean.contains("剧情继续"));
    }

    #[test]
    fn no_block_returns_none_untouched() {
        let text = "今天是个晴天，向明初照常去学校。";
        let (clean, summary) = extract_chapter_diary_block(text);
        assert!(summary.is_none());
        assert_eq!(clean, text);
    }

    #[test]
    fn upsert_respects_manual_edited() {
        let mut diaries = vec![crate::ChapterDiaryEntry {
            chapter_id: "ch01".into(),
            title: String::new(),
            summary: "旧摘要".into(),
            start_turn: 0,
            end_turn: 3,
            updated_at_turn: 3,
            manual_edited: true,
        }];
        assert!(!upsert_chapter_diary(&mut diaries, "ch01", "", "新摘要", 8));
        assert_eq!(diaries[0].summary, "旧摘要");
    }

    #[test]
    fn upsert_new_entry_and_update() {
        let mut diaries: Vec<crate::ChapterDiaryEntry> = Vec::new();
        assert!(upsert_chapter_diary(&mut diaries, "ch02", "第二章", "第一次总结", 10));
        assert_eq!(diaries.len(), 1);
        assert_eq!(diaries[0].title, "第二章");
        assert!(upsert_chapter_diary(&mut diaries, "ch02", "第二章", "更新总结", 20));
        assert_eq!(diaries.len(), 1);
        assert_eq!(diaries[0].summary, "第一次总结更新总结"); // [V1] 累积而非覆盖
        assert_eq!(diaries[0].updated_at_turn, 20);
    }

    #[test]
    fn upsert_merges_subsumed() {
        // 互含（new 是 old 的复述/精炼）→ 不重复累积
        let mut diaries = Vec::new();
        upsert_chapter_diary(&mut diaries, "c", "章", "庄眉在画室看到素描，离开。", 1);
        upsert_chapter_diary(&mut diaries, "c", "章", "庄眉在画室看到素描，离开。她回到宿舍。", 2);
        assert_eq!(diaries[0].summary, "庄眉在画室看到素描，离开。她回到宿舍。");
    }

    #[test]
    fn upsert_merge_respects_budget() {
        let long_old = "旧".repeat(700);
        let long_new = "新".repeat(700);
        let mut diaries = Vec::new();
        upsert_chapter_diary(&mut diaries, "c", "章", &format!("{long_old}A"), 1);
        upsert_chapter_diary(&mut diaries, "c", "章", &format!("B{long_new}"), 2);
        let s = &diaries[0].summary;
        assert!(s.chars().count() <= crate::chapter_diary::SUMMARY_BUDGET, "超预算");
        // 保尾 = 保最新（new 内容保留）
        assert!(s.ends_with(&"新".repeat(100)));
    }

    #[test]
    fn strip_compression_placeholder_filters() {
        // 纯压缩占位行 → 剥离占位片段（节点名保留，rel_line 不丢）
        let out = super::strip_compression_placeholder("当前场景：n2（第5回合记忆压缩）");
        assert_eq!(out, "当前场景：n2");
        // 占位 + rel_line 同行：rel_line 保留
        let out2 = super::strip_compression_placeholder(
            "当前场景：n3（第6回合记忆压缩） 关系已确立：至少接过吻2次，后续亲热要体现熟悉感。",
        );
        assert!(!out2.contains("记忆压缩"), "got {out2}");
        assert!(out2.contains("关系已确立"), "got {out2}");
        // 保留真实摘要，剔除占位
        let mixed = "庄眉进门。\n当前场景：画室（第7回合压缩归档）\n关系确立：完成。";
        let out3 = super::strip_compression_placeholder(mixed);
        assert!(!out3.contains("压缩归档"), "got {out3}");
        assert!(out3.contains("庄眉进门"));
        assert!(out3.contains("关系确立"));
        // 无占位 → 原样
        let plain = "夜色将尽，雨声不停。";
        assert_eq!(super::strip_compression_placeholder(plain), "夜色将尽，雨声不停。");
    }
}
