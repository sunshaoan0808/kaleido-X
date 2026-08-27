//! Story Tavern memory extraction + node advancement (ST-2).
//! Called after turn completion — post-LLM phase 2: a small LLM call for
//! L1 summary, L4 affinity updates, and engine-tag-based node transition.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use crate::{
    memory_weaver::{build_rp_summary_user_text, serialize_for_summary, RP_SUMMARY_SYSTEM_PROMPT}, EngineTag, MemoryL2Event, StoryPack,
    TavernSession,
};

/// Result of a memory + node advance extraction run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnExtraction {
    #[serde(default)]
    pub new_scene_summary: String,
    #[serde(default)]
    pub new_affinity: Value,
    #[serde(default)]
    pub engine_tag: Option<EngineTag>,
    /// If engine_tag == Advance or canon completes node, jump to this node
    #[serde(default)]
    pub advance_to_node_id: Option<String>,
    #[serde(default)]
    pub advance_to_chapter_id: Option<String>,
    #[serde(default)]
    pub options_for_next: Vec<String>,
    /// L2 events this turn
    #[serde(default)]
    pub new_events: Vec<crate::MemoryL2Event>,
    /// L3 edges [{from,to,rel,note}]
    #[serde(default)]
    pub new_edges: Vec<Value>,
    /// L3 facts
    #[serde(default)]
    pub new_facts: Vec<String>,
}

impl Default for TurnExtraction {
    fn default() -> Self {
        Self {
            new_scene_summary: String::new(),
            new_affinity: json!({}),
            engine_tag: Some(EngineTag::Idle),
            advance_to_node_id: None,
            advance_to_chapter_id: None,
            options_for_next: vec![],
            new_events: vec![],
            new_edges: vec![],
            new_facts: vec![],
        }
    }
}

/// Determine engine tag from user message content heuristics.
/// P3 MVP: simple keyword-based; can be upgraded to LLM classification.
pub fn classify_engine_tag(user_message: &str) -> EngineTag {
    let lower = user_message.to_ascii_lowercase();
    // Check specific patterns first that contain substrings of general ones
    if lower.contains("剧情推进")
        || lower.contains("[剧情推进]")
        || lower.contains("跳转")
    {
        EngineTag::Canon
    } else if lower.contains("继续")
        || lower.contains("推进")
        || lower.contains("下一步")
        || lower.contains("然后呢")
        || lower.contains("go on")
        || lower.contains("next")
    {
        EngineTag::Advance
    } else {
        EngineTag::Idle
    }
}

/// Try to advance node based on engine tag and node exits.
/// Returns (next_node_id, next_chapter_id) if advancement occurs.
pub fn try_advance_node(
    pack: &StoryPack,
    session: &mut TavernSession,
    engine_tag: EngineTag,
    extraction: &mut TurnExtraction,
    llm_suggested_node: Option<String>,
) {
    // Free/Side: do not advance mainline node/chapter cursor
    if session.play_mode.freezes_mainline_cursor() {
        return;
    }
    if engine_tag == EngineTag::Idle && llm_suggested_node.is_none() {
        return;
    }

    let current_node_id = match &session.node_id {
        Some(nid) => nid.clone(),
        None => return,
    };

    let current_node = match pack.nodes.iter().find(|n| n.id == current_node_id) {
        Some(n) => n,
        None => return,
    };

    // ST-15: LLM-suggested node advance — take this specific exit
    if let Some(suggested) = llm_suggested_node {
        // Validate suggested node exists in pack
        if pack.nodes.iter().any(|n| n.id == suggested) {
            // Check it appears in current node's exits, or just trust LLM
            extraction.advance_to_node_id = Some(suggested.clone());
            extraction.advance_to_chapter_id = session.chapter_cursor.clone();
            session.node_id = Some(suggested.clone());

            // Check chapter transition
            if let Some(next_node) = pack.nodes.iter().find(|n| n.id == suggested) {
                if next_node.chapter_id != current_node.chapter_id {
                    extraction.advance_to_chapter_id = Some(next_node.chapter_id.clone());
                    session.chapter_cursor = Some(next_node.chapter_id.clone());
                }
            }
            return;
        }
    }

    if current_node.exit.is_empty() {
        // Node has no exits — try next node in chapter
        if let Some(ch_id) = &session.chapter_cursor {
            if let Some(ch) = pack.chapters.iter().find(|c| c.id == *ch_id) {
                let pos = ch.node_ids.iter().position(|nid| *nid == current_node_id);
                if let Some(idx) = pos {
                    if idx + 1 < ch.node_ids.len() {
                        let next = ch.node_ids[idx + 1].clone();
                        extraction.advance_to_node_id = Some(next.clone());
                        extraction.advance_to_chapter_id = Some(ch_id.clone());
                        session.node_id = Some(next);
                    } else {
                        // No more nodes in this chapter — try next chapter
                        try_advance_chapter(pack, session, extraction);
                    }
                }
            }
        }
        return;
    }

    // Exits exist. For canon tag: take first hard exit.
    // For advance tag: also take first exit (default flow).
    if engine_tag == EngineTag::Canon || engine_tag == EngineTag::Advance {
        let first_exit = &current_node.exit[0];
        extraction.advance_to_node_id = Some(first_exit.next.clone());
        extraction.advance_to_chapter_id = session.chapter_cursor.clone();
        session.node_id = Some(first_exit.next.clone());

        // Check if next node is in a different chapter
        if let Some(next_node) = pack.nodes.iter().find(|n| n.id == first_exit.next) {
            if next_node.chapter_id != current_node.chapter_id {
                extraction.advance_to_chapter_id = Some(next_node.chapter_id.clone());
                session.chapter_cursor = Some(next_node.chapter_id.clone());
            }
        }
    }
}

