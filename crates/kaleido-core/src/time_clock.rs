//! 游戏时钟 + 天气权威状态系统（kaleido time/weather constraint）
//!
//! 背景：剧情里时间/天气原本由模型自由发挥、随便跳（同一回合内午后→深夜、
//! 晴→暴雨直跳）。本模块提供**权威时钟**：挂在 `TavernSession` 上持久化，
//! 回合推进时按规则顺移时段、天气按邻接表渐进变化，生成 prompt 时注入
//! 「当前时间/天气」硬约束，守卫校验正文与权威时钟一致。
//!
//! 规则：
//! - 时段是**有序循环**（8 段，跨日 day+1），每回合默认推进 1 段；显式跳过
//!   用 [`GameClock::jump`]（如「次日清晨」「三天后」），仍受正序约束。
//! - 天气是**邻接转移**（无向图），禁止跳变（晴→暴雨直跳被阻止），每回合
//!   默认小概率渐进 1 级或维持；显式 [`GameClock::set_weather`] 走邻接校验。
//! - 兼容：旧 session 无 game_clock → [`GameClock::default`] 播种（清晨/晴）。

use serde::{Deserialize, Serialize};

/// 一天内的有序时段（8 段，循环）。
pub const TIME_SLOTS: [&str; 8] = [
    "清晨", "上午", "正午", "午后", "傍晚", "夜晚", "深夜", "凌晨",
];

/// 季节（用于天气加权与温度推导）。
pub const SEASONS: [&str; 4] = ["春", "夏", "秋", "冬"];

/// 季节按「每季天数」轮转（剧情虚构时间，默认 60 天一季，循环）。
pub const DAYS_PER_SEASON: u32 = 60;

/// 各季的天气加权表（移植自 Weather-Calendar-Assistant，类型须落在
/// [`WEATHERS`] 权威集合内）。用于「默认渐进 / 导演建议」时按季节加权选邻居，
/// 使天气不仅邻接合理，还符合季节（夏无雪、冬多晴冷）。
struct SeasonWeather {
    weather: &'static str,
    /// 出现权重。
    weight: u32,
    /// 温度区间 [min, max]（℃）。
    temp_range: (i32, i32),
}

/// 每季天气加权表（key 季节 → 候选天气+权重+温度区间）。
const SEASON_WEATHER: [(&str, &[SeasonWeather]); 4] = [
    (
        "春",
        &[
            SeasonWeather { weather: "晴", weight: 25, temp_range: (15, 25) },
            SeasonWeather { weather: "多云", weight: 25, temp_range: (12, 22) },
            SeasonWeather { weather: "阴", weight: 10, temp_range: (10, 18) },
            SeasonWeather { weather: "小雨", weight: 15, temp_range: (10, 18) },
            SeasonWeather { weather: "大雨", weight: 8, temp_range: (8, 16) },
            SeasonWeather { weather: "大风", weight: 8, temp_range: (10, 20) },
            SeasonWeather { weather: "雾", weight: 5, temp_range: (8, 15) },
            SeasonWeather { weather: "雨雪", weight: 2, temp_range: (4, 10) },
        ],
    ),
    (
        "夏",
        &[
            SeasonWeather { weather: "晴", weight: 35, temp_range: (28, 38) },
            SeasonWeather { weather: "多云", weight: 15, temp_range: (26, 32) },
            SeasonWeather { weather: "阴", weight: 8, temp_range: (24, 30) },
            SeasonWeather { weather: "小雨", weight: 12, temp_range: (24, 30) },
            SeasonWeather { weather: "大雨", weight: 10, temp_range: (22, 28) },
            SeasonWeather { weather: "暴雨", weight: 8, temp_range: (22, 28) },
            SeasonWeather { weather: "大风", weight: 6, temp_range: (24, 32) },
            SeasonWeather { weather: "雾", weight: 3, temp_range: (24, 30) },
            SeasonWeather { weather: "雪", weight: 1, temp_range: (18, 24) }, // 异常寒潮
        ],
    ),
    (
        "秋",
        &[
            SeasonWeather { weather: "晴", weight: 30, temp_range: (12, 22) },
            SeasonWeather { weather: "多云", weight: 20, temp_range: (10, 20) },
            SeasonWeather { weather: "阴", weight: 10, temp_range: (8, 16) },
            SeasonWeather { weather: "小雨", weight: 12, temp_range: (8, 16) },
            SeasonWeather { weather: "大雨", weight: 5, temp_range: (8, 14) },
            SeasonWeather { weather: "大风", weight: 13, temp_range: (8, 18) },
            SeasonWeather { weather: "雾", weight: 8, temp_range: (5, 14) },
            SeasonWeather { weather: "雨雪", weight: 2, temp_range: (2, 8) },
        ],
    ),
    (
        "冬",
        &[
            SeasonWeather { weather: "晴", weight: 20, temp_range: (-5, 5) },
            SeasonWeather { weather: "多云", weight: 20, temp_range: (-3, 5) },
            SeasonWeather { weather: "阴", weight: 15, temp_range: (-5, 3) },
            SeasonWeather { weather: "雨雪", weight: 8, temp_range: (-4, 2) },
            SeasonWeather { weather: "雪", weight: 17, temp_range: (-8, 0) },
            SeasonWeather { weather: "大雪", weight: 7, temp_range: (-12, -3) },
            SeasonWeather { weather: "雾", weight: 5, temp_range: (-3, 3) },
            SeasonWeather { weather: "大风", weight: 8, temp_range: (-10, 0) },
        ],
    ),
];

