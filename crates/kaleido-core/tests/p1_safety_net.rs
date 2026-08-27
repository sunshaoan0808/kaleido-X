//! P1 safety net: AuthStore session lifecycle (TTL / cap / prune),
//! PartnerStore::build_generation_prompt_full prompt assembly,
//! st_world_info macro/regex edge cases.
//!
//! Complements p0_core_stores (auth flow/ratelimit, job queue) and
//! p0_partner_prompt (state roundtrip + wb id resolution).
use kaleido_core::{AppStateStore, DataRoot, PartnerItem, PartnerStore, WiSettings};
use serde_json::json;

fn temp_root(tag: &str) -> DataRoot {
    let dir = std::env::temp_dir().join(format!(
        "kaleido-snet-{tag}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let root = DataRoot::new(dir).expect("root");
    root.ensure_layout().expect("layout");
    root
}

fn admin_env() {
    std::env::set_var("KALEIDO_ADMIN_USER", "admin");
    std::env::set_var("KALEIDO_ADMIN_PASSWORD", "snet-pass-123");
}

/// Process-wide env (KALEIDO_ADMIN_PASSWORD etc.) is shared by parallel test
/// threads; every test that touches admin env vars must hold this lock.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ===========================================================================
// AuthStore: session lifecycle
// ===========================================================================

#[test]
fn auth_expired_session_is_rejected_and_gcd() {
    let root = temp_root("ttl");
    let _env = ENV_LOCK.lock().unwrap();
    admin_env();
    let store = kaleido_core::AuthStore::load(root.clone()).expect("auth");

    let sess = store.login("admin", "snet-pass-123", "ip:ttl").expect("login");
    assert!(store.resolve_session(&sess.token).is_ok());

    // Force-expire by rewriting sessions.json with a past timestamp.
    let path = root.state_file("sessions.json");
    let mut map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("read sessions"),
    )
    .expect("map");
    {
        let rec = map.get_mut(&sess.token).expect("session record");
        // RFC3339 far in the past
        rec["expires_at"] = json!("2000-01-01T00:00:00Z");
    }
    std::fs::write(&path, serde_json::to_string(&map).unwrap()).unwrap();

    // In-memory sessions were loaded before the edit — reload from disk
    // (load_sessions drops expired entries at load time).
    let store2 = kaleido_core::AuthStore::load(root.clone()).expect("reload");

    // resolve must fail; the dead session was already dropped from memory at load
    // (load_sessions GC). Disk file keeps it until the next persist — that's the
    // documented behavior; assert the memory-level contract here.
    let err = store2
        .resolve_session(&sess.token)
        .err()
        .expect("expired must error");
    assert!(matches!(err, kaleido_core::CoreError::Auth(_)));
    // a fresh login persists its map without the dead token (lazy disk GC)
    let _fresh = store2.login("admin", "snet-pass-123", "ip:ttl2").expect("fresh");
    let map2: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap(),
    )
    .unwrap();
    assert!(!map2.contains_key(&sess.token), "expired token must be gone after next persist");
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
}

#[test]
fn auth_prune_expired_reports_count() {
    let root = temp_root("prune-exp");
    let _env = ENV_LOCK.lock().unwrap();
    admin_env();
    let store = kaleido_core::AuthStore::load(root.clone()).expect("auth");

    let s1 = store.login("admin", "snet-pass-123", "ip:p1").expect("s1");
    let s2 = store.login("admin", "snet-pass-123", "ip:p2").expect("s2");

    // expire only s1 on disk
    let path = root.state_file("sessions.json");
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    map[&s1.token]["expires_at"] = json!("2000-01-01T00:00:00Z");
    std::fs::write(&path, serde_json::to_string(&map).unwrap()).unwrap();

    // reload → load_sessions drops expired at load, so prune sees none left;
    // the disk file still holds it until the next persist. Assert the
    // observable contract instead: after reload, expired token is invalid
    // and stats show exactly one active session.
    let store2 = kaleido_core::AuthStore::load(root.clone()).expect("reload");
    assert!(store2.resolve_session(&s1.token).is_err(), "expired must be rejected");
    assert!(store2.resolve_session(&s2.token).is_ok());
    let n = store2.prune_expired_sessions().expect("prune");
    assert_eq!(n, 0, "load already GC'd the expired session");
    let stats = store2.session_stats();
    assert_eq!(stats.active, 1);
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
}

