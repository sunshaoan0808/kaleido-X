//! 读者速读行为分析（X1b）
//! 吞噬自 xiami skimming.rs（虾米读者速读行为模型）。
//! 纯函数：五类阅读阻力检测 + 审查/修复合同文案，无 LLM / IO。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ReaderPlatform {
    #[default]
    Tomato,
    Qidian,
    Jjwxc,
    General,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ReaderProfile {
    #[default]
    FastScan,
    NormalSerial,
    DeepRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ReviewStrictness {
    #[default]
    Relaxed,
    Balanced,
    Strict,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReaderSkimmingConfig {
    pub enabled: bool,
    pub primary_reader: ReaderProfile,
    pub platform: ReaderPlatform,
    pub strictness: ReviewStrictness,
    pub participate_in_gate: bool,
    pub custom_prompt: String,
}

impl Default for ReaderSkimmingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            primary_reader: ReaderProfile::NormalSerial,
            platform: ReaderPlatform::General,
            strictness: ReviewStrictness::Balanced,
            participate_in_gate: true,
            custom_prompt: String::new(),
        }
    }
}

/// 单条速读风险问题。severity 复用 Kaleido 惯例 u8：1=P1、2=P2、3=P3；P0 不在此模块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkimIssue {
    pub severity: u8,
    pub category: String,
    pub message: String,
    pub evidence: String,
    pub fix: String,
}

/// 五类速读风险检测：
/// 1. 连续超短段（阈值按 profile：22/18/14；连续数上限按 strictness：10/7/5）
/// 2. 文字墙（平台阈值：番茄 420 / 起点·晋江 560 / 通用·自定义 500）
/// 3. 连续纯对白 ≥ 6 段（“”/「」/『』 包裹判定）
/// 4. 重复句首（≥12 字段取前 4 字符，同前缀 ≥ 3）
/// 参与门禁（participate_in_gate）时给 P1，否则一律降为 P2。
pub fn analyze_skimming(content: &str, config: &ReaderSkimmingConfig) -> Vec<SkimIssue> {
    if !config.enabled || content.trim().is_empty() {
        return Vec::new();
    }
    let paragraphs = content
        .split('\n')
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>();
    let mut issues = Vec::new();
    let short_threshold = match config.primary_reader {
        ReaderProfile::FastScan => 22,
        ReaderProfile::NormalSerial => 18,
        ReaderProfile::DeepRead => 14,
    };
    let long_threshold = match config.platform {
        ReaderPlatform::Tomato => 420,
        ReaderPlatform::Qidian | ReaderPlatform::Jjwxc => 560,
        ReaderPlatform::General | ReaderPlatform::Custom => 500,
    };

    let short_runs = longest_run(&paragraphs, |paragraph| {
        paragraph.chars().count() <= short_threshold
    });
    let short_limit = match config.strictness {
        ReviewStrictness::Relaxed => 10,
        ReviewStrictness::Balanced => 7,
        ReviewStrictness::Strict => 5,
    };
    if short_runs >= short_limit {
        issues.push(issue(
            severity(config, 1),
            "skimming_short_paragraph_run",
            &format!("连续 {short_runs} 个超短段落，手机端呈现碎片化"),
            "大量一两句单独成段会制造机械断句和 AI 排版感",
            "按同一动作、感受或交流回合合并段落，保留真正需要停顿的短段",
        ));
    }

    let oversized = paragraphs
        .iter()
        .filter(|paragraph| paragraph.chars().count() > long_threshold)
        .count();
    if oversized > 0 {
        issues.push(issue(
            severity(config, 1),
            "skimming_wall_of_text",
            &format!("发现 {oversized} 个超过 {long_threshold} 字的密集段落"),
            "手机端缺少视觉换气点，读者难以定位动作和信息变化",
            "在动作转向、说话者变化或认知转折处自然分段，不要切成句句独占一行",
        ));
    }

    let dialogue_run = longest_run(&paragraphs, is_dialogue_only);
    if dialogue_run >= 6 {
        issues.push(issue(
            severity(config, 1),
            "skimming_unclear_dialogue",
            &format!("连续 {dialogue_run} 段仅有对白，存在说话者追溯风险"),
            "连续问答缺少动作、称呼、关系压力或场景锚点",
            "仅在可能混淆处补入说话者动作和潜台词，不要每句都机械标注姓名",
        ));
    }

    let repeated_openings = repeated_paragraph_openings(&paragraphs);
    if repeated_openings >= 3 {
        issues.push(issue(
            severity(config, 2),
            "skimming_repeated_opening",
            &format!("至少 {repeated_openings} 个段落使用相同句首结构"),
            "相同主语或连接词连续开段会产生流水账节奏",
            "改变信息出现顺序，让动作、感官、判断和对话根据场景自然轮换",
        ));
    }
    issues
}

