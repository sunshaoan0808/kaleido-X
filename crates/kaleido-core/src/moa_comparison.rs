//! MoA (Mixture of Agents) Comparison Panel
//! Pure Rust logic library for multi-model comparison panels.
//! hermes-fake-moa stripped into core: panels + parallel responses without network/IO.

use serde::{Deserialize, Serialize};

/// Unique model endpoint identifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEndpoint {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub label: String,
}

impl ModelEndpoint {
    /// Display name for UI: [provider] model (label)
    pub fn display_name(&self) -> String {
        format!("[{}] {} {}", self.provider, self.model, self.label)
    }
}

/// A comparison panel definition: 2-5 models side-by-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonPanel {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<ModelEndpoint>,
}

impl ComparisonPanel {
    /// Create new panel.
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            endpoints: Vec::new(),
        }
    }

    /// Add endpoint (max 5 allowed).
    pub fn add(&mut self, ep: ModelEndpoint) -> Result<(), String> {
        if self.endpoints.len() >= 5 {
            return Err("Maximum of 5 endpoints allowed in MoA panel".to_string());
        }
        self.endpoints.push(ep);
        Ok(())
    }

    /// True if 2 <= endpoints.len() <= 5
    pub fn is_valid(&self) -> bool {
        let n = self.endpoints.len();
        n >= 2 && n <= 5
    }
}

/// Single model's raw response from one LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub endpoint_id: String,
    pub raw_text: String,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

/// Session status for comparison run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionStatus {
    Pending,
    Running,
    Complete,
    Failed,
}

/// In-memory comparison session: collects responses from multiple models.
/// `aggregated` holds the final synthesized answer (true MoA aggregator pass),
/// produced after all model responses are collected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonSession {
    pub id: String,
    pub panel_id: String,
    pub prompt: String,
    pub status: SessionStatus,
    pub results: Vec<ModelResponse>,
    /// 真聚合输出：聚合器 LLM 综合多模型答案后的单一最终答案。
    #[serde(default)]
    pub aggregated: Option<String>,
    /// 聚合器调用耗时。
    #[serde(default)]
    pub aggregate_elapsed_ms: Option<u64>,
    /// 聚合器失败原因（聚合失败不影响并排结果展示）。
    #[serde(default)]
    pub aggregate_error: Option<String>,
}

impl ComparisonSession {
    /// Create new session for a panel + prompt.
    pub fn new(id: &str, panel_id: &str, prompt: &str) -> Self {
        Self {
            id: id.to_string(),
            panel_id: panel_id.to_string(),
            prompt: prompt.to_string(),
            status: SessionStatus::Pending,
            results: Vec::new(),
            aggregated: None,
            aggregate_elapsed_ms: None,
            aggregate_error: None,
        }
    }

    /// Add a model's response (enforces state machine).
    pub fn add_result(&mut self, r: ModelResponse) -> Result<(), String> {
        if matches!(
            self.status,
            SessionStatus::Complete | SessionStatus::Failed
        ) {
            return Err("Cannot append result to completed/failed session".to_string());
        }
        self.results.push(r);
        Ok(())
    }

    /// All models succeeded (no errors).
    pub fn all_succeeded(&self) -> bool {
        self.results.iter().all(|r| r.error.is_none())
    }

    /// Count of successful responses.
    pub fn success_count(&self) -> usize {
        self.results.iter().filter(|r| r.error.is_none()).count()
    }

    /// Generate human-readable Markdown comparison table (key output for UI).
    pub fn build_summary(&self) -> String {
        let prompt_short = self
            .prompt
            .chars()
            .take(50)
            .collect::<String>()
            .replace('\n', " ");
        let mut table = format!(
            "# 对比摘要: {} / {}\n",
            self.panel_id, prompt_short
        );
        table.push_str("| 模型 | 状态 | 耗时 | 长度 |\n");
        table.push_str("|------|------|------|------|\n");

        for r in &self.results {
            let status = if r.error.is_some() { "❌" } else { "✅" };
            let time_str = format!("{}ms", r.elapsed_ms);
            let length = if let Some(err) = &r.error {
                format!("❌ error: {}", err.chars().take(20).collect::<String>())
            } else {
                r.raw_text.len().to_string()
            };
            // Real usage: caller maps endpoint_id -> name via panel; placeholder here
            let model = &r.endpoint_id;
            table.push_str(&format!("| {} | {} | {} | {} |\n", model, status, time_str, length));
        }
        table
    }