fn try_advance_chapter(
    pack: &StoryPack,
    session: &mut TavernSession,
    extraction: &mut TurnExtraction,
) {
    let current_ch_id = match &session.chapter_cursor {
        Some(id) => id.clone(),
        None => return,
    };
    let mut chapters: Vec<_> = pack.chapters.iter().collect();
    chapters.sort_by_key(|c| c.order);
    let pos = chapters.iter().position(|c| c.id == current_ch_id);
    if let Some(idx) = pos {
        if idx + 1 < chapters.len() {
            let next_ch = chapters[idx + 1];
            if let Some(first_node) = next_ch.node_ids.first() {
                extraction.advance_to_node_id = Some(first_node.clone());
                extraction.advance_to_chapter_id = Some(next_ch.id.clone());
                session.chapter_cursor = Some(next_ch.id.clone());
                session.node_id = Some(first_node.clone());
            }
        }
    }
}

/// Build the extraction prompt for the post-turn LLM call.
pub fn build_extraction_prompt(
    pack: &StoryPack,
    session: &TavernSession,
    last_user_msg: &str,
    last_assistant_msg: &str,
) -> String {
    format!(
        r#"你是一个故事记忆助手。基于以下对话，提取信息。

剧本：{title}
当前章节：{ch}
当前节点：{node}

最近用户消息：
{last_user}

最近旁白回复：
{last_assistant}

请输出一个 JSON 对象（仅 JSON，无 markdown 包裹）：
{{
  "sceneSummary": "前情提要式场景摘要：按时间顺序概述关键事件（谁做了什么、结果如何），保留剧内时间刻度（如「第一天黄昏」）；必须以**最新**场景为准——写成更早的场景会导致剧情倒退",
  "affinity": {{"角色ID": "好感变化描述"}},
  "events": [{{"kind":"meet|conflict|promise|secret|other","summary":"事件一句话","actors":["角色ID"]}}],
  "edges": [{{"from":"角色A","to":"角色B","rel":"关系词","note":"说明"}}],
  "facts": ["可复用的细粒度事实"],
  "optionsForNext": ["后续走向摘要"]
}}

注意：
- affinity 键为角色 ID，值如 "+5" / "-3"
- events 为 L2 短程事件（最多 3 条）；promise(承诺)/secret(伏笔) **宁多勿漏**——漏掉一条，后续剧情就永远丢失它
- edges/facts 为 L3 细关系/事实（可空）
"#,
        title = pack.title,
        ch = session.chapter_cursor.as_deref().unwrap_or("?"),
        node = session.node_id.as_deref().unwrap_or("?"),
        last_user = last_user_msg.chars().take(200).collect::<String>(),
        last_assistant = last_assistant_msg.chars().take(500).collect::<String>(),
    )
}

/// Parse the extraction LLM response into a TurnExtraction.
pub fn parse_extraction_response(response: &str) -> TurnExtraction {
    // [morphling Wave B2 2026-08-16] 清洗升级（吸收自 SillyTavern-BakemonoMemory query-parser）：
    // 1) 剥 think/analysis/reasoning 块与代码围栏；
    // 2) 从「复述任务+JSON 混排」文本中抠出第一个完整 JSON 对象（日志实证：提炼 LLM
    //    常输出"我们根据用户输入，需要输出JSON…"开头，原实现 from_str 直接失败 → 全丢）。
    let stripped = crate::bakemono_query_parse::strip_reasoning_blocks(response);
    let json_slice = if let Some(start) = stripped.find('{') {
        stripped.rfind('}').map(|end| {
            if end > start {
                &stripped[start..=end]
            } else {
                &stripped
            }
        })
    } else {
        None
    };
    let cleaned = json_slice
        .unwrap_or(stripped.as_str())
        .trim();

    let v: Value = serde_json::from_str(cleaned).unwrap_or_default();
    let mut ext = TurnExtraction::default();

    if let Some(s) = v.get("sceneSummary").and_then(|x| x.as_str()) {
        ext.new_scene_summary = s.to_string();
    }
    if let Some(a) = v.get("affinity") {
        ext.new_affinity = a.clone();
    }
    if let Some(opts) = v.get("optionsForNext").and_then(|x| x.as_array()) {
        for o in opts {
            if let Some(s) = o.as_str() {
                ext.options_for_next.push(s.to_string());
            }
        }
    }
    if let Some(evs) = v.get("events").and_then(|x| x.as_array()) {
        for (i, e) in evs.iter().take(3).enumerate() {
            let mut ev = crate::MemoryL2Event::default();
            ev.id = format!("e-{}", i);
            ev.kind = e.get("kind").and_then(|x| x.as_str()).unwrap_or("other").to_string();
            ev.summary = e.get("summary").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if let Some(actors) = e.get("actors").and_then(|x| x.as_array()) {
                for a in actors {
                    if let Some(s) = a.as_str() {
                        ev.actors.push(s.to_string());
                    }
                }
            }
            if !ev.summary.is_empty() {
                ext.new_events.push(ev);
            }
        }
    }
    if let Some(edges) = v.get("edges").and_then(|x| x.as_array()) {
        for e in edges.iter().take(5) {
            ext.new_edges.push(e.clone());
        }
    }
    if let Some(facts) = v.get("facts").and_then(|x| x.as_array()) {
        for f in facts.iter().take(5) {
            if let Some(s) = f.as_str() {
                if !s.is_empty() {
                    ext.new_facts.push(s.to_string());
                }
            }
        }
    }
    ext
}

