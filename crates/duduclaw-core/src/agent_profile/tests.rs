use super::*;

// Core tests cover rendering and its built-in shape guard. Production callers
// use the security crate wrapper, whose tests exercise the context detector.
fn export(profile: &AgentProfile, format: ExportFormat, include_user: bool) -> Result<Vec<ExportArtifact>, ProfileError> {
    export_with_credential_scan(profile, format, include_user, |_| Ok(false))
}

fn profile() -> AgentProfile {
    AgentProfile {
        id: "support".into(), name: "小嘟".into(), description: "Customer support".into(),
        soul: "  語氣\r\n# Embedded heading\n```example\ncontent\n```\n\n".into(),
        identity: "身分\n".into(), user: "服務對象\r\n偏好繁體中文".into(),
        rules: "查不到就說查不到\n\n".into(), contract: "[boundaries]\r\nmust_not = [\"承諾退款\"]\r\n".into(),
        ..Default::default()
    }
}

fn plain(body: &str) -> String {
    format!("---\nschema: {SCHEMA}\nid: support\nname: 小嘟\n---\n{body}")
}

#[test]
fn valid_schema_is_detected_by_content_and_sections_ignore_case() {
    let input = plain("# pErSoNa\n語氣\n# uSeR\n偏好\n# RuLeS\n規則\n# CoNtRaCt\n[boundaries]\nmust_not = []\n");
    assert!(has_schema(&input));
    let profile = parse(&input).unwrap();
    assert_eq!(profile.soul, "語氣");
    assert_eq!(profile.user, "偏好");
    assert_eq!(profile.rules, "規則");
    assert_eq!(profile.contract, "[boundaries]\nmust_not = []");
}

#[test]
fn unknown_and_cjk_headings_merge_into_rules_even_before_rules() {
    let input = plain("# 工作規範\n第一\n# Persona\nPerson\n# Rules\nSecond\n# その他\nThird\n");
    let profile = parse(&input).unwrap();
    assert_eq!(profile.warnings.len(), 2);
    assert_eq!(profile.rules, "Second\n\n# 工作規範\n第一\n\n# その他\nThird");
}

#[test]
fn missing_schema_and_oversize_are_refused() {
    assert!(!has_schema("# Persona\nHello"));
    assert!(matches!(parse("---\nid: support\n---\n# Persona\nHello"), Err(ProfileError::Schema)));
    assert!(matches!(parse(&"x".repeat(MAX_BYTES + 1)), Err(ProfileError::Oversize)));
}

#[test]
fn crlf_frontmatter_is_accepted() {
    let input = plain("# Persona\n小嘟\n# Rules\n安全規則\n").replace('\n', "\r\n");
    assert!(has_schema(&input));
    assert_eq!(parse(&input).unwrap().rules, "安全規則");
}

#[test]
fn frozen_fields_are_named_and_never_silently_discarded() {
    let input = "---\nschema: duduclaw.agent/v1\nid: support\nduduclaw:\n  runtime: claude\n  capabilities: {allowed_tools: [Bash]}\n  model: {account_pool: [private]}\n  agent: {reports_to: ceo, role: main}\n  heartbeat: {token_enc: encrypted}\n---\n# Persona\nText\n";
    let error = parse(input).unwrap_err().to_string();
    for key in ["runtime", "capabilities", "account_pool", "reports_to", "role", "token_enc"] {
        assert!(error.contains(key), "missing rejected field {key}: {error}");
    }
}

#[test]
fn traversal_ids_are_rejected_before_load() {
    for id in ["../outside", "x/y", "x\\y", ".", "..", "", "支持"] {
        assert!(validate_id(id).is_err());
        assert!(load(Path::new("/nonexistent"), id).unwrap_err().to_string().contains("invalid employee id"));
    }
}

#[test]
fn native_round_trip_preserves_all_file_bytes() {
    let original = profile();
    let files = export(&original, ExportFormat::Duduclaw, true).unwrap();
    let imported = parse(&files[0].content).unwrap();
    assert_eq!(original.soul.as_bytes(), imported.soul.as_bytes());
    assert_eq!(original.identity.as_bytes(), imported.identity.as_bytes());
    assert_eq!(original.user.as_bytes(), imported.user.as_bytes());
    assert_eq!(original.rules.as_bytes(), imported.rules.as_bytes());
    assert_eq!(original.contract.as_bytes(), imported.contract.as_bytes());
}

#[test]
fn native_round_trip_preserves_empty_and_no_newline_files() {
    for text in ["", "one", "\n", "\r\n", "\r", "\n\n", "中文 🐾", "````\n# User\n````"] {
        let original = AgentProfile { id: "a".into(), soul: text.into(), user: text.into(), rules: text.into(), contract: "[boundaries]\nmust_not = []\n".into(), ..Default::default() };
        let output = export(&original, ExportFormat::Duduclaw, true).unwrap();
        let imported = parse(&output[0].content).unwrap();
        assert_eq!(imported.soul, text);
        assert_eq!(imported.user, text);
        assert_eq!(imported.rules, text);
        assert_eq!(imported.contract, original.contract);
    }
}

