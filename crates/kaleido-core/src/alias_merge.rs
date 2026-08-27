//! 角色别名合并（吸收自 AI-Reader-V2 `backend/src/services/alias_resolver.py`，2026-08-15）。
//!
//! 中文小说同一角色常有多称呼（孙悟空 = 美猴王 = 行者 = 齐天大圣；沈棠知 = 沈棠），
//! LLM 逐章抽取会产生分散的别名实体。本模块用 Union-Find 把别名组合并成 canonical 组：
//! - 同一组内所有别名归一为选定的 canonical 名
//! - 亲缘词/泛称/称谓 等**语境性**称呼（妈妈/父亲/哥哥/那怪/群妖）不进 Union-Find 节点，
//!   否则会把毫无关联的角色组桥接起来（「妈妈」在不同章节指不同人）
//! - canonical 选择：组内最短名优先（高频兜底），与源项目一致
//!
//! 纯函数、无 IO、无 LLM 依赖——供 pack 角色清洗/守卫 known 集构建复用。

use std::collections::{BTreeMap, BTreeSet};

/// 语境性称呼（亲缘词/泛称/称谓）。这些词在不同章节/说话人语境下指不同人，
/// 用它们做 Union-Find 键会产生假桥接，合并无关角色组。
/// 吸收自 AI-Reader-V2 `_KINSHIP_TERMS` + `_TITLES` + `_COLLECTIVE_TERMS` 并集。
const CONTEXTUAL_TERMS: &[&str] = &[
    // 直系亲属
    "哥哥", "弟弟", "姐姐", "妹妹", "妈妈", "爸爸", "爸", "妈", "父亲", "母亲", "儿子", "女儿",
    "妻子", "丈夫", "老婆", "老公", "媳妇", "婆婆", "公公", "岳父", "岳母", "丈人", "老丈人",
    "嫂子", "弟媳", "弟媳妇", "姐夫", "妹夫", "爷爷", "奶奶", "外公", "外婆", "祖母", "祖父",
    "孙子", "孙女", "外孙", "外孙女", "侄子", "侄女", "侄儿", "外甥", "女婿", "老伴", "新郎",
    "新娘", "爹", "娘", "双亲", "父母", "兄妹", "兄弟", "姐妹", "弟妹",
    // 称谓/身份（非专名）
    "皇上", "皇后", "太后", "王爷", "公子", "小姐", "夫人", "太太", "老爷", "大人", "少爷",
    "姑娘", "师父", "师傅", "徒弟", "师兄", "师姐", "师弟", "师妹", "长老", "掌门", "阁主",
    "庄主", "城主", "陛下", "殿下", "娘娘", "贵妃", "将军", "军师", "宰相", "尚书", "员外",
    "掌柜", "老板", "老板娘", "大夫", "郎中", "和尚", "道士", "尼姑", "僧人", "主持", "方丈",
    "老板", "教授", "老师", "同学", "班长", "校长", "医生", "护士", "司机", "警察", "保安",
    // 泛称/集体
    "那怪", "群妖", "众妖", "众仙", "众人", "大家", "村民", "百姓", "众人", "手下", "随从",
    "仆从", "侍女", "丫鬟", "家丁", "护卫", "侍卫", "将士", "士兵", "官兵", "贼人", "土匪",
    "路人", "看客", "围观者", "群众", "邻居", "同事", "朋友", "好友", "同学", "室友",
];

/// 高频虚词/句子碎片（沿用 crawl 侧 is_plausible_character_name 同源黑名单，
/// 避免「那就」「冲进下水」这类 LLM 幻觉碎片成为别名节点）。
const FRAGMENT_SUBSTRINGS: &[&str] = &[
    "那就", "另一个", "像是想", "有一种", "里面", "时候", "开始", "已经", "看见", "知道",
    "自己", "什么", "怎么", "一样", "下来", "出去", "回来", "眼前", "身后", "面前", "旁边",
    "突然", "然后", "可是", "但是", "因为", "所以", "如果", "虽然", "不过", "还是", "就是",
    "只是", "并且", "甚至", "大概", "仿佛", "好像", "依然", "终于", "连忙", "赶紧", "立刻",
    "马上", "缓缓", "慢慢", "轻轻", "深深", "紧紧", "冷冷", "淡淡", "微微", "渐渐", "不断",
    "不停", "一直", "再也", "越来越", "纷纷", "粮食", "电车", "下水", "味道", "示意", "尽管",
    "那里", "这里", "起来", "进去", "下去", "上去", "离开", "走近", "抬头", "低头", "转身",
    "回头", "开口", "伸手", "点头", "摇头", "皱眉", "叹气", "微笑", "沉默", "停顿", "犹豫",
];