/// Apply extraction result to session state.
pub fn apply_extraction(
    session: &mut TavernSession,
    extraction: &TurnExtraction,
    _engine_tag: EngineTag,
) {
    // L1: update scene summary (every 5-10 turns, or if non-empty)
    if !extraction.new_scene_summary.is_empty() && (session.turn % 8 == 0 || session.turn <= 3) {
        session.memory_l1.scene_summary = extraction.new_scene_summary.clone();
        session.memory_l1.updated_at_turn = session.turn;
    }

    // L4: merge affinity
    if let Some(aff_obj) = extraction.new_affinity.as_object() {
        if !aff_obj.is_empty() {
            if !session.memory_l4.affinity.is_object() {
                session.memory_l4.affinity = json!({});
            }
            if let Some(cur_obj) = session.memory_l4.affinity.as_object_mut() {
                for (char_id, change) in aff_obj {
                    if let Some(change_str) = change.as_str() {
                        let delta = parse_affinity_delta(change_str);
                        if delta != 0 {
                            let current_val = cur_obj
                                .get(char_id)
                                .and_then(|v| v.as_i64())
                                .unwrap_or(50);
                            let new_val = (current_val + delta).clamp(0, 100);
                            cur_obj.insert(char_id.clone(), json!(new_val));
                        }
                    }
                }
            }
        }
    }

    // L2 events (cap 24)
    if !extraction.new_events.is_empty() {
        for mut ev in extraction.new_events.clone() {
            if ev.id.is_empty() {
                ev.id = format!("e-{}-{}", session.turn, session.memory_l2.events.len());
            }
            ev.turn = session.turn;
            if ev.node_id.is_none() {
                ev.node_id = session.node_id.clone();
            }
            session.memory_l2.events.push(ev);
        }
        const L2_CAP: usize = 24;
        if session.memory_l2.events.len() > L2_CAP {
            let drop_n = session.memory_l2.events.len() - L2_CAP;
            session.memory_l2.events.drain(0..drop_n);
        }
        session.memory_l2.updated_at_turn = session.turn;
    }

    // L3 edges/facts (cap 40 edges, 40 facts)
    if !extraction.new_edges.is_empty() {
        session.memory_l3.edges.extend(extraction.new_edges.clone());
        const E_CAP: usize = 40;
        if session.memory_l3.edges.len() > E_CAP {
            let drop_n = session.memory_l3.edges.len() - E_CAP;
            session.memory_l3.edges.drain(0..drop_n);
        }
        session.memory_l3.updated_at_turn = session.turn;
    }
    if !extraction.new_facts.is_empty() {
        for f in &extraction.new_facts {
            if !session.memory_l3.facts.iter().any(|x| x == f) {
                session.memory_l3.facts.push(f.clone());
            }
        }
        const F_CAP: usize = 40;
        if session.memory_l3.facts.len() > F_CAP {
            let drop_n = session.memory_l3.facts.len() - F_CAP;
            session.memory_l3.facts.drain(0..drop_n);
        }
        session.memory_l3.updated_at_turn = session.turn;
    }

    // Engine tag stored (node advance done separately in start_turn flow)
}

/// Heuristic L2/L3 fill when no LLM extraction JSON is available (ST-12 fallback).
pub fn heuristic_l2_l3_from_turn(
    session: &TavernSession,
    last_user_msg: &str,
    last_assistant_msg: &str,
) -> TurnExtraction {
    let mut ext = TurnExtraction::default();
    let user = last_user_msg.trim();
    let asst = last_assistant_msg.trim();
    if user.is_empty() && asst.is_empty() {
        return ext;
    }
    let kind = if user.contains("答应") || user.contains("保证") || asst.contains("答应") {
        "promise"
    } else if user.contains("秘密") || asst.contains("秘密") || asst.contains("不告诉") {
        "secret"
    } else if user.contains("打架") || user.contains("殴打") || user.contains("争吵")
        || user.contains("冲突") || asst.contains("怒") {
        "conflict"
    } else if session.turn <= 1 {
        "meet"
    } else {
        "other"
    };
    // [ST-35 2026-08-16] 同行/约定识别：正文里「一起去/一块儿去/说好/约好/带你去/陪你」
    // 等约定性表述属于已确立的剧情事实，必须记录进 L2 事件与 L3 facts，跨回合可见。
    // 修复「窝边草」根因：turn13「跟我一块儿去」只存在于正文中段（heuristic 只截前60字），
    // 未被提取 → turn14 模型忘掉约定、改写成独自前往。这里从全文扫描约定句。
    let agreement_kw: [&str; 8] = ["一起去", "一块儿去", "说好", "约好", "带你去", "陪你", "一块过去", "一起过去"];
    let mut agreement_sentences: Vec<String> = Vec::new();
    if !asst.is_empty() {
        // 按句子切分（。！？…；）
        for part in asst.split(['。', '！', '？', '…', '；', '!', '?', '.']) {
            let p = part.trim();
            if p.is_empty() { continue; }
            if agreement_kw.iter().any(|kw| p.contains(kw)) && p.chars().count() <= 60 {
                agreement_sentences.push(p.to_string());
                if agreement_sentences.len() >= 2 { break; }
            }
        }
    }
    let mut summary = String::new();
    if !user.is_empty() {
        summary.push_str(&format!("玩家：{}", user.chars().take(40).collect::<String>()));
    }
    if !asst.is_empty() {
        if !summary.is_empty() { summary.push('；'); }
        summary.push_str(&format!("叙事：{}", asst.chars().take(60).collect::<String>()));
    }
    if !agreement_sentences.is_empty() {
        if !summary.is_empty() { summary.push('；'); }
        summary.push_str(&format!("约定：{}", agreement_sentences.join(" / ")));
        ext.new_facts.push(format!("turn{}:约定——{}", session.turn, agreement_sentences.join(" / ")));
    }
    let mut actors = session.present_character_ids.clone();
    if let Some(f) = &session.focus_character_id {
        if !actors.contains(f) {
            actors.push(f.clone());
        }
    }
    actors.truncate(3);
    let mut ev = crate::MemoryL2Event::default();
    ev.id = format!("e-h-{}", session.turn);
    ev.kind = kind.into();
    ev.summary = summary;
    ev.actors = actors.clone();
    ev.node_id = session.node_id.clone();
    ext.new_events.push(ev);

    // R 层 (2026-08-19): 关系类型启发式分类——intimate/hostile 检测提前，
    // rel 从「conflict→tension 否则 interact」扩展为 4 类（romance/敌对/tension/interact）。
    // 使每回合启发式也能记录关系演变（与 LLM 开放枚举互补，LLM 每 3 回合细粒度补全）。
    // （L4 affinity 兜底逻辑随下方使用同一 intimate/hostile 判定。）
    let intimate = (user.contains("吻") || user.contains("亲嘴") || user.contains("亲吻")
        || user.contains("亲热") || user.contains("接吻")
        || user.contains("抱住") || user.contains("拥抱") || user.contains("抱紧")
        || user.contains("抱着"))
        || (asst.contains("吻") || asst.contains("亲嘴") || asst.contains("亲吻")
        || asst.contains("亲热") || asst.contains("接吻")
        || asst.contains("抱住") || asst.contains("拥抱") || asst.contains("抱紧")
        || asst.contains("抱着"));
    let hostile = (user.contains("打架") || user.contains("殴打") || user.contains("骂")
        || user.contains("怒") || user.contains("威胁") || user.contains("骗")
        || user.contains("背叛") || user.contains("羞辱") || user.contains("伤害")
        || user.contains("杀了") || user.contains("杀人") || user.contains("杀死")
        || user.contains("讨厌") || user.contains("滚开") || user.contains("滚出去")
        || user.contains("滚蛋"))
        || (asst.contains("打架") || asst.contains("殴打") || asst.contains("骂")
        || asst.contains("怒") || asst.contains("威胁") || asst.contains("骗")
        || asst.contains("背叛") || asst.contains("羞辱") || asst.contains("伤害")
        || asst.contains("杀了") || asst.contains("杀人") || asst.contains("杀死")
        || asst.contains("讨厌") || asst.contains("滚开") || asst.contains("滚出去")
        || asst.contains("滚蛋"));
    if let Some(focus) = &session.focus_character_id {
        let rel = if intimate {
            "romance"
        } else if hostile {
            "敌对"
        } else if kind == "conflict" {
            "tension"
        } else {
            "interact"
        };
        ext.new_edges.push(json!({
            "from": "player",
            "to": focus,
            "rel": rel,
            "note": user.chars().take(30).collect::<String>(),
            "turn": session.turn,
        }));
    }
    // L4 affinity 兜底（heuristic 对称化 S9.24）：
    // - 亲密（吻/亲嘴/亲吻/亲热/接吻/拥抱类）→ 焦点角色 +6（ST-FIX: 不用裸「亲」/裸「抱」，
    //   避免「母亲」误命中；拥抱用「抱住/拥抱/抱紧/抱着」而非裸「抱」，避免「抱歉/抱负」误命中）
    // - 冲突/冒犯/欺骗/威胁 → 焦点角色 -6（与主路径 LLM 提取的涨跌对称）
    if let Some(focus) = &session.focus_character_id {
        if intimate {
            ext.new_affinity = json!({focus.as_str(): "+6"});
        } else if hostile {
            ext.new_affinity = json!({focus.as_str(): "-6"});
        }
    }
    if kind == "promise" {
        ext.new_facts.push(format!("turn{}:出现承诺相关互动", session.turn));
    } else if kind == "secret" {
        ext.new_facts.push(format!("turn{}:出现秘密相关信息", session.turn));
    }
    // light L1 also
    if !asst.is_empty() {
        ext.new_scene_summary = asst.chars().take(80).collect();
    }
    ext
}

