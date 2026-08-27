//! 审稿闭环（U4，T1 创作质量）：多维审稿记录持久化。
//!
//! 参考 Openwrite `revision_store.py`：审稿不只给一次性意见，而是把每次审稿
//! 的维度问题清单（≥15 维）持久化到 work 数据目录，后续可按问题逐条发起
//! 修复并复查，形成「审稿 → 修复 → 复查降级」闭环。
//!
//! - `ReviewIssue`：单条问题（维度 / 严重度 / 引文 / 问题说明 / 修复指令 / 状态）。
//! - `ReviewRun`：一次审稿产生的全部问题（快照，版本化）。
//! - `ReviewStore`：per-work 持久化，存 `$DATA/works/{work_id}/reviews.json`
//!   （与 WorksFs 的 per-work 数据根一致，同 CompassStore 模式）。

use crate::{CoreError, CoreResult, DataRoot};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// reviews.json 的 schema 版本。
pub const REVIEW_SCHEMA_VERSION: u32 = 1;

/// 版本化 JSON 存储文件名（位于 work 目录下）。
pub const REVIEW_FILE_NAME: &str = "reviews.json";

/// 单 work 最多保留的审稿轮次（防无限膨胀）。
pub const REVIEW_MAX_RUNS: usize = 50;

/// 问题状态：待修复 / 已修复 / 已采纳（无需改）。
pub const REVIEW_STATUS_OPEN: &str = "open";
pub const REVIEW_STATUS_FIXED: &str = "fixed";
pub const REVIEW_STATUS_ACCEPTED: &str = "accepted";

fn default_review_schema_version() -> u32 {
    REVIEW_SCHEMA_VERSION
}

/// 单条审稿问题。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewIssue {
    /// 维度标识（如 `连续性`、`人物声线`、`文风`…见 REVIEW_DIMENSIONS）。
    pub dimension: String,
    /// 严重度 1-3（3 最重；1 为建议级）。
    pub severity: u8,
    /// 触发问题的原文片段（可空）。
    pub quote: String,
    /// 问题说明（简洁中文）。
    pub problem: String,
    /// 可执行修复指令（LLM 可据此改稿）。
    pub fix_instruction: String,
    /// 状态：open / fixed / accepted。
    #[serde(default = "default_open")]
    pub status: String,
}

fn default_open() -> String {
    REVIEW_STATUS_OPEN.to_string()
}

/// 一次审稿的快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRun {
    /// 审稿轮次 id（`review-{unix_ms}`）。
    pub id: String,
    /// 审稿对象（章节/片段标识）。
    pub target: String,
    /// 时间戳（unix 秒）。
    pub created_at: i64,
    /// 问题清单（LLM 审稿结果，已结构化）。
    pub issues: Vec<ReviewIssue>,
}

/// work 级审稿历史。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewHistory {
    #[serde(default = "default_review_schema_version")]
    pub schema_version: u32,
    /// 全部审稿轮次（新→旧）。
    #[serde(default)]
    pub runs: Vec<ReviewRun>,
}

impl Default for ReviewHistory {
    fn default() -> Self {
        Self {
            schema_version: REVIEW_SCHEMA_VERSION,
            runs: Vec::new(),
        }
    }
}

/// 审稿必须覆盖的 15 个维度（U4 验收要求 ≥15 维）。
pub const REVIEW_DIMENSIONS: [&str; 15] = [
    "连续性",
    "人物声线",
    "文风",
    "剧情逻辑",
    "节奏",
    "视角一致性",
    "对话自然度",
    "世界设定一致",
    "伏笔呼应",
    "时间线",
    "空间描写",
    "情感弧线",
    "信息边界",
    "重复表达",
    "标点排版",
];

/// per-work 审稿历史存储。
#[derive(Clone)]
pub struct ReviewStore {
    data: DataRoot,
    lock: Arc<Mutex<()>>,
}

