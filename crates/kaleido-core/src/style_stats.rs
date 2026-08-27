//! 文风确定性统计（吸收自 oh-story-claudecode `style-profile-generator.md` Step 4，2026-08-15）。
//!
//! 中文网文文风画像的第一步是**确定性量化**（不是 LLM 抽样估计）：
//! - 句长分布：短句(<15字)/中句(15-30)/长句(>30) 占比 + 平均句长
//! - 标点密度：全角标点占非空白字符比例
//! - 段落节奏：平均段长、单段单动作 vs 多动作堆叠
//!
//! 纯函数、无 IO、无 LLM 依赖——供 pack 文风分析/蒸馏风格画像复用。
//! 原实现为 Python 脚本，本模块等价移植（分句正则、标点集合一致）。

/// 全角中文标点 + 常见英文标点（与源项目 `puncts` 统计集合一致）。
const PUNCT_CHARS: &str = "，。！？；：、…—\"\"''（）《》【】";

/// 句长分布统计结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentenceStats {
    pub total: usize,
    /// 短句(<15 字符)占比 0-100
    pub short_lt15_pct: usize,
    /// 中句(15-30)占比 0-100
    pub mid_15to30_pct: usize,
    /// 长句(>30)占比 0-100
    pub long_gt30_pct: usize,
    /// 平均句长（字符，向下取整）
    pub avg_len: usize,
    /// 标点密度（标点数/非空白字符数）0-100
    pub punct_density_pct: usize,
}

/// 分句：按中文句末标点切分，返回句子切片（不含标点，跳过空句）。
fn split_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '。' | '！' | '？') {
            let sent = &text[start..i];
            if !sent.trim().is_empty() {
                out.push(sent.trim());
            }
            start = i + c.len_utf8();
        }
    }
    if start < text.len() {
        let tail = &text[start..];
        if !tail.trim().is_empty() {
            out.push(tail.trim());
        }
    }
    out
}

/// 计算文风统计指标（与 oh-story Step 4 的 Python 1-liner 等价）：
/// - sentences = 按 [。！？]+ 分句数
/// - short/mid/long = <15 / 15..=30 / >30 字符占比（整数百分比）
/// - avg_len = 总句长 / 句数（向下取整）
/// - punct_density = 标点数 / 非空白字符数（百分比）
pub fn compute_style_stats(text: &str) -> SentenceStats {
    let sents = split_sentences(text);
    let total = sents.len().max(1);
    let short = sents.iter().filter(|s| s.chars().count() < 15).count();
    let mid = sents
        .iter()
        .filter(|s| {
            let n = s.chars().count();
            (15..=30).contains(&n)
        })
        .count();
    let lng = sents.iter().filter(|s| s.chars().count() > 30).count();

    let chars_nonws = text.chars().filter(|c| !c.is_whitespace()).count().max(1);
    let puncts = text.chars().filter(|c| PUNCT_CHARS.contains(*c)).count();
    let total_len: usize = sents.iter().map(|s| s.chars().count()).sum();

    SentenceStats {
        total: sents.len(),
        short_lt15_pct: short * 100 / total,
        mid_15to30_pct: mid * 100 / total,
        long_gt30_pct: lng * 100 / total,
        avg_len: total_len / total,
        punct_density_pct: puncts * 100 / chars_nonws,
    }
}

/// 段落节奏：平均段长（字符/段）+ 段数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParagraphStats {
    pub paragraphs: usize,
    pub avg_para_len: usize,
}

/// 按空行切段，统计平均段长。
pub fn compute_paragraph_stats(text: &str) -> ParagraphStats {
    let paras: Vec<&str> = text
        .split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let n = paras.len().max(1);
    let total: usize = paras.iter().map(|p| p.chars().count()).sum();
    ParagraphStats {
        paragraphs: paras.len(),
        avg_para_len: total / n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sentences_dominate() {
        let text = "他来了。她走了。天黑了。雨下了。";
        let s = compute_style_stats(text);
        assert_eq!(s.total, 4);
        // 4 句都 <15 字符
        assert_eq!(s.short_lt15_pct, 100);
        assert_eq!(s.mid_15to30_pct, 0);
        assert_eq!(s.long_gt30_pct, 0);
        assert!(s.avg_len < 15);
        // 标点密度 = 4 个 。/ 非空白字符（4句每句3字=12字+4标点=16）
        assert!(s.punct_density_pct > 0);
    }

    #[test]
    fn mixed_sentence_lengths() {
        // 短句 + 长句混合
        let text = "他来了。这是一个非常非常非常非常非常非常非常非常非常非常长的句子用来测试长句统计是否正确。";
        let s = compute_style_stats(text);
        assert_eq!(s.total, 2);
        assert_eq!(s.short_lt15_pct, 50);
        assert_eq!(s.long_gt30_pct, 50);
    }

    #[test]
    fn empty_text_is_safe() {
        let s = compute_style_stats("");
        assert_eq!(s.total, 0);
        assert_eq!(s.short_lt15_pct, 0);
        assert_eq!(s.punct_density_pct, 0);
    }

    #[test]
    fn punctuation_density_counts_fullwidth() {
        let text = "你好，世界。你好，世界！";
        let s = compute_style_stats(text);
        // 非空白字符：你 好 世 界 ×2 = 8 字 + 4 标点 = 12
        // 标点：，。，！ = 4 → 4/12 = 33%
        assert_eq!(s.punct_density_pct, 33);
    }

    #[test]
    fn paragraph_stats_splits_on_blank_lines() {
        let text = "第一段。\n\n第二段。\n\n第三段。";
        let p = compute_paragraph_stats(text);
        assert_eq!(p.paragraphs, 3);
        assert!(p.avg_para_len > 0);
    }

    #[test]
    fn deterministic_sentence_distribution() {
        // 确定性验证：6 句中 5 短（<15字）1 中（15-30字），预期可精确断言
        let text = "夜色如墨。\n他站在城墙上，望着远处的火光。\n风很冷。\n她握紧了剑柄，眼神坚定而决绝，仿佛已经做好了赴死的准备。\n远处传来战鼓声。\n雷声滚滚，夹杂着士兵的呐喊。";
        let s = compute_style_stats(text);
        assert_eq!(s.total, 6);
        assert_eq!(s.short_lt15_pct, 83);
        assert_eq!(s.mid_15to30_pct, 16);
        assert_eq!(s.long_gt30_pct, 0);
    }
}
