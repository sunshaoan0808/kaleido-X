//! 实体解析层（Elemental Layer，[ENT] Wave）。
//!
//! 背景：关系图谱（relations.json）一度裸字符串端点——"我"/"夏文嘉"/"陈妹妹"
//! 各自为政："我"未归一到主角卡，"陈妹妹"是幽灵（无卡）。本模块把关系边端点
//! 解析到具体角色卡 id，并提供「幽灵清单」与「稀疏角色告警」两个对账工具。
//!
//! 三通道解析：
//! - 专名通道：与角色卡 name 精确匹配（cards 元组签名不含 aliases 声明，故
//!   alias_map 退化为恒等；真实别名声明可在调用侧展开后传入更丰富的 cards）。
//! - 语境称呼通道：与角色卡 name/role 做包含匹配（不区分大小写），唯一候选即
//!   解析，多个候选挂起（None，不误判）。
//! - 叙述者通道：{我,我们,叙述者,主角} → 锚定 pack 主角。
//!
//! 纯函数、无 IO、无 LLM 依赖。

use std::collections::HashSet;

use crate::alias_merge;

/// 端点分级：专名 / 语境称呼(kin) / 碎片(discard)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    /// 合法专名（≥2 汉字、非虚词、非语境称呼）。
    Proper,
    /// 常见亲属/语境称呼（母亲/爸爸/哥哥/妹妹/爷爷/奶奶/那女人/那男人 等）。
    Kin,
    /// 句子片段、纯虚词、碎片（"我"除外——"我"走叙述者通道，见 resolve）。
    Discard,
}

/// 契约默认决策补充的语境称呼（CONTEXTUAL_TERMS 之外的叫法）。
/// 含 CONTEXTUAL_TERMS 全量由 [`is_kin_term`] 内的 `alias_safety_level==0` 兜底覆盖。
const KIN_TERMS: &[&str] = &[
    "叔叔", "阿姨", "那女人", "那男人", "那丫头", "那小子",
];

/// 泛称/集体词：指代不明的一群人或非特定对象，绝不当关系端点。
/// 与 CONTEXTUAL_TERMS 的「泛称/集体」段同源——契约只要求亲属称呼
/// （母亲/父亲）保留为 Kin，泛称词必须仍归 Discard，否则幽灵列表膨胀。
const COLLECTIVE_TERMS: &[&str] = &[
    "那怪", "群妖", "众妖", "众仙", "众人", "村民", "百姓", "手下", "随从",
    "仆从", "侍女", "丫鬟", "家丁", "护卫", "侍卫", "将士", "士兵", "官兵",
    "贼人", "土匪", "路人", "看客", "围观者", "群众", "邻居", "同事", "朋友",
    "好友", "同学", "室友",
];

/// 纯虚词/代词的整名集合（"我们/他们/大家"这类不可当专名的通用指代）。
/// 与 alias_merge FRAGMENT_SUBSTRINGS 互补：那些是句子碎片，这些是完整代词词。
const FUNCTION_WORDS: &[&str] = &[
    "我们", "你们", "他们", "她们", "它们", "这个", "那个", "这些", "那些",
    "自己", "大家", "别人", "有人", "哪里", "这里", "那里", "这样", "那样",
    "谁", "什么", "怎么", "何时", "何地",
];

/// 叙述者泛指集合：第一人称视角/旁白指代，统一锚定 pack 主角。
const NARRATOR_REFS: &[&str] = &["我", "我们", "叙述者", "主角"];

/// 判定一条端点是否属于叙述者泛指。
fn is_narrator_ref(name: &str) -> bool {
    NARRATOR_REFS.contains(&name.trim())
}

/// 名称是否包含子串（不区分大小写）。
fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// 是否语境称呼：显式 KIN_TERMS + 别名合并 CONTEXTUAL_TERMS（alias_safety_level==0）。
fn is_kin_term(name: &str) -> bool {
    KIN_TERMS.contains(&name) || alias_merge::alias_safety_level(name) == 0
}

/// 是否泛称/集体词（指代不明，绝不当端点）。
fn is_collective_term(name: &str) -> bool {
    COLLECTIVE_TERMS.contains(&name)
}

/// 是否纯虚词整名。
fn is_pure_function_word(name: &str) -> bool {
    FUNCTION_WORDS.contains(&name)
}

