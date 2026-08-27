//! AppState + SSE StreamHub (extracted from main.rs P0-1)
use kaleido_core::{
    AgentSessionStore, AppStateStore, AuthStore, JobStore, PackStore, PartnerStore,
    TavernPersonaStore, TavernSessionStore, WorksFs,
};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatStreamEvent {
    pub(crate) run_id: String,
    pub(crate) event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) context_compaction: Option<Value>,
    /// P5: per-call token usage surfaced to chat UI (set on done events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_tokens: Option<i64>,
    /// D2: machine-readable error code (P1-4 envelope parity for SSE events).
    /// Set on eventType="error" events; absent/None on delta/done/thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) code: Option<String>,
}

impl ChatStreamEvent {
    /// D2: uniform error-event constructor — guarantees every SSE error
    /// carries a stable SCREAMING_SNAKE code alongside the human message.
    /// [P7] 生产路径暂未接线（错误事件均手写构造）；保留为约定 API，防漂移。
    #[allow(dead_code)]
    pub(crate) fn error(run_id: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            run_id: run_id.to_string(),
            event_type: "error".into(),
            delta: None,
            message: Some(message.into()),
            context_compaction: None,
            input_tokens: None,
            output_tokens: None,
            code: Some(code.to_string()),
        }
    }
}

pub(crate) struct StreamHub {
    /// run_id → broadcast sender (supports multiple subscribers + reconnects)
    senders: Mutex<HashMap<String, broadcast::Sender<ChatStreamEvent>>>,
    /// F4: run_id → persisted event history for replay on reconnect
    events: Mutex<HashMap<String, Vec<ChatStreamEvent>>>,
    /// cancel flags
    cancelled: Mutex<HashMap<String, bool>>,
}

/// F4: capacity for the broadcast channel — bounded to provide backpressure.
/// When full, `send` returns an error which the worker checks to detect
/// disconnected clients.
const STREAM_CHANNEL_CAPACITY: usize = 256;

