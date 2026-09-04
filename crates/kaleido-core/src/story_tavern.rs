//! Story Tavern (ST-0): domain types + file-backed stores.
//! Layout (ADR-0002 / ADR-0003):
//! - `$DATA/story-packs/{id}/pack.json` + `chapters/*.md`
//! - `$DATA/tavern-sessions/tavern-session-{uuid}.json`
//! - `$DATA/tavern-persona/{characterId}.json`

use crate::{
    st_compass::{Compass, CompassStore},
    st_skimming::SkimIssue,
    text_hash, CoreError, CoreResult, DataRoot,
};
use crate::world_state::{EntityKind as WsEntityKind, WorldEntity, WorldEvent, WorldState};
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};
use uuid::Uuid;

// ─── Enums ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentTier {
    Safe,
    Standard,
    Open,
}

impl ContentTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Standard => "standard",
            Self::Open => "open",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Safe => 0,
            Self::Standard => 1,
            Self::Open => 2,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "safe" => Some(Self::Safe),
            "standard" => Some(Self::Standard),
            "open" => Some(Self::Open),
            _ => None,
        }
    }

    /// contentTierFinal = min(user, card/pack, global)
    pub fn min3(user: Self, pack_or_card: Self, global: Self) -> Self {
        let r = user.rank().min(pack_or_card.rank()).min(global.rank());
        match r {
            0 => Self::Safe,
            1 => Self::Standard,
            _ => Self::Open,
        }
    }
}

impl Default for ContentTier {
    fn default() -> Self {
        Self::Standard
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlayMode {
    Mainline,
    Free,
    /// Side story / 番外：冻结主线 chapterCursor/nodeId，回主线用 resumeNodeId
    Side,
}

impl Default for PlayMode {
    fn default() -> Self {
        Self::Mainline
    }
}

/// P0 (吞噬 denova novel-lite / novel-standard / novel-heavy): 回合叙事生成写作档位。
/// - `lite`：主 Agent 单次直出（现状，零回归）
/// - `standard`：初稿 → 审稿 → 修订
/// - `heavy`：context-plan → write → review → fix → final-gate 全管道（上限 2 轮修复）
/// 会话级持久化；`TurnStartRequest.quality` 可 per-turn 覆盖并回写会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Lite,
    Standard,
    Heavy,
}

impl Default for Quality {
    fn default() -> Self {
        Self::Lite
    }
}

impl Quality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Standard => "standard",
            Self::Heavy => "heavy",
        }
    }
}

/// 宽松反序列化：缺省/非法值一律回落 lite（旧请求体与未知档位兼容）。
impl<'de> Deserialize<'de> for Quality {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "standard" => Self::Standard,
            "heavy" => Self::Heavy,
            _ => Self::Lite,
        })
    }
}

