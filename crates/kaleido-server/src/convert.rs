//! 角色蒸馏模块（"小说→剧本包"转换流水线第一步）。
//!
//! 目标：把 LLM 启发式抽取的小说角色（只有名字、其余字段全空）蒸馏成"有血有肉"的
//! `PackCharacterRef`（参考"仓颉 7 级证据法" + "女娲认知卡结构"）。
//! 手段：向量检索召回原文证据 + LLM 依据"只许从原文提炼、不得编造"约束产出认知卡 JSON。
//!
//! - [`EvidenceBlock`]：证据块（章节号 + 原文片段），检索的最小单元。
//! - [`cosine_top_k`]：手写余弦相似度 top-k（纯函数，可测试）。
//! - [`distill_characters_system_prompt`]：角色认知卡系统提示词。
//! - [`retrieve_character_evidence`]：按名字+别名从章节正文召回原文证据。
//! - [`distill_pack_characters`]：蒸馏整部小说的角色卡列表。

use crate::AppState;
use futures_util::future::join_all;
use kaleido_core::{
    ActorStatePackConfig, EventPackage, NodeExit, PackCharacterRef, RuleSystem, TellerEventCard,
};
use serde_json::{json, Value};

/// 证据块：某章的一段原文（向量检索的最小单元）。
#[derive(Debug, Clone)]
pub struct EvidenceBlock {
    /// 章节标识（如 "ch01"），用于 evidence_refs 与检索展示前缀。
    pub chapter: String,
    /// 原文片段。
    pub text: String,
}

/// 单块目标字数 ~800、相邻块重叠 ~100 字（避免证据被切断裂在关键句处）。
const BLOCK_SIZE: usize = 800;
const BLOCK_OVERLAP: usize = 100;
/// 角色谱 / 角色卡 LLM 超时（秒）。
const LLM_TIMEOUT_SECS: u64 = 240;
/// 单个角色召回的证据片段数上限。
const EVIDENCE_TOP_K: usize = 6;
/// 蒸馏角色数量上限（importance 高的优先）。
// [fix 2026-08-15] 8→12：角色谱 prompt 强化后 10-50 章书可抽出 ≥10 角色，
// 原 8 上限会截断配角蒸馏卡（红姐/山楂/冯婷等只有名字无卡）。
const MAX_DISTIL_CHARS: usize = 12;
/// 角色谱阶段每个章节取出摘要的最大字符数。
#[allow(dead_code)] // [P7] roster 分片阈值预留
const ROSTER_CHUNK_CHARS: usize = 1600;
/// 确定性伪向量的维度（纯函数自检用，不依赖 embed 模型）。
#[allow(dead_code)]
const SIG_DIM: usize = 64;
/// 世界树（Stage 3）：单次盘点后最多蒸馏的实体数（每个实体一次 LLM 调用）。
const MAX_WORLD_ENTITIES: usize = 12;
/// 世界树：单个实体召回的原文证据块数。
const WORLD_EVIDENCE_TOP_K: usize = 4;
/// 节拍（Stage 3）：每章允许的硬节拍条数上限。
const MAX_BEATS: usize = 3;
/// 节拍（Stage 3）：单条硬节拍最大字数。
const BEAT_MAX_LEN: usize = 40;
/// 出口（Stage 3）：每个节点生成的候选出口数上限。
const MAX_EXITS: usize = 3;
/// 素材库蒸馏：事件包数量上限。
const MAX_EVENT_PACKAGES: usize = 4;
/// 素材库蒸馏：单包事件卡数量上限。
const MAX_EVENT_CARDS: usize = 5;
/// 素材库蒸馏：规则检定条数上限。
const MAX_RULE_CHECKS: usize = 8;
/// 素材库蒸馏：规则检定条数下限（低于此值视为不合格，但解析仍返回已收集部分）。
const MIN_RULE_CHECKS: usize = 4;

/// 手写余弦相似度（两组等长标量向量）。
fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na2 = 0.0f64;
    let mut nb2 = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += (x as f64) * (y as f64);
        na2 += (x as f64) * (x as f64);
        nb2 += (y as f64) * (y as f64);
    }
    let denom = na2.sqrt() * nb2.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    (dot / denom) as f32
}

/// 分析文本生成确定性固定维度签名向量（字符出现计数 → L2 归一化）。
/// 仅用于让 [`cosine_top_k`] 在无 embed 模型时也能独立完成可复现的匹配与测试。
#[allow(dead_code)]
fn block_signature(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; SIG_DIM];
    for ch in text.chars().flat_map(|c| c.to_lowercase()) {
        let code = ch as u32;
        let i = (code % SIG_DIM as u32) as usize;
        v[i] += 1.0;
        let j = (code.wrapping_add(31) % SIG_DIM as u32) as usize;
        v[j] += 0.5;
    }
    let norm = v.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x = (*x as f64 / norm) as f32;
        }
    }
    v
}

/// 手写余弦相似度 top-k 检索。
///
/// 对每个 `EvidenceBlock.text` 计算与 `query` 向量的余弦相似度，返回相似度最高的
/// `k` 个 `(index, score)`，按 score 降序；不足 `k` 个时返回全部。
///
/// 说明：为保持纯函数、可脱离 embed 模型测试，这里把块正文经 [`block_signature`]
/// 映射为定维向量后与 `query` 比较；真实蒸馏流程里的向量检索见 [`retrieve_character_evidence`]。
#[allow(dead_code)]
pub fn cosine_top_k(query: &[f32], blocks: &[EvidenceBlock], k: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (i, cosine_sim(query, &block_signature(&b.text))))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
}

/// 在已向量化的嵌入列表里做余弦 top-k（供真实检索路径用，向量维度一致）。
#[allow(dead_code)] // [P7] 向量检索辅助预留
fn top_k_vec(query: &[f32], embs: &[Vec<f32>], k: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = embs
        .iter()
        .enumerate()
        .map(|(i, e)| (i, cosine_sim(query, e)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
}

/// 把一段正文字符串按 `block_size` 字、重叠 `overlap` 字切成若干块（按 char 边界）。
/// 返回 trim 后非空的块；空输入返回空数组。
fn split_into_blocks(text: &str, block_size: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![];
    }
    let bs = block_size.max(1);
    let step = bs.saturating_sub(overlap).max(1);
    let mut blocks = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let end = (i + bs).min(chars.len());
        let piece: String = chars[i..end].iter().collect();
        let trimmed = piece.trim().to_string();
        if !trimmed.is_empty() {
            blocks.push(trimmed);
        }
        if end >= chars.len() {
            break;
        }
        i += step;
    }
    if blocks.is_empty() {
        blocks.push(text.to_string());
    }
    blocks
}

/// 角色认知卡系统提示词（仓颉 7 级证据法 + 女娲认知卡结构 + TavernWeave 模块契约，输出严格 JSON）。
///
/// 模块契约设计(吸收 TavernWeave cot-design 方法论):
/// 每个字段=一个模块,有明确"要回答的问题/输入/产出/跳过条件/降级";
/// 所有结论必须 evidence_refs 可溯源(我们的差异优势,保留);
/// 证据不足的字段显式降级为"未知",绝不脑补。
pub fn distill_characters_system_prompt() -> String {
    r#"你是资深小说角色分析专家。我会提供若干段经过向量检索召回的小说原文证据，请基于这些证据为指定角色建立"认知卡"。

## 全局约束(所有模块适用)
1. 只许从提供的原文证据中提炼，绝不编造、不脑补剧情之外的信息；每条结论必须能在证据里找到依据。
2. 证据等级参考（若证据覆盖则优先采用）：
   ① 代表金句 —— 最能体现其性格与说话方式的原文台词（性格/声线归纳**优先**使用台词证据）
   ② 信念形成故事 —— 角色"我信X是因为Y"的来源
   ③ 关键决策与复盘 —— 角色在节点上做过什么选择、之后如何反思
   ④ 失败与承认 —— 角色承认过错、示弱的时刻
   ⑤ 内在矛盾 —— 言行不一、愿望与压抑的冲突
   ⑥ 认知边界 —— 角色不知道、不愿承认或误解的事
   ⑦ 思维习惯 —— 反复出现的判断方式/口头推理
3. 模块按顺序执行，每个模块先判断"证据是否支撑"，不支撑则按该模块降级规则处理，不得跳过后端模块。

## 模块契约
### M1 身份模块 —— 回答"角色是谁"
- 输入:证据中的称谓/身份/场景描述
- 产出:name(角色名), identity(身份), gender(性别), appearance(外貌)
- gender 降级:证据无法判断("他/她/母亲/父亲"等可推)写 "未知"
- appearance 降级:证据没有外貌描写写 "未知"，绝不脑补
### M2 性格模块 —— 回答"角色怎么处事"
- 输入:行为/决策/复盘证据 + **代表金句（证据等级①，优先）**
- 产出:
  - personality(2-4句，从具体行为与决策归纳，不要标签堆砌；**必须涵盖性格的"带刺面"与"柔软面"两面**，若原文存在直接/泼辣/强硬等互动证据，不得只保留概括性印象)
  - voice_profile(声线特征，1-2句：角色**默认语气**——如"泼辣嗔怪""嘴硬心软""阴沉寡言"——必须包含至少 1 条来自代表金句的证据支撑；严禁用"时而…时而…"稀释成多变无默认)
  - speech_style(口头禅/句式/用词/称谓习惯；**双段式**："默认语气：<一句话>" + "变化范围：<在什么情境下语气怎么变>")
- 约束:性格结论必须能回指证据；**声线结论必须优先引用台词原文（代表金句），不得只依据行为归纳**
### M3 台词模块 —— 回答"角色怎么说话"
- 输入:原文对话证据
- 产出:example_dialogs(必须是原文中出现过的台词，可适度截取), boundaries(该角色绝不会做、或正文中明示的行为底线/禁忌)
### M4 动机模块 —— 回答"角色为什么这么做"
- 产出:motivation(深层动机与目标层级：外显目标 → 内在渴望 → 恐惧/创伤驱动)
- 约束:三层都要有证据支撑，无证据的层级写"证据不足"
### M5 关系模块 —— 回答"角色和谁什么关系"
- 产出:relationships(格式"他人名:关系描述"，需能回到原文)
### M6 认知模块 —— 回答"角色怎么看世界"
- 产出:mental_models(心智模型，3-7条；格式"模型名：一句话描述（适用场景，证据ch）")
- 产出:decision_heuristics(决策启发式，5-10条；格式"规则名:触发场景→做法（案例证据ch）")
- 产出:beliefs(信念形成故事，2-5条；格式"信念:形成它的故事/经历（证据ch）")
### M7 演出模块 —— 回答"角色怎么开场"
- 产出:opening_scene(开场场景画布 2-4 句：地点/时间/在场人物/环境氛围/角色状态，从最常见的出场情境提炼，证据无则"未知")
- 产出:opening_lines(完整开场白 first_mes，60-200字，第一人称沉浸式。必须依次含：①场景画布 ②角色视角与即时情境 ③对方可行动的位置 ④关键约束或悬念 ⑤信息收尾。体现角色声音，不剧透后续情节。证据不足时用证据能支撑的表演，绝不脑补未出现的事实)
- ST-26 开场时间线守卫: opening_scene 与 opening_lines 只能取自该角色**最早出场章节（全书前 1/3 内、且为最早出现的日常/平静场景）**中"已经发生"的画面；**严禁引用后续章节才发生的剧情事件、亲密/越界/冲突场景、或任何带"将来时"标记的锁定节拍内容**。若该角色前 1/3 章节无合适日常场景，opening_lines 用最朴素的"角色在场+可行动"收尾，宁可平淡不可剧透。
- 产出:nsfw_profile(该角色剧情相关的敏感度判定边界：依据原文证据列出"什么内容算露骨/接吻以上/性描写"与"什么只算日常/暧昧/non 敏感"，格式"敏感点→判定边界"，证据不足以判断写"证据不足")
### M8 证据模块 —— 回答"结论从哪来"
- 产出:evidence_refs(每条结论的证据出处，格式"ch12:block3"或"ch34"，章节代号按证据前缀)
- 约束:所有字段的结论都应能在 evidence_refs 找到对应

只输出 JSON，不要任何解释性文字。JSON 结构（严格）：
{"name":"角色名","identity":"身份","gender":"性别或未知","appearance":"外貌或未知","opening_scene":"开场场景画布","opening_lines":"完整开场白 first_mes","personality":"2-4句","voice_profile":"声线特征（默认语气，含代表金句支撑）","speech_style":"默认语气+变化范围双段式","example_dialogs":["原文台词1","原文台词2"],"boundaries":["行为底线/禁忌/绝不做"],"motivation":"深层动机与目标层级","relationships":["他人名:关系描述"],"evidence_refs":["ch12:block3","ch34"],"mental_models":["模型名:一句话描述（证据ch）"],"decision_heuristics":["规则:触发场景→做法（证据ch）"],"beliefs":["信念:形成故事（证据ch）"],"nsfw_profile":"敏感度判定边界"}"#
        .to_string()
}

/// 把章节正文切成带章节号的证据块。
fn build_evidence_blocks(chapters: &[(String, String)]) -> Vec<EvidenceBlock> {
    let mut out = Vec::new();
    for (ch_id, body) in chapters {
        for blk in split_into_blocks(body, BLOCK_SIZE, BLOCK_OVERLAP) {
            out.push(EvidenceBlock {
                chapter: ch_id.clone(),
                text: blk,
            });
        }
    }
    out
}

/// 在阻塞线程池里批量调用本地 embed。
async fn embed_blocking(texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
    tokio::task::spawn_blocking(move || crate::embed_local::embed_many(&texts))
        .await
        .map_err(|e| format!("embed join: {e}"))?
}

/// 按名字+别名从章节正文召回 top-k 原文证据片段。
///
/// - 逐章把正文切成 ~800 字重叠 100 的证据块；
/// - 对每章用 `embed_many` 一次批量向量化（"每章也只 embed 一次"）；
/// - 检索查询（名字+别名）做一次 embedding；
/// - 与全部块向量按余弦相似度 top-k，拼接返回原文片段，每段带 【chXX】 前缀。
pub async fn retrieve_character_evidence(
    _state: &AppState,
    chapters: &[(String, String)],
    name: &str,
    aliases: &[String],
    top_k: usize,
) -> Result<String, String> {
    let blocks = build_evidence_blocks(chapters);
    if blocks.is_empty() {
        return Ok(String::new());
    }

    // 检索查询 = 名字 + 别名
    let mut query = name.trim().to_string();
    for a in aliases {
        let t = a.trim();
        if !t.is_empty() {
            if !query.is_empty() {
                query.push(' ');
            }
            query.push_str(t);
        }
    }
    if query.trim().is_empty() {
        query = "主角".to_string();
    }
    // [P8 D4 2026-08-16] 检索召回补充声线维度：查询词叠加说话动作/语气词，
    // 让台词块（角色怎么说话）与情节块（角色做了什么）都能被召回——
    // 否则蒸馏输入偏向情节，性格归纳拿不到泼辣原文台词（声线漂移源头之一）。
    query.push_str(" 说话 说 骂 吼 嗔怪 语气 喊 叫 嘀咕 抱怨");

    // 查询向量化
    let qvec = embed_blocking(vec![query.clone()])
        .await
        .and_then(|mut v| v.pop().ok_or_else(|| "查询向量为空".to_string()))
        .map_err(|e| format!("角色证据检索[{name}]: embed 查询失败: {e}"))?;

    // 逐章批量向量化（每章一次 embed_many），据此填回所有块的向量
    let mut embs: Vec<Vec<f32>> = vec![vec![]; blocks.len()];
    let mut by_chapter: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, b) in blocks.iter().enumerate() {
        by_chapter.entry(b.chapter.clone()).or_default().push(i);
    }
    for (_, idxs) in by_chapter {
        let texts: Vec<String> = idxs.iter().map(|&i| blocks[i].text.clone()).collect();
        let chunk = embed_blocking(texts)
            .await
            .map_err(|e| format!("角色证据检索[{name}]: embed 章节块失败: {e}"))?;
        for (j, vec) in chunk.into_iter().enumerate() {
            if let Some(&bi) = idxs.get(j) {
                embs[bi] = vec;
            }
        }
    }

    // 余弦 top-k 并拼接证据
    // [fix 2026-08-15 实体精确命中加权] embedding 对实体歧义脆弱：查询「宿舍阿姨」
    // 的 top-k 会被「阿姨=庄眉」的高频块占据（ch09「你喜欢阿姨什么」排第 1），
    // 真实巡逻块被挤出 top-6（兔子想吃窝边草实证：宿舍阿姨卡误判"没有直接出场"）。
    // 修复：块内出现角色名（或任意别名）精确子串的，相似度加固定权重，
    // 保证实体真实出现的块必然进 top-k。
    let mut scored: Vec<(usize, f32)> = embs
        .iter()
        .enumerate()
        .map(|(i, e)| (i, cosine_sim(&qvec, e)))
        .collect();
    let mut entity_terms: Vec<String> = vec![name.trim().to_string()];
    for a in aliases {
        let t = a.trim();
        if !t.is_empty() {
            entity_terms.push(t.to_string());
        }
    }
    // [fix 2026-08-15 2] 只对 ≥3 字的术语做实体加权：2 字别名（「阿姨」）高频歧义
    // （全书指庄眉），若加权会把「阿姨=庄眉」块抬进 top-k，违背实体精确意图。
    // 「宿舍阿姨」4 字全名精确出现才 +0.25，真实巡逻块必进。
    let entity_bonus = 0.25f32; // 实体出现块的加权重（≈相似度 0.25 的抬升）
    for idx in 0..scored.len() {
        let blk_text = &blocks[scored[idx].0].text;
        if entity_terms
            .iter()
            .any(|t| t.chars().count() >= 3 && blk_text.contains(t.as_str()))
        {
            scored[idx].1 += entity_bonus;
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);
    let hits = scored;
    if hits.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    for (i, _score) in hits {
        out.push_str(&format!("【{}】{}\n\n", blocks[i].chapter, blocks[i].text));
    }
    Ok(out.trim().to_string())
}