impl ReviewStore {
    pub fn new(data: DataRoot) -> Self {
        let _ = data.ensure_layout();
        Self {
            data,
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn path_for(&self, work_id: &str) -> CoreResult<PathBuf> {
        let id = validate_work_id(work_id)?;
        Ok(self
            .data
            .root()
            .join("works")
            .join(id)
            .join(REVIEW_FILE_NAME))
    }

    /// 读取审稿历史；文件不存在返回空历史。
    pub fn load(&self, work_id: &str) -> CoreResult<ReviewHistory> {
        let _g = self.lock.lock();
        self.load_locked(work_id)
    }

    /// 锁内读（私有）：调用方必须已持有 self.lock
    fn load_locked(&self, work_id: &str) -> CoreResult<ReviewHistory> {
        let path = self.path_for(work_id)?;
        if !path.exists() {
            return Ok(ReviewHistory::default());
        }
        let raw = fs::read_to_string(path)?;
        ReviewHistory::from_json(&raw)
    }

    /// 追加一次审稿（新轮次置于最前，超出上限裁剪旧的）。
    pub fn append_run(&self, work_id: &str, run: ReviewRun) -> CoreResult<ReviewRun> {
        let _g = self.lock.lock();
        let mut hist = self.load_locked(work_id)?;
        hist.runs.insert(0, run.clone());
        if hist.runs.len() > REVIEW_MAX_RUNS {
            hist.runs.truncate(REVIEW_MAX_RUNS);
        }
        let path = self.path_for(work_id)?;
        write_atomic(&path, &hist.to_json()?)?;
        Ok(run)
    }

    /// 更新指定轮次的某条问题（复查降级 / 标记修复）。
    pub fn update_issue(
        &self,
        work_id: &str,
        run_id: &str,
        issue_idx: usize,
        issue: ReviewIssue,
    ) -> CoreResult<ReviewRun> {
        let _g = self.lock.lock();
        let mut hist = self.load_locked(work_id)?;
        let run = hist
            .runs
            .iter_mut()
            .find(|r| r.id == run_id)
            .ok_or_else(|| CoreError::NotFound(format!("review run {run_id} not found")))?;
        if issue_idx >= run.issues.len() {
            return Err(CoreError::BadRequest(format!(
                "issue index {issue_idx} out of range (len {})",
                run.issues.len()
            )));
        }
        run.issues[issue_idx] = issue;
        let updated = run.clone();
        let path = self.path_for(work_id)?;
        write_atomic(&path, &hist.to_json()?)?;
        Ok(updated)
    }
}

impl ReviewHistory {
    pub fn from_json(raw: &str) -> CoreResult<Self> {
        serde_json::from_str(raw).map_err(Into::into)
    }

    pub fn to_json(&self) -> CoreResult<String> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }
}

/// Work 目录校验：与 `$DATA/works/{id}` 的 id 规则一致，防路径逃逸。
fn validate_work_id(id: &str) -> CoreResult<String> {
    let s = id.trim();
    if s.is_empty()
        || s.len() > 128
        || s.contains('/')
        || s.contains('\\')
        || s.contains("..")
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CoreError::BadRequest("invalid work_id".into()));
    }
    Ok(s.to_string())
}

fn write_atomic(path: &Path, body: &str) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// U5 后置规则检查（T1 创作质量·第二优先）
//
// 参考 Openwrite `post_validator.py`：审稿（LLM 15 维）之外补一层**纯规则引擎**，
// 零 LLM、零新依赖，用少量可配置规则扫描正文，把硬性问题（违禁词 / AI 痕迹句式 /
// 超长句 / 相邻重复词 / 标点滥用）结构化列出，供前端问题面板并入审稿视图。
// ---------------------------------------------------------------------------

/// 规则名常量（前端按 rule 分组展示 / 用户可逐条"采纳"）。
pub const POST_RULE_FORBIDDEN_WORD: &str = "违禁词";
pub const POST_RULE_AI_TRACE: &str = "AI痕迹句式";
pub const POST_RULE_LONG_SENTENCE: &str = "超长句";
pub const POST_RULE_REPEAT_WORD: &str = "重复词";
pub const POST_RULE_PUNCT_ABUSE: &str = "标点滥用";