impl PlayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mainline => "mainline",
            Self::Free => "free",
            Self::Side => "side",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mainline" => Some(Self::Mainline),
            "free" => Some(Self::Free),
            "side" => Some(Self::Side),
            _ => None,
        }
    }

    /// Modes that must not advance mainline node/chapter cursors.
    pub fn freezes_mainline_cursor(self) -> bool {
        matches!(self, Self::Free | Self::Side)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Playable {
    #[serde(rename = "P1")]
    P1,
    #[serde(rename = "P2")]
    P2,
    #[serde(rename = "P3")]
    P3,
    #[serde(rename = "P4")]
    P4,
}

impl Default for Playable {
    fn default() -> Self {
        Self::P1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryRole {
    Supporting,
    Protagonist,
    Isekai,
    Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetaKnowledge {
    None,
    Reader,
}

impl Default for MetaKnowledge {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RewriteIntensity {
    Canon,
    Rewrite,
}

impl Default for RewriteIntensity {
    fn default() -> Self {
        Self::Canon
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineTag {
    Canon,
    Advance,
    Idle,
}

// ─── Pack ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSource {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(default)]
    pub refs: Vec<String>,
}

impl Default for PackSource {
    fn default() -> Self {
        Self {
            source_type: "demo".into(),
            refs: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeExit {
    pub id: String,
    pub when: String,
    pub next: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryNode {
    pub id: String,
    pub chapter_id: String,
    pub title: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub exit: Vec<NodeExit>,
    #[serde(default)]
    pub locked_beats: Vec<String>,
    /// none | branch | open
    #[serde(default = "default_divergence")]
    pub allowed_divergence: String,
    #[serde(default)]
    pub present_characters: Vec<String>,
    #[serde(default)]
    pub location_id: Option<String>,
    /// Short excerpt for prompt (from chapter md)
    #[serde(default)]
    pub summary: String,
}

fn default_divergence() -> String {
    "branch".into()
}

/// Key node candidate for side-branch picker (整本总结 → 重要节点).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideBranchNode {
    pub id: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub present_characters: Vec<String>,
    /// Why this node is offered as a side branch.
    #[serde(default)]
    pub reason: String,
    pub order: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideBranchCatalog {
    pub pack_id: String,
    pub pack_title: String,
    /// Whole-novel blurb distilled from chapters/nodes/lore.
    pub novel_summary: String,
    pub nodes: Vec<SideBranchNode>,
    #[serde(default)]
    pub resume_node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryChapter {
    pub id: String,
    pub title: String,
    pub order: u32,
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Relative path under pack dir, e.g. chapters/ch01.md
    #[serde(default)]
    pub body_path: String,
    /// U10: 章节插图——工作区相对路径（images/illustrations/...），无插图时为空串。
    /// 前端经 GET /api/v1/works/image-data-url?path= 读取展示。serde default 兼容旧 pack.json。
    #[serde(default)]
    pub image_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackCharacterRef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    /// 性别(证据可推则写,否则"未知")。
    #[serde(default)]
    pub gender: String,
    /// 外貌/形象(证据可推则写,否则"未知")。
    #[serde(default)]
    pub appearance: String,
    /// 开场场景一句话(地点/时间/在场人,蒸馏产出)。
    #[serde(default)]
    pub opening_scene: String,
    /// 开场白 1-2 句(蒸馏产出,建立情境+可行动位置+约束)。
    #[serde(default)]
    pub opening_lines: String,
    /// 敏感度判定边界（蒸馏产出,格式"敏感点→判定边界",用于 Open/Standard 档演出护栏）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nsfw_profile: String,
    /// 角色谱 importance(high/medium/low),由蒸馏环节写入,供主角断言/过滤使用。
    #[serde(default)]
    pub importance: String,
    #[serde(default)]
    pub content_tier: Option<ContentTier>,
    #[serde(default)]
    pub example_dialogs: Vec<String>,
    #[serde(default)]
    pub boundaries: Vec<String>,
    #[serde(default)]
    pub personality: String,
    /// [P8 D1 2026-08-16] 声线特征（蒸馏 M2 产出）：角色默认语气一句话 + 代表金句支撑。
    /// 修复宿醉声线漂移源头——personality 概括化丢泼辣面，声线单独成字段防止被稀释。
    #[serde(default)]
    pub voice_profile: String,
    #[serde(default)]
    pub speech_style: String,
    /// 深层动机/目标（女娲"心智模型"提炼，带证据）。
    #[serde(default)]
    pub motivation: String,
    /// 与该角色相关的人与关系简述，格式 "角色名:关系描述"，带证据。
    #[serde(default)]
    pub relationships: Vec<String>,
    /// 每条结论的证据出处，格式 "ch12:block3" 等。
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// 女娲"核心心智模型"：角色看世界的镜片，每条 "模型名：一句话描述（证据ch）"。
    #[serde(default)]
    pub mental_models: Vec<String>,
    /// 女娲"决策启发式"：角色做判断的直觉规则，每条 "规则名：触发场景→做法（证据ch）"。
    #[serde(default)]
    pub decision_heuristics: Vec<String>,
    /// 仓颉"信念形成故事"：角色相信什么、为什么信，每条 "信念：形成故事（证据ch）"。
    #[serde(default)]
    pub beliefs: Vec<String>,
    /// P2-1 立绘层：情绪名 → 表情图片 URL。情绪名对齐 P2-1 枚举（平静/开心/愤怒/悲伤/害羞/惊讶/恐惧/厌恶/疲惫/心动）。serde default 兼容旧 pack 无此字段。
    #[serde(default)]
    pub expressions: std::collections::HashMap<String, String>,
    /// P2-1 立绘层：默认立绘（无情绪匹配时回退）。serde default 兼容旧 pack。
    #[serde(default)]
    pub avatar: Option<String>,
    /// P3 语音层：edge-tts 音色名（如 zh-CN-YunxiNeural）。可选；未指定时前端按角色名 hash 从默认音色池稳定选。serde default/skip 兼容旧 pack + 列表（P2-1 教训：列表端点必须透传）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// SoulLink 吸收：对话驱动的增量档案（标量字段 + personality/worldview/family/relationships/memory 分节）。
    /// AI 每轮生成后自动维护；前端可查看/手动分析/精编。serde default 兼容旧 pack。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<crate::character_archive::CharacterArchive>,
    /// [吞噬 Front Porch AI pockets.dart] 初始口袋与衣物（蒸馏/卡面播种）。
    /// worn = 身上穿的，carrying = 身上带的，均为 display 字符串（如 "jacket (rain-soaked)"）。
    /// 旧 pack 无此字段 → 空口袋，不影响既有会话。
    #[serde(default)]
    pub starting_wardrobe: crate::pockets::Pockets,
}

#[cfg(test)]
mod sprite_ref_tests {
    use super::PackCharacterRef;

    /// P2-1 立绘层：pack roundtrip 带 expressions/avatar 不丢，旧数据（无这两字段）也不爆。
    #[test]
    fn pack_character_ref_expressions_roundtrip() {
        let mut ch = PackCharacterRef {
            id: "c1".into(),
            name: "母亲".into(),
            role: String::new(),
            gender: String::new(),
            appearance: String::new(),
            opening_scene: String::new(),
            opening_lines: String::new(),
            nsfw_profile: String::new(),
            importance: "high".into(),
            content_tier: None,
            example_dialogs: Vec::new(),
            boundaries: Vec::new(),
            personality: String::new(),
            speech_style: String::new(),
            voice_profile: String::new(),
            motivation: String::new(),
            relationships: Vec::new(),
            evidence_refs: Vec::new(),
            mental_models: Vec::new(),
            decision_heuristics: Vec::new(),
            beliefs: Vec::new(),
            expressions: std::collections::HashMap::new(),
            voice: None,
            archive: None,
            avatar: None,
                starting_wardrobe: Default::default(),
        };
        ch.expressions
            .insert("开心".into(), "https://img/x/1.png".into());
        ch.avatar = Some("https://img/x/avatar.png".into());
        ch.nsfw_profile = "接吻以上→nsfw；日常对白→non".into();
        let json = serde_json::to_value(&ch).unwrap();
        let back: PackCharacterRef = serde_json::from_value(json).unwrap();
        assert_eq!(back.expressions.get("开心").map(|s| s.as_str()), Some("https://img/x/1.png"));
        assert_eq!(back.avatar.as_deref(), Some("https://img/x/avatar.png"));
        assert_eq!(back.nsfw_profile, "接吻以上→nsfw；日常对白→non");
    }

    /// 旧 pack 数据无 expressions/avatar 字段——默认值不爆。
    #[test]
    fn pack_character_ref_old_data_defaults() {
        let ch: PackCharacterRef = serde_json::from_value(serde_json::json!({
            "id": "c1", "name": "母亲"
        }))
        .unwrap();
        assert!(ch.expressions.is_empty());
        assert!(ch.avatar.is_none());
        assert!(ch.nsfw_profile.is_empty());
    }
}

// ─── 演出机 S1：导演台（Stage Director）与 Actor 状态（纯数据结构骨架）────────

/// S5: 事件卡包（吞噬 denova event_package）。一包一组同类 TellerEventCard，
/// 由 pack.stage_director.modules.eventPackageIds 指定启用哪些包参与每回合抽取。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventPackage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cards: Vec<TellerEventCard>,
}

/// S5: 事件卡。每回合由 teller 按权重抽一张，注入 ST-27 引导 LLM 演出。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TellerEventCard {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// 注入给 LLM 的演出提示（与 denova event_card.prompt 对齐）。
    #[serde(default)]
    pub prompt: String,
    /// 抽取权重（0 = 不参与；默认 1）。
    #[serde(default = "default_event_weight")]
    pub weight: u32,
    /// 卡片是否启用。
    #[serde(default)]
    pub enabled: bool,
    /// 每会话只抽一次（抽过后 usedInSession 置 true，后续回合排除）。
    #[serde(default)]
    pub once_per_session: bool,
    /// 本会话已抽过（由 pick_event_card 维护；会话开始时应为 false）。
    #[serde(default)]
    pub used_in_session: bool,
    /// 事件类型名（用户可见，如「外门考核打脸」）；缺省空 = 不展示类型标签
    #[serde(default)]
    pub type_name: String,
    /// 类别（如 打脸/奇遇/秘境/恋爱/冲突…）；空 = 未分类
    #[serde(default)]
    pub category: String,
    /// 短标签（可检索）
    #[serde(default)]
    pub tags: Vec<String>,
    /// 强度档（low/medium/high）；空 = 不展示
    #[serde(default)]
    pub intensity: String,
    /// 冷却回合数（非负，默认 0 = 无冷却）：同一张卡抽过后 N 回合内不再抽
    #[serde(default)]
    pub cooldown_turns: u32,
    /// 所属章节范围（如 ["ch01","ch03"]，蒸馏产出，基于切分 chXX 编号与运行时 chapter_cursor 对齐）。
    /// 空 = 旧数据/未标注 → 任意章节可抽（兼容 A3）。
    #[serde(default)]
    pub chapter_range: Vec<String>,
}

fn default_event_weight() -> u32 {
    1
}

/// S5: 每回合事件卡抽取纯函数（吸收 denova event_package teller 抽样逻辑）。
/// - 候选：pack.eventPackages 中 enabled 的包；若 modules.eventPackageIds 非空则只取其中列出的包。
/// - 卡必须 enabled、weight>0；oncePerSession 且 usedInSession 的卡排除。
/// - 冷却：card.cooldown_turns>0 且 last_event 命中同一张卡、且距上次抽取 < 冷却回合 → 排除。
///   last_event=None 视为无冷却限制（兼容旧调用）；next_turn 为本次抽取所在回合（与 last_event.turn 做差）。
/// - A2 按章过滤：current_chapter 传入当前 chapter_cursor（如 "ch03"）时，
///   只抽 chapter_range 覆盖当前章的卡；chapter_range 为空（旧数据/未标注）→ 不参与章过滤（A3 兼容）；
///   有 range 但当前章不命中 → 该卡排除；全部排除 → 返回 None（本回合不抽事件卡）。
/// - 按 weight 加权抽样；seed 由调用方提供（server 可用 turn/时间戳），无 rand 依赖。
pub fn pick_event_card<'a>(
    pack: &'a StoryPack,
    seed: u64,
    next_turn: u32,
    last_event: Option<&EventLogEntry>,
    current_chapter: Option<&str>,
) -> Option<(&'a EventPackage, &'a TellerEventCard)> {
    let mut candidates: Vec<(&'a EventPackage, &'a TellerEventCard)> = Vec::new();
    let allowed = &pack.stage_director.modules.event_package_ids;
    for pkg in &pack.event_packages {
        if !pkg.enabled {
            continue;
        }
        if !allowed.is_empty() && !allowed.contains(&pkg.id) {
            continue;
        }
        for card in &pkg.cards {
            if !card.enabled || card.weight == 0 {
                continue;
            }
            if card.once_per_session && card.used_in_session {
                continue;
            }
            // A2 按章过滤：card 标注了 chapter_range 且当前章非空时，只抽覆盖当前章的卡。
            // 未标注（空 range）或当前章未知（None）→ 不拦截（旧行为）。
            if !card.chapter_range.is_empty() {
                if let Some(cur) = current_chapter {
                    if !card.chapter_range.iter().any(|r| r == cur) {
                        continue;
                    }
                }
            }
            // 冷却：同一张卡抽过后 cooldown_turns 内不再抽（基于 last_event.turn 与 next_turn 的回合差）
            if card.cooldown_turns > 0 {
                if let Some(ev) = last_event {
                    if ev.card_id == card.id && next_turn > ev.turn && next_turn - ev.turn < card.cooldown_turns
                    {
                        continue;
                    }
                }
            }
            candidates.push((pkg, card));
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let total: u64 = candidates.iter().map(|(_, c)| c.weight as u64).sum();
    if total == 0 {
        return None;
    }
    // xorshift64* 确定性伪随机（不同种子 → 不同分布；可复现测试）
    let mut x = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0xBF58_476D_1CE4_E5B9);
    if x == 0 {
        x = 0x9E37_79B9_7F4A_7C15;
    }
    let mut next = move || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let mut r = next() % total;
    for (pkg, card) in &candidates {
        if r < card.weight as u64 {
            return Some((*pkg, *card));
        }
        r -= card.weight as u64;
    }
    Some(*candidates.last().expect("candidates non-empty"))
}

/// S5: 事件卡抽取记录（挂到 session.lastEvent，供前端展示 + ST-27 注入）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventLogEntry {
    #[serde(default)]
    pub turn: u32,
    #[serde(default)]
    pub package_id: String,
    #[serde(default)]
    pub card_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub created_at: String,
    /// 事件类型名（冗余记录卡字段，供本回合注入直接展示；旧数据缺省空 = 不展示类型标签）。
    #[serde(default)]
    pub type_name: String,
    /// 类别（冗余记录卡字段）：本回合注入展示用。
    #[serde(default)]
    pub category: String,
    /// 强度档（冗余记录卡字段，low/medium/high）：本回合注入展示用。
    #[serde(default)]
    pub intensity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StageDirectorConfig {
    #[serde(default)]
    pub run_policy: DirectorRunPolicy,
    #[serde(default)]
    pub modules: DirectorModuleRefs,
    /// P1-2 (吞噬 denova mainline_strength): 主线强度档位。
    /// - strong_arc = 严格遵循原著主线；balanced = 主线优先、允许合理分支（默认）；soft = 宽松探索
    #[serde(default = "default_mainline_strength")]
    pub mainline_strength: String,
    #[serde(default)]
    pub resolved_snapshot: Option<DirectorResolvedSnapshot>,
}

/// P1-2: mainline_strength 默认档位（兼容旧包，缺字段时回退 "balanced"）。
fn default_mainline_strength() -> String {
    "balanced".into()
}

/// S4: 导演计划（吞噬 denova director_plan）。**导演计划是意图，不是自由改写**：
/// 不得改写 locked_beats 硬事实。hits_beats 只允许引用当前 node.locked_beats 原文。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DirectorPlan {
    /// 导演目标（一句话）
    pub goal: String,
    /// 当前压力
    #[serde(default)]
    pub pressure: Option<String>,
    /// 达成代价
    #[serde(default)]
    pub cost: Option<String>,
    /// 声明命中的 locked_beats（红线声明，只能引用当前 node.locked_beats 原文）
    #[serde(default)]
    pub hits_beats: Vec<String>,
    #[serde(default)]
    pub created_turn: u32,
    #[serde(default)]
    pub updated_turn: u32,
    /// G1 (吞噬 denova DirectorPlanDocs.plan): 计划正文——承前（当前回合的叙事意图与承接要点）。
    #[serde(default)]
    pub plan: String,
    /// G1 (吞噬 denova DirectorPlanDocs.agent_brief): 本回合指令——给演出 agent 的导演指令。
    #[serde(default)]
    pub agent_brief: String,
    /// G1 (吞噬 denova DirectorPlanDocs.lore_context): 启后铺垫——世界观/设定上下文。
    #[serde(default)]
    pub lore_context: String,
    /// G2 (吞噬 denova DirectorPlanMetadata.last_run): 最近一次执行状态（ready/running/conflict）。
    #[serde(default)]
    pub last_run: Option<DirectorPlanRunStatus>,
}

/// G2 (吞噬 denova DirectorPlanRunStatus): 导演计划执行状态。旧数据无此字段 → None（未启动）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DirectorPlanRunStatus {
    /// 状态：ready=计划就绪 / running=执行中 / conflict=LLM 失败或冲突
    pub status: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl DirectorPlanRunStatus {
    pub fn ready(summary: impl Into<String>) -> Self {
        Self {
            status: "ready".into(),
            summary: Some(summary.into()),
            error: None,
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }
    pub fn running() -> Self {
        Self {
            status: "running".into(),
            summary: None,
            error: None,
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }
    pub fn conflict(error: impl Into<String>) -> Self {
        Self {
            status: "conflict".into(),
            summary: None,
            error: Some(error.into()),
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }
}

/// S4: 导演计划调度纯函数（吸收 denova director_run_policy.go DecideDirectorRunAfterTurn 的 interval 分支）。
/// - "manual" → false（仅 API 手动触发）
/// - "interval" → interval_turns>0 && turn>=1 && turn % interval_turns == 0 && last_plan_turn != Some(turn)（不重复触发）
/// - "on_demand" → false（由 server 触发条件决定）
/// - 其他 → false
pub fn director_due(
    mode: &str,
    interval_turns: u32,
    turn: u32,
    last_plan_turn: Option<u32>,
) -> bool {
    match mode.trim() {
        "manual" => false,
        "on_demand" => false,
        "interval" => {
            interval_turns > 0
                && turn >= 1
                && turn % interval_turns == 0
                && last_plan_turn != Some(turn)
        }
        _ => false,
    }
}

/// G13/G14 (吞噬 denova workspaceDirectorTaskGroup / GoKeyed): 后台导演任务组。
///
/// 导演 agent 的「回合外单次同步 LLM 调用」会随 HTTP 断线取消；本任务组把导演后台任务
/// 登记为 workspace 级串行任务组：
/// - **同 key 并发串行**：`start(key, task_id, f)` 只在 key 空闲时真正启动（返回 `true`）；
///   同 key 已在跑 → 不启动并返回 `false`（denova 语义：`!started` → 调用方按
///   `context.Canceled` 处理，不排队、不覆盖）。
/// - **任务登记**：`with_task(task_id)` 登记当前执行任务 id（线程级），供后台任务生命周期
///   可见性（GET director-config 的 `directorTask` 字段即由 server 写入 session）。
/// - **panic 捕获**：任务体 panic 被 `catch_unwind` 捕获并打 warn 日志，不炸进程，
///   运行表照常清理，key 可复用。
/// - **异步原语**：`acquire`/`release` 供 server 在 `tokio::spawn` 后台任务里手动
///   登记/释放 key（`start` 即 acquire + 执行 + 释放 的同步封装）。
pub struct DirectorTaskGroup {
    running: Mutex<std::collections::HashMap<String, String>>,
}

impl Default for DirectorTaskGroup {
    fn default() -> Self {
        Self {
            running: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

// 当前线程正在执行的导演任务 key（`start` 执行期间登记，供 `with_task` 无 key 登记语义）。
// [P7] 改为普通注释：doc 注释在 thread_local! 宏调用上不生效（unused_doc_comments）。
thread_local! {
    static DIRECTOR_TASK_KEY: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

impl DirectorTaskGroup {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前 key 是否在跑。
    pub fn is_running(&self, key: &str) -> bool {
        self.running.lock().contains_key(key)
    }

    /// 当前 key 正在跑的任务 id（None = 未在跑）。
    pub fn current_task(&self, key: &str) -> Option<String> {
        self.running.lock().get(key).cloned()
    }

    /// 尝试登记 key（同 key 在跑 → 返回 false 不启动）。
    /// 成功时登记 task_id，并执行 f；f panic 被 `catch_unwind` 捕获（warn 日志），
    /// 运行表照常清理。返回是否真正启动（非执行成败）。
    pub fn start<F>(&self, key: impl Into<String>, task_id: impl Into<String>, f: F) -> bool
    where
        F: FnOnce(),
    {
        let key = key.into();
        let task_id = task_id.into();
        if !self.acquire(key.clone(), task_id.clone()) {
            return false;
        }
        DIRECTOR_TASK_KEY.with(|c| *c.borrow_mut() = Some(key.clone()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        DIRECTOR_TASK_KEY.with(|c| *c.borrow_mut() = None);
        self.release(&key);
        match result {
            Ok(()) => {}
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic payload".into());
                tracing::warn!(
                    key = %key,
                    task_id = %task_id,
                    error = %msg,
                    "director task group caught task panic"
                );
            }
        }
        true
    }

    /// 登记当前执行任务的 task_id（替换/更新当前线程 key 的运行表登记）。
    /// 线程无正在执行的 key 时 no-op（async 后台任务请直接写 session.director_task）。
    pub fn with_task(&self, task_id: impl Into<String>) {
        let task_id = task_id.into();
        let current = DIRECTOR_TASK_KEY.with(|c| c.borrow().clone());
        if let Some(key) = current {
            if let Some(mut running) = self.running.try_lock() {
                if running.contains_key(&key) {
                    running.insert(key, task_id);
                }
            }
        }
    }

    /// 异步场景原语：登记 key（同 key 在跑 → false）。与 `release` 成对使用。
    pub fn acquire(&self, key: impl Into<String>, task_id: impl Into<String>) -> bool {
        let key = key.into();
        let mut running = self.running.lock();
        if running.contains_key(&key) {
            return false;
        }
        running.insert(key, task_id.into());
        true
    }

    /// 异步场景原语：释放 key（不校验归属；成对使用见 [`DirectorTaskGroup::acquire`]）。
    pub fn release(&self, key: &str) {
        self.running.lock().remove(key);
    }
}

/// G3 (吞噬 denova prepareInteractiveDirectorBeforeOpening): 开局导演规划调度纯函数。
/// 会话首个回合（turns == 0）且 opening 已 seed 且尚无任何导演 plan → true（应生成开局三文档：
/// 选角/场景/分支规划，给故事一个导演意图锚点）。
/// 幂等：已有 plan（含 last_run 任意状态）→ false，不重复生成。
pub fn opening_plan_due(turn: u32, has_plan: bool, opening_seeded: bool) -> bool {
    turn == 0 && !has_plan && opening_seeded
}

/// G4 (吞噬 denova fitTextToTokenBudget): 按字符预算拟合文本——**头尾保留、中间省略**。
/// CJK 按 ~1 字符/token 近似（中文 1 token ≈ 1 字符），预算单位统一为「字符」。
/// 超过预算时：头部保留 head_ratio（默认 0.5）份额、尾部保留其余，中间以「\n…（省略 N 字符）…\n」连接。
/// 预算内原样返回（零拷贝语义由调用方决定）。空串/预算为 0 → 空串。
pub fn fit_text_to_token_budget(text: &str, budget: usize, head_ratio: f64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    if total == 0 || budget == 0 {
        return String::new();
    }
    if total <= budget {
        return text.to_string();
    }
    let head = ((budget as f64 * head_ratio.clamp(0.0, 1.0)).floor() as usize).min(total);
    let tail_budget = budget.saturating_sub(head);
    let tail = tail_budget.min(total - head);
    let omitted = total - head - tail;
    let mut out = String::with_capacity(budget + 32);
    out.extend(chars.iter().take(head));
    if omitted > 0 {
        out.push_str(&format!("\n…（省略 {omitted} 字符）…\n"));
    }
    out.extend(chars.iter().skip(total - tail));
    out
}

/// G5 (吞噬 denova ContextLedger): 导演上下文账本条目——记录每个注入块的来源/标题/字节/预算/是否进入最终消息。
/// 仅供审计与日志：构建导演 LLM 请求时逐块登记，可观测「长会话导演上下文溢出」与「裁剪行为」。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectorLedgerEntry {
    /// 来源块标识（如 "recent" / "node_summary" / "strategy"）。
    #[serde(default)]
    pub source: String,
    /// 人类可读标题（如 "最近剧情"）。
    #[serde(default)]
    pub title: String,
    /// 原始字节数。
    #[serde(default)]
    pub body_bytes: usize,
    /// 预算上限（0 = 无预算）。
    #[serde(default)]
    pub limit: usize,
    /// 是否进入最终消息（false = 被预算裁剪/省略）。
    #[serde(default)]
    pub included: bool,
    /// 备注（裁剪原因、省略字符数等）。
    #[serde(default)]
    pub note: String,
}

/// G15 (吞噬 denova retainedTurnsForInteractiveCompaction): 叙界守卫事件保留策略。
/// 守卫事件字符串格式 `[high|med][维度] 消息`。长会话无限增长会拖慢存储/回放，
/// 按「最近 max_recent 条」窗口裁剪（denova 语义：可见轮次 + 压缩保留）；
/// 裁剪时**优先保留 high 严重度**（低优先级 med 先被淘汰），返回裁剪后的完整列表。
pub fn retain_guard_events(events: &[String], max_recent: usize) -> Vec<String> {
    if max_recent == 0 {
        return Vec::new();
    }
    if events.len() <= max_recent {
        return events.to_vec();
    }
    let overflow = events.len() - max_recent;
    // 从头淘汰 overflow 条，优先淘汰 med（high 保底）
    let mut kept: Vec<String> = events.to_vec();
    let mut removed = 0usize;
    // 第一遍：只淘汰 med（从头扫，保留窗口尾巴的 high）
    let mut i = 0usize;
    while i < kept.len() && removed < overflow {
        if !kept[i].starts_with("[high]") {
            kept.remove(i);
            removed += 1;
        } else {
            i += 1;
        }
    }
    // 第二遍：仍超窗 → 从头淘汰（含 high，最老优先）
    while kept.len() > max_recent {
        kept.remove(0);
    }
    kept
}

/// G10 (吞噬 denova D1 回合提交状态机): 回合提交幂等守卫（纯函数）。
/// 同一回合同内容已提交（已有 user 消息且 hash 相同）→ true（拒绝重复提交）；
/// 否则 false。
///
/// 判定规则：取末条 user 消息，其内容 hash 与本次提交 hash 相同，**且**其后已有
/// assistant 回应（该回合已完成，重复提交只回执不重跑）→ true。
/// 其余情况一律 false：
/// - 不同内容（末条 user 消息 hash 不同）→ 放行；
/// - 新回合（末条为 assistant，上一回合已完成；或末条 user 消息尚无 assistant 回应，
///   即回合挂起/LLM 空响应后可复用重试路径）→ 放行。
///
/// hash 用简单 FNV（`crate::text_hash`），由调用方（server `start_turn`）计算。
pub fn turn_submit_guard(session: &TavernSession, user_msg_hash: &str) -> bool {
    let Some(last_user_pos) = session.messages.iter().rposition(|m| m.role == "user") else {
        return false;
    };
    if text_hash(&session.messages[last_user_pos].content) != user_msg_hash {
        return false;
    }
    // 该 user 消息之后必须已有 assistant 回应（回合已完成）才算重复提交；
    // 无回应（回合挂起/失败可重试）→ 放行复用重试路径。
    session.messages[last_user_pos + 1..]
        .iter()
        .any(|m| m.role == "assistant")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectorRunPolicy {
    #[serde(default = "default_director_mode")]
    pub mode: String,
    #[serde(default)]
    pub interval_turns: u32,
    /// G8 (吞噬 denova strategy.failure_policy): 导演计划 LLM 失败时的处理策略。
    /// 枚举: fail_forward=失败继续推进（默认）/ success_at_cost=代价成功 / blocked=阻断 / hard_failure=硬失败
    #[serde(default = "default_failure_policy")]
    pub failure_policy: String,
    /// G8 (吞噬 denova strategy.pacing_curve): 剧情节奏曲线（wave=波浪 / goal-pressure-payoff=目标压力回报 / linear=线性…）。
    /// 空串 = 不指定，由导演计划自由发挥。
    #[serde(default)]
    pub pacing_curve: String,
    /// G8 (吞噬 denova strategy.event_frequency): 事件频率档。off=关闭 / sparse=稀疏 / balanced=均衡（默认）/ frequent=频繁
    #[serde(default = "default_event_frequency")]
    pub event_frequency: String,
    /// G8 (吞噬 denova strategy.rule_visibility_mode): 检定规则可见性。audit_only=仅审计（默认）/ public_roll=公开掷骰
    #[serde(default = "default_rule_visibility")]
    pub rule_visibility_mode: String,
    /// G8 (吞噬 denova strategy.branch_planning_turns): 分支规划回合数（导演计划为未来 N 回合准备承接策略），默认 5。
    #[serde(default = "default_branch_planning_turns")]
    pub branch_planning_turns: u32,
}

fn default_director_mode() -> String {
    "on_demand".into()
}

fn default_failure_policy() -> String {
    "fail_forward".into()
}

fn default_event_frequency() -> String {
    "balanced".into()
}

fn default_rule_visibility() -> String {
    "audit_only".into()
}

fn default_branch_planning_turns() -> u32 {
    5
}

impl Default for DirectorRunPolicy {
    fn default() -> Self {
        Self {
            mode: default_director_mode(),
            interval_turns: 0,
            failure_policy: default_failure_policy(),
            pacing_curve: String::new(),
            event_frequency: default_event_frequency(),
            rule_visibility_mode: default_rule_visibility(),
            branch_planning_turns: default_branch_planning_turns(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DirectorModuleRefs {
    #[serde(default)]
    pub narrative_style_id: Option<String>,
    #[serde(default)]
    pub event_package_ids: Vec<String>,
    #[serde(default)]
    pub rule_system_id: Option<String>,
    #[serde(default)]
    pub actor_state_id: Option<String>,
    #[serde(default)]
    pub image_preset_id: Option<String>,
    /// G11 (吞噬 denova module_refs *_disabled): 显式关闭模块（关闭时保留原 ID 以便重新启用）。
    /// 默认 false = 模块启用；true = 模块关闭但 ID 保留。
    #[serde(default)]
    pub narrative_style_disabled: bool,
    #[serde(default)]
    pub event_packages_disabled: bool,
    #[serde(default)]
    pub rule_system_disabled: bool,
    #[serde(default)]
    pub actor_state_disabled: bool,
    #[serde(default)]
    pub image_preset_disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DirectorResolvedSnapshot {
    #[serde(default)]
    pub narrative_style: Option<Value>,
    #[serde(default)]
    pub event_packages: Vec<Value>,
    #[serde(default)]
    pub rule_system: Option<Value>,
    #[serde(default)]
    pub actor_state: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryPack {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub source: PackSource,
    #[serde(default)]
    pub characters: Vec<PackCharacterRef>,
    #[serde(default)]
    pub world_book_ids: Vec<String>,
    #[serde(default)]
    pub chapters: Vec<StoryChapter>,
    #[serde(default)]
    pub nodes: Vec<StoryNode>,
    #[serde(default)]
    pub lore_entries: Vec<Value>,
    #[serde(default)]
    pub default_mode: PlayMode,
    #[serde(default)]
    pub max_tier: ContentTier,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    /// 演出机导演台配置（S1 骨架，缺省为空策略，不驱动任何行为）。
    #[serde(default)]
    pub stage_director: StageDirectorConfig,
    /// S5: 事件卡包（吞噬 denova event_package）。
    #[serde(default)]
    pub event_packages: Vec<EventPackage>,
    /// S5/S6: 演出机 Actor 状态配置（吞噬 denova actor_state）。
    /// pack 级模板 + 初始角色；create_from_pack 时初始化进 session.actor_states。
    #[serde(default)]
    pub actor_state_config: ActorStatePackConfig,
    /// T 层 (2026-08-19): 原著静态时间线（旁挂 worldline.json 蒸馏产物）。
    /// 运行时按当前章过滤注入（chapter ≤ 当前章），与会话 L2 事件账本构成「静态+动态」双源时间线。
    /// 此前旁挂文件零消费——PackStore 只读 pack.json，worldline.json 蒸了白蒸（D3 缺口）。
    /// serde default 保证旧 pack（无该字段）加载零影响。
    #[serde(default)]
    pub worldline: Vec<Value>,
}

impl StoryPack {
    pub fn is_playable(&self) -> bool {
        if self.characters.is_empty() || self.chapters.len() < 2 {
            return false;
        }
        // Need a connected chain of ≥2 nodes across chapters
        if self.nodes.len() < 2 {
            return false;
        }
        let mut has_edge = false;
        for n in &self.nodes {
            if !n.exit.is_empty() {
                has_edge = true;
                break;
            }
        }
        has_edge
    }

    pub fn first_node_id(&self) -> Option<String> {
        let mut chs = self.chapters.clone();
        chs.sort_by_key(|c| c.order);
        for ch in chs {
            if let Some(nid) = ch.node_ids.first() {
                return Some(nid.clone());
            }
        }
        self.nodes.first().map(|n| n.id.clone())
    }

    pub fn first_chapter_id(&self) -> Option<String> {
        let mut chs = self.chapters.clone();
        chs.sort_by_key(|c| c.order);
        chs.first().map(|c| c.id.clone())
    }
}

/// Lightweight character ref carried by pack list responses, so the create
/// wizard can offer vessel/focus choices without a second full-pack fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackCharacterSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    /// P2-1 立绘层：默认立绘（列表也要带，供剧场立绘容器渲染）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// P2-1 立绘层：情绪名 → 立绘图 URL（列表带全量，前端按情绪取）。
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub expressions: std::collections::HashMap<String, String>,
    /// P3 语音层：edge-tts 音色名（列表端点透传，前端朗读按角色取音色）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSummary {
    pub id: String,
    pub title: String,
    pub max_tier: ContentTier,
    pub chapter_count: usize,
    pub node_count: usize,
    pub character_count: usize,
    pub playable: bool,
    pub language: String,
    /// Short blurb for library cards (from lore 简介 / first lore / first node summary).
    #[serde(default)]
    pub blurb: String,
    /// Display cast (excludes narrator/player and junk auto-extracted names).
    #[serde(default)]
    pub cast_names: Vec<String>,
    /// Lightweight character refs (id+name+role), same filter as cast_names.
    #[serde(default)]
    pub characters: Vec<PackCharacterSummary>,
    /// First chapter title for library subtitle.
    #[serde(default)]
    pub first_chapter_title: String,
    /// Pack source type: novel | manual | demo | …
    #[serde(default)]
    pub source_type: String,
}

fn lore_text_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Heuristic: real character names are short CJK (or latin), not narrative fragments.
pub fn is_clean_cast_name(name: &str) -> bool {
    let n = name.trim();
    let chars: Vec<char> = n.chars().collect();
    if chars.len() < 2 || chars.len() > 8 {
        return false;
    }
    // reject common auto-extract garbage prefixes / particles
    let junk_prefixes = [
        "露出", "眼角", "换鞋", "随口", "轻声", "低头", "抬起", "转身", "伸手", "走过去", "看向",
        "听见", "突然", "只是", "已经", "然后", "因为", "所以", "连忙", "依旧", "还是", "还是",
        "你也没", "你也有", "你倒是", "我也没", "他也没", "她也没", "它也没", "有一", "有一个",
        "有一张", "有一件", "有一本", "有一瓶", "有一把", "第一道", "第二道", "第三道", "第四道",
        "第五道", "第六道", "第七道", "第八道", "第九道", "第一张", "第二张", "第三张", "第四张",
        "第五张", "第六张", "第七张", "第八张", "第九张", "张依旧", "李依旧", "王依旧", "依旧",
        "跟莫", "跟张", "跟李", "跟王", "跟赵", "跟钱", "跟孙", "跟周", "跟吴", "跟郑", "跟陈",
        "跟刘", "跟黄", "跟杨", "跟着", "跟着",
    ];
    for p in junk_prefixes {
        if n.starts_with(p) {
            return false;
        }
    }
    // reject names that start with obvious non-name words (pronouns, particles, prepositions, numbers)
    let non_name_starts: &[(char, Option<char>)] = &[
        ('跟', None), ('同', None), ('向', None), ('把', None), ('被', None), ('给', None),
        ('对', None), ('从', None), ('和', None), ('与', None), ('及', None), ('或', None),
        ('让', None), ('叫', None), ('让', None),
        ('你', None), ('我', None), ('他', None), ('她', None), ('它', None),
        ('这', None), ('那', None), ('第', None),
        ('一', None), ('二', None), ('三', None), ('四', None), ('五', None),
        ('六', None), ('七', None), ('八', None), ('九', None), ('十', None),
        ('有', None), ('没', None), ('不', None), ('也', None), ('就', None), ('还', None),
        ('都', None), ('却', None), ('而', None), ('但', None), ('且', None),
    ];
    if let Some(first) = chars.first().copied() {
        if non_name_starts.iter().any(|(c, _)| *c == first) {
            // [fix 2026-08-16] '向' 是常见姓氏（向明初/向华强）：「向」开头的 3 字及以上
            // 名字放行，仅拦截 2 字介词短语（向他/向前/向外）。
            if !(first == '向' && chars.len() >= 3) {
                return false;
            }
        }
    }
    // reject names that end with adverbial particles / common sentence fragments
    let junk_suffixes = ["连忙", "突然", "一直", "已经", "然后", "于是", "接着", "并且", "而且"];
    for s in junk_suffixes {
        if n.ends_with(s) {
            return false;
        }
    }
    if n.contains('的') || n.contains('了') || n.contains('着') || n.contains('在') {
        // allow classic 2-char names only if pure CJK without particles mid-word — already filtered
        if chars.iter().any(|c| matches!(c, '的' | '了' | '着' | '在' | '把' | '被')) {
            return false;
        }
    }
    let cjk = chars
        .iter()
        .filter(|&&c| matches!(c, '\u{4e00}'..='\u{9fff}' | '·' | '•'))
        .count();
    let latin = chars
        .iter()
        .filter(|&&c| c.is_ascii_alphabetic() || matches!(c, ' ' | '-' | '\''))
        .count();
    cjk == chars.len() || (latin == chars.len() && chars.len() >= 2)
}

fn pack_blurb(p: &StoryPack) -> String {
    // Prefer lore titled 简介 / 概述
    for lore in &p.lore_entries {
        let title = lore_text_field(lore, "title").unwrap_or_default();
        if title.contains("简介") || title.contains("概述") || title.eq_ignore_ascii_case("blurb")
        {
            if let Some(t) = lore_text_field(lore, "text").or_else(|| lore_text_field(lore, "content"))
            {
                return t.chars().take(140).collect();
            }
        }
    }
    // Any permanent lore text
    for lore in &p.lore_entries {
        let permanent = lore
            .get("permanent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if permanent {
            if let Some(t) = lore_text_field(lore, "text").or_else(|| lore_text_field(lore, "content"))
            {
                return t.chars().take(140).collect();
            }
        }
    }
    // First lore
    if let Some(lore) = p.lore_entries.first() {
        if let Some(t) = lore_text_field(lore, "text").or_else(|| lore_text_field(lore, "content")) {
            return t.chars().take(140).collect();
        }
    }
    // First node summary
    if let Some(n) = p.nodes.first() {
        let s = n.summary.trim();
        if !s.is_empty() {
            return s.chars().take(120).collect();
        }
    }
    String::new()
}

fn pack_cast_names(p: &StoryPack) -> Vec<String> {
    p.characters
        .iter()
        .filter(|c| {
            let role = c.role.to_ascii_lowercase();
            if role.contains("narrator") || role.contains("player") {
                return false;
            }
            let name = c.name.trim();
            if name.is_empty() || name == "旁白" || name == "读者" || name == "玩家" {
                return false;
            }
            is_clean_cast_name(name)
        })
        .map(|c| c.name.trim().to_string())
        .take(64)
        .collect()
}

/// Lightweight character refs for list responses (same filter as cast_names).
fn pack_character_summaries(p: &StoryPack) -> Vec<PackCharacterSummary> {
    p.characters
        .iter()
        .filter(|c| {
            let role = c.role.to_ascii_lowercase();
            if role.contains("narrator") || role.contains("player") {
                return false;
            }
            let name = c.name.trim();
            if name.is_empty() || name == "旁白" || name == "读者" || name == "玩家" {
                return false;
            }
            is_clean_cast_name(name)
        })
        .take(64)
        .map(|c| PackCharacterSummary {
            id: c.id.clone(),
            name: c.name.trim().to_string(),
            role: c.role.clone(),
            avatar: c.avatar.clone(),
            expressions: c.expressions.clone(),
            voice: c.voice.clone(),
        })
        .collect()
}

impl From<&StoryPack> for PackSummary {
    fn from(p: &StoryPack) -> Self {
        Self {
            id: p.id.clone(),
            title: p.title.clone(),
            max_tier: p.max_tier,
            chapter_count: p.chapters.len(),
            node_count: p.nodes.len(),
            character_count: p.characters.len(),
            playable: p.is_playable(),
            language: if p.language.is_empty() {
                "zh".into()
            } else {
                p.language.clone()
            },
            blurb: pack_blurb(p),
            cast_names: pack_cast_names(p),
            characters: pack_character_summaries(p),
            first_chapter_title: p
                .chapters
                .first()
                .map(|c| c.title.clone())
                .unwrap_or_default(),
            source_type: p.source.source_type.clone(),
        }
    }
}

// ─── Session ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EntryConfig {
    #[serde(default)]
    pub entry_role: Option<EntryRole>,
    #[serde(default)]
    pub vessel_character_id: Option<String>,
    #[serde(default)]
    pub meta_knowledge: MetaKnowledge,
    #[serde(default)]
    pub rewrite_intensity: RewriteIntensity,
    #[serde(default)]
    pub isekai: Option<Value>,
    #[serde(default)]
    pub extra_profile: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub control_character_id: Option<String>,
    #[serde(default)]
    pub persona: String,
    #[serde(default)]
    pub inventory: Vec<String>,
    #[serde(default)]
    pub flags: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryL1 {
    #[serde(default)]
    pub scene_summary: String,
    #[serde(default)]
    pub updated_at_turn: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryL2Event {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub turn: u32,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub actors: Vec<String>,
    #[serde(default)]
    pub node_id: Option<String>,
    /// Pre-computed embedding vector (ST-21: embedding RAG).
    #[serde(default)]
    pub embedding: Vec<f32>,
}

/// L2: short-horizon event log (capped).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryL2 {
    #[serde(default)]
    pub events: Vec<MemoryL2Event>,
    #[serde(default)]
    pub updated_at_turn: u32,
}

/// L3: fine-grained relationship edges / facts (capped).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryL3 {
    /// list of {from,to,rel,note,turn}
    #[serde(default)]
    pub edges: Vec<Value>,
    /// free-form facts
    #[serde(default)]
    pub facts: Vec<String>,
    /// [fix §10 2026-08-16] 永久层 facts：玩家显式声明的关键物品/收藏/承诺——
    /// 注入时不参与 take 裁剪（窝边草「素描原稿」被 take(6) 挤出的案例根治）。
    #[serde(default)]
    pub pinned: Vec<String>,
    #[serde(default)]
    pub updated_at_turn: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryL4 {
    /// characterId → affinity 0–100
    #[serde(default)]
    pub affinity: Value,
    #[serde(default)]
    pub secrets_known: Vec<String>,
    #[serde(default)]
    pub promises: Vec<String>,
    #[serde(default)]
    pub relationships: Value,
}

/// L2 事件账本上限收缩：保留最近 `max` 条；超限时按重要性淘汰
/// （romance 等关系关键事件保底，其余从头淘汰）。与 retain_guard_events
/// 同策略（denova 语义：可见轮次 + 压缩保留），2026-08-14 补 L2 无上限收缩缺口。
pub fn retain_l2_events(events: &[MemoryL2Event], max: usize) -> Vec<MemoryL2Event> {
    if max == 0 || events.is_empty() {
        return Vec::new();
    }
    if events.len() <= max {
        return events.to_vec();
    }
    let overflow = events.len() - max;
    // 第一遍：优先淘汰非关键事件（kind != romance / 不含关系关键词）
    let mut kept: Vec<MemoryL2Event> = events.to_vec();
    let mut removed = 0usize;
    let mut i = 0usize;
    while i < kept.len() && removed < overflow {
        let e = &kept[i];
        let critical = e.kind == "romance"
            || e.summary.contains("接吻")
            || e.summary.contains("亲嘴")
            || e.summary.contains("亲吻")
            || e.summary.contains("亲热")
            || e.summary.contains("吻")
            || e.summary.contains("承诺")
            || e.summary.contains("秘密");
        if !critical {
            kept.remove(i);
            removed += 1;
        } else {
            i += 1;
        }
    }
    // 第二遍：仍超限则从头淘汰（保窗口尾巴）
    while kept.len() > max {
        kept.remove(0);
    }
    kept
}

#[cfg(test)]
mod l2_event_tests {
    use super::*;

    fn ev(id: &str, kind: &str, summary: &str) -> MemoryL2Event {
        MemoryL2Event {
            id: id.into(),
            turn: 0,
            kind: kind.into(),
            summary: summary.into(),
            actors: vec![],
            node_id: None,
            embedding: vec![],
        }
    }

    #[test]
    fn l2_keeps_within_limit() {
        let evs: Vec<MemoryL2Event> = (0..5).map(|i| ev(&format!("e{i}"), "other", "x")).collect();
        assert_eq!(retain_l2_events(&evs, 10).len(), 5);
    }

    #[test]
    fn l2_trims_old_first() {
        let mut evs: Vec<MemoryL2Event> = (0..10).map(|i| ev(&format!("e{i}"), "other", "x")).collect();
        evs.push(ev("rom1", "romance", "亲吻"));
        let kept = retain_l2_events(&evs, 5);
        assert_eq!(kept.len(), 5);
        // romance 保底：最后一条必须是关键事件
        assert!(kept.last().unwrap().kind == "romance" || kept.last().unwrap().summary.contains("吻"));
    }

    #[test]
    fn l2_empty_and_zero() {
        assert!(retain_l2_events(&[], 5).is_empty());
        assert!(retain_l2_events(&[ev("e1", "other", "x")], 0).is_empty());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TavernMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// [Swipe 多备选 吞噬 Front Porch AI swipes] 同条回复的备选正文（regen 不覆盖，左右切换）。
    #[serde(default)]
    pub swipes: Vec<String>,
    #[serde(default)]
    pub swipe_index: usize,
    #[serde(default)]
    pub engine_tag: Option<EngineTag>,
    /// 消息流内嵌程序卡 HTML(吸收自梨园 show_html);前端沙箱 iframe 渲染。
    #[serde(default)]
    pub program: Option<String>,
    /// 模型推理/导演思考内容（折叠展示，玩家点箭头展开）。正文剥离出的
    /// 「好的我是XX…」自白段与 LLM reasoning_content 均归入此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// [token 显示 2026-08-16] 消息生成消耗的 token 总数（上游 SSE usage.total_tokens）。
    /// 默认 0 = 旧数据/未捕获；前端 stMsgMeta 开关显示「HH:MM · N tok」。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tokens: u32,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

// ─── 演出机 S1：Actor 状态系统（纯数据结构骨架）────────────────────────────

// P2-1: 情绪字段约定——角色 fields["emotion"] 的合法取值（驱动前端表情角标）。
// [P7] EMOTION_FIELD/EMOTION_VALUES 常量已删（无代码引用；约定以注释为准）：
//   字段名固定 "emotion"；合法值 平静/开心/愤怒/悲伤/害羞/惊讶/恐惧/厌恶/疲惫/心动。

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorStateSystem {
    #[serde(default = "default_actor_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub initial_actors: Vec<ActorStateInitialActor>,
    #[serde(default)]
    pub actors: std::collections::HashMap<String, ActorStateEntry>,
    #[serde(default)]
    pub trait_pools: Vec<ActorTraitPool>,
    #[serde(default)]
    pub archive: Vec<ActorArchiveSnapshot>,
    /// 创作罗盘（T2）：全书承诺 + 近期目标。随会话落盘，build_context_text 置顶注入。
    /// 空罗盘（默认）不注入。
    #[serde(default)]
    pub compass: Compass,
}

fn default_actor_schema_version() -> u32 {
    3
}

/// Pack 级 Actor 状态配置（吞噬 denova actor_state）。
/// create_from_pack 时据此初始化 session.actor_states：
/// initial_actors 声明哪些角色启用状态机 + 用哪个模板；templates 提供字段模板。
///
/// 字段生命周期契约(对齐 TavernWeave variable-systems 方法论):
/// - writer:    ST-26 LLM 更新块(经 min/max/options 校验后写回)
/// - reader:    前端 UI(display 字段)/叙事渲染
/// - renderer:  actor_states → 剧情上下文投影
/// - cleanup:   无(字段随 actor 存档归档)
/// - migration: schema_version 控制;旧存档按模板默认值补齐
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorStatePackConfig {
    #[serde(default = "default_actor_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub initial_actors: Vec<ActorStateInitialActor>,
    /// template_id → 字段模板（含数值范围/枚举/更新说明；value 为初始值）。
    #[serde(default)]
    pub templates: std::collections::HashMap<String, ActorStateTemplate>,
}

/// 角色状态模板：一组字段定义（数值范围/枚举/更新说明）+ 可选特质池。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorStateTemplate {
    #[serde(default)]
    pub fields: std::collections::HashMap<String, ActorFieldValue>,
    #[serde(default)]
    pub trait_pools: Vec<ActorTraitPool>,
}

impl ActorStatePackConfig {
    /// 把 pack 级配置物化为可落盘的 ActorStateSystem（不直接改 session 序列化结构）。
    pub fn to_system(&self) -> ActorStateSystem {
        let mut sys = ActorStateSystem {
            schema_version: self.schema_version,
            initial_actors: self.initial_actors.clone(),
            actors: std::collections::HashMap::new(),
            trait_pools: Vec::new(),
            archive: Vec::new(),
            compass: Compass::empty(),
        };
        for ia in &self.initial_actors {
            let mut entry = ActorStateEntry {
                template_id: ia.template_id.clone(),
                fields: std::collections::HashMap::new(),
                traits: Vec::new(),
            };
            if let Some(tpl) = self.templates.get(&ia.template_id) {
                entry.fields = tpl.fields.clone();
                // 收集该模板的 trait_pools（按 id 去重）
                for pool in &tpl.trait_pools {
                    if !sys.trait_pools.iter().any(|p| p.id == pool.id) {
                        sys.trait_pools.push(pool.clone());
                    }
                }
            }
            sys.actors.insert(ia.character_id.clone(), entry);
        }
        sys
    }
}

impl Default for ActorStateSystem {
    fn default() -> Self {
        Self {
            schema_version: default_actor_schema_version(),
            initial_actors: vec![],
            actors: std::collections::HashMap::new(),
            trait_pools: vec![],
            archive: vec![],
            compass: Compass::empty(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorStateInitialActor {
    #[serde(default)]
    pub character_id: String,
    #[serde(default)]
    pub template_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorStateEntry {
    #[serde(default)]
    pub template_id: String,
    #[serde(default)]
    pub fields: std::collections::HashMap<String, ActorFieldValue>,
    #[serde(default)]
    pub traits: Vec<ActorTraitInstance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorFieldValue {
    /// number | string | bool | enum | object | list
    #[serde(default)]
    pub value_type: String,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub display: Option<String>,
    /// ST-26: 该字段在剧情中应如何变化的说明（供 LLM 生成【状态更新】块参考）。
    #[serde(default)]
    pub update_instruction: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorTraitInstance {
    #[serde(default)]
    pub pool_id: String,
    #[serde(default)]
    pub pool_name: Option<String>,
    #[serde(default)]
    pub trait_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub source_turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorTraitPool {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub traits: Vec<ActorTraitDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorTraitDefinition {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub weight: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorArchiveSnapshot {
    #[serde(default)]
    pub character_id: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub state: Value,
    /// S6: 归档原因（auto | manual | story），由 archive_actor 写入。
    #[serde(default)]
    pub reason: String,
}

/// ST-26: 一回合中 LLM 输出的角色状态更新块（吞噬 denova actor_state）。
/// 由后端套入 min/max/type 校验后写入 session.actor_states 并落盘。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorStateUpdate {
    #[serde(default)]
    pub character_id: String,
    #[serde(default)]
    pub fields: std::collections::HashMap<String, Value>,
    #[serde(default)]
    pub add_traits: Vec<ActorTraitInstance>,
    #[serde(default)]
    pub remove_traits: Vec<String>,
}

// ─── 演出机 S3：规则检定（吞噬 denova TurnCheckRequest，d20 最小闭环）────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuleStateBinding {
    #[serde(default)]
    pub field: String, // "敌方.压力" → actor_id.field_id
    #[serde(default)]
    pub on_success: Option<Value>, // "+1" / 数值 / 其它
    #[serde(default)]
    pub on_fail: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuleCheck {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub dice: String,
    #[serde(default)]
    pub modifier: f64,
    #[serde(default)]
    pub failure_policy: String,
    #[serde(default)]
    pub difficulty_guidance: String,
    #[serde(default)]
    pub state_effect_guidance: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub must_check_examples: Vec<String>,
    #[serde(default)]
    pub skip_check_examples: Vec<String>,
    #[serde(default)]
    pub success_hint: String,
    #[serde(default)]
    pub failure_hint: String,
    #[serde(default)]
    pub state_bindings: Vec<RuleStateBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuleSystem {
    #[serde(default)]
    pub checks: Vec<RuleCheck>,
}

impl RuleSystem {
    /// 宽容解析 DirectorResolvedSnapshot.rule_system 的 Value；非对象/解析失败 → None
    pub fn from_value(v: &Value) -> Option<RuleSystem> {
        serde_json::from_value(v.clone()).ok()
    }
}

/// ST-27: LLM 输出的规则检定块（吞噬 denova TurnCheckRequest，精简版）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnCheckRequest {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub challenge: String,
    #[serde(default)]
    pub cost: String,
    #[serde(default)]
    pub difficulty: String, // very_easy|easy|normal|hard|very_hard
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub bonuses: Vec<TurnCheckBonus>,
    #[serde(default)]
    pub outcomes: TurnCheckOutcomes,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnCheckBonus {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnCheckOutcomes {
    #[serde(default)]
    pub critical_success: TurnCheckOutcome,
    #[serde(default)]
    pub success: TurnCheckOutcome,
    #[serde(default)]
    pub failure: TurnCheckOutcome,
    #[serde(default)]
    pub critical_failure: TurnCheckOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnCheckOutcome {
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub state_changes: Vec<TurnStateChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnStateChange {
    #[serde(default)]
    pub actor_id: String,
    #[serde(default)]
    pub field_id: String,
    #[serde(default)]
    pub change: f64,
    #[serde(default)]
    pub reason: String,
}

/// 单条检定历史（全量累积，导演台「检定结果」区块展示用）。与 last_check_results
///（仅最近一回合、注入 prompt 约束）不同，此字段跨回合累积所有检定，供人类审阅。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckHistoryEntry {
    #[serde(default)]
    pub action: String,
    /// critical_success|success|failure|critical_failure
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub result_text: String,
    #[serde(default)]
    pub natural: u32,
    #[serde(default)]
    pub total: f64,
    #[serde(default)]
    pub dc: f64,
    /// 命中角色字段变化（部分角色元数据，可空）。
    #[serde(default)]
    pub state_changes: Vec<TurnStateChange>,
    /// 发生时的回合序号（0 = 开场前）。
    #[serde(default)]
    pub turn: u64,
}

/// "1d20"→(1,20)；"2d6"→(2,6)；非法 → None（支持 XdY，X/Y ≥1）
pub fn parse_dice(spec: &str) -> Option<(usize, usize)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let idx = spec.find(['d', 'D'])?;
    let count = spec[..idx].trim().parse::<usize>().ok()?;
    let faces = spec[idx + 1..].trim().parse::<usize>().ok()?;
    if count < 1 || faces < 1 {
        return None;
    }
    Some((count, faces))
}

/// very_easy=5 easy=8 normal=12 hard=15 very_hard=18；未知 → 12
pub fn difficulty_to_dc(difficulty: &str) -> f64 {
    match difficulty.trim().to_ascii_lowercase().as_str() {
        "very_easy" => 5.0,
        "easy" => 8.0,
        "normal" => 12.0,
        "hard" => 15.0,
        "very_hard" => 18.0,
        _ => 12.0,
    }
}

pub struct CheckResult {
    pub natural: u32,
    pub total: f64,
    pub dc: f64,
    pub outcome: String,     // critical_success|success|failure|critical_failure
    pub result_text: String, // 命中档 result；为空则回退 template success_hint/failure_hint；再空则 challenge
    pub state_changes: Vec<TurnStateChange>,
}

/// roll：掷骰子(dice 解析，X 次 Y 面求和取自然骰；d20 只掷 1 次) + bonuses.value 求和 → total。
/// template: 命中 RuleSystem.checks[template_id]（找不到或 template_id 空 → None）。
/// dc = difficulty_to_dc(difficulty) + template.map(|t| t.modifier).unwrap_or(0.0)  // modifier 正数=更难
/// 判定：natural==20→critical_success；natural==1→critical_failure；total>=dc→success；否则 failure。
pub fn roll_check(
    req: &TurnCheckRequest,
    template: Option<&RuleCheck>,
    mut roll_d20: impl FnMut() -> u32,
) -> CheckResult {
    let dice = template
        .map(|t| t.dice.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("1d20");
    let (count, faces) = parse_dice(dice).unwrap_or((1, 20));
    let mut natural = 0u32;
    for _ in 0..count {
        let v = roll_d20();
        natural += if faces < 20 { ((v - 1) % faces as u32) + 1 } else { v };
    }
    let bonus: f64 = req.bonuses.iter().map(|b| b.value).sum();
    let total = natural as f64 + bonus;
    let dc = difficulty_to_dc(&req.difficulty) + template.map(|t| t.modifier).unwrap_or(0.0);
    let outcome = if natural == 20 {
        "critical_success"
    } else if natural == 1 {
        "critical_failure"
    } else if total >= dc {
        "success"
    } else {
        "failure"
    };
    let hit = match outcome {
        "critical_success" => &req.outcomes.critical_success,
        "success" => &req.outcomes.success,
        "failure" => &req.outcomes.failure,
        _ => &req.outcomes.critical_failure,
    };
    let mut result_text = hit.result.trim().to_string();
    if result_text.is_empty() {
        if let Some(t) = template {
            let hint = if matches!(outcome, "success" | "critical_success") {
                &t.success_hint
            } else {
                &t.failure_hint
            };
            if !hint.trim().is_empty() {
                result_text = hint.trim().to_string();
            }
        }
    }
    if result_text.is_empty() {
        result_text = req.challenge.clone();
    }
    let mut state_changes = hit.state_changes.clone();
    if let Some(t) = template {
        let success_side = matches!(outcome, "success" | "critical_success");
        for b in &t.state_bindings {
            let raw = if success_side { &b.on_success } else { &b.on_fail };
            let Some(raw) = raw else { continue };
            let field = b.field.trim();
            if field.is_empty() {
                continue;
            }
            let (actor_id, field_id) = match field.find('.') {
                Some(i) => (&field[..i], &field[i + 1..]),
                None => (field, ""),
            };
            let (change, reason) = match raw {
                Value::Number(n) => (n.as_f64().unwrap_or(0.0), format!("{field} 检定结算")),
                Value::String(s) => match s.trim().parse::<f64>() {
                    Ok(v) => (v, format!("{field} 检定结算")),
                    Err(_) => (0.0, s.trim().to_string()),
                },
                other => (0.0, other.to_string()),
            };
            state_changes.push(TurnStateChange {
                actor_id: actor_id.trim().to_string(),
                field_id: field_id.trim().to_string(),
                change,
                reason,
            });
        }
    }
    CheckResult {
        natural,
        total,
        dc,
        outcome: outcome.to_string(),
        result_text,
        state_changes,
    }
}

impl ActorStateSystem {
    /// 挂载创作罗盘（T2）：写入系统状态的 compass，供 build_context_text 置顶注入。
    pub fn mount_compass(&mut self, compass: Compass) {
        self.compass = compass;
    }

    /// 当前挂载的创作罗盘（只读）。
    pub fn compass(&self) -> &Compass {
        &self.compass
    }

    /// 渲染所有立着角色的当前状态为可读文本（供 system prompt 注入）。
    /// 创作罗盘段置顶输出于角色状态之前：author_intent/current_focus 非空各自输出
    /// 「【全书承诺】…」「【近期目标】…」；空罗盘不干扰既有输出（无角色+空罗盘仍为空串）。
    pub fn build_context_text(&self) -> String {
        let mut out = self.compass.render_block();
        if self.actors.is_empty() {
            return out;
        }
        for (character_id, entry) in &self.actors {
            out.push_str(&format!("# {}\n", character_id));
            for (name, fv) in &entry.fields {
                let val = fv.value.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "∅".into());
                let mut line = format!("- {}: {}", name, val);
                if let (Some(lo), Some(hi)) = (fv.min, fv.max) {
                    line.push_str(&format!(" ({lo}~{hi})"));
                }
                if let Some(instr) = fv.update_instruction.as_deref() {
                    if !instr.is_empty() {
                        line.push_str(&format!(" —— 更新说明: {instr}"));
                    }
                }
                out.push_str(&line);
                out.push('\n');
            }
            for t in &entry.traits {
                let mut line = format!("- trait: {} ({})", t.name, t.trait_id);
                if let Some(s) = &t.summary {
                    line.push_str(&format!(" —— {s}"));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }

    /// 应用一批状态更新，返回发生变更的字段数。
    /// upsert：character_id 无 entry 时新建；fields 按 update.fields 覆盖。
    /// add_traits append（若同名 trait_id 已存在则忽略）。
    /// remove_traits 按 trait_id 移除。
    pub fn apply_updates(&mut self, updates: &[ActorStateUpdate]) -> usize {
        let mut changed = 0usize;
        for up in updates {
            if up.character_id.is_empty() {
                continue;
            }
            let entry = self.actors.entry(up.character_id.clone()).or_default();
            for (name, val) in &up.fields {
                entry.fields.entry(name.clone()).or_default().value = Some(val.clone());
                changed += 1;
            }
            for t in &up.add_traits {
                if !entry.traits.iter().any(|e| e.trait_id == t.trait_id) {
                    entry.traits.push(t.clone());
                }
            }
            entry.traits.retain(|t| !up.remove_traits.contains(&t.trait_id));
        }
        changed
    }

    /// 按 actor_id+field_id 定位 number 字段，加 change 写回；找不到数值字段则忽略。
    /// 返回成功更新数。value 为 JSON number 或字符串可解析 f64 均可。
    pub fn apply_state_changes(&mut self, changes: &[TurnStateChange]) -> usize {
        let mut updated = 0usize;
        for ch in changes {
            if ch.actor_id.is_empty() || ch.field_id.is_empty() {
                continue;
            }
            let Some(entry) = self.actors.get_mut(&ch.actor_id) else {
                continue;
            };
            let Some(fv) = entry.fields.get_mut(&ch.field_id) else {
                continue;
            };
            let current = match &fv.value {
                Some(Value::Number(n)) => n.as_f64(),
                Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
                _ => None,
            };
            let Some(cur) = current else {
                continue;
            };
            fv.value = Some(json!(cur + ch.change));
            updated += 1;
        }
        updated
    }

    /// S6: 将角色当前状态归档为快照（吞噬 denova actor_archive）。
    /// 返回新快照；角色不存在返回 None。调用方负责把 system 写回。
    pub fn archive_actor(
        &mut self,
        character_id: &str,
        reason: &str,
        created_at: &str,
    ) -> Option<ActorArchiveSnapshot> {
        let entry = self.actors.get(character_id)?;
        let state = serde_json::to_value(entry).ok()?;
        let snap = ActorArchiveSnapshot {
            character_id: character_id.to_string(),
            created_at: created_at.to_string(),
            state,
            reason: reason.to_string(),
        };
        self.archive.push(snap.clone());
        Some(snap)
    }

    /// S6: 从归档恢复角色状态（取最近一次含该角色的快照覆盖 actors[character_id]）。
    pub fn restore_actor(&mut self, character_id: &str) -> bool {
        for snap in self.archive.iter().rev() {
            if snap.character_id != character_id {
                continue;
            }
            if let Ok(entry) = serde_json::from_value::<ActorStateEntry>(snap.state.clone()) {
                self.actors.insert(character_id.to_string(), entry);
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TavernSession {
    pub session_id: String,
    pub pack_id: String,
    #[serde(default)]
    pub pack_missing: bool,
    /// F1: owner user_id of this session. None on legacy data (treated as
    /// restricted — only the creating user can access, falling back to
    /// "first claimant" when unverifiable).
    #[serde(default)]
    pub owner: Option<String>,
    /// P0: 回合生成写作档位（lite/standard/heavy）。默认 lite = 现状单次直出。
    #[serde(default)]
    pub quality: Quality,
    pub playable: Playable,
    pub play_mode: PlayMode,
    /// Frozen at create: min(user, pack/card, global)
    pub content_tier: ContentTier,
    #[serde(default)]
    pub user_tier_request: ContentTier,
    #[serde(default)]
    pub entry: EntryConfig,
    #[serde(default)]
    pub chapter_cursor: Option<String>,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub resume_node_id: Option<String>,
    /// True once opening monologue was seeded (create / ensure-opening / side enter).
    #[serde(default)]
    pub opening_seeded: bool,
    /// Active side-branch node (playMode=side after enter).
    #[serde(default)]
    pub side_branch_node_id: Option<String>,
    /// Display label for active side branch.
    #[serde(default)]
    pub side_branch_label: Option<String>,
    /// 最近一次回档的存档 id（世界线分叉检测用，吸收自 Liyuan worldline.ts）。
    #[serde(default)]
    pub last_restored_save_id: Option<String>,
    /// 当前所在世界线 id（"main"=主线；回档后走出不同路再存档 → 新线）。
    #[serde(default)]
    pub current_worldline_id: Option<String>,
    /// Agent 自建面板（吸收自 Liyuan panels.ts：地图/装备库/线索板等舞台美术层）。
    #[serde(default)]
    pub panels: Vec<TavernPanel>,
    /// MCP 外设工具结果(吸收自 Liyuan mcp.ts, 默认仅本机 stdio server)。
    /// 上一轮【工具】调用的结果, 会在下一轮构建 system prompt 时回填给 LLM。
    #[serde(default)]
    pub mcp_tool_results: Vec<ToolResultBrief>,
    /// skill 工具按需加载结果（吸收自 denova skill.NewMiddleware：模型声明需要完整 SKILL.md 后，下轮构建 system prompt 时注入全文）。
    /// 上轮【技能加载】块触发时写入；下轮 read 到后注入并保持（可被新请求覆盖）。
    #[serde(default)]
    pub skill_load: Option<SkillLoadInfo>,
    #[serde(default = "default_timeline")]
    pub timeline_id: String,
    #[serde(default)]
    pub turn: u32,
    #[serde(default)]
    pub present_character_ids: Vec<String>,
    /// Current focus NPC for multi-speaker turns (ST-10). Rotates among presentCharacterIds.
    #[serde(default)]
    pub focus_character_id: Option<String>,
    /// When true (default for P2), rotate focus after each completed turn.
    #[serde(default = "default_true")]
    pub speaker_rotation: bool,
    #[serde(default)]
    pub player: PlayerState,
    #[serde(default)]
    pub memory_l1: MemoryL1,
    #[serde(default)]
    pub memory_l2: MemoryL2,
    #[serde(default)]
    pub memory_l3: MemoryL3,
    #[serde(default)]
    pub memory_l4: MemoryL4,
    /// P2 (叙界守卫): 生成后多维守卫的违规记录（high=打回阻止推进 / med=仅提示）。
    #[serde(default)]
    pub guard_events: Vec<String>,
    /// L0 recent turns (full transcript retained; prompt uses last N)
    #[serde(default)]
    pub messages: Vec<TavernMessage>,
    #[serde(default)]
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub adult_confirmed: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    /// Author Zone project this session writes docs into (AZ-1+).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_project_id: Option<String>,
    /// Relative works path for live scroll doc, e.g. projects/{id}/sessions/{sid}/live.md
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_live_path: Option<String>,
    /// AZ-6: when false, skip live append even if path set.
    #[serde(default = "default_true")]
    pub author_live_enabled: bool,
    /// AZ-6: append live every N completed turns (1 = every turn).
    #[serde(default = "default_live_every_n")]
    pub author_live_every_n: u32,
    /// AZ-6: also write sessions/.../turns/{n}.md per turn (fail-open).
    #[serde(default)]
    pub author_live_write_turns: bool,
    /// Actor 状态机（吞噬 denova actor-state；空 = 未启用）。
    #[serde(default)]
    pub actor_states: ActorStateSystem,
    /// 会话级世界状态（T2 U2/U7）：实体+关系+事件账本。create_from_pack 时从
    /// actor_state_config 播种 Character 实体；叙事事件经 /world-events 应用。
    #[serde(default)]
    pub world: WorldState,
    /// 游戏时钟+天气权威状态（时间/天气约束系统）。旧数据 default 播种（清晨/晴）。
    #[serde(default)]
    pub game_clock: crate::time_clock::GameClock,
    /// [吞噬 Front Porch AI pockets.dart] 口袋与衣物（per-character, per-session）。
    /// per-character 口袋（worn/carrying/setAside），per-session 隔离。key = character_id。
    /// GameClock 的 day 用于 setAside 晨间过期（clothing 次日清晨过期，possessions 永不过期）。
    #[serde(default)]
    pub pockets: std::collections::HashMap<String, crate::pockets::Pockets>,
    /// [P1-B Porch Life À la carte] 口袋开关（默认开）。关时提示词不注入口袋块，
    /// 但数据仍保留（可再打开）。对齐 Front Porch Porch Life "Own switch. Does not
    /// need the Realism Engine."。
    #[serde(default = "default_true")]
    pub pockets_enabled: bool,
    /// [P2+P3 吞噬 Front Porch AI] Needs 六维 + Growth Rings + World Climate（per-character/per-session，默认空）。
    #[serde(default)]
    pub needs: std::collections::HashMap<String, crate::needs::Needs>,
    #[serde(default)]
    pub growth: crate::character_arc::GrowthStore,
    #[serde(default)]
    pub world_climate: crate::world_climate::WorldClimate,
    /// [P4 吞噬 Front Porch AI chaos/tiers/objectives/dreams] Chaos + Tiers + 目标 + 夜梦（默认空/关）。
    #[serde(default)]
    pub chaos: crate::chaos::ChaosState,
    #[serde(default)]
    pub milestones: Vec<crate::relationship_tiers::Milestone>,
    #[serde(default)]
    pub objectives: Vec<crate::objectives::Objective>,
    #[serde(default)]
    pub ambitions: Vec<crate::objectives::Ambition>,
    #[serde(default)]
    pub dream: crate::dreams::DreamState,
    #[serde(default)]
    pub episodes: crate::dreams::EpisodeStore,
    /// [Journal 存量 吞噬 Front Porch AI journal_store] per-session卡片库（热卡常驻，冷卡按需召回）。
    #[serde(default)]
    pub journal: crate::journal_store::JournalStore,
    /// [羁绊活数值 吞噬 Front Porch AI relationship_service] per-character bond/trust 动态。
    #[serde(default)]
    pub relationships: std::collections::HashMap<String, crate::relationship::Bond>,
    /// [Swipe 多备选] 待继承的备选正文（reroll 时旧正文暂存，下条 assistant 消息继承）。
    #[serde(default)]
    pub pending_swipes: Vec<String>,
    /// [承诺债务 吞噬 Front Porch AI promise_debt] open/kept/broken 追踪。
    #[serde(default)]
    pub promises: crate::promise::PromiseStore,
    /// [偏好加权 吞噬 Front Porch AI preference_scoring] per-character likes/dislikes。
    #[serde(default)]
    pub preferences: std::collections::HashMap<String, crate::promise::Prefs>,
    /// [在场推导 吞噬 Front Porch AI presence_derive] per-character occupation/hours/workdays。
    #[serde(default)]
    pub presence: std::collections::HashMap<String, crate::mood_presence::Presence>,
    /// [世界书定时 吞噬 Front Porch AI lorebook_timed_effects] per-session sticky/cooldown（消息序号计）。
    #[serde(default)]
    pub timed_world_info: crate::st_world_info::TimedWorldInfo,
    /// [全自动事件提取] 回合末后台 LLM 提取物品/承诺/成长/羁绊（默认开，小模型低 token）。
    #[serde(default = "default_true")]
    pub event_extract: bool,
    /// S4: 导演计划（吞噬 denova director_plan）。None = 尚未生成。仅为叙事意图，不改写 locked_beats。
    #[serde(default)]
    pub director_plan: Option<DirectorPlan>,
    /// S4: 导演计划手动触发挂起标记：POST director-plan/run 置 true，下一回合 start_turn 附加生成指令。
    #[serde(default)]
    pub director_pending: bool,
    /// G13/G14: 当前后台导演任务 id（如 "director_plan_update" / "opening_plan"）。
    /// 后台任务运行期间登记，GET director-config 可见；任务结束清空为 None。
    #[serde(default)]
    pub director_task: Option<String>,
    /// S5: 最近一回合抽到的事件卡（None = 尚未抽取/无可用卡）。
    #[serde(default)]
    pub last_event: Option<EventLogEntry>,
    /// P12 (2026-08-15): 最近一回合的检定结果文本（含失败 outcome）。下回合构建
    /// system prompt 时注入为「剧情约束」——检定失败（含 critical_failure）必须
    /// 在正文中真实体现（如母亲明确拒绝/拉开距离/尖叫），不再只是事后 append 展示。
    #[serde(default)]
    pub last_check_results: Vec<String>,
    /// 跨回合累积的全部检定历史（导演台「检定结果」区块展示用）。
    #[serde(default)]
    pub check_history: Vec<CheckHistoryEntry>,
    /// P0-1: 回合检查点（cap 30，吸收自梨园 story_command /rewind /reroll）。
    #[serde(default)]
    pub checkpoints: Vec<TurnCheckpoint>,
    /// U11: epoch 压缩代数 —— 上下文窗口阈值驱动压缩（替换机械 turn%8），每次触发 +1。
    #[serde(default)]
    pub epoch: u32,
    /// U11: 最近一次 epoch 压缩发生的回合。
    #[serde(default)]
    pub epoch_last_turn: Option<u32>,
    /// U11: 最近一次 epoch 压缩触发时的估算上下文字符数。
    #[serde(default)]
    pub epoch_last_chars: Option<u32>,
    /// U11: 多轮累计成本/耗时记账（每次回合终态增量累计；serde default 兼容旧会话文件）。
    #[serde(default)]
    pub turn_cost_ledger: TurnCostLedger,
    /// G10: 最近一次回合提交诊断摘要（stop_turn 或回合完成时写入；serde default 兼容旧会话文件）。
    #[serde(default)]
    pub last_turn_diagnostic: Option<TurnDiagnostic>,
    /// X3 (吞噬自 xiami skimming.rs): 最近一次正文定稿后的速读质检问题（诊断展示用）。
    #[serde(default)]
    pub xiami_skim_issues: Vec<SkimIssue>,
    /// X3: 最近一次速读质检的正文摘录（前 200 字，展示用）。
    #[serde(default)]
    pub xiami_skim_sample: String,
    /// [morphling Wave B3 2026-08-16] 章节剧情摘要账本（吸收自 SillyTavern-BakemonoMemory
    /// summary-memory-model）：每章结束/滚动时由 LLM 提炼「本章剧情进展」，注入 system prompt
    /// 供跨章回顾；UI 可查看/编辑。默认模式 = 生成时顺带总结（C2）；fallback 提炼按
    /// diary_config 的回合/事件阈值触发。
    #[serde(default)]
    pub chapter_diaries: Vec<ChapterDiaryEntry>,
    /// [morphling C3 2026-08-16] 章节摘要 fallback 提炼配置（None = 默认 10 回合 / 20 事件）。
    #[serde(default)]
    pub diary_config: Option<ChapterDiaryConfig>,
    /// [morphling ROMA P0 2026-08-19] 当前回合执行阶段进度（崩溃/中断恢复诊断用）。
    /// 由 turn 后台 worker 在阶段边界写入；中断（流断开/服务重启/超时）时保留现场，
    /// U11 resume 据此判断「上回合死在哪一步」并记录在 resumed 诊断里（不改故事流）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_progress: Option<TurnProgress>,
}

/// [morphling ROMA P0 2026-08-19] 回合进度阶段。
/// 参考 ROMA EventLoopController 的 READY→EXECUTING→COMPLETED/FAILED 状态机——
/// 但这里仅为「中断处快照」用途（幂等观察），不做 DAG 恢复，保持零回归风险。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnPhase {
    /// 已 acquire 回合锁，尚未开始 LLM 流式（含 F3 队列判定后）。
    Queued,
    /// LLM 流式正文生成中（主流 -> 质量管道入口）。
    Streaming,
    /// 已进入质量管道（Quality Refine / 技能模板 stage）。
    Quality,
    /// 正文已定稿、assistant 消息已入列、turn 已 +1（回合完成前的写盘阶段）。
    Persisting,
    /// 回合已完成落盘。
    Done,
}

/// [morphling ROMA P0 2026-08-19] 单条回合进度快照（跟随会话持久化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnProgress {
    pub turn: u32,
    pub phase: TurnPhase,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub at: String,
}

/// 章节剧情摘要条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterDiaryEntry {
    pub chapter_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub start_turn: u32,
    #[serde(default)]
    pub end_turn: u32,
    #[serde(default)]
    pub updated_at_turn: u32,
    /// 用户手动编辑过（或已确认）→ 自动提炼不再覆盖。
    #[serde(default)]
    pub manual_edited: bool,
}

/// [morphling C3 2026-08-16] 章节摘要 fallback 提炼触发配置（None = 默认 10 回合 / 20 事件）。
/// 默认模式是「生成时顺带总结」（每回合自动），此配置只控制 LLM 未输出摘要块时的兜底提炼。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterDiaryConfig {
    /// 每 N 回合兜底提炼一次。
    #[serde(default = "default_diary_turn_interval")]
    pub turn_interval: u32,
    /// 本章事件数 ≥ N 时兜底提炼。
    #[serde(default = "default_diary_event_threshold")]
    pub event_threshold: u32,
}

fn default_diary_turn_interval() -> u32 {
    10
}

fn default_diary_event_threshold() -> u32 {
    20
}

impl Default for ChapterDiaryConfig {
    fn default() -> Self {
        Self {
            turn_interval: default_diary_turn_interval(),
            event_threshold: default_diary_event_threshold(),
        }
    }
}

/// U11: 多轮累计成本/耗时记账（job 级单回合明细在 payload.u11，此处为会话级累计账本）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCostLedger {
    /// 已完成并记账的回合数。
    #[serde(default)]
    pub turns: u32,
    /// 累计 LLM 调用次数（主流 + 质量管道 + 非阻塞后处理，估算口径）。
    #[serde(default)]
    pub llm_calls: u32,
    /// 累计回合耗时（毫秒）。
    #[serde(default)]
    pub total_duration_ms: u64,
    /// 累计估算成本（USD）。
    #[serde(default)]
    pub est_cost_usd: f64,
}

/// G10: 最近一次回合的提交诊断摘要（幂等回执 + 诊断信息）。
/// `stop_turn` 完成（取消回合）或回合正常完成时写入 `TavernSession.last_turn_diagnostic`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiagnostic {
    /// 该回合的回合号（正常完成 = 已完成回合数；被停止 = 在途回合号 turn+1）。
    #[serde(default)]
    pub turn: u32,
    /// 回合是否被接受并正常完成（true = 已产出对白；false = 被停止/取消）。
    #[serde(default)]
    pub accepted: bool,
    /// 回合耗时（毫秒）。
    #[serde(default)]
    pub duration_ms: u64,
    /// LLM 是否成功（对白生成成功）。
    #[serde(default)]
    pub llm_ok: bool,
}

fn default_timeline() -> String {
    "main".into()
}

fn default_true() -> bool {
    true
}

fn default_live_every_n() -> u32 {
    1
}

// ─── Persona (cross-session L4) ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TavernPersona {
    pub character_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub memory_l4: MemoryL4,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub updated_at: String,
}

// ─── Create session request ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub pack_id: String,
    #[serde(default)]
    pub playable: Playable,
    #[serde(default)]
    pub play_mode: PlayMode,
    #[serde(default)]
    pub user_tier: ContentTier,
    #[serde(default)]
    pub global_tier: Option<ContentTier>,
    #[serde(default)]
    pub entry: Option<EntryConfig>,
    #[serde(default)]
    pub player_display_name: Option<String>,
    #[serde(default)]
    pub adult_confirmed: bool,
    #[serde(default)]
    pub title: Option<String>,
    /// F1: owner user_id (set by server handler from session_from).
    #[serde(default)]
    pub owner: Option<String>,
    /// P0: 回合生成写作档位（lite/standard/heavy），默认 lite。
    #[serde(default)]
    pub quality: Quality,
    /// Optional Author Zone project to bind on create (AZ-1+).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_project_id: Option<String>,
    /// U13: Optional work id. When present and that work has a saved creation
    /// compass (CompassStore), auto-mount it on the new session at create time
    /// (so new sessions no longer need a re-PUT after setting the compass).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn safe_id(id: &str) -> CoreResult<String> {
    let s = id.trim();
    if s.is_empty()
        || s.contains('/')
        || s.contains('\\')
        || s.contains("..")
        || s.chars().any(|c| c.is_control())
    {
        return Err(CoreError::BadRequest("invalid id".into()));
    }
    Ok(s.to_string())
}

fn write_atomic(path: &Path, body: &str) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // 通用兜底：清除 U+FFFD 替换符。上游 LLM 会偶发把单字损坏成 2-3 个 FFFD，
    // 此字符是坏字残骸、无恢复语义，落盘前一律剔除，避免脏字符进入任何持久化文件。
    let body = body.replace('\u{FFFD}', "");
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

// ─── PackStore ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PackStore {
    data: DataRoot,
    lock: std::sync::Arc<Mutex<()>>,
}

impl PackStore {
    pub fn new(data: DataRoot) -> Self {
        let _ = data.ensure_layout();
        Self {
            data,
            lock: std::sync::Arc::new(Mutex::new(())),
        }
    }

    fn packs_root(&self) -> PathBuf {
        self.data.story_packs_dir()
    }

    /// 暴露 pack 目录（正文 bodyPath 相对此目录）——剧情助手全书检索等场景用。
    pub fn pack_dir(&self, id: &str) -> CoreResult<PathBuf> {
        let id = safe_id(id)?;
        Ok(self.packs_root().join(id))
    }

    fn pack_json_path(&self, id: &str) -> CoreResult<PathBuf> {
        Ok(self.pack_dir(id)?.join("pack.json"))
    }

    pub fn list(&self) -> CoreResult<Vec<PackSummary>> {
        let _g = self.lock.lock();
        let root = self.packs_root();
        fs::create_dir_all(&root)?;
        let mut out = Vec::new();
        for ent in fs::read_dir(&root)? {
            let ent = ent?;
            if !ent.file_type()?.is_dir() {
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            // [fix 2026-08-16] 跳过备份目录（*-bak-*）：备份 pack.json 内 id 未改写，
            // 会与主 pack 同 id 重复返回（窝边草 pack 曾因此列出两份）。
            if name.contains("-bak-") || name.ends_with("-bak") {
                continue;
            }
            match self.load_unlocked(&name) {
                Ok(p) => out.push(PackSummary::from(&p)),
                Err(e) => tracing::warn!(pack=%name, err=%e, "skip unreadable pack"),
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    pub fn get(&self, id: &str) -> CoreResult<StoryPack> {
        let _g = self.lock.lock();
        self.load_unlocked(id)
    }

    fn load_unlocked(&self, id: &str) -> CoreResult<StoryPack> {
        let path = self.pack_json_path(id)?;
        if !path.exists() {
            return Err(CoreError::NotFound(format!("pack not found: {id}")));
        }
        let raw = fs::read_to_string(&path)?;
        let mut pack: StoryPack = serde_json::from_str(&raw)?;
        // T 层 (2026-08-19): 旁挂 worldline.json（静态蒸馏时间线）加载进 pack.worldline。
        // lenient：文件缺失/解析失败静默空（旧 pack 或未蒸馏 -> 零影响），不阻塞 pack 加载。
        let wl_path = self.pack_dir(id)?.join("worldline.json");
        if let Ok(wl_raw) = fs::read_to_string(&wl_path) {
            if let Ok(list) = serde_json::from_str::<Vec<Value>>(&wl_raw) {
                pack.worldline = list;
            } else {
                tracing::warn!(pack=%id, "worldline.json 解析失败，时间线为空");
            }
        }
        Ok(pack)
    }

    pub fn save(&self, pack: StoryPack) -> CoreResult<StoryPack> {
        self.save_inner(pack, None)
    }

    /// CAS write: only persists if the on-disk `updated_at` still equals `base_revision`.
    pub fn save_with_revision(
        &self,
        pack: StoryPack,
        base_revision: &str,
    ) -> CoreResult<StoryPack> {
        self.save_inner(pack, Some(base_revision))
    }

    fn save_inner(
        &self,
        mut pack: StoryPack,
        base_revision: Option<&str>,
    ) -> CoreResult<StoryPack> {
        let _g = self.lock.lock();
        let is_new = pack.id.trim().is_empty();
        if is_new {
            pack.id = format!("pack-{}", Uuid::new_v4());
        }
        let id = safe_id(&pack.id)?;
        pack.id = id.clone();
        let now = now_rfc3339();
        if pack.created_at.is_empty() {
            pack.created_at = now.clone();
        }
        pack.updated_at = now;
        if pack.language.is_empty() {
            pack.language = "zh".into();
        }

        // Revision CAS: existing packs must be saved on top of the revision the caller read.
        if !is_new {
            if let Some(base) = base_revision {
                let current = match self.load_unlocked(&id) {
                    Ok(c) => c,
                    Err(CoreError::NotFound(_)) => {
                        return Err(CoreError::Conflict(
                            "pack 已被其他操作删除，请重新加载".into(),
                        ));
                    }
                    Err(e) => return Err(e),
                };
                if current.updated_at != base {
                    return Err(CoreError::Conflict(format!(
                        "pack 已被其他操作更新，请重新加载后再保存 (期望 revision {base}，当前 {})",
                        current.updated_at
                    )));
                }
            }
        }

        let dir = self.pack_dir(&id)?;
        fs::create_dir_all(dir.join("chapters"))?;

        // Write chapter bodies if body_path empty but we have content in goals? — no, bodies separate.
        let path = self.pack_json_path(&id)?;
        write_atomic(&path, &serde_json::to_string_pretty(&pack)?)?;
        Ok(pack)
    }

    pub fn write_chapter_body(&self, pack_id: &str, rel_path: &str, body: &str) -> CoreResult<()> {
        let _g = self.lock.lock();
        let id = safe_id(pack_id)?;
        let rel = rel_path.trim().trim_start_matches('/');
        if rel.is_empty() || rel.contains("..") || Path::new(rel).is_absolute() {
            return Err(CoreError::BadRequest("invalid chapter path".into()));
        }
        let full = self.pack_dir(&id)?.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&full, body)?;
        Ok(())
    }

    pub fn read_chapter_body(&self, pack_id: &str, rel_path: &str) -> CoreResult<String> {
        let _g = self.lock.lock();
        let id = safe_id(pack_id)?;
        let rel = rel_path.trim().trim_start_matches('/');
        if rel.is_empty() || rel.contains("..") {
            return Err(CoreError::BadRequest("invalid chapter path".into()));
        }
        let full = self.pack_dir(&id)?.join(rel);
        if !full.exists() {
            return Err(CoreError::NotFound(format!("chapter body missing: {rel}")));
        }
        Ok(fs::read_to_string(full)?)
    }

    pub fn delete(&self, id: &str) -> CoreResult<()> {
        let _g = self.lock.lock();
        let dir = self.pack_dir(id)?;
        if !dir.exists() {
            return Err(CoreError::NotFound(format!("pack not found: {id}")));
        }
        fs::remove_dir_all(dir)?;
        Ok(())
    }


    /// Export pack directory as zip bytes (pack.json + chapters/*).
    pub fn export_zip(&self, pack_id: &str) -> CoreResult<Vec<u8>> {
        let _g = self.lock.lock();
        let id = safe_id(pack_id)?;
        let dir = self.pack_dir(&id)?;
        if !dir.exists() {
            return Err(CoreError::NotFound(format!("pack not found: {id}")));
        }
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zip = ZipWriter::new(cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            // walk dir
            fn add_dir(
                zip: &mut ZipWriter<Cursor<&mut Vec<u8>>>,
                opts: SimpleFileOptions,
                base: &Path,
                rel: &Path,
            ) -> CoreResult<()> {
                let full = base.join(rel);
                if full.is_dir() {
                    for ent in fs::read_dir(&full)? {
                        let ent = ent?;
                        let name = ent.file_name();
                        let child_rel = rel.join(&name);
                        add_dir(zip, opts, base, &child_rel)?;
                    }
                } else if full.is_file() {
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    zip.start_file(rel_str, opts)
                        .map_err(|e| CoreError::BadRequest(format!("zip start: {e}")))?;
                    let data = fs::read(&full)?;
                    zip.write_all(&data)
                        .map_err(|e| CoreError::BadRequest(format!("zip write: {e}")))?;
                }
                Ok(())
            }
            add_dir(&mut zip, opts, &dir, Path::new("pack.json"))?;
            let chapters = dir.join("chapters");
            if chapters.is_dir() {
                for ent in fs::read_dir(&chapters)? {
                    let ent = ent?;
                    if ent.file_type()?.is_file() {
                        let name = ent.file_name();
                        let rel = Path::new("chapters").join(name);
                        add_dir(&mut zip, opts, &dir, &rel)?;
                    }
                }
            }
            zip.finish()
                .map_err(|e| CoreError::BadRequest(format!("zip finish: {e}")))?;
        }
        Ok(buf)
    }

    /// Import pack from zip bytes. Optional id override. Returns saved StoryPack.
    pub fn import_zip(&self, zip_bytes: &[u8], id_override: Option<String>) -> CoreResult<StoryPack> {
        const MAX_PACK_ENTRIES: usize = 2000;
        const MAX_PACK_ENTRY_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB per entry
        const MAX_PACK_TOTAL_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB total expanded
        const MAX_PACK_RATIO: u64 = 200;
        let _g = self.lock.lock();
        let cursor = Cursor::new(zip_bytes);
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| CoreError::BadRequest(format!("invalid zip: {e}")))?;
        if archive.len() > MAX_PACK_ENTRIES {
            return Err(CoreError::BadRequest(format!(
                "zip entry count {} exceeds limit {MAX_PACK_ENTRIES}",
                archive.len()
            )));
        }
        // Find pack.json
        let mut pack_json: Option<String> = None;
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut declared_total: u64 = 0;
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| CoreError::BadRequest(format!("zip entry: {e}")))?;
            let name = file.name().replace('\\', "/").trim_start_matches('/').to_string();
            if name.is_empty() || name.ends_with('/') || name.contains("..") {
                continue;
            }
            // Budget checks against declared sizes before decompressing.
            let (declared, compressed) = (file.size(), file.compressed_size());
            if declared > MAX_PACK_ENTRY_BYTES {
                return Err(CoreError::BadRequest(format!(
                    "zip entry {name} declared {declared} bytes exceeds limit"
                )));
            }
            declared_total = declared_total.saturating_add(declared);
            if declared_total > MAX_PACK_TOTAL_BYTES {
                return Err(CoreError::BadRequest(
                    "zip expanded size exceeds limit".to_string(),
                ));
            }
            if declared > 0 && compressed > 0 && declared / compressed > MAX_PACK_RATIO {
                return Err(CoreError::BadRequest(format!(
                    "zip entry {name} compression ratio exceeds limit"
                )));
            }
            let mut data = Vec::new();
            file.by_ref()
                .take(MAX_PACK_ENTRY_BYTES + 1)
                .read_to_end(&mut data)
                .map_err(|e| CoreError::BadRequest(format!("zip read: {e}")))?;
            if data.len() as u64 > MAX_PACK_ENTRY_BYTES {
                return Err(CoreError::BadRequest(format!(
                    "zip entry {name} expanded beyond limit"
                )));
            }
            if name == "pack.json" || name.ends_with("/pack.json") {
                pack_json = Some(String::from_utf8_lossy(&data).into_owned());
            }
            files.push((name, data));
        }
        let pack_raw = pack_json.ok_or_else(|| CoreError::BadRequest("zip missing pack.json".into()))?;
        let mut pack: StoryPack = serde_json::from_str(&pack_raw)
            .map_err(|e| CoreError::BadRequest(format!("pack.json parse: {e}")))?;
        if let Some(id) = id_override.filter(|s| !s.trim().is_empty()) {
            pack.id = id;
        }
        if pack.id.trim().is_empty() {
            pack.id = format!("pack-{}", Uuid::new_v4());
        }
        let id = safe_id(&pack.id)?;
        pack.id = id.clone();
        let now = now_rfc3339();
        if pack.created_at.is_empty() {
            pack.created_at = now.clone();
        }
        pack.updated_at = now;
        if pack.language.is_empty() {
            pack.language = "zh".into();
        }
        // if id exists, append suffix
        let mut final_id = id.clone();
        let mut dir = self.pack_dir(&final_id)?;
        if dir.exists() {
            final_id = format!("{id}-imp-{}", &Uuid::new_v4().to_string()[..8]);
            pack.id = final_id.clone();
            dir = self.pack_dir(&final_id)?;
        }
        fs::create_dir_all(dir.join("chapters"))?;
        // write files
        for (name, data) in files {
            let rel = if let Some(rest) = name.strip_prefix("pack.json") {
                if rest.is_empty() {
                    "pack.json".to_string()
                } else {
                    // nested pack.json path — only use basename files under chapters
                    continue;
                }
            } else if name.ends_with("/pack.json") {
                "pack.json".to_string()
            } else if let Some(idx) = name.rfind("chapters/") {
                name[idx..].to_string()
            } else if name.starts_with("chapters/") {
                name
            } else {
                continue;
            };
            if rel.contains("..") {
                continue;
            }
            if rel == "pack.json" {
                continue; // write from struct below
            }
            let full = dir.join(&rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent)?;
            }
            write_atomic(
                &full,
                &String::from_utf8_lossy(&data),
            )?;
        }
        let path = dir.join("pack.json");
        write_atomic(&path, &serde_json::to_string_pretty(&pack)?)?;
        Ok(pack)
    }

    /// Seed demo pack if missing. Idempotent.
    pub fn ensure_demo_pack(&self) -> CoreResult<StoryPack> {
        const DEMO_ID: &str = "demo-rain-alley";
        if self.pack_json_path(DEMO_ID)?.exists() {
            return self.get(DEMO_ID);
        }
        self.install_demo_pack()
    }

    pub fn install_demo_pack(&self) -> CoreResult<StoryPack> {
        let pack = build_demo_pack();
        let ch1 = DEMO_CH01_MD;
        let ch2 = DEMO_CH02_MD;
        let saved = self.save(pack)?;
        self.write_chapter_body(&saved.id, "chapters/ch01.md", ch1)?;
        self.write_chapter_body(&saved.id, "chapters/ch02.md", ch2)?;
        Ok(saved)
    }
}

const DEMO_CH01_MD: &str = r#"# 第一章 · 雨巷来客

雨下了一整夜。巷口那盏坏掉的路灯偶尔闪一下，像有人在黑暗里眨眼。

沈棠撑着油纸伞，停在旧茶馆门前。门楣上的铜铃已经锈住，推门时仍发出一声细响。

店里只有一个人。

林晚坐在靠窗的位置，面前是半杯凉透的茶。她抬眼，声音很轻：

「你比约定晚了十二分钟。」

沈棠把伞靠在门边，雨水顺着伞骨滴到青石板上。

「路上有人跟着我。」

林晚的手指在杯沿顿了一下，随即恢复平静：

「那就说明，故事开始了。」
"#;

const DEMO_CH02_MD: &str = r#"# 第二章 · 铜铃之后

铜铃再响时，进来的不是客人。

风把一封没有署名的信吹到门槛内。信封是旧式的牛皮纸，封口用红蜡，印记像一枚破碎的月亮。

林晚没有立刻去捡。她看着沈棠：

「你来选。拆开它，或者我们假装今夜什么都没发生。」

沈棠知道这不是真的选择题。巷子外面的脚步声已经停了。

雨变小了，但没停。
"#;

fn build_demo_pack() -> StoryPack {
    let now = now_rfc3339();
    StoryPack {
        id: "demo-rain-alley".into(),
        title: "雨巷来客".into(),
        source: PackSource {
            source_type: "demo".into(),
            refs: vec![],
        },
        characters: vec![
            PackCharacterRef {
                id: "cc-shentang".into(),
                name: "沈棠".into(),
                role: "主角视角可选".into(),
                gender: "女".into(),
                appearance: "未知".into(),
                opening_scene: "未知".into(),
                opening_lines: "".into(),
                nsfw_profile: String::new(),
                importance: "high".into(),
                content_tier: Some(ContentTier::Standard),
                example_dialogs: vec![
                    "路上有人跟着我。".into(),
                    "你若怕，现在还可以走。".into(),
                ],
                boundaries: vec!["不无现代网络梗".into()],
                personality: "谨慎、观察力强，嘴硬心软".into(),
                speech_style: "短句，偶尔带一点旧小说腔".into(),
                voice_profile: String::new(),
                motivation: String::new(),
                relationships: vec![],
                evidence_refs: vec![],
                mental_models: vec![],
                decision_heuristics: vec![],
                beliefs: vec![],
                expressions: Default::default(),
            voice: None,
            archive: None,
                avatar: None,
                starting_wardrobe: Default::default(),
            },
            PackCharacterRef {
                id: "cc-linwan".into(),
                name: "林晚".into(),
                role: "关键 NPC".into(),
                gender: "女".into(),
                appearance: "未知".into(),
                opening_scene: "未知".into(),
                opening_lines: "".into(),
                nsfw_profile: String::new(),
                importance: "high".into(),
                content_tier: Some(ContentTier::Standard),
                example_dialogs: vec![
                    "你比约定晚了十二分钟。".into(),
                    "那就说明，故事开始了。".into(),
                ],
                boundaries: vec!["不突然变成无理由反派".into()],
                personality: "冷静、话少、知道得比表面多".into(),
                speech_style: "轻声、句子干净".into(),
                voice_profile: String::new(),
                motivation: String::new(),
                relationships: vec![],
                evidence_refs: vec![],
                mental_models: vec![],
                decision_heuristics: vec![],
                beliefs: vec![],
                expressions: Default::default(),
            voice: None,
            archive: None,
                avatar: None,
                starting_wardrobe: Default::default(),
            },
        ],
        world_book_ids: vec![],
        chapters: vec![
            StoryChapter {
                id: "ch01".into(),
                title: "雨巷来客".into(),
                order: 1,
                goals: vec!["抵达茶馆".into(), "与林晚会面".into()],
                node_ids: vec!["n1".into(), "n2".into()],
                body_path: "chapters/ch01.md".into(),
                image_path: String::new(), // U10
            },
            StoryChapter {
                id: "ch02".into(),
                title: "铜铃之后".into(),
                order: 2,
                goals: vec!["面对无名信".into(), "做出第一选择".into()],
                node_ids: vec!["n3".into()],
                body_path: "chapters/ch02.md".into(),
                image_path: String::new(), // U10
            },
        ],
        nodes: vec![
            StoryNode {
                id: "n1".into(),
                chapter_id: "ch01".into(),
                title: "巷口".into(),
                entry: "雨夜，玩家抵达旧茶馆门前".into(),
                exit: vec![NodeExit {
                    id: "e1".into(),
                    when: "enter_teahouse".into(),
                    next: "n2".into(),
                }],
                locked_beats: vec!["不能跳过会面".into()],
                allowed_divergence: "branch".into(),
                present_characters: vec!["cc-shentang".into()],
                location_id: Some("loc-alley".into()),
                summary: "雨巷、坏路灯、旧茶馆门前".into(),
            },
            StoryNode {
                id: "n2".into(),
                chapter_id: "ch01".into(),
                title: "茶馆初见".into(),
                entry: "推门见林晚，对上约定".into(),
                exit: vec![NodeExit {
                    id: "e2".into(),
                    when: "talk_done_or_threat_mentioned".into(),
                    next: "n3".into(),
                }],
                locked_beats: vec!["林晚已知有人跟踪的事实可被揭示".into()],
                allowed_divergence: "branch".into(),
                present_characters: vec!["cc-shentang".into(), "cc-linwan".into()],
                location_id: Some("loc-teahouse".into()),
                summary: "林晚候着，提起迟到与跟踪".into(),
            },
            StoryNode {
                id: "n3".into(),
                chapter_id: "ch02".into(),
                title: "无名信".into(),
                entry: "铜铃再响，红蜡信到门槛".into(),
                exit: vec![
                    NodeExit {
                        id: "e3a".into(),
                        when: "open_letter".into(),
                        // L-5: intentional open ending — final node of the demo pack; the
                        // player stays at the teahouse to keep exploring/dialogue freely
                        // rather than hitting a hard terminal. Both exits loop back to n3.
                        next: "n3".into(),
                    },
                    NodeExit {
                        id: "e3b".into(),
                        when: "ignore_letter".into(),
                        next: "n3".into(),
                    },
                ],
                locked_beats: vec!["信的存在不可抹除".into()],
                allowed_divergence: "branch".into(),
                present_characters: vec!["cc-shentang".into(), "cc-linwan".into()],
                location_id: Some("loc-teahouse".into()),
                summary: "红蜡无名信，门外脚步停住".into(),
            },
        ],
        lore_entries: vec![],
        event_packages: vec![],
        actor_state_config: ActorStatePackConfig::default(),
        default_mode: PlayMode::Mainline,
        max_tier: ContentTier::Standard,
        language: "zh".into(),
        created_at: now.clone(),
        updated_at: now,
        stage_director: StageDirectorConfig::default(),
        worldline: vec![], // T 层测试 fixture：demo pack 无静态时间线
    }
}

// ─── SessionStore ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TavernSessionStore {
    data: DataRoot,
    lock: std::sync::Arc<Mutex<()>>,
}


/// Clip by Unicode scalar count (CJK-safe).
/// P1/C1: entry/开场白占位检测——「本章开始」等占位文本（或过短无场景信息的）不配作为
/// 开场锚点，应回退 node.summary（蒸馏自原著正文，含真实场景），避免 LLM 靠标题脑补
/// （度蜜月 pack 实测：entry="本章开始" 时开幕直接脑补「酒店蜜月套房醒来」）。
/// 判定：命中占位词列表，或 trim 后 <4 字符（无信息量），或含「待更新/占位/TODO/placeholder」。
fn is_placeholder_entry(e: &str) -> bool {
    let t = e.trim();
    if t.chars().count() < 4 {
        return true;
    }
    let lower = t.to_lowercase();
    const PLACEHOLDERS: &[&str] = &[
        "本章开始", "本段开始", "待更新", "占位", "待补充", "待完善", "此处插入",
        "placeholder", "todo", "tbd", "lorem", "xxx", "此处为",
    ];
    PLACEHOLDERS.iter().any(|p| lower.contains(p))
}

pub fn clip_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn chapter_title_of(pack: &StoryPack, chapter_id: &str) -> String {
    pack.chapters
        .iter()
        .find(|c| c.id == chapter_id)
        .map(|c| c.title.clone())
        .unwrap_or_else(|| chapter_id.to_string())
}

fn chapter_order_of(pack: &StoryPack, chapter_id: &str) -> u32 {
    pack.chapters
        .iter()
        .find(|c| c.id == chapter_id)
        .map(|c| c.order)
        .unwrap_or(9999)
}

/// Whole-novel summary from lore + ordered node summaries (no LLM).
pub fn build_pack_novel_summary(pack: &StoryPack) -> String {
    let mut parts: Vec<String> = Vec::new();
    // Prefer lore 简介
    for lore in &pack.lore_entries {
        let title = lore
            .get("title")
            .or_else(|| lore.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let body = lore
            .get("content")
            .or_else(|| lore.get("text"))
            .or_else(|| lore.get("body"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if title.contains("简介") || title.contains("概要") || title.to_ascii_lowercase().contains("blurb") {
            let b = body.trim();
            if !b.is_empty() {
                parts.push(clip_chars(b, 360));
                break;
            }
        }
    }
    if parts.is_empty() {
        for lore in &pack.lore_entries {
            let body = lore
                .get("content")
                .or_else(|| lore.get("text"))
                .or_else(|| lore.get("body"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if body.chars().count() > 40 {
                parts.push(clip_chars(body, 360));
                break;
            }
        }
    }

    let mut nodes = pack.nodes.clone();
    nodes.sort_by_key(|n| chapter_order_of(pack, &n.chapter_id));
    let mut beats: Vec<String> = Vec::new();
    let step = ((nodes.len().max(1) + 5) / 6).max(1);
    for (i, n) in nodes.iter().enumerate() {
        if i == 0 || i + 1 == nodes.len() || i % step == 0 {
            let snip = n.summary.trim();
            if snip.is_empty() {
                continue;
            }
            beats.push(format!("· {}：{}", n.title, clip_chars(snip, 72)));
        }
    }
    if !beats.is_empty() {
        parts.push(format!("关键节点：\n{}", beats.join("\n")));
    }
    if parts.is_empty() {
        parts.push(format!(
            "《{}》共 {} 章、{} 个剧情节点。",
            pack.title,
            pack.chapters.len(),
            pack.nodes.len()
        ));
    }
    parts.join("\n\n")
}

/// Pick important nodes across the novel for side-branch entry points.
pub fn select_side_branch_nodes(pack: &StoryPack, limit: usize) -> Vec<SideBranchNode> {
    let limit = limit.clamp(1, 16);
    let mut nodes = pack.nodes.clone();
    nodes.sort_by_key(|n| chapter_order_of(pack, &n.chapter_id));
    if nodes.is_empty() {
        return Vec::new();
    }
    let n = nodes.len();
    let mut idxs: Vec<usize> = Vec::new();
    // always first + last
    idxs.push(0);
    if n > 1 {
        idxs.push(n - 1);
    }
    // evenly spaced middles
    let want_mid = limit.saturating_sub(idxs.len()).max(0);
    if want_mid > 0 && n > 2 {
        for k in 1..=want_mid {
            let i = (k * (n - 1)) / (want_mid + 1);
            if i > 0 && i + 1 < n {
                idxs.push(i);
            }
        }
    }
    // prefer nodes with richer summary if still short
    if idxs.len() < limit {
        let mut ranked: Vec<(usize, usize)> = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.summary.chars().count(), i))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, i) in ranked {
            if idxs.len() >= limit {
                break;
            }
            if !idxs.contains(&i) {
                idxs.push(i);
            }
        }
    }
    idxs.sort_unstable();
    idxs.dedup();
    idxs.truncate(limit);

    let reasons = [
        "开篇切入",
        "中段转折",
        "关系升温",
        "冲突爆发",
        "情感高潮",
        "收束前夜",
        "结局余韵",
        "关键节点",
    ];
    idxs.into_iter()
        .enumerate()
        .map(|(rank, i)| {
            let node = &nodes[i];
            let reason = if i == 0 {
                "开篇切入".into()
            } else if i + 1 == n {
                "结局余韵".into()
            } else {
                reasons
                    .get((rank + i) % reasons.len())
                    .copied()
                    .unwrap_or("关键节点")
                    .to_string()
            };
            SideBranchNode {
                id: node.id.clone(),
                chapter_id: node.chapter_id.clone(),
                chapter_title: chapter_title_of(pack, &node.chapter_id),
                title: node.title.clone(),
                summary: clip_chars(node.summary.trim(), 220),
                entry: node.entry.clone(),
                present_characters: node.present_characters.clone(),
                reason,
                order: chapter_order_of(pack, &node.chapter_id),
            }
        })
        .collect()
}

pub fn build_side_branch_catalog(pack: &StoryPack, resume_node_id: Option<String>) -> SideBranchCatalog {
    SideBranchCatalog {
        pack_id: pack.id.clone(),
        pack_title: pack.title.clone(),
        novel_summary: build_pack_novel_summary(pack),
        nodes: select_side_branch_nodes(pack, 8),
        resume_node_id,
    }
}

fn present_names(pack: &StoryPack, ids: &[String]) -> String {
    let names: Vec<&str> = ids
        .iter()
        .filter_map(|id| {
            pack.characters
                .iter()
                .find(|c| c.id == *id)
                .map(|c| c.name.as_str())
        })
        .filter(|n| *n != "旁白" && *n != "读者")
        .collect();
    if names.is_empty() {
        "（待定）".into()
    } else {
        names.join("、")
    }
}

/// Mainline / first-open opening monologue + option chips.
pub fn build_mainline_opening(pack: &StoryPack, session: &TavernSession) -> (String, Vec<String>) {
    let node = session
        .node_id
        .as_ref()
        .and_then(|nid| pack.nodes.iter().find(|n| n.id == *nid));
    let ch_title = session
        .chapter_cursor
        .as_ref()
        .map(|cid| chapter_title_of(pack, cid))
        .unwrap_or_else(|| "未知章节".into());
    let node_title = node.map(|n| n.title.as_str()).unwrap_or("起始");
    let entry = node
        .map(|n| {
            let e = n.entry.trim();
            // P1/C1: entry 占位检测——「本章开始」等占位（或 <4 字无信息量）不配作为开场锚点，
            // 回退 node.summary 前 280 字（node.summary 蒸馏自原著该章正文，含真实场景）。
            // 否则 LLM 只能靠标题脑补场景（度蜜月 pack 实测：开幕直接脑补「酒店蜜月套房醒来」）。
            if is_placeholder_entry(e) {
                clip_chars(n.summary.trim(), 280)
            } else if !e.is_empty() {
                e.to_string()
            } else {
                clip_chars(n.summary.trim(), 280)
            }
        })
        .unwrap_or_default();
    let present = present_names(pack, &session.present_character_ids);
    let play = match session.playable {
        Playable::P1 => "P1 旁观",
        Playable::P2 => "P2 多角色",
        Playable::P3 => "P3 进入世界",
        Playable::P4 => "P4 自由演绎",
    };
    let mut entry_line = String::new();
    if let Some(role) = session.entry.entry_role {
        let role_cn = match role {
            EntryRole::Supporting => "配角",
            EntryRole::Protagonist => "主角",
            EntryRole::Isekai => "穿越者",
            EntryRole::Extra => "路人",
        };
        entry_line = format!("进入身份：{}。", role_cn);
    }
    // 演出层 L3a（P14）：蒸馏出的个性化 first_mes——焦点角色的开场场景画布 + 完整开场白。
    // 非空时置于开场正文顶部；空则回退通用模板（不破坏旧 pack）。
    let focus_char = session
        .focus_character_id
        .as_ref()
        .and_then(|fid| pack.characters.iter().find(|c| c.id == *fid))
        .or_else(|| pack.characters.first());
    let opening_scene = focus_char
        .map(|c| c.opening_scene.trim())
        .filter(|s| !s.is_empty());
    let opening_lines = focus_char
        .map(|c| c.opening_lines.trim())
        .filter(|s| !s.is_empty());
    let personalized_opening: Option<String> = match (opening_scene, opening_lines) {
        (Some(scene), Some(lines)) => Some(format!("{scene}\n\n{lines}")),
        (Some(scene), None) => Some(scene.to_string()),
        (None, Some(lines)) => Some(lines.to_string()),
        (None, None) => None,
    };
    let mut body = format!(
        "【开场】\n你踏入《{title}》的世界。\n当前：{ch} · 节点「{nt}」· {play}。\n在场：{present}。\n{entry_line}\n{entry}\n\n夜色与故事同时拉开——你可以选择下一步，或直接输入想做的事。",
        title = pack.title,
        ch = ch_title,
        nt = node_title,
        play = play,
        present = present,
        entry_line = entry_line,
        entry = if entry.is_empty() { "世界在你眼前缓缓展开。".into() } else { entry },
    );
    // 个性化 first_mes 置顶（在通用 brainfuck 之前），保留下方「本段原著场景即你此刻所在」
    // 时空硬锚以继续约束玩家行动范围（个性化开场不替代该锚点）。
    if let Some(personalized) = personalized_opening {
        body = format!("{personalized}\n\n{body}");
    }
    // §13.4② (2026-08-18): 当下状态硬锚——上面的 entry（原著场景）即此刻时空，
    // 玩家行动必须发生在此场景内；禁止跳跃到其他时间地点（机场/酒店/数日后）。
    // 修复实证：度蜜月开幕「环顾四周/打招呼」被 LLM 脑补到三亚机场（选项-场景解耦）。
    // 若玩家明确要求推进时间/地点，由正文末尾 [时间推进: ...] 标注走系统处理。
    body.push_str(
        "\n（本段原著场景即你此刻所在：所有行动（环顾/招呼/探索）都发生在此场景内，\n不得跳跃到其他时间地点；玩家明确要求离开时，在正文末尾用 [时间推进: ...] 标注，\n由系统统一处理时间地点切换。）",
    );
    let options = vec![
        "环顾四周，感受这个世界".into(),
        "与在场的人打招呼".into(),
        "按原著节奏往下走".into(),
    ];
    (body, options)
}

/// Side-branch opening after entering a key node.
pub fn build_side_opening(pack: &StoryPack, session: &TavernSession, node_id: &str) -> (String, Vec<String>) {
    let node = pack.nodes.iter().find(|n| n.id == node_id);
    let title = node.map(|n| n.title.as_str()).unwrap_or(node_id);
    let ch = node
        .map(|n| chapter_title_of(pack, &n.chapter_id))
        .unwrap_or_default();
    let sum = node
        .map(|n| clip_chars(n.summary.trim(), 320))
        .unwrap_or_default();
    let entry = node
        .map(|n| n.entry.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "本章开始".into());
    let present_ids = node
        .map(|n| n.present_characters.clone())
        .unwrap_or_else(|| session.present_character_ids.clone());
    let present = present_names(pack, &present_ids);
    let resume = session
        .resume_node_id
        .as_deref()
        .unwrap_or(session.node_id.as_deref().unwrap_or("?"));
    let novel = clip_chars(&build_pack_novel_summary(pack), 280);
    let body = format!(
        "【支线开场 · {title}】\n整本脉络（节选）：\n{novel}\n\n你从主线锚点 {resume} 岔入支线「{title}」（{ch}）。\n入口：{entry}\n{sum}\n在场：{present}。\n\n这条线可以大胆偏离主线——结束支线后可回到锚点。",
        title = title,
        novel = novel,
        resume = resume,
        ch = ch,
        entry = entry,
        sum = if sum.is_empty() { String::new() } else { format!("节点摘要：{}\n", sum) },
        present = present,
    );
    let options = vec![
        "顺着这条支线深入".into(),
        "先观察在场人物".into(),
        "准备回到主线".into(),
    ];
    (body, options)
}

#[cfg(test)]
mod opening_tests {
    use super::*;

    fn session_with_focus(focus: Option<&str>, present: &[&str]) -> TavernSession {
        TavernSession {
            session_id: "t".into(),
            pack_id: "p".into(),
            pack_missing: false,
            owner: None,
            quality: Quality::Lite,
            playable: Playable::P2,
            play_mode: PlayMode::Mainline,
            content_tier: ContentTier::Standard,
            user_tier_request: ContentTier::Standard,
            entry: EntryConfig::default(),
            chapter_cursor: None,
            node_id: None,
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
            present_character_ids: present.iter().map(|s| s.to_string()).collect(),
            focus_character_id: focus.map(|s| s.to_string()),
            speaker_rotation: false,
            player: PlayerState::default(),
            memory_l1: MemoryL1::default(),
            memory_l2: MemoryL2::default(),
            memory_l3: MemoryL3::default(),
            memory_l4: MemoryL4::default(),
            guard_events: vec![],
            messages: vec![],
            active_run_id: None,
            adult_confirmed: true,
            title: "t".into(),
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
            epoch: 0,
            epoch_last_turn: None,
            epoch_last_chars: None,
            turn_cost_ledger: TurnCostLedger::default(),
            last_turn_diagnostic: None,
            xiami_skim_issues: Vec::new(),
            xiami_skim_sample: String::new(),
            chapter_diaries: Vec::new(),
            turn_progress: None,
            diary_config: None,
            pockets: Default::default(),
            pockets_enabled: true,
            needs: Default::default(),
            growth: Default::default(),
            world_climate: Default::default(),
            chaos: Default::default(),
            milestones: Default::default(),
            objectives: Default::default(),
            ambitions: Default::default(),
            dream: Default::default(),
            episodes: Default::default(),
            journal: Default::default(),
            relationships: Default::default(),
            pending_swipes: Default::default(),
            promises: Default::default(),
            preferences: Default::default(),
            presence: Default::default(),
            timed_world_info: Default::default(),
            event_extract: true,
            world: WorldState::default(),
            game_clock: Default::default(),
        }
    }

    fn char_with_opening(id: &str, name: &str) -> PackCharacterRef {
        PackCharacterRef {
            id: id.into(),
            name: name.into(),
            role: String::new(),
            gender: String::new(),
            appearance: String::new(),
            opening_scene: format!("雨夜的小巷，灯影斑驳，{name} 靠在墙边。"),
            opening_lines: format!("「{name}在这里等你很久了。」她把玩着伞柄，目光在你脸上流连。"),
            nsfw_profile: "接吻以上→nsfw".into(),
            importance: "high".into(),
            content_tier: None,
            example_dialogs: vec![],
            boundaries: vec![],
            personality: String::new(),
            speech_style: String::new(),
            voice_profile: String::new(),
            motivation: String::new(),
            relationships: vec![],
            evidence_refs: vec![],
            mental_models: vec![],
            decision_heuristics: vec![],
            beliefs: vec![],
            expressions: std::collections::HashMap::new(),
            voice: None,
            archive: None,
            avatar: None,
                starting_wardrobe: Default::default(),
        }
    }

    fn pack_with_title(title: &str) -> StoryPack {
        StoryPack {
            id: "p".into(),
            title: title.into(),
            source: PackSource::default(),
            characters: vec![],
            world_book_ids: vec![],
            chapters: vec![],
            nodes: vec![],
            lore_entries: vec![],
            default_mode: PlayMode::Mainline,
            max_tier: ContentTier::Standard,
            language: "zh".into(),
            created_at: String::new(),
            updated_at: String::new(),
            stage_director: StageDirectorConfig::default(),
            event_packages: vec![],
            actor_state_config: ActorStatePackConfig::default(),
            worldline: vec![],
        }
    }

    /// 无开场字段的角色卡 → 回退通用模板，不破坏既有行为。
    #[test]
    fn mainline_opening_falls_back_when_no_scene() {
        let mut pack = pack_with_title("测试世界");
        // c1 无开场字段；c2 在场但在场名字列在 c1 之后且无开场
        pack.characters = vec![PackCharacterRef {
            id: "c1".into(),
            name: "路人".into(),
            opening_scene: String::new(),
            opening_lines: String::new(),
            ..char_with_opening("c1", "路人")
        }];
        let s = session_with_focus(Some("c1"), &["c1"]);
        let (body, _opts) = build_mainline_opening(&pack, &s);
        assert!(body.contains("你踏入《测试世界》的世界"), "应回退通用模板, got: {body}");
    }

    /// P14 演出层 L3a：焦点角色带 opening_scene/opening_lines 时，个性化 first_mes 置顶进入开场。
    #[test]
    fn mainline_opening_consumes_personalized_opening() {
        let mut pack = pack_with_title("测试世界");
        pack.characters = vec![char_with_opening("c1", "林九")];
        let s = session_with_focus(Some("c1"), &["c1"]);
        let (body, _opts) = build_mainline_opening(&pack, &s);
        assert!(body.contains("雨夜的小巷"), "开场应含个性化场景画布: {body}");
        assert!(body.contains("在这里等你很久了"), "开场应含个性化 first_mes: {body}");
        assert!(body.contains("你踏入《测试世界》的世界"), "通用模板仍应保留作为补充: {body}");
    }

    /// P14 演出层 L3a：仅 opening_scene（无 opening_lines）也进入开场，不强制配对。
    #[test]
    fn mainline_opening_scene_only_consumed() {
        let mut pack = pack_with_title("测试世界");
        let mut c = char_with_opening("c1", "林九");
        c.opening_lines = String::new();
        pack.characters = vec![c];
        let s = session_with_focus(Some("c1"), &["c1"]);
        let (body, _opts) = build_mainline_opening(&pack, &s);
        assert!(body.contains("雨夜的小巷"), "仅场景画布也应进入开场: {body}");
        assert!(!body.contains("在这里等你很久了"), "空 opening_lines 不应出现: {body}");
    }

    /// P1/C1: entry 占位（"本章开始"）→ 回退 node.summary 前 280 字（含真实场景），
    /// 不让 LLM 靠标题脑补（度蜜月开幕跳酒店根因链第③环）。
    #[test]
    fn mainline_opening_placeholder_entry_falls_back_to_summary() {
        let mut pack = pack_with_title("测试世界");
        pack.nodes = vec![StoryNode {
            id: "n1".into(),
            chapter_id: "ch01".into(),
            title: "第一章 相遇".into(),
            entry: "本章开始".into(),
            exit: vec![],
            locked_beats: vec![],
            allowed_divergence: "branch".into(),
            present_characters: vec![],
            location_id: None,
            summary: "午后的阳光穿过教学楼旁那排老樟树的枝叶。".into(),
        }];
        let mut s = session_with_focus(Some("c1"), &["c1"]);
        s.node_id = Some("n1".into());
        s.chapter_cursor = Some("ch01".into());
        let (body, _opts) = build_mainline_opening(&pack, &s);
        assert!(!body.contains("本章开始"), "占位 entry 不应进开场: {body}");
        assert!(body.contains("老樟树"), "应回退 summary 真实场景: {body}");
    }

    /// P1/C1: 有实质内容的 entry（非占位）保持原样不回归。
    #[test]
    fn mainline_opening_real_entry_kept() {
        let mut pack = pack_with_title("测试世界");
        pack.nodes = vec![StoryNode {
            id: "n1".into(),
            chapter_id: "ch01".into(),
            title: "第一章 相遇".into(),
            entry: "午后，学校后门，你收到父亲调课的消息。".into(),
            exit: vec![],
            locked_beats: vec![],
            allowed_divergence: "branch".into(),
            present_characters: vec![],
            location_id: None,
            summary: "无关摘要".into(),
        }];
        let mut s = session_with_focus(Some("c1"), &["c1"]);
        s.node_id = Some("n1".into());
        s.chapter_cursor = Some("ch01".into());
        let (body, _opts) = build_mainline_opening(&pack, &s);
        assert!(body.contains("调课的消息"), "实质 entry 应保留: {body}");
        assert!(!body.contains("无关摘要"), "不应误回退 summary: {body}");
    }
}

/// Seed opening monologue if session has no messages yet. Returns true if seeded.
pub fn seed_opening_if_needed(session: &mut TavernSession, pack: &StoryPack) -> bool {
    if session.opening_seeded {
        return false;
    }
    if !session.messages.is_empty() {
        // Already has history (imported / repaired) — mark seeded so we don't inject mid-chat.
        session.opening_seeded = true;
        return false;
    }
    let (content, options) = if session.play_mode == PlayMode::Side {
        if let Some(sid) = session.side_branch_node_id.clone() {
            build_side_opening(pack, session, &sid)
        } else if let Some(nid) = session.node_id.clone() {
            build_side_opening(pack, session, &nid)
        } else {
            build_mainline_opening(pack, session)
        }
    } else {
        build_mainline_opening(pack, session)
    };
    session.messages.push(TavernMessage {
        id: format!("msg-opening-{}", Uuid::new_v4()),
        role: "assistant".into(),
        content,
        created_at: Utc::now().to_rfc3339(),
        options,
        engine_tag: None,
        program: None,
        reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
    });
    session.opening_seeded = true;
    // Opening is not a player turn; keep turn at 0.
    true
}

/// Enter a side branch at node_id: stash resume, switch mode, set cursor, seed side opening.
pub fn enter_side_branch(
    session: &mut TavernSession,
    pack: &StoryPack,
    node_id: &str,
) -> CoreResult<()> {
    let node = pack
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .ok_or_else(|| CoreError::NotFound(format!("node not found: {node_id}")))?;
    if session.play_mode != PlayMode::Side {
        if session.resume_node_id.is_none() {
            session.resume_node_id = session.node_id.clone();
        }
        session.play_mode = PlayMode::Side;
    } else if session.resume_node_id.is_none() {
        session.resume_node_id = session.node_id.clone();
    }
    session.side_branch_node_id = Some(node.id.clone());
    session.side_branch_label = Some(node.title.clone());
    session.node_id = Some(node.id.clone());
    session.chapter_cursor = Some(node.chapter_id.clone());
    if !node.present_characters.is_empty() {
        session.present_character_ids = node.present_characters.clone();
        session.focus_character_id = node.present_characters.first().cloned();
    }
    let (content, options) = build_side_opening(pack, session, &node.id);
    session.messages.push(TavernMessage {
        id: format!("msg-side-opening-{}", Uuid::new_v4()),
        role: "assistant".into(),
        content,
        created_at: Utc::now().to_rfc3339(),
        options,
        engine_tag: None,
        program: None,
        reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
    });
    session.opening_seeded = true;
    Ok(())
}

impl TavernSessionStore {
    pub fn new(data: DataRoot) -> Self {
        let _ = data.ensure_layout();
        Self {
            data,
            lock: std::sync::Arc::new(Mutex::new(())),
        }
    }

    fn dir(&self) -> PathBuf {
        self.data.tavern_sessions_dir()
    }

    fn path_for(&self, session_id: &str) -> CoreResult<PathBuf> {
        let id = safe_id(session_id)?;
        if !id.starts_with("tavern-session-") {
            return Err(CoreError::BadRequest(
                "session id must start with tavern-session-".into(),
            ));
        }
        Ok(self.dir().join(format!("{id}.json")))
    }

    pub fn list(&self) -> CoreResult<Vec<Value>> {
        let _g = self.lock.lock();
        fs::create_dir_all(self.dir())?;
        let mut out = Vec::new();
        for ent in fs::read_dir(self.dir())? {
            let ent = ent?;
            let name = ent.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }
            match fs::read_to_string(ent.path()) {
                Ok(raw) => {
                    if let Ok(s) = serde_json::from_str::<TavernSession>(&raw) {
                        out.push(json!({
                            "sessionId": s.session_id,
                            "packId": s.pack_id,
                            "packMissing": s.pack_missing,
                            "playable": s.playable,
                            "playMode": s.play_mode,
                            "contentTier": s.content_tier,
                            "title": s.title,
                            "turn": s.turn,
                            "updatedAt": s.updated_at,
                            "createdAt": s.created_at,
                        }));
                    }
                }
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| {
            let au = a.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
            let bu = b.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
            bu.cmp(au)
        });
        Ok(out)
    }

    /// F1: list only sessions owned by `user_id`. Legacy sessions with
    /// `owner == None` are excluded (not attributed to any current user).
    pub fn list_owned(&self, user_id: &str) -> CoreResult<Vec<Value>> {
        let _g = self.lock.lock();
        fs::create_dir_all(self.dir())?;
        let mut out = Vec::new();
        for ent in fs::read_dir(self.dir())? {
            let ent = ent?;
            let name = ent.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }
            match fs::read_to_string(ent.path()) {
                Ok(raw) => {
                    if let Ok(s) = serde_json::from_str::<TavernSession>(&raw) {
                        if s.owner.as_deref() != Some(user_id) {
                            continue;
                        }
                        out.push(json!({
                            "sessionId": s.session_id,
                            "packId": s.pack_id,
                            "packMissing": s.pack_missing,
                            "playable": s.playable,
                            "playMode": s.play_mode,
                            "contentTier": s.content_tier,
                            "title": s.title,
                            "turn": s.turn,
                            "updatedAt": s.updated_at,
                            "createdAt": s.created_at,
                        }));
                    }
                }
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| {
            let au = a.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
            let bu = b.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("");
            bu.cmp(au)
        });
        Ok(out)
    }

    /// F1: check that `user_id` owns the session. Returns the session if
    /// authorized, or `Forbidden` / `NotFound` otherwise. Legacy sessions
    /// (`owner == None`) are denied to all users except the one who saves
    /// (which stamps the owner). This is a conservative default: old data
    /// with no owner is not silently exposed to arbitrary users.
    pub fn get_for_owner(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> CoreResult<TavernSession> {
        let sess = self.get(session_id)?;
        match &sess.owner {
            Some(uid) if uid == user_id => Ok(sess),
            _ => Err(CoreError::Forbidden(format!(
                "session not owned by user: {session_id}"
            ))),
        }
    }

    pub fn get(&self, session_id: &str) -> CoreResult<TavernSession> {
        let _g = self.lock.lock();
        let path = self.path_for(session_id)?;
        if !path.exists() {
            return Err(CoreError::NotFound(format!(
                "session not found: {session_id}"
            )));
        }
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, session: TavernSession) -> CoreResult<TavernSession> {
        self.save_inner(session, None)
    }

    /// F2: Atomic turn acquisition — check active_run_id is empty AND set it
    /// to `run_id` in a single locked transaction. If the session already has
    /// an active_run_id, returns `Conflict`. This prevents the race where two
    /// concurrent requests both see `active_run_id == None` and proceed to
    /// spawn duplicate LLM workers.
    ///
    /// Returns the updated session on success.
    pub fn acquire_turn(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> CoreResult<TavernSession> {
        let _g = self.lock.lock();
        let path = self.path_for(session_id)?;
        if !path.exists() {
            return Err(CoreError::NotFound(format!(
                "session not found: {session_id}"
            )));
        }
        let raw = fs::read_to_string(&path)?;
        let mut sess: TavernSession = serde_json::from_str(&raw)?;

        // Reject if already has an active run.
        if sess.active_run_id.is_some() {
            return Err(CoreError::Conflict(format!(
                "turn already in progress: {:?}",
                sess.active_run_id
            )));
        }

        sess.active_run_id = Some(run_id.to_string());
        sess.updated_at = now_rfc3339();
        write_atomic(&path, &serde_json::to_string(&sess)?)?;
        Ok(sess)
    }

    /// F2: Atomic turn release — clear active_run_id if it matches `run_id`.
    /// Uses a single locked transaction to prevent races with acquire_turn.
    /// If `run_id` is None, no-op. If active_run_id doesn't match, no-op.
    pub fn release_turn(
        &self,
        session_id: &str,
        run_id: Option<&str>,
    ) {
        let run_id = match run_id {
            Some(r) => r,
            None => return,
        };
        let _g = self.lock.lock();
        let path = match self.path_for(session_id) {
            Ok(p) => p,
            Err(_) => return,
        };
        if !path.exists() {
            return;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut sess) = serde_json::from_str::<TavernSession>(&raw) else {
            return;
        };
        let should = match &sess.active_run_id {
            Some(cur) => cur == run_id || cur.starts_with("pending-"),
            None => false,
        };
        if should {
            sess.active_run_id = None;
            sess.updated_at = now_rfc3339();
            let _ = write_atomic(&path, &serde_json::to_string(&sess).unwrap_or_default());
        }
    }

    // ─── T2 世界认知（U2 实体图谱 / U7 真相账本）─────────────────────────────

    fn entity_kind_str(kind: &WsEntityKind) -> String {
        match kind {
            WsEntityKind::Character => "character".into(),
            WsEntityKind::Location => "location".into(),
            WsEntityKind::Item => "item".into(),
            WsEntityKind::Faction => "faction".into(),
            WsEntityKind::Concept => "concept".into(),
            WsEntityKind::Custom(s) => s.to_ascii_lowercase(),
        }
    }

    /// 从 pack 播种初始实体（角色 → Character 实体）。会话级 world 的种子来源。
    fn seed_world_from_pack(pack: &StoryPack) -> WorldState {
        let mut ws = WorldState::new();
        for ia in &pack.actor_state_config.initial_actors {
            let name = pack
                .characters
                .iter()
                .find(|c| c.id == ia.character_id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| ia.character_id.clone());
            let description = pack
                .actor_state_config
                .templates
                .get(&ia.template_id)
                .map(|t| format!("actor 模板 {}（{} 个字段）", ia.template_id, t.fields.len()))
                .unwrap_or_else(|| "（无模板描述）".to_string());
            let entity = WorldEntity {
                id: ia.character_id.clone(),
                kind: WsEntityKind::Character,
                name,
                description,
                properties: Default::default(),
                relationships: Vec::new(),
                state_flags: Default::default(),
                counters: Default::default(),
            };
            ws.entities.insert(entity.id.clone(), entity);
        }
        ws
    }

    /// U2: 实体列表查询（kind 筛选 + q 名字/ID 模糊）。
    pub fn world_entities(
        &self,
        session_id: &str,
        kind: Option<String>,
        q: Option<String>,
    ) -> CoreResult<Value> {
        let sess = self.get(session_id)?;
        let q = q.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let mut items: Vec<Value> = Vec::new();
        for e in sess.world.entities.values() {
            let kname = Self::entity_kind_str(&e.kind);
            if let Some(k) = &kind {
                if !k.eq_ignore_ascii_case(&kname) {
                    continue;
                }
            }
            if let Some(qq) = &q {
                let name = e.name.to_lowercase();
                let id = e.id.to_lowercase();
                let ql = qq.to_lowercase();
                if !name.contains(&ql) && !id.contains(&ql) {
                    continue;
                }
            }
            items.push(json!({
                "id": e.id,
                "kind": kname,
                "name": e.name,
                "description": e.description,
                "stateFlags": e.state_flags,
                "counters": e.counters,
                "properties": e.properties,
                "relationCount": e.relationships.len(),
            }));
        }
        items.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        });
        Ok(json!({ "entities": items, "count": items.len() }))
    }

    /// U2: 实体详情 + 关系树（出边/入边展开目标实体名与类型）。
    pub fn world_entity_detail(&self, session_id: &str, entity_id: &str) -> CoreResult<Value> {
        let sess = self.get(session_id)?;
        let entity = sess
            .world
            .get_entity(entity_id)
            .ok_or_else(|| CoreError::NotFound(format!("entity not found: {entity_id}")))?;
        let kname = Self::entity_kind_str(&entity.kind);
        let out_edges: Vec<Value> = entity
            .relationships
            .iter()
            .map(|r| {
                let target = sess.world.get_entity(&r.target_id);
                json!({
                    "targetId": r.target_id,
                    "targetName": target.map(|t| t.name.clone()).unwrap_or_default(),
                    "targetKind": target.map(|t| Self::entity_kind_str(&t.kind)).unwrap_or_default(),
                    "relationType": r.relation_type,
                    "strength": r.strength,
                    "direction": "out",
                })
            })
            .collect();
        let mut in_edges: Vec<Value> = Vec::new();
        for other in sess.world.entities.values() {
            for r in &other.relationships {
                if r.target_id == entity_id {
                    in_edges.push(json!({
                        "sourceId": other.id,
                        "sourceName": other.name,
                        "sourceKind": Self::entity_kind_str(&other.kind),
                        "relationType": r.relation_type,
                        "strength": r.strength,
                        "direction": "in",
                    }));
                }
            }
        }
        Ok(json!({
            "entity": {
                "id": entity.id,
                "kind": kname,
                "name": entity.name,
                "description": entity.description,
                "properties": entity.properties,
                "stateFlags": entity.state_flags,
                "counters": entity.counters,
            },
            "relationships": { "out": out_edges, "in": in_edges },
            "relationshipCount": out_edges.len() + in_edges.len(),
        }))
    }

    /// U2: 应用世界事件（EntityCreated/Updated、flag/counter/relationship/meta），落盘并返回快照。
    pub fn world_apply_events(&self, session_id: &str, events: Vec<WorldEvent>) -> CoreResult<Value> {
        let mut sess = self.get(session_id)?;
        let before = sess.world.event_log.len();
        for ev in &events {
            sess.world.apply(ev.clone());
        }
        let saved = self.save(sess)?;
        Ok(json!({
            "applied": events.len(),
            "eventLogLen": saved.world.event_log.len(),
            "delta": saved.world.event_log.len() - before,
            "snapshot": saved.world.snapshot(),
        }))
    }

    fn truth_keys(ev: &WorldEvent) -> Vec<(String, String)> {
        match ev {
            WorldEvent::EntityCreated(e) => {
                let mut v = vec![(e.id.clone(), "exists".to_string())];
                for k in e.properties.keys() {
                    v.push((e.id.clone(), format!("prop.{k}")));
                }
                for f in e.state_flags.iter() {
                    v.push((e.id.clone(), format!("flag.{f}")));
                }
                for k in e.counters.keys() {
                    v.push((e.id.clone(), format!("counter.{k}")));
                }
                v
            }
            WorldEvent::EntityUpdated { id, changes } => changes
                .keys()
                .map(|k| (id.clone(), format!("prop.{k}")))
                .collect(),
            WorldEvent::EntityRemoved(id) => vec![(id.clone(), "exists".to_string())],
            WorldEvent::FlagSet { entity_id, flag } => {
                Self::truth_scope_keys(entity_id, &format!("flag.{flag}"))
            }
            WorldEvent::FlagCleared { entity_id, flag } => {
                Self::truth_scope_keys(entity_id, &format!("flag.{flag}"))
            }
            WorldEvent::CounterChanged { entity_id, counter, .. } => {
                Self::truth_scope_keys(entity_id, &format!("counter.{counter}"))
            }
            WorldEvent::CounterSet { entity_id, counter, .. } => {
                Self::truth_scope_keys(entity_id, &format!("counter.{counter}"))
            }
            WorldEvent::RelationshipSet {
                source_id,
                target_id,
                relation_type,
                ..
            } => vec![(
                source_id.clone(),
                format!("rel.{relation_type}.{target_id}"),
            )],
            WorldEvent::RelationshipRemoved {
                source_id,
                target_id,
                relation_type,
            } => vec![(
                source_id.clone(),
                format!("rel.{relation_type}.{target_id}"),
            )],
            WorldEvent::MetaSet { key, .. } => vec![("global".to_string(), format!("meta.{key}"))],
            WorldEvent::NarrativeEvent { .. } => Vec::new(),
        }
    }

    fn truth_scope_keys(entity_id: &Option<String>, key: &str) -> Vec<(String, String)> {
        match entity_id {
            Some(id) => vec![(id.clone(), key.to_string())],
            None => vec![("global".to_string(), key.to_string())],
        }
    }

    fn truth_value(sess: &TavernSession, scope: &str, key: &str) -> Value {
        if scope == "global" {
            if let Some(mk) = key.strip_prefix("meta.") {
                return sess.world.meta.get(mk).cloned().unwrap_or(Value::Null);
            }
            if let Some(fk) = key.strip_prefix("flag.") {
                return json!(sess.world.global_flags.contains(fk));
            }
            if let Some(ck) = key.strip_prefix("counter.") {
                return json!(sess.world.global_counters.get(ck).copied().unwrap_or(0));
            }
            return Value::Null;
        }
        let entity = match sess.world.entities.get(scope) {
            Some(e) => e,
            None => return Value::Null,
        };
        if key == "exists" {
            return json!(true);
        }
        if let Some(fk) = key.strip_prefix("flag.") {
            return json!(entity.state_flags.contains(fk));
        }
        if let Some(ck) = key.strip_prefix("counter.") {
            return json!(entity.counters.get(ck).copied().unwrap_or(0));
        }
        if let Some(pk) = key.strip_prefix("prop.") {
            return entity.properties.get(pk).cloned().unwrap_or(Value::Null);
        }
        if let Some(rk) = key.strip_prefix("rel.") {
            if let Some((rtype, tid)) = rk.split_once('.') {
                if let Some(r) = entity
                    .relationships
                    .iter()
                    .find(|r| r.relation_type == rtype && r.target_id == tid)
                {
                    return json!(r.strength);
                }
            }
            return json!(0.0);
        }
        Value::Null
    }

    fn truth_scope_kind(sess: &TavernSession, scope: &str) -> String {
        if scope == "global" {
            return "global".to_string();
        }
        sess.world
            .entities
            .get(scope)
            .map(|e| Self::entity_kind_str(&e.kind))
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// U7: 从 event_log 派生的真相账本（每 key 当前值 + 变更版本/最后事件）。
    pub fn world_truth(&self, session_id: &str) -> CoreResult<Value> {
        let sess = self.get(session_id)?;
        let mut versions: std::collections::HashMap<(String, String), usize> =
            std::collections::HashMap::new();
        let mut last: std::collections::HashMap<(String, String), usize> =
            std::collections::HashMap::new();
        for (idx, ev) in sess.world.event_log.iter().enumerate() {
            for (scope, key) in Self::truth_keys(ev) {
                *versions.entry((scope.clone(), key.clone())).or_insert(0) += 1;
                last.insert((scope, key), idx);
            }
        }
        let mut entries: Vec<Value> = Vec::new();
        for ((scope, key), version) in &versions {
            let last_event = last
                .get(&(scope.clone(), key.clone()))
                .copied()
                .unwrap_or(0);
            let scope_name = if scope == "global" {
                "世界".to_string()
            } else {
                sess.world
                    .entities
                    .get(scope)
                    .map(|e| e.name.clone())
                    .unwrap_or_else(|| scope.clone())
            };
            entries.push(json!({
                "scope": scope,
                "scopeName": scope_name,
                "scopeKind": Self::truth_scope_kind(&sess, scope),
                "key": key,
                "value": Self::truth_value(&sess, scope, key),
                "status": "current",
                "version": version,
                "lastEvent": last_event,
            }));
        }
        entries.sort_by(|a, b| {
            let ak = (a["scope"].as_str().unwrap_or(""), a["key"].as_str().unwrap_or(""));
            let bk = (b["scope"].as_str().unwrap_or(""), b["key"].as_str().unwrap_or(""));
            ak.cmp(&bk)
        });
        Ok(json!({
            "entries": entries,
            "count": entries.len(),
            "eventLogLen": sess.world.event_log.len(),
        }))
    }

    /// U7: 一致性断言——期望值与账本当前值比较（供编排/质检做世界真相校验）。
    pub fn world_truth_check(
        &self,
        session_id: &str,
        entity_id: String,
        key: String,
        expected: Value,
    ) -> CoreResult<Value> {
        let tv = self.world_truth(session_id)?;
        let entries = tv["entries"].as_array().cloned().unwrap_or_default();
        let scope = if entity_id.is_empty() { "global".to_string() } else { entity_id.clone() };
        let hit = entries.iter().find(|e| {
            e["scope"].as_str().unwrap_or("") == scope && e["key"].as_str().unwrap_or("") == key
        });
        let current = hit.map(|e| e["value"].clone()).unwrap_or(Value::Null);
        let ok = current == expected;
        Ok(json!({
            "entityId": entity_id,
            "key": key,
            "expected": expected,
            "current": current,
            "ok": ok,
            "status": if hit.is_none() { "missing" } else if ok { "consistent" } else { "conflict" },
        }))
    }

    /// CAS write: only persists if the on-disk `updated_at` still equals `base_revision`.
    pub fn save_with_revision(
        &self,
        session: TavernSession,
        base_revision: &str,
    ) -> CoreResult<TavernSession> {
        self.save_inner(session, Some(base_revision))
    }

    /// M-2 (CAS): Atomic read-modify-write inside a single locked transaction.
    ///
    /// Loads the session by `session_id`, hands a mutable reference to the
    /// closure `f`, then persists the result. Because the load, mutation, and
    /// write all happen while holding the store mutex, no concurrent save can
    /// interleave — the read-modify-write is race-free without requiring the
    /// caller to thread a base_revision string around.
    ///
    /// The closure may return `Err` to abort the write (e.g. a validation
    /// failure or "turn in progress" guard); in that case the on-disk session
    /// is left untouched.
    ///
    /// Returns the persisted session (post-closure mutation) on success.
    pub fn update_session<F>(
        &self,
        session_id: &str,
        f: F,
    ) -> CoreResult<TavernSession>
    where
        F: FnOnce(&mut TavernSession) -> CoreResult<()>,
    {
        let _g = self.lock.lock();
        let path = self.path_for(session_id)?;
        if !path.exists() {
            return Err(CoreError::NotFound(format!(
                "session not found: {session_id}"
            )));
        }
        let raw = fs::read_to_string(&path)?;
        let mut sess: TavernSession = serde_json::from_str(&raw)?;
        f(&mut sess)?;
        let now = now_rfc3339();
        if sess.created_at.is_empty() {
            sess.created_at = now.clone();
        }
        sess.updated_at = now;
        write_atomic(&path, &serde_json::to_string(&sess)?)?;
        Ok(sess)
    }

    fn save_inner(
        &self,
        mut session: TavernSession,
        base_revision: Option<&str>,
    ) -> CoreResult<TavernSession> {
        let _g = self.lock.lock();
        let is_new = session.session_id.trim().is_empty();
        if is_new {
            session.session_id = format!("tavern-session-{}", Uuid::new_v4());
        }
        let _ = safe_id(&session.session_id)?;
        let now = now_rfc3339();
        if session.created_at.is_empty() {
            session.created_at = now.clone();
        }
        session.updated_at = now;

        // Revision CAS: existing sessions must be saved on top of the revision the caller read.
        if !is_new {
            if let Some(base) = base_revision {
                let path = self.path_for(&session.session_id)?;
                if !path.exists() {
                    return Err(CoreError::Conflict(
                        "session 已被其他操作删除，请重新加载".into(),
                    ));
                }
                let raw = fs::read_to_string(&path)?;
                let current: TavernSession = serde_json::from_str(&raw)?;
                if current.updated_at != base {
                    return Err(CoreError::Conflict(format!(
                        "session 已被其他操作更新，请重新加载后再保存 (期望 revision {base}，当前 {})",
                        current.updated_at
                    )));
                }
            }
        }

        fs::create_dir_all(self.dir())?;
        let path = self.path_for(&session.session_id)?;
        // P0-1(审计): 紧凑序列化替代 to_string_pretty——MB 级会话文件省 30-50% 字节与 CPU
        // （长会话含 30 checkpoint + messages + embedding，写放大 O(n²) 主因）。
        write_atomic(&path, &serde_json::to_string(&session)?)?;
        Ok(session)
    }

    pub fn delete(&self, session_id: &str) -> CoreResult<()> {
        let _g = self.lock.lock();
        let path = self.path_for(session_id)?;
        if !path.exists() {
            return Err(CoreError::NotFound(format!(
                "session not found: {session_id}"
            )));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn create_from_pack(
        &self,
        packs: &PackStore,
        req: CreateSessionRequest,
    ) -> CoreResult<TavernSession> {
        let pack = match packs.get(&req.pack_id) {
            Ok(p) => p,
            Err(CoreError::NotFound(_)) => {
                return Err(CoreError::NotFound(format!(
                    "pack not found: {}",
                    req.pack_id
                )));
            }
            Err(e) => return Err(e),
        };

        // Card max: take min across pack characters' tiers, then pack.max_tier
        let mut card_max = pack.max_tier;
        for c in &pack.characters {
            if let Some(t) = c.content_tier {
                if t.rank() < card_max.rank() {
                    card_max = t;
                }
            }
        }
        let global = req.global_tier.unwrap_or(ContentTier::Open);
        let final_tier = ContentTier::min3(req.user_tier, card_max, global);

        let entry = req.entry.unwrap_or_default();
        // P3 should have entry role; still allow create (wizard half-done) but keep fields
        let control = entry
            .vessel_character_id
            .clone()
            .or_else(|| pack.characters.first().map(|c| c.id.clone()));

        let present: Vec<String> = pack
            .first_node_id()
            .and_then(|nid| pack.nodes.iter().find(|n| n.id == nid).cloned())
            .map(|n| n.present_characters)
            .unwrap_or_else(|| pack.characters.iter().map(|c| c.id.clone()).collect());

        let title = req.title.unwrap_or_else(|| {
            format!(
                "{} · {}",
                pack.title,
                match req.playable {
                    Playable::P1 => "P1",
                    Playable::P2 => "P2",
                    Playable::P3 => "P3",
                    Playable::P4 => "P4",
                }
            )
        });

        let author_project_id = req
            .author_project_id
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        // U13: auto-mount the target work's saved creation compass (if any) so
        // brand-new sessions inherit it without a re-PUT after compass save.
        let compass_auto = req.work_id.as_ref().and_then(|wid| {
            let s = CompassStore::new(self.data.clone());
            match s.load(wid) {
                Ok(c) if !c.is_empty() => Some(c),
                _ => None,
            }
        });
        let mut session = TavernSession {
            session_id: format!("tavern-session-{}", Uuid::new_v4()),
            pack_id: pack.id.clone(),
            pack_missing: false,
            owner: req.owner.clone(),
            quality: req.quality,
            playable: req.playable,
            play_mode: req.play_mode,
            content_tier: final_tier,
            user_tier_request: req.user_tier,
            entry,
            chapter_cursor: pack.first_chapter_id(),
            node_id: pack.first_node_id(),
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
            present_character_ids: present.clone(),
            focus_character_id: present.first().cloned().or_else(|| pack.characters.first().map(|c| c.id.clone())),
            speaker_rotation: matches!(req.playable, Playable::P2 | Playable::P3 | Playable::P4),
            player: PlayerState {
                display_name: req
                    .player_display_name
                    .unwrap_or_else(|| "旅人".into()),
                control_character_id: control,
                persona: String::new(),
                inventory: vec![],
                flags: json!({}),
            },
            memory_l1: MemoryL1::default(),
            memory_l2: MemoryL2::default(),
            memory_l3: MemoryL3::default(),
            memory_l4: MemoryL4::default(),
            guard_events: vec![],
            messages: vec![],
            active_run_id: None,
            adult_confirmed: req.adult_confirmed,
            title,
            created_at: String::new(),
            updated_at: String::new(),
            author_project_id: author_project_id.clone(),
            author_live_path: None,
            author_live_enabled: true,
            author_live_every_n: 1,
            author_live_write_turns: false,
            actor_states: {
                let mut sys = pack.actor_state_config.to_system();
                if let Some(c) = compass_auto {
                    sys.mount_compass(c);
                }
                sys
            },
            world: Self::seed_world_from_pack(&pack),
            // [1B 2026-08-18] 初始时间天气从 pack 开场信号推导（角色 openingScene + 首节点摘要），
            // 避免默认「清晨/晴/春」与原著设定冲突（宿醉「夏末雨夜」被压死事故，见
            // docs/宿醉时间天气原著冲突-20260817.md）。
            game_clock: {
                let mut clock_signals = String::new();
                for c in &pack.characters {
                    if !c.opening_scene.is_empty() {
                        clock_signals.push_str(&c.opening_scene);
                        clock_signals.push(' ');
                    }
                }
                if let Some(nid) = pack.first_node_id() {
                    if let Some(n) = pack.nodes.iter().find(|n| n.id == nid) {
                        if !n.summary.is_empty() {
                            clock_signals.push_str(&n.summary);
                        }
                    }
                }
                crate::time_clock::GameClock::derive_from_text(&clock_signals)
            },
            director_plan: None,
            director_pending: false,
            director_task: None,
            last_event: None,
            last_check_results: vec![],
        check_history: vec![],
            checkpoints: vec![],
            // U11: 新会话默认 epoch=0、空账本（serde default 兼容旧会话文件）。
            epoch: 0,
            epoch_last_turn: None,
            epoch_last_chars: None,
            turn_cost_ledger: TurnCostLedger::default(),
            last_turn_diagnostic: None,
            xiami_skim_issues: Vec::new(),
            xiami_skim_sample: String::new(),
            chapter_diaries: Vec::new(),
            turn_progress: None,
            diary_config: None,
            pockets: {
                let mut m = std::collections::HashMap::new();
                for c in &pack.characters {
                    if !c.starting_wardrobe.is_empty() {
                        m.insert(c.id.clone(), c.starting_wardrobe.clone());
                    }
                }
                m
            },
            pockets_enabled: true,
            needs: Default::default(),
            growth: Default::default(),
            world_climate: Default::default(),
            chaos: Default::default(),
            milestones: Default::default(),
            objectives: Default::default(),
            ambitions: Default::default(),
            dream: Default::default(),
            episodes: Default::default(),
            journal: Default::default(),
            relationships: Default::default(),
            pending_swipes: Default::default(),
            promises: Default::default(),
            preferences: Default::default(),
            presence: Default::default(),
            timed_world_info: Default::default(),
            event_extract: true,
        };
        if let Some(pid) = author_project_id {
            session.author_live_path = Some(format!(
                "projects/{pid}/sessions/{}/live.md",
                session.session_id
            ));
        }
        // First-open / new session: seed opening monologue (主线开场白).
        let _ = seed_opening_if_needed(&mut session, &pack);
        self.save(session)
    }

    /// When pack deleted: mark sessions pack_missing (read-only play later).
    pub fn mark_pack_missing(&self, pack_id: &str) -> CoreResult<usize> {
        let _g = self.lock.lock();
        fs::create_dir_all(self.dir())?;
        let mut n = 0usize;
        for ent in fs::read_dir(self.dir())? {
            let ent = ent?;
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut s) = serde_json::from_str::<TavernSession>(&raw) else {
                continue;
            };
            if s.pack_id == pack_id && !s.pack_missing {
                s.pack_missing = true;
                s.updated_at = now_rfc3339();
                let _ = write_atomic(&path, &serde_json::to_string_pretty(&s)?);
                n += 1;
            }
        }
        Ok(n)
    }
}

// ─── PersonaStore ────────────────────────────────────────────────────────────

// ─── Saves / checkpoints (ST-7) ──────────────────────────────────────────────

/// Agent 自建面板（吸收自 Liyuan panels.ts）：舞台美术/元信息层，绝不承载剧情正文。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TavernPanel {
    pub name: String,
    /// markdown | svg | html
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub updated_at: String,
}

impl TavernPanel {
    pub fn new(name: String, kind: String, content: String) -> Self {
        Self {
            name,
            kind,
            content,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// P0-1: 回合级检查点（吸收自梨园 story_command /rewind /reroll）。
/// 只存可变字段快照 + messages 长度，不持有 messages 引用/整 session，供回退恢复。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnCheckpoint {
    pub turn: u32,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub chapter_cursor: Option<String>,
    #[serde(default)]
    pub messages_len: usize,
    #[serde(default)]
    pub memory_l1: MemoryL1,
    #[serde(default)]
    pub memory_l2: MemoryL2,
    #[serde(default)]
    pub present_character_ids: Vec<String>,
    #[serde(default)]
    pub focus_character_id: Option<String>,
    #[serde(default)]
    pub director_plan: Option<DirectorPlan>,
    #[serde(default)]
    pub last_event: Option<EventLogEntry>,
    #[serde(default)]
    pub panels: Vec<TavernPanel>,
    #[serde(default)]
    pub created_at: String,
}

impl TavernSession {
    /// P0-1: 快照当前完成态（turn 已 +1、assistant 已入列、memory/director/panels 已更新）。
    /// 在回合 job 落盘前调用；cap 30，超限丢弃最旧。
    pub fn push_checkpoint(&mut self) {
        let cp = TurnCheckpoint {
            turn: self.turn,
            node_id: self.node_id.clone(),
            chapter_cursor: self.chapter_cursor.clone(),
            messages_len: self.messages.len(),
            memory_l1: self.memory_l1.clone(),
            // P0-4: checkpoint 剥离 L2 embedding——30 份快照 × 512 维冗余达 MB 级。
            // 恢复后空向量由 RAG 的 `!e.embedding.is_empty()` 保护自动跳过语义匹配，
            // 仅文本/关键词匹配仍生效（语义 RAG 由当前会话 memory_l2 提供）。
            memory_l2: MemoryL2 {
                events: self
                    .memory_l2
                    .events
                    .iter()
                    .map(|e| MemoryL2Event {
                        id: e.id.clone(),
                        turn: e.turn,
                        kind: e.kind.clone(),
                        summary: e.summary.clone(),
                        actors: e.actors.clone(),
                        node_id: e.node_id.clone(),
                        embedding: Vec::new(),
                    })
                    .collect(),
                updated_at_turn: self.memory_l2.updated_at_turn,
            },
            present_character_ids: self.present_character_ids.clone(),
            focus_character_id: self.focus_character_id.clone(),
            director_plan: self.director_plan.clone(),
            last_event: self.last_event.clone(),
            panels: self.panels.clone(),
            created_at: now_rfc3339(),
        };
        self.checkpoints.push(cp);
        const MAX_CHECKPOINTS: usize = 30;
        if self.checkpoints.len() > MAX_CHECKPOINTS {
            let drop = self.checkpoints.len() - MAX_CHECKPOINTS;
            self.checkpoints.drain(..drop);
        }
    }

    /// P0-1: 回退 steps 个回合：恢复 checkpoint 快照字段并按 messages_len 截断消息。
    /// 列表尾部恒为「当前已完成回合」，故 steps 从尾前一个开始取（回退 1 = 倒数第二个）。
    /// 返回实际回退步数（0 = 无可回退）。
    pub fn restore_checkpoint(&mut self, steps: usize) -> CoreResult<usize> {
        if steps == 0 || self.checkpoints.is_empty() {
            return Ok(0);
        }
        let len = self.checkpoints.len();
        let idx = len.saturating_sub(steps + 1).min(len - 1);
        let cp = self.checkpoints[idx].clone();
        self.turn = cp.turn;
        self.node_id = cp.node_id;
        self.chapter_cursor = cp.chapter_cursor;
        self.memory_l1 = cp.memory_l1;
        self.memory_l2 = cp.memory_l2;
        self.present_character_ids = cp.present_character_ids;
        self.focus_character_id = cp.focus_character_id;
        self.director_plan = cp.director_plan;
        self.last_event = cp.last_event;
        self.panels = cp.panels;
        self.messages.truncate(cp.messages_len);
        self.checkpoints.truncate(idx + 1);
        Ok(len - 1 - idx)
    }

    /// [morphling ROMA P0 2026-08-19] 记录当前回合执行阶段进度（幂等快照，跟随会话落盘）。
    /// 用于崩溃/中断时判断「上回合死在哪一步」，不改故事流。run_id 可为空串。
    pub fn set_turn_progress(&mut self, turn: u32, phase: TurnPhase, run_id: &str, note: &str) {
        self.turn_progress = Some(TurnProgress {
            turn,
            phase,
            run_id: run_id.to_string(),
            note: note.to_string(),
            at: now_rfc3339(),
        });
    }
}

/// MCP 外设工具执行结果摘要（吸收自 Liyuan mcp.ts, 默认仅本机 stdio server）。
/// 只存截断摘要防注入, 完整结果不落库。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultBrief {
    /// 形如 `gemini-search:web_search`
    pub tool: String,
    pub ok: bool,
    pub summary: String,
}

/// skill 工具按需加载结果（吸收自 denova skill.NewMiddleware：模型声明需要完整 SKILL.md 后，下轮构建 system prompt 时注入全文）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillLoadInfo {
    /// 请求时命中的写作档位（lite|standard|heavy）。
    pub tier: String,
    /// 完整 SKILL.md + 分档 stage 模板的注入文本。
    pub markdown: String,
}

/// 世界线视图（吸收自 Liyuan worldline.ts 的 WorldlineView）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldlineLine {
    pub id: String,
    /// 分叉源存档 id（主线无）。
    pub fork_from_save_id: Option<String>,
    pub saves: Vec<TavernSaveMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldlineView {
    pub lines: Vec<WorldlineLine>,
    /// 当前所在世界线 id。
    pub current_worldline_id: String,
    /// 当前分支上最近的存档 id（无则 None）。
    pub current_save_id: Option<String>,
}

/// 由会话 + 存档列表构建世界线视图（纯函数）。
/// 主线 = fork_from 为空的存档；分叉线 = 从某存档 fork 出的存档序列。
pub fn build_worldline(session: &TavernSession, saves: &[TavernSaveMeta]) -> WorldlineView {
    let mut lines: Vec<WorldlineLine> = Vec::new();

    // 主线：worldline_id == "main"（兼容旧存档：无 worldline_id 时按 fork_from 为空处理）
    let mut main: Vec<TavernSaveMeta> = saves
        .iter()
        .filter(|s| s.worldline_id == "main" || (s.worldline_id.is_empty() && s.fork_from_save_id.is_none()))
        .cloned()
        .collect();
    main.sort_by(|a, b| a.turn.cmp(&b.turn));
    lines.push(WorldlineLine {
        id: "main".into(),
        fork_from_save_id: None,
        saves: main,
    });

    // 分叉线：按 fork_from_save_id 分组
    let mut fork_map: std::collections::BTreeMap<String, Vec<TavernSaveMeta>> =
        std::collections::BTreeMap::new();
    for s in saves.iter().filter(|s| s.worldline_id != "main") {
        let key = s.worldline_id.clone();
        fork_map.entry(key).or_default().push(s.clone());
    }
    for (wid, mut svs) in fork_map {
        svs.sort_by(|a, b| a.turn.cmp(&b.turn));
        let fork_from = svs.iter().find_map(|s| s.fork_from_save_id.clone());
        lines.push(WorldlineLine {
            id: wid,
            fork_from_save_id: fork_from,
            saves: svs,
        });
    }

    let current_worldline_id = session
        .current_worldline_id
        .clone()
        .unwrap_or_else(|| "main".into());
    let current_save_id = saves
        .iter()
        .filter(|s| s.worldline_id == current_worldline_id)
        .max_by(|a, b| a.created_at.cmp(&b.created_at))
        .map(|s| s.save_id.clone());

    WorldlineView {
        lines,
        current_worldline_id,
        current_save_id,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TavernSaveMeta {
    pub save_id: String,
    pub session_id: String,
    pub label: String,
    pub turn: u32,
    pub node_id: Option<String>,
    pub chapter_cursor: Option<String>,
    pub play_mode: PlayMode,
    pub created_at: String,
    /// 分叉源存档 id（主线存档无）。
    #[serde(default)]
    pub fork_from_save_id: Option<String>,
    /// 所属世界线 id（"main" 或分叉线 id）。
    #[serde(default)]
    pub worldline_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TavernSave {
    pub save_id: String,
    pub session_id: String,
    pub label: String,
    pub turn: u32,
    pub created_at: String,
    /// 分叉源存档 id（主线存档无）。
    #[serde(default)]
    pub fork_from_save_id: Option<String>,
    /// 所属世界线 id。
    #[serde(default)]
    pub worldline_id: String,
    pub snapshot: TavernSession,
}

impl TavernSessionStore {
    fn saves_root(&self) -> PathBuf {
        self.data.tavern_saves_dir()
    }

    fn saves_dir_for(&self, session_id: &str) -> CoreResult<PathBuf> {
        let id = safe_id(session_id)?;
        Ok(self.saves_root().join(id))
    }

    fn save_path(&self, session_id: &str, save_id: &str) -> CoreResult<PathBuf> {
        let sid = safe_id(session_id)?;
        let sav = safe_id(save_id)?;
        if !sav.starts_with("save-") {
            return Err(CoreError::BadRequest("save id must start with save-".into()));
        }
        Ok(self.saves_root().join(sid).join(format!("{sav}.json")))
    }

    pub fn list_saves(&self, session_id: &str) -> CoreResult<Vec<TavernSaveMeta>> {
        let _g = self.lock.lock();
        let dir = self.saves_dir_for(session_id)?;
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for ent in fs::read_dir(&dir)? {
            let ent = ent?;
            let name = ent.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }
            match fs::read_to_string(ent.path()) {
                Ok(raw) => {
                    if let Ok(s) = serde_json::from_str::<TavernSave>(&raw) {
                        out.push(TavernSaveMeta {
                            save_id: s.save_id,
                            session_id: s.session_id,
                            label: s.label,
                            turn: s.turn,
                            node_id: s.snapshot.node_id.clone(),
                            chapter_cursor: s.snapshot.chapter_cursor.clone(),
                            play_mode: s.snapshot.play_mode,
                            created_at: s.created_at,
                            fork_from_save_id: s.fork_from_save_id,
                            worldline_id: s.worldline_id,
                        });
                    }
                }
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// 世界线视图（吸收自 Liyuan worldline.ts）：按分叉组织存档线。
    /// 注意：不能持锁调 list_saves（其内部会再次 lock，重入死锁）。
    pub fn worldline(&self, session_id: &str) -> CoreResult<WorldlineView> {
        let sess = {
            let _g = self.lock.lock();
            let path_sess = self.path_for(session_id)?;
            if !path_sess.exists() {
                return Err(CoreError::NotFound(format!("session not found: {session_id}")));
            }
            serde_json::from_str::<TavernSession>(&fs::read_to_string(&path_sess)?)?
        };
        let saves = self.list_saves(session_id)?;
        Ok(build_worldline(&sess, &saves))
    }

    pub fn create_save(&self, session_id: &str, label: Option<String>) -> CoreResult<TavernSave> {
        let _g = self.lock.lock();
        let path_sess = self.path_for(session_id)?;
        if !path_sess.exists() {
            return Err(CoreError::NotFound(format!("session not found: {session_id}")));
        }
        let mut sess: TavernSession = serde_json::from_str(&fs::read_to_string(&path_sess)?)?;
        let save_id = format!("save-{}", Uuid::new_v4());
        let now = now_rfc3339();
        let label = label
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("第{}回合", sess.turn));

        // 世界线分叉检测（吸收自 Liyuan worldline.ts）：
        // 回档后走出不同路（turn 前进）再存档 → 产生新世界线；同回合立即存 → 不算分叉。
        let restored_marker = sess.last_restored_save_id.clone();
        let (fork_from_save_id, worldline_id) =
            if let Some(restored_id) = restored_marker {
            let restored_turn = fs::read_to_string(self.save_path(session_id, &restored_id)?)
                .ok()
                .and_then(|raw| serde_json::from_str::<TavernSave>(&raw).ok())
                .map(|s| s.turn)
                .unwrap_or(u32::MAX);
            if restored_turn < sess.turn {
                let new_line = format!("wl-{}", Uuid::new_v4());
                sess.last_restored_save_id = None; // 一次回档 → 一次分叉
                (Some(restored_id.clone()), new_line)
            } else {
                (None, sess.current_worldline_id.clone().unwrap_or_else(|| "main".into()))
            }
        } else {
            (None, sess.current_worldline_id.clone().unwrap_or_else(|| "main".into()))
        };

        let mut snapshot = sess;
        snapshot.active_run_id = None;
        let save = TavernSave {
            save_id: save_id.clone(),
            session_id: session_id.to_string(),
            label,
            turn: snapshot.turn,
            created_at: now,
            fork_from_save_id,
            worldline_id: worldline_id.clone(),
            snapshot,
        };
        let path = self.save_path(session_id, &save_id)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&path, &serde_json::to_string_pretty(&save)?)?;

        // 同步回写会话：当前世界线推进（仅分叉时），并清除已消费的回档标记
        if save.fork_from_save_id.is_some() {
            if let Ok(mut live) = serde_json::from_str::<TavernSession>(&fs::read_to_string(&path_sess)?) {
                live.current_worldline_id = Some(worldline_id);
                live.last_restored_save_id = None;
                let _ = write_atomic(&path_sess, &serde_json::to_string_pretty(&live)?);
            }
        }
        Ok(save)
    }

    pub fn get_save(&self, session_id: &str, save_id: &str) -> CoreResult<TavernSave> {
        let _g = self.lock.lock();
        let path = self.save_path(session_id, save_id)?;
        if !path.exists() {
            return Err(CoreError::NotFound(format!("save not found: {save_id}")));
        }
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn delete_save(&self, session_id: &str, save_id: &str) -> CoreResult<()> {
        let _g = self.lock.lock();
        let path = self.save_path(session_id, save_id)?;
        if !path.exists() {
            return Err(CoreError::NotFound(format!("save not found: {save_id}")));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    /// Restore a save into the live session (same sessionId). Keeps sessionId; clears active run.
    /// Fork a save into a NEW session (cross-session branch, Front Porch forkFromMessage parity).
    /// New session_id, same pack, snapshot restored, parent linked via fork_from_save_id lineage.
    /// Journal/Growth copySessionTo semantics: receipts >= cursor stay behind — here cursor = snapshot turn's message len.
    pub fn fork_save_to_session(&self, session_id: &str, save_id: &str, label: Option<String>) -> CoreResult<TavernSession> {
        let _g = self.lock.lock();
        let path_save = self.save_path(session_id, save_id)?;
        if !path_save.exists() {
            return Err(CoreError::NotFound(format!("save not found: {save_id}")));
        }
        let save: TavernSave = serde_json::from_str(&fs::read_to_string(&path_save)?)?;
        let mut snap = save.snapshot.clone();
        let new_id = format!("tavern-session-{}", Uuid::new_v4());
        snap.session_id = new_id.clone();
        snap.active_run_id = None;
        snap.checkpoints = vec![];
        snap.turn_progress = None;
        // lineage: new worldline forked from this save
        snap.last_restored_save_id = None;
        snap.current_worldline_id = Some(format!("wl-{}", Uuid::new_v4()));
        let now = now_rfc3339();
        snap.created_at = now.clone();
        snap.updated_at = now;
        let flabel = label.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| format!("分叉自「{}」", save.label));
        snap.messages.push(TavernMessage {
            id: format!("msg-{}", Uuid::new_v4()),
            role: "assistant".into(),
            content: format!("〔分叉〕新分支「{flabel}」（源存档「{}」第{}回合）。旧会话不受影响。", save.label, save.turn),
            created_at: now_rfc3339(),
            options: vec![],
            swipes: vec![],
            swipe_index: 0,
            engine_tag: None,
            program: None,
            reasoning: None,
            tokens: 0,
        });
        let _ = safe_id(&new_id)?;
        fs::create_dir_all(self.dir())?;
        let path = self.path_for(&new_id)?;
        write_atomic(&path, &serde_json::to_string_pretty(&snap)?)?;
        // record fork lineage as a save in the NEW session pointing back
        let fork_save = TavernSave {
            save_id: format!("save-{}", Uuid::new_v4()),
            session_id: new_id.clone(),
            label: format!("分叉点（源 {}/{})", session_id, save_id),
            turn: snap.turn,
            created_at: now_rfc3339(),
            fork_from_save_id: Some(save_id.to_string()),
            worldline_id: snap.current_worldline_id.clone().unwrap_or_else(|| "main".into()),
            snapshot: snap.clone(),
        };
        let fpath = self.save_path(&new_id, &fork_save.save_id)?;
        if let Some(parent) = fpath.parent() { let _ = fs::create_dir_all(parent); }
        let _ = write_atomic(&fpath, &serde_json::to_string_pretty(&fork_save)?);
        Ok(snap)
    }

    pub fn restore_save(&self, session_id: &str, save_id: &str) -> CoreResult<TavernSession> {
        let _g = self.lock.lock();
        let path_save = self.save_path(session_id, save_id)?;
        if !path_save.exists() {
            return Err(CoreError::NotFound(format!("save not found: {save_id}")));
        }
        let save: TavernSave = serde_json::from_str(&fs::read_to_string(&path_save)?)?;
        let path_sess = self.path_for(session_id)?;
        if !path_sess.exists() {
            return Err(CoreError::NotFound(format!("session not found: {session_id}")));
        }
        let live: TavernSession = serde_json::from_str(&fs::read_to_string(&path_sess)?)?;
        if live.active_run_id.is_some() {
            return Err(CoreError::BadRequest(
                "turn in progress; stop or wait before restore".into(),
            ));
        }
        let mut snap = save.snapshot;
        snap.session_id = live.session_id.clone();
        snap.active_run_id = None;
        // 世界线（吸收自 Liyuan worldline.ts）：记录回档来源 + 切回该存档所在世界线
        snap.last_restored_save_id = Some(save_id.to_string());
        snap.current_worldline_id = Some(save.worldline_id.clone());
        let note = format!(
            "〔回档〕已恢复存档「{}」（第{}回合 · 节点 {}）。",
            save.label,
            save.turn,
            snap.node_id.as_deref().unwrap_or("?")
        );
        snap.messages.push(TavernMessage {
            id: format!("msg-{}", Uuid::new_v4()),
            role: "assistant".into(),
            content: note,
            created_at: now_rfc3339(),
            options: vec![],
            engine_tag: None,
            program: None,
            reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
        });
        // inline save under same lock
        if snap.session_id.trim().is_empty() {
            snap.session_id = format!("tavern-session-{}", Uuid::new_v4());
        }
        let _ = safe_id(&snap.session_id)?;
        let now = now_rfc3339();
        if snap.created_at.is_empty() {
            snap.created_at = now.clone();
        }
        snap.updated_at = now;
        fs::create_dir_all(self.dir())?;
        let path = self.path_for(&snap.session_id)?;
        write_atomic(&path, &serde_json::to_string_pretty(&snap)?)?;
        Ok(snap)
    }
}

#[derive(Clone)]
pub struct TavernPersonaStore {
    data: DataRoot,
    lock: std::sync::Arc<Mutex<()>>,
}

impl TavernPersonaStore {
    pub fn new(data: DataRoot) -> Self {
        let _ = data.ensure_layout();
        Self {
            data,
            lock: std::sync::Arc::new(Mutex::new(())),
        }
    }

    fn path_for(&self, character_id: &str) -> CoreResult<PathBuf> {
        let id = safe_id(character_id)?;
        Ok(self.data.tavern_persona_dir().join(format!("{id}.json")))
    }

    pub fn get(&self, character_id: &str) -> CoreResult<TavernPersona> {
        let _g = self.lock.lock();
        let path = self.path_for(character_id)?;
        if !path.exists() {
            return Ok(TavernPersona {
                character_id: character_id.to_string(),
                ..Default::default()
            });
        }
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, mut persona: TavernPersona) -> CoreResult<TavernPersona> {
        let _g = self.lock.lock();
        if persona.character_id.trim().is_empty() {
            return Err(CoreError::BadRequest("characterId required".into()));
        }
        let id = safe_id(&persona.character_id)?;
        persona.character_id = id;
        persona.updated_at = now_rfc3339();
        fs::create_dir_all(self.data.tavern_persona_dir())?;
        let path = self.path_for(&persona.character_id)?;
        write_atomic(&path, &serde_json::to_string_pretty(&persona)?)?;
        Ok(persona)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────


/// Rotate focus among present characters. Returns new focus id.
pub fn rotate_focus_character(session: &mut TavernSession) -> Option<String> {
    if !session.speaker_rotation {
        return session.focus_character_id.clone();
    }
    let candidates = session.present_character_ids.clone();
    if candidates.is_empty() {
        return session.focus_character_id.clone();
    }
    let cur = session.focus_character_id.clone();
    let idx = cur
        .as_ref()
        .and_then(|c| candidates.iter().position(|x| x == c))
        .unwrap_or(0);
    let next = candidates[(idx + 1) % candidates.len()].clone();
    session.focus_character_id = Some(next.clone());
    Some(next)
}

pub fn ensure_focus_character(session: &mut TavernSession) {
    if session.focus_character_id.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
        // if focus not in present, reset
        if let Some(f) = &session.focus_character_id {
            if !session.present_character_ids.is_empty() && !session.present_character_ids.contains(f) {
                session.focus_character_id = session.present_character_ids.first().cloned();
            }
        }
        return;
    }
    session.focus_character_id = session
        .present_character_ids
        .first()
        .cloned()
        .or(session.player.control_character_id.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn clean_cast_name_keeps_xiang_surname() {
        // [fix 2026-08-16] 男主「向明初」以「向」为姓，曾被 non_name_starts 黑名单误杀，
        // 导致蒸馏成功的男主卡在书架/角色列表不可见。
        assert!(is_clean_cast_name("向明初"));
        assert!(is_clean_cast_name("向华强"));
        // 2 字介词短语仍应被拒（「向」+方位/人称）
        assert!(!is_clean_cast_name("向他"));
        assert!(!is_clean_cast_name("向前"));
        assert!(!is_clean_cast_name("向外"));
    }

    #[test]
    fn clean_cast_name_keeps_regular_names() {
        assert!(is_clean_cast_name("庄眉"));
        assert!(is_clean_cast_name("山楂"));
        assert!(is_clean_cast_name("莫旺财"));
        assert!(is_clean_cast_name("宿舍阿姨"));
    }

    mod tempfile_shim {
        use super::*;
        pub struct Tmp {
            path: PathBuf,
        }
        impl Tmp {
            pub fn new() -> Self {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!("kaleido-tavern-test-{nanos}"));
                fs::create_dir_all(&path).unwrap();
                Self { path }
            }
            pub fn path(&self) -> &Path {
                &self.path
            }
        }
        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    #[test]
    fn tier_min3() {
        assert_eq!(
            ContentTier::min3(ContentTier::Open, ContentTier::Standard, ContentTier::Open),
            ContentTier::Standard
        );
        assert_eq!(
            ContentTier::min3(ContentTier::Safe, ContentTier::Open, ContentTier::Open),
            ContentTier::Safe
        );
    }

    #[test]
    fn b1_regression_turn_save_after_acquire_persists_turn_and_messages() {
        // B1 (P0, 2026-08-26): tavern turn succeeded 但 session.turns=0。
        // 生产序列（story_tavern start_turn → worker 尾部落盘）：
        //   acquire_turn（锁写 activeRunId）→ save_with_revision(用户消息) →
        //   worker: get() → push(asst)+turn+=1 → save()【无 base_revision 的普通保存】
        // 实测该序列在 04:45 生产环境把回合弄丢（turn 回 0 / messages 回 1），
        // 本测试固化该最小复现：普通 save 绝不允许回滚已持久化的 turn/messages。
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let packs = PackStore::new(data.clone());
        let sessions = TavernSessionStore::new(data.clone());

        let pack = packs.ensure_demo_pack().unwrap();
        let mut sess = sessions
            .create_from_pack(
                &packs,
                CreateSessionRequest {
                    pack_id: pack.id.clone(),
                    playable: Playable::P3,
                    play_mode: PlayMode::Mainline,
                    quality: Quality::Lite,
                    user_tier: ContentTier::Open,
                    global_tier: Some(ContentTier::Open),
                    author_project_id: None,
                    work_id: None,
                    entry: None,
                    player_display_name: Some("b1".into()),
                    adult_confirmed: true,
                    title: None,
                    owner: None,
                },
            )
            .unwrap();
        // create_from_pack 不 seed 开场（生产由 GET auto-seed / ensure-opening 负责）；
        // 本测试自 seed 一条开场 assistant 消息以对齐生产基线（messages=1）。
        sess.messages.push(TavernMessage {
            id: "msg-b1-opening".into(),
            role: "assistant".into(),
            content: "【开场】雨夜，玩家抵达旧茶馆门前。".into(),
            created_at: "2026-08-26T04:40:00Z".into(),
            options: vec![],
            swipes: vec![],
            swipe_index: 0,
            engine_tag: None,
            program: None,
            reasoning: None,
            tokens: 0,
        });
        sess.opening_seeded = true;
        sessions.save(sess.clone()).unwrap();

        // 1) handler: 原子取锁（activeRunId=run）
        let run_id = "b1-run-1";
        sess = sessions.acquire_turn(&sess.session_id, run_id).unwrap();
        assert_eq!(sess.active_run_id.as_deref(), Some(run_id));

        // 2) handler: CAS 写入用户消息（save_with_revision, base=acquire 返回的 updated_at）
        sess.messages.push(TavernMessage {
            id: "msg-b1-user".into(),
            role: "user".into(),
            content: "请直接回复两个字：好的".into(),
            created_at: "2026-08-26T04:45:18Z".into(),
            options: vec![],
            swipes: vec![],
            swipe_index: 0,
            engine_tag: None,
            program: None,
            reasoning: None,
            tokens: 0,
        });
        sess.active_run_id = Some(run_id.to_string());
        let base_rev = sess.updated_at.clone();
        sessions
            .save_with_revision(sess.clone(), &base_rev)
            .expect("user-msg CAS save must succeed");

        // 3) worker 尾部: get() → push assistant + turn+=1 + 清锁 → 普通 save()
        let mut disk = sessions.get(&sess.session_id).unwrap();
        disk.messages.push(TavernMessage {
            id: "msg-b1-asst".into(),
            role: "assistant".into(),
            content: "好的".into(),
            created_at: "2026-08-26T04:45:51Z".into(),
            options: vec![],
            swipes: vec![],
            swipe_index: 0,
            engine_tag: None,
            program: None,
            reasoning: None,
            tokens: 0,
        });
        disk.turn += 1;
        disk.active_run_id = None;
        sessions.save(disk.clone()).expect("worker final save must succeed");

        // 4) 断言落盘终态 —— B1 症状即此处失败（turn=0 / messages 缺失）
        let after = sessions.get(&sess.session_id).unwrap();
        assert_eq!(after.turn, 1, "turn must persist after successful turn");
        assert_eq!(after.messages.len(), 4, "opening+user+assistant(+1) must persist");
        assert!(after.messages.iter().any(|m| m.role == "user" && m.content == "请直接回复两个字：好的"));
        assert_eq!(
            after.messages.last().map(|m| (m.role.as_str(), m.content.as_str())),
            Some(("assistant", "好的"))
        );
        assert_eq!(after.active_run_id, None);
    }

    #[test]
    fn demo_pack_playable_and_session() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let packs = PackStore::new(data.clone());
        let sessions = TavernSessionStore::new(data.clone());

        let pack = packs.ensure_demo_pack().unwrap();
        assert_eq!(pack.id, "demo-rain-alley");
        assert!(pack.is_playable());
        assert_eq!(pack.chapters.len(), 2);
        assert!(packs
            .read_chapter_body(&pack.id, "chapters/ch01.md")
            .unwrap()
            .contains("沈棠"));

        let sess = sessions
            .create_from_pack(
                &packs,
                CreateSessionRequest {
                    pack_id: pack.id.clone(),
                    playable: Playable::P3,
                    play_mode: PlayMode::Mainline,
                    quality: Quality::Lite,
                    user_tier: ContentTier::Open,
                    global_tier: Some(ContentTier::Open),
                    author_project_id: None,
                    work_id: None,
                    entry: Some(EntryConfig {
                        entry_role: Some(EntryRole::Supporting),
                        vessel_character_id: Some("cc-linwan".into()),
                        meta_knowledge: MetaKnowledge::Reader,
                        rewrite_intensity: RewriteIntensity::Canon,
                        isekai: None,
                        extra_profile: None,
                    }),
                    player_display_name: Some("测试者".into()),
                    adult_confirmed: true,
                    title: None,
                    owner: None,
                },
            )
            .unwrap();
        assert!(sess.session_id.starts_with("tavern-session-"));
        assert_eq!(sess.content_tier, ContentTier::Standard); // pack max standard
        assert_eq!(sess.node_id.as_deref(), Some("n1"));

        let loaded = sessions.get(&sess.session_id).unwrap();
        assert_eq!(loaded.pack_id, "demo-rain-alley");

        packs.delete(&pack.id).unwrap();
        let n = sessions.mark_pack_missing("demo-rain-alley").unwrap();
        assert!(n >= 1);
        let after = sessions.get(&sess.session_id).unwrap();
        assert!(after.pack_missing);
    }

    #[test]
    fn u13_create_session_auto_mounts_work_compass() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let packs = PackStore::new(data.clone());
        let sessions = TavernSessionStore::new(data.clone());
        let compass_store = CompassStore::new(data.clone());

        let pack = packs.ensure_demo_pack().unwrap();
        let work = "work-u13-demo";
        // 1) 目标 work 已存在创作罗盘（等价于前端先设置罗盘再开新会话）
        compass_store
            .save(
                work,
                &Compass::new("承诺：雨巷的秘密", "近期目标：找到沈棠的信"),
            )
            .unwrap();
        // 2) 开新 session 时带 work_id → 自动挂载罗盘
        let sess = sessions
            .create_from_pack(
                &packs,
                CreateSessionRequest {
                    pack_id: pack.id.clone(),
                    playable: Playable::P3,
                    play_mode: PlayMode::Mainline,
                    quality: Quality::Lite,
                    user_tier: ContentTier::Open,
                    global_tier: Some(ContentTier::Open),
                    author_project_id: None,
                    work_id: Some(work.to_string()),
                    entry: Some(EntryConfig {
                        entry_role: Some(EntryRole::Supporting),
                        vessel_character_id: Some("cc-linwan".into()),
                        meta_knowledge: MetaKnowledge::Reader,
                        rewrite_intensity: RewriteIntensity::Canon,
                        isekai: None,
                        extra_profile: None,
                    }),
                    player_display_name: Some("测试者".into()),
                    adult_confirmed: true,
                    title: None,
                    owner: None,
                },
            )
            .unwrap();
        assert_eq!(
            sess.actor_states.compass().author_intent,
            "承诺：雨巷的秘密"
        );
        assert_eq!(
            sess.actor_states.compass().current_focus,
            "近期目标：找到沈棠的信"
        );
        // 落盘后 reload 仍挂载（持久化正确）
        let loaded = sessions.get(&sess.session_id).unwrap();
        assert!(!loaded.actor_states.compass().is_empty());
        // 3) 不带 work_id → 不自动挂载（保持空罗盘，现状不变）
        let sess2 = sessions
            .create_from_pack(
                &packs,
                CreateSessionRequest {
                    pack_id: pack.id.clone(),
                    playable: Playable::P3,
                    play_mode: PlayMode::Mainline,
                    quality: Quality::Lite,
                    user_tier: ContentTier::Open,
                    global_tier: Some(ContentTier::Open),
                    author_project_id: None,
                    work_id: None,
                    entry: None,
                    player_display_name: Some("无罗盘".into()),
                    adult_confirmed: true,
                    title: None,
                    owner: None,
                },
            )
            .unwrap();
        assert!(sess2.actor_states.compass().is_empty());
    }

    #[test]
    fn persona_roundtrip() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let store = TavernPersonaStore::new(data);
        let p = store
            .save(TavernPersona {
                character_id: "cc-linwan".into(),
                display_name: "林晚".into(),
                memory_l4: MemoryL4 {
                    affinity: json!({"cc-shentang": 55}),
                    secrets_known: vec!["有人跟踪".into()],
                    promises: vec![],
                    relationships: json!({}),
                },
                notes: "跨会话好感".into(),
                updated_at: String::new(),
            })
            .unwrap();
        let g = store.get("cc-linwan").unwrap();
        assert_eq!(g.display_name, "林晚");
        assert_eq!(p.character_id, g.character_id);
    }

    pub(super) fn minimal_pack() -> StoryPack {
        StoryPack {
            id: String::new(),
            title: "CAS 测试".into(),
            source: PackSource::default(),
            characters: vec![],
            world_book_ids: vec![],
            chapters: vec![],
            nodes: vec![],
            lore_entries: vec![],
            default_mode: PlayMode::Mainline,
            max_tier: ContentTier::Standard,
            language: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            stage_director: StageDirectorConfig::default(),
            event_packages: vec![],
            actor_state_config: ActorStatePackConfig::default(),
            worldline: vec![],
        }
    }

    pub(super) fn minimal_session() -> TavernSession {
        TavernSession {
            session_id: String::new(),
            pack_id: "pack-cas-test".into(),
            pack_missing: false,
            owner: None,
            quality: Quality::Lite,
            playable: Playable::P1,
            play_mode: PlayMode::Mainline,
            content_tier: ContentTier::Standard,
            user_tier_request: ContentTier::Standard,
            entry: EntryConfig::default(),
            chapter_cursor: None,
            node_id: None,
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
            speaker_rotation: false,
            player: PlayerState::default(),
            memory_l1: MemoryL1::default(),
            memory_l2: MemoryL2::default(),
            memory_l3: MemoryL3::default(),
            memory_l4: MemoryL4::default(),
            guard_events: vec![],
            messages: vec![],
            active_run_id: None,
            adult_confirmed: true,
            title: "CAS 测试".into(),
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
            turn_progress: None,
            diary_config: None,
            pockets: Default::default(),
            pockets_enabled: true,
            needs: Default::default(),
            growth: Default::default(),
            world_climate: Default::default(),
            chaos: Default::default(),
            milestones: Default::default(),
            objectives: Default::default(),
            ambitions: Default::default(),
            dream: Default::default(),
            episodes: Default::default(),
            journal: Default::default(),
            relationships: Default::default(),
            pending_swipes: Default::default(),
            promises: Default::default(),
            preferences: Default::default(),
            presence: Default::default(),
            timed_world_info: Default::default(),
            event_extract: true,
            world: WorldState::default(),
            game_clock: Default::default(),
        }
    }

    #[test]
    fn pack_save_with_revision_ok() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let packs = PackStore::new(data);

        let saved = packs.save(minimal_pack()).unwrap();
        assert!(!saved.updated_at.is_empty());
        assert!(!saved.id.is_empty());

        let again = packs
            .save_with_revision(saved.clone(), &saved.updated_at)
            .unwrap();
        assert_eq!(again.id, saved.id);
        assert!(!again.updated_at.is_empty());
    }

    #[test]
    fn pack_save_with_revision_conflict() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let packs = PackStore::new(data);

        let saved = packs.save(minimal_pack()).unwrap();
        let wrong = format!("{}bogus", saved.updated_at);
        let err = packs.save_with_revision(saved.clone(), &wrong).unwrap_err();
        assert!(
            matches!(err, CoreError::Conflict(_)),
            "expected Conflict, got {err:?}"
        );

        // Disk content must be untouched by the failed write.
        let on_disk = packs.load_unlocked(&saved.id).unwrap();
        assert_eq!(on_disk.updated_at, saved.updated_at);
        assert_eq!(on_disk.title, saved.title);
    }

    #[test]
    fn pack_save_without_revision_compat() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let packs = PackStore::new(data);

        let first = packs.save(minimal_pack()).unwrap();
        let reloaded = packs.load_unlocked(&first.id).unwrap();
        let second = packs.save(reloaded).unwrap();
        assert!(!first.updated_at.is_empty());
        assert!(!second.updated_at.is_empty());
        assert_eq!(second.id, first.id);
    }

    #[test]
    fn pack_save_with_revision_missing_file() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let packs = PackStore::new(data);

        let mut p = minimal_pack();
        p.id = "pack-cas-missing".into();
        let err = packs
            .save_with_revision(p, "2026-08-01T00:00:00Z")
            .unwrap_err();
        assert!(
            matches!(err, CoreError::Conflict(_)),
            "expected Conflict for missing file, got {err:?}"
        );
    }

    #[test]
    fn session_save_with_revision_ok_and_conflict() {
        let tmp = tempfile_shim::Tmp::new();
        let data = DataRoot::new(tmp.path()).unwrap();
        let sessions = TavernSessionStore::new(data);

        let saved = sessions.save(minimal_session()).unwrap();
        assert!(!saved.updated_at.is_empty());

        let ok = sessions
            .save_with_revision(saved.clone(), &saved.updated_at)
            .unwrap();
        assert_eq!(ok.session_id, saved.session_id);

        let wrong = format!("{}bogus", saved.updated_at);
        let err = sessions.save_with_revision(saved, &wrong).unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)));
    }

    #[test]
    fn checkpoint_push_restore_rewinds_messages_and_fields() {
        let mut s = minimal_session();
        fn msg(role: &str, content: &str) -> TavernMessage {
            TavernMessage {
                id: format!("msg-{}", role),
                role: role.into(),
                content: content.into(),
                created_at: String::new(),
                options: vec![],
                engine_tag: None,
                program: None,
                reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
            }
        }

        // Turn 1 completion
        s.messages.push(msg("user", "你好"));
        s.messages.push(msg("assistant", "嗨"));
        s.turn = 1;
        s.memory_l1.scene_summary = "scene-1".into();
        s.push_checkpoint();
        assert_eq!(s.checkpoints.len(), 1);

        // Turn 2 completion (memory + node mutated)
        s.messages.push(msg("user", "继续"));
        s.messages.push(msg("assistant", "好的"));
        s.turn = 2;
        s.node_id = Some("n2".into());
        s.memory_l1.scene_summary = "scene-2".into();
        s.push_checkpoint();
        assert_eq!(s.checkpoints.len(), 2);

        // Rewind 1 → back to end of turn 1
        let rewound = s.restore_checkpoint(1).unwrap();
        assert_eq!(rewound, 1);
        assert_eq!(s.turn, 1);
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages.last().unwrap().content, "嗨");
        assert_eq!(s.node_id.as_deref(), None);
        assert_eq!(s.memory_l1.scene_summary, "scene-1");
        assert_eq!(s.checkpoints.len(), 1);

        // Rewind beyond available clamps to oldest checkpoint (only 1 left → no-op)
        let rewound = s.restore_checkpoint(5).unwrap();
        assert_eq!(rewound, 0);
        assert_eq!(s.turn, 1);

        // steps=0 / empty are no-ops
        assert_eq!(s.restore_checkpoint(0).unwrap(), 0);
        s.checkpoints.clear();
        assert_eq!(s.restore_checkpoint(1).unwrap(), 0);
    }
}


#[cfg(test)]
mod focus_tests {
    use super::*;
    #[test]
    fn rotate_focus_cycles() {
        let mut s = TavernSession {
            session_id: "tavern-session-test".into(),
            pack_id: "p".into(),
            pack_missing: false,
            owner: None,
            quality: Quality::Lite,
            playable: Playable::P2,
            play_mode: PlayMode::Mainline,
            content_tier: ContentTier::Standard,
            user_tier_request: ContentTier::Standard,
            entry: EntryConfig::default(),
            chapter_cursor: None,
            node_id: None,
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
            present_character_ids: vec!["a".into(), "b".into(), "c".into()],
            focus_character_id: Some("a".into()),
            speaker_rotation: true,
            player: PlayerState::default(),
            memory_l1: MemoryL1::default(),
            memory_l2: MemoryL2::default(),
            memory_l3: MemoryL3::default(),
            memory_l4: MemoryL4::default(),
            guard_events: vec![],
            messages: vec![],
            active_run_id: None,
            adult_confirmed: true,
            title: "t".into(),
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
            turn_progress: None,
            diary_config: None,
            pockets: Default::default(),
            pockets_enabled: true,
            needs: Default::default(),
            growth: Default::default(),
            world_climate: Default::default(),
            chaos: Default::default(),
            milestones: Default::default(),
            objectives: Default::default(),
            ambitions: Default::default(),
            dream: Default::default(),
            episodes: Default::default(),
            journal: Default::default(),
            relationships: Default::default(),
            pending_swipes: Default::default(),
            promises: Default::default(),
            preferences: Default::default(),
            presence: Default::default(),
            timed_world_info: Default::default(),
            event_extract: true,
            world: WorldState::default(),
            game_clock: Default::default(),
        };
        assert_eq!(rotate_focus_character(&mut s).as_deref(), Some("b"));
        assert_eq!(rotate_focus_character(&mut s).as_deref(), Some("c"));
        assert_eq!(rotate_focus_character(&mut s).as_deref(), Some("a"));
    }
}

// ─── 世界线测例（吸收自 Liyuan worldline.ts） ──────────────────────────────

#[cfg(test)]
mod worldline_tests {
    use super::*;

    fn meta(save_id: &str, label: &str, turn: u32, fork_from: Option<&str>, wl: &str) -> TavernSaveMeta {
        TavernSaveMeta {
            save_id: save_id.into(),
            session_id: "s1".into(),
            label: label.into(),
            turn,
            node_id: None,
            chapter_cursor: None,
            play_mode: PlayMode::Mainline,
            created_at: format!("2026-08-01T00:00:{:02}Z", turn),
            fork_from_save_id: fork_from.map(|s| s.to_string()),
            worldline_id: wl.into(),
        }
    }

    fn session(wl: Option<&str>, restored: Option<&str>) -> TavernSession {
        TavernSession {
            session_id: "s1".into(),
            pack_id: "p1".into(),
            pack_missing: false,
            owner: None,
            quality: Quality::Lite,
            playable: Playable::P2,
            play_mode: PlayMode::Mainline,
            content_tier: ContentTier::Standard,
            panels: vec![],
            mcp_tool_results: vec![],
            skill_load: None,
            user_tier_request: ContentTier::Standard,
            entry: EntryConfig::default(),
            chapter_cursor: None,
            node_id: None,
            resume_node_id: None,
            opening_seeded: false,
            side_branch_node_id: None,
            side_branch_label: None,
            current_worldline_id: wl.map(|x| x.to_string()),
            last_restored_save_id: restored.map(|x| x.to_string()),
            timeline_id: "main".into(),
            turn: 10,
            present_character_ids: vec![],
            focus_character_id: None,
            speaker_rotation: false,
            player: PlayerState::default(),
            memory_l1: MemoryL1::default(),
            memory_l2: MemoryL2::default(),
            memory_l3: MemoryL3::default(),
            memory_l4: MemoryL4::default(),
            guard_events: vec![],
            messages: vec![],
            active_run_id: None,
            adult_confirmed: true,
            title: "t".into(),
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
            turn_progress: None,
            diary_config: None,
            pockets: Default::default(),
            pockets_enabled: true,
            needs: Default::default(),
            growth: Default::default(),
            world_climate: Default::default(),
            chaos: Default::default(),
            milestones: Default::default(),
            objectives: Default::default(),
            ambitions: Default::default(),
            dream: Default::default(),
            episodes: Default::default(),
            journal: Default::default(),
            relationships: Default::default(),
            pending_swipes: Default::default(),
            promises: Default::default(),
            preferences: Default::default(),
            presence: Default::default(),
            timed_world_info: Default::default(),
            event_extract: true,
            world: WorldState::default(),
            game_clock: Default::default(),
        }
    }

    #[test]
    fn test_build_worldline_main_only() {
        let saves = vec![
            meta("save-a", "第2回合", 2, None, "main"),
            meta("save-b", "第5回合", 5, None, "main"),
        ];
        let view = build_worldline(&session(Some("main"), None), &saves);
        assert_eq!(view.lines.len(), 1);
        assert_eq!(view.lines[0].id, "main");
        assert_eq!(view.lines[0].saves.len(), 2);
        assert_eq!(view.current_worldline_id, "main");
        assert_eq!(view.current_save_id.as_deref(), Some("save-b"));
    }

    #[test]
    fn test_build_worldline_fork() {
        let saves = vec![
            meta("save-a", "第2回合", 2, None, "main"),
            meta("save-b", "第5回合", 5, Some("save-a"), "wl-1"),
        ];
        let view = build_worldline(&session(Some("wl-1"), None), &saves);
        assert_eq!(view.lines.len(), 2);
        let fork = view.lines.iter().find(|l| l.id == "wl-1").expect("fork line");
        assert_eq!(fork.fork_from_save_id.as_deref(), Some("save-a"));
        assert_eq!(fork.saves.len(), 1);
        assert_eq!(view.current_worldline_id, "wl-1");
        assert_eq!(view.current_save_id.as_deref(), Some("save-b"));
    }

    #[test]
    fn test_fork_detection_turn_advanced() {
        // 回档(save-a, turn=2) 后走出不同路到 turn=5 再存 → 分叉
        let mut sess = session(None, Some("save-a"));
        sess.turn = 5;
        let _saves = vec![meta("save-a", "第2回合", 2, None, "main")];
        // create_save 逻辑的纯函数复现：restored_turn(2) < sess.turn(5) → fork
        let restored_turn = 2;
        assert!(restored_turn < sess.turn);
        // build_worldline 视角：新线从 save-a fork
        let view = build_worldline(
            &session(Some("wl-new"), None),
            &[meta("save-a", "第2回合", 2, None, "main"), meta("save-c", "第5回合", 5, Some("save-a"), "wl-new")],
        );
        assert_eq!(view.lines.len(), 2);
        assert_eq!(view.current_worldline_id, "wl-new");
    }
}

#[cfg(test)]
mod actor_state_tests {
    use super::*;

    fn field(v: Value) -> ActorFieldValue {
        ActorFieldValue {
            value_type: "number".into(),
            value: Some(v),
            min: Some(0.0),
            max: Some(100.0),
            options: vec![],
            display: Some("血".into()),
            update_instruction: Some("受击时下降".into()),
        }
    }

    fn trait_inst(pool: &str, id: &str, name: &str) -> ActorTraitInstance {
        ActorTraitInstance {
            pool_id: pool.into(),
            pool_name: None,
            trait_id: id.into(),
            name: name.into(),
            summary: Some("简述".into()),
            source_turn_id: None,
        }
    }

    #[test]
    fn build_context_text_empty_is_empty_string() {
        let sys = ActorStateSystem::default();
        assert!(sys.build_context_text().is_empty());
    }

    #[test]
    fn build_context_text_includes_character_and_fields() {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "hero".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: [("hp".into(), field(json!(80)))]
                    .into_iter()
                    .collect(),
                traits: vec![trait_inst("p1", "tr1", "热血")],
            },
        );
        let text = sys.build_context_text();
        assert!(text.contains("hero"));
        assert!(text.contains("hp"));
        assert!(text.contains("80"));
        assert!(text.contains("热血"));
        // update_instruction 应出现在备注里
        assert!(text.contains("受击时下降"));
    }

    #[test]
    fn build_context_text_injects_compass_block() {
        let mut sys = ActorStateSystem::default();
        sys.mount_compass(Compass::new("主角必须活到最后", "本周写完第三章"));
        let text = sys.build_context_text();
        assert!(text.contains("## 创作罗盘"));
        assert!(text.contains("【全书承诺】主角必须活到最后"));
        assert!(text.contains("【近期目标】本周写完第三章"));
        // 无角色时仅输出罗盘段。
        assert!(text.starts_with("## 创作罗盘\n"));
    }

    #[test]
    fn build_context_text_compass_before_characters() {
        let mut sys = ActorStateSystem::default();
        sys.mount_compass(Compass::new("全书承诺 A", ""));
        sys.actors.insert(
            "hero".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: [("hp".into(), field(json!(80)))]
                    .into_iter()
                    .collect(),
                traits: vec![trait_inst("p1", "tr1", "热血")],
            },
        );
        let text = sys.build_context_text();
        // 罗盘段置顶，角色信息在其后。
        assert!(text.starts_with("## 创作罗盘\n"));
        let compass_end = text.find("【全书承诺】").unwrap();
        let hero_pos = text.find("# hero").unwrap();
        assert!(compass_end < hero_pos, "compass must appear before characters");
        // 空字段（current_focus）不输出。
        assert!(!text.contains("【近期目标】"));
        assert!(text.contains("hp"));
    }

    #[test]
    fn build_context_text_empty_compass_not_injected() {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "hero".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: Default::default(),
                traits: vec![],
            },
        );
        let text = sys.build_context_text();
        assert!(!text.contains("创作罗盘"));
        assert!(!text.contains("【全书承诺】"));
        assert!(!text.contains("【近期目标】"));
        assert!(text.contains("hero"));
    }

    #[test]
    fn actor_system_compass_survives_json_roundtrip() {
        let mut sys = ActorStateSystem::default();
        sys.mount_compass(Compass::new("承诺", "目标"));
        let raw = serde_json::to_string(&sys).unwrap();
        let back: ActorStateSystem = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.compass.author_intent, "承诺");
        assert_eq!(back.compass.current_focus, "目标");

        // 旧存档无 compass 字段 → 反序列化为空罗盘（零回归）。
        let legacy = r#"{"schemaVersion":3,"initialActors":[],"actors":{},"traitPools":[],"archive":[]}"#;
        let legacy_back: ActorStateSystem = serde_json::from_str(legacy).unwrap();
        assert!(legacy_back.compass.is_empty());
    }

    #[test]
    fn apply_updates_upserts_new_character() {
        let mut sys = ActorStateSystem::default();
        let updates = vec![ActorStateUpdate {
            character_id: "villain".into(),
            fields: [("hp".into(), json!(50))].into_iter().collect(),
            add_traits: vec![],
            remove_traits: vec![],
        }];
        let n = sys.apply_updates(&updates);
        assert_eq!(n, 1);
        let entry = sys.actors.get("villain").expect("upserted");
        assert_eq!(entry.fields["hp"].value.as_ref(), Some(&json!(50)));
    }

    #[test]
    fn apply_updates_overwrites_existing_fields() {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "hero".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: [("hp".into(), field(json!(100)))]
                    .into_iter()
                    .collect(),
                traits: vec![],
            },
        );
        let updates = vec![ActorStateUpdate {
            character_id: "hero".into(),
            fields: [("hp".into(), json!(30))].into_iter().collect(),
            add_traits: vec![],
            remove_traits: vec![],
        }];
        let n = sys.apply_updates(&updates);
        assert_eq!(n, 1);
        assert_eq!(sys.actors["hero"].fields["hp"].value.as_ref(), Some(&json!(30)));
    }

    #[test]
    fn apply_updates_add_traits_ignores_duplicate() {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "hero".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: [("hp".into(), field(json!(100)))].into_iter().collect(),
                traits: vec![trait_inst("p1", "tr1", "热血")],
            },
        );
        let updates = vec![ActorStateUpdate {
            character_id: "hero".into(),
            fields: [("hp".into(), json!(10))].into_iter().collect(),
            add_traits: vec![trait_inst("p1", "tr1", "热血"), trait_inst("p2", "tr2", "冷静")],
            remove_traits: vec![],
        }];
        let n = sys.apply_updates(&updates);
        assert_eq!(n, 1);
        let traits = &sys.actors["hero"].traits;
        // 已有 tr1 不重复追加；新增 tr2
        assert_eq!(traits.iter().filter(|t| t.trait_id == "tr1").count(), 1);
        assert!(traits.iter().any(|t| t.trait_id == "tr2"));
        assert_eq!(traits.len(), 2);
    }

    #[test]
    fn apply_updates_remove_traits() {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "hero".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: [("hp".into(), field(json!(100)))].into_iter().collect(),
                traits: vec![trait_inst("p1", "tr1", "热血"), trait_inst("p2", "tr2", "冷静")],
            },
        );
        let updates = vec![ActorStateUpdate {
            character_id: "hero".into(),
            fields: [("hp".into(), json!(10))].into_iter().collect(),
            add_traits: vec![],
            remove_traits: vec!["tr1".into()],
        }];
        let n = sys.apply_updates(&updates);
        assert_eq!(n, 1);
        let traits = &sys.actors["hero"].traits;
        assert!(!traits.iter().any(|t| t.trait_id == "tr1"));
        assert!(traits.iter().any(|t| t.trait_id == "tr2"));
        assert_eq!(traits.len(), 1);
    }

    #[test]
    fn apply_updates_returns_field_count() {
        let mut sys = ActorStateSystem::default();
        let updates = vec![
            ActorStateUpdate {
                character_id: "a".into(),
                fields: [("hp".into(), json!(1)), ("mp".into(), json!(2))]
                    .into_iter()
                    .collect(),
                add_traits: vec![],
                remove_traits: vec![],
            },
            ActorStateUpdate {
                character_id: "a".into(),
                fields: [("hp".into(), json!(9))].into_iter().collect(),
                add_traits: vec![],
                remove_traits: vec![],
            },
            ActorStateUpdate {
                character_id: "b".into(),
                fields: [("sp".into(), json!(3))].into_iter().collect(),
                add_traits: vec![],
                remove_traits: vec![],
            },
        ];
        let n = sys.apply_updates(&updates);
        assert_eq!(n, 4);
        assert_eq!(sys.actors.len(), 2);
    }

    #[test]
    fn apply_updates_skips_empty_character_id() {
        let mut sys = ActorStateSystem::default();
        let updates = vec![ActorStateUpdate {
            character_id: String::new(),
            fields: [("hp".into(), json!(5))].into_iter().collect(),
            add_traits: vec![],
            remove_traits: vec![],
        }];
        let n = sys.apply_updates(&updates);
        assert_eq!(n, 0);
        assert!(sys.actors.is_empty());
    }
}

#[cfg(test)]
mod rule_check_tests {
    use super::*;

    fn req() -> TurnCheckRequest {
        TurnCheckRequest {
            action: "潜行".into(),
            intent: "不被发现接近目标".into(),
            challenge: "走廊有巡逻守卫".into(),
            cost: "被发现后警报响起".into(),
            difficulty: "normal".into(),
            template_id: Some("stealth".into()),
            bonuses: vec![TurnCheckBonus {
                reason: "潜行专长".into(),
                value: 2.0,
            }],
            outcomes: TurnCheckOutcomes {
                critical_success: TurnCheckOutcome {
                    result: "无声无息".into(),
                    state_changes: vec![],
                },
                success: TurnCheckOutcome {
                    result: "成功抵达".into(),
                    state_changes: vec![],
                },
                failure: TurnCheckOutcome {
                    result: "被守卫察觉".into(),
                    state_changes: vec![],
                },
                critical_failure: TurnCheckOutcome {
                    result: "直接暴露".into(),
                    state_changes: vec![],
                },
            },
        }
    }

    #[test]
    fn parse_dice_basic() {
        assert_eq!(parse_dice("1d20"), Some((1, 20)));
        assert_eq!(parse_dice("2d6"), Some((2, 6)));
        assert_eq!(parse_dice("bad"), None);
        assert_eq!(parse_dice(""), None);
    }

    #[test]
    fn difficulty_to_dc_mapping() {
        assert_eq!(difficulty_to_dc("very_easy"), 5.0);
        assert_eq!(difficulty_to_dc("easy"), 8.0);
        assert_eq!(difficulty_to_dc("normal"), 12.0);
        assert_eq!(difficulty_to_dc("hard"), 15.0);
        assert_eq!(difficulty_to_dc("very_hard"), 18.0);
        assert_eq!(difficulty_to_dc("whatever"), 12.0);
    }

    #[test]
    fn roll_check_natural_20_is_critical_success() {
        let r = roll_check(&req(), None, || 20);
        assert_eq!(r.outcome, "critical_success");
        assert_eq!(r.natural, 20);
        assert_eq!(r.result_text, "无声无息");
    }

    #[test]
    fn roll_check_natural_1_is_critical_failure() {
        let r = roll_check(&req(), None, || 1);
        assert_eq!(r.outcome, "critical_failure");
        assert_eq!(r.natural, 1);
        assert_eq!(r.result_text, "直接暴露");
    }

    #[test]
    fn roll_check_hit_and_miss() {
        let mut q = req();
        q.difficulty = "normal".into();
        q.bonuses.clear();
        let hit = roll_check(&q, None, || 12);
        assert_eq!(hit.outcome, "success");
        let miss = roll_check(&q, None, || 11);
        assert_eq!(miss.outcome, "failure");
    }

    #[test]
    fn roll_check_bonuses_add_to_total() {
        let r = roll_check(&req(), None, || 10);
        // dc normal=12，total=10+2=12 → success
        assert_eq!(r.outcome, "success");
        assert_eq!(r.total, 12.0);
        assert_eq!(r.dc, 12.0);
    }

    #[test]
    fn roll_check_template_modifier_raises_dc() {
        let tpl = RuleCheck {
            id: "stealth".into(),
            label: "潜行".into(),
            dice: "1d20".into(),
            modifier: 4.0,
            ..Default::default()
        };
        let r = roll_check(&req(), Some(&tpl), || 12);
        // dc=16，total=14 → failure
        assert_eq!(r.dc, 16.0);
        assert_eq!(r.outcome, "failure");
    }

    #[test]
    fn roll_check_template_state_binding_on_fail() {
        let tpl = RuleCheck {
            id: "stealth".into(),
            label: "潜行".into(),
            dice: "1d20".into(),
            modifier: 0.0,
            state_bindings: vec![RuleStateBinding {
                field: "敌方.压力".into(),
                on_success: None,
                on_fail: Some(json!("+1")),
            }],
            ..Default::default()
        };
        let r = roll_check(&req(), Some(&tpl), || 5);
        assert_eq!(r.outcome, "failure");
        assert!(r.state_changes.iter().any(|c| c.actor_id == "敌方"
            && c.field_id == "压力"
            && c.change == 1.0));
    }

    #[test]
    fn roll_check_result_falls_back_to_template_hint() {
        let tpl = RuleCheck {
            id: "stealth".into(),
            label: "潜行".into(),
            dice: "1d20".into(),
            modifier: 0.0,
            failure_hint: "你弄出了声响，可能引起注意。".into(),
            ..Default::default()
        };
        let mut q = req();
        q.outcomes.failure.result = String::new();
        let r = roll_check(&q, Some(&tpl), || 5);
        assert_eq!(r.outcome, "failure");
        assert_eq!(r.result_text, "你弄出了声响，可能引起注意。");
    }

    #[test]
    fn apply_state_changes_numeric_add_and_ignore_non_numeric() {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "敌方".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: [
                    (
                        "压力".into(),
                        ActorFieldValue {
                            value_type: "number".into(),
                            value: Some(json!(3)),
                            min: Some(0.0),
                            max: Some(100.0),
                            options: vec![],
                            display: Some("压力".into()),
                            update_instruction: None,
                        },
                    ),
                    (
                        "名字".into(),
                        ActorFieldValue {
                            value_type: "string".into(),
                            value: Some(json!("守卫长")),
                            min: None,
                            max: None,
                            options: vec![],
                            display: None,
                            update_instruction: None,
                        },
                    ),
                ]
                .into_iter()
                .collect(),
                traits: vec![],
            },
        );
        let changes = vec![
            TurnStateChange {
                actor_id: "敌方".into(),
                field_id: "压力".into(),
                change: 1.0,
                reason: "检定".into(),
            },
            TurnStateChange {
                actor_id: "敌方".into(),
                field_id: "名字".into(),
                change: 5.0,
                reason: "不应生效".into(),
            },
            TurnStateChange {
                actor_id: "不存在".into(),
                field_id: "压力".into(),
                change: 1.0,
                reason: "忽略".into(),
            },
        ];
        let n = sys.apply_state_changes(&changes);
        assert_eq!(n, 1);
        assert_eq!(sys.actors["敌方"].fields["压力"].value.as_ref(), Some(&json!(4.0)));
    }

    #[test]
    fn apply_state_changes_numeric_string_value() {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "敌方".into(),
            ActorStateEntry {
                template_id: "t1".into(),
                fields: [(
                    "压力".into(),
                    ActorFieldValue {
                        value_type: "string".into(),
                        value: Some(json!("10")),
                        min: None,
                        max: None,
                        options: vec![],
                        display: None,
                        update_instruction: None,
                    },
                )]
                .into_iter()
                .collect(),
                traits: vec![],
            },
        );
        let changes = vec![TurnStateChange {
            actor_id: "敌方".into(),
            field_id: "压力".into(),
            change: -2.0,
            reason: "检定".into(),
        }];
        let n = sys.apply_state_changes(&changes);
        assert_eq!(n, 1);
        assert_eq!(sys.actors["敌方"].fields["压力"].value.as_ref(), Some(&json!(8.0)));
    }
}

#[cfg(test)]
mod director_plan_tests {
    use super::tests::{minimal_pack, minimal_session};
    use super::*;

    #[test]
    fn director_run_policy_g8_defaults() {
        // G8: 缺字段反序列化回退默认值（fail_forward / balanced / audit_only / 5）
        let legacy = r#"{"mode":"interval","intervalTurns":2}"#;
        let rp: DirectorRunPolicy = serde_json::from_str(legacy).unwrap();
        assert_eq!(rp.mode, "interval");
        assert_eq!(rp.interval_turns, 2);
        assert_eq!(rp.failure_policy, "fail_forward");
        assert_eq!(rp.pacing_curve, "");
        assert_eq!(rp.event_frequency, "balanced");
        assert_eq!(rp.rule_visibility_mode, "audit_only");
        assert_eq!(rp.branch_planning_turns, 5);
        // 显式值可覆盖默认
        let full = r#"{"mode":"manual","failurePolicy":"hard_failure","eventFrequency":"frequent","ruleVisibilityMode":"public_roll","branchPlanningTurns":7}"#;
        let rp2: DirectorRunPolicy = serde_json::from_str(full).unwrap();
        assert_eq!(rp2.failure_policy, "hard_failure");
        assert_eq!(rp2.event_frequency, "frequent");
        assert_eq!(rp2.rule_visibility_mode, "public_roll");
        assert_eq!(rp2.branch_planning_turns, 7);
    }

    #[test]
    fn director_module_refs_g11_disabled_default_false() {
        // G11: 旧数据无 *_disabled 字段 → 默认 false（模块启用，零回归）
        let legacy = r#"{"narrativeStyleId":"ns1","eventPackageIds":["pkg-a"]}"#;
        let mr: DirectorModuleRefs = serde_json::from_str(legacy).unwrap();
        assert_eq!(mr.narrative_style_id.as_deref(), Some("ns1"));
        assert!(!mr.narrative_style_disabled);
        assert!(!mr.event_packages_disabled);
        assert!(!mr.rule_system_disabled);
        assert!(!mr.actor_state_disabled);
        assert!(!mr.image_preset_disabled);
        // 显式 disabled=true 且保留原 ID（denova 语义）
        let full = r#"{"narrativeStyleId":"ns1","narrativeStyleDisabled":true,"actorStateDisabled":true}"#;
        let mr2: DirectorModuleRefs = serde_json::from_str(full).unwrap();
        assert_eq!(mr2.narrative_style_id.as_deref(), Some("ns1"));
        assert!(mr2.narrative_style_disabled);
        assert!(mr2.actor_state_disabled);
        assert!(!mr2.event_packages_disabled);
    }

    #[test]
    fn mainline_strength_soft_guidance_compat() {
        // G8: mainline_strength 兼容——soft 旧值与 soft_guidance 都应被前端/注入识别
        let soft: StageDirectorConfig = serde_json::from_str(r#"{"mainlineStrength":"soft"}"#).unwrap();
        assert_eq!(soft.mainline_strength, "soft");
        let sg: StageDirectorConfig = serde_json::from_str(r#"{"mainlineStrength":"soft_guidance"}"#).unwrap();
        assert_eq!(sg.mainline_strength, "soft_guidance");
        // 缺省 → balanced
        let d: StageDirectorConfig = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(d.mainline_strength, "balanced");
    }

    #[test]
    fn director_plan_g1_three_docs_defaults() {
        // G1: 三文档缺省空串（旧数据零回归）
        let legacy = r#"{"goal":"主线推进","pressure":"高","cost":"牺牲","hitsBeats":["beat-1"],"createdTurn":1,"updatedTurn":1}"#;
        let plan: DirectorPlan = serde_json::from_str(legacy).unwrap();
        assert_eq!(plan.goal, "主线推进");
        assert_eq!(plan.plan, "");
        assert_eq!(plan.agent_brief, "");
        assert_eq!(plan.lore_context, "");
        assert_eq!(plan.last_run, None);
        // 显式值可设置
        let full = r#"{"goal":"g","plan":"计划正文","agentBrief":"本回合指令","loreContext":"世界观铺垫"}"#;
        let p2: DirectorPlan = serde_json::from_str(full).unwrap();
        assert_eq!(p2.plan, "计划正文");
        assert_eq!(p2.agent_brief, "本回合指令");
        assert_eq!(p2.lore_context, "世界观铺垫");
    }

    #[test]
    fn director_plan_g2_run_status_serialization() {
        // G2: ready/running/conflict 状态构造 + 序列化
        let r = DirectorPlanRunStatus::ready("推进主线");
        assert_eq!(r.status, "ready");
        assert_eq!(r.summary.as_deref(), Some("推进主线"));
        let json_r = serde_json::to_value(&r).unwrap();
        assert_eq!(json_r["status"], "ready");
        assert!(json_r["updatedAt"].is_string());
        let c = DirectorPlanRunStatus::conflict("LLM 失败");
        assert_eq!(c.status, "conflict");
        assert_eq!(c.error.as_deref(), Some("LLM 失败"));
        // 反序列化旧数据（无 last_run）→ None
        let legacy = r#"{"goal":"g"}"#;
        let plan: DirectorPlan = serde_json::from_str(legacy).unwrap();
        assert_eq!(plan.last_run, None);
        // 完整 plan + last_run round-trip
        let full = r#"{"goal":"g","lastRun":{"status":"ready","summary":"s","updatedAt":"2026-08-12T00:00:00Z"}}"#;
        let p2: DirectorPlan = serde_json::from_str(full).unwrap();
        assert_eq!(p2.last_run.as_ref().map(|x| x.status.as_str()), Some("ready"));
        assert_eq!(p2.last_run.as_ref().and_then(|x| x.summary.as_deref()), Some("s"));
    }

    #[test]
    fn fit_text_to_token_budget_basic() {
        // G4: 预算内原样返回
        let s = "短文本";
        assert_eq!(fit_text_to_token_budget(s, 100, 0.5), s);
        // 超预算：头尾保留 + 中间省略标记
        let long = "甲乙丙丁戊己庚辛壬癸";
        let out = fit_text_to_token_budget(long, 6, 0.5);
        assert!(out.contains("省略 4 字符"), "got: {out}");
        assert!(out.starts_with("甲乙丙"), "头部保留: {out}");
        assert!(out.ends_with("壬癸"), "尾部保留: {out}");
        // 空串 / 预算 0 → 空串
        assert_eq!(fit_text_to_token_budget("", 10, 0.5), "");
        assert_eq!(fit_text_to_token_budget("abc", 0, 0.5), "");
    }

    #[test]
    fn fit_text_to_token_budget_head_ratio_edge() {
        // head_ratio=1.0 → 全头部保留、无尾部
        let long = "abcdefgh";
        let out = fit_text_to_token_budget(long, 5, 1.0);
        assert!(out.starts_with("abcde"), "got: {out}");
        // head_ratio=0.0 → 全尾部保留
        let out2 = fit_text_to_token_budget(long, 5, 0.0);
        assert!(out2.ends_with("fgh"), "got: {out2}");
        // head_ratio 越界 clamp
        let out3 = fit_text_to_token_budget(long, 4, 2.0);
        assert!(out3.contains("省略"), "clamp 后仍裁剪: {out3}");
    }

    #[test]
    fn director_ledger_entry_serialization() {
        // G5: 账本条目 camelCase 序列化 + 缺省反序列化
        let e = DirectorLedgerEntry {
            source: "recent".into(),
            title: "最近剧情".into(),
            body_bytes: 2048,
            limit: 1024,
            included: false,
            note: "超预算，省略 800 字符".into(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["source"], "recent");
        assert_eq!(v["bodyBytes"], 2048);
        assert_eq!(v["included"], false);
        let back: DirectorLedgerEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back.source, "recent");
        assert_eq!(back.note, "超预算，省略 800 字符");
        // 缺省字段 → 默认值（bool 缺省 false = 未登记即未包含）
        let d: DirectorLedgerEntry = serde_json::from_str(r#"{"source":"s"}"#).unwrap();
        assert_eq!(d.body_bytes, 0);
        assert!(!d.included);
    }

    #[test]
    fn retain_guard_events_basic() {
        // G15: 未超窗 → 原样返回
        let ev = vec!["[high][人物] 甲".to_string(), "[med][节拍] 乙".to_string()];
        assert_eq!(retain_guard_events(&ev, 10), ev);
        // 超窗 → 裁剪到 max_recent，保留窗口尾巴
        let many: Vec<String> = (0..10).map(|i| format!("[med][d] 事件{i}")).collect();
        let kept = retain_guard_events(&many, 5);
        assert_eq!(kept.len(), 5);
        assert!(kept[0].contains("事件5"), "保留最近窗口: {kept:?}");
        // max_recent=0 → 空
        assert!(retain_guard_events(&many, 0).is_empty());
    }

    #[test]
    fn retain_guard_events_prefers_high() {
        // G15: 超窗时 med 先被淘汰，high 保底
        let events: Vec<String> = vec![
            "[med][d] 早".into(),
            "[high][人物] 关键".into(),
            "[med][d] 中".into(),
            "[med][d] 晚".into(),
        ];
        let kept = retain_guard_events(&events, 2);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|e| e.starts_with("[high]")), "high 保底: {kept:?}");
        // 全部 high 时从最老淘汰
        let all_high: Vec<String> = (0..6).map(|i| format!("[high][d] 事件{i}")).collect();
        let kept2 = retain_guard_events(&all_high, 3);
        assert_eq!(kept2.len(), 3);
        assert!(kept2[0].contains("事件3"), "最老优先淘汰: {kept2:?}");
    }

    #[test]
    fn director_due_interval_hit() {
        // interval=3：turn 3 命中（turn%3==0 && last != Some(3)）
        assert!(director_due("interval", 3, 3, None));
        assert!(director_due("interval", 3, 6, Some(3)));
        // interval=1：每个已推进回合都命中
        assert!(director_due("interval", 1, 1, None));
        assert!(director_due("interval", 1, 5, Some(4)));
    }

    #[test]
    fn director_due_interval_miss() {
        assert!(!director_due("interval", 3, 1, None));
        assert!(!director_due("interval", 3, 2, None));
        assert!(!director_due("interval", 3, 4, Some(3)));
        // turn=0 不触发
        assert!(!director_due("interval", 3, 0, None));
    }

    #[test]
    fn director_due_interval_no_repeat() {
        // 同一回合刚生成过计划，不重复触发
        assert!(!director_due("interval", 3, 3, Some(3)));
        assert!(!director_due("interval", 1, 1, Some(1)));
    }

    #[test]
    fn director_due_interval_zero_defensive() {
        // interval_turns=0 防御：永不触发
        assert!(!director_due("interval", 0, 3, None));
        assert!(!director_due("interval", 0, 0, None));
    }

    #[test]
    fn opening_plan_due_first_turn_seeded() {
        // 首回合（turn=0）+ opening 已 seed + 无 plan → 触发开局导演规划
        assert!(opening_plan_due(0, false, true));
    }

    #[test]
    fn opening_plan_due_already_has_plan() {
        // 已有 plan → 幂等，不重复生成
        assert!(!opening_plan_due(0, true, true));
        assert!(!opening_plan_due(0, true, false));
    }

    #[test]
    fn opening_plan_due_not_first_turn_or_unseeded() {
        // 非首回合 → 不触发
        assert!(!opening_plan_due(1, false, true));
        assert!(!opening_plan_due(2, false, true));
        // opening 未 seed（空会话/无开场）→ 不触发
        assert!(!opening_plan_due(0, false, false));
    }

    #[test]
    fn director_due_manual_and_ondemand_false() {
        assert!(!director_due("manual", 3, 3, None));
        assert!(!director_due("manual", 1, 1, None));
        assert!(!director_due("on_demand", 3, 3, None));
        assert!(!director_due("on_demand", 1, 6, Some(3)));
    }

    #[test]
    fn director_due_invalid_mode_false() {
        assert!(!director_due("", 3, 3, None));
        assert!(!director_due("every_turn", 3, 3, None));
        assert!(!director_due("  ", 3, 3, None));
    }

    #[test]
    fn director_plan_serde_roundtrip() {
        let plan = DirectorPlan {
            goal: "引出铜铃来信".into(),
            pressure: Some("门外脚步已停".into()),
            cost: Some("若拆信则再无回头路".into()),
            hits_beats: vec!["信的存在不可抹除".into()],
            created_turn: 3,
            updated_turn: 3,
            plan: String::new(),
            agent_brief: String::new(),
            lore_context: String::new(),
            last_run: None,
        };
        let raw = serde_json::to_string(&plan).unwrap();
        let back: DirectorPlan = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.goal, "引出铜铃来信");
        assert_eq!(back.pressure.as_deref(), Some("门外脚步已停"));
        assert_eq!(back.cost.as_deref(), Some("若拆信则再无回头路"));
        assert_eq!(back.hits_beats, vec!["信的存在不可抹除".to_string()]);
        assert_eq!(back.created_turn, 3);
        assert_eq!(back.updated_turn, 3);
        // camelCase JSON 字段名
        assert!(raw.contains("\"hitsBeats\""));
        assert!(raw.contains("\"createdTurn\""));
        assert!(raw.contains("\"updatedTurn\""));
    }

    #[test]
    fn director_plan_missing_fields_default() {
        // 缺省字段（pressure/cost/hits_beats/createdTurn/updatedTurn）走 serde default
        let raw = r#"{"goal":"引出铜铃来信"}"#;
        let plan: DirectorPlan = serde_json::from_str(raw).unwrap();
        assert_eq!(plan.goal, "引出铜铃来信");
        assert!(plan.pressure.is_none());
        assert!(plan.cost.is_none());
        assert!(plan.hits_beats.is_empty());
        assert_eq!(plan.created_turn, 0);
        assert_eq!(plan.updated_turn, 0);
    }

    #[test]
    fn session_defaults_director_plan_none() {
        // 旧 session 无 director_plan/director_pending 字段 → 正常加载（serde default）
        let raw = r#"{
            "sessionId": "tavern-session-old",
            "packId": "pack-1",
            "playable": "P3",
            "playMode": "mainline",
            "contentTier": "standard",
            "turn": 5
        }"#;
        let sess: TavernSession = serde_json::from_str(raw).unwrap();
        assert_eq!(sess.session_id, "tavern-session-old");
        assert!(sess.director_plan.is_none());
        assert!(!sess.director_pending);
        assert!(sess.director_task.is_none());
    }

    // ─── G13/G14 DirectorTaskGroup 串行任务组 单测 ───────────────────────────

    #[test]
    fn task_group_same_key_second_start_returns_false() {
        let group = std::sync::Arc::new(DirectorTaskGroup::new());
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let t = {
            let group = group.clone();
            let gate = gate.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                let started = group.start("session-a", "director_plan_update", || {
                    gate.wait(); // 信号：任务已进入运行表
                    while !release.load(std::sync::atomic::Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                });
                assert!(started);
            })
        };
        gate.wait(); // 等任务 A 真正在跑

        // 同 key 第二个 start → false，且闭包不执行
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran2 = ran.clone();
        let started2 = group.start("session-a", "director_plan_update_2", || {
            ran2.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(!started2);
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(group.is_running("session-a"));
        assert_eq!(
            group.current_task("session-a"),
            Some("director_plan_update".into())
        );

        release.store(true, std::sync::atomic::Ordering::SeqCst);
        t.join().unwrap();

        // 结束后 key 释放，可再次启动
        assert!(!group.is_running("session-a"));
        let started3 = group.start("session-a", "director_plan_update_3", || {});
        assert!(started3);
        assert!(!group.is_running("session-a"));
    }

    #[test]
    fn task_group_panic_caught_does_not_crash_thread() {
        let group = DirectorTaskGroup::new();
        let started = group.start("panic-key", "task-panic", || {
            panic!("deliberate director task panic");
        });
        assert!(started);
        // panic 被捕获后运行表清理，key 可复用（不炸进程）
        assert!(!group.is_running("panic-key"));
        let ok = group.start("panic-key", "task-after", || {});
        assert!(ok);
        assert!(!group.is_running("panic-key"));
    }

    #[test]
    fn task_group_with_task_registers_current_task() {
        let group = DirectorTaskGroup::new();
        let mut observed: Option<String> = None;
        group.start("wk", "initial", || {
            group.with_task("updated");
            observed = group.current_task("wk");
        });
        assert_eq!(observed, Some("updated".into()));
        assert!(!group.is_running("wk"));
    }

    #[test]
    fn task_group_acquire_release_primitives() {
        let group = DirectorTaskGroup::new();
        assert!(group.acquire("a1", "t1"));
        assert!(!group.acquire("a1", "t2"));
        assert_eq!(group.current_task("a1"), Some("t1".into()));
        assert!(group.is_running("a1"));
        group.release("a1");
        assert!(!group.is_running("a1"));
        assert!(group.acquire("a1", "t3"));
        group.release("a1");
    }

    #[test]
    fn task_group_different_keys_do_not_block_each_other() {
        // denova GoKeyed 语义：按 key 串行，不同 key 互不阻塞
        let group = std::sync::Arc::new(DirectorTaskGroup::new());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for i in 0..2 {
            let group = group.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let key = format!("key-{i}");
                let started =
                    group.start(key, format!("task-{i}"), || {
                        barrier.wait(); // 两个任务同时进入 → 不同 key 并行执行
                    });
                assert!(started);
            }));
        }
        barrier.wait();
        for h in handles {
            h.join().unwrap();
        }
        assert!(!group.is_running("key-0"));
        assert!(!group.is_running("key-1"));
    }

    #[test]
    fn session_loads_existing_director_plan() {
        // 落库后的 session 可回读 director_plan
        let raw = r#"{
            "sessionId": "tavern-session-x",
            "packId": "pack-1",
            "playable": "P3",
            "playMode": "mainline",
            "contentTier": "standard",
            "turn": 6,
            "directorPlan": {
                "goal": "推进到无名信",
                "pressure": "脚步逼近",
                "cost": "暴露行踪",
                "hitsBeats": ["信的存在不可抹除"],
                "createdTurn": 3,
                "updatedTurn": 6
            },
            "directorPending": true
        }"#;
        let sess: TavernSession = serde_json::from_str(raw).unwrap();
        let plan = sess.director_plan.expect("plan loaded");
        assert_eq!(plan.goal, "推进到无名信");
        assert_eq!(plan.hits_beats, vec!["信的存在不可抹除".to_string()]);
        assert_eq!(plan.updated_turn, 6);
        assert!(sess.director_pending);
    }

    // ─── S5: 事件卡包 ──────────────────────────────────────────────

    fn event_pack_fixture() -> StoryPack {
        let mut pack = minimal_pack();
        pack.stage_director.modules.event_package_ids = vec!["pkg-a".into(), "pkg-b".into()];
        pack.event_packages = vec![
            EventPackage {
                id: "pkg-a".into(),
                name: "A".into(),
                description: None,
                enabled: true,
                cards: vec![
                    TellerEventCard {
                        id: "a1".into(),
                        title: "A1".into(),
                        prompt: "p1".into(),
                        weight: 1,
                        enabled: true,
                        once_per_session: false,
                        used_in_session: false,
                        type_name: "外门考核".into(),
                        category: "打脸".into(),
                        tags: vec!["门派".into(), "考核".into()],
                        intensity: "medium".into(),
                        cooldown_turns: 2,
                        chapter_range: vec![],
                    },
                    TellerEventCard {
                        id: "a2".into(),
                        title: "A2".into(),
                        prompt: "p2".into(),
                        weight: 9,
                        enabled: true,
                        once_per_session: false,
                        used_in_session: false,
                        type_name: "误入香闺".into(),
                        category: "恋爱".into(),
                        tags: vec!["闺房".into(), "幽会".into()],
                        intensity: "low".into(),
                        cooldown_turns: 3,
                        chapter_range: vec![],
                    },
                ],
            },
            EventPackage {
                id: "pkg-b".into(),
                name: "B".into(),
                description: None,
                enabled: true,
                cards: vec![TellerEventCard {
                    id: "b1".into(),
                    title: "B1".into(),
                    prompt: "p3".into(),
                    weight: 1,
                    enabled: true,
                    once_per_session: false,
                    used_in_session: false,
                    type_name: "秘境争夺".into(),
                    category: "秘境".into(),
                    tags: vec!["秘境".into()],
                    intensity: "high".into(),
                    cooldown_turns: 0,
                    chapter_range: vec![],
                }],
            },
        ];
        pack
    }

    #[test]
    fn pick_event_card_empty_pack_returns_none() {
        let pack = minimal_pack(); // 无 eventPackages
        assert!(pick_event_card(&pack, 42, 1, None, None).is_none());
    }

    #[test]
    fn pick_event_card_disabled_package_excluded() {
        let mut pack = event_pack_fixture();
        pack.event_packages[0].enabled = false;
        let (pkg, card) = pick_event_card(&pack, 7, 1, None, None).expect("pkg-b card");
        assert_eq!(pkg.id, "pkg-b");
        assert_eq!(card.id, "b1");
    }

    #[test]
    fn pick_event_card_module_ids_filter() {
        let mut pack = event_pack_fixture();
        pack.stage_director.modules.event_package_ids = vec!["pkg-a".into()];
        let (pkg, _) = pick_event_card(&pack, 7, 1, None, None).expect("pkg-a card");
        assert_eq!(pkg.id, "pkg-a");
    }

    #[test]
    fn pick_event_card_weighted_prefers_high_weight() {
        let pack = event_pack_fixture();
        let mut a2_hits = 0;
        for seed in 0..200u64 {
            let (_, card) = pick_event_card(&pack, seed, 1, None, None).expect("card");
            if card.id == "a2" {
                a2_hits += 1;
            }
        }
        // 权重 9 vs 1(+1) → a2 应占绝大多数
        assert!(a2_hits > 150, "a2 hits {a2_hits}");
    }

    #[test]
    fn pick_event_card_once_per_session_excludes_used() {
        let mut pack = event_pack_fixture();
        pack.event_packages[0].cards[0].once_per_session = true;
        pack.event_packages[0].cards[0].used_in_session = true;
        let (_, card) = pick_event_card(&pack, 1, 1, None, None).expect("card");
        assert_ne!(card.id, "a1");
    }

    /// 冷却语义：cooldown_turns>0 且距上次抽同卡 < 冷却回合 → 排除该卡。
    /// 夹具 a2 卡 cooldown_turns=3 且 weight=9（超高权重）；last_event 记 a2 在 turn=10 抽出，
    /// next_turn=11 时差值 1 < 3 仍在冷却期 → 无论 seed 都抽不到 a2。
    #[test]
    fn pick_event_card_cooldown_excludes_recent_card() {
        let pack = event_pack_fixture();
        let last = EventLogEntry {
            turn: 10,
            package_id: "pkg-a".into(),
            card_id: "a2".into(),
            title: "A2".into(),
            prompt: "p2".into(),
            created_at: String::new(),
            type_name: "误入香闺".into(),
            category: "恋爱".into(),
            intensity: "low".into(),
        };
        let mut a2_hits = 0;
        for seed in 0..200u64 {
            let (_, card) = pick_event_card(&pack, seed, 11, Some(&last), None).expect("card");
            if card.id == "a2" {
                a2_hits += 1;
            }
        }
        // 冷却期（11-10=1 < 3）内 a2 绝不能出现
        assert_eq!(a2_hits, 0, "a2 should be in cooldown");
        // 兜底：明确断言抽到的是非冷却卡
        let (_, card) = pick_event_card(&pack, 7, 11, Some(&last), None).expect("card");
        assert_ne!(card.id, "a2");
    }

    /// 冷却结束后恢复：next_turn 距上次抽卡 >= cooldown_turns 后同卡可再次抽到。
    /// 夹具 a2 卡 cooldown_turns=3：last_event 记 a2 在 turn=10 抽出，next_turn=13 时
    /// 差值 3 >= 3 冷却已过 → a2 回归高权重候选（占绝大多数）。
    #[test]
    fn pick_event_card_cooldown_recovers_after_elapsed() {
        let pack = event_pack_fixture();
        let last = EventLogEntry {
            turn: 10,
            package_id: "pkg-a".into(),
            card_id: "a2".into(),
            title: "A2".into(),
            prompt: "p2".into(),
            created_at: String::new(),
            type_name: "误入香闺".into(),
            category: "恋爱".into(),
            intensity: "low".into(),
        };
        let mut a2_hits = 0;
        for seed in 0..200u64 {
            let (_, card) = pick_event_card(&pack, seed, 13, Some(&last), None).expect("card");
            if card.id == "a2" {
                a2_hits += 1;
            }
        }
        // 冷却已过（13-10=3 >= 3）→ a2 恢复为高权重候选
        assert!(a2_hits > 150, "a2 hits {a2_hits}");
    }

    /// A2 按章过滤：卡标注 chapter_range 且当前章命中 → 正常选出；
    /// 当前章不命中 → 该卡排除；全部排除 → None（本回合不抽事件卡）。
    #[test]
    fn pick_event_card_chapter_range_filters_by_current_chapter() {
        let mut pack = event_pack_fixture();
        // a1/a2 标注 ch01-ch03，b1 无标注（旧数据兼容）
        pack.event_packages[0].cards[0].chapter_range = vec!["ch01".into(), "ch02".into()];
        pack.event_packages[0].cards[1].chapter_range = vec!["ch01".into(), "ch02".into()];
        // 当前章 ch01 → a1/a2 可抽（b1 无标注也可抽）→ 不会返回 None
        assert!(pick_event_card(&pack, 7, 1, None, Some("ch01")).is_some());
        // 当前章 ch03 → 标注卡 a1/a2 排除，仅剩无标注的 b1 → 抽到 b1
        let (pkg, card) = pick_event_card(&pack, 7, 1, None, Some("ch03")).expect("b1 应存活");
        assert_eq!(pkg.id, "pkg-b");
        assert_eq!(card.id, "b1");
        // 全部卡都标注且都不覆盖当前章 → None（本回合不抽）
        pack.event_packages[1].cards[0].chapter_range = vec!["ch05".into()];
        // ch03: a1/a2 排除（ch01-ch02），b1 排除（ch05）→ 无候选
        assert!(pick_event_card(&pack, 7, 1, None, Some("ch03")).is_none());
        // 当前章未知（None）→ 按旧行为不拦截
        assert!(pick_event_card(&pack, 7, 1, None, None).is_some());
    }

    #[test]
    fn event_package_serde_camel_case_roundtrip() {
        let raw = r#"{"id":"p","name":"包","enabled":true,"cards":[{"id":"c","title":"T","prompt":"P","weight":3,"enabled":true,"oncePerSession":true,"usedInSession":false,"typeName":"外门考核打脸","category":"打脸","tags":["门派","考核"],"intensity":"medium","cooldownTurns":2,"chapterRange":["ch01","ch03"]}]}"#;
        let pkg: EventPackage = serde_json::from_str(raw).unwrap();
        assert_eq!(pkg.cards[0].weight, 3);
        assert!(pkg.cards[0].once_per_session);
        // 新字段 G7：camelCase 映射 + 值落盘
        assert_eq!(pkg.cards[0].type_name, "外门考核打脸");
        assert_eq!(pkg.cards[0].category, "打脸");
        assert_eq!(pkg.cards[0].tags, vec!["门派".to_string(), "考核".to_string()]);
        assert_eq!(pkg.cards[0].intensity, "medium");
        assert_eq!(pkg.cards[0].cooldown_turns, 2);
        // A1：chapterRange camelCase 反序列化
        assert_eq!(pkg.cards[0].chapter_range, vec!["ch01".to_string(), "ch03".to_string()]);
        let back = serde_json::to_string(&pkg).unwrap();
        assert!(back.contains("\"oncePerSession\""));
        assert!(back.contains("\"usedInSession\""));
        // 新字段序列化回 camelCase
        assert!(back.contains("\"typeName\":\"外门考核打脸\""));
        assert!(back.contains("\"chapterRange\":[\"ch01\",\"ch03\"]"));
        assert!(back.contains("\"cooldownTurns\":2"));
    }

    #[test]
    fn event_package_missing_fields_default() {
        let raw = r#"{"id":"p"}"#;
        let pkg: EventPackage = serde_json::from_str(raw).unwrap();
        assert!(!pkg.enabled);
        assert!(pkg.cards.is_empty());
    }

    #[test]
    fn session_last_event_missing_defaults_none() {
        let raw = r#"{"sessionId":"tavern-session-le","packId":"p","playable":"P3","playMode":"mainline","contentTier":"standard","turn":1}"#;
        let sess: TavernSession = serde_json::from_str(raw).unwrap();
        assert!(sess.last_event.is_none());
    }

    #[test]
    fn session_last_event_roundtrip() {
        let mut s = minimal_session();
        s.last_event = Some(EventLogEntry {
            turn: 2,
            package_id: "pkg-a".into(),
            card_id: "a2".into(),
            title: "A2".into(),
            prompt: "p2".into(),
            created_at: "2026-08-05T00:00:00Z".into(),
            type_name: "误入香闺".into(),
            category: "恋爱".into(),
            intensity: "low".into(),
        });
        let raw = serde_json::to_string(&s).unwrap();
        let back: TavernSession = serde_json::from_str(&raw).unwrap();
        let ev = back.last_event.expect("last_event");
        assert_eq!(ev.card_id, "a2");
        assert_eq!(ev.turn, 2);
    }

    // ─── S6: Actor 归档 ────────────────────────────────────────────

    fn actor_system_fixture() -> ActorStateSystem {
        let mut sys = ActorStateSystem::default();
        sys.actors.insert(
            "cc-linwan".into(),
            ActorStateEntry {
                template_id: "tpl-1".into(),
                fields: [(
                    "气力".to_string(),
                    ActorFieldValue {
                        value_type: "number".into(),
                        value: Some(json!(72)),
                        min: None,
                        max: None,
                        options: vec![],
                        display: None,
                        update_instruction: None,
                    },
                )]
                .into_iter()
                .collect(),
                traits: vec![],
            },
        );
        sys
    }

    #[test]
    fn archive_actor_creates_snapshot_with_reason() {
        let mut sys = actor_system_fixture();
        let snap = sys
            .archive_actor("cc-linwan", "manual", "2026-08-05T00:00:00Z")
            .expect("snap");
        assert_eq!(snap.reason, "manual");
        assert_eq!(snap.character_id, "cc-linwan");
        assert_eq!(sys.archive.len(), 1);
    }

    #[test]
    fn archive_actor_missing_character_returns_none() {
        let mut sys = actor_system_fixture();
        assert!(
            sys.archive_actor("cc-none", "manual", "2026-08-05T00:00:00Z")
                .is_none()
        );
        assert!(sys.archive.is_empty());
    }

    #[test]
    fn restore_actor_restores_latest_snapshot() {
        let mut sys = actor_system_fixture();
        sys.archive_actor("cc-linwan", "story", "2026-08-05T00:00:00Z");
        // 修改状态后恢复
        sys.actors
            .get_mut("cc-linwan")
            .unwrap()
            .fields
            .get_mut("气力")
            .unwrap()
            .value = Some(json!(30));
        assert!(sys.restore_actor("cc-linwan"));
        let restored = sys.actors.get("cc-linwan").unwrap();
        let v = restored
            .fields
            .get("气力")
            .and_then(|f| f.value.as_ref())
            .unwrap();
        assert_eq!(v, &json!(72));
    }

    #[test]
    fn restore_actor_without_snapshot_returns_false() {
        let mut sys = actor_system_fixture();
        assert!(!sys.restore_actor("cc-shentang"));
    }

    #[test]
    fn restore_actor_picks_most_recent_snapshot() {
        let mut sys = actor_system_fixture();
        sys.archive_actor("cc-linwan", "auto", "2026-08-05T00:00:00Z");
        sys.actors
            .get_mut("cc-linwan")
            .unwrap()
            .fields
            .get_mut("气力")
            .unwrap()
            .value = Some(json!(50));
        sys.archive_actor("cc-linwan", "manual", "2026-08-05T01:00:00Z");
        sys.actors
            .get_mut("cc-linwan")
            .unwrap()
            .fields
            .get_mut("气力")
            .unwrap()
            .value = Some(json!(10));
        assert!(sys.restore_actor("cc-linwan"));
        let v = sys
            .actors
            .get("cc-linwan")
            .unwrap()
            .fields
            .get("气力")
            .and_then(|f| f.value.as_ref())
            .unwrap();
        assert_eq!(v, &json!(50));
    }

    #[test]
    fn actor_archive_reason_missing_defaults_empty() {
        let raw = r#"{"characterId":"cc-linwan","createdAt":"2026-08-05T00:00:00Z","state":{}}"#;
        let snap: ActorArchiveSnapshot = serde_json::from_str(raw).unwrap();
        assert!(snap.reason.is_empty());
        let back = serde_json::to_string(&snap).unwrap();
        assert!(back.contains("\"reason\""));
    }

    // 兜底写盘：write_atomic 必须剔除 U+FFFD 替换符（上游 LLM 偶发把单字损坏成 2-3 个 FFFD）。
    #[test]
    fn write_atomic_strips_fffd() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kaleido-clean-test-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.json");
        write_atomic(&path, "手段\u{FFFD}\u{FFFD}诚意 对弗\u{FFFD}\u{FFFD}\u{FFFD}德").unwrap();
        let out = fs::read_to_string(&path).unwrap();
        assert!(!out.contains('\u{FFFD}'), "落盘后不应残留 U+FFFD: {:?}", out);
        assert!(out.contains("手段诚意"), "fffd 应被剔除: {:?}", out);
        fs::remove_dir_all(&dir).unwrap();
    }
}

// ─── G10: 回合提交幂等守卫测试 ───────────────────────────────────────────────

#[cfg(test)]
mod turn_submit_guard_tests {
    use super::*;

    fn msg(role: &str, content: &str) -> TavernMessage {
        TavernMessage {
            id: format!("m-{role}-{content}"),
            role: role.into(),
            content: content.into(),
            created_at: String::new(),
            options: vec![],
            engine_tag: None,
            program: None,
            reasoning: None,
            swipes: vec![],
            swipe_index: 0,
            tokens: 0,
        }
    }

    fn session_with(msgs: &[(&str, &str)]) -> TavernSession {
        TavernSession {
            session_id: "t".into(),
            pack_id: "p".into(),
            pack_missing: false,
            owner: None,
            quality: Quality::Lite,
            playable: Playable::P1,
            play_mode: PlayMode::Mainline,
            content_tier: ContentTier::Standard,
            user_tier_request: ContentTier::Standard,
            entry: EntryConfig::default(),
            chapter_cursor: None,
            node_id: None,
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
            speaker_rotation: false,
            player: PlayerState::default(),
            memory_l1: MemoryL1::default(),
            memory_l2: MemoryL2::default(),
            memory_l3: MemoryL3::default(),
            memory_l4: MemoryL4::default(),
            guard_events: vec![],
            messages: msgs.iter().map(|(r, c)| msg(r, c)).collect(),
            active_run_id: None,
            adult_confirmed: true,
            title: "t".into(),
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
            last_event: None,
            last_check_results: vec![],
        check_history: vec![],
            checkpoints: vec![],
            epoch: 0,
            epoch_last_turn: None,
            epoch_last_chars: None,
            turn_cost_ledger: TurnCostLedger::default(),
            last_turn_diagnostic: None,
            xiami_skim_issues: Vec::new(),
            xiami_skim_sample: String::new(),
            chapter_diaries: Vec::new(),
            turn_progress: None,
            diary_config: None,
            pockets: Default::default(),
            pockets_enabled: true,
            needs: Default::default(),
            growth: Default::default(),
            world_climate: Default::default(),
            chaos: Default::default(),
            milestones: Default::default(),
            objectives: Default::default(),
            ambitions: Default::default(),
            dream: Default::default(),
            episodes: Default::default(),
            journal: Default::default(),
            relationships: Default::default(),
            pending_swipes: Default::default(),
            promises: Default::default(),
            preferences: Default::default(),
            presence: Default::default(),
            timed_world_info: Default::default(),
            event_extract: true,
            director_task: None,
            world: WorldState::default(),
            game_clock: Default::default(),
        }
    }

    /// 三态之一：同回合同内容已提交（末条 user 消息 hash 相同且已有 assistant 回应）→ true。
    #[test]
    fn same_content_duplicate_rejected() {
        let s = session_with(&[("user", "你好"), ("assistant", "你好呀")]);
        assert!(turn_submit_guard(&s, &text_hash("你好")));
    }

    /// 三态之二：不同内容 → false。
    #[test]
    fn different_content_allowed() {
        let s = session_with(&[("user", "你好"), ("assistant", "你好呀")]);
        assert!(!turn_submit_guard(&s, &text_hash("再见")));
        // 空会话首条提交也不判重复。
        let empty = session_with(&[]);
        assert!(!turn_submit_guard(&empty, &text_hash("开始")));
    }

    /// 三态之三：新回合 → false。
    /// 上一回合已完成（末条为 assistant）→ 新回合可提交；
    /// 新回合首个提交（末条 user 消息尚无 assistant 回应，可复用重试路径）→ 不判重复。
    #[test]
    fn new_turn_allowed() {
        let completed = session_with(&[("user", "你好"), ("assistant", "你好呀")]);
        assert!(!turn_submit_guard(&completed, &text_hash("继续")));

        let pending = session_with(&[("user", "你好"), ("assistant", "你好呀"), ("user", "继续")]);
        assert!(!turn_submit_guard(&pending, &text_hash("继续")));

        // LLM 空响应后重试同一输入：末条 user 消息无 assistant 回应 → 放行复用重试。
        let failed = session_with(&[("user", "你好")]);
        assert!(!turn_submit_guard(&failed, &text_hash("你好")));
    }
}