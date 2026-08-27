//! 大纲补丁系统 + 章节执行合同（X4）
//! 吞噬自 xiami outline.rs（虾米大纲补丁影响分析 + 章节执行合同）。
//! 纯函数无 IO / LLM：补丁影响分析（analyze_outline_impact）+ 章节执行合同
//! （build_chapter_execution_contract / render_execution_contract / validate_contract_references）。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// 章节简略剧情视图（映射 xiami ChapterBrief 的关键字段，不依赖 NovelProject）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterBriefView {
    pub chapter_number: u32,
    pub key_events: Vec<String>,        // 必须事件
    pub character_focus: String,
    pub forbidden_reveals: Vec<String>, // 禁止提前揭示
    pub dependency_chapters: Vec<u32>,  // 依赖章节（影响传播用）
    pub hook_ids: Vec<String>,          // 伏笔动作
    pub world_rule_ids: Vec<String>,    // 世界规则引用（结构变化检测用）
    pub character_ids: Vec<String>,     // 人物引用（结构变化检测用）
    pub summary: String,
}

/// 补丁影响分析结果（映射 xiami OutlineImpactAnalysis）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct OutlineImpactAnalysis {
    pub direct_impacts: Vec<u32>,       // 简略剧情被修改的章节
    pub indirect_impacts: Vec<u32>,     // 依赖传播影响的章节
    pub unaffected: Vec<u32>,           // 不受影响的章节
    pub blocking_conflicts: Vec<String>, // 已发布章节被覆盖（阻断）
    pub high_risk_changes: Vec<String>, // 结构级变更（依赖/人物/伏笔/规则/禁止揭示）
}

/// 补丁影响分析：输入候选新章节列表 vs 现状列表，输出影响分析。
/// 语义（吞噬自 xiami outline.rs analyze_outline_impact）：
/// - direct = 新旧不一致的章节号
/// - indirect = 通过 dependency_chapters 反向传播可达的章节（不含 direct）
/// - blocking_conflicts = direct 中 <= 已发布章节号（current_chapter）的条目
/// - high_risk_changes = direct 中发生结构级变更（key_events / 人物 / 伏笔 / 规则 / 禁止揭示集合变化）
pub fn analyze_outline_impact(
    current_chapter: u32,
    before: &[ChapterBriefView],
    after: &[ChapterBriefView],
) -> OutlineImpactAnalysis {
    let before_map: HashMap<u32, &ChapterBriefView> = before
        .iter()
        .map(|brief| (brief.chapter_number, brief))
        .collect();
    let after_map: HashMap<u32, &ChapterBriefView> = after
        .iter()
        .map(|brief| (brief.chapter_number, brief))
        .collect();
    let all_numbers: HashSet<u32> = before_map
        .keys()
        .chain(after_map.keys())
        .copied()
        .collect();
    let direct: HashSet<u32> = all_numbers
        .iter()
        .copied()
        .filter(|number| before_map.get(number) != after_map.get(number))
        .collect();

    let reverse = reverse_dependencies(after);
    let mut indirect = HashSet::new();
    let mut queue: Vec<u32> = direct.iter().copied().collect();
    while let Some(chapter) = queue.pop() {
        if let Some(dependents) = reverse.get(&chapter) {
            for dependent in dependents {
                if !direct.contains(dependent) && indirect.insert(*dependent) {
                    queue.push(*dependent);
                }
            }
        }
    }

    let mut blocking_conflicts = direct
        .iter()
        .copied()
        .filter(|number| *number <= current_chapter)
        .map(|number| format!("第 {number} 章已经发布，补丁不得覆盖"))
        .collect::<Vec<_>>();
    blocking_conflicts.sort();
    let mut high_risk_changes = direct
        .iter()
        .copied()
        .filter_map(|number| {
            let previous = before_map.get(&number)?;
            let next = after_map.get(&number)?;
            structural_brief_change(previous, next)
                .then(|| format!("第 {number} 章修改了依赖、人物、伏笔、世界规则或禁止揭示边界"))
        })
        .collect::<Vec<_>>();
    high_risk_changes.sort();

    let mut direct_impacts: Vec<u32> = direct.into_iter().collect();
    let mut indirect_impacts: Vec<u32> = indirect.into_iter().collect();
    let mut unaffected: Vec<u32> = all_numbers
        .into_iter()
        .filter(|number| !direct_impacts.contains(number) && !indirect_impacts.contains(number))
        .collect();
    direct_impacts.sort_unstable();
    indirect_impacts.sort_unstable();
    unaffected.sort_unstable();
    OutlineImpactAnalysis {
        direct_impacts,
        indirect_impacts,
        unaffected,
        blocking_conflicts,
        high_risk_changes,
    }
}

