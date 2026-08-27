//! 渐进式文本压缩器（关键词评分 + 首末段优先）。
//!
//! 参考实现（纯 std 移植）：Openwrite `tools/progressive_compressor.py`。
//! 用于替代会话/章节正文的硬截断（如 `take(6000)` / `take(12000)`），
//! 在限定长度内保留关键情节、转折点与首末段。

use crate::text_chunker::split_paragraphs;

/// 关键情节关键词表：命中 +4/个。
const KEYWORDS: &[&str] = &[
    "突然", "然而", "决定", "死", "突破", "发现", "终于", "原来", "真相", "背叛", "牺牲", "觉醒",
    "离开", "回到", "约定", "秘密", "危险", "凶手", "失败", "成功", "揭露", "失踪", "恢复", "复仇",
    "告白",
];

/// 对话段落判定：以「」/“”/" 引号开头或结尾。
fn is_dialogue(para: &str) -> bool {
    let p = para.trim();
    let quote = |c: char| matches!(c, '「' | '」' | '“' | '”' | '"');
    match (p.chars().next(), p.chars().last()) {
        (Some(first), Some(last)) => quote(first) || quote(last),
        _ => false,
    }
}

fn score_paragraph(index: usize, total: usize, para: &str) -> i32 {
    let mut score = 0i32;
    if index == 0 {
        score += 10; // 首段优先
    }
    if index + 1 == total {
        score += 8; // 末段次之
    }
    for kw in KEYWORDS {
        if para.contains(kw) {
            score += 4;
        }
    }
    if is_dialogue(para) {
        score -= 2; // 对话段略低优先级
    }
    score
}

fn truncate_to_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// 按关键词评分渐进压缩单段文本，长度不超过 `target_chars + 200`。
///
/// - 文本 ≤ `target_chars` 时直接返回原文；
/// - 按段落切分（复用 `split_paragraphs`），首段 +10、末段 +8、关键词 +4/个、对话段 -2；
/// - 按分数降序选取段落至接近 `target_chars`，输出保持原文顺序；不足则放宽到全部段落。
pub fn compress_text(text: &str, target_chars: usize) -> String {
    let cap = target_chars + 200;
    if text.chars().count() <= target_chars {
        return text.to_string();
    }

    let paragraphs = split_paragraphs(text);
    if paragraphs.is_empty() {
        return String::new();
    }
    let total = paragraphs.len();

    // 打分，稳定排序（同分保持原文顺序）
    let mut scored: Vec<(i32, usize)> = paragraphs
        .iter()
        .enumerate()
        .map(|(i, p)| (score_paragraph(i, total, p), i))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let first = 0usize;
    let last = total - 1;
    let mut selected: Vec<usize> = Vec::new();
    let mut used: usize = 0;

    for (_, idx) in scored {
        let len = paragraphs[idx].chars().count();
        // 拼接时每新增一个段落多一个 "\n\n" 分隔符，计入预算
        let add_cost = len + if selected.is_empty() { 0 } else { 2 };
        if idx == first || idx == last {
            // 首末段强制保留
            selected.push(idx);
            used += add_cost;
        } else if used + add_cost <= cap {
            selected.push(idx);
            used += add_cost;
        }
    }

    selected.sort_unstable();
    let mut result = selected
        .iter()
        .map(|&i| paragraphs[i].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    if result.chars().count() > cap {
        result = truncate_to_chars(&result, cap);
    }
    result
}

/// 逐块压缩多段历史后拼接，总长不超过 `target_chars + 200`。
pub fn compress_history_blocks(blocks: &[String], target_chars: usize) -> String {
    let blocks: Vec<&String> = blocks.iter().filter(|b| !b.is_empty()).collect();
    if blocks.is_empty() {
        return String::new();
    }
    let per = (target_chars / blocks.len()).max(1);
    let mut parts = Vec::with_capacity(blocks.len());
    for b in &blocks {
        parts.push(compress_text(b, per));
    }
    let mut result = parts.join("\n\n");
    let cap = target_chars + 200;
    if result.chars().count() > cap {
        result = truncate_to_chars(&result, cap);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn char_count(s: &str) -> usize {
        s.chars().count()
    }

    #[test]
    fn compress_keeps_length_bound_and_key_content() {
        let mut paras = vec![
            "故事开篇，清晨的薄雾笼罩着整个村庄。".to_string(),
        ];
        for i in 0..12 {
            paras.push(format!("第{}天，平静的日子里并无大事发生。", i));
        }
        paras.push("他突然发现一个惊人的真相，原来凶手就在身边。".to_string());
        for i in 0..8 {
            paras.push(format!("日复一日的流水账描写，没有任何意义。{}", i));
        }
        paras.push("最后的夜晚，他们终于决定回到故乡，与所有秘密告别。".to_string());
        let text = paras.join("\n\n");

        let out = compress_text(&text, 100);
        assert!(
            char_count(&out) <= 100 + 200,
            "压缩后应 ≤ target+200，实际 {}",
            char_count(&out)
        );
        assert!(out.contains("村庄"), "应保留首段：{}", out);
        assert!(out.contains("告别"), "应保留末段：{}", out);
        assert!(out.contains("真相"), "应保留关键词段落：{}", out);
    }

    #[test]
    fn short_text_returns_unchanged() {
        let text = "短文本不超过目标长度。";
        assert_eq!(compress_text(text, 100), text);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(compress_text("", 100), "");
    }

    #[test]
    fn history_blocks_total_length_bounded() {
        let blocks: Vec<String> = (0..3)
            .map(|i| {
                (0..40)
                    .map(|j| format!("第{i}章第{j}段：他忽然发现真相，决定回到过去。"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();
        let out = compress_history_blocks(&blocks, 100);
        assert!(
            char_count(&out) <= 100 + 200,
            "多块历史压缩后总长应受限，实际 {}",
            char_count(&out)
        );
        assert!(!out.is_empty());
    }

    #[test]
    fn empty_history_blocks_returns_empty() {
        assert_eq!(compress_history_blocks(&[], 100), "");
    }
}
