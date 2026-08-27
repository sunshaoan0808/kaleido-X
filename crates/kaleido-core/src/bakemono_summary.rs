//! 摘要层级与生成目标选择（吸收自 SillyTavern-BakemonoMemory
//! `src/summary/levels.js` + `src/summary/target-selection.js`）。
//!
//! 多层摘要体系：0=剧情摘要 / 1=阶段总结 / 2=多次总结 / 3+=长期总览。
//! 目标选择：全部/最早 N 条/楼层范围 + 连续性缺口检测。纯函数。
//! 测例翻译自 `tests/planning-modules.test.mjs`（target selection 部分）。

use std::collections::BTreeSet;

/// 摘要层级（0=剧情摘要, 1=阶段总结, 2=多次总结, 3+=长期总览）。
pub fn get_summary_level(level: Option<i64>, kind: Option<&str>) -> i64 {
    if let Some(l) = level {
        if l >= 0 {
            return l;
        }
    }
    match kind {
        Some(k) if k == "epic" => 2,
        Some(k) if k == "stage" => 1,
        _ => 0,
    }
}

/// 下一轮多次总结的目标层级：现有最高 +1（至少 2）。
pub fn get_next_multi_summary_level(targets: &[i64]) -> i64 {
    let max = targets.iter().copied().fold(1i64, |m, l| m.max(l));
    (max + 1).max(2)
}

/// 多次总结标签。
pub fn get_multi_summary_label(level: i64) -> String {
    if level <= 2 {
        "多次总结".to_string()
    } else if level == 3 {
        "长期总览".to_string()
    } else {
        format!("长期总览 L{}", level)
    }
}

/// 摘要类型标签。
pub fn get_summary_kind_label(kind: &str, is_epic: bool) -> String {
    if kind == "story" {
        "剧情摘要".to_string()
    } else if is_epic || kind == "epic" {
        "多次总结".to_string()
    } else {
        "阶段总结".to_string()
    }
}

// ─── 目标选择（target-selection.js）───────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    All,
    Oldest,
    Range,
}

#[derive(Debug, Clone)]
pub struct GenerationConfig {
    pub mode: TargetMode,
    pub count: usize,
    pub range: String,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            mode: TargetMode::All,
            count: 20,
            range: String::new(),
        }
    }
}

/// 宽松数字范围解析："7-5, 11，nope" → ids {5,6,7,11} + invalid ["nope"]。
pub fn parse_loose_number_range(value: &str) -> (BTreeSet<u64>, Vec<String>) {
    let mut ids = BTreeSet::new();
    let mut invalid = Vec::new();
    for part in value
        .split(|c: char| c == ',' || c == '，' || c.is_whitespace())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(mut start), Ok(mut end)) = (a.trim().parse::<u64>(), b.trim().parse::<u64>()) {
                if start > end {
                    std::mem::swap(&mut start, &mut end);
                }
                for id in start..=end {
                    ids.insert(id);
                }
                continue;
            }
        } else if let Ok(n) = part.parse::<u64>() {
            ids.insert(n);
            continue;
        }
        invalid.push(part.to_string());
    }
    (ids, invalid)
}

/// 目标块（消息楼层归属）。
#[derive(Debug, Clone)]
pub struct TargetBlock {
    pub hash: String,
    pub message_id: Option<i64>,
    pub source_message_ids: Vec<i64>,
    pub block_index: usize,
}

fn block_sort_key(b: &TargetBlock) -> (i64, usize) {
    (
        b.message_id
            .unwrap_or(i64::MAX)
            .min(b.source_message_ids.first().copied().unwrap_or(i64::MAX)),
        b.block_index,
    )
}

/// 排序（messageId 优先，blockIndex 次之）。
pub fn get_sorted_target_blocks(blocks: &[TargetBlock]) -> Vec<TargetBlock> {
    let mut sorted: Vec<TargetBlock> = blocks.to_vec();
    sorted.sort_by_key(block_sort_key);
    sorted
}