/// 权威天气集合（邻接转移见 [`weather_neighbors`]）。
pub const WEATHERS: [&str; 11] = [
    "晴", "多云", "阴", "小雨", "大雨", "暴雨", "雨雪", "雪", "大雪", "雾", "大风",
];

/// 天气邻接转移表（无向图，key 的邻居在 value 里）。禁止 key 直接跳到非邻居。
fn weather_neighbors(w: &str) -> &'static [&'static str] {
    match w {
        "晴" => &["多云", "雾"],
        "多云" => &["晴", "阴", "大风"],
        "阴" => &["多云", "小雨", "大风", "雾"],
        "小雨" => &["阴", "大雨", "雨雪", "雾"],
        "大雨" => &["小雨", "暴雨", "雨雪"],
        "暴雨" => &["大雨"],
        "雨雪" => &["小雨", "大雨", "雪"],
        "雪" => &["雨雪", "大雪", "阴"],
        "大雪" => &["雪"],
        "雾" => &["晴", "阴", "小雨"],
        "大风" => &["多云", "阴"],
        _ => &[],
    }
}

/// 将自由文本规范化到权威时段（用于 `/time` 指令与正文标签解析）。
/// 返回 None 表示无法识别（调用方应报「无法识别的时段」）。
pub fn normalize_time_slot(input: &str) -> Option<&'static str> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // 先精确匹配权威时段。
    if let Some(slot) = TIME_SLOTS.iter().find(|t| **t == s) {
        return Some(slot);
    }
    // 常见别名/自然语言。
    if s.contains("清晨") || s.contains("黎明") || s.contains("破晓") || s.contains("天刚亮")
        || s.contains("拂晓") {
        return Some("清晨");
    }
    if s.contains("早晨") || s.contains("早上") || s.contains("上午") {
        return Some("上午");
    }
    if s.contains("正午") || s.contains("中午") || s.contains("午时") {
        return Some("正午");
    }
    if s.contains("午后") || s.contains("下午") {
        return Some("午后");
    }
    if s.contains("傍晚") || s.contains("黄昏") || s.contains("日落") || s.contains("夕阳") {
        return Some("傍晚");
    }
    if s.contains("夜晚") || s.contains("夜里") || s.contains("晚上") || s.contains("入夜") {
        return Some("夜晚");
    }
    if s.contains("深夜") || s.contains("半夜") || s.contains("午夜") || s.contains("子夜") {
        return Some("深夜");
    }
    if s.contains("凌晨") || s.contains("后半夜") {
        return Some("凌晨");
    }
    None
}

/// 解析跳转指令里的「第 N 天 / N 天后 / 次日」等数量词。
/// 返回天数偏移（0 = 当天）。失败返回 None。
fn parse_day_offset(input: &str) -> Option<u32> {
    if input.contains("次日") || input.contains("第二天") || input.contains("隔天") {
        return Some(1);
    }
    if input.contains("第三天") {
        return Some(2);
    }
    if input.contains("三天后") || input.contains("三日") {
        return Some(3);
    }
    // 数字提取：如「第3天」「5天后」
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok()
}

