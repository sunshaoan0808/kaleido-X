//! kaleido-server (S4): headless axum API
//! - Auth: username/password + bearer
//! - Partner: world books / character cards CRUD + systemPrompt injection
//! - Works: path-jailed workspace filesystem under $KALEIDO_DATA/works/{workspace_id}
//! - Mobile-compatible partner/chat APIs under /api/mobile/*
//! - Chat: start → {runId}, stream → SSE ChatStreamEvent JSON
//! - Agent sessions + app state under $KALEIDO_DATA/Kaleido/
//! - SPA under /web (chat + partner + works + settings)

use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use kaleido_core::{
    AgentSessionStore, AppStateStore, AuthStore, DataRoot, JobRecord, JobStore, PackStore, PartnerStore,
    TavernPersonaStore, TavernSessionStore, WorksFs,
};
use serde::Deserialize;
use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
};
use tower_http::{compression::CompressionLayer, cors::CorsLayer, services::ServeDir};
use tracing::info;

mod outline;
mod llm_stream;
mod archive_prompts;
mod llm_provider;
mod utf8_stream;
mod background;
mod st_import;
mod book_travel;
mod novel_api;
mod moa_api;
mod state;
mod auth_mw;
mod tickets;
mod error_map;
mod error_codes;
mod config;
mod routes_jobs;
mod routes_partner;
mod routes_works;
mod routes_chat;
mod routes_auth;
mod routes_embed;
mod chat_pipeline;
pub(crate) use state::{AppState, ChatStreamEvent, StreamHub};
pub(crate) use auth_mw::{auth_middleware, extract_bearer};
pub(crate) use auth_mw::{session_from, admin_session_from, session_from_any};
pub(crate) use tickets::sse_ticket_endpoint;
pub(crate) use error_map::map_core_err;
pub(crate) use routes_jobs::stream_job_sse;
pub(crate) use routes_partner::{vector_settings_from_value, vector_query_text, resolve_wb_ids_for_prompt, collect_vector_hits};
pub(crate) use routes_partner::{api_search};
pub(crate) use routes_chat::{DEFAULT_STORY_AGENT_PROMPT};
pub(crate) use routes_auth::sessions_prune;
pub(crate) use routes_embed::embeddings_openai;

/// Upstream `defaultStoryAgentPrompt` (v0.9.2 useSettingsStore) — DM/跑团 system base.
mod crawler;
mod chat_shelf;
// 剧本转换流水线第一步：角色蒸馏（LLM + 向量检索）
mod convert;
// t4-agent-tools (S5-W1): read/list/write/bash under data_root jail
mod agent_tools;
// S5-W2 T4: agent sessions CRUD + constrained tool loop
mod agent_sessions;
// S7-W3 T1: session-scoped todos GET/PUT + tools/todo
mod agent_todo;
// S5-W2 T5/T6/T7/T8
mod skills;
// P4 (吞噬 denova 创作 Skill 层): 写作 Skill 运行时装载器
mod skill_layer;
mod deai;
mod stats;
mod st_export;
// S5-W3: file versions + real LLM connectivity probe
mod versions;
mod llm_test;
// P7: import-safety preview endpoint
mod import_scan;
// P7: import-safety preview endpoint
mod encoding_sniff;
// S7-W2: style presets + works extensions
mod style_presets;
mod works_ext;
mod user_app_state;
mod dialogue_tavern;
// P1: character relationship graph API
mod graph;
// P2: chapter outlines + foreshadows API
mod foreshadow;
mod analysis;
mod relation_evolution;
mod ai_admin;
// U3: 场记卡（每场摘要持久化 + 资料抽屉「场记」视图）
mod scene_cards;
mod character_card;
// Morphling: Decision Cards + Dynamic Panels (Liyuan-inspired)
mod decisions;
mod panels;
mod appearance;
// Author Zone AZ-1
mod author;
// U12: 双 Agent 分工与工作流（Goethe 规划 → Dante 写作）
mod dual_agent;
mod embed_local;
// Story Tavern ST-0: packs / tavern sessions / persona
mod story_tavern;
// U4: 审稿闭环（T1 创作质量）——触发审稿 / 历史 / 逐条修复复查
mod review_tavern;
mod reference_library;
// Tavern MCP 外设（吸收自 Liyuan mcp.ts：本机 MCP server 工具源, 默认仅本机）
mod tavern_mcp;
// 生图 + TTS 工具端点（uniapi cogview-4 / edge-tts 等本机渠道）
mod kaleido_tools;
mod asr;
// U10: 图像管线消费模块（bookcover / illustration / lore-image / image-presets）
mod image_pipeline;
// U14-M1: 文档评审锚点系统（段落级锚点 + 评论/反馈）
mod book_annotations;
// U14-M2: BookRegistry 书架 API（服务端结构化 Registry）
mod bookshelf_registry;
// U14-M3: 书籍导出管线（TXT/UTF-8 手稿导出）
mod book_export;
// Harness P3: self-evolving refine 接线（LlmClient 适配 + 闭环 + REST 网关）
mod harness_bridge;
mod harness_api;

