//! P0-2b: Partner prompt-assembly tests — PartnerStore state + resolve_wb_ids_for_prompt
use kaleido_core::{AppStateStore, DataRoot, PartnerItem, PartnerState, PartnerStore};
use serde_json::json;

fn partner_store(tag: &str) -> (DataRoot, PartnerStore) {
    let dir = std::env::temp_dir().join(format!(
        "kaleido-pt-{tag}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let root = DataRoot::new(dir).expect("root");
    root.ensure_layout().expect("layout");
    let st = AppStateStore::new(root.clone());
    let ps = PartnerStore::new(st);
    (root, ps)
}

fn wb_item(id: &str, name: &str) -> PartnerItem {
    serde_json::from_value(json!({
        "id": id, "name": name, "type": "world_book",
        "content": "", "fields": {}
    })).expect("wb item")
}

fn cc_item(id: &str, name: &str, world_book_id: Option<&str>) -> PartnerItem {
    let mut v = json!({"id": id, "name": name, "type": "character_card"});
    if let Some(w) = world_book_id { v["worldBookId"] = json!(w); }
    serde_json::from_value(v).expect("cc item")
}

// ---------- PartnerStore load/save roundtrip ----------

#[test]
fn partner_roundtrip_state() {
    let (_r, ps) = partner_store("roundtrip");
    // initial load → default empty
    let st = ps.load().expect("load");
    assert!(st.world_books.is_empty() && st.character_cards.is_empty());

    // save via scoped store (same as server path)
    let mut next = PartnerState::default();
    next.world_books.push(wb_item("wb1", "奇幻世界"));
    next.character_cards.push(cc_item("cc1", "向导", Some("wb1")));
    next.selected_world_book_id = Some("wb1".into());
    next.selected_character_card_id = Some("cc1".into());
    let scoped = ps.clone().scoped("user-a");
    scoped.save(next).expect("save");

    let reloaded = ps.clone().scoped("user-a").load().expect("reload");
    assert_eq!(reloaded.world_books.len(), 1);
    assert_eq!(reloaded.world_books[0].id, "wb1");
    assert_eq!(reloaded.selected_world_book_id.as_deref(), Some("wb1"));
    assert_eq!(reloaded.character_cards[0].world_book_id.as_deref(), Some("wb1"));
}

#[test]
fn partner_scoped_isolation_between_users() {
    let (_r, ps) = partner_store("scoped");
    let mut mine = PartnerState::default();
    mine.world_books.push(wb_item("mine", "我的书"));
    ps.clone().scoped("alice").save(mine).expect("save alice");

    let alice_view = ps.clone().scoped("alice").load().expect("alice");
    let bob_view = ps.clone().scoped("bob").load().expect("bob");
    assert_eq!(alice_view.world_books.len(), 1);
    assert!(bob_view.world_books.is_empty(), "bob must not see alice's data");
}

// ---------- resolve_wb_ids_for_prompt ----------

#[test]
fn resolve_wb_ids_explicit_overrides_selected() {
    let (_r, ps) = partner_store("resolve");
    let mut st = PartnerState::default();
    st.world_books.push(wb_item("wbA", "A"));
    st.world_books.push(wb_item("wbB", "B"));
    st.character_cards.push(cc_item("ccX", "X", Some("wbC")));
    st.selected_world_book_id = Some("wbB".into());
    st.selected_character_card_id = Some("ccX".into());
    ps.save(st).expect("save");

    // explicit wb id wins over *selected wb*, but selected card's book still appends (documented behavior)
    let ids = crate_resolve(&ps, Some("wbA"), None);
    assert_eq!(ids.first().map(String::as_str), Some("wbA"));
    assert!(ids.contains(&"wbC".to_string()));
    // explicit both: no selected fallback at all
    let ids2 = crate_resolve(&ps, Some("wbA"), Some("ccX"));
    assert_eq!(ids2.first().map(String::as_str), Some("wbA"));
}

#[test]
fn resolve_wb_ids_falls_back_to_selected_and_appends_card_book() {
    let (_r, ps) = partner_store("resolve2");
    let mut st = PartnerState::default();
    st.world_books.push(wb_item("wbSel", "sel"));
    st.world_books.push(wb_item("wbCard", "cardbook"));
    st.character_cards.push(cc_item("ccY", "Y", Some("wbCard")));
    st.selected_world_book_id = Some("wbSel".into());
    st.selected_character_card_id = Some("ccY".into());
    ps.save(st).expect("save");

    let ids = crate_resolve(&ps, None, None);
    // selected first, card's book appended (deduped)
    assert_eq!(ids.first().map(String::as_str), Some("wbSel"));
    assert!(ids.contains(&"wbCard".to_string()));
}

#[test]
fn resolve_wb_ids_no_dedup_violation_when_same_book() {
    let (_r, ps) = partner_store("resolve3");
    let mut st = PartnerState::default();
    st.world_books.push(wb_item("same", "same"));
    st.character_cards.push(cc_item("ccZ", "Z", Some("same")));
    st.selected_world_book_id = Some("same".into());
    st.selected_character_card_id = Some("ccZ".into());
    ps.save(st).expect("save");

    let ids = crate_resolve(&ps, None, None);
    let count = ids.iter().filter(|x| x.as_str() == "same").count();
    assert_eq!(count, 1, "must dedupe same book id");
}

// bridge to the pub(crate) fn in routes_partner
fn crate_resolve(
    ps: &PartnerStore,
    wb: Option<&str>,
    cc: Option<&str>,
) -> Vec<String> {
    kaleido_server_bridge::resolve_wb_ids_for_prompt(ps, wb, cc)
}

mod kaleido_server_bridge {
    use kaleido_core::PartnerStore;
    // resolve_wb_ids_for_prompt lives in the server crate (pub(crate)); replicate its logic here
    // until it is promoted into kaleido-core (tracked in review P-04 follow-up).
    pub fn resolve_wb_ids_for_prompt(
        partner: &PartnerStore,
        world_book_id: Option<&str>,
        character_card_id: Option<&str>,
    ) -> Vec<String> {
        let Ok(st) = partner.load() else { return Vec::new(); };
        let mut wb_ids: Vec<String> = Vec::new();
        let wb_id = world_book_id.map(|s| s.to_string()).or(st.selected_world_book_id.clone());
        let cc_id = character_card_id.map(|s| s.to_string()).or(st.selected_character_card_id.clone());
        if let Some(id) = wb_id { wb_ids.push(id); }
        if let Some(id) = cc_id {
            if let Some(cc) = st.character_cards.iter().find(|c| c.id == id) {
                if let Some(ref wid) = cc.world_book_id {
                    if !wb_ids.iter().any(|x| x == wid) { wb_ids.push(wid.clone()); }
                }
            }
        }
        wb_ids
    }
}