/// 从 LLM 返回 JSON 中安全取值：snake_case 优先，兼容 camelCase。
fn jstr(v: &Value, snake: &str, camel: &str) -> String {
    v.get(snake)
        .or_else(|| v.get(camel))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn jstr_arr(v: &Value, snake: &str, camel: &str) -> Vec<String> {
    v.get(snake)
        .or_else(|| v.get(camel))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// 证据召回空时的 pure-text 词频兜底：按角色名/别名在章节正文中定位，
/// 截取含名字的上下文片段拼成证据。用于 embed 向量检索召回为空的情形
/// （如全本高频角色但向量 query 偏移），确保主角不因召回空被跳过。
fn fallback_evidence_scan(
    chapters: &[(String, String)],
    name: &str,
    aliases: &[String],
    max_snippets: usize,
) -> String {
    let mut needles: Vec<String> = {
        let mut v = vec![name.trim().to_string()];
        for a in aliases {
            let t = a.trim();
            if !t.is_empty() {
                v.push(t.to_string());
            }
        }
        v
    };
    needles.retain(|s| !s.is_empty());
    if needles.is_empty() {
        needles.push("主角".to_string());
    }

    let mut out = String::new();
    let mut found = 0usize;
    'outer: for (cid, body) in chapters {
        for nd in &needles {
            if let Some(pos) = body.find(nd.as_str()) {
                // 前后各取 150 字符，char-safe（pos 是 find 返回的边界）
                let prefix: String = body[..pos].chars().rev().take(150).collect::<Vec<_>>().into_iter().rev().collect();
                let suffix: String = body[pos..].chars().take(150).collect();
                out.push_str(&format!("【{cid}】…{}{}…\n\n", prefix, suffix));
                found += 1;
                if found >= max_snippets {
                    break 'outer;
                }
                break; // 每章只取第一处，避免同名片段灌满
            }
        }
    }
    out.trim().to_string()
}

/// 构造"角色谱"步骤的 LLM 输入：书名 + 每章摘要（整体截断到 max_chars）。
fn build_roster_input(title: &str, chapters: &[(String, String)], max_chars: usize) -> String {
    let mut out = format!("小说《{title}》章节正文摘要：\n");
    let budget = max_chars;
    let mut used = out.chars().count();
    // A0 完整版 (2026-08-19): 章节前缀 chXX 用原著章号（缺章自动跳号），与 build_pack_from_chapters
    // 的 pack.chapters.id 同一编号体系——保证事件卡蒸馏标注的 chapterRange 与运行时 cursor 对齐，
    // 修复「原著标题 vs 切分序号」错位（A1 锚定 + D1）。此前用切分序号 i+1，源缺章时蒸馏标号错位。
    let chapter_ids = crate::crawler::chapter_id_seq(chapters);
    // [fix 2026-08-15 角色谱全量覆盖] 原实现按序逐章喂到预算耗尽——13 章书 6000 字预算
    // 只覆盖前 ~4 章，后段角色（红姐/山楂/冯婷/莫旺财）LLM 根本看不见，导致角色谱永远
    // 只有前几章主角。改为均匀分层采样：预算内均匀取 ≤12 个采样点覆盖全书。
    // [fix 2026-08-15 2] ≤24 章全喂：12 采样点覆盖 13 章会漏 1 章（ch12 缺失实证）。
    // 24 章以内全部章节都喂（每章均分预算），24 章以上才分层采样。
    if chapters.len() <= 24 {
        // 中短书全喂（每章截断到预算均分，保证每章都可见不漏章）
        let per = (budget.saturating_sub(used) / chapters.len().max(1)).max(100);
        for (i, (cid, body)) in chapters.iter().enumerate() {
            if used >= budget {
                break;
            }
            let part: String = body
                .trim()
                .chars()
                .take(per)
                .take(budget.saturating_sub(used))
                .collect();
            // A1: 章节前缀带 chXX 切分序号（ch01 起按输入顺序），与运行时 chapter_cursor 编号对齐，
            // 供事件卡蒸馏标注 chapterRange 时使用同一编号体系（避免「原著标题 vs 切分序号」语义错位）。
            let label = format!("{}:{cid}", chapter_ids[i]);
            out.push_str(&format!("【{label}】{part}\n"));
            used += label.chars().count() + part.chars().count() + 4;
        }
    } else {
        // 长书分层采样：均匀取 ≤12 个采样点（开头/1/11…结尾）。
        // [fix 2026-08-15 预算均分] 原实现每点全量 1600 字，6000 字预算只够
        // 喂 3-4 个点就 break——13 章书只覆盖前 4 章（兔子想吃窝边草实证：
        // LLM 只看到 ch01-04，红姐/山楂/冯婷/莫旺财全在 ch05+ 看不见）。
        // 改为预算均分给全部采样点：每点 ~500 字，12 个点全部可见。
        let n = chapters.len();
        let mut sampled: Vec<usize> = Vec::new();
        for k in 0..12 {
            sampled.push((n - 1) * k / 11);
        }
        sampled.dedup();
        let per = (budget.saturating_sub(used) / sampled.len().max(1)).max(100);
        for idx in sampled {
            if used >= budget {
                break;
            }
            let (cid, body) = &chapters[idx];
            let part: String = body
                .trim()
                .chars()
                .take(per)
                .take(budget.saturating_sub(used))
                .collect();
            // A1：长书采样同样带 chXX 序号前缀。
            let label = format!("{}:{cid}", chapter_ids[idx]);
            out.push_str(&format!("【{label}】{part}\n"));
            used += label.chars().count() + part.chars().count() + 4;
        }
    }
    out
}

/// 把角色认知卡 JSON 填充成 `PackCharacterRef`。
fn card_to_ref(idx: usize, name: &str, importance: &str, card: &Value) -> PackCharacterRef {
    let identity = jstr(card, "identity", "identity");
    let role = if identity.is_empty() {
        if importance == "high" {
            "protagonist".to_string()
        } else {
            "supporting".to_string()
        }
    } else {
        identity
    };
    PackCharacterRef {
        id: format!("c-distil-{idx}"),
        name: name.to_string(),
        role,
        gender: jstr(card, "gender", "gender"),
        appearance: jstr(card, "appearance", "appearance"),
        opening_scene: jstr(card, "opening_scene", "openingScene"),
        opening_lines: jstr(card, "opening_lines", "openingLines"),
        importance: importance.to_string(),
        content_tier: None,
        example_dialogs: jstr_arr(card, "example_dialogs", "exampleDialogs"),
        boundaries: jstr_arr(card, "boundaries", "boundaries"),
        personality: jstr(card, "personality", "personality"),
        voice_profile: jstr(card, "voice_profile", "voiceProfile"),
        speech_style: jstr(card, "speech_style", "speechStyle"),
        motivation: jstr(card, "motivation", "motivation"),
        relationships: jstr_arr(card, "relationships", "relationships"),
        evidence_refs: jstr_arr(card, "evidence_refs", "evidenceRefs"),
        mental_models: jstr_arr(card, "mental_models", "mentalModels"),
        decision_heuristics: jstr_arr(card, "decision_heuristics", "decisionHeuristics"),
        beliefs: jstr_arr(card, "beliefs", "beliefs"),
        nsfw_profile: jstr(card, "nsfw_profile", "nsfwProfile"),
        expressions: Default::default(),
            voice: None,
            archive: None,
        avatar: None,
    }
}

/// 从召回证据字符串里提取涉及的章节代号（形如 "ch01"）。
fn extract_evidence_chapters(evidence: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for line in evidence.lines() {
        let Some(start) = line.find('【') else { continue };
        let after = &line[start..];
        let Some(rest) = after.strip_prefix('【') else { continue };
        if let Some(end) = rest.find('】') {
            let tag = rest[..end].trim().to_string();
            if !tag.is_empty() && !seen.contains(&tag) {
                seen.push(tag);
            }
        }
    }
    seen
}

/// 递归检查一个 LLM 产出的 JSON `Value` 里是否含 U+FFFD(替换符)。
/// 上游模型会偶发把单字损坏成 FFFD，此类内容应丢弃重试而非落盘。
fn json_has_fffd(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains('\u{FFFD}'),
        serde_json::Value::Array(items) => items.iter().any(json_has_fffd),
        serde_json::Value::Object(map) => map.values().any(json_has_fffd),
        _ => false,
    }
}

