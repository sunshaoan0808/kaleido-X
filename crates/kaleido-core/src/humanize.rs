//! 去 AI 味检测与质量评分（确定性纯核，无 LLM）。
//!
//! 吞噬自 `op7418/humanizer-zh`（MIT，Claude Code Skill，484 行规则文档）。
//! 上游：翻译自 `blader/humanizer` + 参考 `hardikpandya/stop-slop` +
//! 维基 `Signs of AI writing`。本模块将其 24 类模式转写为中文正则/词表，
//! 并实现 5 维 50 分制质量评分。只检测不改写（改写由 LLM 档位/人工做）。

use serde::{Deserialize, Serialize};

// ── 命中 ────────────────────────────────────────────────────────────────────

/// 单条命中：模式编号（1-24）+ 片段 + 说明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanizeHit {
    pub pattern: u8,
    pub name: String,
    pub snippet: String,
}

// ── 报告 ────────────────────────────────────────────────────────────────────

/// 5 维评分（各 0-10）+ 总分 50 + 命中清单。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanizeReport {
    pub directness: u8,
    pub rhythm: u8,
    pub trust: u8,
    pub authenticity: u8,
    pub concision: u8,
    pub total: u8,
    #[serde(default)]
    pub hits: Vec<HumanizeHit>,
    /// 正文字符数（评分分母）。
    #[serde(default)]
    pub chars: usize,
}

impl HumanizeReport {
    pub fn grade(&self) -> &'static str {
        match self.total {
            45..=50 => "优秀",
            35..=44 => "良好",
            _ => "需修订",
        }
    }
}

// ── 词表（humanizer-zh 核心规则速查转写） ────────────────────────────────────

/// 高频 AI 词汇（模式 7 + 内容模式关键词子集）。
const AI_WORDS: &[&str] = &[
    "此外", "值得注意的是", "至关重要", "深入探讨", "强调", "持久的", "增强", "培养",
    "获得", "突出", "相互作用", "复杂性", "关键", "格局", "展示", "织锦", "证明",
    "宝贵的", "充满活力的", "彰显", "凸显", "象征着", "标志着", "见证了", "奠定基础",
    "不可磨灭", "深深植根", "转折点", "焦点", "致力于", "坐落于", "位于", "著名的",
    "令人叹为观止", "必游之地", "迷人的", "丰富的", "深刻的", "开创性的",
];

/// 填充短语（模式 22）。
const FILLER_PHRASES: &[&str] = &[
    "为了实现这一目标", "由于下雨的事实", "在这个时间点", "在您需要帮助的情况下",
    "系统具有处理的能力", "值得注意的是数据显示", "希望这对您有帮助", "请告诉我",
    "截至", "根据我最后的训练", "基于可用信息", "好问题", "您说得完全正确",
];

/// 协作交流痕迹（模式 19/21）。
const CHAT_TRACES: &[&str] = &[
    "希望这对您有帮助", "当然！", "一定！", "您说得完全正确", "您想要",
    "请告诉我如果您想", "这是一个", "让我扩展任何部分",
];

/// 通用积极结论（模式 24）。
const ROSY_CLOSERS: &[&str] = &[
    "未来看起来光明", "激动人心的时代", "追求卓越", "向正确方向迈出",
    "继续蓬勃发展", "不可或缺的一部分",
];

/// 模糊归因（模式 5）。
const VAGUE_ATTR: &[&str] = &[
    "行业报告显示", "观察者指出", "专家认为", "一些批评者认为", "多个来源",
    "研究人员和保护主义者",
];

fn push_hit(hits: &mut Vec<HumanizeHit>, pattern: u8, name: &str, text: &str, idx: usize) {
    let chars: Vec<char> = text.chars().collect();
    let s = idx.saturating_sub(12);
    let e = (idx + 24).min(chars.len());
    let snippet: String = chars[s..e].iter().collect();
    hits.push(HumanizeHit { pattern, name: name.into(), snippet: snippet.trim().into() });
}

fn find_all(hits: &mut Vec<HumanizeHit>, pattern: u8, name: &str, text: &str, words: &[&str]) {
    for w in words {
        let mut rest = text;
        let mut base = 0usize; // char offset
        while let Some(rel) = rest.find(w) {
            let rel_chars = rest[..rel].chars().count();
            push_hit(hits, pattern, name, text, base + rel_chars);
            let step = &rest[rel..rel + w.len()];
            base += rel_chars + step.chars().count();
            rest = &rest[rel + w.len()..];
            if rest.is_empty() { break; }
        }
    }
}

