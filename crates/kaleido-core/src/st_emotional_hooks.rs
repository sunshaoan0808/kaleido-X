//! 情绪钩子排布引擎（X1c）
//! 吞噬自 xiami emotional_hooks.rs（虾米情绪钩子排布引擎）。
//! 纯函数：重复牵引信号检测 + 排布/执行/结算三份中文合同，无 LLM / IO。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum HookStatus {
    #[default]
    New,
    Active,
    Delayed,
    Resolved,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EmotionalHookDensity {
    #[default]
    Relaxed,
    Balanced,
    Tight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EmotionalHookPreset {
    #[default]
    Tomato,
    Qidian,
    Jjwxc,
    General,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EmotionalHookConfig {
    pub enabled: bool,
    pub density: EmotionalHookDensity,
    pub preset: EmotionalHookPreset,
    pub max_active_hooks: usize,
    pub detect_repetition: bool,
    pub warn_overdue_hooks: bool,
    pub allow_quiet_aftermath: bool,
    pub allow_plan_adjustment: bool,
    pub custom_prompt: String,
}

impl Default for EmotionalHookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            density: EmotionalHookDensity::Balanced,
            preset: EmotionalHookPreset::General,
            max_active_hooks: 4,
            detect_repetition: true,
            warn_overdue_hooks: true,
            allow_quiet_aftermath: true,
            allow_plan_adjustment: true,
            custom_prompt: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct HookRecord {
    pub id: String,
    pub title: String,
    pub reader_promise: String,
    pub progress_notes: String,
    pub setup_turn: u64,
    pub current_status: HookStatus,
    pub resolution_plan: String,
    pub risk: String,
}

/// Kaleido 无 PlotEntry；用轻量结构代替（吞噬自 xiami plot_log 条目的对应字段）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PlotSignalSample {
    pub ending_state: String,
    pub summary: String,
    pub hook_changes: Vec<String>,
}

/// 12 种同构章节结尾牵引信号
const REPETITIVE_ENDING_SIGNALS: [&str; 12] = [
    "敲门",
    "门外",
    "来电",
    "文件",
    "档案",
    "监控",
    "编号",
    "序列号",
    "神秘人",
    "陌生人",
    "脚步声",
    "黑屏",
];

/// 检测最近 6 条 plot 记录中是否反复出现同构结尾信号。
/// 某信号出现 ≥2 次 → 返回该信号；无则返回 `None`。
pub fn repeated_recent_hook_signal(recent_plot_entries: &[PlotSignalSample]) -> Option<&'static str> {
    let recent = recent_plot_entries.iter().rev().take(6).collect::<Vec<_>>();
    let mut counts = HashMap::new();
    for signal in REPETITIVE_ENDING_SIGNALS {
        let count = recent
            .iter()
            .filter(|entry| {
                entry.ending_state.contains(signal)
                    || entry.summary.contains(signal)
                    || entry.hook_changes.iter().any(|item| item.contains(signal))
            })
            .count();
        counts.insert(signal, count);
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .max_by_key(|(_, count)| *count)
        .map(|(signal, _)| signal)
}

/// 情绪钩子排布合同（中文）
pub fn render_hook_planning_contract(
    config: &EmotionalHookConfig,
    active_hooks: usize,
    repetition: Option<&'static str>,
) -> String {
    if !config.enabled {
        return String::new();
    }
    let repetition_note = repetition
        .map(|signal| format!("最近章节反复使用“{signal}”类牵引，后续避免再次照搬。"))
        .unwrap_or_default();
    let custom = optional_custom_prompt(&config.custom_prompt);
    format!(
        "# 情绪钩子排布合同\n密度：{}；平台预设：{}；当前活跃剧情伏笔：{active_hooks}/{}。\n每章只确定一个主情绪钩子，可有 0—2 个次钩子；优先推进既有承诺。钩子必须来自人物欲望、选择、代价、关系或既有因果，不能靠凭空来信、敲门、文件或神秘人强拉悬念。规划时明确：入章读者问题、情绪蓄压、人物选择、情绪转折、余震、离章读者问题。{}{}{}\n情绪钩子不是剧情伏笔的同义词，只有涉及未来事实承诺时才进入伏笔账本；Agent 对话只是作者引导，不得直接登记为正史。",
        density_label(config.density),
        preset_label(config.preset),
        config.max_active_hooks,
        quiet_rule(config),
        plan_adjustment_rule(config),
        join_optional(&repetition_note, &custom),
    )
}

/// 本章情绪钩子执行约束（中文）
pub fn render_hook_execution_contract(config: &EmotionalHookConfig) -> String {
    if !config.enabled {
        return String::new();
    }
    let custom = optional_custom_prompt(&config.custom_prompt);
    format!(
        "# 本章情绪钩子执行约束\n正文要让读者经历‘期待—阻力—选择—后果—余震’，而不是按时间记账。一个主情绪钩子贯穿本章，次钩子最多 2 个；用人物判断、动作、潜台词和关系压力承载情绪，不要只宣告情绪。结尾牵引应是本章选择自然造成的新问题，不强制每章反转，也不得把正文压缩成梗概。{}{}",
        quiet_rule(config),
        custom,
    )
}

/// 情绪钩子结算合同（中文）
pub fn render_hook_ledger_contract(config: &EmotionalHookConfig) -> String {
    if !config.enabled {
        return String::new();
    }
    format!(
        "# 情绪钩子结算\n只结算最终正文真实发生的情绪承诺、人物选择、关系变化与余震。不要把一般情绪波动批量新增为剧情伏笔。{}{}",
        if config.warn_overdue_hooks {
            "若既有读者承诺长期未推进，在 continuityRisks 中记录提醒，不得虚构推进。"
        } else {
            ""
        },
        if config.allow_plan_adjustment {
            "可依据实际结算结果调整未来计划，但不得改写已发布正史。"
        } else {
            "不得自动改动未来钩子计划。"
        }
    )
}

/// 静场/疗伤章规则：允许必要静场时保护余波章，否则要求每章都有清晰情绪转折
fn quiet_rule(config: &EmotionalHookConfig) -> &'static str {
    if config.allow_quiet_aftermath {
        "允许必要的静场、疗伤、关系余波和规则建立章；静场以认知、关系、目标或状态的微小变化完成推进，不因没有大反转判失败。"
    } else {
        "每章必须形成清晰的情绪转折与下一步行动压力。"
    }
}