fn parse_affinity_delta(s: &str) -> i64 {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('+') {
        rest.parse::<i64>().unwrap_or(0)
    } else if let Some(rest) = s.strip_prefix('-') {
        -rest.parse::<i64>().unwrap_or(0)
    } else {
        s.parse::<i64>().unwrap_or(0)
    }
}

/// Full extraction + node advance flow (called after turn LLM finishes).
pub fn post_turn_extraction(
    pack: &StoryPack,
    session: &mut TavernSession,
    last_user_msg: &str,
    _last_assistant_msg: &str,
    extraction: &TurnExtraction,
) {
    let engine_tag = classify_engine_tag(last_user_msg);

    // Apply memory updates
    apply_extraction(session, extraction, engine_tag);

    // Node advancement
    let mut ext_clone = extraction.clone();
    try_advance_node(pack, session, engine_tag, &mut ext_clone, None);
}

/// Memory compression: trim caches and update summary.
/// Called every 30+ turns to prevent prompt bloat.
pub fn apply_memory_compression(
    session: &mut TavernSession,
    compressed_summary: &str,
) {
    // Update L1 scene summary with compressed view
    session.memory_l1.scene_summary = format!(
        "{}（第{}回合压缩归档）",
        compressed_summary,
        session.turn,
    );
    session.memory_l1.updated_at_turn = session.turn;

    // L2: keep top 12 events (from current 24)
    if session.memory_l2.events.len() > 12 {
        let drop = session.memory_l2.events.len() - 12;
        session.memory_l2.events.drain(0..drop);
    }

    // L3: keep top 20 edges
    if session.memory_l3.edges.len() > 20 {
        let drop = session.memory_l3.edges.len() - 20;
        session.memory_l3.edges.drain(0..drop);
    }

    // L3: keep top 20 facts
    if session.memory_l3.facts.len() > 20 {
        let drop = session.memory_l3.facts.len() - 20;
        session.memory_l3.facts.drain(0..drop);
    }
}

/// [morphling Wave B3 2026-08-16] 章节剧情摘要提炼 prompt 构建
/// （吸收自 SillyTavern-BakemonoMemory summary-memory-model：章节级长期记忆账本）。
pub fn build_chapter_diary_prompt(session: &TavernSession, chapter_title: &str) -> (String, String) {
    let events: Vec<String> = session
        .memory_l2
        .events
        .iter()
        .map(|e| format!("[t{}][{}] {}", e.turn, e.kind, e.summary))
        .collect();
    let system = "你是故事章节摘要提炼器。把一章的剧情事件压缩成一段章节总结（200-400字）：\
交代本章发生了什么、关键转折、角色状态变化。只输出总结文本本身，不要JSON、不要标题、\
不要「以下是总结」之类的开场白、不要复述任务。";
    let user = format!(
        "章节：{}\n本章事件：\n{}\n既有摘要（如有，需融入更新，不要丢失已有关键信息）：\n{}",
        chapter_title,
        if events.is_empty() {
            "（暂无事件记录）".to_string()
        } else {
            events.join("\n")
        },
        // [V2 2026-08-17] 过滤压缩占位（「第N回合记忆压缩/压缩归档」空洞文本），
        // 避免章节摘要吸收 epoch 压缩写入 L1 的占位噪声。
        crate::chapter_diary::strip_compression_placeholder(&session.memory_l1.scene_summary),
    );
    (system.to_string(), user)
}