/// 简化版「合法专名」：≥2 汉字 且 非纯虚词。句子碎片已在 classify 前一道过滤。
fn is_proper_plausible(name: &str) -> bool {
    let han_count = name
        .chars()
        .filter(|c| matches!(*c, '\u{4e00}'..='\u{9fff}'))
        .count();
    han_count >= 2 && !is_pure_function_word(name)
}

/// 判定端点分级。
///
/// 规则：
/// - Proper：合法专名（≥2 汉字、非虚词）且非语境称呼；
/// - Kin：常见亲属/语境称呼（含 but not limited to CONTEXTUAL_TERMS）；
/// - Discard：句子片段、纯虚词、碎片（"我" 返回 Discard——专走叙述者通道，
///   见 [`resolve_entity_endpoint`]，其先处理叙述者通道再调本函数）。
pub fn classify_endpoint(name: &str) -> EndpointKind {
    let n = name.trim();
    if n.is_empty() {
        return EndpointKind::Discard;
    }
    // 泛称/集体词优先 Discard（指代不明的一群人，绝不当端点）。
    if is_collective_term(n) {
        return EndpointKind::Discard;
    }
    // 语境称呼优先：那女人/母亲/爸爸 等都算 Kin（包含 CONTEXTUAL_TERMS 亲属/称谓段）。
    if is_kin_term(n) {
        return EndpointKind::Kin;
    }
    // 句子碎片（复用 alias_merge 同源黑名单）→ Discard。
    if alias_merge::is_unsafe_alias(n) {
        return EndpointKind::Discard;
    }
    // 纯虚词整名（我们/他们/大家…）→ Discard。
    if is_pure_function_word(n) {
        return EndpointKind::Discard;
    }
    // 合法专名。
    if is_proper_plausible(n) {
        return EndpointKind::Proper;
    }
    EndpointKind::Discard
}

/// 角色的 role 是否把该卡自身定位为「叙述者/主角」（而非描述他人身份）。
///
/// 用「叙述者，」/「叙述者、」/纯「叙述者」这类身份短语判断，刻意排除
/// 「叙述者的前女友」「主角的初恋女友」「主角夏文嘉的母亲」等描述当事人
/// 亲属/关系对象的角色——否则「我」会错误锚定到母亲/前女友卡上。
fn is_narrator_role(role: &str) -> bool {
    let r = role.trim();
    (r == "叙述者" || r.starts_with("叙述者，") || r.starts_with("叙述者、"))
        || (r == "主角" || r.starts_with("主角，") || r.starts_with("主角、"))
}

/// 叙述者锚定：pack 主角卡 id。
///
/// 优先级（[ENT] 派生自契约「锚定 pack 主角」）：
/// 1. role 定位为叙述者/主角 且 importance==high 的卡（第一人称主角-叙述者合一，
///    宿醉=夏文嘉 c-distil-4；不会被「主角夏文嘉的母亲」c-distil-0 截胡）；
/// 2. importance==high 的第一张卡；
/// 3. role 定位为叙述者/主角 的第一张卡；
/// 4. 都无 → None。
fn narrator_anchor(cards: &[(String, String, String, String)]) -> Option<String> {
    // 1. 叙述者-主角卡 + high。
    if let Some((id, _, _role, _imp)) = cards.iter().find(|(_, _, role, imp)| {
        is_narrator_role(role) && imp.eq_ignore_ascii_case("high")
    }) {
        return Some(id.clone());
    }
    // 2. 首个 importance==high。
    if let Some((id, _, _, _)) = cards.iter().find(|(_, _, _, imp)| imp.eq_ignore_ascii_case("high")) {
        return Some(id.clone());
    }
    // 3. 首个 role 定位为叙述者/主角。
    if let Some((id, _, _role, _)) = cards.iter().find(|(_, _, role, _)| is_narrator_role(role)) {
        return Some(id.clone());
    }
    None
}

/// 按 name 精确匹配一张卡 → id。
fn exact_name_match(cards: &[(String, String, String, String)], name: &str) -> Option<String> {
    cards
        .iter()
        .find(|(_, n, _, _)| n == name)
        .map(|(id, _, _, _)| id.clone())
}