/// 判定 LLM 产出的角色卡是否为"空壳"——JSON 结构合法但核心内容字段全空。
/// 上游模型在通道抖动时会返回 `{}` 或全空字段的"空壳 JSON"——解析能成功、
/// 不触发 fffd/空响应重试，却会在 card_to_ref 后产出空壳卡(只有默认 role 与
/// importance)。空壳卡一旦落盘会污染 pack，且其 importance=high 会堵塞
/// chars_done 门，导致角色再也无法被自动重蒸。因此解析通过后必须再验核心字段。
///
/// 注意：这里用"任一核心字段非空即非空壳"的宽松判定，刻意不要求
/// personality/speech_style/motivation 三字段齐全——B5 降级保底的"最小卡"
/// 可能只有 identity+personality（如 `{"name":"主角","personality":"勇敢坚毅"}`），
/// 那是配额耗尽时的保底产物，不应被此处重试逻辑吞掉。薄卡(缺核心字段但非全空)
/// 由 crawler.rs 合并守卫在落 pack 前过滤，与重试逻辑分层。
fn json_is_empty_shell(v: &serde_json::Value) -> bool {
    let core = [
        "identity",
        "personality",
        "speech_style",
        "motivation",
        "opening_lines",
    ];
    let any_content = core.iter().any(|k| {
        v.get(*k)
            .and_then(|x| x.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    });
    !any_content
}

/// 蒸馏单个角色认知卡，对 LLM 空响应 / JSON 解析失败 / 含损坏字(U+FFFD)做最多 3 次重试（间隔 500ms）。
///
/// `attempt` 触发一次 LLM 调用（返回携带原文的 `Result<String, String>`）。
/// 返回 `Some(JSON)` 表示取得合法认知卡；重试 3 次仍无合法 JSON 时返回 `None`，
/// 由调用方跳过该角色，不让整批蒸馏因单个角色塌陷。
///
/// 重试上限按角色重要性分级：主角/重要配角(high/medium)给更多轮次，
/// 避免上游 deepseek-flash 的 U+FFFD 坏字把核心角色整张卡吞掉导致主角丢失；
/// 边缘角色(low)保持少轮，节省成本。
async fn distill_one_character_card<F, Fut>(
    name: &str,
    importance: &str,
    mut attempt: F,
) -> Option<Value>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    // B4: 网络错误(conn_err)与坏字(fffd)分开计数，各有独立上限。
    // fffd 配额按重要性分级：high 10 / medium 6 / low 4；网络错误单独给上限 6。
    // 注:deepseek 长中文输入下 FFFD 复现率实测可达 ~33%,high 给 10 次(0.33^10≈0.002%)
    // 确保主角几乎必然出卡;low 角色少轮节省成本。
    let max_fffd_rounds: u32 = match importance.trim().to_lowercase().as_str() {
        "high" => 10,
        "medium" => 6,
        _ => 4,
    };
    let max_conn_err_rounds: u32 = match importance.trim().to_lowercase().as_str() {
        "high" | "medium" => 6,
        _ => 2, // 边缘角色网络错误快速放弃，避免单角色拖死整批蒸馏
    }; // 网络错误重试上限（独立于 fffd 配额）
    let is_high = importance.trim().to_lowercase() == "high";

    let mut round_fffd: u32 = 0; // fffd / 空响应 / JSON 解析失败
    let mut round_conn: u32 = 0; // 网络错误 / 调用失败

    loop {
        let done_fffd = round_fffd >= max_fffd_rounds;
        let done_conn = round_conn >= max_conn_err_rounds;
        if done_fffd || done_conn {
            if done_fffd {
                tracing::warn!(
                    name = %name, round_fffd, round_conn,
                    "角色蒸馏[{name}] fffd 配额耗尽({round_fffd}/{max_fffd_rounds})"
                );
            } else {
                tracing::warn!(
                    name = %name, round_fffd, round_conn,
                    "角色蒸馏[{name}] 网络错误配额耗尽({round_conn}/{max_conn_err_rounds})"
                );
            }
            break;
        }
        let ok = match attempt().await {
            Ok(raw) => raw,
            Err(e) => {
                round_conn += 1;
                tracing::warn!(
                    name = %name, round_conn, round_fffd,
                    "角色蒸馏[{name}] 调用失败: {e}，重试 (round_conn={round_conn})"
                );
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
        };
        if ok.trim().is_empty() {
            round_fffd += 1;
            tracing::warn!(
                name = %name, round_fffd, round_conn,
                "角色蒸馏[{name}] 空响应，重试 (round_fffd={round_fffd})"
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            continue;
        }
        if let Some(mut v) = crate::llm_stream::extract_json_value(&ok) {
            if json_has_fffd(&v) {
                round_fffd += 1;
                tracing::warn!(
                    name = %name, round_fffd, round_conn,
                    "角色蒸馏[{name}] 输出含 U+FFFD 损坏字，丢弃重试 (round_fffd={round_fffd})"
                );
                // [DIAG] 首次 fffd 落盘原始返回，定位是模型噪声还是服务端解析问题
                if round_fffd == 1 {
                    let _ = std::fs::write(
                        std::path::Path::new("fffd-raw-dump.json"),
                        serde_json::to_string_pretty(&serde_json::json!({
                            "name": name,
                            "raw": ok,
                            "raw_len": ok.chars().count(),
                            "fffd_count": ok.chars().filter(|c| *c == '\u{FFFD}').count(),
                        }))
                        .unwrap_or_default(),
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            if json_is_empty_shell(&v) {
                round_fffd += 1;
                tracing::warn!(
                    name = %name, round_fffd, round_conn,
                    "角色蒸馏[{name}] 输出为空壳 JSON（核心字段全空），丢弃重试 (round_fffd={round_fffd})"
                );
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            // ST-26 校验层 (2026-08-18): opening_scene 确定性违规检测——后期场景/剧透信号
            // → 回退「证据不足」（宁可平淡不可剧透）。实证：度蜜月 04:49 蒸馏违反 ST-26
            // （蜜月套房 + 「另：接沈雨棠电话」），纯 prompt 约束 LLM 不遵守，加代码兜底。
            if let Some(map) = v.as_object_mut() {
                let os_violates = map
                    .get("opening_scene")
                    .and_then(|x| x.as_str())
                    .map(opening_scene_violates_st26)
                    .unwrap_or(false);
                if os_violates {
                    tracing::warn!(
                        name = %name,
                        "角色蒸馏[{name}] opening_scene 违反 ST-26（后期场景/剧透），回退证据不足"
                    );
                    map.insert(
                        "opening_scene".into(),
                        serde_json::Value::String("证据不足，无合适开场场景（ST-26 守卫回退）".into()),
                    );
                    map.insert(
                        "opening_lines".into(),
                        serde_json::Value::String("（该角色尚未有合适的日常开场场景，等待剧情展开。）".into()),
                    );
                }
            }
            return Some(v);
        }
        round_fffd += 1;
        tracing::warn!(
            name = %name, round_fffd, round_conn,
            "角色蒸馏[{name}] JSON 解析失败，重试 (round_fffd={round_fffd})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // B5: high 角色保底兜底 — fffd 配额耗尽后，再尝试 2 次，力求至少产出最小卡
    if is_high {
        tracing::warn!(
            name = %name, round_fffd, round_conn,
            "角色蒸馏[{name}] 启动 high 角色降级保底"
        );
        for fallback_round in 0..2u32 {
            match attempt().await {
                Ok(raw) => {
                    if raw.trim().is_empty() {
                        continue;
                    }
                    if let Some(v) = crate::llm_stream::extract_json_value(&raw) {
                        if json_has_fffd(&v) {
                            continue;
                        }
                        if json_is_empty_shell(&v) {
                            tracing::warn!(
                                name = %name, fallback_round,
                                "角色蒸馏[{name}] 降级保底输出为空壳 JSON，继续尝试 (fallback_round={fallback_round})"
                            );
                            continue;
                        }
                        tracing::warn!(
                            name = %name, fallback_round,
                            "角色蒸馏[{name}] 降级保底成功，产出最小卡"
                        );
                        return Some(v);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        name = %name, fallback_round,
                        "角色蒸馏[{name}] 降级保底调用失败: {e}"
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        tracing::warn!(
            name = %name,
            "角色蒸馏[{name}] 降级保底仍无法产出认知卡，跳过"
        );
    } else {
        tracing::warn!(name = %name, "角色蒸馏[{name}] 重试后仍无法产出认知卡，跳过");
    }
    None
}

/// 角色名合法性过滤：拦截 LLM 角色清单里的正文碎片/短语/普通名词。
///
/// 背景（2026-08-13 乱码名专项）：LLM 抽取角色名时把句子片段当人名
/// （「冲进下水」「过电车轨」「那就」「纷纷询」等），产生无内容的空壳卡
/// 污染 pack。判定规则（误杀控制：真名/称呼 苏早/沈棠知/光头男/外婆/
/// 林小宇/弗雷德/王一博/李进 均不命中）：
/// - 长度 1..=12；不含标点/空白/引号
/// - 高频虚词单字命中且**不在末位** → 拒（末位容忍单字名如「李进」）
/// - 句子碎片黑名单子串命中 → 拒
/// - 首字为典型动作动词 → 拒（「听早早」）
/// ST-26 校验层 (2026-08-18): 蒸馏产出 opening_scene 的确定性违规检测。
/// 背景实证：度蜜月 08-18 04:49 在 ST-26 prompt 在线的蒸馏仍产出「蜜月套房/接沈雨棠电话」
/// 后期场景卡——纯 prompt 约束 LLM 可不遵守，需代码兜底。
/// 检测信号（保守，宁可回退不可剧透）：
/// ① 后期场景词：酒店/套房/机场/蜜月/海边/海滩/度假村/客房
/// ② 剧透后缀：另：/另注/深夜/电话/将（将来时叙事）出现在开场画布
/// 命中任一 → 违规（蒸馏侧回退「证据不足」，不脑补后期画面）。
pub fn opening_scene_violates_st26(os: &str) -> bool {
    const LATE_WORDS: [&str; 12] = [
        "酒店", "套房", "机场", "蜜月", "海边", "海滩", "度假村", "客房", "月见", "领证后",
        "海岛", "度假",
    ];
    if LATE_WORDS.iter().any(|w| os.contains(w)) {
        return true;
    }
    const SPOILER_MARKERS: [&str; 6] = ["另：", "另注", "深夜", "后来", "之后", "将要"];
    SPOILER_MARKERS.iter().any(|m| os.contains(m))
}

pub(crate) fn is_plausible_character_name(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return false;
    }
    let chars: Vec<char> = n.chars().collect();
    let len = chars.len();
    if len > 12 {
        return false;
    }
    // 标点/空白/引号/控制字符
    if chars.iter().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(
                *c,
                '，' | '。' | '！' | '？' | '、' | '；' | '：' | '“' | '”' | '‘' | '’' | '（' | '）'
                    | '《' | '》' | '〈' | '〉' | '【' | '】' | '…' | '·' | '-' | '_' | ',' | '.'
                    | ';' | ':' | '!' | '?' | '"' | '\'' | '`' | '/' | '\\' | '|' | '~' | '@'
                    | '#' | '$' | '%' | '^' | '&' | '*' | '=' | '+' | '[' | ']' | '(' | ')' | '{'
                    | '}'
            )
    }) {
        return false;
    }
    // 高频虚词单字（命中且不在末位 → 拒）。刻意不含「一」（王一博/周一）「小」「子」。
    const VIRTUAL: &str = "的了着过在把被向从到就都也还又很更最和与或而且但是为对给让那这个里中后前旁进出上下起像如有没不别要会能我你他她它们种样般地得并已曾正才只再却若虽因果则各每某另将既";
    // [fix 2026-08-15] 音译名容忍：加里克斯/格列佛/萨拉丁 这类 3+ 字名字含「里/斯/尔/格/克/特/罗/拉/尼/卡」
    // 是正常音译字符，不能因 VIRTUAL 里的「里」误杀。检测到音译特征字 ≥1 个时跳过虚词检查。
    const TRANSLIT: &str = "里斯尔格克特罗拉尼卡穆奥芬恩德";
    let has_translit = chars.iter().filter(|c| TRANSLIT.contains(**c)).count() >= 2;
    if !has_translit {
        for (i, c) in chars.iter().enumerate() {
            if i < len - 1 && VIRTUAL.contains(*c) {
                // [fix 2026-08-15] 姓氏豁免：3 字以上名字首字命中虚词可能是姓氏
                // （向明初 = 向+明初，向是姓；「那怪/那就」2 字仍严格拦截）。
                // 2 字名严格：那怪/那小子 这类泛称不得放行。
                if i == 0 && len >= 3 {
                    continue;
                }
                return false;
            }
        }
    }
    // 句子碎片黑名单（子串命中 → 拒）
    const FRAGMENTS: &[&str] = &[
        "那就", "另一个", "像是想", "有一种", "里面", "时候", "开始", "已经", "看见", "知道", "自己",
        "什么", "怎么", "一样", "下来", "出去", "回来", "眼前", "身后", "面前", "旁边", "突然", "然后",
        "可是", "但是", "因为", "所以", "如果", "虽然", "不过", "还是", "就是", "只是", "并且", "甚至",
        "大概", "仿佛", "好像", "依然", "终于", "连忙", "赶紧", "立刻", "马上", "缓缓", "慢慢", "轻轻",
        "深深", "紧紧", "冷冷", "淡淡", "微微", "渐渐", "不断", "不停", "一直", "再也", "越来越", "纷纷",
        "粮食", "电车", "下水", "味道", "示意", "尽管", "走得", "来得", "说得", "看得", "想着", "看着",
        "那里", "这里", "起来", "进去", "下去", "上去", "离开", "走近", "抬头", "低头", "转身", "回头",
        "开口", "伸手", "点头", "摇头", "皱眉", "叹气", "微笑", "沉默", "停顿", "犹豫", "考虑", "决定",
        "想要", "觉得", "感到", "发现", "明白", "了解", "记得", "忘记", "告诉", "询问", "回答", "答应",
        "拒绝", "接受", "留下", "拉住", "抓住", "推开", "放下", "拿起", "抱起", "站起", "坐下", "躺下",
        "走过", "路过", "穿过", "越过", "冲进", "冲出", "跑进", "跑出", "走进", "走出", "退后", "上前",
        "靠近", "接近", "远离", "脸色", "声音", "目光", "眼神", "语气", "表情", "样子", "感觉", "心里", "颜色",
        "胸前", "怀里", "桌上", "门外", "屋里", "家里", "学校", "公司", "房间", "门口", "窗前",
    ];
    for f in FRAGMENTS {
        if n.contains(f) {
            return false;
        }
    }
    // 首字为典型动作动词 → 拒（「听早早」=“听，早早…”的碎片）
    const VERB_HEAD: &str = "听说看走来去冲拉留放拿问答跟随喊叫望瞧盯嗅尝吃喝玩打拍推抱背提扛抬拽拖蹬踩踢跳跑爬飞驶开关门关停站坐躺睡醒念读写画唱弹吹奏演算数记查找搜索摸碰擦洗刷扫拖抹挥舞甩掷抛投射砍劈刺戳插扎缝补织戴穿脱换洗晾晒煮炒煎炖蒸烤烧点熄灭燃";
    if VERB_HEAD.contains(chars[0]) {
        return false;
    }
    true
}

/// 完整角色蒸馏流程：产出有血有肉的 `PackCharacterRef` 列表。
///
/// a) 让 LLM 从全书摘要产出"角色谱 JSON"；
/// b) 按 importance 高 / 前 ≤[`MAX_DISTIL_CHARS`] 个逐个召回证据、蒸馏认知卡；
/// c) 每个卡带 evidence_refs。
pub async fn distill_pack_characters(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
    incremental_pid: Option<&str>,
) -> Result<Vec<PackCharacterRef>, String> {
    let client = reqwest::Client::new();
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("角色蒸馏: LLM 未配置".into());
    }


    // a) 角色谱：LLM 抽取角色清单（name/aliases/identity/importance）并过滤乱码名；
    let roster_input = build_roster_input(title, chapters, max_chars);
    let roster_system = "你是小说角色清单助手。从给定小说正文中识别**所有**有叙事功能的人物（排除旁白/读者/玩家），只输出 JSON 数组，每个元素为 {\"name\":\"角色名\",\"aliases\":[\"别名\"],\"identity\":\"身份\",\"appears_first\":\"首次出场章节\",\"importance\":\"high|medium|low\"}。importance 依据戏份与剧情权重判断。name 必须是明确的人物专名（姓名/称呼/外号），严禁输出句子片段、短语、普通名词或口语碎片；拿不准的名字不要输出。\
要求：1) 宁多勿漏——主角、重要配角、次要配角、有名字的龙套都要列出；\
2) 数量参考：10 章以下 ≥5 个，10-50 章 ≥10 个，50 章以上 ≥15 个；\
3) 只输出 JSON 数组，不要任何解释文字。";
    let raw = crate::llm_stream::chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        &state.provider_kind,
        roster_system,
        &roster_input,
        0.1, 16384, LLM_TIMEOUT_SECS,
        &client,
    )
    .await
    .map_err(|e| format!("角色蒸馏[角色谱]: {e}"))?;
    let roster_v = crate::llm_stream::extract_json_value(&raw).or_else(|| {
        // [DIAG] 解析失败时落盘原始返回，定位上游格式问题
        let _ = std::fs::write(
            std::path::Path::new("roster-parse-fail.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "raw": raw,
                "raw_len": raw.chars().count(),
            }))
            .unwrap_or_default(),
        );
        None
    })
    .ok_or_else(|| "角色蒸馏[角色谱]: 无法解析 LLM 返回的 JSON".to_string())?;
    let roster: Vec<Value> = match roster_v {
        Value::Array(a) => a,
        other => vec![other],
    };

    // 过滤 + 排序（high>medium>low） + 截断
    let rank = |im: &str| -> u8 { match im { "high" => 0, "medium" => 1, _ => 2 } };
    let mut candidates: Vec<(String, Vec<String>, String)> = Vec::new();
    for v in &roster {
        let name = jstr(v, "name", "name");
        if name.is_empty() {
            continue;
        }
        // [fix 2026-08-13 乱码名专项] 角色名合法性过滤：LLM 常把正文碎片当人名
        // （「冲进下水」「过电车轨」「那就」等），过滤后不进入蒸馏候选。
        if !is_plausible_character_name(&name) {
            tracing::warn!(name = %name, "角色蒸馏[角色谱]: 过滤疑似乱码角色名");
            continue;
        }
        let aliases = jstr_arr(v, "aliases", "aliases");
        let importance = jstr(v, "importance", "importance").to_lowercase();
        candidates.push((name, aliases, importance));
    }
    candidates.sort_by_key(|(_, _, im)| rank(im));
    // [fix 2026-08-15 关系端点补漏] 角色谱 LLM 往往只给主角（13 章书只给 3 个），
    // 但关系图谱（distill_chapter_relations）已确定性识别更多角色端点
    // （红姐/冯婷/莫旺财/浓妆女人 等）。把这些端点合并进候选，medium 优先，
    // 补漏不覆盖 LLM 已识别的高优先角色。
    // [fix 2026-08-15 去重] 同名角色会同时出现在 LLM 角色谱和关系端点里
    // （实证：冯婷×2/红姐×2/浓妆女人×2），合并前按 name 去重。
    let mut seen_names: std::collections::HashSet<String> =
        candidates.iter().map(|(n, _, _)| n.clone()).collect();
    if let Some(pid) = incremental_pid {
        let rel_path = state.packs.pack_dir(pid).ok().map(|d| d.join("relations.json"));
        if let Some(rp) = rel_path {
            if let Ok(s) = std::fs::read_to_string(&rp) {
                if let Ok(edges) = serde_json::from_str::<Vec<Value>>(&s) {
                    for e in edges {
                        for k in ["from", "to"] {
                            let nm = e.get(k).and_then(|x| x.as_str()).unwrap_or("").trim();
                            if nm.is_empty() || !seen_names.insert(nm.to_string()) {
                                continue;
                            }
                            if !is_plausible_character_name(nm) {
                                continue;
                            }
                            candidates.push((nm.to_string(), vec![], "medium".to_string()));
                        }
                    }
                }
            }
        }
    }
    candidates.sort_by_key(|(_, _, im)| rank(im));
    let candidates: Vec<_> = candidates.into_iter().take(MAX_DISTIL_CHARS).collect();
    // [DIAG] 落盘角色谱原始返回 + 最终 candidates，供排查庄眉等主角缺卡问题。
    // [fix 2026-08-15] 按 pack 目录隔离：原全局单文件 roster-diag.json 会被
    // 其他 pack 的蒸馏覆盖，导致续跑判定读到陈旧 roster（跨 pack 污染，如
    // 兔子想吃窝边草 的向明初被旧版误杀后，旧 roster 判定"high 已覆盖"跳过重蒸馏）。
    {
        let probe = serde_json::json!({
            "title": title,
            "raw_roster": String::from_utf8_lossy(raw.as_bytes()).to_string(),
            "candidates": candidates.iter().map(|(n,a,im)| json!({"name":n,"aliases":a,"importance":im})).collect::<Vec<_>>(),
        });
        let diag_path = incremental_pid
            .and_then(|pid| {
                state
                    .packs
                    .pack_dir(pid)
                    .ok()
                    .map(|d| d.join("roster-diag.json"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from("roster-diag.json"));
        if let Err(e) = std::fs::write(
            &diag_path,
            serde_json::to_string_pretty(&probe).unwrap_or_default(),
        ) {
            tracing::warn!(path = %diag_path.display(), "roster-diag 落盘失败: {e}");
        }
    }
    if candidates.is_empty() {
        return Ok(vec![]);
    }

    // b) 逐角色蒸馏认知卡
    let mut cards: Vec<PackCharacterRef> = Vec::new();
    // [DIAG] 每个候选角色的处理结果追踪
    let mut diag_trace: Vec<serde_json::Value> = Vec::new();
    for (n, (name, aliases, importance)) in candidates.into_iter().enumerate() {
        let evidence =
            retrieve_character_evidence(state, chapters, &name, &aliases, EVIDENCE_TOP_K).await?;
        let evidence = evidence.trim();
        // [DIAG] 记录本角色召回情况
        diag_trace.push(serde_json::json!({
            "name": name,
            "importance": importance,
            "evidence_len": evidence.chars().count(),
            "evidence_head": evidence.chars().take(120).collect::<String>(),
            "outcome": "pending",
        }));
        if evidence.is_empty() {
            // 主角/重要角色证据召回为空：pure-text 词频兜底，避免 high 角色因嵌入检索
            // 偏移而被静默跳过（如庄眉全本高频但向量 query 未命中）。
            if importance == "high" {
                let fb = fallback_evidence_scan(chapters, &name, &aliases, 8);
                if !fb.is_empty() {
                    tracing::warn!(
                        name = %name,
                        "角色蒸馏: 向量召回为空，high 角色启用词频兜底取证"
                    );
                    // 重新进入蒸馏，用兜底证据
                    let card_user = format!(
                        "小说《{title}》\n目标角色：{name}\n\
                         检索到的原文证据（【ch】前缀指示章节）：\n{fb}\n\
                         请按系统要求为该角色输出一份严格 JSON 角色认知卡。"
                    );
                    let card_system = distill_characters_system_prompt();
                    let Some(card_v) = distill_one_character_card(
                        &name,
                        &importance,
                        || {
                            crate::llm_stream::chat_completion_dispatch(
                                &llm.base_url,
                                &llm.api_key,
                                &llm.model,
                                &state.provider_kind,
                                &card_system,
                                &card_user,
                                0.1, 16384, LLM_TIMEOUT_SECS,
                                &client,
                            )
                        },
                    )
                    .await
                    else {
                        if let Some(t) = diag_trace.last_mut() { t["outcome"] = "failed_fallback_distill".into(); }
                        continue;
                    };
                    let mut card = card_to_ref(n, &name, &importance, &card_v);
                    if card.evidence_refs.is_empty() {
                        card.evidence_refs = extract_evidence_chapters(&fb);
                    }
                    cards.push(card);
                    if let Some(t) = diag_trace.last_mut() { t["outcome"] = "ok_fallback".into(); }
                    // [增量存档] 每蒸馏完一个角色立即写回 pack，避免中断丢全部
                    if let Some(pid) = incremental_pid {
                        if let Ok(mut p) = state.packs.get(pid) {
                            p.characters = cards.clone();
                            let _ = state.packs.save(p);
                        }
                    }
                    continue;
                }
            }
            tracing::warn!(name=%name, "角色蒸馏: 无召回证据，跳过");
            if let Some(t) = diag_trace.last_mut() { t["outcome"] = "skipped_no_evidence".into(); }
            continue;
        }
        let card_user = format!(
            "小说《{title}》\n目标角色：{name}\n\
             检索到的原文证据（【ch】前缀指示章节）：\n{evidence}\n\
             请按系统要求为该角色输出一份严格 JSON 角色认知卡。"
        );
        let card_system = distill_characters_system_prompt();
        let Some(card_v) = distill_one_character_card(
            &name,
            &importance,
            || {
                crate::llm_stream::chat_completion_dispatch(
                    &llm.base_url,
                    &llm.api_key,
                    &llm.model,
                    &state.provider_kind,
                    &card_system,
                    &card_user,
                    0.1, 16384, LLM_TIMEOUT_SECS,
                    &client,
                )
            },
        )
        .await
        else {
            if let Some(t) = diag_trace.last_mut() { t["outcome"] = "failed_distill".into(); }
            continue;
        };
        let mut card = card_to_ref(n, &name, &importance, &card_v);
        if card.evidence_refs.is_empty() {
            // 兜底：用本次召回到的证据章节补足 evidence_refs
            card.evidence_refs = extract_evidence_chapters(evidence);
        }
        cards.push(card);
        if let Some(t) = diag_trace.last_mut() { t["outcome"] = "ok".into(); }
        // [增量存档] 每蒸馏完一个角色立即写回 pack，避免中断丢全部
        if let Some(pid) = incremental_pid {
            if let Ok(mut p) = state.packs.get(pid) {
                p.characters = cards.clone();
                let _ = state.packs.save(p);
            }
        }
    }
    // [DIAG] 角色蒸馏逐角色追踪落盘
    {
        let probe = serde_json::json!({ "title": title, "trace": diag_trace, "cards": cards.iter().map(|c| c.name.clone()).collect::<Vec<_>>() });
        if let Ok(s) = std::fs::write(
            std::path::Path::new("char-diag.json"),
            serde_json::to_string_pretty(&probe).unwrap_or_default(),
        ) { let _ = s; }
    }
    Ok(cards)
}

/// 指定角色蒸馏：跳过角色谱自动识别，按调用方名单直接蒸馏指定角色。
///
/// - 每个名字：向量召回证据 → 为空则词频兜底取证（调用方点名即重视，不限于 high）；
/// - importance 优先复用 `roster-diag.json` 中同名候选的标注，无则默认 "high"；
/// - 增量存档：每蒸馏完一张卡立即合并写回 pack（同名替换、其余追加）；
/// - 新卡 id 从既有 `c-distil-N` 之后续排，避免与存量卡冲突。
pub async fn distill_named_characters(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    names: &[String],
    incremental_pid: Option<&str>,
) -> Result<Vec<PackCharacterRef>, String> {
    let client = reqwest::Client::new();
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("角色蒸馏: LLM 未配置".into());
    }
    let names: Vec<String> = names
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return Ok(vec![]);
    }

    // 复用最近一次角色谱的 aliases/importance（若名单角色曾进过谱，取其别名与权重）
    let mut known: std::collections::HashMap<String, (Vec<String>, String)> = Default::default();
    if let Ok(s) = std::fs::read_to_string("roster-diag.json") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            if let Some(cands) = v.get("candidates").and_then(|c| c.as_array()) {
                for c in cands {
                    let n = c
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if n.is_empty() {
                        continue;
                    }
                    let aliases = c
                        .get("aliases")
                        .and_then(|a| a.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let imp = c
                        .get("importance")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    known.insert(n, (aliases, imp));
                }
            }
        }
    }

    // 新卡 id 起点：既有 c-distil-N 最大值 + 1
    let start_idx = incremental_pid
        .and_then(|pid| state.packs.get(pid).ok())
        .map(|p| {
            p.characters
                .iter()
                .filter_map(|c| {
                    c.id
                        .strip_prefix("c-distil-")
                        .and_then(|s| s.parse::<usize>().ok())
                })
                .max()
                .map(|m| m + 1)
                .unwrap_or(0)
        })
        .unwrap_or(0);

    let mut cards: Vec<PackCharacterRef> = Vec::new();
    // [DIAG] 每个指定角色的处理结果追踪
    let mut diag_trace: Vec<serde_json::Value> = Vec::new();
    for (n, name) in names.iter().enumerate() {
        let (aliases, imp) = known
            .get(name)
            .cloned()
            .unwrap_or_else(|| (Vec::new(), "high".to_string()));
        let importance = if imp.trim().is_empty() {
            "high".to_string()
        } else {
            imp
        };
        let evidence = retrieve_character_evidence(state, chapters, name, &aliases, EVIDENCE_TOP_K)
            .await?
            .trim()
            .to_string();
        diag_trace.push(serde_json::json!({
            "name": name,
            "importance": importance,
            "evidence_len": evidence.chars().count(),
            "evidence_head": evidence.chars().take(120).collect::<String>(),
            "outcome": "pending",
        }));
        // 向量召回为空 → 词频兜底取证（点名角色一视同仁，不限于 high）
        let effective = if evidence.is_empty() {
            let fb = fallback_evidence_scan(chapters, name, &aliases, 8);
            if !fb.is_empty() {
                tracing::warn!(name = %name, "指定角色蒸馏: 向量召回为空，启用词频兜底取证");
            }
            fb
        } else {
            evidence
        };
        if effective.trim().is_empty() {
            tracing::warn!(name = %name, "指定角色蒸馏: 无召回证据，跳过");
            if let Some(t) = diag_trace.last_mut() {
                t["outcome"] = "skipped_no_evidence".into();
            }
            continue;
        }
        let card_user = format!(
            "小说《{title}》\n目标角色：{name}\n\
             检索到的原文证据（【ch】前缀指示章节）：\n{effective}\n\
             请按系统要求为该角色输出一份严格 JSON 角色认知卡。"
        );
        let card_system = distill_characters_system_prompt();
        let Some(card_v) = distill_one_character_card(
            &name,
            &importance,
            || {
                crate::llm_stream::chat_completion_dispatch(
                    &llm.base_url,
                    &llm.api_key,
                    &llm.model,
                    &state.provider_kind,
                    &card_system,
                    &card_user,
                    0.1, 16384, LLM_TIMEOUT_SECS,
                    &client,
                )
            },
        )
        .await
        else {
            if let Some(t) = diag_trace.last_mut() {
                t["outcome"] = "failed_distill".into();
            }
            continue;
        };
        let mut card = card_to_ref(start_idx + n, name, &importance, &card_v);
        if card.evidence_refs.is_empty() {
            card.evidence_refs = extract_evidence_chapters(&effective);
        }
        cards.push(card);
        if let Some(t) = diag_trace.last_mut() {
            t["outcome"] = "ok".into();
        }
        // [增量存档] 每蒸馏完一个角色立即合并写回 pack（同名替换、其余追加）
        if let Some(pid) = incremental_pid {
            if let Ok(mut p) = state.packs.get(pid) {
                if let Some(last) = cards.last() {
                    p.characters.retain(|c| c.name != last.name);
                    p.characters.push(last.clone());
                    let _ = state.packs.save(p);
                }
            }
        }
    }
    // [DIAG] 指定角色蒸馏逐角色追踪落盘
    {
        let probe = serde_json::json!({
            "title": title,
            "kind": "named",
            "trace": diag_trace,
            "cards": cards.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        });
        if let Ok(s) = std::fs::write(
            std::path::Path::new("char-diag.json"),
            serde_json::to_string_pretty(&probe).unwrap_or_default(),
        ) {
            let _ = s;
        }
    }
    Ok(cards)
}

// ─── Stage 3：世界树 / 节拍 / 多出口 / 世界线 ────────────────────────────────

/// 一次 LLM JSON 调用（复用 chat_completion_dispatch + extract_json_value）。
///
/// - `stage`：失败信息里的阶段名，方便调用方定位失败点。
/// - LLM 未配置 / 调用失败 / JSON 解析失败一律返回 Err。
async fn llm_json(
    state: &AppState,
    system: &str,
    user: &str,
    stage: &str,
) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(LLM_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("{stage}: 构建 LLM client 失败: {e}"))?;
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err(format!("{stage}: LLM 未配置"));
    }
    for round in 0..3u32 {
        match crate::llm_stream::chat_completion_dispatch(
            &llm.base_url,
            &llm.api_key,
            &llm.model,
            &state.provider_kind,
            system,
            user,
            0.1, 16384, LLM_TIMEOUT_SECS,
            &client,
        )
        .await
        {
            Ok(raw) => {
                if let Some(v) = crate::llm_stream::extract_json_value(&raw) {
                    // 上游 opencode zen 的 deepseek-v4-flash-free 会 ~25% 概率随机把单个
                    // 中文字采样成 U+FFFD（整字转 2-3 个替换符，位置随机）。含损坏字的结构化
                    // 内容是脏数据，丢弃重试以规避。
                    if json_has_fffd(&v) {
                        tracing::warn!(stage, round, "{stage}: LLM 输出含 U+FFFD 损坏字，重试");
                    } else {
                        return Ok(v);
                    }
                } else {
                    tracing::warn!(stage, round, "{stage}: JSON 解析失败，重试");
                }
            }
            Err(e) => {
                tracing::warn!(stage, round, "{stage}: LLM 调用失败: {e}，重试");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Err(format!("{stage}: 重试 3 次后仍失败"))
}

/// 把任意 JSON 值视为 JSON 数组（对象/字符串也包一层，返回 Vec）。
fn as_json_array(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        other => vec![other],
    }
}

/// 世界树条目规范化：缺失字段补默认值，供落库前使用。
///
/// - `title`/`text` 缺失或为空 → 返回 None（无标题/无证据归纳的条目不落库）。
/// - `tags` 缺省空数组；`permanent` 缺省 true；`importance` 缺省 low；
/// - `type` 缺省 setting；`confidence` 缺省 0.5（并夹到 0.0~1.0）。
fn normalize_world_lore_entry(v: &Value) -> Option<Value> {
    let title = jstr(v, "title", "title");
    let text = jstr(v, "text", "text");
    if title.is_empty() || text.is_empty() {
        return None;
    }
    let importance = jstr(v, "importance", "importance");
    let importance = if importance.is_empty() { "low".into() } else { importance };
    let kind = jstr(v, "type", "type");
    let kind = if kind.is_empty() { "setting".into() } else { kind };
    let confidence = v
        .get("confidence")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    let activation = {
        let a = jstr(v, "activation", "activation");
        if a.is_empty() { "keyword".to_string() } else { a }
    };
    let depth = v.get("depth").and_then(|x| x.as_i64()).map(|x| x.clamp(1, 10)).unwrap_or(5);
    Some(json!({
        "title": title,
        "text": text,
        "tags": jstr_arr(v, "tags", "tags"),
        "permanent": v.get("permanent").and_then(|x| x.as_bool()).unwrap_or(true),
        "importance": importance,
        "type": kind,
        "evidence_refs": jstr_arr(v, "evidence_refs", "evidence_refs"),
        "confidence": confidence,
        "activation": activation,
        "depth": depth,
    }))
}

/// 节拍数组解析：过滤空串、截断长度与条数。
fn parse_beats(v: &Value, max_beats: usize, max_len: usize) -> Vec<String> {
    as_json_array(v.clone())
        .into_iter()
        .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(max_len).collect())
        .take(max_beats)
        .collect()
}

/// 出口候选解析：`[{"when":..., "hint":...}]`，过滤 when 为空项。
/// 返回 `(when, hint)` 列表。
fn parse_exit_candidates(v: &Value) -> Vec<(String, String)> {
    as_json_array(v.clone())
        .into_iter()
        .filter_map(|x| {
            let when = jstr(&x, "when", "when");
            if when.is_empty() {
                return None;
            }
            let hint = jstr(&x, "hint", "hint");
            Some((when, hint))
        })
        .take(MAX_EXITS)
        .collect()
}

/// 世界线大事记解析：只保留 `event` 非空的条目。
fn parse_worldline(v: &Value) -> Vec<Value> {
    as_json_array(v.clone())
        .into_iter()
        .filter(|e| !jstr(e, "event", "event").is_empty())
        .collect()
}

/// 世界树：LLM 全景盘点世界观实体，逐实体召回证据并蒸馏 lore 条目。
///
/// - 盘点：LLM 输出实体清单（name + type + keywords + importance）；
/// - 证据：对每个实体用 `block_signature` + `cosine_top_k` 召回原文（零成本词袋，
///   不依赖 embed 服务）；
/// - 蒸馏：把召回证据交给 LLM 归纳出 3-5 句描述（只许从证据提炼）；
/// - 无召回证据的条目不落（tracing::warn），text 来自证据归纳。
pub async fn distill_world_lore(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<Vec<Value>, String> {
    if chapters.is_empty() {
        return Ok(vec![]);
    }
    let inventory_input = build_roster_input(title, chapters, max_chars);
    let inventory_system = "你是小说世界观梳理专家。请从给定小说正文中盘点世界观实体（世界/势力/地点/物品/规则/背景设定），只输出 JSON 数组，每个元素为 {\"name\":\"实体名\",\"type\":\"world|faction|location|item|rule|setting\",\"keywords\":[\"检索用关键词\"],\"importance\":\"high|medium|low\"}。importance 依据对剧情的重要性判断。只输出 JSON。";
    let inv_v = match llm_json(state, inventory_system, &inventory_input, "世界树[实体清单]").await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("世界树: 实体清单失败，跳过世界蒸馏: {e}");
            return Ok(vec![]);
        }
    };
    let inventory = as_json_array(inv_v);

    let rank = |im: &str| -> u8 { match im { "high" => 0, "medium" => 1, _ => 2 } };
    let mut entities: Vec<Value> = inventory;
    entities.sort_by_key(|e| rank(&jstr(e, "importance", "importance")));
    entities.truncate(MAX_WORLD_ENTITIES);

    let blocks = build_evidence_blocks(chapters);
    let mut out: Vec<Value> = Vec::new();
    for e in entities {
        let name = jstr(&e, "name", "name");
        if name.is_empty() {
            continue;
        }
        let kind = jstr(&e, "type", "type");
        let mut query = name.clone();
        for kw in jstr_arr(&e, "keywords", "keywords") {
            query.push(' ');
            query.push_str(&kw);
        }
        if !kind.is_empty() {
            query.push(' ');
            query.push_str(&kind);
        }

        // 词袋余弦 top-k 召回原文证据
        let qsig = block_signature(&query.trim());
        let hits = cosine_top_k(&qsig, &blocks, WORLD_EVIDENCE_TOP_K);
        if hits.is_empty() {
            tracing::warn!(name=%name, "世界树: 无召回证据，条目不落库");
            continue;
        }
        let mut evidence = String::new();
        let mut refs: Vec<String> = Vec::new();
        for (bi, _score) in hits {
            evidence.push_str(&format!("【{}】{}\n\n", blocks[bi].chapter, blocks[bi].text));
            refs.push(format!("{}:block{}", blocks[bi].chapter, bi + 1));
        }

        let card_user = format!(
            "小说《{title}》\n世界观实体：{name}（类型：{kind}）\n\
             检索到的原文证据（【ch】前缀指示章节）：\n{evidence}\n\
             请按系统要求基于证据为该实体输出一条严格 JSON 世界树条目。"
        );
        let card_system = "你是世界观设定整理专家。请基于提供的原文证据归纳该实体的描述。\
            核心约束：1) 只许从证据中提炼，禁止编造证据之外的信息；2) text 用 3-5 句描述该实体；\
            3) tags 为 2-4 个关键词；4) importance 为 high|medium|low；\
            5) type 为 world|faction|location|item|rule|setting；\
            6) confidence 为 0.0~1.0 的置信度；\
            7) activation 为 keyword|always|conditional（该实体何时被检索激活，keyword=靠关键词命中，always=常驻，conditional=条件触发）；\
            8) depth 为 1~10 的提示词深度（该实体对主线越核心越靠前）。\
            只输出 JSON，结构（严格）：\
            {\"title\":\"实体名\",\"text\":\"3-5句描述\",\"tags\":[\"关键词\"],\"permanent\":true,\"importance\":\"high|medium|low\",\"type\":\"location\",\"evidence_refs\":[\"ch01:block3\"],\"confidence\":0.9,\"activation\":\"keyword\",\"depth\":3}";
        let card_v = match llm_json(state, card_system, &card_user, &format!("世界树[{name}]")).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(name=%name, "世界树[{name}] 蒸馏失败，不落库: {e}");
                continue;
            }
        };
        let mut entry = match normalize_world_lore_entry(&card_v) {
            Some(e) => e,
            None => {
                tracing::warn!(name=%name, "世界树: 蒸馏条目缺 title/text，不落库");
                continue;
            }
        };
        // 兜底：若 evidence_refs 空则用本次召回补足
        let refs_empty = entry
            .get("evidence_refs")
            .and_then(|x| x.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if refs_empty {
            entry["evidence_refs"] = json!(refs);
        }
        out.push(entry);
    }
    Ok(out)
}

/// 每章节拍：对每章喂正文（截断到 max_chars），LLM 输出该章实际发生的 1-3 条硬节拍。
///
/// 返回 Vec 与 chapters 对齐；某章正文为空时该章产出空 Vec。
pub async fn distill_locked_beats(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<Vec<Vec<String>>, String> {
    // C6: 逐章并发化（并发度上限 4）
    let concurrency_limit = 4usize;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency_limit));

    let futures: Vec<_> = chapters.iter().enumerate().map(|(idx, (cid, body))| {
        let state_ref = state;
        let title = title.to_string();
        let cid = cid.clone();
        let body = body.clone();
        let sem = std::sync::Arc::clone(&semaphore);
        async move {
            if body.trim().is_empty() {
                return (idx, vec![]);
            }
            let _permit = sem.acquire().await.expect("semaphore closed");
            let part: String = body.chars().take(max_chars).collect();
            let user = format!(
                "小说《{title}》\n章节：{cid}\n本章正文：\n{part}\n\
                 请按系统要求输出本章的硬节拍 JSON 数组。"
            );
            let system = "你是剧本节拍分析师。请从本章正文中提炼该章实际发生的、不可改写的事实/转折/必须呈现的场面，\
                作为硬节拍。核心约束：1) 每条 ≤40 字；2) 1-3 条；3) 只许写本章实际发生之事，禁止编造或展望。\
                只输出 JSON 数组，如 [\"节拍一\",\"节拍二\"]。";
            let v = match llm_json(state_ref, system, &user, &format!("节拍[{cid}]")).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(cid=%cid, "节拍[{cid}] 失败，该章节拍置空: {e}");
                    return (idx, vec![]);
                }
            };
            (idx, parse_beats(&v, MAX_BEATS, BEAT_MAX_LEN))
        }
    }).collect();

    let results = join_all(futures).await;
    let mut out: Vec<Vec<String>> = Vec::with_capacity(chapters.len());
    // 保序：按 idx 排序后填充
    let mut sorted_results: Vec<(usize, Vec<String>)> = results.into_iter().collect();
    sorted_results.sort_by_key(|(idx, _)| *idx);
    for (_idx, beats) in sorted_results {
        out.push(beats);
    }
    Ok(out)
}