#[derive(Deserialize)]
struct ChatStartRequest {
    message: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    system: Option<String>,
}

#[derive(Deserialize)]
struct TitleUpdate {
    title: String,
}

#[derive(Deserialize)]
struct StopPayload {
    #[serde(alias = "runId")]
    run_id: String,
}





async fn root_redirect() -> impl IntoResponse {
    // serve SPA index if present
    (
        StatusCode::FOUND,
        [(header::LOCATION, "/web/")],
        "redirect to /web/",
    )
}

/// Rename compat (museai→kaleido): at process start, copy every set MUSEAI_*
/// var into the corresponding unset KALEIDO_* var. All later reads of
/// KALEIDO_* (including DataRoot::from_env, AuthStore, embed_local, …) then
/// work unchanged against a legacy env file. KALEIDO_* keeps priority.
fn seed_legacy_env_aliases() {
    for (k, v) in std::env::vars() {
        if let Some(suffix) = k.strip_prefix("MUSEAI_") {
            let target = format!("KALEIDO_{suffix}");
            if std::env::var_os(&target).is_none() {
                std::env::set_var(&target, &v);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    seed_legacy_env_aliases();
    tracing_subscriber::fmt()
        .with_env_filter(
            env::var("RUST_LOG")
                .unwrap_or_else(|_| "kaleido_server=info,tower_http=info,kaleido_core=info".into()),
        )
        .init();

    let data = DataRoot::from_env()?;
    let auth = AuthStore::load(data.clone())?;
    let jobs = JobStore::new(data.clone());
    // P0-3(审计): 启动时裁剪 jobs 目录（保留最新 500 个终态 job 文件，防无界增长）。
    jobs.prune_terminal(500);
    let sessions = AgentSessionStore::new(data.clone());
    let app_state = AppStateStore::new(data.clone());
    let partner = PartnerStore::new(app_state.clone());
    let works = WorksFs::new(data.clone());
    let packs = PackStore::new(data.clone());
    let search = kaleido_core::hybrid_search::SearchIndex::new(data.clone())?;
    let sessions_tavern = TavernSessionStore::new(data.clone());
    let personas = TavernPersonaStore::new(data.clone());
    story_tavern::bootstrap_demo(&packs);

    // Prefer crate-relative web/, then $KALEIDO_DATA/web, then cwd web/
    let web_dir = env::var("KALEIDO_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let candidates = [
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web"),
                data.root().join("web"),
                PathBuf::from("./web"),
            ];
            candidates
                .into_iter()
                .find(|p| p.join("index.html").exists())
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web"))
        });
    // ensure web dir exists
    let _ = std::fs::create_dir_all(&web_dir);

    let regex_library = kaleido_core::RegexLibraryStore::new(&data);
    let vector_index = kaleido_core::VectorIndexStore::new(&data);
    let reviews = kaleido_core::ReviewStore::new(data.clone());
    let mut state = AppState {
        auth,
        jobs,
        sessions,
        app_state,
        partner,
        works,
        packs,
        search,
        sessions_tavern,
        personas,
        regex_library,
        vector_index,
        reviews,
        hub: Arc::new(StreamHub::new()),
        llm_base: env::var("LLM_BASE_URL").ok(),
        llm_key: env::var("LLM_API_KEY").ok(),
        llm_model: env::var("LLM_MODEL").unwrap_or_else(|_| "deepseek-v4-flash-free".into()),
        provider_kind: env::var("KALEIDO_LLM_PROVIDER").unwrap_or_else(|_| "OpenAI".into()),
        embedding_base: env::var("EMBEDDING_BASE_URL").ok().or_else(|| Some("http://127.0.0.1:20145".into())),
        image_base_url: env::var("IMAGE_BASE_URL").ok().or_else(|| Some("http://127.0.0.1:18998/v1".into())),
        image_api_key: env::var("IMAGE_API_KEY").ok(),
        image_model: env::var("IMAGE_MODEL").unwrap_or_else(|_| "cogview-4".into()),
        cf_image_base_url: env::var("CF_IMAGE_BASE_URL").ok().or_else(|| Some("http://127.0.0.1:4001/v1".into())),
        cf_image_model: env::var("CF_IMAGE_MODEL").ok().or_else(|| Some("@cf/black-forest-labs/flux-1-schnell".into())),
        grok2api_image_base_url: env::var("GROK2API_IMAGE_BASE_URL").ok(),
        grok2api_image_key: env::var("GROK2API_IMAGE_KEY").ok(),
        grok2api_image_model: env::var("GROK2API_IMAGE_MODEL").ok(),
        plugin_registry: Arc::new(kaleido_core::plugin::PluginRegistry::new()),
        world_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        weaver_config: kaleido_core::memory_weaver::WeaverConfig::default(),
        graph: kaleido_core::graph_store::GraphStore::open(&data.root().join("graph.sqlite"))
            .expect("open graph store"),
        foreshadow: kaleido_core::foreshadow_store::ForeshadowStore::open(&data.root().join("plot.sqlite"))
            .expect("open foreshadow store"),
        analysis: kaleido_core::analysis_store::AnalysisStore::open(
            &data.root().join("plot.sqlite"),
        )
        .expect("open analysis store"),
        ai_admin: kaleido_core::ai_admin_store::AiAdminStore::open(
            &data.root().join("plot.sqlite"),
        )
        .expect("open ai admin store"),
        scene_cards: kaleido_core::scene_card_store::SceneCardStore::open(&data.root().join("plot.sqlite"))
            .expect("open scene card store"),
        reference_library: crate::reference_library::ReferenceLibraryStore::new(data.root()),
        rpm: crate::ai_admin::RpmLimiter::new(),
        director_tasks: std::sync::Arc::new(kaleido_core::DirectorTaskGroup::new()),
    };

    // 重启恢复调度：把磁盘上 running/queued 的非终态 job 分类处理。
    // 1) shelf_distil_world：有 checkpoint 幂等续跑能力 → rearm 后重新交给后台执行体续跑。
    // 2) 其他 kind（chat/story_turn/book_travel/analysis/import…）：进程重启后 worker 已死
    //    （tokio spawn 不跨进程、LLM 流已断），无法续跑 → 启动时直接标 failed 释放并发槽位，
    //    否则孤儿 running job 永远占满 max_concurrent_jobs，新回合 try_start 排队超时（幽灵 job）。
    //    story_turn 由 U11 提交时兜底（用户重发消息走新 run），旧记录 failed 不冲突。
    // recover_hook 只收 JobRecord，真正的 spawn 由 AppState 侧决定（kaleido-core 不依赖 server）。
    {
        let state_for_hook = state.clone();
        let recover_hook: Arc<dyn Fn(&JobRecord) + Send + Sync> =
            Arc::new(move |rec: &JobRecord| {
                if rec.kind != "shelf_distil_world" {
                    // 孤儿 job 清理：非终态 → failed 终态，释放 running/queued 占用的槽位。
                    if kaleido_core::is_active_job_status(&rec.status) {
                        let _ = state_for_hook.jobs.complete(
                            &rec.run_id,
                            "failed",
                            None,
                            Some("服务重启：孤儿 job 无法续跑，已清理释放并发槽位".into()),
                        );
                    }
                    return;
                }
                if !kaleido_core::is_active_job_status(&rec.status) {
                    return;
                }
                let st = state_for_hook.clone();
                let run_id = rec.run_id.clone();
                // 先复位为 queued（不计入 running 并发上限）并打恢复提示，再调度执行体。
                let _ = st.jobs.rearm_interrupted(&run_id, "服务重启，任务恢复续跑");
                let slug = rec
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("slug"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if slug.is_empty() {
                    let _ = st
                        .jobs
                        .complete(&run_id, "failed", None, Some("恢复续跑失败：缺少 slug".into()));
                    return;
                }
                let title = rec
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("title"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let resume_meta = rec.payload.clone();
                tokio::spawn(async move {
                    crate::crawler::exec_shelf_distil_world(
                        st,
                        run_id,
                        slug,
                        title,
                        resume_meta,
                    )
                    .await;
                });
            });
        state.jobs.set_recover_hook(recover_hook);
        state.jobs.dispatch_recovered();
    }

    let api = Router::new()
                                                                .route("/api/v1/sessions/prune", post(sessions_prune))
                        // T7 novel_workflow 接线 — PlanningGate / RevisionPhases / KnowledgeState / ForeshadowLedger
        .route("/api/v1/novel/planning/new", post(novel_api::planning_new))
        .route("/api/v1/novel/planning/advance", post(novel_api::planning_advance))
        .route("/api/v1/novel/planning/{gate_id}", get(novel_api::planning_get))
        .route("/api/v1/novel/revision/new", post(novel_api::revision_new))
        .route("/api/v1/novel/revision/check", post(novel_api::revision_check))
        .route("/api/v1/novel/revision/next", post(novel_api::revision_next))
        .route("/api/v1/novel/revision/{gate_id}", get(novel_api::revision_get))
        .route("/api/v1/novel/knowledge/scene", post(novel_api::knowledge_scene))
        .route("/api/v1/novel/knowledge/check", post(novel_api::knowledge_check))
        .route("/api/v1/novel/knowledge/{ledger_id}", get(novel_api::knowledge_get))
        .route("/api/v1/novel/foreshadow/plant", post(novel_api::foreshadow_plant))
        .route("/api/v1/novel/foreshadow/resolve", post(novel_api::foreshadow_resolve))
        .route("/api/v1/novel/foreshadow/{ledger_id}", get(novel_api::foreshadow_get))
        // S5-W2 T1 Story 跑团 — start/stop/stream (reuses mobile SSE hub, kind=story)
                                // 剧情助手（story/冒险/跑团版）：上下文来自客户端传入的剧情消息
        .route("/api/v1/story/assistant", post(story_tavern::story_assistant_chat))
        // jobs v2 (W0) — list/create + detail/cancel/stream; GET by id keeps chat run_id compat
        // jobs v2 (W0) — extracted to routes_jobs.rs (P0-1); nested at /api/v1/jobs
        // (fix: was .merge() with relative paths → runtime route-overlap panic)
        .nest("/api/v1/jobs", routes_jobs::router())
        // partner/settings/vector/regex — extracted (P0-1 Stage3)
        .merge(routes_partner::router())
        // chat/story + mobile compat (P0-1 Stage4)
        .merge(routes_chat::router())
        // auth/health + embed (P0-1 Stage5)
        .merge(routes_auth::router())
        .merge(routes_embed::router())
        // works filesystem (P0-1 Stage4) — nested further below at /api/v1/works
        // (removed duplicate .merge() here: it re-registered "/" at root → runtime panic)
        // S5-W2 T2 background — multi-stage (stage_one|items|character_card) + start/stop/stream
        .route("/api/v1/background/start", post(background::start))
        .route("/api/v1/background/stop", post(background::stop))
        .route("/api/v1/background/apply", post(background::apply))
        .route("/api/v1/background/stream", get(background::stream))
        .route("/api/v1/background/runs/{id}", get(background::get_run))
        .route(
            "/api/v1/background/runs/{id}/resume",
            post(background::resume),
        )
        .route("/api/v1/background/{stage}", post(background::start_stage))
        // S5-W2 T3 book_travel — classify + multi-step (assemble/plan_scene/change/beat/judge/memory)
        .route("/api/v1/search", get(api_search))
        .route("/api/v1/book-travel/classify", post(book_travel::classify))
        .route("/api/v1/book-travel/start", post(book_travel::start))
        .route("/api/v1/book-travel/stop", post(book_travel::stop))
        .route("/api/v1/book-travel/stream", get(book_travel::stream))
        .route("/api/v1/book-travel/runs", get(book_travel::list_runs))
        .route("/api/v1/book-travel/runs/{id}", get(book_travel::get_run))
        .route("/api/v1/book-travel/runs/{id}/open-session", post(book_travel::open_session))
        .route("/api/v1/book-travel/pipeline", post(book_travel::start_pipeline))
        .route("/api/v1/book-travel/{step}", post(book_travel::start_step))
        // S3 partner + settings
                                
                                                                .route("/api/v1/crawler/fanqie", post(crawler::fanqie))
        .route("/api/v1/crawler/fanqie/meta", get(crawler::fanqie_meta))
        .route("/api/v1/crawler/fanqie/search", get(crawler::fanqie_search))
        .route("/api/v1/crawler/fanqie/progress", get(crawler::fanqie_progress))
        .route(
            "/api/v1/crawler/novels",
            get(crawler::novels_list)
                .post(crawler::novels_import)
                // TXT/MD 导入把全文放 JSON body：axum 默认 2MB 会拒大文件(413)，
                // 放宽到 64MB，让 handler 内的 20M 字符上限先于 413 生效。
                .layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/api/v1/crawler/novels/{slug}/content", get(crawler::novel_content))
        .route("/api/v1/crawler/novels/{slug}/cover", get(crawler::novel_cover))
        .route("/api/v1/crawler/novels/{slug}/to-pack", post(crawler::novel_to_pack))
        .route("/api/v1/crawler/novels/{slug}/distil", post(crawler::novel_distil))
        .route("/api/v1/crawler/novels/{slug}/distil-world", post(crawler::novel_distil_world))
        .route("/api/v1/crawler/novels/{slug}/export", get(crawler::novel_export))
        .merge(import_scan::router())
        .merge(chat_shelf::router())
        // t4-agent-tools mount (read/list/write/bash) — owner: t4
        .merge(agent_tools::router())
        // S5-W2 T4 agent sessions CRUD + constrained tool loop
        .merge(agent_sessions::router())
        .merge(agent_todo::router())
        .merge(skills::router())
        .merge(deai::router())
        .merge(stats::router())
        .merge(st_export::router())
        .merge(moa_api::routes())
        .merge(versions::router())
        .merge(llm_test::router())
        // Morphling: Decision Cards + Dynamic Panels (Liyuan-inspired)
        .merge(decisions::router())
        .merge(panels::router())
        // S4 works filesystem + W11 limits — extracted to routes_works.rs (P0-1 Stage4);
        // nested at /api/v1/works. The 3 inline routes that used to sit here
        // (/file with body-limit layer, /dir, /rename) now live in routes_works::router()
        // (fix: was .merge() → duplicate/overlapping routes at runtime)
        .nest("/api/v1/works", routes_works::router())
        // S7-W2 works extensions + style presets
        .merge(works_ext::router())
        .merge(user_app_state::router())
        .merge(graph::router())
        .merge(foreshadow::router())
        .merge(analysis::router())
        .merge(review_tavern::router())
        .merge(reference_library::router())
        .merge(dialogue_tavern::router())
        .merge(ai_admin::router())
        .merge(scene_cards::router())
        .merge(appearance::router())
        .merge(style_presets::router())
        // Author Zone AZ-1
        .merge(author::router())
                .route("/api/v1/embeddings", post(embeddings_openai))
        // 生图 + TTS 工具（uniapi cogview-4 / edge-tts）
        .route("/api/v1/kaleido-tools/image", post(crate::kaleido_tools::generate_image))
        .route("/api/v1/kaleido-tools/tts", post(crate::kaleido_tools::text_to_speech))
        .route("/api/v1/kaleido-tools/asr", post(crate::kaleido_tools::speech_to_text))
        // U10: 图像管线消费（bookcover / illustration / lore-image / presets）
        .merge(image_pipeline::router())
        // Story Tavern ST-0
        .merge(story_tavern::router())
        // Harness P3: self-evolving refine REST（/api/v1/harness/*）
        .merge(harness_api::router())
        // U12: 双 Agent 分工与工作流
        .merge(dual_agent::router())
        // U14-M1: 文档评审锚点系统
        .merge(book_annotations::router())
        // U14-M2: BookRegistry 书架 API
        .merge(bookshelf_registry::router())
        // U14-M3: 书籍导出管线
        .merge(book_export::router())
        // t3-outline reverse preview/save (MVP heuristic)
        .route("/api/v1/outline/reverse/preview", post(outline::preview_reverse))
        .route("/api/v1/outline/reverse/save", post(outline::save_reverse))
        .route("/api/v1/outline/reverse/analyze", post(outline::analyze_reverse))
        .route("/api/v1/outline/reverse/finalize", post(outline::finalize_reverse))
        // mobile compat (upstream runtime.ts)
                                                                                .route("/", get(root_redirect));

    let static_svc = ServeDir::new(&web_dir).append_index_html_on_directories(true);

    // M-5: CORS allow-list from env (comma-separated), defaults to localhost loopback only.
    let mut cors_origins: Vec<HeaderValue> = Vec::new();
    if let Ok(cfg) = env::var("KALEIDO_CORS_ORIGINS") {
        for origin in cfg.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Ok(hv) = origin.parse::<HeaderValue>() {
                cors_origins.push(hv);
            }
        }
    }
    if cors_origins.is_empty() {
        cors_origins = vec![
            "http://127.0.0.1:18766".parse::<HeaderValue>().unwrap(),
            "http://localhost:18766".parse::<HeaderValue>().unwrap(),
        ];
    }

    let app = api
        .nest_service("/web", static_svc)
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .layer(
            CorsLayer::new()
                .allow_origin(cors_origins)
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::PATCH, Method::OPTIONS])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        )
        .layer(CompressionLayer::new())
        .with_state(state.clone());

    let host = env::var("KALEIDO_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = env::var("KALEIDO_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(18766);
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    info!(%addr, "kaleido-server listening");
    info!(web=%web_dir.display(), "web shell dir");
    info!(data_root=%std::env::var("KALEIDO_DATA").unwrap_or_else(|_| "./data".into()), "data root");

    // [fix 2026-08-15 孤儿回合清扫] 服务启动即检查：重启打断的进行中回合
    // （active_run_id 残留 + job running 但 hub 无活 worker）→ 标记中断 + 释放锁，
    // 避免前端永久「生成中」。须在 listener bind 前、state 构建后调用。
    story_tavern::sweep_orphan_runs(&state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tokio::spawn(async {
        let r = tokio::task::spawn_blocking(|| crate::embed_local::ensure_local()).await;
        match r {
            Ok(Ok(())) => tracing::info!("embed_local warm ok"),
            Ok(Err(e)) => tracing::warn!(error=%e, "embed_local warm failed (remote fallback remains)"),
            Err(e) => tracing::warn!(error=%e, "embed_local warm join failed"),
        }
    });
    chat_shelf::spawn_schedule_tick(state.clone());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(graceful_shutdown_signal(state.clone()))
    .await?;
    Ok(())
}

/// [P5 2026-08-16] 优雅停机：收到 SIGTERM/SIGINT 后不立即退出——
/// 等待所有活跃回合（JobStore running/queued + tavern active_run_id）自然完成落盘，
/// 避免部署窗口杀掉进行中的 LLM 生成（宿醉「上一回合因服务重启而中断」根因）。
/// 等待窗口 180s（超过则强制退出，由 systemd TimeoutStopSec 兜底 SIGKILL）。
async fn graceful_shutdown_signal(state: AppState) {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "SIGTERM handler 注册失败，跳过优雅停机");
            return;
        }
    };
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "SIGINT handler 注册失败，跳过优雅停机");
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => tracing::warn!("收到 SIGTERM——进入优雅停机，等待活跃回合完成"),
        _ = int.recv() => tracing::warn!("收到 SIGINT——进入优雅停机，等待活跃回合完成"),
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        let jobs_active = state.jobs.running_count() + state.jobs.queued_count();
        let tavern_active = count_active_tavern_runs(&state.sessions_tavern);
        if jobs_active == 0 && tavern_active == 0 {
            tracing::info!("无活跃回合，安全退出");
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                jobs_active, tavern_active,
                "优雅停机等待超时(180s)，强制退出"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// 统计所有 tavern 会话中 active_run_id 非空的活跃回合数。
fn count_active_tavern_runs(store: &TavernSessionStore) -> usize {
    let Ok(sessions) = store.list() else {
        return 0;
    };
    sessions
        .iter()
        .filter(|s| {
            s.get("activeRunId")
                .or_else(|| s.get("active_run_id"))
                .and_then(|v| v.as_str())
                .map(|r| !r.is_empty())
                .unwrap_or(false)
        })
        .count()
}