/// 章节执行合同（映射 xiami ChapterExecutionContract）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterExecutionContract {
    pub chapter_number: u32,
    pub source_plan_id: String,
    pub required_events: Vec<String>,            // 必须事件
    pub required_character_changes: Vec<String>, // 必须人物变化
    pub required_hook_actions: Vec<String>,      // 伏笔动作（"推进或维持伏笔 {id}，不得伪造已回收状态"）
    pub forbidden_reveals: Vec<String>,          // 禁止提前揭示
    pub opening_state: String,                   // 开场状态
    pub ending_state: String,                    // 目标结尾状态
    pub emotional_arc: String,                   // 情绪轨迹
    pub dialogue_focus: String,                  // 对白焦点
    pub target_words: usize,                     // 目标字数
}

/// 从 ChapterBriefView 构建执行合同（映射 xiami build_chapter_execution_contract 的映射规则）。
/// 视图无 prerequisites/scene_goal/outcomes/ending_hook 时：
/// - opening_state 退化为 summary（视图中的开场状态描述）
/// - ending_state 留空（渲染时按"无新增硬性项"显示）
pub fn build_chapter_execution_contract(
    brief: &ChapterBriefView,
    source_plan_id: &str,
    target_words: usize,
) -> ChapterExecutionContract {
    ChapterExecutionContract {
        chapter_number: brief.chapter_number,
        source_plan_id: source_plan_id.to_owned(),
        required_events: brief.key_events.clone(),
        required_character_changes: if brief.character_focus.trim().is_empty() {
            Vec::new()
        } else {
            vec![brief.character_focus.trim().to_owned()]
        },
        required_hook_actions: brief
            .hook_ids
            .iter()
            .map(|id| format!("推进或维持伏笔 {id}，不得伪造已回收状态"))
            .collect(),
        forbidden_reveals: brief.forbidden_reveals.clone(),
        opening_state: brief.summary.trim().to_owned(),
        ending_state: String::new(),
        emotional_arc: String::new(),
        dialogue_focus: brief.character_focus.trim().to_owned(),
        target_words,
    }
}

/// 渲染执行合同为注入 system prompt 的中文文本（映射 xiami render_execution_contract）。
pub fn render_execution_contract(contract: &ChapterExecutionContract) -> String {
    format!(
        "# 章节执行合同\n合同来源：计划 {}\n章节：第 {} 章\n开场状态：{}\n必须完成事件：{}\n必须产生人物变化：{}\n伏笔动作：{}\n禁止提前揭示：{}\n目标结尾状态：{}\n情绪轨迹：{}\n对白焦点：{}\n目标字数：约 {} 字\n\n验收规则：必须事件、人物选择和伏笔动作逐项落在正文因果中；禁止揭示不得命中。允许场景表达自然变化，但不得改变核心结果。必须事件均为**本章将要发生的未来剧情**，不得作为已发生事实在开场提前回述；正文只呈现已推进到的事件本身。",
        contract.source_plan_id,
        contract.chapter_number,
        display_list(&[contract.opening_state.clone()]),
        display_list(&contract.required_events),
        display_list(&contract.required_character_changes),
        display_list(&contract.required_hook_actions),
        display_list(&contract.forbidden_reveals),
        display_list(&[contract.ending_state.clone()]),
        contract.emotional_arc,
        contract.dialogue_focus,
        contract.target_words,
    )
}

/// 校验合同引用的伏笔 id 是否存在（映射 xiami validate_contract_references 的语义简化版）。
/// 返回未收录的伏笔 id（缺失引用警告）。
pub fn validate_contract_references(
    contract: &ChapterExecutionContract,
    known_hook_ids: &[String],
) -> Vec<String> {
    let known: HashSet<&str> = known_hook_ids.iter().map(String::as_str).collect();
    contract
        .required_hook_actions
        .iter()
        .filter_map(|action| {
            extract_hook_id(action).filter(|id| !known.contains(id)).map(str::to_owned)
        })
        .collect()
}