/// 权威时钟 + 天气。挂在 session 上持久化。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameClock {
    /// 第几天（从 1 开始）。
    #[serde(default = "default_day")]
    pub day: u32,
    /// 当前时段（TIME_SLOTS 之一）。
    #[serde(default = "default_slot")]
    pub time_of_day: String,
    /// 当前天气（WEATHERS 之一）。
    #[serde(default = "default_weather")]
    pub weather: String,
    /// 最近一次推进发生在第几回合（0 = 尚未推进）。
    #[serde(default)]
    pub advanced_turn: u32,
    /// 已发生的时间跳跃次数（诊断用）。
    #[serde(default)]
    pub jumps: u32,
    /// 当前季节（SEASONS 之一）。由 day 按 [`DAYS_PER_SEASON`] 轮转推导，
    /// 也可用 [`GameClock::set_season`] 显式覆盖（剧情设定强制季节）。
    #[serde(default = "default_season")]
    pub season: String,
    /// 当前气温（℃）。由季节+天气推导（季节加权表温度区间），供 prompt 注入。
    #[serde(default)]
    pub temp: i32,
    /// 最近一次 LLM 剧情时间评估所在回合（0 = 尚未评估）。用于低频评估频率控制，
    /// 避免每回合都调 LLM（成本）；配合 [`GameClock::llm_eval_due`] 使用。
    #[serde(default)]
    pub last_llm_eval_turn: u32,
}

fn default_day() -> u32 { 1 }
fn default_slot() -> String { "清晨".into() }
fn default_weather() -> String { "晴".into() }
/// 默认季节为空串（非 SEASONS 合法值）→ [`GameClock::season`] 走 day 轮转推导。
fn default_season() -> String { String::new() }

impl Default for GameClock {
    fn default() -> Self {
        Self {
            day: 1,
            time_of_day: "清晨".into(),
            weather: "晴".into(),
            advanced_turn: 0,
            jumps: 0,
            season: String::new(),
            temp: 20,
            last_llm_eval_turn: 0,
        }
    }
}

/// [0,1) 均匀随机（进程内简单 LCG，无需引入 rand 依赖）。
fn rand01() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);
    let mut x = STATE.fetch_add(0x9E3779B97F4A7C15, Ordering::Relaxed);
    // xorshift64*
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let r = x.wrapping_mul(0x2545F4914F6CDD1D);
    (r >> 11) as f64 / (1u64 << 53) as f64
}

impl GameClock {
    /// 当前时段在 TIME_SLOTS 中的下标。
    fn slot_index(&self) -> usize {
        TIME_SLOTS.iter().position(|t| *t == self.time_of_day.as_str()).unwrap_or(0)
    }

    /// 每回合默认推进：顺移 1 个时段；从「凌晨」顺移回「清晨」时 day+1。
    /// 返回是否跨日。
    pub fn advance_turn(&mut self, turn: u32) -> bool {
        let idx = self.slot_index();
        let next = (idx + 1) % TIME_SLOTS.len();
        let crossed = next == 0; // 回到清晨 → 跨日
        self.time_of_day = TIME_SLOTS[next].to_string();
        if crossed {
            self.day += 1;
        }
        self.advanced_turn = turn;
        crossed
    }

    /// 显式时间跳转（`/time 次日清晨`、`/time 三天后`、`/time 午后`）。
    /// 规则：
    /// - 必须在**正序**（不早于当前时段）；若目标时段落后于当前，视为次日（加 day）。
    /// - 支持天偏移（次日/第N天/N天后）。
    /// 返回 (跨日数, 说明)。Err = 无法解析或非法。
    pub fn jump(&mut self, input: &str, turn: u32) -> Result<(u32, String), String> {
        let day_off = parse_day_offset(input).unwrap_or(0);
        let Some(slot) = normalize_time_slot(input) else {
            return Err(format!("无法识别的时段/跳转：{input:?}（可选：{}/次日/N天后）", TIME_SLOTS.join("/")));
        };
        let target_idx = TIME_SLOTS.iter().position(|t| *t == slot).unwrap();
        let cur_idx = self.slot_index();
        // 正序校验：若目标时段 <= 当前时段且非同日，属于「次日」范畴（向后）。
        let mut days = day_off;
        // 正序顺延：仅当未显式指定天偏移（day_off==0）时，目标时段早于/等于当前
        // 时段才顺延到次日同段。显式「次日/N天后」已含天偏移，不再叠加。
        if day_off == 0 && (target_idx < cur_idx || (target_idx == cur_idx)) {
            days += 1;
        }
        self.day += days;
        self.time_of_day = slot.to_string();
        self.advanced_turn = turn;
        self.jumps += 1;
        Ok((days, format!("第 {} 天{}", self.day, slot)))
    }