/// 解析章节摘要响应：剥思考块/围栏后取纯文本，截断 800 字。
/// [morphling C7 2026-08-16] 过滤提炼 prompt 复述泄漏（LLM 偶发把指令原文
/// 复述进输出末尾——如「我们只需要输出总结文本本身」「字数控制在 200-400」）。
pub fn parse_chapter_diary_response(raw: &str) -> String {
    let cleaned = crate::bakemono_query_parse::strip_reasoning_blocks(raw);
    // 指令复述特征行：命中即截断（其后都是 prompt 回声，非总结内容）
    const INSTRUCTION_MARKERS: [&str; 12] = [
        "我们只需要输出",
        "只需要输出总结",
        "需要根据章节事件",
        "字数控制在",
        "注意：只输出",
        "不要JSON",
        "不要标题",
        "内容要涵盖",
        "需要压缩成",
        "注意角色状态",
        "只输出总结文本",
        "你是一个故事章节摘要",
    ];
    let mut kept = Vec::new();
    for line in cleaned.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if INSTRUCTION_MARKERS.iter().any(|m| l.contains(m)) {
            break;
        }
        kept.push(l);
    }
    let text = kept.join("\n");
    let text = text.trim().trim_matches('"').trim();
    text.chars().take(800).collect()
}

/// Build LLM prompt for memory compression.
/// RP 场记结构（吸收自 Liyuan compaction.ts）：叙事正文（serialize 过滤）+ 账本快照
/// + 既有摘要增量合并，替代原来的 120 字短摘要挤压。
pub fn build_compression_prompt(
    session: &TavernSession,
    pack: &StoryPack,
) -> (String, String) {
    use std::fmt::Write;

    // System prompt: 场记式接力摘要
    let sys = RP_SUMMARY_SYSTEM_PROMPT.to_string();

    // User prompt: 剧本上下文 + 叙事正文（serialize 过滤，限量防调用过大）
    let mut user = String::new();
    writeln!(user, "剧本：{}", pack.title).ok();
    writeln!(user, "当前节点：{}", session.node_id.as_deref().unwrap_or("?")).ok();
    writeln!(user, "当前回合：{}", session.turn).ok();

    let narrative = serialize_for_summary(&session.messages, "玩家", "旁白");
    let bounded = crate::progressive_compress::compress_text(&narrative, 12000);
    if !bounded.trim().is_empty() {
        writeln!(user, "\n<conversation>\n{}\n</conversation>", bounded).ok();
    }

    // 账本快照（L2/L3 聚合，辅助参考；previous-summary 由既有 scene_summary 担任）
    let mut snap = String::new();
    if !session.memory_l2.events.is_empty() {
        writeln!(snap, "事件：").ok();
        for ev in session.memory_l2.events.iter().rev().take(10) {
            writeln!(snap, "• {}（{}）: {}", ev.id, ev.kind, ev.summary).ok();
        }
    }
    if !session.memory_l3.edges.is_empty() {
        writeln!(snap, "关系：").ok();
        for edge in session.memory_l3.edges.iter().rev().take(8) {
            let from = edge.get("from").and_then(|v| v.as_str()).unwrap_or("?");
            let to = edge.get("to").and_then(|v| v.as_str()).unwrap_or("?");
            let rel = edge.get("rel").and_then(|v| v.as_str()).unwrap_or("?");
            writeln!(snap, "• {} → {} ({})", from, to, rel).ok();
        }
    }
    if !session.memory_l3.facts.is_empty() {
        writeln!(snap, "事实：").ok();
        for f in session.memory_l3.facts.iter().rev().take(8) {
            writeln!(snap, "• {}", f).ok();
        }
    }

    let previous = if session.memory_l1.scene_summary.trim().is_empty() {
        None
    } else {
        Some(session.memory_l1.scene_summary.as_str())
    };
    let user_text = build_rp_summary_user_text(&bounded, &snap, previous);
    user.push_str(&user_text);

    (sys, user)
}

// ---------- Cross-Session Memory (ST-19) ----------

/// A single memory entry persisted across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossSessionEntry {
    pub id: String,
    pub pack_id: String,
    pub session_id: String,
    pub character_ids: Vec<String>,
    pub node_id: Option<String>,
    pub turn: u32,
    pub kind: String,
    pub summary: String,
    pub actors: Vec<String>,
    pub created_at: String,
}

/// Index of cross-session memories, stored as a single JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossSessionStore {
    pub entries: Vec<CrossSessionEntry>,
}

