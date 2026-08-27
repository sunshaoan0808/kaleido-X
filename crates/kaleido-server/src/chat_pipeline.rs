//! Chat pipeline stages — extracted from start_chat_from_body (P0-1 follow-up)
use axum::response::Response;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::state::AppState;
use crate::error_codes::*;
use crate::DEFAULT_STORY_AGENT_PROMPT;
use crate::routes_partner::{vector_settings_from_value, vector_query_text, resolve_wb_ids_for_prompt, collect_vector_hits};
use kaleido_core::SessionRecord;

#[allow(dead_code)] // [P7] chat pipeline 上下文暂由 main 内联构造，结构体预留
pub(crate) struct PromptContext {
    pub req_val: Value,
    pub base: String,
    pub key: String,
    pub model: String,
    pub temperature: f64,
    pub max_tokens: u64,
    pub top_p: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub oai_messages: Vec<Value>,
    pub transcript: Vec<Value>,
    pub chat_id_for_wi: String,
    pub input_tokens_hint: i64,
}


#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // [P7] 同上——唯一调用方已内联化，保留防漂移
pub(crate) async fn build_prompt_context(
    state: &AppState,
    session: &SessionRecord,
    kind: &str,
    partner: &kaleido_core::PartnerStore,
    body: &str,
    llm_base_url: &str,
    llm_api_key: &str,
    llm_model_fallback: &str,
    llm_resolved_model: &str,
) -> Result<PromptContext, Response> {
    let mut req_val: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return Err(bad_request("CHPIPE_INVALID", format!("Invalid JSON: {e}")));
        }
    };

    // Server injects LLM credentials (strip client secrets)
    let model = req_val
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let m = llm_resolved_model.to_string();
            if m.is_empty() {
                llm_model_fallback.to_string()
            } else {
                m
            }
        });

    if let Some(obj) = req_val.as_object_mut() {
        obj.insert("modelInterface".into(), json!("OpenAI"));
        obj.insert("baseUrl".into(), json!(""));
        obj.insert("apiKey".into(), json!(""));
        obj.insert("model".into(), json!(model.clone()));
    }

    // Partner injection (S3): world book + character card into systemPrompt
    // Prefer client-provided systemPrompt only if it already contains partner markers;
    // otherwise assemble from settings + selected partner items.
    let client_system = req_val
        .get("systemPrompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let wb_id = req_val
        .get("worldBookId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let cc_id = req_val
        .get("characterCardId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let base_prompt = {
        let from_settings = state.app_state.partner_chat_prompt();
        if !client_system.trim().is_empty()
            && (client_system.contains("## 伴侣对话世界设定")
                || client_system.contains("## 你的角色人设设定"))
        {
            client_system.clone()
        } else if kind == "story" {
            // Story DM: client system wins if provided; else upstream defaultStoryAgentPrompt.
            if !client_system.trim().is_empty() {
                client_system.clone()
            } else {
                DEFAULT_STORY_AGENT_PROMPT.to_string()
            }
        } else if !client_system.trim().is_empty() && kind != "chat" {
            // other non-chat kinds: keep client system as base
            client_system.clone()
        } else {
            from_settings
        }
    };
    let messages = req_val
        .get("messages")
        .cloned()
        .unwrap_or_else(|| json!([]));

    // Oldest-first (role, content) for ST World Info scan depth buffer
    let mut chat_pairs: Vec<(String, String)> = Vec::new();
    if let Some(arr) = messages.as_array() {
        for m in arr {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() {
                continue;
            }
            chat_pairs.push((role.to_string(), content.to_string()));
        }
    }

    // Update world state with the last user message (audit P0#3: 按 workspace 分片)
    if let Some((_, ref content)) = chat_pairs.last().filter(|(r, _)| r == "user") {
        use kaleido_core::world_state::WorldEvent;
        let we = WorldEvent::NarrativeEvent {
            summary: content.clone(),
            character_ids: vec![],
            turn: 0,
        };
        if let Ok(mut ws) = state.world_state.lock() {
            ws.entry(session.workspace_id.clone())
                .or_default()
                .apply(we);
        }
    }

    // Optional client WI settings override
    let wi_settings = req_val.get("worldInfoSettings").cloned().and_then(|v| {
        serde_json::from_value::<kaleido_core::WiSettings>(v).ok()
    });
    // Chat/session id for timed WI persistence (sticky/cooldown)
    let chat_id_for_wi = req_val
        .get("sessionId")
        .or_else(|| req_val.get("session_id"))
        .or_else(|| req_val.get("chatId"))
        .or_else(|| req_val.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let timed_store = kaleido_core::TimedWorldInfoStore::new(state.auth.data_root());
    let timed_in = if chat_id_for_wi.is_empty() {
        None
    } else {
        Some(timed_store.load(&chat_id_for_wi))
    };
    let mut wi_scan_ctx = req_val
        .get("worldInfoScanContext")
        .cloned()
        .and_then(|v| serde_json::from_value::<kaleido_core::WiScanContext>(v).ok())
        .unwrap_or_default();
    if wi_scan_ctx.trigger.is_empty() {
        // map job kind → ST-ish trigger
        wi_scan_ctx.trigger = match kind {
            "story" => "normal".into(),
            "chat" => "normal".into(),
            _ => "normal".into(),
        };
        if req_val.get("continue").and_then(|v| v.as_bool()).unwrap_or(false)
            || req_val.get("isContinue").and_then(|v| v.as_bool()).unwrap_or(false)
        {
            wi_scan_ctx.trigger = "continue".into();
        }
    }
    let max_ctx_tokens = req_val
        .get("maxContextTokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(8192) as i32;
    // W5: fill vector hits for vectorized WI entries before scan
    {
        let vset = vector_settings_from_value(
            req_val
                .get("vectorSettings")
                .or_else(|| req_val.get("worldInfoVectorSettings")),
        );
        let depth = req_val
            .get("worldInfoSettings")
            .and_then(|w| w.get("depth"))
            .and_then(|d| d.as_i64())
            .unwrap_or(2) as i32;
        let qtext = vector_query_text(&chat_pairs, depth);
        let wb_ids = resolve_wb_ids_for_prompt(&partner, wb_id, cc_id);
        let (hits, verr) = collect_vector_hits(&state, &wb_ids, &qtext, &vset);
        if let Some(e) = verr {
            warn!(error=%e, "W5 vector path skipped");
        }
        if !hits.is_empty() {
            info!(hits = hits.len(), "W5 vector hits for WI scan");
        }
        wi_scan_ctx.vector_hits = hits;
        wi_scan_ctx.vector_settings = Some(vset);
    }
    let mut wi_message_injections: Vec<serde_json::Value> = Vec::new();

    let (system_prompt, gen_meta) = if kind == "chat"
        || kind == "story"
        || client_system.contains("## 伴侣对话世界设定")
        || client_system.contains("## 世界书")
        || wb_id.is_some()
        || cc_id.is_some()
    {
        match partner.build_generation_prompt_full(
            &base_prompt,
            wb_id,
            cc_id,
            &chat_pairs,
            wi_settings,
            timed_in,
            false,
            Some(wi_scan_ctx.clone()),
            max_ctx_tokens,
        ) {
            Ok(r) => {
                if !r.wi.activated.is_empty() {
                    info!(
                        activated = r.wi.activated.len(),
                        regex = r.regex_script_count,
                        overflowed = r.wi.overflowed,
                        skipped_vec = r.wi.skipped_vectorized,
                        "ST world-info scan"
                    );
                }
                if let Some(ref tw) = r.timed_world_info {
                    if !chat_id_for_wi.is_empty() {
                        if let Err(e) = timed_store.save(&chat_id_for_wi, tw) {
                            warn!(error=%e, chat_id=%chat_id_for_wi, "timed WI save failed");
                        }
                    }
                }
                wi_message_injections = r.message_injections.clone();
                // W7: persist automation trigger log on real generation
                if !r.automation_ids.is_empty() {
                    let detailed: Vec<(String, String, String, String, String)> = r
                        .wi
                        .activated
                        .iter()
                        .filter(|a| !a.automation_id.trim().is_empty())
                        .map(|a| {
                            (
                                a.automation_id.clone(),
                                a.uid.clone(),
                                a.world.clone(),
                                a.comment.clone(),
                                a.reason.clone(),
                            )
                        })
                        .collect();
                    let auto_store =
                        kaleido_core::AutomationTriggerStore::new(state.auth.data_root());
                    if !detailed.is_empty() {
                        let _ = auto_store.record_detailed(
                            &detailed,
                            &chat_id_for_wi,
                            "partner_chat",
                        );
                    } else {
                        let _ = auto_store.record(
                            &r.automation_ids,
                            &[],
                            &chat_id_for_wi,
                            "partner_chat",
                        );
                    }
                }
                let meta = json!({
                    "wiActivated": r.wi.activated.len(),
                    "wiBudgetTokens": r.wi.budget_tokens,
                    "wiOverflowed": r.wi.overflowed,
                    "regexScripts": r.regex_script_count,
                    "anBefore": !r.wi.an_before.is_empty(),
                    "anAfter": !r.wi.an_after.is_empty(),
                    "depthEntries": r.wi.depth_entries.len(),
                    "emBefore": !r.wi.em_before.is_empty(),
                    "emAfter": !r.wi.em_after.is_empty(),
                    "outlets": r.wi.outlet_entries.len(),
                    "chatInjections": r.message_injections.len(),
                    "skippedVectorized": r.wi.skipped_vectorized,
                    "vectorActivated": r.wi.vector_activated,
                    "skippedFilter": r.wi.skipped_filter,
                    "skippedTrigger": r.wi.skipped_trigger,
                    "automationIds": r.automation_ids,
                    "exampleMessages": r.example_messages.len(),
                    "timedSticky": r.timed_world_info.as_ref().map(|t| t.sticky.len()).unwrap_or(0),
                    "timedCooldown": r.timed_world_info.as_ref().map(|t| t.cooldown.len()).unwrap_or(0),
                    "wiEntries": r.wi.activated.iter().map(|a| json!({
                        "uid": a.uid,
                        "world": a.world,
                        "comment": a.comment,
                        "reason": a.reason,
                        "order": a.order,
                        "position": a.position,
                    })).collect::<Vec<_>>(),
                });
                (r.system_prompt, meta)
            }
            Err(e) => {
                warn!(error=%e, "partner generation prompt failed; using base");
                (base_prompt, json!({}))
            }
        }
    } else if client_system.trim().is_empty() {
        ("You are Kaleido partner chat assistant.".into(), json!({}))
    } else {
        (client_system, json!({}))
    };

    let settings = state.app_state.load_settings_public().ok();
    let temperature = req_val
        .get("temperature")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.temperature))
        .unwrap_or(0.7);
    let max_tokens = req_val
        .get("maxOutputTokens")
        .and_then(|v| v.as_u64())
        .or_else(|| settings.as_ref().and_then(|s| s.max_output_tokens))
        .unwrap_or(4096);
    let top_p = req_val
        .get("topP")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.top_p));
    let frequency_penalty = req_val
        .get("frequencyPenalty")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.frequency_penalty));
    let presence_penalty = req_val
        .get("presencePenalty")
        .and_then(|v| v.as_f64())
        .or_else(|| settings.as_ref().and_then(|s| s.presence_penalty));

    // Global library ∪ character-scoped regex (W6) for prompt-path rewrites
    let regex_scripts = {
        let card_fields = cc_id.and_then(|id| {
            partner.load().ok().and_then(|pst| {
                pst.character_cards
                    .into_iter()
                    .find(|c| c.id == id)
                    .and_then(|c| c.fields)
            })
        });
        kaleido_core::resolve_runtime_scripts(&state.regex_library, card_fields.as_ref())
    };

    // Inject world state narrative summary into system prompt (audit P0#3: 仅本 workspace)
    let system_prompt = if let Ok(ws) = state.world_state.lock() {
        let summary = ws.get(&session.workspace_id).map(|s| s.narrative_summary());
        if let Some(summary) = summary {
            if !summary.is_empty() {
                format!("{}\n\n## Current World State\n{}", system_prompt, summary)
            } else {
                system_prompt
            }
        } else {
            system_prompt
        }
    } else {
        system_prompt
    };

    // Build OpenAI messages
    let mut oai_messages = vec![json!({"role":"system","content": system_prompt})];
    let mut transcript: Vec<serde_json::Value> = Vec::new();
    if let Some(arr) = messages.as_array() {
        let n = arr.len();
        for (i, m) in arr.iter().enumerate() {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() && role != "assistant" {
                continue;
            }
            let depth = (n - 1 - i) as i32;
            let placement = if role == "user" {
                kaleido_core::RegexPlacement::UserInput
            } else {
                kaleido_core::RegexPlacement::AiOutput
            };
            let content = kaleido_core::get_regexed_string(
                content,
                placement,
                &regex_scripts,
                false,
                true,
                Some(depth),
            );
            transcript.push(json!({"role": role, "content": content}));
        }
    }
    // ST-style: insert WI depth/outlet/EM injections into transcript by depth
    // depth 0 → just before the last message (or append if empty)
    if !wi_message_injections.is_empty() {
        // sort by depth descending so earlier inserts don't shift later indices wrongly
        let mut inj = wi_message_injections.clone();
        inj.sort_by(|a, b| {
            let da = a.get("depth").and_then(|v| v.as_i64()).unwrap_or(0);
            let db = b.get("depth").and_then(|v| v.as_i64()).unwrap_or(0);
            db.cmp(&da)
        });
        for item in inj {
            let depth = item.get("depth").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("system");
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() { continue; }
            let msg = json!({"role": role, "content": content, "wiSlot": true});
            // insert so that `depth` messages remain after this injection from the end
            let idx = if transcript.len() > depth {
                transcript.len() - depth
            } else {
                0
            };
            transcript.insert(idx, msg);
        }
    }
    oai_messages.extend(transcript);

    // Weaver context compaction: if total tokens exceed threshold, compact older messages
    {
        use kaleido_core::memory_weaver::{self, MessageStats};
        let total_est: usize = oai_messages[1..] // skip system message
            .iter()
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
            .map(memory_weaver::estimate_tokens)
            .sum();
        if memory_weaver::should_weave(total_est, &state.weaver_config) {
            let msg_stats: Vec<MessageStats> = oai_messages[1..]
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user").to_string();
                    let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    MessageStats {
                        index: i,
                        role,
                        char_count: content.len(),
                        estimated_tokens: memory_weaver::estimate_tokens(content),
                        engine_tag: None,
                    }
                })
                .collect();
            if let Some(cut_idx) = memory_weaver::find_cut_point(&msg_stats, state.weaver_config.keep_recent_tokens) {
                let oai_cut = cut_idx + 2; // +1 for system at idx 0, +1 because find_cut_point returns last compacted index
                if oai_cut > 1 && oai_cut < oai_messages.len() {
                    let compacted_count = cut_idx + 1;
                    let retained_tokens: usize = msg_stats[cut_idx..]
                        .iter()
                        .map(|s| s.estimated_tokens)
                        .sum();
                    info!(
                        compacted = compacted_count,
                        retained = oai_messages.len() - oai_cut,
                        total_before = total_est,
                        retained_tokens = retained_tokens,
                        "weaver context compaction"
                    );
                    // S7: archive compacted messages into session-level vector index BEFORE dropping them
                    // (history vector recall — P1-1). Best-effort; failure only logs.
                    if !chat_id_for_wi.is_empty() {
                        let sess_key = format!("sess-{chat_id_for_wi}");
                        let archived: Vec<(String, String)> = oai_messages[1..oai_cut]
                            .iter()
                            .filter_map(|m| {
                                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("").trim();
                                if content.is_empty() || content.starts_with('[') {
                                    return None;
                                }
                                let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");
                                let text = format!("{role}: {content}");
                                let uid = format!(
                                    "m-{}-{}",
                                    role,
                                    kaleido_core::text_hash(&text).chars().take(10).collect::<String>()
                                );
                                Some((uid, text))
                            })
                            .collect();
                        if !archived.is_empty() {
                            let texts: Vec<String> = archived.iter().map(|(_, t)| t.clone()).collect();
                            match tokio::task::spawn_blocking(move || crate::embed_local::embed_many(&texts))
                                .await
                            {
                                Ok(Ok(embeds)) => {
                                    let mut idx = state.vector_index.load(&sess_key);
                                    let known: std::collections::HashSet<String> =
                                        idx.entries.iter().map(|e| e.uid.clone()).collect();
                                    let mut added = 0usize;
                                    for ((uid, text), v) in archived.iter().zip(embeds.iter()) {
                                        if !known.contains(uid) && !v.is_empty() {
                                            idx.entries.push(kaleido_core::VectorIndexEntry {
                                                uid: uid.clone(),
                                                world: "history".into(),
                                                text: text.clone(),
                                                text_hash: kaleido_core::text_hash(text),
                                                vector: v.clone(),
                                            });
                                            added += 1;
                                        }
                                    }
                                    if added > 0 {
                                        match state.vector_index.save(idx) {
                                            Ok(f) => info!(sess = %chat_id_for_wi, entries = f.entries.len(), "S7 history archived to vector index"),
                                            Err(e) => warn!(error = %e, "S7 vector archive save failed"),
                                        }
                                    }
                                }
                                Ok(Err(e)) => warn!(error = %e, "S7 embed_many failed"),
                                Err(e) => warn!(error = %e, "S7 embed join failed"),
                            }
                        }
                    }
                    oai_messages.drain(1..oai_cut);
                    oai_messages.insert(1, json!({
                        "role": "system",
                        "content": format!(
                            "[{} earlier messages compacted. Retaining recent ~{} tokens.]",
                            compacted_count, retained_tokens
                        )
                    }));
                }
            }
        }
    }

    // S7 (P1-1): history vector recall — query session vector index with recent context,
    // inject top hits back into the prompt so compacted details are recoverable.
    if !chat_id_for_wi.is_empty() {
        let sess_key = format!("sess-{chat_id_for_wi}");
        let idx = state.vector_index.load(&sess_key);
        if !idx.entries.is_empty() {
            let qtext: Vec<String> = oai_messages
                .iter()
                .rev()
                .take(4)
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()))
                .filter(|c| !c.trim().is_empty() && !c.starts_with('['))
                .collect();
            let qtext = qtext.join("\n");
            if !qtext.trim().is_empty() {
                let qtext2 = qtext.clone();
                match tokio::task::spawn_blocking(move || crate::embed_local::embed_one(&qtext2)).await {
                    Ok(Ok(qv)) => {
                        let vset = kaleido_core::VectorActivationSettings {
                            enabled: true,
                            score_threshold: 0.42,
                            top_k: 4,
                        };
                        let hits = kaleido_core::rank_hits(&idx, &qv, &vset);
                        if !hits.is_empty() {
                            let lines: Vec<String> = hits
                                .iter()
                                .filter_map(|h| {
                                    idx.entries
                                        .iter()
                                        .find(|e| e.uid == h.uid)
                                        .map(|e| e.text.clone())
                                })
                                .collect();
                            if !lines.is_empty() {
                                let recall = format!(
                                    "【历史回忆·向量检索命中】\n{}",
                                    lines.join("\n\n")
                                );
                                oai_messages.insert(1, json!({
                                    "role": "system",
                                    "content": recall,
                                    "s7Recall": true,
                                }));
                                info!(sess = %chat_id_for_wi, hits = hits.len(), "S7 history recall injected");
                            }
                        }
                    }
                    Ok(Err(e)) => warn!(error = %e, "S7 recall embed failed"),
                    Err(e) => warn!(error = %e, "S7 recall join failed"),
                }
            }
        }
    }

    let _gen_meta = gen_meta;

    // P5 usage recording: capture managed-provider identity + estimated input
    // tokens (only recorded when the runtime resolved through ai_admin).
    let input_est = oai_messages
        .iter()
        .map(|m| kaleido_core::memory_weaver::estimate_tokens(m.get("content").and_then(|c| c.as_str()).unwrap_or("")))
        .sum::<usize>() as i64;
    Ok(PromptContext {
        req_val,
        base: llm_base_url.to_string(),
        key: llm_api_key.to_string(),
        model, temperature, max_tokens, top_p,
        frequency_penalty, presence_penalty,
        oai_messages,
        transcript: Vec::new(), // merged into oai_messages at L430 (extend)
        chat_id_for_wi, input_tokens_hint: input_est,
    })
}