// ── 主入口 ──────────────────────────────────────────────────────────────────

/// 对正文跑 24 类中的确定性子集检测 + 5 维评分。
///
/// 覆盖：1 意义夸大/4 宣传/5 模糊归因（词表）→直接性；
/// 7 AI 词汇/22 填充/8 系动词回避（拥有/设有/提供+一个）→精炼度；
/// 9 否定排比（不仅…而且/不仅仅是…而是）/10 三段式（、…、…和…）/13 破折号（——）/14 粗体（**）→节奏；
/// 19 协作痕迹/20 免责/21 谄媚/24 积极结论 →真实性/信任度。
pub fn analyze(text: &str) -> HumanizeReport {
    let mut hits: Vec<HumanizeHit> = vec![];
    let chars_count = text.chars().count();

    // —— 词表类 ——
    find_all(&mut hits, 7, "AI 高频词", text, AI_WORDS);
    find_all(&mut hits, 22, "填充短语", text, FILLER_PHRASES);
    find_all(&mut hits, 19, "协作交流痕迹", text, CHAT_TRACES);
    find_all(&mut hits, 24, "通用积极结论", text, ROSY_CLOSERS);
    find_all(&mut hits, 5, "模糊归因", text, VAGUE_ATTR);

    // —— 结构类 ——
    // 9 否定式排比
    for pat in ["不仅仅是", "不仅是", "不只是"] {
        let mut rest = text;
        let mut base = 0usize;
        while let Some(rel) = rest.find(pat) {
            let rel_chars = rest[..rel].chars().count();
            let idx = base + rel_chars;
            let window: String = rest[rel..].chars().take(24).collect();
            if window.contains("而是") || window.contains("而且") {
                push_hit(&mut hits, 9, "否定式排比", text, idx);
            }
            base += rel_chars + pat.chars().count();
            rest = &rest[rel + pat.len()..];
            if rest.is_empty() { break; }
        }
    }
    // 13 破折号（每 2 个记 1 次，避免刷屏）
    {
        let n = text.matches('—').count() / 2 + text.matches("——").count();
        // text.matches('—') 与 "——" 重叠：直接数 "——" 出现次数
        let m = text.matches("——").count();
        let _ = n;
        let mut rest = text;
        let mut base = 0usize;
        let mut k = 0;
        while let Some(rel) = rest.find("——") {
            if k % 2 == 0 { push_hit(&mut hits, 13, "破折号", text, base + rest[..rel].chars().count()); }
            k += 1;
            base += rest[..rel].chars().count() + 2;
            rest = &rest[rel + 6..];
            if rest.is_empty() { break; }
        }
        let _ = m;
    }
    // 14 粗体
    {
        let mut rest = text;
        let mut base = 0usize;
        while let Some(rel) = rest.find("**") {
            push_hit(&mut hits, 14, "粗体强调", text, base + rest[..rel].chars().count());
            base += rest[..rel].chars().count() + 2;
            rest = &rest[rel + 2..];
            if rest.is_empty() { break; }
        }
    }
    // 10 三段式（顿号分隔三项 + 和/与/及）：粗检
    {
        let mut rest = text;
        let mut base = 0usize;
        while let Some(rel) = rest.find('、') {
            let rel_chars = rest[..rel].chars().count();
            let idx = base + rel_chars;
            let window: String = rest[rel..].chars().take(30).collect();
            let dun = window.matches('、').count();
            if dun >= 2 && (window.contains('和') || window.contains('与') || window.contains("及")) {
                push_hit(&mut hits, 10, "三段式列举", text, idx);
                let adv: String = rest[rel..].chars().take(10).collect();
                base += rel_chars + adv.chars().count();
                rest = &rest[rel + adv.len()..];
            } else {
                base += rel_chars + 1;
                rest = &rest[rel + '、'.len_utf8()..];
            }
            if rest.is_empty() { break; }
        }
    }
    // 8 系动词回避（拥有/设有一个 + 量词结构）: 简检“拥有/设有/提供”
    for w in ["拥有", "设有一个", "提供一个"] {
        let mut rest = text;
        let mut base = 0usize;
        while let Some(rel) = rest.find(w) {
            push_hit(&mut hits, 8, "系动词回避", text, base + rest[..rel].chars().count());
            base += rest[..rel].chars().count() + w.chars().count();
            rest = &rest[rel + w.len()..];
            if rest.is_empty() { break; }
        }
    }

    // —— 5 维评分（按命中密度扣分，每维 10 起） ——
    let count = |ps: &[u8]| hits.iter().filter(|h| ps.contains(&h.pattern)).count() as i32;
    let density = chars_count.max(1) as f32 / 1000.0; // 每千字归一
    let sub = |n: i32| ((n as f32 / density.max(0.2)).round() as i32).clamp(0, 10);
    let directness = (10 - sub(count(&[1, 4, 5, 7]))).max(0) as u8;
    let rhythm = (10 - sub(count(&[9, 10, 13, 14]))).max(0) as u8;
    let trust = (10 - sub(count(&[5, 19, 20, 21]))).max(0) as u8;
    let authenticity = (10 - sub(count(&[19, 21, 24]))).max(0) as u8;
    let concision = (10 - sub(count(&[7, 8, 22]))).max(0) as u8;
    let total = directness + rhythm + trust + authenticity + concision;

    HumanizeReport { directness, rhythm, trust, authenticity, concision, total, hits, chars: chars_count }
}