/// 单条规则违例（对应任务书 {rule, line, quote, fix} + severity）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostIssue {
    pub rule: String,
    /// 1-3（3 最重）。
    pub severity: u8,
    /// 1-based 行号（按 \n 划分；0 表示跨行/无法定位）。
    pub line: usize,
    /// 触发原文片段。
    pub quote: String,
    /// 修改建议（中文）。
    pub fix: String,
}

/// UTF-8 安全截取：按字节区间裁剪 quote 时向内收缩到字符边界。
/// [P13 2026-08-26] 修复：中文（3 字节/字）下 `pos±N` 落进多字节字符内部会 panic
/// （"end byte index 28 is not a char boundary; it is inside '推'"）。
fn clip_utf8(line: &str, start: usize, end: usize) -> String {
    let mut s = start.min(line.len());
    while s > 0 && !line.is_char_boundary(s) {
        s -= 1;
    }
    let mut e = end.min(line.len());
    while e > s && !line.is_char_boundary(e) {
        e -= 1;
    }
    line[s..e].to_string()
}

/// 默认违禁词表（最小硬集合；可按团队口径扩展）。
const FORBIDDEN_WORD_LIST: &[&str] = &[
    "傻逼", "妈的", "操你妈", "狗日的", "废物", "去死", "全家死光", "畜生",
    "贱人", "婊子", "娘希匹",
];

/// AI 痕迹句式特征（网文写作常见模板腔）。
const AI_TRACE_PHRASES: &[&str] = &[
    "值得注意的是", "总的来说", "综上所述", "众所周知", "首先", "其次", "最后",
    "不仅如此", "可以说", "换句话说", "从某种角度来说", "究其原因", "不难看出",
    "在这个充满", "这是一个充满", "随着时代", "在现代社会", "时代洪流",
];

/// 单句最大字符数（超过则判超长句）。
const LONG_SENTENCE_CHAR_LIMIT: usize = 60;

/// 对正文执行全部后置规则，返回违例列表（按位置排序）。
pub fn run_post_check(content: &str) -> Vec<PostIssue> {
    let mut out: Vec<PostIssue> = Vec::new();
    if content.trim().is_empty() {
        return out;
    }
    let lines: Vec<&str> = content.split('\n').collect();
    // 1) 违禁词（逐行，可定位行号）
    for (i, line) in lines.iter().enumerate() {
        for w in FORBIDDEN_WORD_LIST {
            if let Some(pos) = line.find(w) {
                let start = pos.saturating_sub(6);
                let end = (pos + w.len() + 6).min(line.len());
                out.push(PostIssue {
                    rule: POST_RULE_FORBIDDEN_WORD.to_string(),
                    severity: 3,
                    line: i + 1,
                    quote: clip_utf8(line, start, end),
                    fix: format!("将「{w}」替换为中性表达，或删除该词"),
                });
            }
        }
    }
    // 2) AI 痕迹句式（全文去重，只报首次命中行）
    {
        let mut seen: Vec<usize> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            for ph in AI_TRACE_PHRASES {
                if seen.contains(&i) {
                    break;
                }
                if let Some(pos) = line.find(ph) {
                    seen.push(i);
                    let start = pos.saturating_sub(4);
                    let end = (pos + ph.len() + 10).min(line.len());
                    out.push(PostIssue {
                        rule: POST_RULE_AI_TRACE.to_string(),
                        severity: 2,
                        line: i + 1,
                        quote: clip_utf8(line, start, end),
                        fix: format!(
                            "「{ph}」是模板腔连接词，建议改成事件性推进或直接删去，让动作/对话自证"
                        ),
                    });
                    break;
                }
            }
        }
    }
    // 3) 超长句（按中英文句末标点切分，取超限者）
    for (i, line) in lines.iter().enumerate() {
        let sentences = split_sentences(line);
        for s in sentences {
            let clean: String = s.trim().chars().collect();
            if clean.chars().count() > LONG_SENTENCE_CHAR_LIMIT {
                let clip: String = clean.chars().take(LONG_SENTENCE_CHAR_LIMIT + 8).collect();
                out.push(PostIssue {
                    rule: POST_RULE_LONG_SENTENCE.to_string(),
                    severity: 2,
                    line: i + 1,
                    quote: clip,
                    fix: "单句过长（>60 字），建议在动作/语气转折处拆成 2-3 个短句".to_string(),
                });
            }
        }
    }
    // 4) 相邻重复（2-gram 连续出现两次，如「她说她说」「很好很好」）
    let joined: Vec<char> = content.chars().filter(|c| !c.is_whitespace()).collect();
    if joined.len() > 4 {
        let n = joined.len();
        let mut i = 0;
        while i + 4 < n {
            if joined[i] == joined[i + 2] && joined[i + 1] == joined[i + 3] {
                let slice: String = joined[i..i + 4].iter().collect();
                let line = line_of_joined(content, i);
                out.push(PostIssue {
                    rule: POST_RULE_REPEAT_WORD.to_string(),
                    severity: 1,
                    line,
                    quote: slice.clone(),
                    fix: format!("「{slice}」疑似相邻重复短语，精简其一"),
                });
                i += 4;
            } else {
                i += 1;
            }
        }
    }
    // 5) 标点滥用（连续 3+ 同标点 / 一串省略号）
    for (i, line) in lines.iter().enumerate() {
        let runs = consecutive_punct_runs(line);
        for (p, len) in runs {
            let quote: String = std::iter::repeat(p).take(len).collect();
            out.push(PostIssue {
                rule: POST_RULE_PUNCT_ABUSE.to_string(),
                severity: 1,
                line: i + 1,
                quote,
                fix: format!("连续 {len} 个「{p}」属于情绪化标点滥用，建议至多保留 1-2 个"),
            });
        }
    }
    out.sort_by_key(|x| (x.line, x.rule.clone()));
    out
}