    /// 显式改天气（`/weather 大雨`）。必须走邻接转移（渐进），禁止跳变。
    /// Err = 非权威天气或与当前不邻接。
    pub fn set_weather(&mut self, input: &str) -> Result<String, String> {
        let s = input.trim().trim_matches('"');
        if !WEATHERS.contains(&s) {
            return Err(format!("无法识别的天气：{s:?}（可选：{}）", WEATHERS.join("/")));
        }
        if s == self.weather {
            return Ok(self.weather.clone());
        }
        let neighbors = weather_neighbors(&self.weather);
        if !neighbors.contains(&s) {
            return Err(format!(
                "天气不能从「{}」直接跳到「{}」（需渐进：{}）",
                self.weather, s, neighbors.join("→")
            ));
        }
        self.weather = s.to_string();
        Ok(self.weather.clone())
    }

    /// 用户显式指令改天气（`/weather 暴雨`）。**用户指令第一原则**：不校验邻接，
    /// 直接设置（允许跳变），仅校验权威天气名。Err = 非权威天气。
    pub fn force_weather(&mut self, input: &str) -> Result<String, String> {
        let s = input.trim().trim_matches('"');
        if !WEATHERS.contains(&s) {
            return Err(format!("无法识别的天气：{s:?}（可选：{}）", WEATHERS.join("/")));
        }
        if s != self.weather {
            self.weather = s.to_string();
            self.temp = self.derive_temp(&s);
        }
        Ok(self.weather.clone())
    }

    /// 生成器/导演建议：渐进变化。若建议与当前邻接则采纳，否则忽略（保守）。
    /// 返回是否变化。
    pub fn suggest_weather(&mut self, suggested: &str) -> bool {
        if !WEATHERS.contains(&suggested) {
            return false;
        }
        if self.set_weather(suggested).is_ok() {
            self.temp = self.derive_temp(&suggested);
            return true;
        }
        false
    }

    /// 当前季节：若显式设置过（season 非默认推导值且合法）用之；否则按 day 轮转。
    /// 简化：season 字段若为 SEASONS 之一则直接返回（含显式覆盖），
    /// 否则按 [`DAYS_PER_SEASON`] 从 day 推导。
    pub fn season(&self) -> &str {
        if SEASONS.contains(&self.season.as_str()) {
            &self.season
        } else {
            SEASONS[((self.day.saturating_sub(1)) / DAYS_PER_SEASON % 4) as usize]
        }
    }

    /// 显式强制季节（剧情设定用）。Err = 非合法季节。
    pub fn set_season(&mut self, input: &str) -> Result<String, String> {
        let s = input.trim();
        let resolved = match s {
            "春" | "春天" | "春季" => "春",
            "夏" | "夏天" | "夏季" => "夏",
            "秋" | "秋天" | "秋季" => "秋",
            "冬" | "冬天" | "冬季" => "冬",
            _ => return Err(format!("无法识别的季节：{s:?}（可选：春/夏/秋/冬）")),
        };
        self.season = resolved.to_string();
        self.temp = self.derive_temp(&self.weather);
        Ok(resolved.to_string())
    }

    /// 由季节+天气推导当前气温：取季节加权表中该天气的温度区间中值。
    fn derive_temp(&self, weather: &str) -> i32 {
        let (_, list) = SEASON_WEATHER
            .iter()
            .find(|(s, _)| *s == self.season())
            .copied()
            .unwrap_or((SEASONS[0], &SEASON_WEATHER[0].1));
        if let Some(sw) = list.iter().find(|sw| sw.weather == weather) {
            let (lo, hi) = sw.temp_range;
            return (lo + hi) / 2;
        }
        // 表中无该天气 → 温和兜底
        18
    }