/// 逐章角色关系提取（Wave C，吸收自 AI-Reader-V2 `RelationshipFact` + ChapterFact 逐章模式）。
///
/// 每章喂正文（截断到 max_chars），LLM 输出该章出现的角色关系边：
/// `[{from, to, rel, note}]`——from/to 必须是人物专名（复用 alias_merge 安全过滤防
/// 「母亲/哥哥」这类语境称呼成为关系端点），rel 为关系类型（师徒/恋人/仇敌/上下级/…），
/// note ≤40 字证据。返回与 chapters 对齐；空章/失败产出空 Vec。
/// 输出边与 Kaleido L3 MemoryL3.edges 的 `{from,to,rel,note,turn}` 结构兼容。
pub async fn distill_chapter_relations(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<Vec<Vec<serde_json::Value>>, String> {
    let concurrency_limit = 4usize;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency_limit));

    let futures: Vec<_> = chapters.iter().enumerate().map(|(idx, (cid, body))| {
        let state_ref = state;
        let title = title.to_string();
        let cid = cid.clone();
        let body = body.clone();
        let sem = std::sync::Arc::clone(&semaphore);
        async move {
            if body.trim().is_empty() {
                return (idx, vec![]);
            }
            let _permit = sem.acquire().await.expect("semaphore closed");
            let part: String = body.chars().take(max_chars).collect();
            let user = format!(
                "小说《{title}》\n章节：{cid}\n本章正文：\n{part}\n\
                 请按系统要求输出本章的角色关系 JSON 数组。"
            );
            // [ENT] 系统提示改：允许亲属/语境称呼端点（母亲/父亲）；"我/我们"归叙述者真名；边带 kind。
            let system = "你是小说角色关系分析师。从本章正文中提取该章明确呈现的角色关系，\
                输出 JSON 数组：[{\"from\":\"角色A\",\"to\":\"角色B\",\"rel\":\"关系类型\",\"note\":\"一句话证据\",\"kind\":\"proper\"}]。\
                核心约束：1) from/to 必须是人物（明确人物专名 或 亲属/语境称呼如\"母亲\"\"父亲\"\"哥哥\"均可），\
                严禁输出句子片段、普通名词、指代不明的碎片；\
                2) 第一人称视角下\"我/我们\"一律用叙述者真名（如主角名），不输出\"我\"；\
                3) rel 用简洁关系词（师徒/恋人/夫妻/仇敌/上下级/同事/好友/敌对/暗恋/亲属…）；\
                4) note ≤40 字，写本章实际呈现的关系证据；5) 只写本章明确出现的关系，禁止臆测；\
                6) 每条边可带 kind 字段：proper（专名）/ kin（语境称呼如母亲/父亲）；\
                7) 最多 8 条。只输出 JSON 数组，不要其他文字。";
            let v = match llm_json(state_ref, system, &user, &format!("关系[{cid}]")).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(cid=%cid, "关系[{cid}] 失败，该章关系置空: {e}");
                    return (idx, vec![]);
                }
            };
            let mut edges = Vec::new();
            if let Some(arr) = v.as_array() {
                for e in arr {
                    let from = e
                        .get("from")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim();
                    let to = e
                        .get("to")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim();
                    let rel = e
                        .get("rel")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim();
                    if from.is_empty() || to.is_empty() || rel.is_empty() || from == to {
                        continue;
                    }
                    // [ENT] 端点分级：Proper/Kin → 保留（Kin 打 kind=kin）；Discard → 丢弃。
                    // 不再用 is_unsafe_alias 一刀切丢弃（否则"母亲"全书高频却 0 边）。
                    use kaleido_core::entity_resolve::{classify_endpoint, EndpointKind};
                    let from_kind = classify_endpoint(from);
                    let to_kind = classify_endpoint(to);
                    if matches!(from_kind, EndpointKind::Discard)
                        || matches!(to_kind, EndpointKind::Discard)
                    {
                        continue;
                    }
                    let note = e
                        .get("note")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(40)
                        .collect::<String>();
                    // [ENT] 输出边结构 {from,to,rel,note,kind?}；kind 仅任一端为 Kin 时输出。
                    let is_kin = matches!(from_kind, EndpointKind::Kin)
                        || matches!(to_kind, EndpointKind::Kin);
                    edges.push(if is_kin {
                        serde_json::json!({
                            "from": from,
                            "to": to,
                            "rel": rel,
                            "note": note,
                            "kind": "kin",
                        })
                    } else {
                        serde_json::json!({
                            "from": from,
                            "to": to,
                            "rel": rel,
                            "note": note,
                        })
                    });
                }
            }
            (idx, edges)
        }
    }).collect();

    let results = join_all(futures).await;
    let mut sorted_results: Vec<(usize, Vec<serde_json::Value>)> = results.into_iter().collect();
    sorted_results.sort_by_key(|(idx, _)| *idx);
    Ok(sorted_results.into_iter().map(|(_, v)| v).collect())
}