/// 按句末标点切句（。！？…；换行视为一句），保留原作标点。
fn split_sentences(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        cur.push(ch);
        if matches!(ch, '。' | '！' | '？' | '；' | ';' | '!') {
            out.push(cur.clone());
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 计算拼掉空白后的字符偏移 → 原始行号。
fn line_of_joined(content: &str, clean_offset: usize) -> usize {
    let mut clean_count = 0usize;
    for (i, line) in content.split('\n').enumerate() {
        let nonspace = line.chars().filter(|c| !c.is_whitespace()).count();
        if clean_count + nonspace > clean_offset {
            return i + 1;
        }
        clean_count += nonspace;
    }
    content.split('\n').count().max(1)
}

/// 找到一段里连续同类标点的 run（长度 >=3 才报）。
fn consecutive_punct_runs(line: &str) -> Vec<(char, usize)> {
    let mut runs: Vec<(char, usize)> = Vec::new();
    let mut last: Option<char> = None;
    let mut len = 0usize;
    for ch in line.chars() {
        let is_punct = matches!(ch, '！' | '？' | '，' | '!' | '?' | '～' | '…');
        if is_punct && last == Some(ch) {
            len += 1;
        } else {
            if let (Some(c), true) = (last, len >= 3) {
                runs.push((c, len));
            }
            last = Some(ch);
            len = 1;
        }
    }
    if let (Some(c), true) = (last, len >= 3) {
        runs.push((c, len));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub struct Tmp {
        path: PathBuf,
    }

    impl Tmp {
        pub fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("st_review_test_{nanos}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn issue(dimension: &str, severity: u8) -> ReviewIssue {
        ReviewIssue {
            dimension: dimension.to_string(),
            severity,
            quote: String::new(),
            problem: format!("{dimension} 问题"),
            fix_instruction: format!("修复 {dimension}"),
            status: REVIEW_STATUS_OPEN.to_string(),
        }
    }

    #[test]
    fn default_history_empty() {
        let h = ReviewHistory::default();
        assert!(h.runs.is_empty());
        assert_eq!(h.schema_version, REVIEW_SCHEMA_VERSION);
    }

    #[test]
    fn append_and_load_roundtrip() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path.clone()).unwrap();
        let store = ReviewStore::new(data);
        let run = ReviewRun {
            id: "review-1".into(),
            target: "chapter-01".into(),
            created_at: 1,
            issues: vec![issue("连续性", 2), issue("节奏", 1)],
        };
        let saved = store.append_run("work-a", run.clone()).unwrap();
        assert_eq!(saved.issues.len(), 2);
        let hist = store.load("work-a").unwrap();
        assert_eq!(hist.runs.len(), 1);
        assert_eq!(hist.runs[0].id, "review-1");
        // 未触及 work 为空
        let other = store.load("work-b").unwrap();
        assert!(other.runs.is_empty());
    }

    #[test]
    fn update_issue_marks_fixed() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path.clone()).unwrap();
        let store = ReviewStore::new(data);
        store
            .append_run(
                "work-a",
                ReviewRun {
                    id: "r1".into(),
                    target: "c1".into(),
                    created_at: 1,
                    issues: vec![issue("文风", 2), issue("标点排版", 1)],
                },
            )
            .unwrap();
        let mut fixed = issue("文风", 2);
        fixed.status = REVIEW_STATUS_FIXED.to_string();
        let run = store.update_issue("work-a", "r1", 0, fixed).unwrap();
        assert_eq!(run.issues[0].status, REVIEW_STATUS_FIXED);
        assert_eq!(run.issues[1].dimension, "标点排版");
        // 越界索引报错
        assert!(store.update_issue("work-a", "r1", 99, issue("文风", 1)).is_err());
        // 未知 run 报错
        assert!(store.update_issue("work-a", "nope", 0, issue("文风", 1)).is_err());
    }

    #[test]
    fn trims_to_max_runs() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path.clone()).unwrap();
        let store = ReviewStore::new(data);
        for i in 0..(REVIEW_MAX_RUNS + 10) {
            store
                .append_run(
                    "work-a",
                    ReviewRun {
                        id: format!("r{i}"),
                        target: "c".into(),
                        created_at: i as i64,
                        issues: vec![],
                    },
                )
                .unwrap();
        }
        let hist = store.load("work-a").unwrap();
        assert_eq!(hist.runs.len(), REVIEW_MAX_RUNS);
    }

    #[test]
    fn invalid_work_id_rejected() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path.clone()).unwrap();
        let store = ReviewStore::new(data);
        for bad in ["", "..", "a/b", "a\\b", "a..b", "s p a c e"] {
            assert!(store.load(bad).is_err(), "work_id `{bad}` should be rejected");
        }
    }

    #[test]
    fn stores_are_isolated_per_work() {
        let tmp = Tmp::new();
        let data = DataRoot::new(tmp.path.clone()).unwrap();
        let store = ReviewStore::new(data);
        store
            .append_run(
                "work-a",
                ReviewRun {
                    id: "r1".into(),
                    target: "c".into(),
                    created_at: 1,
                    issues: vec![issue("文风", 1)],
                },
            )
            .unwrap();
        let b = store.load("work-b").unwrap();
        assert!(b.runs.is_empty());
    }
}

#[cfg(test)]
mod post_check_utf8_tests {
    use super::*;

    /// [P13 2026-08-26] 回归：中文行内 quote 字节裁剪曾 panic
    /// （"end byte index 28 is not a char boundary; it is inside '推'"）。
    #[test]
    fn post_check_chinese_quote_no_panic() {
        // AI 痕迹 + 违禁词同现，且命中词两侧都是多字节字符。
        let content = "值得注意的是，沈棠推门而入。她心想：这天气真是妈的糟糕。\n掌柜综上所述地点了点头。";
        let issues = run_post_check(content);
        assert!(!issues.is_empty());
        for issue in &issues {
            assert!(std::str::from_utf8(issue.quote.as_bytes()).is_ok());
        }
    }

    #[test]
    fn clip_utf8_shrinks_to_char_boundary() {
        let line = "值得注意的是沈棠推门";
        // 人为构造非边界字节区间（'值' 占 3 字节：0..3；取 start=1/end=5 均非法）。
        let clipped = clip_utf8(line, 1, 5);
        assert!(line.contains(&clipped) || clipped.is_empty());
        // 边界内的合法区间不受影响。
        assert_eq!(clip_utf8(line, 0, line.len()), line);
    }
}
