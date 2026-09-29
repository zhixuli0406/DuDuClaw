use super::*;

#[test]
fn frontmatter_validator_accepts_full_schema() {
    let content = "---\n\
                       title: Test Page\n\
                       created: 2026-04-22T10:00:00Z\n\
                       updated: 2026-04-22T10:00:00Z\n\
                       tags: [a, b]\n\
                       layer: context\n\
                       trust: 0.7\n\
                       ---\n\
                       body\n";
    assert!(validate_wiki_frontmatter(content).is_ok());
}

#[test]
fn frontmatter_validator_rejects_missing_frontmatter() {
    let err = validate_wiki_frontmatter("# body only\n").unwrap_err();
    assert!(err.contains("Missing YAML frontmatter"), "got: {}", err);
}

#[test]
fn frontmatter_validator_rejects_missing_required_fields() {
    // Missing tags, layer, trust
    let content = "---\n\
                       title: T\n\
                       created: 2026-04-22T00:00:00Z\n\
                       updated: 2026-04-22T00:00:00Z\n\
                       ---\n\
                       body\n";
    let err = validate_wiki_frontmatter(content).unwrap_err();
    assert!(err.contains("tags"), "err should mention tags: {}", err);
    assert!(err.contains("layer"), "err should mention layer: {}", err);
    assert!(err.contains("trust"), "err should mention trust: {}", err);
}

#[test]
fn frontmatter_validator_rejects_out_of_range_trust() {
    let content = "---\n\
                       title: T\n\
                       created: 2026-04-22T00:00:00Z\n\
                       updated: 2026-04-22T00:00:00Z\n\
                       tags: []\n\
                       layer: context\n\
                       trust: 1.5\n\
                       ---\n";
    let err = validate_wiki_frontmatter(content).unwrap_err();
    assert!(
        err.contains("0.0") || err.contains("[0.0, 1.0]"),
        "got: {}",
        err
    );
}

#[test]
fn frontmatter_validator_rejects_non_numeric_trust() {
    let content = "---\n\
                       title: T\n\
                       created: 2026-04-22T00:00:00Z\n\
                       updated: 2026-04-22T00:00:00Z\n\
                       tags: []\n\
                       layer: context\n\
                       trust: high\n\
                       ---\n";
    let err = validate_wiki_frontmatter(content).unwrap_err();
    assert!(err.contains("trust"), "got: {}", err);
}

#[test]
fn detect_fallback_catches_cjk_marker() {
    let body = "本報告基於訓練資料推測，web_search 工具回傳空結果。";
    assert!(detect_fallback_content(body).is_some());
}

#[test]
fn detect_fallback_catches_english_marker() {
    let body = "Unable to fetch live data; based on training data up to 2024.";
    assert!(detect_fallback_content(body).is_some());
}

#[test]
fn detect_fallback_ignores_clean_body() {
    let body = "TEMPO framework alternates policy refinement and critic recalibration.";
    assert!(detect_fallback_content(body).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_rejects_fallback_content() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    let args = serde_json::json!({
        "page_path": "research/bad.md",
        "content": "---\n\
                        title: Bad Page\n\
                        created: 2026-04-22T00:00:00Z\n\
                        updated: 2026-04-22T00:00:00Z\n\
                        tags: [research]\n\
                        layer: context\n\
                        trust: 0.5\n\
                        ---\n\
                        查無結果，基於訓練資料整理。\n",
    });

    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Fallback content detected"),
        "expected fallback rejection, got: {}",
        text
    );
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "should be isError=true"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_rejects_missing_frontmatter() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    let args = serde_json::json!({
        "page_path": "research/plain.md",
        "content": "# Just a title\n\nbody without frontmatter\n",
    });

    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("schema check failed") && text.contains("Missing YAML frontmatter"),
        "got: {}",
        text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_allows_fallback_mode_opt_in() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    let args = serde_json::json!({
        "page_path": "research/postmortem.md",
        "content": "---\n\
                        title: Postmortem\n\
                        created: 2026-04-22T00:00:00Z\n\
                        updated: 2026-04-22T00:00:00Z\n\
                        tags: [fallback-mode, postmortem]\n\
                        layer: context\n\
                        trust: 0.2\n\
                        ---\n\
                        web_search failed repeatedly; archiving this record.\n",
    });

    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    // Opt-in path should bypass the fallback rejection entirely.
    assert!(
        !text.contains("Fallback content detected"),
        "opt-in should not trigger rejection, got: {}",
        text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_accepts_clean_karpathy_page() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    let args = serde_json::json!({
        "page_path": "entities/tempo-framework.md",
        "content": "---\n\
                        title: TEMPO Framework\n\
                        created: 2026-04-22T00:00:00Z\n\
                        updated: 2026-04-22T00:00:00Z\n\
                        tags: [reasoning, test-time-training]\n\
                        layer: context\n\
                        trust: 0.6\n\
                        ---\n\
                        TEMPO alternates policy refinement with critic recalibration.\n",
    });

    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Written shared wiki page"),
        "clean page should succeed, got: {}",
        text
    );
    assert!(!result["isError"].as_bool().unwrap_or(false));
}

