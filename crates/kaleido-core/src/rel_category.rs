//! rel→graph category 映射：蒸馏自由关系词 → graph_store 五类。
//!
//! graph_store::VALID_CATEGORIES 硬校验 [family, social, emotional, conflict, uncertain]，
//! 蒸馏 prompt 产出的是自由词（母子/暗恋/师兄弟/情敌/同事…），直接写库会
//! InvalidCategory 报错。本模块做一次单向映射，未命中兜底 uncertain。
//!
//! 匹配顺序（先 conflict 后 social 后 family 后 emotional）：
//! - "情敌" 必须抢在 emotional 之前（否则落入 uncertain）；
//! - "师兄弟/师姐妹" 必须抢在 family 的 "兄弟" 之前（否则血缘误判）；
//! - "兄弟" 单独出现默认血缘（结拜/结义另由 social 词表覆盖）。

const CONFLICT_TERMS: &[&str] = &[
    "情敌", "仇敌", "死敌", "宿敌", "死对头", "敌人", "对手", "敌对", "仇人", "仇家",
    "冤家", "陷害", "背叛", "利用", "欺骗", "报复", "仇恨", "宿怨", "世仇", "打压",
    "排挤", "针对", "嫌隙", "水火不容",
];

const SOCIAL_TERMS: &[&str] = &[
    "师兄弟", "师姐妹", "师徒", "师父", "师傅", "徒弟", "弟子", "学生", "老师", "导师",
    "上下级", "上司", "下属", "领导", "同事", "同学", "好友", "朋友", "闺蜜", "死党",
    "结拜", "结义", "主仆", "主人", "仆人", "奴仆", "雇主", "雇员", "邻居", "同门",
    "队友", "搭档", "战友", "笔友", "网友", "同僚", "同行", "合作", "同盟", "盟友",
    "合伙人", "恩人", "救命恩人", "知己", "知音", "至交", "故交", "旧识", "熟人",
    "竹马", "青梅竹马", "发小",
];

const FAMILY_TERMS: &[&str] = &[
    "母子", "母女", "父子", "父女", "姐弟", "姐妹", "兄弟", "兄妹", "祖孙", "婆媳",
    "翁婿", "叔侄", "姑侄", "舅甥", "亲属", "家人", "母亲", "父亲", "妈妈", "爸爸",
    "爹", "娘", "儿子", "女儿", "哥哥", "弟弟", "姐姐", "妹妹", "爷爷", "奶奶",
    "外公", "外婆", "姥姥", "姥爷", "叔叔", "婶婶", "舅舅", "舅妈", "姑姑", "姑父",
    "阿姨", "姨夫", "表哥", "表姐", "堂哥", "堂姐", "一家", "家族", "养父", "养母",
    "继父", "继母", "兄长", "家姐", "爹娘", "爹妈", "双亲", "娘亲",
];

const EMOTIONAL_TERMS: &[&str] = &[
    "恋人", "情侣", "暗恋", "初恋", "暧昧", "未婚妻", "未婚夫", "前女友", "前男友",
    "前任", "爱人", "情人", "心上人", "心动", "爱慕", "追求", "表白", "恋爱", "热恋",
    "结婚", "婚姻", "夫妻", "伴侣", "配偶", "对象", "男友", "女友", "红颜", "蓝颜",
    "喜欢", "好感",
];

/// 将蒸馏产出的自由关系词映射到 graph_store 五类；未命中 → "uncertain"。
pub fn normalize_rel_category(rel: &str) -> &'static str {
    let r = rel.trim();
    if r.is_empty() {
        return "uncertain";
    }
    for t in CONFLICT_TERMS {
        if r.contains(t) {
            return "conflict";
        }
    }
    for t in SOCIAL_TERMS {
        if r.contains(t) {
            return "social";
        }
    }
    for t in FAMILY_TERMS {
        if r.contains(t) {
            return "family";
        }
    }
    for t in EMOTIONAL_TERMS {
        if r.contains(t) {
            return "emotional";
        }
    }
    "uncertain"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_family() {
        assert_eq!(normalize_rel_category("母子"), "family");
        assert_eq!(normalize_rel_category("父亲"), "family");
        assert_eq!(normalize_rel_category("兄弟姐妹"), "family");
        assert_eq!(normalize_rel_category("兄妹"), "family");
        assert_eq!(normalize_rel_category("家人"), "family");
    }

    #[test]
    fn maps_emotional() {
        assert_eq!(normalize_rel_category("暗恋"), "emotional");
        assert_eq!(normalize_rel_category("初恋"), "emotional");
        assert_eq!(normalize_rel_category("前任恋人"), "emotional");
        assert_eq!(normalize_rel_category("夫妻"), "emotional");
        assert_eq!(normalize_rel_category("前女友"), "emotional");
    }

    #[test]
    fn maps_social() {
        assert_eq!(normalize_rel_category("师兄弟"), "social");
        assert_eq!(normalize_rel_category("师徒"), "social");
        assert_eq!(normalize_rel_category("同事"), "social");
        assert_eq!(normalize_rel_category("好友"), "social");
        assert_eq!(normalize_rel_category("结拜兄弟"), "social");
        // 纯血缘兄弟仍归 family（social 表无裸"兄弟"）
        assert_eq!(normalize_rel_category("兄弟"), "family");
    }

    #[test]
    fn maps_conflict() {
        assert_eq!(normalize_rel_category("情敌"), "conflict");
        assert_eq!(normalize_rel_category("仇敌"), "conflict");
        assert_eq!(normalize_rel_category("死对头"), "conflict");
        assert_eq!(normalize_rel_category("敌对"), "conflict");
    }

    #[test]
    fn maps_uncertain() {
        assert_eq!(normalize_rel_category("陌生人"), "uncertain");
        assert_eq!(normalize_rel_category(""), "uncertain");
        assert_eq!(normalize_rel_category("   "), "uncertain");
        assert_eq!(normalize_rel_category("缘分"), "uncertain");
    }
}