/// 多出口：按节点对应章节喂上下文，LLM 为每个节点生成 2-3 个候选出口。
///
/// - `node_ids` 与 `chapters` 对齐（第 i 个节点对应第 i 章）；
/// - LLM 只产出 `when`（玩家选项）与 `hint`（引导一句，不含 next id）；
/// - `next` 由本函数按原拓扑分配：按出口顺序循环映射到后续节点 id；
/// - 最后节点无后续节点时回退到自身（保持可推进的闭环）。
pub async fn distill_exits(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    node_ids: &[String],
    max_chars: usize,
) -> Result<Vec<Vec<NodeExit>>, String> {
    // C6: 逐章并发化（并发度上限 4）
    let concurrency_limit = 4usize;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency_limit));

    let futures: Vec<_> = chapters.iter().enumerate().map(|(i, (cid, body))| {
        let state_ref = state;
        let title = title.to_string();
        let cid = cid.clone();
        let body = body.clone();
        let node_id = node_ids.get(i).cloned().unwrap_or_else(|| format!("n{}", i + 1));
        let subsequent: Vec<String> = node_ids.get((i + 1)..).map(|s| s.to_vec()).unwrap_or_default();
        let sem = std::sync::Arc::clone(&semaphore);
        async move {
            if body.trim().is_empty() {
                return (i, vec![]);
            }
            let _permit = sem.acquire().await.expect("semaphore closed");
            let part: String = body.chars().take(max_chars).collect();
            let user = format!(
                "小说《{title}》\n当前节点：{node_id}（章节：{cid}）\n本章正文：\n{part}\n\
                 请按系统要求为玩家生成本节点结束时的候选出口 JSON 数组。"
            );
            let system = "你是剧情分支设计专家。请为当前节点生成 2-3 个玩家在本章结束时可能的选择出口。\
                核心约束：1) when 是玩家选项文案（第一人称行动，如\"决定追上去\"）；\
                2) hint 是引导下一步的一句话（不含 next 指向的节点 id）；\
                3) 出口必须源自本章内容。只输出 JSON 数组，如 [{\"when\":\"...\",\"hint\":\"...\"}]。";
            let v = match llm_json(state_ref, system, &user, &format!("多出口[{cid}]")).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(cid=%cid, "多出口[{cid}] 失败，该节点出口置空: {e}");
                    return (i, vec![]);
                }
            };
            let candidates = parse_exit_candidates(&v);
            if candidates.is_empty() {
                return (i, vec![]);
            }
            let mut exits: Vec<NodeExit> = Vec::new();
            for (j, (when, _hint)) in candidates.into_iter().enumerate() {
                let next = if subsequent.is_empty() {
                    node_id.clone()
                } else {
                    subsequent[j % subsequent.len()].clone()
                };
                exits.push(NodeExit {
                    id: format!("e{}-{}", i + 1, j),
                    when,
                    next,
                });
            }
            (i, exits)
        }
    }).collect();

    let results = join_all(futures).await;
    let mut out: Vec<Vec<NodeExit>> = Vec::with_capacity(chapters.len());
    let mut sorted_results: Vec<(usize, Vec<NodeExit>)> = results.into_iter().collect();
    sorted_results.sort_by_key(|(idx, _)| *idx);
    for (_idx, exits) in sorted_results {
        out.push(exits);
    }
    Ok(out)
}

/// 世界线：LLM 输出全书大事记 JSON 数组。
///
/// 仅产出结构化数据，落盘（worldline.json）由调用方负责。
pub async fn distill_worldline(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<Vec<Value>, String> {
    let input = build_roster_input(title, chapters, max_chars);
    let system = "你是小说大事记整理专家。请从给定小说正文中梳理全书关键事件的时间线，\
        只输出 JSON 数组，每个元素为 {\"event\":\"事件描述\",\"time_point\":\"时间/阶段\",\
        \"characters\":[\"涉及角色\"],\"importance\":\"high|medium|low\",\"chapter\":\"ch01\"}。\
        核心约束：只写正文实际发生的事件；chapter 用章节代号（如 ch01）。只输出 JSON。";
    let v = match llm_json(state, system, &input, "世界线[大事记]").await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("世界线: 失败，返回空: {e}");
            return Ok(vec![]);
        }
    };
    Ok(parse_worldline(&v))
}

// ─── 素材库蒸馏：事件包 / 演员状态 / 文风 / 规则检定 ─────────────────────────

/// 盘点小说关键实体与人物名（供事件包 prompt 注入真实世界元素）。
///
/// 失败时仅 `tracing::warn!` 并返回空数组（不影响主流程），单条降级。
async fn distill_name_inventory(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Vec<String> {
    let input = build_roster_input(title, chapters, max_chars);
    let system = "你是小说实体盘点助手。请从给定小说正文中盘点关键人物名与地名/势力/境界/物品/组织名，\
        只输出 JSON 数组，每个元素为一个名字字符串（如\"江景离\"、\"青云宗\"）。只输出 JSON，最多 30 个。";
    let v = match llm_json(state, system, &input, "事件包[实体盘点]").await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("事件包: 实体盘点失败，跳过实体注入: {e}");
            return vec![];
        }
    };
    as_json_array(v)
        .into_iter()
        .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .take(30)
        .collect()
}

/// 事件包解析（纯函数）：过滤非法卡/包，返回结构化 `EventPackage` 列表。
///
/// - 卡：id/title/prompt 缺一即跳过该卡；weight 缺省/为 0 时按 1；enabled 缺省 true；
///   once_per_session 兼容 snake/camel 两种键名。
/// - 包：id/name 缺一跳过；整包卡全非法则跳过该包；包数上限 [`MAX_EVENT_PACKAGES`]。
fn parse_event_packages(v: &Value) -> Vec<EventPackage> {
    let mut out: Vec<EventPackage> = Vec::new();
    for pkg in as_json_array(v.clone()) {
        let id = jstr(&pkg, "id", "id");
        let name = jstr(&pkg, "name", "name");
        if id.is_empty() || name.is_empty() {
            tracing::warn!("事件包蒸馏: 包缺 id/name，跳过");
            continue;
        }
        let mut cards: Vec<TellerEventCard> = Vec::new();
        let raw_cards = pkg
            .get("cards")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        for c in raw_cards {
            let card_id = jstr(&c, "id", "id");
            let card_title = jstr(&c, "title", "title");
            let card_prompt = jstr(&c, "prompt", "prompt");
            if card_id.is_empty() || card_title.is_empty() || card_prompt.is_empty() {
                tracing::warn!(pkg_id = %id, "事件包蒸馏: 卡缺 id/title/prompt，跳过");
                continue;
            }
            let weight = c
                .get("weight")
                .and_then(|x| x.as_u64())
                .map(|x| x as u32)
                .unwrap_or(1)
                .max(1);
            let enabled = c.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
            let once_per_session = c
                .get("once_per_session")
                .or_else(|| c.get("oncePerSession"))
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            // G7 新字段宽容解析：camel 优先、容忍 snake；全部缺省不跳卡（沿用「缺字段兜底」语义）
            let type_name = c
                .get("typeName")
                .or_else(|| c.get("type_name"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let category = jstr(&c, "category", "category");
            let tags = jstr_arr(&c, "tags", "tags");
            let intensity = jstr(&c, "intensity", "intensity");
            let cooldown_turns = c
                .get("cooldownTurns")
                .or_else(|| c.get("cooldown_turns"))
                .and_then(|x| x.as_u64())
                .map(|x| x as u32)
                .unwrap_or(0);
            // A1: 章节范围解析（蒸馏产出 chapterRange，基于切分 chXX 编号）。
            // 容忍 camel/snake，缺省空 = 未标注 → 运行时任意章可抽（A3 兼容旧 pack）。
            let chapter_range = c
                .get("chapterRange")
                .or_else(|| c.get("chapter_range"))
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            cards.push(TellerEventCard {
                id: card_id,
                title: card_title,
                prompt: card_prompt,
                weight,
                enabled,
                once_per_session,
                used_in_session: false,
                type_name,
                category,
                tags,
                intensity,
                cooldown_turns,
                chapter_range,
            });
            if cards.len() >= MAX_EVENT_CARDS {
                break;
            }
        }
        if cards.is_empty() {
            tracing::warn!(pkg_id = %id, "事件包蒸馏: 整包卡全非法，跳过该包");
            continue;
        }
        let description = jstr(&pkg, "description", "description");
        out.push(EventPackage {
            id,
            name,
            description: if description.is_empty() {
                None
            } else {
                Some(description)
            },
            enabled: pkg.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
            cards,
        });
        if out.len() >= MAX_EVENT_PACKAGES {
            break;
        }
    }
    out
}

/// 演员状态解析（纯函数）：宽容解析 LLM 输出的 ActorStatePackConfig。
///
/// - 解析失败返回默认空配置；
/// - 模板：仅保留非空 key 且 fields/trait_pools 至少一项的模板；
/// - initial_actors：仅保留 character_id/template_id 非空且模板存在的条目。
/// - [fix 2026-08-15 白名单过滤] `allowed_chars`：角色卡名单（Some 时生效）。
///   丢弃 character_id 不在名单的 initial_actors，并清理无引用的孤立模板
///   （实证：LLM 自创「舍友」模板但角色卡只有「莫旺财」→ 悬空引用）。
fn parse_actor_state_config(v: &Value, allowed_chars: Option<&std::collections::HashSet<String>>) -> ActorStatePackConfig {
    let mut cfg: ActorStatePackConfig = match serde_json::from_value(v.clone()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("演员状态蒸馏: 无法解析 ActorStatePackConfig: {e}");
            return ActorStatePackConfig::default();
        }
    };
    cfg.templates.retain(|k, t| {
        !k.trim().is_empty() && (!t.fields.is_empty() || !t.trait_pools.is_empty())
    });
    cfg.initial_actors.retain(|ia| {
        !ia.character_id.trim().is_empty()
            && !ia.template_id.trim().is_empty()
            && cfg.templates.contains_key(&ia.template_id)
            && allowed_chars.map_or(true, |s| s.contains(&ia.character_id.trim().to_string()))
    });
    // 清理无 initial_actors 引用的孤立模板（自创角色名被过滤后残留）
    let referenced: std::collections::HashSet<String> = cfg
        .initial_actors
        .iter()
        .map(|ia| ia.template_id.clone())
        .collect();
    cfg.templates.retain(|k, _| referenced.contains(k));
    cfg.schema_version = 1;
    cfg
}

/// 文风归一化（纯函数）：非对象返回空对象，字段缺省补空串。
fn normalize_narrative_style(v: &Value) -> Value {
    if !v.is_object() {
        return json!({});
    }
    json!({
        "style": jstr(v, "style", "style"),
        "tone": jstr(v, "tone", "tone"),
        "pacing": jstr(v, "pacing", "pacing"),
        "scene_focus": jstr(v, "scene_focus", "sceneFocus"),
        "prose_guidance": jstr(v, "prose_guidance", "proseGuidance"),
    })
}

/// 规则检定解析（纯函数）：宽容解析 RuleSystem，过滤 id/label/dice 空的条，
/// 上限 [`MAX_RULE_CHECKS`]，返回 `{"checks":[...]}`。
fn normalize_rule_system(v: &Value) -> Value {
    let raw = v.get("checks").cloned().unwrap_or_else(|| v.clone());
    let sys: RuleSystem = match serde_json::from_value(json!({ "checks": raw })) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("规则检定: 解析失败，返回空: {e}");
            return json!({ "checks": [] });
        }
    };
    let mut out: Vec<Value> = Vec::new();
    for c in sys.checks {
        if c.id.trim().is_empty() || c.label.trim().is_empty() || c.dice.trim().is_empty() {
            continue;
        }
        out.push(serde_json::to_value(c).unwrap_or_else(|_| json!({})));
        if out.len() >= MAX_RULE_CHECKS {
            break;
        }
    }
    if out.len() < MIN_RULE_CHECKS {
        tracing::warn!(
            got = out.len(),
            "规则检定: 有效条数低于下限，仍返回已收集部分"
        );
    }
    json!({ "checks": out })
}