// ─────────────────────────────────────────────────────────────────────
// RFC-21 §3 — Shared-wiki SoT namespace policy integration
// ─────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_denied_when_namespace_is_read_only() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
    );

    let args = serde_json::json!({
        "page_path": "identity/discord-users.md",
        "content": clean_karpathy_page("Identity Roster"),
    });
    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "expected isError=true, got payload: {result}"
    );
    assert!(text.contains("Shared wiki write denied"), "got: {text}");
    assert!(
        text.contains("identity"),
        "should name the namespace, got: {text}"
    );
    assert!(
        text.contains("identity-provider"),
        "should name the synced_from capability, got: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_wiki_write_allowed_when_namespace_is_unlisted() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml(&agents_dir, "agnes");

    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
    );

    // 'concepts' is not listed — must remain writable.
    let args = serde_json::json!({
        "page_path": "concepts/return-policy.md",
        "content": clean_karpathy_page("Return Policy"),
    });
    let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Written shared wiki page"),
        "unlisted namespace should be writable, got: {text}"
    );
    assert!(!result["isError"].as_bool().unwrap_or(false));
}

// ─────────────────────────────────────────────────────────────────────
// WP7 — department knowledge-base isolation (departments/<dept>/…)
// ─────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn wp7_agent_can_write_and_read_own_department() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "monet", "art");

    let write_args = serde_json::json!({
        "page_path": "departments/art/palette.md",
        "content": clean_karpathy_page("Palette"),
    });
    let w = handle_shared_wiki_write(&write_args, tmp.path(), "monet").await;
    assert!(
        w["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("Written shared wiki page"),
        "own-department write should succeed: {w}"
    );

    let read_args = serde_json::json!({ "page_path": "departments/art/palette.md" });
    let r = handle_shared_wiki_read(&read_args, tmp.path(), "monet").await;
    assert!(
        !r["isError"].as_bool().unwrap_or(false),
        "own-department read should succeed: {r}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wp7_agent_cannot_write_other_department() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "monet", "art");

    // art agent writing into sales' namespace → denied, nothing persisted.
    let args = serde_json::json!({
        "page_path": "departments/sales/quota.md",
        "content": clean_karpathy_page("Quota"),
    });
    let result = handle_shared_wiki_write(&args, tmp.path(), "monet").await;
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "cross-department write must be denied: {result}"
    );
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("write denied"),
        "got: {result}"
    );
    assert!(
        !tmp.path()
            .join("shared/wiki/departments/sales/quota.md")
            .exists(),
        "denied write must not create the file"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wp7_no_department_agent_denied_all_departments() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    // Agent with no department field at all.
    write_agent_toml(&agents_dir, "agnes");

    let args = serde_json::json!({
        "page_path": "departments/art/palette.md",
        "content": clean_karpathy_page("Palette"),
    });
    let w = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
    assert!(
        w["isError"].as_bool().unwrap_or(false),
        "no-department write to a dept must be denied: {w}"
    );

    let r = handle_shared_wiki_read(
        &serde_json::json!({ "page_path": "departments/art/palette.md" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(
        r["isError"].as_bool().unwrap_or(false),
        "no-department read of a dept page must be denied: {r}"
    );

    // But the company layer stays open to a no-department agent.
    let company = handle_shared_wiki_write(
        &serde_json::json!({
            "page_path": "sop/onboarding.md",
            "content": clean_karpathy_page("Onboarding"),
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(
        company["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("Written shared wiki page"),
        "company-layer write should succeed for a no-department agent: {company}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wp7_explicit_scope_declaration_overrides_builtin() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "monet", "art");

    // Operator takes over the departments namespace as operator_only:
    // the built-in "own department" allowance is deferred, so even the
    // owning-department agent is now denied writes (policy wins).
    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."departments"]
                mode = "operator_only"
            "#,
    );
    let args = serde_json::json!({
        "page_path": "departments/art/palette.md",
        "content": clean_karpathy_page("Palette"),
    });
    let result = handle_shared_wiki_write(&args, tmp.path(), "monet").await;
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "explicit operator_only must deny: {result}"
    );
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("operator_only"),
        "explicit policy (not built-in dept rule) should be the reason: {result}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wp7_ls_hides_other_departments() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "monet", "art");
    write_agent_toml_dept(&agents_dir, "seller", "sales");

    // art page (by monet) + sales page (by seller) + a company page.
    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "departments/art/palette.md", "content": clean_karpathy_page("Palette") }),
        tmp.path(), "monet",
    ).await;
    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "departments/sales/quota.md", "content": clean_karpathy_page("Quota") }),
        tmp.path(), "seller",
    ).await;
    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "sop/hours.md", "content": clean_karpathy_page("Hours") }),
        tmp.path(), "monet",
    ).await;

    let ls = handle_shared_wiki_ls(tmp.path(), "monet").await;
    let text = ls["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("departments/art/palette.md"),
        "own dept visible: {text}"
    );
    assert!(
        text.contains("sop/hours.md"),
        "company page visible: {text}"
    );
    assert!(
        !text.contains("departments/sales/quota.md"),
        "other dept must be hidden: {text}"
    );
}

