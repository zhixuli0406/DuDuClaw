use super::*;

fn home_with(agent_id: &str, body: Option<&str>) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("duduclaw-atoml-{}", uuid::Uuid::new_v4()));
    let dir = home.join("agents").join(agent_id);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(b) = body {
        std::fs::write(dir.join("agent.toml"), b).unwrap();
    }
    home
}

#[test]
fn default_direction_agent_ids_skip_unreadable_names_but_keep_dir_names() {
    for body in [
        None,                                 // no agent.toml
        Some(""),                             // empty file
        Some("[budget]\nhard_stop = true\n"), // no [agent]
        Some("[agent]\n"),                    // table, no name
        Some("[agent]\nname = 42\n"),         // wrong type
        Some("agent = \"scalar\"\n"),         // wrong-typed section
        Some("not toml [[["),                 // malformed
    ] {
        let home = home_with("dirname", body);
        let ids = collect_existing_agent_identifiers(&home);
        assert!(
            ids.contains("dirname"),
            "the directory name is always reserved, for {body:?}"
        );
        assert_eq!(ids.len(), 1, "no phantom id from {body:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    // A readable name reserves BOTH the dir name and the declared name.
    let home = home_with("dirname", Some("[agent]\nname = \"declared\"\n"));
    let ids = collect_existing_agent_identifiers(&home);
    assert!(ids.contains("dirname") && ids.contains("declared"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn default_direction_agent_status_unknown_is_indeterminate_not_fatal() {
    for body in [
        None,
        Some(""),
        Some("[agent]\n"),
        Some("[agent]\nstatus = \"retired_maybe\""), // unrecognised value
        Some("[agent]\nstatus = 3\n"),               // wrong type
        Some("agent = \"scalar\"\n"),
        Some("not toml [[["),
    ] {
        let home = home_with("a", body);
        assert!(
            agent_status_of(&home, "a").is_none(),
            "indeterminate expected for {body:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    let home = home_with("a", Some("[agent]\nstatus = \"active\"\n"));
    assert_eq!(
        agent_status_of(&home, "a"),
        Some(duduclaw_core::types::AgentStatus::Active)
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn default_direction_computer_use_gate_is_fail_closed() {
    // The gate reads `[capabilities] computer_use` through the shared
    // parse point; everything except an explicit `true` must deny.
    for (body, want) in [
        ("", false),
        ("[capabilities]\n", false),
        ("[capabilities]\ncomputer_use = false\n", false),
        ("[capabilities]\ncomputer_use = \"true\"\n", false), // wrong type
        ("[capabilities]\ncomputer_use = 1\n", false),        // wrong type
        ("capabilities = \"scalar\"\n", false),
        ("not toml [[[", false),
        ("[capabilities]\ncomputer_use = true\n", true),
    ] {
        assert_eq!(
            duduclaw_core::agent_toml::parse(body)
                .capabilities
                .computer_use,
            want,
            "for {body:?}"
        );
    }
}