/// reverse_dependencies 用 HashMap<u32, Vec<u32>> 从 after 的 dependency_chapters 构建。
/// 与 xiami reverse_dependencies 的 dependency 分支同语义。
fn reverse_dependencies(briefs: &[ChapterBriefView]) -> HashMap<u32, Vec<u32>> {
    let mut reverse = HashMap::<u32, Vec<u32>>::new();
    for brief in briefs {
        for dependency in &brief.dependency_chapters {
            reverse
                .entry(*dependency)
                .or_default()
                .push(brief.chapter_number);
        }
    }
    for dependents in reverse.values_mut() {
        dependents.sort_unstable();
        dependents.dedup();
    }
    reverse
}

/// 结构级变更判定（xiami 语义）：key_events 集合不同 ∨ character_ids 集合不同 ∨
/// hook_ids 集合不同 ∨ world_rule_ids 集合不同 ∨ forbidden_reveals 集合不同。
fn structural_brief_change(left: &ChapterBriefView, right: &ChapterBriefView) -> bool {
    set_changed(&left.key_events, &right.key_events)
        || set_changed(&left.character_ids, &right.character_ids)
        || set_changed(&left.hook_ids, &right.hook_ids)
        || set_changed(&left.world_rule_ids, &right.world_rule_ids)
        || set_changed(&left.forbidden_reveals, &right.forbidden_reveals)
}

/// 集合（去重、乱序无关）比较。
fn set_changed(left: &[String], right: &[String]) -> bool {
    let left: HashSet<&str> = left.iter().map(String::as_str).collect();
    let right: HashSet<&str> = right.iter().map(String::as_str).collect();
    left != right
}