    /// 按季节加权 + 邻接约束，从当前天气的邻居里挑一个「最符合季节」的渐进候选。
    /// 返回 (候选天气, 权重)。用于导演建议缺省时自动渐进。
    pub fn weighted_neighbor(&self) -> Option<(&'static str, u32)> {
        let (_, list) = SEASON_WEATHER
            .iter()
            .find(|(s, _)| *s == self.season())
            .copied()
            .unwrap_or((SEASONS[0], &SEASON_WEATHER[0].1));
        let neighbors = weather_neighbors(&self.weather);
        // 在邻居 ∩ 季节表 中取最高权重者
        list.iter()
            .filter(|sw| neighbors.contains(&sw.weather))
            .max_by_key(|sw| sw.weight)
            .map(|sw| (sw.weather, sw.weight))
    }

    /// 每回合默认渐进天气：在邻接候选里按季节权重加权随机，维持或渐进 1 级。
    /// 返回是否变化。
    pub fn auto_advance_weather(&mut self) -> bool {
        // 60% 维持当前，40% 渐进到季节加权最合适邻居
        if rand01() < 0.6 {
            return false;
        }
        if let Some((cand, _)) = self.weighted_neighbor() {
            if cand != self.weather {
                self.weather = cand.to_string();
                self.temp = self.derive_temp(cand);
                return true;
            }
        }
        false
    }

    /// 生成 prompt 用的硬状态行（WORLD 结构化：时间｜天气｜季节｜气温）。
    pub fn state_line(&self) -> String {
        format!(
            "第{}天｜{}｜{}｜{}季｜{}℃",
            self.day,
            self.time_of_day,
            self.weather,
            self.season(),
            self.temp
        )
    }

    /// [1B 2026-08-18] 从 pack 开场信号文本推导初始时间/天气。
    /// 信号源：角色卡 openingScene、首个 node summary/楔子正文（如「夏末傍晚的家中客厅，天雨欲来」）。
    /// 只写入权威集合内的合法值（TIME_SLOTS/SEASONS/WEATHERS）；无信号字段保持默认
    /// （清晨/晴/season 空 → day 轮转推导），避免默认值与原著设定冲突
    /// （宿醉「夏末雨夜」曾被压成「清晨晴春」，见 docs/宿醉时间天气原著冲突-20260817.md）。
    /// 匹配策略：长词优先（「暴雨」先于「雨」），首现即取；季节词含「末/初/深」等修饰也识别。
    pub fn derive_from_text(text: &str) -> Self {
        let mut c = Self::default();
        let t = text;

        // 季节：首现「春/夏/秋/冬」（带修饰词也命中：夏末/深秋/初冬 → 夏/秋/冬）
        for s in SEASONS {
            if t.contains(s) {
                c.season = s.to_string();
                break;
            }
        }

        // 时段：TIME_SLOTS 顺序首现即取（「凌晨」排在最后自然兜底当夜）
        for slot in TIME_SLOTS {
            if t.contains(slot) {
                c.time_of_day = slot.to_string();
                break;
            }
        }

        // 天气：长词优先（暴雨/大雨/小雨/大雪/雨雪 皆含「雨」或「雪」，必须长词先匹配）
        const LONG_WEATHERS: [&str; 6] = ["暴雨", "大雨", "小雪", "大雪", "雨雪", "小雨"];
        let mut weather_found = None;
        for w in LONG_WEATHERS {
            if t.contains(w) {
                weather_found = Some(w);
                break;
            }
        }
        if weather_found.is_none() {
            // 单字兜底：雨的变体映射到「小雨」（权威集合无单字「雨」），雪→雪，其余直取
            if t.contains("雨") {
                weather_found = Some("小雨");
            } else if t.contains("雪") {
                weather_found = Some("雪");
            } else {
                for w in ["晴", "阴", "多云", "雾", "大风"] {
                    if t.contains(w) {
                        weather_found = Some(w);
                        break;
                    }
                }
            }
        }
        if let Some(w) = weather_found {
            // 只写权威集合内的值
            if WEATHERS.contains(&w) {
                c.weather = w.to_string();
            }
        }

        // 季节/天气落定后重算气温（避免默认 20℃ 与推导值不匹配）
        if !c.season.is_empty() {
            // season() 对空串走日轮转 —— 这里显式 set 的季节已是合法值，直接按现字段推
            let list = SEASON_WEATHER
                .iter()
                .find(|(s, _)| *s == c.season)
                .map(|(_, l)| l)
                .unwrap_or(&SEASON_WEATHER[0].1);
            if let Some(sw) = list.iter().find(|sw| sw.weather == c.weather) {
                c.temp = (sw.temp_range.0 + sw.temp_range.1) / 2;
            }
        }
        c
    }