// ── 后处理（H5 确定性硬修） ─────────────────────────────────────────────────

/// 破折号硬修：中文叙事里“——”90% 可换逗号/句号。
/// 规则：夹在同样句式间 → 逗号；其余 → 句号。返回（新文本，替换数）。
pub fn hard_fix_dashes(text: &str) -> (String, usize) {
    let n = text.matches("——").count();
    if n == 0 { return (text.to_string(), 0); }
    // 启发式：前后都是汉字的“——像/像是/仿佛”解释性插入 → 逗号，否则句号
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(rel) = rest.find("——") {
        out.push_str(&rest[..rel]);
        let after: String = rest[rel + 6..].chars().take(8).collect();
        let first: String = after.chars().take(2).collect();
        if first.starts_with("像") || first.starts_with("仿") || first.starts_with("如") {
            out.push('，');
        } else {
            out.push('。');
        }
        rest = &rest[rel + 6..];
    }
    out.push_str(rest);
    (out, n)
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_prose_scores_high() {
        let r = analyze("雨下了一整夜。沈棠把伞收了，站在门口没动。");
        assert!(r.total >= 45, "total={}", r.total);
        assert!(r.hits.is_empty());
    }

    #[test]
    fn ai_words_hit() {
        let r = analyze("此外，这家店作为城市咖啡文化的焦点，彰显了其重要性。");
        assert!(r.hits.iter().any(|h| h.pattern == 7));
        assert!(r.directness < 10);
    }

    #[test]
    fn neg_parallel_hit() {
        let r = analyze("这不仅仅是一次更新，而是我们思考方式的革命。");
        assert!(r.hits.iter().any(|h| h.pattern == 9));
    }

    #[test]
    fn dash_hit() {
        let r = analyze("她看着银锁——那两朵白梅——没有说话。");
        assert!(r.hits.iter().any(|h| h.pattern == 13));
    }

    #[test]
    fn triple_hit() {
        let r = analyze("活动包括演讲、讨论和社交，与会者可以期待创新、灵感和洞察。");
        assert!(r.hits.iter().any(|h| h.pattern == 10));
    }

    #[test]
    fn bold_hit() {
        let r = analyze("它融合了 **OKR**、**KPI** 和画布。");
        assert!(r.hits.iter().any(|h| h.pattern == 14));
    }

    #[test]
    fn filler_hit() {
        let r = analyze("在这个时间点，系统具有处理的能力。");
        assert!(r.hits.iter().any(|h| h.pattern == 22));
    }

    #[test]
    fn dash_hard_fix() {
        let (out, n) = hard_fix_dashes("她看着银锁——那两朵白梅——没有说话。");
        assert_eq!(n, 2);
        assert!(!out.contains("——"));
        assert!(out.contains("，") || out.contains("。"));
        let (same, zero) = hard_fix_dashes("雨下了一整夜。");
        assert_eq!(zero, 0);
        assert_eq!(same, "雨下了一整夜。");
    }

    #[test]
    fn chat_trace_and_rosy() {
        let r = analyze("希望这对您有帮助！公司的未来看起来光明。");
        assert!(r.hits.iter().any(|h| h.pattern == 19 || h.pattern == 24));
        assert!(r.authenticity < 10);
    }
}