/// 从合同伏笔动作文案提取伏笔 id（生成格式固定：推进或维持伏笔 {id}，不得伪造已回收状态）。
fn extract_hook_id(action: &str) -> Option<&str> {
    const PREFIX: &str = "推进或维持伏笔 ";
    const SUFFIX: &str = "，不得伪造已回收状态";
    let id = action.strip_prefix(PREFIX)?.strip_suffix(SUFFIX).unwrap_or(action);
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// 列表渲染：空列表显示"无新增硬性项"，否则以"；"连接（映射 xiami display_list）。
fn display_list(items: &[String]) -> String {
    let items = items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if items.is_empty() {
        "无新增硬性项".to_owned()
    } else {
        items.join("；")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief(number: u32) -> ChapterBriefView {
        ChapterBriefView {
            chapter_number: number,
            key_events: vec!["发现线索".to_owned()],
            character_focus: "主角做出选择".to_owned(),
            summary: "发生不可逆变化".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn direct_change_marks_modified_chapter() {
        let before = vec![brief(1), brief(2)];
        let mut after = vec![brief(1), brief(2)];
        after[0].key_events = vec!["新事件".to_owned()];
        let impact = analyze_outline_impact(0, &before, &after);
        assert_eq!(impact.direct_impacts, vec![1]);
        assert!(impact.indirect_impacts.is_empty());
        assert_eq!(impact.unaffected, vec![2]);
    }

    #[test]
    fn dependency_propagation_marks_indirect_impacts() {
        let mut before = vec![brief(1), brief(2), brief(3)];
        let mut after = vec![brief(1), brief(2), brief(3)];
        // 依赖关系在 before/after 保持一致：2 依赖 1，3 依赖 2 —— 只影响传播，不产生 direct。
        for v in [&mut before, &mut after] {
            v[1].dependency_chapters = vec![1];
            v[2].dependency_chapters = vec![2];
        }
        after[0].summary = "第一章结果被改动".to_owned();
        let impact = analyze_outline_impact(0, &before, &after);
        assert_eq!(impact.direct_impacts, vec![1]);
        assert_eq!(impact.indirect_impacts, vec![2, 3]);
        assert!(impact.unaffected.is_empty());
    }

    #[test]
    fn published_chapter_patch_is_blocked() {
        let before = vec![brief(1), brief(2)];
        let mut after = vec![brief(1), brief(2)];
        after[0].summary = "试图覆盖已发布内容".to_owned();
        let impact = analyze_outline_impact(1, &before, &after);
        assert_eq!(impact.blocking_conflicts, vec!["第 1 章已经发布，补丁不得覆盖"]);
        assert!(impact.high_risk_changes.is_empty());
    }

    #[test]
    fn structural_change_is_flagged_high_risk() {
        let before = vec![brief(1)];
        let mut after = vec![brief(1)];
        after[0].hook_ids = vec!["hook-new".to_owned()];
        let impact = analyze_outline_impact(0, &before, &after);
        assert!(impact.direct_impacts.contains(&1));
        assert_eq!(
            impact.high_risk_changes,
            vec!["第 1 章修改了依赖、人物、伏笔、世界规则或禁止揭示边界"]
        );
    }

    #[test]
    fn unchanged_outline_has_no_impacts() {
        let before = vec![brief(1), brief(2), brief(3)];
        let after = vec![brief(1), brief(2), brief(3)];
        let impact = analyze_outline_impact(0, &before, &after);
        assert!(impact.direct_impacts.is_empty());
        assert!(impact.indirect_impacts.is_empty());
        assert_eq!(impact.unaffected, vec![1, 2, 3]);
        assert!(impact.blocking_conflicts.is_empty());
        assert!(impact.high_risk_changes.is_empty());
    }

    #[test]
    fn contract_maps_brief_fields() {
        let view = ChapterBriefView {
            chapter_number: 3,
            key_events: vec!["取得证据".to_owned(), "迫使对手让步".to_owned()],
            character_focus: String::new(),
            forbidden_reveals: vec!["幕后黑手身份".to_owned()],
            hook_ids: vec!["hook-1".to_owned()],
            summary: "取证对峙".to_owned(),
            ..Default::default()
        };
        let contract = build_chapter_execution_contract(&view, "plan-a", 3600);
        assert_eq!(contract.chapter_number, 3);
        assert_eq!(contract.source_plan_id, "plan-a");
        assert_eq!(contract.required_events.len(), 2);
        assert!(contract.required_character_changes.is_empty());
        assert_eq!(
            contract.required_hook_actions,
            vec!["推进或维持伏笔 hook-1，不得伪造已回收状态"]
        );
        assert_eq!(contract.forbidden_reveals, vec!["幕后黑手身份"]);
        assert_eq!(contract.opening_state, "取证对峙");
        assert_eq!(contract.target_words, 3600);
    }

    #[test]
    fn contract_carries_character_focus_as_change() {
        let view = ChapterBriefView {
            chapter_number: 2,
            character_focus: "主角做出选择".to_owned(),
            ..Default::default()
        };
        let contract = build_chapter_execution_contract(&view, "director", 0);
        assert_eq!(contract.required_character_changes, vec!["主角做出选择"]);
        assert_eq!(contract.dialogue_focus, "主角做出选择");
    }

    #[test]
    fn render_contract_covers_all_fields() {
        let view = ChapterBriefView {
            chapter_number: 4,
            key_events: vec!["发现线索".to_owned()],
            character_focus: "主角".to_owned(),
            forbidden_reveals: vec!["真相".to_owned()],
            hook_ids: vec!["hook-2".to_owned()],
            summary: "开场对峙".to_owned(),
            ..Default::default()
        };
        let contract = build_chapter_execution_contract(&view, "plan-x", 5000);
        let rendered = render_execution_contract(&contract);
        for needle in [
            "章节执行合同",
            "合同来源：计划 plan-x",
            "第 4 章",
            "必须完成事件",
            "必须产生人物变化",
            "伏笔动作",
            "禁止提前揭示",
            "目标结尾状态",
            "对白焦点",
            "约 5000 字",
            "验收规则",
            "不得改变核心结果",
        ] {
            assert!(rendered.contains(needle), "rendered missing: {needle}\n{rendered}");
        }
    }

    #[test]
    fn validate_references_reports_missing_hooks() {
        let view = ChapterBriefView {
            chapter_number: 1,
            hook_ids: vec!["hook-known".to_owned(), "hook-missing".to_owned()],
            ..Default::default()
        };
        let contract = build_chapter_execution_contract(&view, "director", 0);
        let missing = validate_contract_references(&contract, &["hook-known".to_owned()]);
        assert_eq!(missing, vec!["hook-missing"]);
    }
}