    /// LLM 剧情时间评估是否到期：距上次评估 ≥ `interval` 回合才允许评估（低频，
    /// 省成本）。从未评估过（last_llm_eval_turn=0）时首回合(turn≥1)即评估一次，
    /// 后续按 interval 间隔。interval=0 恒评估。返回 true = 应当做一次剧情评估。
    pub fn llm_eval_due(&self, turn: u32, interval: u32) -> bool {
        if interval == 0 {
            return true;
        }
        if self.last_llm_eval_turn == 0 {
            return turn >= 1;
        }
        turn.saturating_sub(self.last_llm_eval_turn) >= interval
    }

    /// 从玩家输入/正文文本中解析时间推进信号（剧情关键词驱动）。
    /// 识别顺序（与 [`GameClock::jump`] 同一套解析）：
    /// 1. 显式「[时间推进: ...]」标注（模型/剧情标记）
    /// 2. 自然语言时间词（次日起床/过了三天/睡到天亮 等）
    /// 返回 Some(跳转指令文本) 供 [`GameClock::jump`] 执行；None = 无推进信号。
    /// 保守：只识别明确的时间推进表达，避免把闲聊里的「明天」「晚上」误当推进。
    pub fn extract_advance_signal(text: &str) -> Option<String> {
        // 1) 显式标注 [时间推进: ...]
        if let Some(idx) = text.find("时间推进:") {
            let tail = &text[idx + "时间推进:".len()..];
            let end = tail.find([']', '」', '。', '\n']).unwrap_or(tail.len());
            let req = tail[..end].trim();
            if !req.is_empty() && (normalize_time_slot(req).is_some() || parse_day_offset(req).is_some()) {
                return Some(req.to_string());
            }
        }
        // 2) 自然语言推进表达（限定场景，防误触发）：次日/睡到天亮/过了一夜 → 次日清晨
        const PH: [&str; 6] = ["次日", "第二天", "睡一觉", "睡到天亮", "过了一夜", "隔天"];
        if PH.iter().any(|p| text.contains(p)) {
            return Some("次日清晨".into());
        }
        // 3) 「N天后」「N日后」数字式（如「三天后」「过了7天」）
        for pat in ["日后", "天后", "天过去", "天转"] {
            if let Some(idx) = text.find(pat) {
                let head = &text[..idx];
                let digits: String = head
                    .chars()
                    .rev()
                    .take(4)
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                if let Ok(n) = digits.parse::<u32>() {
                    if (1..=30).contains(&n) {
                        return Some(format!("{}天后", n));
                    }
                }
            }
        }
        // 3b) 「过了 N 天/日」：`过/等` 后（可带「了」）跟数字再跟『天』『日』（如「过了7天」「过了三日」）
        for (marker, tw) in [("过", "天"), ("过", "日"), ("等", "天"), ("等", "日")] {
            let mut search_from = 0;
            while let Some(idx) = text[search_from..].find(marker) {
                let abs = search_from + idx + marker.len();
                let rest = &text[abs..];
                // 跳过可选「了」
                let rest = rest.strip_prefix('了').unwrap_or(rest);
                let num_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
                if num_len > 0 {
                    let after_num: String = rest.chars().skip(num_len).take(2).collect();
                    if after_num.starts_with(tw) {
                        if let Ok(n) = rest[..num_len].parse::<u32>() {
                            if (1..=30).contains(&n) {
                                return Some(format!("{}天后", n));
                            }
                        }
                    }
                }
                search_from = abs;
            }
        }
        // 4) 中文数字「X」天（一二三四…日/天）
        const CN: [(&str, u32); 11] = [
            ("一", 1), ("两", 2), ("二", 2), ("三", 3), ("四", 4), ("五", 5),
            ("六", 6), ("七", 7), ("八", 8), ("九", 9), ("十", 10),
        ];
        for (cn, n) in CN {
            for suf in ["日后", "天后"] {
                if text.contains(&format!("{}{}", cn, suf)) || text.contains(&format!("{}{}", cn, "天过去")) {
                    return Some(format!("{}天后", n));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_cycles_and_crosses_day() {
        let mut c = GameClock::default();
        assert_eq!(c.state_line(), "第1天｜清晨｜晴｜春季｜20℃");
        // 清晨→上午→…→凌晨→清晨（跨日）
        let mut crossed = false;
        for _ in 0..8 {
            crossed = c.advance_turn(1);
        }
        assert!(crossed, "第 8 次应跨日");
        assert_eq!(c.day, 2);
        assert_eq!(c.time_of_day, "清晨");
    }

    #[test]
    fn jump_forward_and_norm_positive_order() {
        let mut c = GameClock::default();
        // 当前清晨，跳到深夜 → 同日正序
        let (days, desc) = c.jump("深夜", 5).unwrap();
        assert_eq!(days, 0);
        assert!(desc.contains("深夜"));
        assert_eq!(c.day, 1);
        // 从深夜跳回上午 → 应视为次日
        let (days, _) = c.jump("上午", 6).unwrap();
        assert_eq!(days, 1);
        assert_eq!(c.day, 2);
        // 次日清晨
        let (days, _) = c.jump("次日清晨", 7).unwrap();
        assert_eq!(days, 1);
        assert_eq!(c.day, 3);
        assert_eq!(c.time_of_day, "清晨");
    }

    #[test]
    fn weather_progressive_only() {
        let mut c = GameClock::default();
        // 晴 → 多云 邻接，ok
        assert!(c.set_weather("多云").is_ok());
        // 多云 → 暴雨 不邻接，拒绝
        assert!(c.set_weather("暴雨").is_err());
        // 多云 → 阴 邻接
        assert!(c.set_weather("阴").is_ok());
        // 阴 → 小雨 邻接
        assert!(c.set_weather("小雨").is_ok());
        assert_eq!(c.weather, "小雨");
    }

    #[test]
    fn normalize_slots() {
        assert_eq!(normalize_time_slot("黎明"), Some("清晨"));
        assert_eq!(normalize_time_slot("下午"), Some("午后"));
        assert_eq!(normalize_time_slot("半夜"), Some("深夜"));
        assert_eq!(normalize_time_slot("中午"), Some("正午"));
        assert_eq!(normalize_time_slot("黄昏"), Some("傍晚"));
        assert_eq!(normalize_time_slot("乱码"), None);
    }

    #[test]
    fn season_derives_and_overrides() {
        let mut c = GameClock::default();
        // 默认 season 为空串 → 按 day 推导（day=1 → 春）
        assert_eq!(c.season(), "春");
        // day 推进跨过季界 → 夏
        c.day = super::DAYS_PER_SEASON + 1;
        assert_eq!(c.season(), "夏");
        // 显式覆盖
        c.set_season("冬").unwrap();
        assert_eq!(c.season(), "冬");
        c.set_season("夏天").unwrap();
        assert_eq!(c.season(), "夏");
        assert!(c.set_season("黄梅天").is_err());
    }

    #[test]
    fn temp_derives_from_season_weather() {
        let mut c = GameClock::default();
        assert_eq!(c.temp, 20); // 春/晴
        // 冬 + 晴 → 低温（0℃ 附近，低于春季晴）
        c.set_season("冬").unwrap();
        assert!(c.temp <= 5, "冬晴应为低温，got {}", c.temp);
        // 夏 + 晴 → 高温
        c.set_season("夏").unwrap();
        assert!(c.temp >= 28, "夏晴应高温，got {}", c.temp);
    }

    #[test]
    fn state_line_has_season_and_temp() {
        let mut c = GameClock::default();
        let line = c.state_line();
        assert!(line.contains("春季"), "got {line}");
        assert!(line.contains("℃"), "got {line}");
        c.set_season("秋").unwrap();
        assert!(c.state_line().contains("秋季"));
    }

    #[test]
    fn weighted_neighbor_stays_adjacent_and_seasonal() {
        let mut c = GameClock::default();
        // 春 + 晴：邻居里挑季节加权最高者，必须仍是晴的邻居
        c.set_season("春").unwrap();
        c.set_weather("晴").unwrap();
        let (cand, _) = c.weighted_neighbor().expect("晴必有邻居");
        let neighbors = weather_neighbors("晴");
        assert!(neighbors.contains(&cand), "候选 {cand} 非晴的邻居");
    }

    #[test]
    fn force_weather_allows_jumps() {
        let mut c = GameClock::default();
        // 晴 → 暴雨 不邻接，set_weather 拒绝
        assert!(c.set_weather("暴雨").is_err());
        // 但 force_weather（用户指令）直接设置
        assert!(c.force_weather("暴雨").is_ok());
        assert_eq!(c.weather, "暴雨");
        // 非权威天气仍拒绝
        assert!(c.force_weather("雷暴").is_err());
    }

    #[test]
    fn llm_eval_due_interval() {
        let mut c = GameClock::default();
        // turn 0（尚未有任何回合）: 不评估（开局需先有剧情）
        assert!(!c.llm_eval_due(0, 4));
        // turn 1（首回合，从未评估）: 评估
        assert!(c.llm_eval_due(1, 4));
        // 从未评估且 turn>=1 → 始终应评估（首回合后若一直没记录，继续评估）
        assert!(c.llm_eval_due(4, 4));
        // 记录评估后 turn 5（距上次 4 回合）: 应评估
        c.last_llm_eval_turn = 1;
        assert!(c.llm_eval_due(5, 4));
        assert!(!c.llm_eval_due(4, 4)); // 恰好 3 回合差，未到间隔
        // interval=0 恒评估
        assert!(c.llm_eval_due(100, 0));
    }

    #[test]
    fn extract_advance_signal_variants() {
        // 显式标注
        assert_eq!(GameClock::extract_advance_signal("正文 [时间推进: 次日清晨] 结束").as_deref(), Some("次日清晨"));
        // 自然语言
        assert_eq!(GameClock::extract_advance_signal("我睡一觉再谈").as_deref(), Some("次日清晨"));
        assert_eq!(GameClock::extract_advance_signal("等过了一夜再说").as_deref(), Some("次日清晨"));
        // 数字天数
        assert_eq!(GameClock::extract_advance_signal("三天后再见").as_deref(), Some("3天后"));
        assert_eq!(GameClock::extract_advance_signal("过了7天").as_deref(), Some("7天后"));
        // 中文数字
        assert_eq!(GameClock::extract_advance_signal("五日后启程").as_deref(), Some("5天后"));
        // 无信号（闲聊）
        assert_eq!(GameClock::extract_advance_signal("今天晚上吃什么"), None);
        assert_eq!(GameClock::extract_advance_signal("你好呀"), None);
    }

    // [1B 2026-08-18] derive_from_text：从 pack 开场信号推导初始时间天气。
    #[test]
    fn derive_from_text_suxiao_rain_evening() {
        // 宿醉楔子：夏末傍晚 + 天雨欲来云遮月 → 夏/傍晚/雨
        let c = GameClock::derive_from_text("夏末傍晚的家中客厅，天雨欲来云遮月");
        assert_eq!(c.season, "夏");
        assert_eq!(c.time_of_day, "傍晚");
        assert!(c.weather == "小雨" || c.weather == "阴", "weather={}", c.weather);
    }

    #[test]
    fn derive_from_text_no_signal_keeps_default() {
        // 无任何信号的 pack → 保持默认（清晨/晴/season 空→日轮转）
        let c = GameClock::derive_from_text("不可知的位置");
        assert_eq!(c.time_of_day, "清晨");
        assert_eq!(c.weather, "晴");
        assert!(c.season.is_empty());
    }

    #[test]
    fn derive_from_text_snow_winter_night() {
        // 冬夜大雪 → 冬/夜晚/大雪（长词优先于「雪」）
        let c = GameClock::derive_from_text("深冬的夜晚，大雪纷飞");
        assert_eq!(c.season, "冬");
        assert_eq!(c.time_of_day, "夜晚");
        assert_eq!(c.weather, "大雪");
    }

    #[test]
    fn derive_from_text_weather_valid_in_authority_set() {
        // 推导出的天气必须是权威集合成员
        for txt in ["清晨晴日", "暴雨倾盆的午后", "雾蒙蒙的黎明", "大风呼啸的上午"] {
            let c = GameClock::derive_from_text(txt);
            assert!(WEATHERS.contains(&c.weather.as_str()), "{txt} → {}", c.weather);
        }
    }
}
