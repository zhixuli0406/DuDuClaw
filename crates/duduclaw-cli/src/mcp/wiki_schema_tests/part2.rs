use super::*;

#[tokio::test(flavor = "current_thread")]
async fn wp23_visible_to_departments_filters_read_and_ls() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "hilda", "hr");
    write_agent_toml_dept(&agents_dir, "monet", "art");

    // Operator restricts the `hr` namespace to hr + legal departments.
    // agent_writable keeps writes open so we can seed the page via MCP.
    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."hr"]
                mode = "agent_writable"
                visible_to_departments = ["hr", "legal"]
            "#,
    );

    // hr agent writes an hr-namespace page (write policy is agent_writable).
    let w = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "hr/salary.md", "content": clean_karpathy_page("Salary bands") }),
        tmp.path(),
        "hilda",
    )
    .await;
    assert!(
        w["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("Written shared wiki page"),
        "hr agent should be able to write its restricted namespace: {w}"
    );

    // hr agent (in-department) can read it.
    let r_ok = handle_shared_wiki_read(
        &serde_json::json!({ "page_path": "hr/salary.md" }),
        tmp.path(),
        "hilda",
    )
    .await;
    assert!(
        !r_ok["isError"].as_bool().unwrap_or(false),
        "in-department read must succeed: {r_ok}"
    );

    // art agent (not hr/legal) is denied the read — fail-closed.
    let r_deny = handle_shared_wiki_read(
        &serde_json::json!({ "page_path": "hr/salary.md" }),
        tmp.path(),
        "monet",
    )
    .await;
    assert!(
        r_deny["isError"].as_bool().unwrap_or(false),
        "out-of-department read must be denied: {r_deny}"
    );

    // ls hides the restricted page from the art agent but shows it to hr.
    let ls_art = handle_shared_wiki_ls(tmp.path(), "monet").await;
    assert!(
        !ls_art["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("hr/salary.md"),
        "restricted page must not appear in an out-of-department listing"
    );
    let ls_hr = handle_shared_wiki_ls(tmp.path(), "hilda").await;
    assert!(
        ls_hr["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("hr/salary.md"),
        "restricted page must appear for an in-department agent"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wp23_no_department_agent_denied_restricted_namespace() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    // Agent with NO department field.
    write_agent_toml(&agents_dir, "agnes");
    write_agent_toml_dept(&agents_dir, "hilda", "hr");

    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."hr"]
                mode = "agent_writable"
                visible_to_departments = ["hr"]
            "#,
    );

    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "hr/salary.md", "content": clean_karpathy_page("Salary") }),
        tmp.path(),
        "hilda",
    )
    .await;

    // No-department agent is fail-closed out of the declared namespace.
    let r = handle_shared_wiki_read(
        &serde_json::json!({ "page_path": "hr/salary.md" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(
        r["isError"].as_bool().unwrap_or(false),
        "no-department read of a restricted namespace must be denied: {r}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wp23_undeclared_namespace_visible_to_all() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "hilda", "hr");
    write_agent_toml_dept(&agents_dir, "monet", "art");

    // `hr` is declared, but `sop` is not — sop stays open to everyone.
    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."hr"]
                mode = "agent_writable"
                visible_to_departments = ["hr"]
            "#,
    );

    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "sop/hours.md", "content": clean_karpathy_page("Hours") }),
        tmp.path(),
        "hilda",
    )
    .await;

    // art agent reads the undeclared company page fine (no regression).
    let r = handle_shared_wiki_read(
        &serde_json::json!({ "page_path": "sop/hours.md" }),
        tmp.path(),
        "monet",
    )
    .await;
    assert!(
        !r["isError"].as_bool().unwrap_or(false),
        "undeclared namespace must stay visible to all: {r}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_unaffected_when_no_scope_policy_present() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    // No .scope.toml written — behaviour must match v1.10.1 exactly.
    let args = serde_json::json!({
        "page_path": "identity/should-still-work.md",
        "content": clean_karpathy_page("Pre-RFC behaviour"),
    });
    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Written shared wiki page"),
        "absent policy must not regress writes, got: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_unaffected_when_scope_toml_is_malformed() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    // Fail-safe path: malformed file must never block legitimate writes.
    write_scope_policy(tmp.path(), "this is :: not = valid = toml ===");

    let args = serde_json::json!({
        "page_path": "identity/discord-users.md",
        "content": clean_karpathy_page("Identity Roster"),
    });
    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "malformed policy must fail-safe to writable, got: {result}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_delete_denied_when_namespace_is_operator_only() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    // First write a page (no policy yet so the write succeeds).
    let write_args = serde_json::json!({
        "page_path": "policies/security.md",
        "content": clean_karpathy_page("Security Policy"),
    });
    let write_res = handle_shared_wiki_write(&write_args, tmp.path(), "agnes").await;
    assert!(!write_res["isError"].as_bool().unwrap_or(false));

    // Now lock down the policies/ namespace.
    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."policies"]
                mode = "operator_only"
            "#,
    );

    let del_args = serde_json::json!({ "page_path": "policies/security.md" });
    let del_res = handle_shared_wiki_delete(&del_args, tmp.path(), "agnes").await;
    let text = del_res["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        del_res["isError"].as_bool().unwrap_or(false),
        "operator_only delete should be denied even for the original author, got: {del_res}"
    );
    assert!(text.contains("Shared wiki delete denied"), "got: {text}");
    assert!(
        text.contains("operator_only"),
        "should name the mode, got: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_delete_main_role_requires_typed_agent_role_not_a_substring() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    // "mallory" only carries the literal inside a comment — the old
    // unanchored contains() check would have treated this as main.
    let mallory = agents_dir.join("mallory");
    fs::create_dir_all(&mallory).unwrap();
    fs::write(
        mallory.join("agent.toml"),
        "# note to self: never set role = \"main\" here\n[agent]\nname = \"mallory\"\nrole = \"worker\"\n",
    )
    .unwrap();

    // "boss" is a genuine typed main agent.
    let boss = agents_dir.join("boss");
    fs::create_dir_all(&boss).unwrap();
    fs::write(
        boss.join("agent.toml"),
        "[agent]\nname = \"boss\"\nrole = \"main\"\n",
    )
    .unwrap();

    let write_args = serde_json::json!({
        "page_path": "notes/acl-probe.md",
        "content": clean_karpathy_page("ACL Probe"),
    });
    let write_res = handle_shared_wiki_write(&write_args, tmp.path(), "agnes").await;
    assert!(!write_res["isError"].as_bool().unwrap_or(false));

    let del_args = serde_json::json!({ "page_path": "notes/acl-probe.md" });
    let spoof = handle_shared_wiki_delete(&del_args, tmp.path(), "mallory").await;
    assert!(
        spoof["isError"].as_bool().unwrap_or(false),
        "a role=main literal in a comment must not grant main-agent delete rights, got: {spoof}"
    );

    let real = handle_shared_wiki_delete(&del_args, tmp.path(), "boss").await;
    assert!(
        !real["isError"].as_bool().unwrap_or(false),
        "a typed [agent] role=\"main\" must still be honored, got: {real}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wiki_namespace_status_reports_loaded_policy() {
    let tmp = TempDir::new();
    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"

                [namespaces."policies"]
                mode = "operator_only"
            "#,
    );

    let result = handle_wiki_namespace_status(tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("\"namespace\": \"identity\""), "got: {text}");
    assert!(text.contains("\"mode\": \"read_only\""), "got: {text}");
    assert!(
        text.contains("\"synced_from\": \"identity-provider\""),
        "got: {text}"
    );
    assert!(text.contains("\"namespace\": \"policies\""), "got: {text}");
    assert!(text.contains("\"mode\": \"operator_only\""), "got: {text}");
    assert!(text.contains("\"policy_loaded\": true"), "got: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn wiki_namespace_status_reports_empty_policy_when_file_absent() {
    let tmp = TempDir::new();
    let result = handle_wiki_namespace_status(tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("none configured") && text.contains("agent_writable"),
        "got: {text}"
    );
    assert!(text.contains("\"policy_loaded\": false"), "got: {text}");
}

// ─────────────────────────────────────────────────────────────────────
// RFC-21 §1 — Identity Resolution MCP tool integration
// ─────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn identity_resolve_returns_payload_for_known_handle() {
    let tmp = TempDir::new();
    write_identity_record(
        tmp.path(),
        "ruby.md",
        "person_id: person_2f9\n\
             display_name: Ruby Lin\n\
             roles: [customer-pm]\n\
             project_ids: [proj-alpha]\n\
             channel_handles:\n  discord: \"1234567890\"\n",
    );

    let args = serde_json::json!({
        "channel": "discord",
        "external_id": "1234567890",
    });
    let result = handle_identity_resolve(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");

    assert!(!result["isError"].as_bool().unwrap_or(false));
    assert!(
        text.contains("\"person_id\": \"person_2f9\""),
        "got: {text}"
    );
    assert!(
        text.contains("\"display_name\": \"Ruby Lin\""),
        "got: {text}"
    );
    assert!(text.contains("\"source\": \"wiki-cache\""), "got: {text}");
    assert!(
        text.contains("Resolved person via wiki-cache"),
        "got: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn identity_resolve_returns_polite_miss_for_unknown_handle() {
    let tmp = TempDir::new();
    // No identity records at all → must report "no match", not error.
    let args = serde_json::json!({
        "channel": "discord",
        "external_id": "9999999",
    });
    let result = handle_identity_resolve(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "unknown person is not an error, got: {result}"
    );
    assert!(text.contains("No identity record matched"), "got: {text}");
    assert!(text.contains("treat as a stranger"), "got: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn identity_resolve_rejects_missing_channel_or_external_id() {
    let tmp = TempDir::new();
    let r1 = handle_identity_resolve(
        &serde_json::json!({ "external_id": "1234" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(r1["isError"].as_bool().unwrap_or(false));
    assert!(
        r1["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("channel")
    );

    let r2 = handle_identity_resolve(
        &serde_json::json!({ "channel": "discord" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(r2["isError"].as_bool().unwrap_or(false));
    assert!(
        r2["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("external_id")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn identity_resolve_accepts_unknown_channel_kind_via_other_variant() {
    let tmp = TempDir::new();
    write_identity_record(
        tmp.path(),
        "matrix-user.md",
        "person_id: person_mx\n\
             display_name: Matrix User\n\
             channel_handles:\n  matrix: \"@user:example.org\"\n",
    );

    // 'matrix' isn't a built-in ChannelKind variant — must still resolve
    // via the Other(_) catch-all.
    let args = serde_json::json!({
        "channel": "matrix",
        "external_id": "@user:example.org",
    });
    let result = handle_identity_resolve(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("\"person_id\": \"person_mx\""), "got: {text}");
}