/// 按模式选择生成目标。
pub fn select_generation_targets(blocks: &[TargetBlock], config: &GenerationConfig) -> Vec<TargetBlock> {
    let sorted = get_sorted_target_blocks(blocks);
    match config.mode {
        TargetMode::Oldest => {
            let count = config.count.max(1);
            sorted.into_iter().take(count).collect()
        }
        TargetMode::Range => {
            let (ids, _) = parse_loose_number_range(&config.range);
            if ids.is_empty() {
                return sorted;
            }
            sorted
                .into_iter()
                .filter(|b| {
                    let mut src: Vec<i64> = b.source_message_ids.clone();
                    if let Some(m) = b.message_id {
                        src.push(m);
                    }
                    src.into_iter().any(|id| ids.contains(&(id as u64)))
                })
                .collect()
        }
        TargetMode::All => sorted,
    }
}

/// 分批：oldest 单批；all/range 按 batch_size 切批。
pub fn partition_generation_targets(
    blocks: &[TargetBlock],
    kind: &str,
    config: &GenerationConfig,
) -> Vec<Vec<TargetBlock>> {
    let sorted = get_sorted_target_blocks(blocks);
    match config.mode {
        TargetMode::Oldest => {
            let selected = select_generation_targets(&sorted, config);
            if selected.is_empty() {
                Vec::new()
            } else {
                vec![selected]
            }
        }
        _ => {
            let pool = if config.mode == TargetMode::Range {
                select_generation_targets(&sorted, config)
            } else {
                sorted
            };
            let default_count = if kind == "epic" { 5 } else { 20 };
            let batch_size = if config.count > 0 {
                config.count
            } else {
                default_count
            };
            let batch_size = batch_size.max(1);
            let mut batches = Vec::new();
            let mut idx = 0;
            while idx < pool.len() {
                let end = (idx + batch_size).min(pool.len());
                batches.push(pool[idx..end].to_vec());
                idx = end;
            }
            batches.into_iter().filter(|b| !b.is_empty()).collect()
        }
    }
}

/// 楼层记忆记录。
#[derive(Debug, Clone)]
pub struct FloorRecord {
    pub id: u64,
    pub summary_state: String,
}