impl CrossSessionStore {
    pub fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("memory.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("cross-session memory.json parse error: {e}, starting fresh");
                Self { entries: Vec::new() }
            }),
            Err(_) => Self { entries: Vec::new() },
        }
    }

    pub fn save(&self, dir: &std::path::Path) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("cross-session mkdir: {e}");
            return;
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join("memory.json"), text);
        }
    }

    pub fn add_entry(&mut self, entry: CrossSessionEntry) {
        self.entries.push(entry);
        // Cap at 500 entries
        if self.entries.len() > 500 {
            self.entries.drain(0..self.entries.len() - 500);
        }
    }

    /// Query entries relevant to a pack and set of characters.
    pub fn query(&self, pack_id: &str, character_ids: &[&str], limit: usize) -> Vec<&CrossSessionEntry> {
        let mut scored: Vec<(i32, &CrossSessionEntry)> = self
            .entries
            .iter()
            .filter(|e| e.pack_id == pack_id)
            .map(|e| {
                // Score: actor overlap + recency
                let mut score = 0i32;
                for actor in &e.actors {
                    if character_ids.contains(&actor.as_str()) {
                        score += 3;
                    }
                }
                for cid in &e.character_ids {
                    if character_ids.contains(&cid.as_str()) {
                        score += 2;
                    }
                }
                (score, e)
            })
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

/// Persist a turn's extraction results to cross-session memory.
pub fn persist_cross_session(
    cross_dir: &std::path::Path,
    pack_id: &str,
    session_id: &str,
    turn: u32,
    node_id: Option<&str>,
    character_ids: &[String],
    events: &[MemoryL2Event],
    facts: &[String],
    edges: &[Value],
) {
    let mut store = CrossSessionStore::load(cross_dir);
    let now = chrono::Utc::now().to_rfc3339();

    for ev in events {
        let entry = CrossSessionEntry {
            id: ev.id.clone(),
            pack_id: pack_id.to_string(),
            session_id: session_id.to_string(),
            character_ids: character_ids.to_vec(),
            node_id: node_id.map(|s| s.to_string()).or_else(|| ev.node_id.clone()),
            turn,
            kind: format!("event:{}", ev.kind),
            summary: ev.summary.clone(),
            actors: ev.actors.clone(),
            created_at: now.clone(),
        };
        store.add_entry(entry);
    }
    for (fi, f) in facts.iter().enumerate() {
        let entry = CrossSessionEntry {
            id: format!("fact-{}-{}-{}", session_id, turn, fi),
            pack_id: pack_id.to_string(),
            session_id: session_id.to_string(),
            character_ids: character_ids.to_vec(),
            node_id: node_id.map(|s| s.to_string()),
            turn,
            kind: "fact".into(),
            summary: f.clone(),
            actors: character_ids.to_vec(),
            created_at: now.clone(),
        };
        store.add_entry(entry);
    }
    // edges as JSON strings
    for (ei, edge) in edges.iter().enumerate() {
        let summary = edge.get("from").and_then(|v| v.as_str()).unwrap_or("?")
            .to_string() + " → " + edge.get("to").and_then(|v| v.as_str()).unwrap_or("?")
            + " (" + edge.get("rel").and_then(|v| v.as_str()).unwrap_or("?") + ")";
        let entry = CrossSessionEntry {
            id: format!("edge-{}-{}-{}", session_id, turn, ei),
            pack_id: pack_id.to_string(),
            session_id: session_id.to_string(),
            character_ids: character_ids.to_vec(),
            node_id: node_id.map(|s| s.to_string()),
            turn,
            kind: "edge".into(),
            summary,
            actors: character_ids.to_vec(),
            created_at: now.clone(),
        };
        store.add_entry(entry);
    }

    store.save(cross_dir);
}

/// Build a cross-session memory context string for the system prompt.
pub fn build_cross_session_context(
    cross_dir: &std::path::Path,
    pack_id: &str,
    character_ids: &[&str],
    max_entries: usize,
) -> String {
    let store = CrossSessionStore::load(cross_dir);
    let results = store.query(pack_id, character_ids, max_entries);
    if results.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    lines.push("## 过往经历（跨会话记忆）".into());
    for e in results {
        lines.push(format!("  • [{}] {} — {}", e.turn, e.kind, e.summary));
    }
    lines.join("\n")
}

/// Tokenize Chinese+English text for similarity search.
/// CJK characters → unigrams + bigrams; ASCII → lowercase words.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphanumeric() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '-') {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect::<String>().to_lowercase());
        } else if is_cjk(chars[i]) {
            tokens.push(chars[i].to_string());
            if i + 1 < chars.len() && is_cjk(chars[i + 1]) {
                tokens.push(format!("{}{}", chars[i], chars[i + 1]));
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    // Deduplicate preserving first occurrence
    let mut seen = HashSet::new();
    tokens.retain(|t| seen.insert(t.clone()));
    tokens
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{2F800}'..='\u{2FA1F}'
    )
}

/// Cosine similarity between a query token set and a target text's TF vector.
pub fn cosine_similarity(query_tokens: &[String], target: &str) -> f32 {
    let target_tokens = tokenize(target);
    let mut q_tf: HashMap<&str, f32> = HashMap::new();
    let mut t_tf: HashMap<&str, f32> = HashMap::new();
    for t in query_tokens {
        *q_tf.entry(t.as_str()).or_insert(0.0) += 1.0;
    }
    for t in &target_tokens {
        *t_tf.entry(t.as_str()).or_insert(0.0) += 1.0;
    }
    let q_norm: f32 = q_tf.values().map(|v| v * v).sum::<f32>().sqrt();
    let t_norm: f32 = t_tf.values().map(|v| v * v).sum::<f32>().sqrt();
    if q_norm == 0.0 || t_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = q_tf
        .iter()
        .map(|(k, v)| v * t_tf.get(k).copied().unwrap_or(0.0))
        .sum();
    dot / (q_norm * t_norm)
}