/// 别名安全度：0=硬拒（语境称呼，绝不能当 UF 节点）、1=可疑（含碎片特征）、2=安全。
pub fn alias_safety_level(alias: &str) -> u8 {
    let a = alias.trim();
    if a.is_empty() {
        return 0;
    }
    if CONTEXTUAL_TERMS.contains(&a) {
        return 0;
    }
    for f in FRAGMENT_SUBSTRINGS {
        if a.contains(f) {
            return 1;
        }
    }
    2
}

/// 判断一个别名是否安全（仅 level 2 安全可参与合并；0 语境称呼 + 1 碎片均拒绝，
/// 碎片如「那就」「冲进下水」是 LLM 幻觉，连叶子都不该进）。
pub fn is_unsafe_alias(alias: &str) -> bool {
    alias_safety_level(alias) != 2
}

#[derive(Debug, Default, Clone)]
struct UnionFind {
    parent: BTreeMap<String, String>,
    size: BTreeMap<String, usize>,
}

impl UnionFind {
    fn find(&mut self, x: &str) -> String {
        if !self.parent.contains_key(x) {
            self.parent.insert(x.to_string(), x.to_string());
            self.size.insert(x.to_string(), 1);
        }
        // 路径压缩
        let mut root = self.parent[x].clone();
        while self.parent[&root] != root {
            root = self.parent[&root].clone();
        }
        self.parent.insert(x.to_string(), root.clone());
        root
    }

    fn union(&mut self, a: &str, b: &str) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        // union by size：小树挂大树
        let sa = *self.size.get(&ra).unwrap_or(&1);
        let sb = *self.size.get(&rb).unwrap_or(&1);
        if sa < sb {
            self.parent.insert(ra.clone(), rb.clone());
            self.size.insert(rb.clone(), sa + sb);
        } else {
            self.parent.insert(rb.clone(), ra.clone());
            self.size.insert(ra.clone(), sa + sb);
        }
    }

    fn groups(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for x in self.parent.keys() {
            let mut root = x.clone();
            while self.parent[&root] != root {
                root = self.parent[&root].clone();
            }
            out.entry(root).or_default().push(x.clone());
        }
        out
    }
}

/// 合并别名组：输入 `pairs` 为「别名 → canonical 候选」的等价声明（如
/// [(美猴王, 孙悟空), (行者, 孙悟空), (齐天大圣, 孙悟空)]），
/// 输出 alias → canonical 映射。语境性称呼（妈妈/父亲）不作为 UF 节点参与合并。
///
/// canonical 选择（吸收自 AI-Reader name_authority "3-char 10x threshold"）：
/// 组内 3 字全名优先（权重 ×10），其次按出现频次，最后字典序——确保
/// 「孙悟空」胜过「行者」「孙行者」（2/4 字降权），避免短称/长称抢 canonical。
pub fn build_alias_map(pairs: &[(String, String)]) -> BTreeMap<String, String> {
    let mut uf = UnionFind::default();
    // 第一遍：收集所有安全节点 + 频次统计
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    let mut freq: BTreeMap<String, usize> = BTreeMap::new();
    for (alias, canon) in pairs {
        for n in [alias, canon] {
            if !is_unsafe_alias(n) {
                nodes.insert(n.clone());
                *freq.entry(n.clone()).or_default() += 1;
            }
        }
    }
    // 第二遍：union 等价对（任一端不安全 → 跳过，防假桥接）
    for (alias, canon) in pairs {
        if is_unsafe_alias(alias) || is_unsafe_alias(canon) {
            continue;
        }
        uf.union(alias, canon);
    }
    // canonical = 3 字全名优先（权重 ×10），其次频次，最后字典序
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    for (_root, members) in uf.groups() {
        let canon = members
            .iter()
            .min_by(|a, b| {
                let wa = if a.as_str().chars().count() == 3 { 1 } else { 0 };
                let wb = if b.as_str().chars().count() == 3 { 1 } else { 0 };
                wb.cmp(&wa)
                    .then_with(|| {
                        freq.get(b.as_str()).unwrap_or(&1).cmp(freq.get(a.as_str()).unwrap_or(&1))
                    })
                    .then_with(|| a.cmp(b))
            })
            .cloned()
            .unwrap_or_default();
        for m in members {
            result.insert(m, canon.clone());
        }
    }
    result
}