/// 三通道解析一条边端点 → 角色卡 id。
///
/// 1. 专名通道：与角色卡 name 精确匹配（cards 元组无 aliases 声明，alias_map 恒等）；
/// 2. 语境称呼通道：与角色卡 name 或 role 做包含匹配，唯一候选即解析，
///    多个候选 → None（挂起，不误判）；
/// 3. 叙述者通道：name ∈ {我,我们,叙述者,主角} → 锚定 pack 主角。
///
/// 输入 `cards: &[(id, name, role, importance)]`。
pub fn resolve_entity_endpoint(
    name: &str,
    cards: &[(String, String, String, String)],
) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    // 叙述者通道先行（"我"在 classify 归 Discard，专走此通道）。
    if is_narrator_ref(n) {
        return narrator_anchor(cards);
    }
    match classify_endpoint(n) {
        EndpointKind::Discard => None,
        EndpointKind::Proper => {
            // 专名通道：只做精确匹配，避免将"陈妹妹"误并入含"妹妹"的卡。
            exact_name_match(cards, n)
        }
        EndpointKind::Kin => {
            // 语境称呼通道：先精确 name 匹配（母亲 == 卡名 母亲），
            // 再包含匹配（name/role 唯一候选）。
            if let Some(id) = exact_name_match(cards, n) {
                return Some(id);
            }
            let candidates: Vec<String> = cards
                .iter()
                .filter(|(_, cname, crole, _)| {
                    contains_ci(cname, n) || contains_ci(crole, n)
                })
                .map(|(id, _, _, _)| id.clone())
                .collect();
            if candidates.len() == 1 {
                Some(candidates[0].clone())
            } else {
                None // 多个候选 → 挂起，不误判
            }
        }
    }
}

/// 幽灵列表：edges 中解析不出卡 id 的端点（去重、保序）。
pub fn collect_ghosts(
    edges: &[serde_json::Value],
    cards: &[(String, String, String, String)],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for e in edges {
        for k in ["from", "to"] {
            let nm = e
                .get(k)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if nm.is_empty() {
                continue;
            }
            if resolve_entity_endpoint(&nm, cards).is_none() {
                if seen.insert(nm.clone()) {
                    out.push(nm);
                }
            }
        }
    }
    out
}