/// 连续性缺口：目标覆盖范围内缺少 saved/covered 摘要的楼层。
pub fn find_target_continuity_gaps(
    blocks: &[TargetBlock],
    floor_records: &[FloorRecord],
) -> Vec<FloorRecord> {
    let mut target_ids: BTreeSet<u64> = BTreeSet::new();
    for b in blocks {
        if let Some(m) = b.message_id {
            if m >= 0 {
                target_ids.insert(m as u64);
            }
        }
        for m in &b.source_message_ids {
            if *m >= 0 {
                target_ids.insert(*m as u64);
            }
        }
    }
    let mut records: Vec<FloorRecord> = floor_records
        .iter()
        .filter(|r| r.summary_state == "saved" || r.summary_state == "covered" || r.summary_state == "missing" || r.summary_state == "draft")
        .cloned()
        .collect();
    records.sort_by_key(|r| r.id);
    let record_ids: BTreeSet<u64> = records.iter().map(|r| r.id).collect();
    let matched: Vec<u64> = target_ids.iter().copied().filter(|id| record_ids.contains(id)).collect();
    if records.is_empty() || matched.is_empty() {
        return Vec::new();
    }
    let first_target = *matched.iter().min().unwrap();
    let last_target = *matched.iter().max().unwrap();
    let previous_ready = records
        .iter()
        .filter(|r| r.id < first_target && (r.summary_state == "saved" || r.summary_state == "covered"))
        .last();
    let range_start = match previous_ready {
        Some(r) => r.id + 1,
        None => records[0].id,
    };
    records
        .into_iter()
        .filter(|r| {
            r.id >= range_start
                && r.id <= last_target
                && !target_ids.contains(&r.id)
                && (r.summary_state == "missing" || r.summary_state == "draft")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(hash: &str, message_id: i64, block_index: usize) -> TargetBlock {
        TargetBlock {
            hash: hash.to_string(),
            message_id: Some(message_id),
            source_message_ids: Vec::new(),
            block_index,
        }
    }

    fn source_block(hash: &str, source_ids: Vec<i64>, block_index: usize) -> TargetBlock {
        TargetBlock {
            hash: hash.to_string(),
            message_id: None,
            source_message_ids: source_ids,
            block_index,
        }
    }

    #[test]
    fn summary_levels_are_deterministic() {
        assert_eq!(get_summary_level(None, Some("stage")), 1);
        assert_eq!(get_summary_level(Some(2), None), 2);
        assert_eq!(get_summary_level(None, Some("epic")), 2);
        assert_eq!(get_summary_level(None, None), 0);
        assert_eq!(get_next_multi_summary_level(&[0, 1]), 2);
        assert_eq!(get_next_multi_summary_level(&[2]), 3);
        assert_eq!(get_multi_summary_label(2), "多次总结");
        assert_eq!(get_multi_summary_label(3), "长期总览");
        assert_eq!(get_summary_kind_label("story", false), "剧情摘要");
        assert_eq!(get_summary_kind_label("stage", false), "阶段总结");
    }

    #[test]
    fn target_selection_sorts_filters_ranges_and_partitions() {
        let blocks = vec![
            block("late", 30, 0),
            source_block("source-only", vec![14, 12], 1),
            block("early", 5, 2),
        ];
        let (ids, invalid) = parse_loose_number_range("7-5, 11，nope");
        assert_eq!(ids, [5, 6, 7, 11].into_iter().collect());
        assert_eq!(invalid, vec!["nope".to_string()]);

        let all: Vec<String> = select_generation_targets(
            &blocks,
            &GenerationConfig::default(),
        )
        .into_iter()
        .map(|b| b.hash)
        .collect();
        assert_eq!(all, vec!["early", "source-only", "late"]);

        let oldest: Vec<String> = select_generation_targets(
            &blocks,
            &GenerationConfig {
                mode: TargetMode::Oldest,
                count: 2,
                ..Default::default()
            },
        )
        .into_iter()
        .map(|b| b.hash)
        .collect();
        assert_eq!(oldest, vec!["early", "source-only"]);

        let range: Vec<String> = select_generation_targets(
            &blocks,
            &GenerationConfig {
                mode: TargetMode::Range,
                range: "14, 30".to_string(),
                ..Default::default()
            },
        )
        .into_iter()
        .map(|b| b.hash)
        .collect();
        assert_eq!(range, vec!["source-only", "late"]);

        let batches: Vec<Vec<String>> = partition_generation_targets(
            &blocks,
            "stage",
            &GenerationConfig {
                mode: TargetMode::All,
                count: 2,
                ..Default::default()
            },
        )
        .into_iter()
        .map(|b| b.into_iter().map(|x| x.hash).collect())
        .collect();
        assert_eq!(batches, vec![vec!["early", "source-only"], vec!["late"]]);

        let oldest_batches: Vec<Vec<String>> = partition_generation_targets(
            &blocks,
            "stage",
            &GenerationConfig {
                mode: TargetMode::Oldest,
                count: 2,
                ..Default::default()
            },
        )
        .into_iter()
        .map(|b| b.into_iter().map(|x| x.hash).collect())
        .collect();
        assert_eq!(oldest_batches, vec![vec!["early", "source-only"]]);
    }

    #[test]
    fn continuity_gaps_found_between_targets() {
        let records = vec![
            FloorRecord { id: 2, summary_state: "saved".to_string() },
            FloorRecord { id: 4, summary_state: "missing".to_string() },
            FloorRecord { id: 6, summary_state: "draft".to_string() },
            FloorRecord { id: 8, summary_state: "saved".to_string() },
        ];
        let gaps: Vec<u64> = find_target_continuity_gaps(&[block("t", 8, 0)], &records)
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(gaps, vec![4, 6]);

        let gaps2: Vec<u64> = find_target_continuity_gaps(&[source_block("t", vec![4, 8], 0)], &records)
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(gaps2, vec![6]);
    }
}