/// 素材库蒸馏：事件包。LLM 一次生成 2~4 个事件包（奇遇/战斗/社交/修炼/日常等），
/// 每包 3~5 张 `TellerEventCard`，prompt 必须使用小说世界真实元素（盘点实体名注入 system）。
///
/// LLM 失败/解析失败一律 `tracing::warn!` 并返回 Ok(已收集部分或空)，不整批中止。
pub async fn distill_event_packages(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<Vec<EventPackage>, String> {
    if chapters.is_empty() {
        return Ok(vec![]);
    }
    let input = build_roster_input(title, chapters, max_chars);
    // 盘点实体/人物名（失败降级为空，不影响主流程）
    let names = distill_name_inventory(state, title, chapters, max_chars).await;
    let entity_hint = if names.is_empty() {
        String::new()
    } else {
        format!(
            "\n本小说的关键实体/人物（事件卡 prompt 必须尽量使用这些真实名字，而非泛称）：\n{}",
            names.join("、")
        )
    };
    let system = format!(
        "你是小说演出事件设计师。请从给定小说正文归纳该世界的演出事件包。\
         每个事件包是一组\"事件卡\"，在每回合演出时按权重抽取一张，注入 LLM 演出提示。\
         核心约束：1) 按故事类型组织事件包（如奇遇/战斗/社交/修炼/日常/悬疑），2-4 个包；\
         2) 每包 3-5 张事件卡；3) 事件卡 prompt 必须使用小说世界的真实元素（地名/人物/境界/势力），\
         描述该事件如何在本世界演出，禁止空泛模板；4) enabled 默认 true；\
         5) 对\"一次性的奇遇/大事件\"卡 oncePerSession 设 true，日常可重复卡设 false；\
         6) weight 为抽取权重（1-10）；\
         7) typeName 为用户可见的事件类型名（如「外门考核打脸」）；category 从包名/卡性质归纳\
         （如 打脸/奇遇/秘境/恋爱/冲突…）；\
         8) intensity 三档 low/medium/high；tags 给 2-3 个短标签便于检索；\
         9) cooldownTurns 为冷却回合：可重复卡设 2-3（抽过后 N 回合内不再抽），\
         一次性大事件卡设 0 或高值。\
         10) chapterRange 标注该事件发生的章节范围，格式为切分序号 chXX 列表\
         （如 \"ch01\"、[\"ch01\",\"ch03\"]）。输入章节前缀【chXX:原著标题】中的 chXX\
         即切分序号，必须使用该编号体系（严禁写中文数字或原著标题）。\
         日常/持续性事件可标较宽范围，一次性大事件（如首次登场/名场面）标精确章节甚至单章。{entity_hint}\
         只输出 JSON 数组，结构（严格）：\
         [{{\"id\":\"pkg-adventure\",\"name\":\"奇遇包\",\"description\":\"描述\",\"enabled\":true,\
         \"cards\":[{{\"id\":\"card-1\",\"title\":\"标题\",\"prompt\":\"演出提示\",\"weight\":3,\
         \"enabled\":true,\"oncePerSession\":true,\"typeName\":\"外门考核打脸\",\"category\":\"打脸\",\
         \"tags\":[\"门派\",\"考核\"],\"intensity\":\"medium\",\"cooldownTurns\":2,\"chapterRange\":[\"ch01\",\"ch03\"]}}]}}]"
    );
    let v = match llm_json(state, &system, &input, "事件包[生成]").await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("事件包: 生成失败，返回空: {e}");
            return Ok(vec![]);
        }
    };
    Ok(parse_event_packages(&v))
}

/// 素材库蒸馏：演员状态。LLM 一次生成完整 ActorStatePackConfig JSON，
/// 按角色卡为各角色建 template（字段含数值范围/枚举/更新说明）并声明 initial_actors。
///
/// LLM 失败/解析失败一律 `tracing::warn!` 并返回 Ok(默认空配置)，不整批中止。
pub async fn distill_actor_state(
    state: &AppState,
    title: &str,
    characters: &[PackCharacterRef],
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<ActorStatePackConfig, String> {
    let cfg = ActorStatePackConfig::default();
    if chapters.is_empty() {
        return Ok(cfg);
    }
    let input = build_roster_input(title, chapters, max_chars);
    let mut char_roster = String::new();
    for c in characters {
        char_roster.push_str(&format!(
            "- {}（{}）\n",
            c.name,
            if c.role.is_empty() { "未知" } else { &c.role }
        ));
    }
    let system = "你是小说角色状态配置专家。请为给定小说的角色设计 ActorState 模板与初始状态。\
        每个角色一个模板（字段：数值状态如修为/境界/伤势/好感/金钱，含 valueType/min/max/value；\
        字符串状态如身份；枚举状态如势力立场；可附带特质池 traitPools）。\
        核心约束：1) 主角与重要配角各建一个模板；2) 每个模板对应 initialActors 中一条\
        （characterId 用角色卡名，templateId 指向 templates 的 key）；\
        3) valueType 为 number|string|bool|enum|object|list，number 必须给 min/max；\
        4) value 为初始值；5) display 为该字段展示名；6) updateInstruction 说明剧情中该字段如何变化。\
        [fix 2026-08-15 白名单] characterId 必须严格取自下方「角色卡清单」中的名字，\
        禁止自创清单外的角色名（如「舍友」——若清单里是「莫旺财」就必须用「莫旺财」）；\
        只输出 JSON，结构（严格）：\
        {\"schemaVersion\":1,\"initialActors\":[{\"characterId\":\"角色名\",\"templateId\":\"tpl-xxx\"}],\
        \"templates\":{\"tpl-xxx\":{\"fields\":{\"修为\":{\"valueType\":\"number\",\"value\":10,\"min\":0,\"max\":100,\
        \"display\":\"修为\",\"updateInstruction\":\"修炼/战斗后变化\"}},\
        \"traitPools\":[{\"id\":\"pool-1\",\"name\":\"特质\",\"description\":\"...\",\
        \"traits\":[{\"id\":\"t1\",\"name\":\"...\",\"summary\":\"...\",\"weight\":1}]}]}}}";
    let user = format!(
        "{input}\n角色卡清单：\n{char_roster}\n请按系统要求为这些角色生成 ActorStatePackConfig JSON。"
    );
    let v = match llm_json(state, system, &user, "演员状态[生成]").await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("演员状态: 生成失败，返回空: {e}");
            return Ok(cfg);
        }
    };
    // [DIAG] 落盘 LLM 原始返回 + 白名单过滤详情，排查「白名单后全空」场景
    {
        let allow: std::collections::HashSet<String> = characters
            .iter()
            .map(|c| c.name.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        let raw_names: Vec<String> = v
            .get("initialActors")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("characterId").and_then(|c| c.as_str()))
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let rejected: Vec<&String> = raw_names
            .iter()
            .filter(|n| !allow.contains(n.as_str()))
            .collect();
        tracing::info!(
            raw_count = raw_names.len(),
            rejected_count = rejected.len(),
            rejected = ?rejected,
            "演员状态[白名单诊断]"
        );
    }
    Ok(parse_actor_state_config(
        &v,
        Some(
            &characters
                .iter()
                .map(|c| c.name.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect::<std::collections::HashSet<_>>(),
        ),
    ))
}

/// 素材库蒸馏：文风引导。LLM 归纳全书作者文风，
/// 输出 `{style,tone,pacing,scene_focus,prose_guidance}` 写入 resolved_snapshot.narrative_style。
///
/// LLM 失败/解析失败一律 `tracing::warn!` 并返回 Ok(空对象)，不整批中止。
pub async fn distill_narrative_style(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<Value, String> {
    if chapters.is_empty() {
        return Ok(json!({}));
    }
    let input = build_roster_input(title, chapters, max_chars);
    let system = "你是小说文风分析师。请从给定小说正文归纳作者的叙事文风，只输出 JSON 对象，字段：\
        style（叙事风格，如\"第三人称全知\"\"第一人称沉浸\"）；\
        tone（整体基调，如\"热血\"\"悬疑\"\"日常治愈\"）；\
        pacing（叙事节奏，如\"快节奏爽文\"\"慢热细腻\"）；\
        scene_focus（描写侧重，如\"战斗场面\"\"人物心理\"\"环境氛围\"）；\
        prose_guidance（对 LLM 演出时的行文指引，2-3 句，指出该仿照的句式/修辞/人称习惯）。\
        只输出 JSON。";
    let v = match llm_json(state, system, &input, "文风[生成]").await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("文风: 生成失败，返回空: {e}");
            return Ok(json!({}));
        }
    };
    Ok(normalize_narrative_style(&v))
}

/// 方案A(2026-08-16)：`node.entry` 叙事入口蒸馏。
///
/// 回推最初设计（12c748ca）：`node.entry` 语义应为"玩家进入该节点第一眼看到的
/// 叙事入口句"（如 `雨夜，玩家抵达旧茶馆门前`），而非 `build_pack_from_chapters`
/// 里硬编码的占位 `"本章开始"`。这里用单次 LLM 调用为每个节点生成 1~2 句叙事入口。
///
/// 失败不阻主线：返回 `HashMap<node_id, entry>`；解析/LLM 失败返回 Err，
/// 由调用方 `tracing::warn!` 后保留原占位 entry。
pub async fn distill_node_entries(
    state: &AppState,
    title: &str,
    nodes: &[kaleido_core::StoryNode],
    chapters: &[(String, String)],
) -> Result<std::collections::HashMap<String, String>, String> {
    let client = reqwest::Client::new();
    let llm = state
        .app_state
        .resolve_llm(state.llm_base.as_deref(), state.llm_key.as_deref(), &state.llm_model);
    if llm.base_url.trim().is_empty() || llm.api_key.trim().is_empty() {
        return Err("节点入口蒸馏: LLM 未配置".into());
    }
    let chapter_map: std::collections::HashMap<&str, &str> =
        chapters.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut blocks = Vec::new();
    for n in nodes {
        let head = chapter_map
            .get(n.chapter_id.as_str())
            .map(|b| b.chars().take(600).collect::<String>())
            .unwrap_or_default();
        if head.trim().is_empty() {
            continue;
        }
        blocks.push(json!({
            "nodeId": n.id,
            "chapter": n.chapter_id,
            "title": n.title,
            "opening": head,
        }));
    }
    if blocks.is_empty() {
        return Ok(Default::default());
    }
    let system = "你是小说剧本节点的'叙事入口'提炼助手。为每个节点从对应章节正文开头提炼一句'入口'(entry)：玩家进入该节点第一眼看到的、有画面感、带氛围/微悬疑的场景句，2~30 字，第三人称。示例：'雨夜，玩家抵达旧茶馆门前'、'推门见林晚，对上约定'。\
规则：1) 只能从该章开头已发生/已在场的事实提炼，不得编造人物或事件；2) 不得剧透本章后续的大事件/转折；3) 不用将来时、不用推测语气；4) 该章开头纯抒情/过渡、无实质场景时 entry 置空字符串。\
只输出 JSON 数组，元素为 {\"nodeId\":\"输入中的值\",\"entry\":\"入口句或空\"}，nodeId 必须与输入一一对应，不要输出任何解释文字。";
    let user_json = serde_json::to_string(&json!({
        "title": title,
        "nodes": blocks,
    }))
    .unwrap_or_default();
    let raw = crate::llm_stream::chat_completion_dispatch(
        &llm.base_url,
        &llm.api_key,
        &llm.model,
        &state.provider_kind,
        system,
        &user_json,
        0.1, 16384, LLM_TIMEOUT_SECS,
        &client,
    )
    .await
    .map_err(|e| format!("节点入口蒸馏: {e}"))?;
    let v = crate::llm_stream::extract_json_value(&raw)
        .ok_or_else(|| "节点入口蒸馏: 无法解析 LLM 返回的 JSON".to_string())?;
    let arr = match v {
        Value::Array(a) => a,
        other => vec![other],
    };
    let mut out = std::collections::HashMap::new();
    for e in arr {
        let id = e.get("nodeId").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let entry = e.get("entry").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if !id.is_empty() && !entry.trim().is_empty() {
            out.insert(id, entry.trim().to_string());
        }
    }
    Ok(out)
}

