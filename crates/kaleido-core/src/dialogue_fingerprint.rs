//! U6: 对话质量检测 —— 角色指纹 + 漂移分（T1 创作质量 · 第三优先）。
//!
//! 参考 Openwrite `dialogue_fingerprint.py` 四维思路，做成纯规则零 LLM：
//! 1. 应然指纹：从 `PackCharacterRef`（speech_style / personality / example_dialogs /
//!    boundaries）提取人称代词分布、语气词分布、句长特征、口头禅、越界词表。
//! 2. 实然检测：新对白 vs 指纹 → 漂移分 [0,1] + 越界引用 + 口头禅重复率。
//! 3. 可序列化为 per-character fingerprint json（前端展示 / 对白行标红）。

use serde::{Deserialize, Serialize};

use crate::story_tavern::PackCharacterRef;

// ─── 常见中文人称代词（第一人称倾向）────────────────────────────────────────
const PRONOUNS_1ST: &[&str] = &[
    "我", "咱", "俺", "吾", "在下", "老子", "本座", "本官", "本帅", "本将军",
    "人家", "奴家", "妾身", "哀家", "臣妾", "朕", "孤", "寡人", "贫道", "洒家",
    "小女子", "老身", "某", "小弟", "兄弟", "鄙人", "晚辈", "小的", "咱家",
];

// ─── 常见语气词（口语感）───────────────────────────────────────────────────
const PARTICLES: &[&str] = &[
    "啊", "呀", "吧", "呢", "嘛", "哦", "唉", "哟", "哈", "咯", "啦", "么",
    "哉", "兮", "罢了", "便是", "不成", "来着", "哇", "咧", "啵", "哩", "嘞",
];

// ─── 句子分隔符 ────────────────────────────────────────────────────────────
const SENT_SEP: &[char] = &['。', '！', '？', '；', '\n', '…', '.', '!', '?', ';'];

/// 单角色指纹（可序列化为 json，前端直接展示）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CharacterFingerprint {
    pub character_id: String,
    pub name: String,
    /// 人称代词频率表（词, 频次），按频次降序。
    pub pronouns: Vec<(String, u32)>,
    /// 语气词频率表（词, 频次），按频次降序。
    pub particles: Vec<(String, u32)>,
    /// 平均句长（字符数）。
    pub avg_sentence_len: f64,
    /// 短句（<=8 字）占比 [0,1]。
    pub short_sentence_ratio: f64,
    /// 口头禅：example_dialogs 高频 2-gram，取 top N（出现 >=2 次）。
    pub catchphrases: Vec<String>,
    /// 越界禁忌词（boundaries 原文，命中即告警）。
    pub boundaries: Vec<String>,
    /// 风格描述（speech_style / personality 摘要，给前端提示）。
    pub style_note: String,
    /// 指纹依据：example_dialogs 条数。
    pub sample_count: usize,
}

/// 漂移检测报告。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DriftReport {
    pub character_id: String,
    /// 综合漂移分 [0,1]，越高越离谱。
    pub drift_score: f64,
    pub pronoun_drift: f64,
    pub particle_drift: f64,
    pub sentence_drift: f64,
    /// 口头禅命中次数。
    pub catchphrase_hits: Vec<String>,
    /// 越界词命中（boundaries 中的词出现在新对白）。
    pub boundary_hits: Vec<String>,
    /// 中文可读的原因摘要。
    pub reasons: Vec<String>,
}

// ─── 工具：切句 ────────────────────────────────────────────────────────────
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if SENT_SEP.contains(&ch) {
            let s = cur.trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
            cur.clear();
        }
    }
    let tail = cur.trim().to_string();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

