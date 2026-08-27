//! Tavern MCP 外设（吸收自 Liyuan mcp.ts, 默认仅本机 stdio server）。
//!
//! 本模块让剧情 LLM 能调用本机 MCP server 提供的真实工具（如 Gemini 搜索）。
//! 设计原则（与 GA 视觉 MCP 对接同一哲学）：
//! - 不引入 MCP 框架依赖，直接按 MCP JSON-RPC over stdio 协议自转调用；
//! - 每次调用独立 spawn 进程，单次往返（initialize → method），超时即 kill；
//! - 默认仅本机：配置放 `<data_root>/mcp.json`，缺省内置 gemini-search（本机 /opt/gemini-search-mcp）。
//!
//! 剧情集成（见 story_tavern.rs）：
//! - system prompt 注入工具清单 + 【工具】标记协议；
//! - LLM 输出形如 `【工具】server:tool\n{json}` 的独立段落 → 后端解析执行；
//! - 结果截断后存入 session.mcp_tool_results，下一轮 system prompt 回填。

use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// mcp.json 中单个 server 的配置。
#[derive(Debug, Clone, Deserialize)]
pub struct McpServer {
    pub id: String,
    #[allow(dead_code)] // [P7] MCP 工具定义展示名预留
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// 工具清单条目（注入 prompt 用）。
#[derive(Debug, Clone)]
pub struct McpToolEntry {
    pub server_id: String,
    pub name: String,
    pub description: String,
}

/// 一次【工具】调用（LLM 输出解析产物）。
#[derive(Debug, Clone)]
pub struct McpCall {
    pub server: String,
    pub tool: String,
    pub arguments: Value,
}

const TOOL_CACHE_TTL: Duration = Duration::from_secs(60);

static TOOL_CACHE: OnceLock<tokio::sync::Mutex<Option<(Instant, Vec<McpToolEntry>)>>> =
    OnceLock::new();

fn tool_cache() -> &'static tokio::sync::Mutex<Option<(Instant, Vec<McpToolEntry>)>> {
    TOOL_CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// 读取 MCP server 配置。缺省（无 mcp.json 或文件为空）仅内置本机 gemini-search。
pub fn load_servers(data_root: &Path) -> Vec<McpServer> {
    let cfg = data_root.join("mcp.json");
    if let Ok(text) = std::fs::read_to_string(&cfg) {
        if let Ok(list) = serde_json::from_str::<Vec<McpServer>>(&text) {
            if !list.is_empty() {
                return list;
            }
        }
    }
    // 默认仅本机：Gemini 搜索 MCP（Google AI Mode，/opt/gemini-search-mcp）
    vec![McpServer {
        id: "gemini-search".into(),
        name: "Gemini 搜索".into(),
        command: "python3".into(),
        args: vec!["-m".into(), "gemini_search_mcp".into()],
        cwd: Some("/opt/gemini-search-mcp".into()),
    }]
}

/// stdio 单次往返：spawn → initialize → notifications/initialized → method → 读结果。
/// 超时或进程退出视为失败。调用方负责 kill 子进程。
async fn mcp_roundtrip(
    server: &McpServer,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let mut cmd = Command::new(&server.command);
    cmd.args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = &server.cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", server.id))?;
    let stdin = child.stdin.take().ok_or_else(|| "no stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "no stdout".to_string())?;
    let mut stdin = tokio::io::BufWriter::new(stdin);
    let mut reader = BufReader::new(stdout).lines();

    let init_id: i64 = 1;
    let req_id: i64 = 2;
    let init = json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "kaleido-tavern", "version": "1"}
        }
    });
    if stdin
        .write_all(format!("{init}\n").as_bytes())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(format!("MCP {} 写入失败", server.id));
    }
    if stdin.flush().await.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(format!("MCP {} flush 失败", server.id));
    }

    let deadline = Instant::now() + timeout;
    let mut handshook = false;
    let mut result: Option<Value> = None;
    loop {
        if Instant::now() > deadline {
            break;
        }
        let line = match tokio::time::timeout(Duration::from_secs(2), reader.next_line()).await {
            Ok(Ok(Some(l))) => l,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(format!("MCP {} 读取失败: {e}", server.id));
            }
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if v["id"] == json!(init_id) {
            handshook = true;
            // 握手完成 → 发 initialized 通知 + 目标请求
            let notif = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
            let req = json!({"jsonrpc":"2.0","id":req_id,"method":method,"params":params});
            if stdin
                .write_all(format!("{notif}\n{req}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        } else if v["id"] == json!(req_id) {
            result = Some(v);
            break;
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    if !handshook {
        return Err(format!("MCP {} 未完成握手", server.id));
    }
    match result {
        Some(v) if v.get("result").is_some() => Ok(v["result"].clone()),
        Some(v) => Err(format!(
            "MCP {} {} 返回错误: {}",
            server.id,
            method,
            v["error"]
        )),
        None => Err(format!("MCP {} {} 超时/无响应", server.id, method)),
    }
}

/// 工具清单（60s 缓存）。失败仅记录 warn，不影响剧情。
pub async fn list_tools_cached(data_root: &Path) -> Vec<McpToolEntry> {
    let servers = load_servers(data_root);
    {
        let cache = tool_cache().lock().await;
        if let Some((at, tools)) = cache.as_ref() {
            if at.elapsed() < TOOL_CACHE_TTL {
                return tools.clone();
            }
        }
    }
    // 内建本地工具（server_id=kaleido，call_tool 内部分发，不依赖外部 MCP 进程）
    let mut out = vec![McpToolEntry {
        server_id: "kaleido".into(),
        name: "recall_foreshadow".into(),
        description: "回收一条伏笔（status=recalled）：玩家在叙事中揭示了该伏笔后调用，传 foreshadow_id。之后该伏笔从 system prompt 的伏笔小节消失。".into(),
    }];
    for s in &servers {
        match mcp_roundtrip(s, "tools/list", json!({}), Duration::from_secs(10)).await {
            Ok(v) => {
                if let Some(tools) = v.get("tools").and_then(|t| t.as_array()) {
                    for t in tools {
                        out.push(McpToolEntry {
                            server_id: s.id.clone(),
                            name: t["name"].as_str().unwrap_or("?").to_string(),
                            description: t["description"].as_str().unwrap_or("").to_string(),
                        });
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "mcp tools/list failed"),
        }
    }
    *tool_cache().lock().await = Some((Instant::now(), out.clone()));
    out
}

/// 工具清单 → prompt 段落（含【工具】标记协议说明）。
pub fn tools_markdown(tools: &[McpToolEntry]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "\n## 外设工具（MCP，本机）".to_string(),
        "需要查证现实信息（地名/天气/时间/新闻/人物背景等）时，可调用下列工具获取真实数据，再把结果织入叙事。调用格式为独立段落（不混在正文中）：".to_string(),
        "【工具】server_id:tool_name".to_string(),
        "{\"参数名\": \"值\"}".to_string(),
        "例：\n【工具】gemini-search:web_search\n{\"query\": \"北京 今天 天气\"}".to_string(),
        "工具清单：".to_string(),
    ];
    for t in tools {
        let desc = t
            .description
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        lines.push(format!("- `{}`（{}）：{}", t.name, t.server_id, desc));
    }
    lines.join("\n")
}

/// 调用单个 MCP 工具，返回截断后的文本结果（防注入）。
pub async fn call_tool(
    data_root: &Path,
    server_id: &str,
    tool: &str,
    arguments: Value,
) -> Result<String, String> {
    // 内建本地工具（server_id=kaleido）：不经过外部 MCP 进程。
    if server_id == "kaleido" {
        return call_builtin_tool(data_root, tool, arguments).await;
    }
    let servers = load_servers(data_root);
    let server = servers
        .iter()
        .find(|s| s.id == server_id)
        .ok_or_else(|| format!("未注册 MCP server: {server_id}"))?;
    let params = json!({"name": tool, "arguments": arguments});
    let v = mcp_roundtrip(server, "tools/call", params, Duration::from_secs(90)).await?;
    let mut parts = Vec::new();
    if let Some(content) = v.get("content").and_then(|c| c.as_array()) {
        for c in content {
            if let Some(text) = c["text"].as_str() {
                parts.push(text.to_string());
            }
        }
    }
    if let Some(sc) = v.get("structuredContent") {
        parts.push(sc.to_string());
    }
    if parts.is_empty() {
        return Ok("(工具返回空)".to_string());
    }
    let joined = parts.join("\n");
    Ok(joined.chars().take(3000).collect())
}

/// 内建本地工具分发（P0 闭环: 伏笔回收）。
async fn call_builtin_tool(data_root: &Path, tool: &str, arguments: Value) -> Result<String, String> {
    match tool {
        "recall_foreshadow" => {
            let id = arguments
                .get("foreshadow_id")
                .or_else(|| arguments.get("id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "recall_foreshadow 需要参数 foreshadow_id".to_string())?;
            let store = kaleido_core::foreshadow_store::ForeshadowStore::open(&data_root.join("plot.sqlite"))
                .map_err(|e| format!("foreshadow store: {e}"))?;
            let fs = store
                .get_foreshadow(id)
                .map_err(|e| format!("get_foreshadow: {e}"))?
                .ok_or_else(|| format!("伏笔不存在: {id}"))?;
            // 乐观并发: 带 expected_version_no 条件更新，冲突返回结构化错误让 LLM 重试。
            let updated = store
                .update_foreshadow(
                    id,
                    Some(fs.title),
                    Some(fs.description),
                    Some("recalled".into()),
                    Some(fs.weight),
                    Some(fs.parent_ids.clone()),
                    Some(fs.expected_version_no),
                )
                .map_err(|e| format!("recall 失败（可能版本冲突）: {e}"))?;
            Ok(format!("已回收伏笔「{}」→ status=recalled", updated.title))
        }
        other => Err(format!("未注册的内建工具: {other}")),
    }
}

/// 从叙事文本提取【工具】调用块并剥离（独立段落，不混在正文）。
pub fn split_mcp_calls_from_narrative(text: &str) -> (String, Vec<McpCall>) {
    const MARKER: &str = "【工具】";
    let mut calls = Vec::new();
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(MARKER) {
        clean.push_str(&rest[..pos]);
        let after = &rest[pos + MARKER.len()..];
        let line_end = after.find('\n').unwrap_or(after.len());
        let head = after[..line_end].trim();
        // head 形如 server:tool
        let (server, tool) = match head.split_once(':') {
            Some((s, t)) => {
                let s = s.trim();
                let t = t.trim();
                if s.is_empty() || t.is_empty() {
                    (String::new(), String::new())
                } else {
                    (s.to_string(), t.to_string())
                }
            }
            None => (String::new(), String::new()),
        };
        let mut args = Value::Null;
        let mut consumed = line_end;
        if server.is_empty() || tool.is_empty() {
            // 非法调用块：保留原文（不剥离），避免吞掉用户文本
            clean.push_str(MARKER);
            rest = after;
            continue;
        }
        // 可选 JSON 参数块：紧跟换行后的 { ... }
        let after_line = &after[line_end..];
        let json_start = after_line.find('{');
        if let Some(js) = json_start {
            if let Some(close) = find_json_close(&after_line[js..]) {
                if let Ok(v) = serde_json::from_str::<Value>(&after_line[js..js + close + 1]) {
                    args = v;
                    consumed = line_end + js + close + 1;
                }
            }
        }
        calls.push(McpCall {
            server,
            tool,
            arguments: args,
        });
        rest = &after[consumed..];
        // 剥掉块尾紧邻的换行，避免正文留下空行
        if rest.starts_with('\n') {
            rest = &rest[1..];
        }
    }
    clean.push_str(rest);
    (clean, calls)
}

/// 找到以 args[0] 为 '{' 的 JSON 对象的结束下标（跳过字符串内的括号）。
fn find_json_close(s: &str) -> Option<usize> {
    if !s.starts_with('{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, ch) in s.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_mcp_calls() {
        let text = "旁白：主角走进雨夜。\n【工具】gemini-search:web_search\n{\"query\": \"北京 今天 天气\"}\n他推开了门。";
        let (clean, calls) = split_mcp_calls_from_narrative(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].server, "gemini-search");
        assert_eq!(calls[0].tool, "web_search");
        assert_eq!(calls[0].arguments["query"], "北京 今天 天气");
        assert!(!clean.contains("【工具】"));
        assert!(clean.contains("推开"));
        assert!(clean.contains("雨夜"));
    }

    #[test]
    fn test_split_mcp_calls_no_args() {
        let text = "【工具】gemini-search:web_search\n正文";
        let (clean, calls) = split_mcp_calls_from_narrative(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, Value::Null);
        assert_eq!(clean, "正文");
    }

    #[test]
    fn test_find_json_close_nested() {
        let s = "{\"a\": {\"b\": \"}\"}, \"c\": 1} tail";
        assert_eq!(&s[..find_json_close(s).unwrap() + 1], "{\"a\": {\"b\": \"}\"}, \"c\": 1}");
    }

    /// 真实链路：stdio JSON-RPC → /opt/gemini-search-mcp → Google AI Mode 搜索。
    /// 默认不跑；显式验证用：cargo test -- --ignored tavern_mcp::tests::test_call_real_gemini
    #[tokio::test]
    #[ignore = "requires local gemini-search-mcp"]
    async fn test_call_real_gemini() {
        let data_root = std::env::temp_dir().join("tavern-mcp-it");
        std::fs::create_dir_all(&data_root).unwrap();
        let res = call_tool(
            &data_root,
            "gemini-search",
            "web_search",
            serde_json::json!({"query": "北京今天天气"}),
        )
        .await;
        match &res {
            Ok(s) => {
                assert!(!s.trim().is_empty(), "result empty");
                println!("RESULT_LEN={} HEAD={}", s.len(), &s[..s.len().min(200)]);
            }
            Err(e) => panic!("call failed: {e}"),
        }
    }
}