/// Build a formatted "📖 记忆" context string from session memory.
/// Filters events/facts/edges by relevance to current characters and node.
/// When `query_embedding` is provided, also ranks by cosine similarity (embedding RAG).
pub fn build_memory_context(
    session: &TavernSession,
    max_events: usize,
    max_facts: usize,
    max_edges: usize,
    query_embedding: Option<&[f32]>,
) -> String {
    let mut lines = Vec::new();

    // --- Events ---
    if !session.memory_l2.events.is_empty() {
        let char_ids: Vec<&str> = session.present_character_ids.iter().map(|s| s.as_str()).collect();
        let cur_node = session.node_id.as_deref().unwrap_or("");

        let mut scored: Vec<(f32, &MemoryL2Event)> = session
            .memory_l2
            .events
            .iter()
            .map(|e| {
                let mut score = 0f32;
                // relevance: same node
                if e.node_id.as_deref() == Some(cur_node) && !cur_node.is_empty() {
                    score += 3.0;
                }
                // relevance: actor overlap
                for actor in &e.actors {
                    if char_ids.contains(&actor.as_str()) {
                        score += 2.0;
                    }
                }
                // relevance: recency
                if e.turn >= session.turn.saturating_sub(3) {
                    score += 1.0;
                }
                // semantic: cosine similarity (embedding RAG)
                if let Some(q_emb) = query_embedding {
                    let sim = if !e.embedding.is_empty() {
                        // Real embedding similarity
                        let dot: f32 = e.embedding.iter().zip(q_emb.iter()).map(|(a, b)| a * b).sum();
                        let mag_e: f32 = e.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
                        let mag_q: f32 = q_emb.iter().map(|x| x * x).sum::<f32>().sqrt();
                        let denom = mag_e.max(1e-8) * mag_q.max(1e-8);
                        (dot / denom).clamp(-1.0, 1.0)
                    } else {
                        // Fallback to token-level cosine
                        let q_tokens = tokenize(&format!("{} {}", e.kind, e.summary));
                        let target = format!("{} {}", e.kind, e.summary);
                        cosine_similarity(&q_tokens, &target)
                    };
                    // blend: 60% relevance + 40% semantic
                    score = score * 0.6 + sim * 3.0 * 0.4;
                }
                (score, e)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        lines.push("📖 记忆".into());
        lines.push("".into());

        let take = max_events.min(scored.len());
        if take > 0 {
            lines.push("过去事件：".into());
            for (_score, e) in scored.iter().take(take) {
                let actor_str = if e.actors.is_empty() {
                    String::new()
                } else {
                    format!("（{}）", e.actors.join(", "))
                };
                lines.push(format!(
                    "  • [turn {}] {}{} — {}",
                    e.turn,
                    if e.kind.is_empty() { "事件".into() } else { e.kind.clone() },
                    actor_str,
                    e.summary
                ));
            }
        }
    }

    // --- Edges ---
    if !session.memory_l3.edges.is_empty() && max_edges > 0 {
        lines.push("".into());
        lines.push("人物关系：".into());
        let take = max_edges.min(session.memory_l3.edges.len());
        for edge in session.memory_l3.edges.iter().take(take) {
            let from = edge.get("from").and_then(|v| v.as_str()).unwrap_or("?");
            let to = edge.get("to").and_then(|v| v.as_str()).unwrap_or("?");
            let rel = edge.get("rel").and_then(|v| v.as_str()).unwrap_or("?");
            let note = edge.get("note").and_then(|v| v.as_str()).unwrap_or("");
            if !note.is_empty() {
                lines.push(format!("  • {} → {}（{}）：{}", from, to, rel, note));
            } else {
                lines.push(format!("  • {} → {}（{}）", from, to, rel));
            }
        }
    }

    // --- Facts ---
    if !session.memory_l3.facts.is_empty() && max_facts > 0 {
        lines.push("".into());
        lines.push("已知事实：".into());
        let take = max_facts.min(session.memory_l3.facts.len());
        for fact in session.memory_l3.facts.iter().rev().take(take).rev() {
            lines.push(format!("  • {}", fact));
        }
    }

    if lines.len() <= 2 {
        return String::new();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{parse_affinity_delta, parse_extraction_response};
    use crate::{
        apply_extraction, classify_engine_tag, heuristic_l2_l3_from_turn, EngineTag, PlayerState,
        TavernSession, TurnExtraction,
    };
    use crate::story_tavern::{ActorStateSystem, TurnCostLedger};
    use crate::world_state::WorldState;
    use serde_json::json;

    #[test]
    fn test_classify_engine() {
        assert_eq!(classify_engine_tag("继续"), EngineTag::Advance);
        assert_eq!(classify_engine_tag("然后呢"), EngineTag::Advance);
        assert_eq!(classify_engine_tag("剧情推进",), EngineTag::Canon);
        assert_eq!(classify_engine_tag("[剧情推进]跳时间"), EngineTag::Canon);
        assert_eq!(classify_engine_tag("你好"), EngineTag::Idle);
    }

    #[test]
    fn test_heuristic_l2_l3() {
        let mut session = create_test_session();
        session.turn = 2;
        session.present_character_ids = vec!["cc-a".into(), "cc-b".into()];
        session.focus_character_id = Some("cc-a".into());
        let ext = heuristic_l2_l3_from_turn(&session, "我答应你保密", "她点头，把秘密藏进袖中。");
        assert!(!ext.new_events.is_empty());
        assert_eq!(ext.new_events[0].kind, "promise");
        apply_extraction(&mut session, &ext, EngineTag::Idle);
        assert!(!session.memory_l2.events.is_empty());
        assert!(!session.memory_l3.edges.is_empty());
    }

    #[test]
    fn test_heuristic_agreement_detection() {
        // ST-35: 「窝边草」复现——「跟我一块儿去」位于正文中段（非前60字），
        // 必须被提取为 L2 事件约定 + L3 facts，跨回合可见。
        let sess = TavernSession {
            turn: 13,
            focus_character_id: Some("cc-a".into()),
            present_character_ids: vec!["cc-a".into(), "cc-b".into()],
            ..create_test_session()
        };
        let long_asst = "她推开碗，眼睛亮起来：「下午我想去趟店里。你要是闲着，跟我一块儿去，回来顺便买点菜。」\n向明初应下：「那下午我跟您去店里。」";
        let ext = heuristic_l2_l3_from_turn(&sess, "好，那就说定了", long_asst);
        assert!(
            ext.new_facts.iter().any(|f| f.contains("约定")),
            "约定应进入 L3 facts: {:?}",
            ext.new_facts
        );
        assert!(
            ext.new_events[0].summary.contains("约定"),
            "约定应附加到 L2 事件 summary: {:?}",
            ext.new_events[0].summary
        );

        // 无约定关键词 → 不误报
        let ext2 = heuristic_l2_l3_from_turn(&sess, "今天天气不错", "她点点头，继续看书。");
        assert!(ext2.new_facts.iter().all(|f| !f.contains("约定")));
    }

    #[test]
    fn test_parse_affinity() {
        assert_eq!(parse_affinity_delta("+5"), 5);
        assert_eq!(parse_affinity_delta("-10"), -10);
        assert_eq!(parse_affinity_delta("0"), 0);
        assert_eq!(parse_affinity_delta("+15"), 15);
    }

    #[test]
    fn test_heuristic_affinity_symmetric() {
        // 亲密 → +6
        let sess = TavernSession {
            memory_l4: Default::default(),
            turn: 2,
            focus_character_id: Some("cc-a".into()),
            present_character_ids: vec!["cc-a".into()],
            ..create_test_session()
        };
        let ext = heuristic_l2_l3_from_turn(&sess, "我轻轻抱住她", "她身子一颤，耳根红了。");
        assert_eq!(ext.new_affinity.get("cc-a").and_then(|v| v.as_str()), Some("+6"));
        // R 层: 亲密 → romance 关系边
        assert_eq!(ext.new_edges.first().and_then(|e| e.get("rel")).and_then(|v| v.as_str()), Some("romance"));

        // 冲突/冒犯 → -6
        let ext2 = heuristic_l2_l3_from_turn(&sess, "你骗我！我恨你", "她冷笑一声，转身离去。");
        assert_eq!(ext2.new_affinity.get("cc-a").and_then(|v| v.as_str()), Some("-6"));
        // R 层: 敌意 → 敌对 关系边
        assert_eq!(ext2.new_edges.first().and_then(|e| e.get("rel")).and_then(|v| v.as_str()), Some("敌对"));

        // 中性 → 无 affinity 变化
        let ext3 = heuristic_l2_l3_from_turn(&sess, "今天天气不错", "她点点头，继续看书。");
        assert!(ext3.new_affinity.as_object().map_or(true, |m| m.is_empty()));
        // R 层: 中性 → interact 兜底
        assert_eq!(ext3.new_edges.first().and_then(|e| e.get("rel")).and_then(|v| v.as_str()), Some("interact"));

        // 亲密优先于冲突（两者同时出现时）
        let ext4 = heuristic_l2_l3_from_turn(&sess, "我抱住她骂她骗子", "她哭着推开我。");
        assert_eq!(ext4.new_affinity.get("cc-a").and_then(|v| v.as_str()), Some("+6"));
        // R 层: 亲密优先 → romance（非 敌对）
        assert_eq!(ext4.new_edges.first().and_then(|e| e.get("rel")).and_then(|v| v.as_str()), Some("romance"));
    }

    #[test]
    fn test_extraction_prompt_roundtrip() {
        let json_str = r#"{
            "sceneSummary": "在茶馆中与林晚见面",
            "affinity": {"cc-linwan": "+5"},
            "optionsForNext": ["拆开信", "假装没看见"]
        }"#;
        let ext = parse_extraction_response(json_str);
        assert_eq!(ext.new_scene_summary, "在茶馆中与林晚见面");
        assert_eq!(
            ext.new_affinity.get("cc-linwan").and_then(|v| v.as_str()),
            Some("+5")
        );
        assert_eq!(ext.options_for_next.len(), 2);
    }

    #[test]
    fn test_apply_affinity() {
        let mut session = TavernSession {
            memory_l4: Default::default(),
            turn: 3,
            ..create_test_session()
        };
        let ext = TurnExtraction {
            new_affinity: json!({"cc-linwan": "+10"}),
            ..Default::default()
        };
        apply_extraction(&mut session, &ext, EngineTag::Idle);

        let aff = session.memory_l4.affinity.get("cc-linwan").and_then(|v| v.as_i64());
        assert_eq!(aff, Some(60)); // default 50 + 10
    }

    fn create_test_session() -> TavernSession {
        TavernSession {
            session_id: "test".into(),
            pack_id: "demo".into(),
            pack_missing: false,
            owner: None,
            quality: crate::Quality::Lite,
            playable: crate::Playable::P1,
            play_mode: crate::PlayMode::Mainline,
            content_tier: crate::ContentTier::Standard,
            user_tier_request: crate::ContentTier::Standard,
            entry: Default::default(),
            chapter_cursor: Some("ch01".into()),
            node_id: Some("n1".into()),
            resume_node_id: None,
            opening_seeded: false,
            side_branch_node_id: None,
            side_branch_label: None,
            current_worldline_id: None,
            last_restored_save_id: None,
            panels: vec![],
            mcp_tool_results: vec![],
            skill_load: None,
            timeline_id: "main".into(),
            turn: 0,
            present_character_ids: vec![],
            focus_character_id: None,
            speaker_rotation: true,
            player: PlayerState::default(),
            memory_l1: Default::default(),
            memory_l2: Default::default(),
            memory_l3: Default::default(),
            memory_l4: Default::default(),
            guard_events: vec![],
            messages: vec![],
            active_run_id: None,
            adult_confirmed: false,
            title: "test".into(),
            created_at: String::new(),
            updated_at: String::new(),
            author_project_id: None,
            author_live_path: None,
            author_live_enabled: true,
            author_live_every_n: 1,
            author_live_write_turns: false,
            actor_states: ActorStateSystem::default(),
            director_plan: None,
            director_pending: false,
            director_task: None,
            last_event: None,
            last_check_results: vec![],
            check_history: vec![],
            checkpoints: vec![],
            // U11: 测试构造显式初始化新字段。
            epoch: 0,
            epoch_last_turn: None,
            epoch_last_chars: None,
            turn_cost_ledger: TurnCostLedger::default(),
            last_turn_diagnostic: None,
            xiami_skim_issues: Vec::new(),
            xiami_skim_sample: String::new(),
            chapter_diaries: Vec::new(),
            diary_config: None,
            turn_progress: None,
            world: WorldState::default(),
            game_clock: Default::default(),
        }
    }
}