fn plan_adjustment_rule(config: &EmotionalHookConfig) -> &'static str {
    if config.allow_plan_adjustment {
        "允许 AI 根据已发布正史调整尚未执行的钩子排布。"
    } else {
        "不得自行改动未来钩子排布，只能报告冲突。"
    }
}

fn optional_custom_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("\n# 用户自定义情绪钩子要求\n{trimmed}")
    }
}

fn join_optional(left: &str, right: &str) -> String {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("\n{left}"),
        (true, false) => right.to_owned(),
        (false, false) => format!("\n{left}{right}"),
    }
}

fn density_label(value: EmotionalHookDensity) -> &'static str {
    match value {
        EmotionalHookDensity::Relaxed => "舒缓",
        EmotionalHookDensity::Balanced => "平衡",
        EmotionalHookDensity::Tight => "紧凑",
    }
}

fn preset_label(value: EmotionalHookPreset) -> &'static str {
    match value {
        EmotionalHookPreset::Tomato => "番茄",
        EmotionalHookPreset::Qidian => "起点",
        EmotionalHookPreset::Jjwxc => "晋江",
        EmotionalHookPreset::General => "通用",
        EmotionalHookPreset::Custom => "自定义",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_repeated_recent_signal() {
        let samples = vec![
            PlotSignalSample {
                ending_state: "门外传来敲门声".to_owned(),
                ..Default::default()
            },
            PlotSignalSample {
                summary: "神秘人现身".to_owned(),
                ..Default::default()
            },
            PlotSignalSample {
                ending_state: "又有人敲门".to_owned(),
                ..Default::default()
            },
        ];
        assert_eq!(repeated_recent_hook_signal(&samples), Some("敲门"));
    }

    #[test]
    fn no_signal_with_single_occurrence_returns_none() {
        let samples = vec![
            PlotSignalSample {
                ending_state: "火势渐熄".to_owned(),
                ..Default::default()
            },
            PlotSignalSample {
                summary: "雨停，巷口安静下来".to_owned(),
                ..Default::default()
            },
        ];
        assert_eq!(repeated_recent_hook_signal(&samples), None);
    }

    #[test]
    fn only_considers_the_most_recent_six_entries() {
        let mut samples = vec![
            PlotSignalSample {
                ending_state: "旧案卷宗已封存".to_owned(),
                ..Default::default()
            },
            PlotSignalSample {
                ending_state: "档案室无人值守".to_owned(),
                ..Default::default()
            },
            PlotSignalSample {
                ending_state: "调阅到新的编号".to_owned(),
                ..Default::default()
            },
        ];
        for _ in 0..6 {
            samples.push(PlotSignalSample {
                ending_state: "一章平淡收束".to_owned(),
                ..Default::default()
            });
        }
        assert_eq!(repeated_recent_hook_signal(&samples), None);
    }

    #[test]
    fn disabled_module_injects_nothing() {
        let config = EmotionalHookConfig {
            enabled: false,
            ..EmotionalHookConfig::default()
        };
        assert!(render_hook_planning_contract(&config, 0, None).is_empty());
        assert!(render_hook_execution_contract(&config).is_empty());
        assert!(render_hook_ledger_contract(&config).is_empty());
    }

    #[test]
    fn quiet_aftermath_is_explicitly_protected() {
        let config = EmotionalHookConfig::default();
        let rendered = render_hook_execution_contract(&config);
        assert!(rendered.contains("静场"));
        assert!(rendered.contains("不因没有大反转判失败"));
    }

    #[test]
    fn planning_contract_carries_key_rules() {
        let config = EmotionalHookConfig::default();
        let rendered = render_hook_planning_contract(&config, 3, Some("敲门"));
        assert!(rendered.contains("当前活跃剧情伏笔：3/4"));
        assert!(rendered.contains("反复使用“敲门”"));
        assert!(rendered.contains("每章只确定一个主情绪钩子"));
        assert!(rendered.contains("情绪钩子不是剧情伏笔的同义词"));
    }

    #[test]
    fn ledger_contract_carries_warning_and_adjustment_rules() {
        let config = EmotionalHookConfig::default();
        let rendered = render_hook_ledger_contract(&config);
        assert!(rendered.contains("不得虚构推进"));
        assert!(rendered.contains("不得改写已发布正史"));
    }
}