/// 读者速读行为审查合同（中文）
pub fn render_skimming_review_contract(config: &ReaderSkimmingConfig) -> String {
    if !config.enabled {
        return String::new();
    }
    let custom = optional_custom_prompt(&config.custom_prompt);
    format!(
        "# 读者速读行为审查\n以{}平台、{}读者、{}强度模拟阅读。检查信息平台期、无效描写、重复心理、对白归属不清、无效问答、重复解释、连续碎段、文字墙，以及长时间没有选择/情绪/关系/状态变化。不要只为极快读者优化；有效心理、后果余味、世界规则建立、安静关系场景和必要铺垫均应保留。先让段落承担动作、判断、阻力或关系功能，再考虑移动、合并或删去重复，不能把章节压缩成梗概。{}",
        platform_label(config.platform),
        reader_label(config.primary_reader),
        strictness_label(config.strictness),
        custom,
    )
}

/// 速读风险定向修复合同（中文；禁压成摘要）
pub fn render_skimming_repair_contract(config: &ReaderSkimmingConfig) -> String {
    if !config.enabled {
        return String::new();
    }
    format!(
        "# 速读风险定向修复\n只修复列明的阅读阻力：优先调整顺序、补人物判断或阻力、澄清对白归属、合并重复；保留有效心理、场景气氛、情绪余波、关系停顿和必要铺垫。严禁删成摘要、梗概或剧情记账，严禁改变事实、视角、角色立场、因果结果、伏笔含义和世界规则。{}",
        optional_custom_prompt(&config.custom_prompt)
    )
}

/// 参与门禁才保留 blocking 严重度，否则一律降为 P2（2）
fn severity(config: &ReaderSkimmingConfig, blocking: u8) -> u8 {
    if config.participate_in_gate {
        blocking
    } else {
        2
    }
}

fn longest_run<F>(paragraphs: &[&str], predicate: F) -> usize
where
    F: Fn(&&str) -> bool,
{
    let mut longest = 0;
    let mut current = 0;
    for paragraph in paragraphs {
        if predicate(paragraph) {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// 纯对白判定：整段被中文引号包裹
fn is_dialogue_only(paragraph: &&str) -> bool {
    let value = paragraph.trim();
    (value.starts_with('“') && value.ends_with('”'))
        || (value.starts_with('「') && value.ends_with('」'))
        || (value.starts_with('『') && value.ends_with('』'))
}

/// 统计 ≥12 字段落中最常见的 4 字符句首出现次数
fn repeated_paragraph_openings(paragraphs: &[&str]) -> usize {
    let mut counts = HashMap::new();
    for paragraph in paragraphs.iter().filter(|item| item.chars().count() >= 12) {
        let prefix = paragraph.chars().take(4).collect::<String>();
        *counts.entry(prefix).or_insert(0usize) += 1;
    }
    counts.values().copied().max().unwrap_or(0)
}

fn optional_custom_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("\n# 用户自定义速读审查要求\n{trimmed}")
    }
}

fn issue(
    severity: u8,
    category: &str,
    message: &str,
    evidence: &str,
    fix: &str,
) -> SkimIssue {
    SkimIssue {
        severity,
        category: category.to_owned(),
        message: message.to_owned(),
        evidence: evidence.to_owned(),
        fix: fix.to_owned(),
    }
}

fn platform_label(value: ReaderPlatform) -> &'static str {
    match value {
        ReaderPlatform::Tomato => "番茄",
        ReaderPlatform::Qidian => "起点",
        ReaderPlatform::Jjwxc => "晋江",
        ReaderPlatform::General => "通用",
        ReaderPlatform::Custom => "自定义",
    }
}

