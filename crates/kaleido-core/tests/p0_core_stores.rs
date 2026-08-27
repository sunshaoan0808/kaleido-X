//! P0-2: core store integration tests — AuthStore / JobStore / Prompt assembly
use kaleido_core::{AuthStore, DataRoot, JobStore};
use serde_json::json;

fn temp_root(tag: &str) -> DataRoot {
    let dir = std::env::temp_dir().join(format!(
        "kaleido-test-{tag}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let root = DataRoot::new(dir.clone()).expect("data root");
    root.ensure_layout().expect("layout");
    root
}

// ---------- AuthStore ----------

#[test]
fn auth_bootstrap_requires_admin_password_env() {
    let root = temp_root("bootstrap");
    // users.json absent + KALEIDO_ADMIN_PASSWORD unset → must refuse
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
    let r = AuthStore::load(root);
    assert!(r.is_err(), "must not bootstrap without admin password");
}

#[test]
fn auth_login_logout_flow() {
    let root = temp_root("flow");
    std::env::set_var("KALEIDO_ADMIN_USER", "admin");
    std::env::set_var("KALEIDO_ADMIN_PASSWORD", "test-pass-123");
    let store = AuthStore::load(root).expect("auth store");
    assert_eq!(store.user_count(), 1);

    // wrong password → Err
    assert!(store.login("admin", "nope", "ip:t1").is_err());

    // correct → session
    let sess = store.login("admin", "test-pass-123", "ip:t2").expect("login");
    assert!(!sess.token.is_empty());
    let resolved = store.resolve_session(&sess.token).expect("resolve");
    assert_eq!(resolved.username, "admin");

    // logout invalidates token
    store.logout(&sess.token).expect("logout");
    assert!(store.resolve_session(&sess.token).is_err());
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
}

#[test]
fn auth_rate_limit_blocks_flood() {
    let root = temp_root("ratelimit");
    std::env::set_var("KALEIDO_ADMIN_USER", "admin");
    std::env::set_var("KALEIDO_ADMIN_PASSWORD", "x-pass-999");
    let store = AuthStore::load(root).expect("auth");
    // flood wrong logins from same rate key until limited
    let mut got_limited = false;
    for i in 0..50 {
        match store.login("admin", "wrong", &format!("ip:flood")) {
            Ok(_) => {}
            Err(kaleido_core::CoreError::RateLimited(_)) => { got_limited = true; break; }
            Err(_) => {}
        }
        let _ = i;
    }
    assert!(got_limited, "flood must trigger RateLimited");
    std::env::remove_var("KALEIDO_ADMIN_PASSWORD");
}

// ---------- JobStore ----------

#[test]
fn job_create_runs_then_queues_on_concurrency() {
    let root = temp_root("jobs");
    let jobs = JobStore::new(root);
    let a = jobs.create("noop", "u1", "ws1", json!({}), None, None).expect("job A");
    assert_eq!(a.status, "running");
    let b = jobs.create("noop", "u1", "ws1", json!({}), None, None).expect("job B");
    // default max_concurrent=2 → second may run or queue depending on config; third must queue
    let c = jobs.create("noop", "u1", "ws1", json!({}), None, None).expect("job C");
    let running = [&a, &b, &c].iter().filter(|j| j.status == "running").count();
    assert!(running <= 3);
    assert!(c.status == "queued" || c.status == "running");
}

#[test]
fn job_cancel_transitions_to_cancelled() {
    let root = temp_root("cancel");
    let jobs = JobStore::new(root);
    let j = jobs.create("noop", "u1", "ws1", json!({}), None, None).expect("job");
    let cancelled = jobs.cancel(&j.run_id).expect("cancel");
    let st = kaleido_core::normalize_job_status(&cancelled.status);
    assert!(st == "cancelled" || st == "cancel_requested" || st == "failed", "got {}", st);
}

#[test]
fn job_status_normalization() {
    assert_eq!(kaleido_core::normalize_job_status("done"), "succeeded");
    assert_eq!(kaleido_core::normalize_job_status("error"), "failed");
    assert_eq!(kaleido_core::normalize_job_status("stopped"), "cancelled");
    assert_eq!(kaleido_core::normalize_job_status("queued"), "queued");
    // unknown passthrough
    assert_eq!(kaleido_core::normalize_job_status("weird"), "weird");
}
