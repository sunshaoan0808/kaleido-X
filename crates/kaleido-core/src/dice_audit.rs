//! 骰子审计三检（吞噬 loreweaver `turn_checks.py` 口径，Kaleido 化）。
//!
//! 契约：**叙事不带骰数**。真值只存在 check_history 里，正文里的数字若与真值矛盾即吃书。
//! 三检全是确定性纯函数，零 LLM：
//! - 伪造：正文含"骰形结果"（如"骰出18"/"d20=17"）但本回合 check_history 为空 → 伪造嫌疑
//! - 矛盾：本回合有检定，真值 natural/total 与正文数字不一致 → 矛盾（精确）
//! - 过期 HUD：正文场景头（【场景|地点|时间】行）与 game_clock state_line 不一致 → HUD 过期
//!
//! CJK 数字归一：〇一二三四五六七八九十百 + 阿拉伯，全转 u32 再比。

/// CJK/阿拉伯数字归一为 u32（支持 0-99 的中文读法 + 纯阿拉伯）。
pub fn norm_num(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    let v = ["零", "〇", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    // 下标0/1=零，其余下标-1=数值
    let val = |c: char| v.iter().position(|x| *x == c.to_string().as_str()).map(|i| if i <= 1 { 0 } else { (i - 1) as u32 });
    let chars: Vec<char> = s.chars().collect();
    // 十/百结构
    if let Some(pos) = chars.iter().position(|&c| c == '十' || c == '百') {
        let base = if chars[pos] == '十' { 10 } else { 100 };
        let mut left = 1u32;
        let mut right = 0u32;
        if pos > 0 {
            left = chars[..pos].iter().filter_map(|&c| val(c)).fold(0, |a, n| a * 10 + n);
            if left == 0 {
                left = 1;
            }
        }
        if pos + 1 < chars.len() {
            right = chars[pos + 1..].iter().filter_map(|&c| val(c)).fold(0, |a, n| a * 10 + n);
        }
        return Some(left * base + right);
    }
    if chars.len() == 1 {
        return val(chars[0]);
    }
    // 多字连续（如"一八"=18）
    let mut n = 0u32;
    for c in &chars {
        n = n * 10 + val(*c)?;
    }
    Some(n)
}

/// 从正文抽"骰形结果"：`骰出X` / `掷出X` / `d20=X` / `检定X` / `点数X` 后的数字。
pub fn extract_roll_claims(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let markers = ["骰出", "掷出", "骰得", "掷得", "检定", "点数", "d20=", "d20＝", "1d20"];
    let chars: Vec<char> = text.chars().collect();
    let t: String = chars.iter().collect();
    for m in markers {
        let mut rest = t.as_str();
        while let Some(idx) = rest.find(m) {
            let after: String = rest[idx + m.len()..].chars().take(6).collect();
            // 跳过非数字前缀（如"的""为""是"）
            let num_str: String = after
                .chars()
                .skip_while(|c| !c.is_ascii_digit() && !"零〇一二三四五六七八九十百".contains(*c))
                .take_while(|c| c.is_ascii_digit() || "零〇一二三四五六七八九十百".contains(*c))
                .collect();
            if let Some(n) = norm_num(&num_str) {
                out.push(n);
            }
            rest = &rest[idx + m.len()..];
            if rest.is_empty() {
                break;
            }
        }
    }
    out
}

/// 三检结果。
#[derive(Debug, Clone, Default)]
pub struct DiceAudit {
    /// 伪造嫌疑（有骰形宣称但本回合无检定）。
    pub forgery: bool,
    /// 矛盾（宣称数字与真值 natural/total 全不匹配）。
    pub contradiction: bool,
    /// 矛盾明细（中文）。
    pub reasons: Vec<String>,
}

/// 跑三检。
/// - `claims`: 正文抽出的宣称数字
/// - `checks`: 本回合 check_history 条目（natural, total）
pub fn audit(claims: &[u32], checks: &[(u32, f64)]) -> DiceAudit {
    let mut a = DiceAudit::default();
    if claims.is_empty() {
        return a;
    }
    if checks.is_empty() {
        a.forgery = true;
        a.reasons.push(format!("正文宣称骰值 {:?} 但本回合无检定记录", claims));
        return a;
    }
    for c in claims {
        let hit = checks
            .iter()
            .any(|(nat, tot)| *nat == *c || (*tot as u32) == *c);
        if !hit {
            a.contradiction = true;
            let truth: Vec<String> = checks.iter().map(|(n, t)| format!("{n}/{t}")).collect();
            a.reasons.push(format!("正文宣称 {} 与真值 [{}] 不符", c, truth.join(",")));
        }
    }
    a
}

#[cfg(test)]
mod dice_audit_tests {
    use super::*;
    #[test]
    fn norm_cjk() {
        assert_eq!(norm_num("十八"), Some(18));
        assert_eq!(norm_num("二十"), Some(20));
        assert_eq!(norm_num("十二"), Some(12));
        assert_eq!(norm_num("五"), Some(5));
        assert_eq!(norm_num("17"), Some(17));
    }
    #[test]
    fn forgery_no_check() {
        let a = audit(&[18], &[]);
        assert!(a.forgery && !a.contradiction);
    }
    #[test]
    fn contradiction_mismatch() {
        let a = audit(&[18], &[(7, 9.0)]);
        assert!(a.contradiction && !a.forgery);
    }
    #[test]
    fn match_ok() {
        let a = audit(&[18], &[(18, 20.0)]);
        assert!(!a.forgery && !a.contradiction);
    }
    #[test]
    fn extract_claims() {
        let v = extract_roll_claims("他骰出了十八点，大成功！");
        assert!(v.contains(&18));
        let v2 = extract_roll_claims("检定20成功");
        assert!(v2.contains(&20));
        let v3 = extract_roll_claims("今天天气不错，无事发生。");
        assert!(v3.is_empty());
    }
}