#[test]
fn every_export_format_maps_expected_files() {
    let p = profile();
    for (name, expected) in [
        ("duduclaw", vec!["support.agent.md"]),
        ("openclaw", vec!["SOUL.md", "AGENTS.md", "IDENTITY.md", "USER.md"]),
        ("hermes", vec!["SOUL.md", "AGENTS.md", "USER.md"]),
        ("agents-md", vec!["AGENTS.md"]),
        ("claude-subagent", vec!["support.md"]),
    ] {
        let files = export(&p, name.parse().unwrap(), true).unwrap();
        assert_eq!(files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), expected);
        assert!(files.iter().any(|f| f.content.contains("承諾退款")));
    }
    assert!("unsupported".parse::<ExportFormat>().is_err());
}

#[test]
fn user_is_excluded_unless_explicitly_included_in_every_format() {
    let mut p = profile();
    p.user = "PRIVATE_USER_SENTINEL".into();
    for format in [ExportFormat::Duduclaw, ExportFormat::Openclaw, ExportFormat::Hermes, ExportFormat::AgentsMd, ExportFormat::ClaudeSubagent] {
        let files = export(&p, format, false).unwrap();
        assert!(!files.iter().any(|f| f.name == "USER.md" || f.content.contains("PRIVATE_USER_SENTINEL")));
        let files = export(&p, format, true).unwrap();
        assert!(files.iter().any(|f| f.content.contains("PRIVATE_USER_SENTINEL")));
    }
}

#[test]
fn secret_refusal_names_section_without_echoing_secret() {
    let mut p = profile();
    p.soul = "sk-test-abcdefghijklmnopqrstuvwxyz123456".into();
    let error = export(&p, ExportFormat::Duduclaw, false).unwrap_err().to_string();
    assert!(error.contains("Persona"));
    assert!(!error.contains("abcdefghijklmnopqrstuvwxyz"));
    p.soul.clear();
    p.user = "secret://private/account".into();
    assert!(export(&p, ExportFormat::Duduclaw, false).is_ok());
    assert!(matches!(export(&p, ExportFormat::Duduclaw, true), Err(ProfileError::Secret(section)) if section == "User"));
    p.user.clear();
    p.name = "secret://private/account".into();
    assert!(matches!(export(&p, ExportFormat::Duduclaw, false), Err(ProfileError::Secret(section)) if section == "Settings"));
}

#[test]
fn credential_scan_failure_refuses_without_echoing_content() {
    let error = export_with_credential_scan(&profile(), ExportFormat::Duduclaw, false, |_| Err(()))
        .unwrap_err().to_string();
    assert!(error.contains("Persona"));
    assert!(!error.contains("語氣"));
}

#[test]
fn disk_settings_exclude_credentials_authority_and_mcp() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("agent.toml"), "[agent]\nname = 'support'\ndisplay_name = 'Support'\nrole = 'main'\nreports_to = 'ceo'\n[channels]\ntelegram_token_enc = 'encrypted'\n[model]\npreferred = 'example-model'\naccount_pool = ['private']\n[capabilities]\nallowed_tools = ['Bash']\n[permissions]\ncan_create_agents = true\n[mcp]\nexternal = []\n").unwrap();
    std::fs::write(dir.path().join(".mcp.json"), "secret sentinel").unwrap();
    std::fs::write(dir.path().join("MEMORY.md"), "memory sentinel").unwrap();
    let p = load(dir.path(), "support").unwrap();
    assert!(p.settings.contains_key("model"));
    for key in ["channels", "capabilities", "permissions", "mcp"] { assert!(!p.settings.contains_key(key)); }
    let output = export(&p, ExportFormat::Duduclaw, false).unwrap();
    for forbidden in ["account_pool", "token_enc", "reports_to", "secret sentinel", "memory sentinel"] {
        assert!(!output[0].content.contains(forbidden));
    }
}

#[test]
fn user_character_limit_counts_unicode_characters() {
    assert!(parse(&plain(&format!("# User\n{}", "用".repeat(MAX_USER_CHARS)))).is_ok());
    assert!(parse(&plain(&format!("# User\n{}", "用".repeat(MAX_USER_CHARS + 1)))).is_err());
}

#[test]
fn duplicate_sections_and_unterminated_native_blocks_are_refused() {
    assert!(parse(&plain("# Rules\nA\n# RULES\nB")).is_err());
    assert!(parse(&plain("# User\n```duduclaw-user\nUnclosed")).is_err());
}

#[cfg(unix)]
#[test]
fn export_loader_refuses_links() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("agent.toml"), "[agent]\nname = 'a'\n").unwrap();
    std::fs::write(outside.path().join("secret"), "private").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("SOUL.md")).unwrap();
    assert!(load(dir.path(), "a").is_err());
}