/// 稀疏告警：有卡但未出现在任何边端点（from/to 原始名或解析后 id 都算）的角色名。
///
/// 对每条边，统计被提及的卡 id（`from_id`/`to_id`）与卡原名（`from`/`to` 恰等于某卡 name）。
/// 一张卡只要 id 或 name 任一被提及即不算稀疏。返回按卡序排列的稀疏角色名。
pub fn find_sparse_characters(
    cards: &[(String, String, String, String)],
    edges: &[serde_json::Value],
) -> Vec<String> {
    let card_ids: HashSet<&str> = cards.iter().map(|(id, _, _, _)| id.as_str()).collect();
    let card_names: HashSet<&str> = cards.iter().map(|(_, n, _, _)| n.as_str()).collect();
    let mut mentioned: HashSet<String> = HashSet::new();
    for e in edges {
        for k in ["from", "to"] {
            // 解析后 id（crawler 5.5 段会写入 from_id/to_id）
            if let Some(id) = e.get(format!("{k}_id")).and_then(|x| x.as_str()) {
                if card_ids.contains(id) {
                    mentioned.insert(id.to_string());
                }
            }
            // 原始名恰等于某卡 name
            if let Some(nm) = e.get(k).and_then(|x| x.as_str()) {
                if card_names.contains(nm) {
                    mentioned.insert(nm.to_string());
                }
            }
        }
    }
    cards
        .iter()
        .filter(|(id, n, _, _)| !mentioned.contains(id) && !mentioned.contains(n))
        .map(|(_, n, _, _)| n.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, name: &str, role: &str, imp: &str) -> (String, String, String, String) {
        (id.to_string(), name.to_string(), role.to_string(), imp.to_string())
    }

    /// 宿醉基准 pack 的 5 张卡（与 data/story-packs/.../pack.json 对齐）。
    fn suzui_cards() -> Vec<(String, String, String, String)> {
        vec![
            c("c-distil-0", "母亲", "主角夏文嘉的母亲，已婚，家庭主妇", "high"),
            c("c-distil-1", "父亲", "夏文嘉的父亲，母亲的丈夫，家庭中的严厉角色", "low"),
            c("c-distil-2", "蒋闵柔", "叙述者的前女友（小女朋友），被叙述者称为柔柔", "low"),
            c("c-distil-3", "祁双双", "主角的初恋女友", "low"),
            c("c-distil-4", "夏文嘉", "叙述者，母亲的儿子", "high"),
        ]
    }

    #[test]
    fn proper_exact_name_match() {
        let cards = suzui_cards();
        assert_eq!(resolve_entity_endpoint("夏文嘉", &cards), Some("c-distil-4".to_string()));
    }

    #[test]
    fn kin_unique_candidate() {
        let cards = suzui_cards();
        // 母亲 → c-distil-0（name 精确 === "母亲"）
        assert_eq!(resolve_entity_endpoint("母亲", &cards), Some("c-distil-0".to_string()));
        // 父亲 → c-distil-1
        assert_eq!(resolve_entity_endpoint("父亲", &cards), Some("c-distil-1".to_string()));
    }

    #[test]
    fn kin_multiple_candidates_pending() {
        // 两个角色 role 都含"医生" → "医生" 挂起（None，不误判）。
        let cards = vec![
            c("c-a", "张三", "神经外科医生", "low"),
            c("c-b", "李四", "急诊科医生", "low"),
        ];
        assert_eq!(resolve_entity_endpoint("医生", &cards), None);
    }

    #[test]
    fn narrator_channel_anchors_protagonist() {
        let cards = suzui_cards();
        // 宿醉："我" → c-distil-4（夏文嘉，主角-叙述者合一）。
        assert_eq!(resolve_entity_endpoint("我", &cards), Some("c-distil-4".to_string()));
        assert_eq!(resolve_entity_endpoint("我们", &cards), Some("c-distil-4".to_string()));
    }

    #[test]
    fn ghost_collection() {
        let cards = suzui_cards();
        let edges = vec![
            serde_json::json!({"from": "陈妹妹", "to": "我", "rel": "亲属"}),
            serde_json::json!({"from": "夏文嘉", "to": "母亲", "rel": "母子"}),
        ];
        let ghosts = collect_ghosts(&edges, &cards);
        // 陈妹妹 无卡 → 幽灵；"我"解析成功不进幽灵；夏文嘉/母亲 有卡不进幽灵。
        assert_eq!(ghosts, vec!["陈妹妹".to_string()]);
    }

    #[test]
    fn sparse_character_warning() {
        let cards = suzui_cards();
        let edges = vec![
            serde_json::json!({"from": "母亲", "to": "父亲", "rel": "夫妻"}),
            serde_json::json!({"from": "夏文嘉", "to": "母亲", "rel": "母子"}),
        ];
        // c-distil-2 (蒋闵柔)、c-distil-3 (祁双双) 未出现在任何边端点 → 稀疏告警。
        let sparse = find_sparse_characters(&cards, &edges);
        assert!(sparse.contains(&"蒋闵柔".to_string()));
        assert!(sparse.contains(&"祁双双".to_string()));
        assert!(!sparse.contains(&"母亲".to_string()));
    }

    #[test]
    fn classify_kin_and_proper_and_discard() {
        assert_eq!(classify_endpoint("夏文嘉"), EndpointKind::Proper);
        assert_eq!(classify_endpoint("母亲"), EndpointKind::Kin);
        assert_eq!(classify_endpoint("那女人"), EndpointKind::Kin);
        assert_eq!(classify_endpoint("那怪"), EndpointKind::Discard); // 泛称/集体
        assert_eq!(classify_endpoint("众人"), EndpointKind::Discard); // 泛称/集体
        assert_eq!(classify_endpoint("村民"), EndpointKind::Discard); // 泛称/集体
        assert_eq!(classify_endpoint("我们"), EndpointKind::Discard); // 纯虚词
        assert_eq!(classify_endpoint("我"), EndpointKind::Discard); // 专走叙述者通道
        assert_eq!(classify_endpoint("那就"), EndpointKind::Discard); // 句子碎片
        assert_eq!(classify_endpoint("陈妹妹"), EndpointKind::Proper); // 专名（幽灵由卡缺失导致）
    }
}