/// F4: an operator declaring the `departments` namespace in `.scope.toml`
/// tightens the WRITE policy — it must NOT open cross-department reads.
#[tokio::test(flavor = "current_thread")]
async fn wp7_explicit_scope_does_not_open_cross_department_reads() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "monet", "art");
    write_agent_toml_dept(&agents_dir, "seller", "sales");

    // Populate a sales page BEFORE any policy is declared.
    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "departments/sales/quota.md", "content": clean_karpathy_page("Quota") }),
        tmp.path(), "seller",
    ).await;

    // Operator declares the departments namespace (would previously flip the
    // buggy `explicit_override` and open reads to everyone).
    write_scope_policy(
        tmp.path(),
        r#"
                [namespaces."departments"]
                mode = "operator_only"
            "#,
    );

    // Cross-department read is STILL denied.
    let r = handle_shared_wiki_read(
        &serde_json::json!({ "page_path": "departments/sales/quota.md" }),
        tmp.path(),
        "monet",
    )
    .await;
    assert!(
        r["isError"].as_bool().unwrap_or(false),
        "explicit .scope.toml must NOT open cross-department reads: {r}"
    );

    // And ls still hides it.
    let ls = handle_shared_wiki_ls(tmp.path(), "monet").await;
    let text = ls["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("departments/sales/quota.md"),
        "other dept must stay hidden under explicit scope: {text}"
    );
}

/// F5: `shared_wiki_stats` must not leak other departments' page paths or
/// author counts.
#[tokio::test(flavor = "current_thread")]
async fn wp7_stats_hides_other_departments() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "monet", "art");
    write_agent_toml_dept(&agents_dir, "seller", "sales");

    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "departments/art/palette.md", "content": clean_karpathy_page("Palette") }),
        tmp.path(), "monet",
    ).await;
    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "departments/sales/quota.md", "content": clean_karpathy_page("Quota") }),
        tmp.path(), "seller",
    ).await;

    let stats = handle_shared_wiki_stats(tmp.path(), "monet").await;
    let text = stats["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("departments/art"),
        "own dept dir visible: {text}"
    );
    assert!(
        !text.contains("sales"),
        "other dept path/author must not leak: {text}"
    );
    assert!(
        !text.contains("seller"),
        "other dept author must not leak: {text}"
    );
}

/// F5: `shared_wiki_lint` must not surface other departments' page paths.
#[tokio::test(flavor = "current_thread")]
async fn wp7_lint_hides_other_departments() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    write_agent_toml_dept(&agents_dir, "monet", "art");
    write_agent_toml_dept(&agents_dir, "seller", "sales");

    // A deliberately schema-broken sales page so lint would want to name it.
    let _ = handle_shared_wiki_write(
        &serde_json::json!({ "page_path": "departments/art/palette.md", "content": clean_karpathy_page("Palette") }),
        tmp.path(), "monet",
    ).await;
    let sales_dir = tmp.path().join("shared/wiki/departments/sales");
    fs::create_dir_all(&sales_dir).unwrap();
    fs::write(sales_dir.join("quota.md"), "no frontmatter at all").unwrap();

    let lint = handle_shared_wiki_lint(tmp.path(), "monet").await;
    let text = lint["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("departments/sales/quota.md"),
        "other dept page must not appear in lint report: {text}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// WP2.3 — namespace read-visibility via `visible_to_departments`
// ─────────────────────────────────────────────────────────────────────