    /// 构造真聚合 prompt：把原始 prompt + 各模型成功答案打包，
    /// 交给聚合器 LLM 综合为单一最终答案。
    pub fn build_aggregate_prompt(&self) -> String {
        let mut p = String::new();
        p.push_str("你是一个答案聚合器（Mixture-of-Agents aggregator）。\n");
        p.push_str("下面是同一个问题由多个不同模型分别给出的答案。请综合它们：\n");
        p.push_str("- 取各家之长：正确且完整的部分保留，遗漏的部分互补，冲突处以多数/更可信者为准；\n");
        p.push_str("- 去除重复、纠正文法错误，保持结构清晰；\n");
        p.push_str("- 输出一份最终答案（单一版本），不要罗列各模型原文，不要提及\"模型A说\"之类的来源标注；\n");
        p.push_str("- 使用与原始问题相同的语言作答。\n\n");
        p.push_str("【原始问题】\n");
        p.push_str(&self.prompt);
        p.push_str("\n\n【各模型答案】\n");
        let ok: Vec<&ModelResponse> = self.results.iter().filter(|r| r.error.is_none()).collect();
        for (i, r) in ok.iter().enumerate() {
            p.push_str(&format!("--- 答案 {} ({}ms) ---\n", i + 1, r.elapsed_ms));
            p.push_str(&r.raw_text);
            p.push_str("\n\n");
        }
        p.push_str("【最终答案】\n");
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panel_add_ok() {
        let mut p = ComparisonPanel::new("p1", "Test Panel");
        let ep = ModelEndpoint {
            id: "ep1".into(),
            provider: "axoshub".into(),
            model: "grok-4".into(),
            label: "Groq".into(),
        };
        assert!(p.add(ep).is_ok());
        assert_eq!(p.endpoints.len(), 1);
        // 1 个端点尚未达到对比下限（>=2），面板仍 invalid
        assert!(!p.is_valid());
        let ep2 = ModelEndpoint {
            id: "ep2".into(),
            provider: "axoshub".into(),
            model: "deepseek-v4".into(),
            label: "DeepSeek".into(),
        };
        assert!(p.add(ep2).is_ok());
        assert_eq!(p.endpoints.len(), 2);
        assert!(p.is_valid());
    }

    #[test]
    fn test_panel_add_max5() {
        let mut p = ComparisonPanel::new("p1", "Test");
        for i in 1..=5 {
            let ep = ModelEndpoint {
                id: format!("ep{i}"),
                provider: "test".into(),
                model: format!("m{i}"),
                label: "".into(),
            };
            assert!(p.add(ep).is_ok());
        }
        assert_eq!(p.endpoints.len(), 5);
        assert!(p.is_valid());
    }

    #[test]
    fn test_panel_add_too_many() {
        let mut p = ComparisonPanel::new("p1", "Test");
        for i in 1..=6 {
            let ep = ModelEndpoint {
                id: format!("ep{i}"),
                provider: "test".into(),
                model: format!("m{i}"),
                label: "".into(),
            };
            if i <= 5 {
                assert!(p.add(ep).is_ok());
            } else {
                assert!(p.add(ep).is_err());
            }
        }
        assert_eq!(p.endpoints.len(), 5);
        // 5 个端点是合法上限，面板有效
        assert!(p.is_valid());
    }

    #[test]
    fn test_panel_invalid_less_than_2() {
        let p = ComparisonPanel::new("p1", "Test");
        assert!(!p.is_valid());
    }

    #[test]
    fn test_session_add_result() {
        let mut s = ComparisonSession::new("s1", "p1", "test prompt");
        let r = ModelResponse {
            endpoint_id: "ep1".into(),
            raw_text: "hello".into(),
            elapsed_ms: 100,
            error: None,
        };
        assert!(s.add_result(r).is_ok());
        assert_eq!(s.results.len(), 1);
    }

    #[test]
    fn test_session_cannot_add_after_complete() {
        let mut s = ComparisonSession::new("s1", "p1", "test");
        s.status = SessionStatus::Complete;
        let r = ModelResponse {
            endpoint_id: "ep1".into(),
            raw_text: "hello".into(),
            elapsed_ms: 100,
            error: None,
        };
        assert!(s.add_result(r).is_err());
    }

    #[test]
    fn test_all_succeeded() {
        let mut s = ComparisonSession::new("s1", "p1", "prompt");
        s.add_result(ModelResponse {
            endpoint_id: "1".into(),
            raw_text: "ok".into(),
            elapsed_ms: 10,
            error: None,
        })
        .unwrap();
        s.add_result(ModelResponse {
            endpoint_id: "2".into(),
            raw_text: "ok".into(),
            elapsed_ms: 20,
            error: None,
        })
        .unwrap();
        assert!(s.all_succeeded());
        assert_eq!(s.success_count(), 2);
    }

    #[test]
    fn test_build_summary() {
        let mut s = ComparisonSession::new("p1", "TestPanel", "Compare models?");
        s.add_result(ModelResponse {
            endpoint_id: "ep1".into(),
            raw_text: "response1".into(),
            elapsed_ms: 123,
            error: None,
        })
        .unwrap();
        s.add_result(ModelResponse {
            endpoint_id: "ep2".into(),
            raw_text: "response2".into(),
            elapsed_ms: 456,
            error: Some("timeout".into()),
        })
        .unwrap();
        let summary = s.build_summary();
        assert!(summary.contains("# 对比摘要:"));
        assert!(summary.contains("| 模型 | 状态 | 耗时 | 长度 |"));
        assert!(summary.contains("✅"));
        assert!(summary.contains("❌"));
    }

    #[test]
    fn test_build_aggregate_prompt_skips_errors() {
        let mut s = ComparisonSession::new("s1", "p1", "写一首关于雨的诗");
        s.add_result(ModelResponse {
            endpoint_id: "ep1".into(),
            raw_text: "answer one".into(),
            elapsed_ms: 100,
            error: None,
        })
        .unwrap();
        s.add_result(ModelResponse {
            endpoint_id: "ep2".into(),
            raw_text: "".into(),
            elapsed_ms: 200,
            error: Some("upstream 429".into()),
        })
        .unwrap();
        s.add_result(ModelResponse {
            endpoint_id: "ep3".into(),
            raw_text: "answer three".into(),
            elapsed_ms: 300,
            error: None,
        })
        .unwrap();
        let p = s.build_aggregate_prompt();
        assert!(p.contains("写一首关于雨的诗"));
        assert!(p.contains("answer one"));
        assert!(p.contains("answer three"));
        assert!(!p.contains("upstream 429"));
        // 错误项被跳过且重新编号：answer three 变为第 2 份
        assert!(p.contains("答案 1 (100ms)"));
        assert!(p.contains("答案 2 (300ms)"));
        assert!(!p.contains("答案 3"));
    }

    #[test]
    fn test_session_aggregated_fields_default() {
        // 旧数据反序列化：无 aggregated 字段时兼容
        let json = r#"{"id":"s1","panel_id":"p1","prompt":"q","status":"Complete","results":[]}"#;
        let s: ComparisonSession = serde_json::from_str(json).unwrap();
        assert_eq!(s.aggregated, None);
        assert_eq!(s.aggregate_elapsed_ms, None);
        assert_eq!(s.aggregate_error, None);
    }
}