/// 归一化一个名字：返回 canonical（若在映射中）或原名。
pub fn resolve_alias(alias_map: &BTreeMap<String, String>, name: &str) -> String {
    alias_map
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn merge_basic_aliases() {
        // 孙悟空 = 美猴王 = 行者 = 齐天大圣（AI-Reader demo 用例）
        let pairs = vec![
            p("美猴王", "孙悟空"),
            p("行者", "孙悟空"),
            p("齐天大圣", "孙悟空"),
            p("孙行者", "孙悟空"),
        ];
        let map = build_alias_map(&pairs);
        assert_eq!(map.get("美猴王").unwrap(), "孙悟空");
        assert_eq!(map.get("行者").unwrap(), "孙悟空");
        assert_eq!(map.get("齐天大圣").unwrap(), "孙悟空");
        assert_eq!(map.get("孙行者").unwrap(), "孙悟空");
        // canonical 自身也入 map（孙悟空→孙悟空），共 5 个 key
        assert_eq!(map.get("孙悟空").unwrap(), "孙悟空");
        assert_eq!(map.len(), 5);
    }

    #[test]
    fn kinship_terms_do_not_bridge_groups() {
        // 「母亲」是语境称呼，绝不能让两个不同家族的角色组桥接
        let pairs = vec![
            p("林母", "林逸的母亲"),
            p("母亲", "林母"),
            p("苏母", "苏婉的母亲"),
            p("母亲", "苏母"),
        ];
        let map = build_alias_map(&pairs);
        // 林母/苏母 各自成组（「母亲」不进 UF 节点，不桥接两组）
        let lin: Vec<&String> = map.iter().filter(|(k, _)| k.as_str() == "林母" || k.as_str() == "林逸的母亲").map(|(_, v)| v).collect();
        assert_eq!(lin.len(), 2);
        assert!(lin.iter().all(|v| v.as_str() == "林母" || v.as_str() == "林逸的母亲"));
        // 两个组 canonical 互不相同
        let lin_canon = map.get("林母").unwrap().clone();
        let su_canon = map.get("苏母").unwrap().clone();
        assert_ne!(lin_canon, su_canon);
        // 母亲 无映射（未成为节点）
        assert!(!map.contains_key("母亲"));
    }

    #[test]
    fn canonical_prefers_shortest() {
        // 3 字全名优先（沈棠知 3 字 > 沈棠 2 字）——吸收 AI-Reader "3-char 10x threshold"
        let pairs = vec![p("沈棠知", "沈棠知"), p("沈棠", "沈棠知")];
        let map = build_alias_map(&pairs);
        assert_eq!(map.get("沈棠").unwrap(), "沈棠知");
        assert_eq!(map.get("沈棠知").unwrap(), "沈棠知");
    }

    #[test]
    fn fragment_names_are_rejected() {
        let pairs = vec![p("那就", "李四"), p("冲进下水", "李四")];
        let map = build_alias_map(&pairs);
        assert!(!map.contains_key("那就"));
        assert!(!map.contains_key("冲进下水"));
    }

    #[test]
    fn unsafe_alias_as_leaf_still_resolves() {
        // 显式声明 (母亲, 苏母)：母亲 不安全不作为节点，但「苏母」仍正常归一
        let pairs = vec![p("母亲", "苏母"), p("苏母", "苏婉的母亲")];
        let map = build_alias_map(&pairs);
        // 苏母 2 字 > 苏婉的母亲 5 字，canonical = 苏母
        assert_eq!(map.get("苏母").unwrap(), "苏母");
        assert_eq!(map.get("苏婉的母亲").unwrap(), "苏母");
        assert!(!map.contains_key("母亲"));
    }

    #[test]
    fn resolve_unknown_returns_original() {
        let map = build_alias_map(&[p("美猴王", "孙悟空")]);
        assert_eq!(resolve_alias(&map, "猪八戒"), "猪八戒");
        assert_eq!(resolve_alias(&map, "美猴王"), "孙悟空");
    }
}