/// 统计词表在文本中的频次。
fn count_words(text: &str, words: &[&str]) -> Vec<(String, u32)> {
    let mut counts: Vec<(String, u32)> = Vec::new();
    for w in words {
        let n = text.matches(w).count() as u32;
        if n > 0 {
            counts.push((w.to_string(), n));
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts
}

/// 提取 2-gram 高频词（口头禅候选）。
fn top_bigrams(texts: &[String], top_n: usize) -> Vec<String> {
    use std::collections::HashMap;
    let mut freq: HashMap<String, u32> = HashMap::new();
    for t in texts {
        let chars: Vec<char> = t.chars().collect();
        if chars.len() < 2 {
            continue;
        }
        for i in 0..chars.len() - 1 {
            // 跳过含标点的 bigram（“你。”这种不算口头禅）
            if chars[i].is_ascii_punctuation()
                || chars[i + 1].is_ascii_punctuation()
                || is_cn_punct(chars[i])
                || is_cn_punct(chars[i + 1])
            {
                continue;
            }
            let mut b = String::new();
            b.push(chars[i]);
            b.push(chars[i + 1]);
            *freq.entry(b).or_insert(0) += 1;
        }
    }
    let mut items: Vec<(String, u32)> = freq.into_iter().filter(|(_, n)| *n >= 2).collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    items.truncate(top_n);
    items.into_iter().map(|(w, _)| w).collect()
}

fn is_cn_punct(c: char) -> bool {
    matches!(
        c,
        '，' | '。' | '！' | '？' | '；' | '：' | '、' | '（' | '）' | '「' | '」' | '『' | '』' | '…' | '·' | '“' | '”' | '‘' | '’'
    )
}

fn clamp01(v: f64) -> f64 {
    if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

/// 从角色卡构建应然指纹。
pub fn build_fingerprint(c: &PackCharacterRef) -> CharacterFingerprint {
    let mut sample = String::new();
    let n = c.example_dialogs.len();
    for d in &c.example_dialogs {
        sample.push_str(d);
        sample.push('\n');
    }

    let sentences = split_sentences(&sample);
    let avg_len = if sentences.is_empty() {
        0.0
    } else {
        let total: usize = sentences.iter().map(|s| s.chars().count()).sum();
        total as f64 / sentences.len() as f64
    };
    let short_ratio = if sentences.is_empty() {
        0.0
    } else {
        let short = sentences.iter().filter(|s| s.chars().count() <= 8).count();
        short as f64 / sentences.len() as f64
    };

    let mut style_note = c.speech_style.clone();
    if !c.personality.is_empty() {
        if !style_note.is_empty() {
            style_note.push('；');
        }
        style_note.push_str(&c.personality);
    }

    CharacterFingerprint {
        character_id: c.id.clone(),
        name: c.name.clone(),
        pronouns: count_words(&sample, PRONOUNS_1ST),
        particles: count_words(&sample, PARTICLES),
        avg_sentence_len: avg_len,
        short_sentence_ratio: short_ratio,
        catchphrases: top_bigrams(&c.example_dialogs, 5),
        boundaries: c.boundaries.clone(),
        style_note,
        sample_count: n,
    }
}

/// 分布距离（0=相同，1=完全不同）。把 (词,频) 表投影到固定词表上比较。
fn dist_profile(a: &[(String, u32)], b: &[(String, u32)]) -> f64 {
    // 取并集词表，比较归一化占比的差平方均值。
    let mut keys: Vec<&str> = Vec::new();
    for (w, _) in a.iter().chain(b.iter()) {
        if !keys.iter().any(|k| *k == w.as_str()) {
            keys.push(w.as_str());
        }
    }
    if keys.is_empty() {
        return 0.0;
    }
    let ta: u32 = a.iter().map(|(_, n)| n).sum();
    let tb: u32 = b.iter().map(|(_, n)| n).sum();
    if ta == 0 && tb == 0 {
        return 0.0;
    }
    let mut sum_sq = 0.0;
    for &k in &keys {
        let pa = a.iter().find(|(w, _)| w.as_str() == k).map(|(_, n)| *n as f64 / ta.max(1) as f64).unwrap_or(0.0);
        let pb = b.iter().find(|(w, _)| w.as_str() == k).map(|(_, n)| *n as f64 / tb.max(1) as f64).unwrap_or(0.0);
        let d = pa - pb;
        sum_sq += d * d;
    }
    clamp01((sum_sq / keys.len() as f64).sqrt())
}

/// 检测：新对白 vs 指纹 → 漂移报告。
/// 「自称X」边界的身份自称代词映射（词面匹配失效时的语义归一）。
/// 如 boundary=「自称陛下」→ 角色台词中出现 朕/孤/寡人 即视为越界。
fn self_title_aliases(title: &str) -> &'static [&'static str] {
    if title.contains("陛下") || title.contains("皇帝") || title.contains("皇上") || title.contains("圣上")
    {
        &["朕", "孤", "寡人"]
    } else if title.contains("本座") {
        &["本座"]
    } else if title.contains("哀家") || title.contains("太后") {
        &["哀家", "本宫"]
    } else if title.contains("贫道") {
        &["贫道", "小道"]
    } else if title.contains("洒家") {
        &["洒家", "俺"]
    } else if title.contains("老子") || title.contains("大爷") {
        &["老子", "爷"]
    } else if title.contains("下官") || title.contains("本官") {
        &["本官", "下官"]
    } else if title.contains("臣妾") || title.contains("妾") {
        &["臣妾", "妾身"]
    } else if title.contains("奴") {
        &["奴家", "奴婢"]
    } else {
        &[]
    }
}

pub fn drift_check(fp: &CharacterFingerprint, content: &str) -> DriftReport {
    let clean: String = content.trim().to_string();
    let mut reasons = Vec::new();

    // 1. 人称漂移
    let observed_pronouns = count_words(&clean, PRONOUNS_1ST);
    let pronoun_drift = dist_profile(&fp.pronouns, &observed_pronouns);
    if pronoun_drift > 0.5 && !fp.pronouns.is_empty() {
        reasons.push(format!("人称习惯偏移（漂移 {:.2}）", pronoun_drift));
    }

    // 2. 语气词漂移
    let observed_particles = count_words(&clean, PARTICLES);
    let particle_drift = dist_profile(&fp.particles, &observed_particles);
    if particle_drift > 0.6 && !fp.particles.is_empty() {
        reasons.push(format!("语气词风格偏移（漂移 {:.2}）", particle_drift));
    }

    // 3. 句长漂移（绝对差/基准）
    let sentences = split_sentences(&clean);
    let sentence_drift = if sentences.is_empty() || fp.avg_sentence_len <= 0.0 {
        0.0
    } else {
        let avg: f64 = sentences.iter().map(|s| s.chars().count() as f64).sum::<f64>() / sentences.len() as f64;
        let rel = (avg - fp.avg_sentence_len).abs() / fp.avg_sentence_len.max(1.0);
        clamp01(rel / 2.0)
    };
    if sentence_drift > 0.5 {
        reasons.push(format!("句长习惯偏移（漂移 {:.2}）", sentence_drift));
    }

    // 4. 口头禅命中（重复率）
    let mut catchphrase_hits = Vec::new();
    for cp in &fp.catchphrases {
        if clean.contains(cp.as_str()) {
            catchphrase_hits.push(cp.clone());
        }
    }
    if !catchphrase_hits.is_empty() {
        reasons.push(format!("口头禅重复：{}", catchphrase_hits.join("、")));
    }

    // 5. 越界引用（词面命中 + 「自称X」语义归一）
    // boundaries 可能存语义标签（如「自称陛下」「皇帝」）而非台词原文：
    // 词面 contains 对「自称陛下」永远不命中（角色不会说这四个字），
    // 故对「自称X」边界解析出身份自称代词（朕/孤/寡人…）做语义检测。
    let mut boundary_hits = Vec::new();
    for b in &fp.boundaries {
        let btrim = b.trim();
        if btrim.is_empty() {
            continue;
        }
        let mut hit = btrim.len() >= 2 && clean.contains(btrim);
        if !hit {
            if let Some(rest) = btrim.strip_prefix("自称") {
                if self_title_aliases(rest.trim()).iter().any(|a| clean.contains(a)) {
                    hit = true;
                }
            }
        }
        if hit {
            boundary_hits.push(b.clone());
        }
    }
    if !boundary_hits.is_empty() {
        reasons.push(format!("越界引用：{}", boundary_hits.join("、")));
    }

    // 综合分：人称 0.25 + 语气 0.2 + 句长 0.25 + 口头禅 0.15 + 越界 0.15
    let mut score = pronoun_drift * 0.25
        + particle_drift * 0.20
        + sentence_drift * 0.25
        + (if catchphrase_hits.is_empty() { 0.0 } else { 0.15 })
        + (if boundary_hits.is_empty() { 0.0 } else { 0.15 });
    // 样本太少（无 example_dialogs）时降权，避免空指纹也报漂移
    if fp.sample_count == 0 {
        score *= 0.4;
        if score > 0.01 {
            reasons.push("该角色无示例对白，指纹置信度低".to_string());
        }
    }

    DriftReport {
        character_id: fp.character_id.clone(),
        drift_score: clamp01(score),
        pronoun_drift,
        particle_drift,
        sentence_drift,
        catchphrase_hits,
        boundary_hits,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_char() -> PackCharacterRef {
        PackCharacterRef {
            id: "c1".into(),
            name: "沈棠".into(),
            role: "关键 NPC".into(),
            gender: String::new(),
            appearance: String::new(),
            opening_scene: String::new(),
            opening_lines: String::new(),
            nsfw_profile: String::new(),
            importance: "high".into(),
            content_tier: None,
            example_dialogs: vec![
                "我在这儿住了十年了。".into(),
                "罢了，你若执意要去，我也不拦你。".into(),
                "今夜风大，早些歇息吧。".into(),
                "我啊，早就不信那些了。".into(),
                "去吧，路上小心些。".into(),
            ],
            boundaries: vec!["自称陛下".into(), "皇帝".into()],
            personality: "温和内敛".into(),
            speech_style: "短句，轻声".into(),
            voice_profile: String::new(),
            motivation: String::new(),
            relationships: Vec::new(),
            evidence_refs: Vec::new(),
            mental_models: Vec::new(),
            decision_heuristics: Vec::new(),
            beliefs: Vec::new(),
            expressions: Default::default(),
            voice: None,
            archive: None,
            avatar: None,
                starting_wardrobe: Default::default(),
        }
    }

    #[test]
    fn fingerprint_basic() {
        let fp = build_fingerprint(&sample_char());
        assert_eq!(fp.character_id, "c1");
        assert!(!fp.pronouns.is_empty());
        assert!(fp.avg_sentence_len > 0.0);
        assert!(fp.catchphrases.len() <= 5);
        assert_eq!(fp.boundaries.len(), 2);
    }

    #[test]
    fn drift_consistent() {
        let fp = build_fingerprint(&sample_char());
        // 同风格对白 → 低分
        let ok = drift_check(&fp, "罢了，你若非去不可，我也不拦你。路上小心些。");
        assert!(ok.drift_score < 0.6, "score={}", ok.drift_score);
        // 越界 + 句长暴增 → 高分
        let bad = drift_check(&fp, "朕乃九五之尊，统御四海八荒九州万方，天下苍生皆俯首称臣，尔等区区草民焉敢造次？");
        assert!(bad.drift_score > 0.5, "score={}", bad.drift_score);
        assert!(!bad.boundary_hits.is_empty());
        // 「自称X」语义归一：词面不含 boundary 词，但自称代词命中 → 越界
        let self_title = drift_check(
            &fp,
            "孤已派人查过，此事与尔等无关。退下吧。",
        );
        assert!(
            !self_title.boundary_hits.is_empty(),
            "自称陛下 → 台词含「孤」应命中越界: {:?}",
            self_title.boundary_hits
        );
        // 词面命中仍有效
        let literal = drift_check(&fp, "皇帝老儿也敢拦我？");
        assert!(!literal.boundary_hits.is_empty());
    }
}

/// 回合声线检测：正文按“角色名：/：”切出台词 → per 角色 drift_check → 取最高分。
/// names：角色名 + id（双口径）。返回 (character_id, DriftReport)（无台词/无指纹则 None）。
pub fn check_turn_voices(
    fps: &[CharacterFingerprint],
    names: &[(String, String)],
    text: &str,
) -> Option<(String, DriftReport)> {
    let mut best: Option<(String, DriftReport)> = None;
    for (cid, name) in names {
        let nm = name.trim();
        if nm.is_empty() { continue; }
        // 角色名出现门：出现 ≥2 次才算（场景标签那 1 次不算数，
        // 防“只在场景标签出现”被算漂移，如 turn 34 沈棠）
        if text.matches(nm).count() < 2 { continue; }
        // 切出台词：按 “名：”/“名:”/“「” 分段
        let mut lines: Vec<String> = Vec::new();
        for sep in [format!("{}：", name), format!("{}:", name)] {
            let mut rest = text;
            while let Some(pos) = rest.find(sep.as_str()) {
                let after = &rest[pos + sep.len()..];
                // 取到行尾/下一段
                let end = after.find('\n').unwrap_or(after.len());
                let line = after[..end].trim().to_string();
                if line.chars().count() >= 2 {
                    lines.push(line);
                }
                rest = &after[end.min(after.len())..];
                if rest.is_empty() { break; }
            }
        }
        let fp = match fps.iter().find(|f| f.character_id == *cid || f.name == *name) {
            Some(f) => f,
            None => continue,
        };
        if fp.sample_count == 0 { continue; }
        // 整段回退：无显式“名：”前缀时（叙事体正文），直接整段比对
        // （口头禅/语气词/句长分布仍有信号；归因精度低于切分，阈值侧已用 0.6 过滤）
        let joined = if lines.is_empty() { text.to_string() } else { lines.join("\n") };
        let rep = drift_check(fp, &joined);
        let replace = match &best {
            None => true,
            Some((_, b)) => rep.drift_score > b.drift_score,
        };
        if replace {
            best = Some((cid.clone(), rep));
        }
    }
    best
}

#[cfg(test)]
mod turn_voices_tests {
    use super::*;
    use serde_json::json;
    fn fp_pack() -> crate::StoryPack {
        let c: PackCharacterRef = serde_json::from_value(json!({
            "id": "c1", "name": "沈棠", "role": "NPC",
            "exampleDialogs": ["我在这儿住了十年了。", "罢了，你若执意要去，我也不拦你。", "今夜风大，早些歇息吧。"],
            "boundaries": ["自称陛下"], "speechStyle": "短句"
        })).unwrap();
        let pack_json = json!({"id": "p", "title": "t", "characters": []});
        let mut pack: crate::StoryPack = serde_json::from_value(pack_json).unwrap();
        pack.characters = vec![c];
        pack
    }
    #[test]
    fn turn_voices_consistent_low() {
        let pack = fp_pack();
        let fps = build_all(&pack);
        let names = vec![("c1".into(), "沈棠".into())];
        let text = "沈棠：罢了，你若非去不可，我也不拦你。\n沈棠：今夜风大，早些歇息吧。";
        let r = check_turn_voices(&fps, &names, text).unwrap();
        assert!(r.1.drift_score < 0.6, "score={}", r.1.drift_score);
    }
    #[test]
    fn turn_voices_boundary_high() {
        let pack = fp_pack();
        let fps = build_all(&pack);
        let names = vec![("c1".into(), "沈棠".into())];
        let text = "沈棠：孤已派人查过，此事与尔等无关。退下吧。沈棠转身离去。";
        let r = check_turn_voices(&fps, &names, text).unwrap();
        assert!(!r.1.boundary_hits.is_empty());
        assert!(r.1.drift_score > 0.1, "score={}", r.1.drift_score);
    }
    #[test]
    fn turn_voices_name_gate() {
        // 角色名出现门：正文无角色名 → None（防整段污染）
        let pack = fp_pack();
        let fps = build_all(&pack);
        let names = vec![("c1".into(), "沈棠".into())];
        assert!(check_turn_voices(&fps, &names, "雨很大，巷子很深，无人说话。").is_none());
    }
    #[test]
    fn turn_voices_whole_fallback() {
        // 整段回退：有角色名但无显式前缀 → 整段比对，有信号
        let pack = fp_pack();
        let fps = build_all(&pack);
        let names = vec![("c1".into(), "沈棠".into())];
        let r = check_turn_voices(&fps, &names, "沈棠站在雨中，久久没有说话。沈棠抬起头。");
        assert!(r.is_some());
    }
}

/// 供 server 层调用：一次生成多角色指纹（含空角色卡占位）。
pub fn build_all(pack: &crate::StoryPack) -> Vec<CharacterFingerprint> {
    pack.characters.iter().map(build_fingerprint).collect()
}