#[test]
fn yaml_contract_lists_become_native_boundaries() {
    let p = parse(&plain("# Contract\nmust_not:\n- 不承諾退款金額\nmust_always:\n- 查不到就說查不到\n")).unwrap();
    let contract: toml::Table = toml::from_str(&p.contract).unwrap();
    assert_eq!(contract["boundaries"]["must_not"][0].as_str(), Some("不承諾退款金額"));
    assert_eq!(contract["boundaries"]["must_always"][0].as_str(), Some("查不到就說查不到"));
}

#[test]
fn invalid_contracts_are_refused_before_import() {
    assert!(parse(&plain("# Contract\nmust_not: just-a-string\n")).is_err());
    assert!(parse(&plain("# Contract\n```duduclaw-contract\n[boundaries]\nmust_not = 1\n\n```\n")).is_err());
}

#[test]
fn oversized_native_body_still_pins_native_schema_detection() {
    let input = plain(&format!("# Persona\n{}", "x".repeat(MAX_BYTES + 1)));
    assert!(has_schema(&input));
    assert!(matches!(parse(&input), Err(ProfileError::Oversize)));
}

#[test]
fn schema_probe_needs_only_header_prefix_and_cannot_downgrade_bad_yaml() {
    for input in [
        "---\nschema: duduclaw.agent/v1\n",
        "---\nschema: duduclaw.agent/v1\nbroken: [\n---\n",
        "---\nschema: duduclaw.agent/v1\nschema: another-format\n---\n",
        "---\n'schema': 'duduclaw.agent/v1'\ninvalid: [\n",
    ] {
        assert!(has_schema(input), "native declaration lost: {input}");
        assert!(parse(input).is_err());
    }
}

#[test]
fn native_slot_byte_limits_match_the_shared_writer() {
    for section in ["Persona", "Identity", "Rules", "Contract"] {
        let mut p = AgentProfile { id: "support".into(), ..Default::default() };
        let limit = if section == "Contract" { MAX_CONTRACT_BYTES } else { MAX_MARKDOWN_BYTES };
        let text = if section == "Contract" {
            format!("#{}", "x".repeat(limit - 1))
        } else {
            "x".repeat(limit)
        };
        match section {
            "Persona" => p.soul = text,
            "Identity" => p.identity = text,
            "Rules" => p.rules = text,
            _ => p.contract = text,
        }
        assert!(validate_sections(&p).is_ok(), "at limit: {section}");
        let exported = export(&p, ExportFormat::Duduclaw, false).unwrap();
        assert!(parse(&exported[0].content).is_ok(), "boundary export must import: {section}");
        match section {
            "Persona" => p.soul.push('x'),
            "Identity" => p.identity.push('x'),
            "Rules" => p.rules.push('x'),
            _ => p.contract.push('x'),
        }
        assert!(validate_sections(&p).unwrap_err().to_string().contains(section));
        assert!(export(&p, ExportFormat::Duduclaw, false).is_err());
    }
}

#[test]
fn parser_checks_slot_limits_after_unknown_headings_are_merged() {
    let persona = plain(&format!("# Persona\n{}", "x".repeat(MAX_MARKDOWN_BYTES + 1)));
    assert!(parse(&persona).unwrap_err().to_string().contains("Persona exceeds"));
    let rules = plain(&format!("# Rules\n{}\n# 額外規則\nextra", "x".repeat(MAX_MARKDOWN_BYTES)));
    assert!(parse(&rules).unwrap_err().to_string().contains("Rules exceeds"));
    let contract = plain(&format!("# Contract\n#{}", "x".repeat(MAX_CONTRACT_BYTES)));
    assert!(parse(&contract).unwrap_err().to_string().contains("Contract exceeds"));
    let yaml_contract = plain(&format!("# Contract\nmust_not: ['{}']", "x".repeat(MAX_CONTRACT_BYTES)));
    assert!(parse(&yaml_contract).unwrap_err().to_string().contains("Contract exceeds"));
    let cjk = plain(&format!("# Persona\n{}", "字".repeat(MAX_MARKDOWN_BYTES / 3 + 1)));
    assert!(parse(&cjk).unwrap_err().to_string().contains("Persona exceeds"));
}

#[test]
fn oversized_excluded_user_does_not_block_native_export() {
    let mut p = profile();
    p.user = "x".repeat(MAX_MARKDOWN_BYTES + 1);
    assert!(validate_sections(&p).is_err());
    let exported = export(&p, ExportFormat::Duduclaw, false).unwrap();
    assert!(parse(&exported[0].content).unwrap().user.is_empty());
    assert!(export(&p, ExportFormat::Duduclaw, true).is_err());
}

#[test]
fn misplaced_contract_boundaries_never_silently_disappear() {
    assert!(parse(&plain("# Contract\nmust_not = ['No refunds']\n")).is_err());
    let mut source = profile();
    source.contract = "must_not = ['No refunds']\n".into();
    assert!(export(&source, ExportFormat::Duduclaw, false).is_err());
    let raw = plain("# Contract\n```duduclaw-contract\nmust_not = ['No refunds']\n\n```\n");
    assert!(parse(&raw).is_err());
}