impl StreamHub {
    pub(crate) fn new() -> Self {
        Self {
            senders: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn register(&self, run_id: &str) -> broadcast::Sender<ChatStreamEvent> {
        let (tx, _rx) = broadcast::channel(STREAM_CHANNEL_CAPACITY);
        self.senders.lock().insert(run_id.to_string(), tx.clone());
        self.events.lock().insert(run_id.to_string(), Vec::new());
        self.cancelled.lock().insert(run_id.to_string(), false);
        tx
    }

    /// F4: Subscribe to a run's event stream. Replays all persisted events
    /// then continues with live events. Returns None if the run has no
    /// registered sender (job not found / already cleaned up).
    pub(crate) fn subscribe(&self, run_id: &str) -> Option<(broadcast::Receiver<ChatStreamEvent>, Vec<ChatStreamEvent>)> {
        let tx = self.senders.lock().get(run_id).cloned()?;
        let replay = self.events.lock().get(run_id).cloned().unwrap_or_default();
        Some((tx.subscribe(), replay))
    }

    /// F4: Send an event, persisting it for replay. Returns `true` if sent
    /// successfully, `false` if all receivers are gone (client disconnected).
    pub(crate) fn send(&self, run_id: &str, evt: ChatStreamEvent) -> bool {
        // Persist for replay
        if let Some(history) = self.events.lock().get_mut(run_id) {
            // Cap history to prevent unbounded growth (keep last 1000 events).
            if history.len() >= 1000 {
                history.remove(0);
            }
            history.push(evt.clone());
        }
        let result = self.senders.lock().get(run_id).map(|tx| tx.send(evt));
        match result {
            Some(Ok(_)) => true,
            Some(Err(_)) => false, // no active receivers
            None => false,
        }
    }

    pub(crate) fn cancel(&self, run_id: &str) {
        self.cancelled.lock().insert(run_id.to_string(), true);
        // send error/done so SSE closes
        let _ = self.send(run_id, ChatStreamEvent {
            run_id: run_id.to_string(),
            event_type: "error".into(),
            delta: None,
            message: Some("stopped".into()),
            context_compaction: None,
                input_tokens: None,
                output_tokens: None,
                code: Some("STOPPED".into()),
        });
        self.cleanup(run_id);
    }

    pub(crate) fn is_cancelled(&self, run_id: &str) -> bool {
        self.cancelled
            .lock()
            .get(run_id)
            .copied()
            .unwrap_or(false)
    }

    /// U11: live-worker probe —— 该 run 是否有挂着的流式 worker（register 后 cleanup 前为 true）。
    /// 进程重启后 hub 为空 → false，用于判定「孤儿 running job」以支持 story turn resume。
    pub(crate) fn has_live_worker(&self, run_id: &str) -> bool {
        self.senders.lock().contains_key(run_id)
    }

    pub(crate) fn cleanup(&self, run_id: &str) {
        self.senders.lock().remove(run_id);
        self.events.lock().remove(run_id);
        self.cancelled.lock().remove(run_id);
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) auth: AuthStore,
    pub(crate) jobs: JobStore,
    pub(crate) sessions: AgentSessionStore,
    pub(crate) app_state: AppStateStore,
    pub(crate) partner: PartnerStore,
    pub(crate) works: WorksFs,
    pub(crate) packs: PackStore,
    pub(crate) search: kaleido_core::hybrid_search::SearchIndex,
    pub(crate) sessions_tavern: TavernSessionStore,
    pub(crate) personas: TavernPersonaStore,
    pub(crate) regex_library: kaleido_core::RegexLibraryStore,
    pub(crate) vector_index: kaleido_core::VectorIndexStore,
    pub(crate) hub: Arc<StreamHub>,
    pub(crate) llm_base: Option<String>,
    pub(crate) llm_key: Option<String>,
    pub(crate) llm_model: String,
    pub(crate) provider_kind: String,
    pub(crate) embedding_base: Option<String>,
    pub(crate) image_base_url: Option<String>,
    pub(crate) image_api_key: Option<String>,
    pub(crate) image_model: String,
    pub(crate) cf_image_base_url: Option<String>,
    pub(crate) cf_image_model: Option<String>,
    pub(crate) grok2api_image_base_url: Option<String>,
    pub(crate) grok2api_image_key: Option<String>,
    pub(crate) grok2api_image_model: Option<String>,
    pub(crate) plugin_registry: Arc<kaleido_core::plugin::PluginRegistry>,
    /// audit P0#3: 按 workspace 分片的世界状态，消除跨会话/跨用户污染。
    /// key = workspace_id；缺失时为 None（不注入摘要）。
    pub(crate) world_state:
        Arc<std::sync::Mutex<std::collections::HashMap<String, kaleido_core::world_state::WorldState>>>,
    pub(crate) weaver_config: kaleido_core::memory_weaver::WeaverConfig,
    pub(crate) graph: kaleido_core::graph_store::GraphStore,
    pub(crate) foreshadow: kaleido_core::foreshadow_store::ForeshadowStore,
    pub(crate) analysis: kaleido_core::analysis_store::AnalysisStore,
    pub(crate) reviews: kaleido_core::ReviewStore,
    pub(crate) ai_admin: kaleido_core::ai_admin_store::AiAdminStore,
    pub(crate) scene_cards: kaleido_core::scene_card_store::SceneCardStore,
    /// audit P0#4: 共享 ReferenceLibraryStore（Arc<Mutex> 单例），消除每次请求 new() 导致的锁失效
    pub(crate) reference_library: crate::reference_library::ReferenceLibraryStore,
    pub(crate) rpm: crate::ai_admin::RpmLimiter,
    /// G13/G14: 导演后台任务组（workspace 级串行，key = session_id）。HTTP 断线不取消。
    pub(crate) director_tasks: std::sync::Arc<kaleido_core::DirectorTaskGroup>,
}