fn reader_label(value: ReaderProfile) -> &'static str {
    match value {
        ReaderProfile::FastScan => "快速扫读",
        ReaderProfile::NormalSerial => "普通追更",
        ReaderProfile::DeepRead => "沉浸深读",
    }
}

fn strictness_label(value: ReviewStrictness) -> &'static str {
    match value {
        ReviewStrictness::Relaxed => "宽松",
        ReviewStrictness::Balanced => "平衡",
        ReviewStrictness::Strict => "严格",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_module_does_not_analyze_or_inject() {
        let config = ReaderSkimmingConfig {
            enabled: false,
            ..ReaderSkimmingConfig::default()
        };
        assert!(analyze_skimming("门开了。\n人走了。", &config).is_empty());
        assert!(render_skimming_review_contract(&config).is_empty());
        assert!(render_skimming_repair_contract(&config).is_empty());
    }

    #[test]
    fn warning_only_mode_does_not_create_blocking_issues() {
        let config = ReaderSkimmingConfig {
            participate_in_gate: false,
            ..ReaderSkimmingConfig::default()
        };
        let content = (0..9).map(|_| "门开了。").collect::<Vec<_>>().join("\n");
        let issues = analyze_skimming(&content, &config);
        assert!(!issues.is_empty());
        assert!(issues.iter().all(|issue| issue.severity == 2));
    }

    #[test]
    fn detects_all_five_skimming_categories() {
        let wall =
            "夜风穿过巷口，卷起地上的纸屑，他压低帽檐快步前行，经过亮灯的便利店时停顿了一下，确认身后没有尾巴，才转进左手边的窄巷。"
                .repeat(10);
        let content = [
            "他推开门，看见屋里一片狼藉。",
            "他推开门，房间空无一人。",
            "他推开门，桌上放着一封信。",
            "“你是来找这个的吗？”",
            "“这里只有你一个人来过。”",
            "“那封信不是给现任的。”",
            "“你最好现在离开。”",
            "“我不会走的，除非告诉我真相。”",
            "“真相会害死你。”",
            wall.as_str(),
        ]
        .join("\n");
        let config = ReaderSkimmingConfig {
            primary_reader: ReaderProfile::FastScan,
            platform: ReaderPlatform::General,
            strictness: ReviewStrictness::Strict,
            ..ReaderSkimmingConfig::default()
        };
        let issues = analyze_skimming(&content, &config);
        let categories = issues
            .iter()
            .map(|issue| issue.category.as_str())
            .collect::<Vec<_>>();
        assert!(categories.contains(&"skimming_short_paragraph_run"));
        assert!(categories.contains(&"skimming_wall_of_text"));
        assert!(categories.contains(&"skimming_unclear_dialogue"));
        assert!(categories.contains(&"skimming_repeated_opening"));
    }

    #[test]
    fn contracts_carry_key_constraints() {
        let config = ReaderSkimmingConfig::default();
        let review = render_skimming_review_contract(&config);
        assert!(review.contains("有效心理"));
        assert!(review.contains("梗概"));
        let repair = render_skimming_repair_contract(&config);
        assert!(repair.contains("严禁删成摘要"));
        assert!(repair.contains("保留有效心理"));
        assert!(repair.contains("严禁改变事实"));
    }

    #[test]
    fn custom_prompt_is_appended_when_present() {
        let config = ReaderSkimmingConfig {
            custom_prompt: "重点检查打斗场景的节奏".to_owned(),
            ..ReaderSkimmingConfig::default()
        };
        let review = render_skimming_review_contract(&config);
        assert!(review.contains("用户自定义速读审查要求"));
        assert!(review.contains("重点检查打斗场景的节奏"));
    }
}
