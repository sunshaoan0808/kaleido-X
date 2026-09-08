//! 幻觉岛过滤 + 六维冲突检测 + 关系自检（吸收自 AI-Reader-V2，MIT）。
//!
//! - `grounded_in`：原文 grounding 校验（hallucination_filter.py 口径：
//!   提取名必须在原文出现，否则判幻觉；短名<2字跳过防误杀）。
//! - `Conflict`：ability/relation/death 三维（conflict_detector.py 口径压缩版：
//!   能力回退 A→B→A / 关系 flip-flop + 敌对↔亲属 / 死亡后复活）。
//! - `self_ref_ok`：关系自检（profile_quality_checker.py 口径：自引用拒绝）。

use serde::{Deserialize, Serialize};

/// grounding：名是否在原文出现（别名展开后任一命中即算）。
pub fn grounded_in(name: &str, aliases: &[String], corpus: &str) -> bool {
    if name.chars().count() < 2 {
        return true; // 短名不可靠，保留（原版 MIN_VERIFIABLE_LEN=2）
    }
    if corpus.contains(name) {
        return true;
    }
    aliases.iter().any(|a| !a.is_empty() && corpus.contains(a))
}

/// 批量找未 grounding 名（返回幻觉候选）。
pub fn find_ungrounded(names: &[(String, Vec<String>)], corpus: &str) -> Vec<String> {
    names
        .iter()
        .filter(|(n, a)| !grounded_in(n, a, corpus))
        .map(|(n, _)| n.clone())
        .collect()
}

/// 冲突类型（AI-Reader-V2 六维的子集：先上 ability/relation/death）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictKind {
    Ability,
    Relation,
    Death,
}

/// 一条冲突。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub kind: ConflictKind,
    /// high=严重/med=一般/low=提示
    pub severity: &'static str,
    pub description: String,
    pub entity: String,
    pub turns: Vec<i64>,
}

/// 能力时间线：角色→维度→[(turn, 值)]，检 A→B→A 回退。
pub fn check_ability_regress(
    timelines: &[(String, String, Vec<(i64, String)>)],
) -> Vec<Conflict> {
    let mut out = Vec::new();
    for (cname, dim, tl) in timelines {
        if tl.len() < 3 {
            continue;
        }
        for i in 2..tl.len() {
            let (t_prev, v_prev) = &tl[i - 1];
            let (t_cur, v_cur) = &tl[i];
            for j in (0..i - 1).rev() {
                let (t_early, v_early) = &tl[j];
                if v_cur == v_early && v_prev != v_cur {
                    out.push(Conflict {
                        kind: ConflictKind::Ability,
                        severity: "med",
                        description: format!(
                            "{cname} 的{dim}从「{v_early}」(t{t_early})变为「{v_prev}」(t{t_prev})又回到「{v_cur}」(t{t_cur})，疑似回退"
                        ),
                        entity: cname.clone(),
                        turns: vec![*t_early, *t_prev, *t_cur],
                    });
                    break;
                }
            }
        }
    }
    out
}

const HOSTILE: &[&str] = &["敌对", "仇人", "对手", "仇敌"];
const FAMILY: &[&str] = &["亲属", "父子", "母子", "兄弟", "姐妹", "夫妻", "父女", "母女"];

fn has_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// 关系时间线：(a,b)→[(turn, 类型)]，检敌对↔亲属 + flip-flop。
pub fn check_relation_flip(
    timelines: &[((String, String), Vec<(i64, String)>)],
) -> Vec<Conflict> {
    let mut out = Vec::new();
    for ((pa, pb), tl) in timelines {
        if tl.len() < 2 {
            continue;
        }
        for i in 1..tl.len() {
            let (t0, v0) = &tl[i - 1];
            let (t1, v1) = &tl[i];
            let v0h = has_any(v0, HOSTILE);
            let v1f = has_any(v1, FAMILY);
            let v0f = has_any(v0, FAMILY);
            let v1h = has_any(v1, HOSTILE);
            if v0h && v1f {
                out.push(Conflict {
                    kind: ConflictKind::Relation,
                    severity: "med",
                    description: format!(
                        "{pa}与{pb}的关系从「{v0}」(t{t0})变为「{v1}」(t{t1})，敌对→亲属转变异常"
                    ),
                    entity: pa.clone(),
                    turns: vec![*t0, *t1],
                });
            } else if v0f && v1h {
                out.push(Conflict {
                    kind: ConflictKind::Relation,
                    severity: "low",
                    description: format!(
                        "{pa}与{pb}的关系从「{v0}」(t{t0})变为「{v1}」(t{t1})，亲属→敌对（可能是叛变剧情）"
                    ),
                    entity: pa.clone(),
                    turns: vec![*t0, *t1],
                });
            }
        }
        if tl.len() >= 3 {
            for i in 2..tl.len() {
                let (t0, v0) = &tl[i - 2];
                let (t1, v1) = &tl[i - 1];
                let (t2, v2) = &tl[i];
                if v0 == v2 && v1 != v0 {
                    out.push(Conflict {
                        kind: ConflictKind::Relation,
                        severity: "low",
                        description: format!(
                            "{pa}与{pb}的关系反复：「{v0}」→「{v1}」→「{v2}」(t{t0}/t{t1}/t{t2})"
                        ),
                        entity: pa.clone(),
                        turns: vec![*t0, *t1, *t2],
                    });
                }
            }
        }
    }
    out
}

