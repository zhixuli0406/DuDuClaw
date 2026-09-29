use super::*;

#[tokio::test(flavor = "current_thread")]
async fn a_shape_refusal_names_the_field_and_shows_the_example() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(2));
    // Missing a required key.
    let out = handle_team_handoff(
        &serde_json::json!({ "packet": { "packet_id": "pk-1", "output_format": "markdown" }}),
        tmp.path(),
        "planner-1",
    )
    .await;
    assert_eq!(refusal_code(&out), "invalid_packet");
    let text = text_of(&out);
    assert!(
        text.contains("missing field `objective`"),
        "serde's own message must survive verbatim: {text}"
    );
    assert!(
        text.contains(duduclaw_core::task_packet::MINIMAL_PACKET_EXAMPLE),
        "the refusal must show the shape to send: {text}"
    );
    assert!(text.contains("next_steps"), "optional keys listed: {text}");

    // A misspelled key: serde names it and lists the legal ones.
    let out = handle_team_handoff(
        &serde_json::json!({ "packet": {
            "packet_id": "pk-1", "objective": "x", "output_format": "markdown",
            "objectives": "typo",
        }}),
        tmp.path(),
        "planner-1",
    )
    .await;
    assert_eq!(refusal_code(&out), "invalid_packet");
    assert!(text_of(&out).contains("objectives"), "{}", text_of(&out));

    // An unknown output_format token names the four legal ones.
    let out = handle_team_handoff(
        &serde_json::json!({ "packet": {
            "packet_id": "pk-1", "objective": "x", "output_format": "csv",
        }}),
        tmp.path(),
        "planner-1",
    )
    .await;
    assert_eq!(refusal_code(&out), "invalid_packet");
    let text = text_of(&out);
    for token in duduclaw_core::task_packet::OutputFormat::TOKENS {
        assert!(text.contains(token), "`{token}` missing from: {text}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_validation_refusal_names_the_offending_item() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(2));
    let mut p = packet(Role::Planner, Role::Executor, 2);
    p.constraints = vec![
        duduclaw_core::task_packet::Constraint::new("c1", "ok"),
        duduclaw_core::task_packet::Constraint::new("c2", "越".repeat(201)),
    ];
    let out = call(tmp.path(), "planner-1", &p).await;
    assert_eq!(refusal_code(&out), "field_too_long");
    assert!(
        text_of(&out).contains("constraints[1].text"),
        "{}",
        text_of(&out)
    );
}

#[test]
fn sole_target_agrees_with_the_edge_table() {
    for from in Role::ALL {
        let legal: Vec<Role> = Role::ALL
            .iter()
            .copied()
            .filter(|to| is_legal_handoff_edge(*from, *to))
            .collect();
        match sole_legal_handoff_target(*from) {
            Some(to) => assert_eq!(legal, vec![to], "{from} must have exactly one target"),
            None => assert!(legal.is_empty(), "{from} must have no target"),
        }
    }
}

#[test]
fn team_handoff_description_shows_the_minimal_example() {
    // The ToolDef holds the example as a literal (a `const` cannot be
    // `concat!`-ed); this is what keeps it identical to the core const the
    // refusal hint and the composer render from.
    let def = tools()
        .find(|t| t.name == "team_handoff")
        .expect("catalogued");
    let example = duduclaw_core::task_packet::MINIMAL_PACKET_EXAMPLE;
    let param = def
        .params
        .iter()
        .find(|p| p.name == "packet")
        .expect("packet param");
    assert!(param.description.contains(example), "{}", param.description);
    // O7: the example used to be inlined in BOTH the prose description and
    // the `packet` param, i.e. 216 identical bytes paid twice on every spawn.
    // The param is where a caller looks for an argument's shape, so that is
    // the copy that stays; the prose must NOT repeat it.
    assert!(
        !def.description.contains(example),
        "the minimal packet example is duplicated in the prose description — \
         it belongs to the `packet` param only: {}",
        def.description
    );
    for key in duduclaw_core::task_packet::OPTIONAL_PACKET_KEYS {
        assert!(
            param.description.contains(key),
            "optional key `{key}` is not documented in the param description"
        );
    }
}

/// W3-3b (debt #12): the tool no longer rides on the `working_state`
/// family's `memory:write`. `working_state_*` writes only the caller's own
/// `<agent_dir>/state/`; an unpinned `team_handoff` writes into the shared
/// `<home>/team_packets/` tree, and `memory:write` is externally
/// grantable — so the handoff channel now sits on its own internal-only
/// scope. `mcp_auth::team_handoff_is_unreachable_from_every_external_principal`
/// locks the external half.
#[test]
fn scope_is_internal_only_and_separate_from_the_working_state_family() {
    use crate::mcp_auth::{EXTERNALLY_GRANTABLE_SCOPES, Scope, tool_requires_scope};
    assert_eq!(
        tool_requires_scope("team_handoff"),
        Some(Scope::TeamHandoff)
    );
    assert_ne!(
        tool_requires_scope("team_handoff"),
        tool_requires_scope("working_state_handoff"),
        "team_handoff must NOT share the externally-grantable working_state scope"
    );
    assert!(!EXTERNALLY_GRANTABLE_SCOPES.contains(&Scope::TeamHandoff));
    assert!(!EXTERNAL_TOOLS_WHITELIST.contains(&"team_handoff"));
    assert!(tools().any(|t| t.name == "team_handoff"));
    // Self-echo: a packet can never ground its author's claims.
    assert!(duduclaw_core::grounding::is_self_echo_tool("team_handoff"));
}