#[test]
fn auth_session_cap_auto_evict_frees_slot_for_new_login() {
    let root = temp_root("cap-evict");
    let _env = ENV_LOCK.lock().unwrap();
    std::env::set_var("KALEIDO_ADMIN_USER", "admin");
    std::env::set_var("KALEIDO_ADMIN_PASSWORD", "cap-pass-999");
    // cap=1 via settings-store override
    let st = AppStateStore::new(root.clone());
    st.save("settings-store", r#"{"sessionMax": 1}"#).expect("cap");
    let store = kaleido_core::AuthStore::load(root.clone()).expect("auth");

    let first = store.login("admin", "cap-pass-999", "ip:c1").expect("first");
    // second login at cap with auto_evict (default policy) → evicts oldest, succeeds
    let second = store.login("admin", "cap-pass-999", "ip:c2").expect("second");
    // oldest (first) is gone
    assert!(store.resolve_session(&first.token).is_err(), "evicted session must be invalid");
    assert!(store.resolve_session(&second.token).is_ok());
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
}

#[test]
fn auth_session_cap_reject_mode_returns_session_cap_error() {
    let root = temp_root("cap-reject");
    let _env = ENV_LOCK.lock().unwrap();
    std::env::set_var("KALEIDO_ADMIN_USER", "admin");
    std::env::set_var("KALEIDO_ADMIN_PASSWORD", "rej-pass-777");
    let st = AppStateStore::new(root.clone());
    st.save(
        "settings-store",
        r#"{"sessionMax": 1, "sessionCapPolicy": "reject"}"#,
    )
    .expect("cap+policy");
    let store = kaleido_core::AuthStore::load(root.clone()).expect("auth");

    let _first = store.login("admin", "rej-pass-777", "ip:r1").expect("first");
    match store.login("admin", "rej-pass-777", "ip:r2") {
        Err(kaleido_core::CoreError::SessionCap { cap, .. }) => assert_eq!(cap, 1),
        other => panic!("expected SessionCap, got {:?}", other.map(|_| ())),
    }
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
}

#[test]
fn auth_prune_oldest_drops_oldest_only() {
    let root = temp_root("prune-old");
    let _env = ENV_LOCK.lock().unwrap();
    admin_env();
    let store = kaleido_core::AuthStore::load(root.clone()).expect("auth");
    let a = store.login("admin", "snet-pass-123", "ip:o1").expect("a");
    let b = store.login("admin", "snet-pass-123", "ip:o2").expect("b");
    let c = store.login("admin", "snet-pass-123", "ip:o3").expect("c");
    let removed = store.prune_oldest_sessions(2).expect("prune");
    assert_eq!(removed, 2);
    // c is newest → survives; a,b dropped (created order == insertion order)
    assert!(store.resolve_session(&a.token).is_err());
    assert!(store.resolve_session(&b.token).is_err());
    assert!(store.resolve_session(&c.token).is_ok());
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
}

// ===========================================================================
// PartnerStore::build_generation_prompt_full — prompt assembly regression
// ===========================================================================

fn partner(tag: &str) -> PartnerStore {
    let root = temp_root(tag);
    let st = AppStateStore::new(root);
    PartnerStore::new(st)
}

fn wb_with_entries(id: &str) -> PartnerItem {
    serde_json::from_value(json!({
        "id": id,
        "name": "TestBook",
        "type": "world_book",
        "content": "",
        "fields": {
            "stBookRaw": { "entries": [
                {
                    "uid": "k1",
                    "keys": ["storm"],
                    "content": "STORM_LORE_CONTENT 风暴设定",
                    "comment": "storm lore",
                    "constant": false,
                    "disable": false,
                    "order": 10,
                    "position": 0
                },
                {
                    "uid": "c1",
                    "keys": [],
                    "content": "CONSTANT_LORE_CONTENT 常驻设定",
                    "constant": true,
                    "disable": false,
                    "order": 1,
                    "position": 0
                }
            ]}
        }
    }))
    .expect("wb item")
}

#[test]
fn prompt_constant_entry_injected_without_keyword() {
    let ps = partner("gp-const");
    ps.upsert_world_book(wb_with_entries("wb-a")).expect("upsert");
    ps.select(Some("wb-a".into()), None).expect("select");

    let r = ps
        .build_generation_prompt("base persona", Some("wb-a"), None, &[], None)
        .expect("build");
    // constant entry always present…
    assert!(r.system_prompt.contains("CONSTANT_LORE_CONTENT"), "got: {}", r.system_prompt);
    // …keyed entry absent when keyword never mentioned…
    assert!(!r.system_prompt.contains("STORM_LORE_CONTENT"));
    // …and section header used.
    assert!(r.system_prompt.contains("世界书（前置）") || r.system_prompt.contains("## 世界书"));
}

#[test]
fn prompt_keyword_activates_keyed_entry() {
    let ps = partner("gp-key");
    ps.upsert_world_book(wb_with_entries("wb-b")).expect("upsert");
    ps.select(Some("wb-b".into()), None).expect("select");

    let chat = vec![
        ("user".into(), "hello".to_string()),
        ("assistant".into(), "the storm is coming".to_string()),
    ];
    let r = ps
        .build_generation_prompt("base", Some("wb-b"), None, &chat, None)
        .expect("build");
    assert!(r.system_prompt.contains("STORM_LORE_CONTENT"), "keyword miss: {}", r.system_prompt);
    // constant entry activates with reason=constant; keyed one with key reason
    assert_eq!(r.wi.activated.len(), 2);
    let storm = r
        .wi
        .activated
        .iter()
        .find(|a| a.content.contains("STORM_LORE_CONTENT"))
        .expect("storm entry activated");
    assert_eq!(storm.reason, "key:storm");
    assert!(
        r.wi.activated.iter().any(|a| a.reason == "constant"),
        "constant entry should carry reason=constant"
    );
}

#[test]
fn prompt_empty_base_gets_default_persona() {
    let ps = partner("gp-default");
    let r = ps
        .build_generation_prompt("", None, None, &[], None)
        .expect("build");
    assert!(r.system_prompt.contains("伴侣"), "default persona expected, got: {}", r.system_prompt);
}

#[test]
fn prompt_char_card_macro_substitution_via_scan_ctx() {
    let ps = partner("gp-macro");
    // world book content uses {{char}} macro
    ps.upsert_world_book(serde_json::from_value(json!({
        "id": "wb-m", "name": "MacroBook", "type": "world_book", "content": "",
        "fields": { "stBookRaw": { "entries": [
            { "uid": "m1", "keys": [], "constant": true,
              "content": "{{char}} loves {{user}} MACRO_MARK", "position": 0, "order": 1 }
        ]}}
    })).expect("wb")).expect("upsert");
    ps.upsert_character_card(serde_json::from_value(json!({
        "id": "cc-m", "name": "Alice", "type": "character_card",
        "content": "", "fields": {}, "worldBookId": null
    })).expect("cc")).expect("upsert cc");
    ps.select(Some("wb-m".into()), Some("cc-m".into())).expect("select");

    let ctx = kaleido_core::WiScanContext {
        user_name: "Bob".into(),
        char_name: "Alice".into(),
        trigger: "normal".into(),
        ..Default::default()
    };
    let r = ps
        .build_generation_prompt_full("base", Some("wb-m"), Some("cc-m"), &[], None, None, false, Some(ctx), 8192)
        .expect("build full");
    assert!(r.system_prompt.contains("Alice loves Bob MACRO_MARK"),
        "macro substitution failed:\n{}", r.system_prompt);
    assert!(r.system_prompt.contains("你的角色人设设定"), "card section missing");
}

#[test]
fn prompt_select_unknown_wb_errors_not_found() {
    let ps = partner("gp-notfound");
    let err = ps.select(Some("ghost-wb".into()), None).unwrap_err();
    assert!(matches!(err, kaleido_core::CoreError::NotFound(_)));
}

#[test]
fn prompt_budget_respects_tiny_max_context() {
    let ps = partner("gp-budget");
    // one huge constant entry
    ps.upsert_world_book(serde_json::from_value(json!({
        "id": "wb-big", "name": "BigBook", "type": "world_book", "content": "",
        "fields": { "stBookRaw": { "entries": [
            { "uid": "big", "keys": [], "constant": true,
              "content": "X".repeat(20_000), "position": 0, "order": 1 }
        ]}}
    })).expect("wb")).expect("upsert");
    ps.select(Some("wb-big".into()), None).expect("select");

    let ctx = kaleido_core::WiScanContext {
        max_context_tokens: 1024,
        ..Default::default()
    };
    let r = ps
        .build_generation_prompt_full("base", Some("wb-big"), None, &[], Some(WiSettings::default()), None, false, Some(ctx), 1024)
        .expect("build");
    // budget must clamp the huge constant entry — full 20k X's must NOT appear raw.
    // (estimate mode chars→tokens: 20k chars ≫ 25% of 1024 tokens budget)
    let x_count = r.system_prompt.matches('X').count();
    assert!(x_count < 20_000, "budget not applied: {} X's leaked into prompt", x_count);
}

// ===========================================================================
// st_world_info unit edges
// ===========================================================================

#[test]
fn substitute_params_case_insensitive_all_forms() {
    let ctx = kaleido_core::WiScanContext {
        user_name: "U-NAME".into(),
        char_name: "C-NAME".into(),
        ..Default::default()
    };
    let out = kaleido_core::substitute_params(
        "{{user}} {{User}} {{USER}} <USER> | {{char}} {{Char}} {{CHAR}} <BOT> <CHAR>",
        &ctx,
    );
    assert_eq!(
        out,
        "U-NAME U-NAME U-NAME U-NAME | C-NAME C-NAME C-NAME C-NAME C-NAME"
    );
    // empty content stays empty; unknown macros untouched
    assert_eq!(kaleido_core::substitute_params("", &ctx), "");
    assert_eq!(
        kaleido_core::substitute_params("{{random}}", &ctx),
        "{{random}}"
    );
}

#[test]
fn parse_regex_from_string_valid_and_invalid() {
    use kaleido_core::parse_regex_from_string;
    let re = parse_regex_from_string(r"/\bstorm\b/i").expect("valid regex form");
    assert!(re.is_match("A STORM approaches"));
    assert!(parse_regex_from_string("plain text key").is_none(), "non-/…/ form must be None");
    assert!(
        parse_regex_from_string("/[unclosed/").is_none(),
        "invalid regex body must be None"
    );
}