/// 素材库蒸馏：规则检定。LLM 输出 4~8 条与小说世界匹配的 RuleCheck（d20/d100），
/// 结果 `{"checks":[...]}` 写入 resolved_snapshot.rule_system。
///
/// LLM 失败/解析失败一律 `tracing::warn!` 并返回 Ok(空 checks)，不整批中止。
pub async fn distill_rule_system(
    state: &AppState,
    title: &str,
    chapters: &[(String, String)],
    max_chars: usize,
) -> Result<Value, String> {
    if chapters.is_empty() {
        return Ok(json!({ "checks": [] }));
    }
    let input = build_roster_input(title, chapters, max_chars);
    let system = "你是小说规则检定设计专家。请从给定小说正文归纳该世界适用的规则检定（d20 或 d100）。\
        核心约束：1) 4-8 条，与小说世界匹配（如修炼突破/战斗/交涉/盗窃/察觉/炼丹等）；\
        2) 每条字段：id（唯一）、label（检定名）、dice（d20 或 d100）、modifier（修正值，可负）、\
        failurePolicy（\"retry|block|escalate\"）、difficultyGuidance（难度指引）、\
        stateEffectGuidance（状态影响说明）、trigger（触发条件）、mustCheckExamples（必须检定的场景）、\
        skipCheckExamples（免检定的场景）、successHint、failureHint、\
        stateBindings（可选：[{\"field\":\"角色.字段\",\"onSuccess\":\"+1\"}]）。\
        只输出 JSON，结构（严格）：{\"checks\":[...]}。";
    let v = match llm_json(state, system, &input, "规则检定[生成]").await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("规则检定: 生成失败，返回空: {e}");
            return Ok(json!({ "checks": [] }));
        }
    };
    Ok(normalize_rule_system(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 角色名合法性过滤：历史乱码名全部拦截，真名/称呼全部放行。
    /// 注：「湮风通」「人里克斯」这类无虚词罕见组合由第二层兜底拦截
    /// （auto cast 正文存在性 / 角色谱证据检索），纯规则层不拦。
    #[test]
    fn plausible_name_rejects_garbage_fragments() {
        let garbage = [
            "冲进下水", "过电车轨", "向越过街", "里有一种", "像是想", "下拉出一", "那种味",
            "那就", "有自己知", "示意我别", "尽管我知", "另一个", "纷纷询", "粮食", "留下一",
            "听早早", "身后", "突然", "颜色",
            "",
            "abcdefghijklm", // 13 字超长
            "林小宇、苏婉", // 带标点
        ];
        for g in garbage {
            assert!(!is_plausible_character_name(g), "应拦截乱码名: {g}");
        }
    }

    #[test]
    fn plausible_name_keeps_real_names() {
        let real = [
            "林小宇", "苏婉", "林远", "林逸", "陆清韵", "林父", "光头男", "沈棠知", "苏早",
            "外婆", "弗雷德", "王一博", "李进", "妈妈", "爸爸", "老板", "小美", "老王",
            "向明初", "莫旺财", "山楂", "冯婷", "浓妆女人",
        ];
        for r in real {
            assert!(is_plausible_character_name(r), "应放行真名: {r}");
        }
    }

    /// 余弦相似度：同向高分、垂直为 0、维度不一致为 0。
    #[test]
    fn cosine_sim_basic() {
        assert_eq!(cosine_sim(&[], &[]), 0.0);
        assert_eq!(cosine_sim(&[1.0], &[1.0, 2.0]), 0.0);
        assert!((cosine_sim(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_sim(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert!((cosine_sim(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
    }

    /// card_to_ref 能解析女娲/仓颉三维认知字段（mental_models / decision_heuristics / beliefs），
    /// 且缺失时安全回退为空数组（向后兼容旧 LLM 输出）。
    #[test]
    fn card_to_ref_parses_nuwa_cangjie_fields() {
        let card = serde_json::json!({
            "name": "沈棠",
            "identity": "主角",
            "personality": "谨慎嘴硬心软",
            "speech_style": "短句",
            "evidence_refs": ["ch01:block3"],
            "mental_models": ["棋手:把人心当棋盘（ch02）"],
            "decision_heuristics": ["先找退路:遇险先保后路再谈成败（ch04）"],
            "beliefs": ["规则大于人情:因师门教训形成（ch03）"],
            "opening_scene": "暮色书斋，烛火明灭，窗外的雨敲在青石板上。",
            "opening_lines": "「沈棠在这里候你多时。」她合上手中的卷册，眉眼抬向你。",
            "nsfw_profile": "露骨→禁止；接吻以上→nsfw；日常对白→non",
        });
        let c = card_to_ref(0, "沈棠", "high", &card);
        assert_eq!(c.mental_models, vec!["棋手:把人心当棋盘（ch02）".to_string()]);
        assert_eq!(c.decision_heuristics, vec!["先找退路:遇险先保后路再谈成败（ch04）".to_string()]);
        assert_eq!(c.beliefs, vec!["规则大于人情:因师门教训形成（ch03）".to_string()]);
        assert_eq!(c.role, "主角");
        assert_eq!(c.opening_scene, "暮色书斋，烛火明灭，窗外的雨敲在青石板上。");
        assert!(c.opening_lines.contains("沈棠在这里候你多时"), "first_mes 解析, got: {}", c.opening_lines);
        assert_eq!(c.nsfw_profile, "露骨→禁止；接吻以上→nsfw；日常对白→non");

        // 缺省时安全回退
        let bare = serde_json::json!({ "name": "无名", "identity": "" });
        let b = card_to_ref(1, "无名", "low", &bare);
        assert!(b.mental_models.is_empty());
        assert!(b.decision_heuristics.is_empty());
        assert!(b.beliefs.is_empty());
        assert!(b.nsfw_profile.is_empty(), "缺省回退为空串");
    }

    /// top-k 排序正确：相似度最高的排最前，并按 index 对齐。
    #[test]
    fn cosine_top_k_ranks_desc() {
        let blocks = vec!["苹果 苹果 苹果", "香蕉 香蕉", "苹果", "橘子 橘子 橘子 橘子"]
            .into_iter()
            .map(|t| EvidenceBlock { chapter: "ch01".into(), text: t.into() })
            .collect::<Vec<_>>();
        let query = block_signature("苹果");
        let hits = cosine_top_k(&query, &blocks, 4);
        assert_eq!(hits.len(), 4);
        // 严格降序
        for w in hits.windows(2) {
            assert!(w[0].1 >= w[1].1);
        }
        // 与查询完全同构的块（单次"苹果"）相似度最高 → 应排第一
        assert_eq!(hits[0].0, 2);
        // 纯相关（多次"苹果"+空格稀释方向）仍高于完全不相关块
        let idx0 = hits.iter().position(|(i, _)| *i == 0).unwrap();
        let idx3 = hits.iter().position(|(i, _)| *i == 3).unwrap();
        assert!(idx0 < idx3);
    }

    /// top-k 截断：只返回前 k 个。
    #[test]
    fn cosine_top_k_truncates() {
        let blocks = vec![
            EvidenceBlock { chapter: "ch01".into(), text: "a b c d e f g h".into() },
            EvidenceBlock { chapter: "ch02".into(), text: "1 2 3 4 5 6 7 8".into() },
            EvidenceBlock { chapter: "ch03".into(), text: "! @ # $ % ^ & *".into() },
            EvidenceBlock { chapter: "ch04".into(), text: "q w e r t y u i".into() },
        ];
        let query = block_signature("a b c d e f g h");
        let hits = cosine_top_k(&query, &blocks, 2);
        assert_eq!(hits.len(), 2);
        // k 超过块数 → 全部返回
        let all = cosine_top_k(&query, &blocks, 99);
        assert_eq!(all.len(), 4);
    }

    /// 分块边界：~800 字重叠 100 字的切分行为。
    #[test]
    fn split_blocks_boundaries() {
        // 长度 < block size → 单块（整段）
        let short = "短".repeat(50);
        let blocks = split_into_blocks(&short, BLOCK_SIZE, BLOCK_OVERLAP);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].chars().count(), 50);

        // 长度 2000 字 → 应有约 3 块（步长 700）
        let long = "长".repeat(2000);
        let blocks = split_into_blocks(&long, BLOCK_SIZE, BLOCK_OVERLAP);
        assert!(blocks.len() >= 2);
        // 相邻块应当重叠（相邻块最多差 block_size-overlap 个新增字符）
        for b in &blocks {
            assert!(!b.is_empty());
            assert!(b.chars().count() <= BLOCK_SIZE);
        }
        // 空输入
        assert!(split_into_blocks("", BLOCK_SIZE, BLOCK_OVERLAP).is_empty());
    }

    /// 从召回证据字符串提取章节代号。
    #[test]
    fn extract_evidence_chapters_dedup() {
        let ev = "【ch01】第一段\n\n【ch02】第二段\n【ch01】更多\n";
        let tags = extract_evidence_chapters(ev);
        assert_eq!(tags, vec!["ch01".to_string(), "ch02".to_string()]);
    }

    /// 角色卡重试：mock LLM 首次返回空、第二次返回合法 JSON，应产出该角色卡。
    #[tokio::test]
    async fn retry_character_card_yields_on_second_response() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let card_v = distill_one_character_card("沈棠", "medium", {
            let calls = std::rc::Rc::clone(&calls);
            move || {
                let calls = std::rc::Rc::clone(&calls);
                async move {
                    let n = calls.get();
                    calls.set(n + 1);
                    if n == 0 {
                        Err("extraction empty response".into())
                    } else {
                        Ok("{\"name\":\"沈棠\",\"personality\":\"谨慎嘴硬心软\"}".into())
                    }
                }
            }
        })
        .await;
        let card_v = card_v.expect("首次空响应后重试应产出合法 JSON 卡");
        assert_eq!(calls.get(), 2);
        assert_eq!(card_v["name"], "沈棠");
        assert_eq!(card_v["personality"], "谨慎嘴硬心软");
    }

    // 角色卡重试：mock LLM 恒空，重试 3 次后应返回 None 且不 panic（单角色被跳过）。
    #[tokio::test]
    async fn retry_character_card_skips_on_persistent_empty() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let card_v = distill_one_character_card("路人甲", "low", {
            let calls = std::rc::Rc::clone(&calls);
            move || {
                let calls = std::rc::Rc::clone(&calls);
                async move {
                    calls.set(calls.get() + 1);
                    Ok(String::new())
                }
            }
        })
        .await;
        assert!(card_v.is_none(), "恒空响应下应跳过该角色");
        assert_eq!(calls.get(), 4, "low 角色 fffd 配额 4 次（83746baf 3→4），应恰好在 4 次后放弃");
    }

    // 角色卡重试：mock LLM 首次返回含 U+FFFD 损坏字的 JSON、第二次返回干净 JSON，
    // 应丢弃损坏卡重试并产出干净卡（修复上游偶发把单字采样成 FFFD）。
    #[tokio::test]
    async fn retry_character_card_skips_fffd_and_yields_clean() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let card_v = distill_one_character_card("沈棠", "high", {
            let calls = std::rc::Rc::clone(&calls);
            move || {
                let calls = std::rc::Rc::clone(&calls);
                async move {
                    let n = calls.get();
                    calls.set(n + 1);
                    if n == 0 {
                        Ok("{\"name\":\"沈棠\",\"personality\":\"对弗\u{FFFD}\u{FFFD}德的信任\"}"
                            .into())
                    } else {
                        Ok("{\"name\":\"沈棠\",\"personality\":\"对弗雷德的信任\"}".into())
                    }
                }
            }
        })
        .await;
        let card_v = card_v.expect("含 FFFD 损坏字重试后应产出干净 JSON 卡");
        assert_eq!(calls.get(), 2, "首次损坏应触发重试");
        assert_eq!(card_v["personality"], "对弗雷德的信任");
    }

    // ─── Stage 3 纯函数测试 ─────────────────────────────────────────────────

    /// 世界树条目缺字段兜底：permanent/tags/importance/type/confidence 均有默认值。
    #[test]
    fn normalize_world_lore_defaults() {
        let v = json!({"title": "青云宗", "text": "以剑修为主的宗门。"});
        let e = normalize_world_lore_entry(&v).expect("title+text 应有条目");
        assert_eq!(e["title"], "青云宗");
        assert_eq!(e["text"], "以剑修为主的宗门。");
        assert_eq!(e["tags"].as_array().map(|a| a.len()), Some(0));
        assert_eq!(e["permanent"], true);
        assert_eq!(e["importance"], "low");
        assert_eq!(e["type"], "setting");
        assert_eq!(e["confidence"].as_f64().unwrap(), 0.5);
    }

    /// 世界树条目缺 title/text 不落库。
    #[test]
    fn normalize_world_lore_rejects_missing_core() {
        assert!(normalize_world_lore_entry(&json!({"title": "无文本"})).is_none());
        assert!(normalize_world_lore_entry(&json!({"text": "无标题"})).is_none());
        assert!(normalize_world_lore_entry(&json!({"title": "  ", "text": "  "})).is_none());
    }

    /// confidence 越界夹紧、snake_case 字段可读。
    #[test]
    fn normalize_world_lore_clamps_confidence() {
        let v = json!({
            "title": "青云宗",
            "text": "以剑修为主的宗门。",
            "permanent": false,
            "tags": ["宗门"],
            "importance": "high",
            "type": "faction",
            "confidence": 1.7,
            "evidence_refs": ["ch01:block2"],
        });
        let e = normalize_world_lore_entry(&v).unwrap();
        assert_eq!(e["permanent"], false);
        assert_eq!(e["importance"], "high");
        assert_eq!(e["type"], "faction");
        assert_eq!(e["confidence"].as_f64().unwrap(), 1.0);
        assert_eq!(e["evidence_refs"][0], "ch01:block2");
    }

    /// 节拍数组解析：截断长度与条数、过滤空串。
    #[test]
    fn beats_parse_truncates() {
        let v = json!(["节拍一", "节拍二", "", "长".repeat(60), "节拍三", "节拍四"]);
        let beats = parse_beats(&v, 3, 40);
        assert_eq!(beats.len(), 3);
        for b in &beats {
            assert!(b.chars().count() <= 40);
        }
        // 对象包装 / 字符串包装容错
        let empty: Vec<String> = vec![];
        assert_eq!(parse_beats(&json!({"k": "v"}), 3, 40), empty);
        assert_eq!(parse_beats(&json!("单条"), 3, 40), vec!["单条".to_string()]);
    }

    /// 出口候选解析：when 非空、hint 可空、上限 3。
    #[test]
    fn exit_candidates_parse() {
        let v = json!([
            {"when": "决定追上去", "hint": "看看那人去了哪"},
            {"when": "", "hint": "空 when 应被过滤"},
            {"when": "留在原地", "hint": ""},
            {"when": "第四项"},
            {"when": "第五项"},
        ]);
        let c = parse_exit_candidates(&v);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], ("决定追上去".to_string(), "看看那人去了哪".to_string()));
        assert_eq!(c[1], ("留在原地".to_string(), String::new()));
    }

    /// 世界线 JSON 解析容错：event 非空保留、其余丢弃，非数组包装后过滤。
    #[test]
    fn worldline_parse_tolerant() {
        let v = json!([
            {"event": "宗门大战", "time_point": "卷一", "characters": ["叶辰"], "importance": "high", "chapter": "ch01"},
            {"time_point": "无事件被丢弃", "characters": []},
            "字符串项被丢弃",
        ]);
        let list = parse_worldline(&v);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["event"], "宗门大战");
        // 对象（非数组）包装后过滤
        let obj = parse_worldline(&json!({"event": "单事件"}));
        assert_eq!(obj.len(), 1);
        assert_eq!(obj[0]["event"], "单事件");
        let empty = parse_worldline(&json!([]));
        assert_eq!(empty.len(), 0);
    }

    // ─── 素材库蒸馏解析测试（纯函数，不依赖 LLM）───────────────────────────────

    /// 事件包解析：合法样例产出结构化包与卡，缺字段兜底生效。
    #[test]
    fn event_packages_parse_valid() {
        let v = json!([
            {
                "id": "pkg-adventure",
                "name": "奇遇包",
                "description": "主角在山野间的奇遇",
                "enabled": true,
                "cards": [
                    {"id": "card-1", "title": "悬崖奇遇", "prompt": "江景离在青云宗后山悬崖发现遗迹", "weight": 3, "enabled": true, "oncePerSession": true},
                    {"id": "card-2", "title": "集市偶遇", "prompt": "主角在临安城集市遇到神秘摊主"},
                    {"id": "card-3", "title": "灵兽拦路", "prompt": "一只灵兽拦住去路", "weight": 1, "enabled": true, "once_per_session": false}
                ]
            }
        ]);
        let pkgs = parse_event_packages(&v);
        assert_eq!(pkgs.len(), 1);
        let p = &pkgs[0];
        assert_eq!(p.id, "pkg-adventure");
        assert_eq!(p.name, "奇遇包");
        assert!(p.enabled);
        assert_eq!(p.cards.len(), 3);
        // weight 缺省兜底 1；oncePerSession 缺省 false
        assert_eq!(p.cards[1].weight, 1);
        assert!(!p.cards[1].once_per_session);
        assert!(!p.cards[1].used_in_session);
        // 兼容 snake_case once_per_session
        assert!(!p.cards[2].once_per_session);
        // 事件包能被 pick_event_card 抽出（模块语义：启用包 + enabled 卡）
        let pkg_value = serde_json::to_value(&pkgs).unwrap();
        assert!(pkg_value.as_array().is_some());
    }

    /// 事件包解析：非法卡（缺 id/title/prompt）跳过；整包卡全非法则跳过该包。
    #[test]
    fn event_packages_parse_skips_invalid() {
        let v = json!([
            {
                "id": "pkg-bad",
                "name": "坏包",
                "cards": [
                    {"id": "", "title": "无 id", "prompt": "x"},
                    {"id": "c", "title": "", "prompt": "x"},
                    {"id": "c", "title": "无 prompt"}
                ]
            },
            {
                "id": "pkg-ok",
                "name": "好包",
                "cards": [
                    {"id": "c1", "title": "正常卡", "prompt": "有效提示", "weight": 5}
                ]
            }
        ]);
        let pkgs = parse_event_packages(&v);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].id, "pkg-ok");
        assert_eq!(pkgs[0].cards.len(), 1);
        // 缺 id/name 的包被跳过
        let bad_pkg = json!([{"name": "无 id 包", "cards": [{"id": "c", "title": "t", "prompt": "p"}]}]);
        assert!(parse_event_packages(&bad_pkg).is_empty());
        // 非数组包装容错
        let wrapped = json!({"id": "pkg-w", "name": "w", "cards": [{"id": "c", "title": "t", "prompt": "p"}]});
        let pkgs2 = parse_event_packages(&wrapped);
        assert_eq!(pkgs2.len(), 1);
    }

    /// 事件包解析：包数与卡数有上限。
    #[test]
    fn event_packages_parse_caps() {
        let pkgs: Vec<Value> = (0..8)
            .map(|i| {
                let cards: Vec<Value> = (0..8)
                    .map(|j| json!({"id": format!("c{i}-{j}"), "title": "t", "prompt": "p", "weight": 1}))
                    .collect();
                json!({"id": format!("pkg-{i}"), "name": format!("包{i}"), "cards": cards})
            })
            .collect();
        let out = parse_event_packages(&json!(pkgs));
        assert_eq!(out.len(), MAX_EVENT_PACKAGES);
        for p in &out {
            assert!(p.cards.len() <= MAX_EVENT_CARDS);
        }
        assert_eq!(out[0].cards.len(), MAX_EVENT_CARDS);
    }

    /// 事件包解析 G7：camelCase 新字段（typeName/category/tags/intensity/cooldownTurns）落盘。
    #[test]
    fn event_packages_parse_new_fields_camel() {
        let v = json!([
            {
                "id": "pkg-adventure",
                "name": "奇遇包",
                "cards": [
                    {
                        "id": "card-1",
                        "title": "牌坊考核",
                        "prompt": "外门考核开始",
                        "typeName": "外门考核打脸",
                        "category": "打脸",
                        "tags": ["门派", "考核", "打脸"],
                        "intensity": "medium",
                        "cooldownTurns": 2
                    }
                ]
            }
        ]);
        let pkgs = parse_event_packages(&v);
        assert_eq!(pkgs.len(), 1);
        let c = &pkgs[0].cards[0];
        assert_eq!(c.type_name, "外门考核打脸");
        assert_eq!(c.category, "打脸");
        assert_eq!(c.tags, vec!["门派".to_string(), "考核".to_string(), "打脸".to_string()]);
        assert_eq!(c.intensity, "medium");
        assert_eq!(c.cooldown_turns, 2);
    }

    /// 事件包解析 G7：snake_case 新字段（type_name/category/tags/intensity/cooldown_turns）兼容落盘。
    #[test]
    fn event_packages_parse_new_fields_snake() {
        let v = json!([
            {
                "id": "pkg-romance",
                "name": "恋爱包",
                "cards": [
                    {
                        "id": "card-2",
                        "title": "误入香闺",
                        "prompt": "雨夜误入沈棠闺房",
                        "type_name": "误入香闺",
                        "category": "恋爱",
                        "tags": ["闺房", "幽会"],
                        "intensity": "low",
                        "cooldown_turns": 3
                    }
                ]
            }
        ]);
        let pkgs = parse_event_packages(&v);
        assert_eq!(pkgs.len(), 1);
        let c = &pkgs[0].cards[0];
        assert_eq!(c.type_name, "误入香闺");
        assert_eq!(c.category, "恋爱");
        assert_eq!(c.tags, vec!["闺房".to_string(), "幽会".to_string()]);
        assert_eq!(c.intensity, "low");
        assert_eq!(c.cooldown_turns, 3);
    }

    /// 事件包解析 G7：缺新字段不跳卡（默认 type_name/category/intensity 空、tags 空、cooldown_turns 0）。
    #[test]
    fn event_packages_parse_missing_new_fields_no_skip() {
        let v = json!([
            {
                "id": "pkg-legacy",
                "name": "旧包",
                "cards": [
                    {"id": "card-9", "title": "旧卡", "prompt": "旧数据无新字段"}
                ]
            }
        ]);
        let pkgs = parse_event_packages(&v);
        assert_eq!(pkgs.len(), 1, "缺新字段不得跳卡");
        let c = &pkgs[0].cards[0];
        assert_eq!(c.type_name, "");
        assert_eq!(c.category, "");
        assert!(c.tags.is_empty());
        assert_eq!(c.intensity, "");
        assert_eq!(c.cooldown_turns, 0);
        // 旧字段照常
        assert_eq!(c.id, "card-9");
        assert_eq!(c.title, "旧卡");
        assert!(c.enabled);
    }

    /// 演员状态解析：合法样例 → templates + initial_actors 结构正确，schema_version 归 1。
    #[test]
    fn actor_state_parse_valid() {
        let v = json!({
            "schemaVersion": 1,
            "initialActors": [
                {"characterId": "江景离", "templateId": "tpl-protagonist"},
                {"characterId": "沈棠", "templateId": "tpl-supporting"}
            ],
            "templates": {
                "tpl-protagonist": {
                    "fields": {
                        "修为": {"valueType": "number", "value": 10, "min": 0, "max": 100, "display": "修为", "updateInstruction": "修炼/战斗后变化"},
                        "好感": {"valueType": "number", "value": 50, "min": 0, "max": 100, "display": "好感"}
                    },
                    "traitPools": [{"id": "pool-1", "name": "灵根", "traits": [{"id": "t1", "name": "单灵根", "weight": 1}]}]
                },
                "tpl-supporting": {
                    "fields": {"伤势": {"valueType": "number", "value": 0, "min": 0, "max": 10, "display": "伤势"}},
                    "traitPools": []
                }
            }
        });
        let cfg = parse_actor_state_config(&v, None);
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.templates.len(), 2);
        assert_eq!(cfg.initial_actors.len(), 2);
        let tpl = &cfg.templates["tpl-protagonist"];
        assert_eq!(tpl.fields.len(), 2);
        assert_eq!(tpl.fields["修为"].value_type, "number");
        assert_eq!(tpl.fields["修为"].min, Some(0.0));
        assert_eq!(tpl.fields["修为"].max, Some(100.0));
        assert_eq!(tpl.trait_pools.len(), 1);
        assert_eq!(cfg.initial_actors[0].character_id, "江景离");
        assert_eq!(cfg.initial_actors[0].template_id, "tpl-protagonist");
        // to_system 能把配置物化为 ActorStateSystem
        let sys = cfg.to_system();
        assert!(sys.actors.contains_key("江景离"));
        assert!(sys.actors.contains_key("沈棠"));
    }

    /// 演员状态解析：非法模板/悬空引用被裁剪，解析失败返回默认空配置。
    #[test]
    fn actor_state_parse_prunes_invalid() {
        let v = json!({
            "initialActors": [
                {"characterId": "悬空", "templateId": "tpl-missing"},
                {"characterId": "好角色", "templateId": "tpl-ok"}
            ],
            "templates": {
                "tpl-missing": {"fields": {}, "traitPools": []},
                "tpl-ok": {"fields": {"声望": {"valueType": "number", "value": 5, "min": 0, "max": 10}}},
                "": {"fields": {"x": {"valueType": "number"}}}
            }
        });
        let cfg = parse_actor_state_config(&v, None);
        assert_eq!(cfg.templates.len(), 1);
        assert!(cfg.templates.contains_key("tpl-ok"));
        assert_eq!(cfg.initial_actors.len(), 1);
        assert_eq!(cfg.initial_actors[0].character_id, "好角色");
        // 白名单过滤：LLM 自创的「舍友」不在角色卡名单 → 丢弃该 actor + 孤立模板
        let whitelist: std::collections::HashSet<String> =
            ["江景离", "莫旺财"].iter().map(|s| s.to_string()).collect();
        let v2 = json!({
            "initialActors": [
                {"characterId": "莫旺财", "templateId": "tpl-mwc"},
                {"characterId": "舍友", "templateId": "tpl-sheyou"}
            ],
            "templates": {
                "tpl-mwc": {"fields": {"声望": {"valueType": "number", "value": 5, "min": 0, "max": 10}}},
                "tpl-sheyou": {"fields": {"地位": {"valueType": "string", "value": "舍友"}}}
            }
        });
        let cfg2 = parse_actor_state_config(&v2, Some(&whitelist));
        assert_eq!(cfg2.initial_actors.len(), 1);
        assert_eq!(cfg2.initial_actors[0].character_id, "莫旺财");
        assert!(cfg2.templates.contains_key("tpl-mwc"));
        assert!(!cfg2.templates.contains_key("tpl-sheyou"), "孤立模板应被清理");
        // 非对象 → 默认空配置
        let empty = parse_actor_state_config(&json!([1, 2, 3]), None);
        assert!(empty.templates.is_empty());
        assert!(empty.initial_actors.is_empty());
        // 完全非 JSON 结构 → 默认空配置
        let bad = parse_actor_state_config(&json!("not an object"), None);
        assert!(bad.templates.is_empty());
    }

    /// 文风归一化：字段齐整、缺省补空、非对象返回空对象。
    #[test]
    fn narrative_style_normalize() {
        let v = json!({
            "style": "第三人称全知",
            "tone": "热血",
            "pacing": "快节奏爽文",
            "scene_focus": "战斗场面",
            "prose_guidance": "多用短句，战斗段落镜头感强。"
        });
        let s = normalize_narrative_style(&v);
        assert_eq!(s["style"], "第三人称全知");
        assert_eq!(s["tone"], "热血");
        assert_eq!(s["prose_guidance"], "多用短句，战斗段落镜头感强。");
        // 缺省字段补空串
        let s2 = normalize_narrative_style(&json!({"style": "第一人称"}));
        assert_eq!(s2["tone"], "");
        // 非对象 → 空对象
        let s3 = normalize_narrative_style(&json!([1]));
        assert_eq!(s3.as_object().map(|o| o.len()), Some(0));
    }

    /// 规则检定解析：合法样例全部保留，能反序列化为 RuleSystem。
    #[test]
    fn rule_system_parse_valid() {
        let v = json!({
            "checks": [
                {"id": "breakthrough", "label": "修炼突破", "dice": "d100", "modifier": 0, "trigger": "尝试突破境界", "mustCheckExamples": ["冲击筑基"], "skipCheckExamples": ["日常吐纳"], "successHint": "突破成功", "failureHint": "突破失败，走火入魔"},
                {"id": "combat", "label": "战斗", "dice": "d20", "modifier": 2, "trigger": "与敌交手", "mustCheckExamples": ["与妖兽搏斗"], "failurePolicy": "block"},
                {"id": "persuade", "label": "交涉", "dice": "d20", "modifier": -1, "trigger": "游说他人", "mustCheckExamples": ["说服掌门"]},
                {"id": "stealth", "label": "潜行", "dice": "d20", "modifier": 0, "trigger": "潜入禁地", "mustCheckExamples": ["夜探藏经阁"]},
                {"id": "perception", "label": "察觉", "dice": "d20", "modifier": 1, "trigger": "观察四周", "mustCheckExamples": ["发现埋伏"]}
            ]
        });
        let r = normalize_rule_system(&v);
        let checks = r["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 5);
        let sys = RuleSystem::from_value(&r).expect("应能反序列化为 RuleSystem");
        assert_eq!(sys.checks.len(), 5);
        assert_eq!(sys.checks[0].id, "breakthrough");
        assert_eq!(sys.checks[0].dice, "d100");
        assert_eq!(sys.checks[1].modifier, 2.0);
    }

    /// 规则检定解析：id/label/dice 空的条被过滤；非 checks 结构返回空。
    #[test]
    fn rule_system_parse_skips_invalid() {
        let v = json!({
            "checks": [
                {"id": "", "label": "无 id", "dice": "d20"},
                {"id": "a", "label": "", "dice": "d20"},
                {"id": "b", "label": "无骰", "dice": ""},
                {"id": "ok", "label": "合格", "dice": "d20", "trigger": "触发"}
            ]
        });
        let r = normalize_rule_system(&v);
        let checks = r["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["id"], "ok");
        // 无 checks 键 → 空
        let r2 = normalize_rule_system(&json!({"foo": "bar"}));
        assert_eq!(r2["checks"].as_array().map(|a| a.len()), Some(0));
        // checks 为字符串 → 解析失败返回空
        let r3 = normalize_rule_system(&json!({"checks": "oops"}));
        assert_eq!(r3["checks"].as_array().map(|a| a.len()), Some(0));
    }

    // ─── B4: 网络错误不消耗 fffd 配额 ─────────────────────────────────────────

    /// 模拟桩：4 次 fffd + 2 次网络错误，应能救回（网络错误重试成功→出卡），而不是全耗完跳过。
    #[tokio::test]
    async fn retry_character_card_network_errors_separate_from_fffd() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let card_v = distill_one_character_card("庄眉", "high", {
            let calls = std::rc::Rc::clone(&calls);
            move || {
                let calls = std::rc::Rc::clone(&calls);
                async move {
                    let n = calls.get();
                    calls.set(n + 1);
                    match n {
                        0 => Err("error sending request for url ...9527".into()),
                        1 => Ok("{\"name\":\"庄眉\",\"personality\":\"对弗\u{FFFD}\u{FFFD}德的信任\"}".into()),
                        2 => Err("error sending request for url ...9527".into()),
                        3 => Ok("{\"name\":\"庄眉\",\"personality\":\"对弗\u{FFFD}德的信任\"}".into()),
                        4 => Ok("{\"name\":\"庄眉\",\"personality\":\"对弗雷德的信任\"}".into()),
                        _ => Ok("{\"name\":\"庄眉\",\"personality\":\"对弗雷德的信任\"}".into()),
                    }
                }
            }
        })
        .await;
        let card_v = card_v.expect("4次 fffd + 2次网络错误后应产出合法 JSON 卡");
        assert_eq!(card_v["name"], "庄眉");
        assert_eq!(card_v["personality"], "对弗雷德的信任");
        // 检查调用次数：2次网络错误 + 2次 fffd + 1次成功 = 5 次
        assert_eq!(calls.get(), 5, "应恰好在第 5 次调用时成功（2次网络+2次fffd+1次成功）");
    }

    // ─── B5: high 角色保底兜底 ─────────────────────────────────────────────────

    /// 模拟桩：high 角色 fffd 耗尽后降级保底应产出最小卡。
    #[tokio::test]
    async fn retry_character_card_high_fallback_yields_minimal_card() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let card_v = distill_one_character_card("主角", "high", {
            let calls = std::rc::Rc::clone(&calls);
            move || {
                let calls = std::rc::Rc::clone(&calls);
                async move {
                    let n = calls.get();
                    calls.set(n + 1);
                    // 前 6 轮全 fffd → fffd 配额耗尽
                    if n < 6 {
                        Ok("{\"name\":\"主角\",\"personality\":\"对弗\u{FFFD}的信任\"}".into())
                    } else if n == 6 {
                        // 降级保底第 1 轮
                        Ok("{\"name\":\"主角\",\"personality\":\"勇敢坚毅\"}".into())
                    } else {
                        Ok("{\"name\":\"主角\",\"personality\":\"勇敢坚毅\"}".into())
                    }
                }
            }
        })
        .await;
        let card_v = card_v.expect("high 角色降级保底应产出最小卡");
        assert_eq!(card_v["name"], "主角");
        // 至少调用了 7 次（6次 fffd + 1次保底）
        assert!(calls.get() >= 7, "应至少调用 7 次（6次 fffd + 1次保底）");
    }

    /// 模拟桩：非 high 角色 fffd 耗尽后不应触发保底。
    #[tokio::test]
    async fn retry_character_card_non_high_no_fallback() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let card_v = distill_one_character_card("路人", "low", {
            let calls = std::rc::Rc::clone(&calls);
            move || {
                let calls = std::rc::Rc::clone(&calls);
                async move {
                    calls.set(calls.get() + 1);
                    Ok("{\"name\":\"路人\",\"personality\":\"\u{FFFD}信任\"}".into())
                }
            }
        })
        .await;
        assert!(card_v.is_none(), "非 high 角色 fffd 耗尽后不应触发保底");
        assert_eq!(calls.get(), 4, "low 角色 fffd 配额 4 次，应恰好在 4 次后放弃");
    }

    // ─── B4: 网络错误独立配额测试 ─────────────────────────────────────────────

    /// 模拟桩：低角色连续网络错误 → conn_err 上限 2 快速放弃（83746baf 优化），
    /// fffd 配额(3)未动——验证网络错误独立于坏字配额。
    #[tokio::test]
    async fn retry_character_card_low_conn_errors_separate() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let card_v = distill_one_character_card("路人", "low", {
            let calls = std::rc::Rc::clone(&calls);
            move || {
                let calls = std::rc::Rc::clone(&calls);
                async move {
                    let n = calls.get();
                    calls.set(n + 1);
                    Err("网络超时".into())
                }
            }
        })
        .await;
        assert!(card_v.is_none(), "low 角色网络错误快速放弃，不产出卡");
        assert_eq!(calls.get(), 2, "low 角色 2 次网络错误后放弃（conn_err 上限 2）");
    }

    // ─── D1: high 角色证据空时词频兜底 ──────────────────────────────────────
    #[test]
    fn fallback_evidence_scan_recalls_high_frequency_character() {
        let chapters = vec![
            ("ch01".to_string(), "庄眉走进派出所带走向明初，作为母亲她异常冷静。".to_string()),
            ("ch02".to_string(), "向明初在宿舍发呆，完全没有庄眉的消息。".to_string()),
            ("ch03".to_string(), "路边卖糖水的老王吆喝着生意。".to_string()),
        ];
        // 庄眉：别名含"眉姐"
        let ev = fallback_evidence_scan(&chapters, "庄眉", &["眉姐".to_string()], 8);
        assert!(!ev.is_empty(), "高频角色应能按名字召回证据");
        assert!(ev.contains("庄眉"), "兜底证据应包含角色名原文");
        assert!(ev.contains("ch01"), "应标注来源章节");
        assert!(ev.contains("ch02"), "应跨章召回(另一方无助时仍提及庄眉)");
    }

    #[test]
    fn fallback_evidence_scan_skips_absent_character() {
        let chapters = vec![
            ("ch01".to_string(), "老王在街角卖糖水，招呼着过路的学子。".to_string()),
            ("ch02".to_string(), "宿舍阿姨锁好门,叮嘱大家早点休息。".to_string()),
        ];
        // 全本不出现的角色 → 应返回空(继续走跳过分支)
        let ev = fallback_evidence_scan(&chapters, "庄眉", &[], 8);
        assert!(ev.is_empty(), "原文未出现的角色兜底也应为空");
    }

    #[test]
    fn st26_late_hotel_detected() {
        // 度蜜月实测违规样本：蜜月套房 → 触发
        assert!(opening_scene_violates_st26(
            "清晨，蜜月套房内，木质地板微凉，海浪声从推拉门方向渗入。"
        ));
    }

    #[test]
    fn st26_spoiler_suffix_detected() {
        // 剧透后缀「另：」 → 触发
        assert!(opening_scene_violates_st26(
            "海边度假房间，海风灌入，纱帘鼓动。另：深夜接沈雨棠电话时"
        ));
    }

    #[test]
    fn st26_daily_scene_allowed() {
        // 学校/家中日常场景 → 放行
        assert!(!opening_scene_violates_st26(
            "学校走廊，午后阳光穿过樟树枝叶，学生涌出教室。林婉清背着包往外走。"
        ));
    }

    #[test]
    fn st26_empty_allowed() {
        assert!(!opening_scene_violates_st26("证据不足，无实际出场场景"));
    }
}