/// 死亡连续性：death_turn 后仍登场 → high。
pub fn check_death_reappear(deaths: &[(String, i64)], appearances: &[(String, i64)]) -> Vec<Conflict> {
    let mut out = Vec::new();
    for (name, dt) in deaths {
        for (who, at) in appearances {
            if who == name && at > dt {
                out.push(Conflict {
                    kind: ConflictKind::Death,
                    severity: "high",
                    description: format!("角色「{name}」在 t{dt} 阵亡，但在 t{at} 再次出现"),
                    entity: name.clone(),
                    turns: vec![*dt, *at],
                });
            }
        }
    }
    out
}

/// 关系自检：自引用拒绝（alias 解析后 source==target）。
pub fn self_ref_ok(character: &str, target: &str, aliases: &[(String, String)]) -> bool {
    fn canon<'a>(n: &'a str, aliases: &'a [(String, String)]) -> &'a str {
        aliases
            .iter()
            .find(|(a, _)| a == n)
            .map(|(_, c)| c.as_str())
            .unwrap_or(n)
    }
    canon(character, aliases) != canon(target, aliases) && character != target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounding_short_name_kept() {
        assert!(grounded_in("阿", &[], "雨巷"));
        assert!(grounded_in("沈棠", &[], "沈棠站在雨中"));
        assert!(!grounded_in("韩立", &[], "沈棠站在雨中"));
        assert!(grounded_in("韩立", &["韩老魔".into()], "韩老魔来了"));
        // 别名展开命中
        assert!(find_ungrounded(
            &[("韩立".into(), vec![]), ("沈棠".into(), vec![])],
            "沈棠站在雨中"
        ) == vec!["韩立".to_string()]);
    }

    #[test]
    fn ability_regress_detected() {
        let tl = vec![("沈棠".into(), "修为".into(), vec![
            (1, "炼气".into()),
            (5, "筑基".into()),
            (9, "炼气".into()),
        ])];
        let c = check_ability_regress(&tl);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind, ConflictKind::Ability);
        // 正常递进不报
        let tl2 = vec![("沈棠".into(), "修为".into(), vec![(1, "炼气".into()), (5, "筑基".into())])];
        assert!(check_ability_regress(&tl2).is_empty());
    }

    #[test]
    fn relation_flip_detected() {
        let tl = vec![(
            ("沈棠".into(), "林晚".into()),
            vec![(1, "敌对".into()), (5, "姐妹".into())],
        )];
        let c = check_relation_flip(&tl);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].severity, "med");
        // flip-flop
        let tl2 = vec![(
            ("A".into(), "B".into()),
            vec![(1, "朋友".into()), (2, "敌对".into()), (3, "朋友".into())],
        )];
        let c2 = check_relation_flip(&tl2);
        assert!(c2.iter().any(|x| x.description.contains("反复")));
    }

    #[test]
    fn death_reappear_high() {
        let c = check_death_reappear(&[("白梅".into(), 10)], &[("白梅".into(), 20)]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].severity, "high");
        assert!(check_death_reappear(&[("白梅".into(), 10)], &[("白梅".into(), 5)]).is_empty());
    }

    #[test]
    fn self_ref_rejected() {
        let aliases = vec![("阿沅".into(), "沈棠".into())];
        assert!(!self_ref_ok("沈棠", "阿沅", &aliases)); // 别名后自指
        assert!(!self_ref_ok("沈棠", "沈棠", &[]));
        assert!(self_ref_ok("沈棠", "林晚", &aliases));
    }
}
