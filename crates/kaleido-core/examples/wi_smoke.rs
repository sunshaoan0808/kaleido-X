use kaleido_core::{
    check_world_info, check_world_info_timed, entries_from_world_book, format_wi_for_system,
    get_regexed_string, parse_decorators, parse_mes_examples, parse_regex_script, run_regex_script, substitute_params,
    CharacterFilter, RegexPlacement, WiEntry, WiPosition, WiScanContext, WiSettings,
};
use serde_json::json;

fn base_ent(uid: &str, keys: &[&str], content: &str, constant: bool) -> WiEntry {
    WiEntry {
        uid: uid.into(),
        world: "test".into(),
        keys: keys.iter().map(|s| s.to_string()).collect(),
        keysecondary: vec![],
        content: content.into(),
        comment: uid.into(),
        constant,
        disable: false,
        selective: false,
        selective_logic: kaleido_core::SelectiveLogic::AndAny,
        order: 10,
        position: WiPosition::Before,
        depth: 4,
        probability: 100.0,
        use_probability: false,
        scan_depth: None,
        case_sensitive: None,
        match_whole_words: None,
        group: String::new(),
        group_override: false,
        group_weight: 100.0,
        use_group_scoring: None,
        exclude_recursion: false,
        prevent_recursion: false,
        delay_until_recursion: false,
        ignore_budget: false,
        sticky: None,
        cooldown: None,
        delay: None,
        decorators: Vec::new(),
        outlet_name: String::new(),
        character_filter: None,
        triggers: Vec::new(),
        vectorized: false,
        automation_id: String::new(),
        role: 0,
    }
}

fn main() {
    assert_eq!(parse_decorators("@@activate\nX").0, vec!["@@activate".to_string()]);
    let ctx = WiScanContext {
        user_name: "Alice".into(),
        char_name: "Bob".into(),
        trigger: "normal".into(),
        character_name: "Bob".into(),
        ..Default::default()
    };
    assert!(substitute_params("hi {{user}} and {{char}}", &ctx).contains("Alice"));
    assert!(substitute_params("hi {{user}} and {{char}}", &ctx).contains("Bob"));

    let book = json!({"stBookRaw":{"name":"B","entries":[
        {"keys":["{{char}}"],"content":"CHAR_KEY_HIT for {{user}}","enabled":true,"position":0,"insertion_order":10},
        {"keys":["vec"],"content":"VEC","enabled":true,"extensions":{"vectorized":true},"insertion_order":9},
        {"keys":[],"content":"ONLY_CONTINUE","enabled":true,"constant":true,"extensions":{"triggers":["continue"]},"insertion_order":8},
        {"keys":[],"content":"FOR_BOB","enabled":true,"constant":true,"characterFilter":{"names":["Bob"],"isExclude":false},"insertion_order":7},
        {"keys":[],"content":"NOT_BOB","enabled":true,"constant":true,"characterFilter":{"names":["Bob"],"isExclude":true},"insertion_order":6},
        {"keys":[],"content":"DEPTH_USER","enabled":true,"constant":true,"position":4,"extensions":{"depth":1,"role":1},"insertion_order":5},
        {"keys":[],"content":"OUT","enabled":true,"constant":true,"position":7,"extensions":{"outlet_name":"pipe"},"insertion_order":4},
        {"keys":[],"content":"@@activate\nFORCE","enabled":true,"insertion_order":3}
    ]}});
    let ents = entries_from_world_book("W", Some(&book), "");
    let r = check_world_info_timed(
        &ents,
        &["Bob stands near Alice".into()],
        4096,
        &WiSettings::default(),
        None,
        false,
        Some(ctx.clone()),
    );
    let contents: Vec<_> = r.activated.iter().map(|a| a.content.clone()).collect();
    println!("activated={contents:?}");
    println!("skipped vec={} filter={} trigger={}", r.skipped_vectorized, r.skipped_filter, r.skipped_trigger);
    assert!(contents.iter().any(|c| c.contains("CHAR_KEY_HIT") && c.contains("Alice")));
    assert!(!contents.iter().any(|c| c.contains("VEC")));
    assert!(!contents.iter().any(|c| c == "ONLY_CONTINUE")); // trigger mismatch
    assert!(contents.iter().any(|c| c.contains("FOR_BOB")));
    assert!(!contents.iter().any(|c| c.contains("NOT_BOB")));
    assert!(contents.iter().any(|c| c.contains("FORCE")));
    assert!(r.prompt_slots.outlet_entries.iter().any(|o| o.name=="pipe"));
    assert!(r.prompt_slots.chat_injections.iter().any(|i| i.kind=="depth" && i.role=="user"));
    assert!(r.skipped_vectorized >= 1);
    assert!(r.skipped_trigger >= 1);

    // continue trigger allows ONLY_CONTINUE
    let mut ctx2 = ctx.clone();
    ctx2.trigger = "continue".into();
    let r2 = check_world_info_timed(&ents, &["x".into()], 4096, &WiSettings::default(), None, true, Some(ctx2));
    assert!(r2.activated.iter().any(|a| a.content.contains("ONLY_CONTINUE")));

    // sticky
    let mut sticky_e = base_ent("s1", &["dragon"], "DRAGON", false);
    sticky_e.sticky = Some(5);
    let r1 = check_world_info_timed(&[sticky_e.clone()], &["a dragon".into()], 4096, &WiSettings::default(), None, false, None);
    let timed = r1.timed_world_info.unwrap();
    let r3 = check_world_info_timed(&[sticky_e], &["next".into(),"y".into()], 4096, &WiSettings::default(), Some(timed), false, None);
    assert!(r3.activated.iter().any(|a| a.reason=="sticky"));

    let script = parse_regex_script(&json!({"findRegex":"/\\(OOC:.*?\\)/gi","replaceString":"","placement":[2],"promptOnly":true})).unwrap();
    assert_eq!(get_regexed_string("Hi (OOC: x)", RegexPlacement::AiOutput, &[script], false, true, None), "Hi ");
    let _ = (check_world_info, format_wi_for_system, run_regex_script, CharacterFilter::default());
    // EM example pairs
    let pairs = parse_mes_examples(
        "<START>\n{{user}}: Hi\n{{char}}: Hello there\n<START>\n{{user}}: Bye\n{{char}}: Later",
        "Alice",
        "Bob",
    );
    assert!(pairs.iter().any(|(r,c)| r=="user" && c.contains("Hi")));
    assert!(pairs.iter().any(|(r,c)| r=="assistant" && c.contains("Hello")));

    let mut em = base_ent("em", &[], "<START>\n{{user}}: Ping\n{{char}}: Pong", true);
    em.position = WiPosition::EmTop;
    em.automation_id = "auto.demo".into();
    let r_em = check_world_info_timed(&[em], &["x".into()], 4096, &WiSettings::default(), None, true, Some(ctx));
    assert!(r_em.example_messages.iter().any(|m| m.role=="user" && m.content.contains("Ping")));
    assert!(r_em.example_messages.iter().any(|m| m.role=="assistant" && m.content.contains("Pong")));
    assert!(r_em.automation_ids.iter().any(|a| a=="auto.demo"));

    println!("WI_SMOKE_OK");
}
