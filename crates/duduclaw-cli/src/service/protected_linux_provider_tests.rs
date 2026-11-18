//! R8r-2 `service provider set` tests: argument shape, stdin only, the size
//! cap, fixed messages that never contain the input, AI-session refusal.
use super::*;
use std::io::Cursor;

const KEY: &str = "sk-test-NEVER-SHOWN-0123456789";
fn document(providers: &str) -> Vec<u8> { format!(r#"{{"providers":[{providers}]}}"#).into_bytes() }
fn provider(name: &str, base_url: &str) -> String {
    format!(r#"{{"name":"{name}","api_key":"{KEY}","base_url":"{base_url}"}}"#)
}
fn set(employee_id: Option<&str>, employee: Option<&str>, clear: bool) -> ProviderCommand {
    ProviderCommand::Set { protected_linux: true, employee_id: employee_id.map(Into::into), employee: employee.map(Into::into), clear }
}

#[test]
fn service_provider_set_cli_parses_exact_arguments() {
    use clap::Parser;
    crate::test_support::run_on_big_stack(|| {
        let parse = |extra: &[&str]| {
            let mut args = vec!["duduclaw", "service", "provider", "set"];
            args.extend_from_slice(extra);
            crate::Cli::try_parse_from(args)
        };
        let cli = parse(&["--protected-linux", "--employee", "sales"]).unwrap();
        assert!(matches!(cli.command, crate::Commands::Ops(crate::OpsCommands::Service {
            command: crate::ServiceCommands::Provider { command: ProviderCommand::Set {
                protected_linux: true, employee_id: None, employee: Some(ref label), clear: false } } }) if label == "sales"));
        let cli = parse(&["--protected-linux", "--employee-id", "0123456789abcdef0123456789abcdef", "--clear"]).unwrap();
        assert!(matches!(cli.command, crate::Commands::Ops(crate::OpsCommands::Service {
            command: crate::ServiceCommands::Provider { command: ProviderCommand::Set { employee_id: Some(_), employee: None, clear: true, .. } } })));
        for bad in [&["--employee", "sales"][..], &["--protected-linux"], &["--protected-linux", "--employee", "a", "--employee-id", "0123456789abcdef0123456789abcdef"],
            &["--protected-linux", "--employee", "sales", "providers.json"], &["--protected-linux", "--employee", "sales", "--api-key", "x"],
            &["--protected-linux", "--employee", "sales", "--file", "providers.json"]] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    });
}

#[test]
fn service_provider_set_targets_are_checked_with_fixed_messages() {
    assert_eq!(target(None, Some("sales")).unwrap(), Target::Label("sales".into()));
    assert_eq!(target(Some("0123456789abcdef0123456789abcdef"), None).unwrap(), Target::Id("0123456789abcdef0123456789abcdef".into()));
    for bad in ["0123456789ABCDEF0123456789ABCDEF", "00000000000000000000000000000000", "abc", "0123456789abcdef0123456789abcdeg"] {
        let error = target(Some(bad), None).unwrap_err().to_string();
        assert!(error.contains("--employee-id") && !error.contains(bad), "{error}");
    }
    let long = "x".repeat(65);
    for bad in ["", "sa les", "../etc", long.as_str()] {
        let error = target(None, Some(bad)).unwrap_err().to_string();
        assert!(error.contains("--employee") && (bad.is_empty() || !error.contains(bad)), "{error}");
    }
    assert!(target(None, None).is_err());
    assert!(target(Some("0123456789abcdef0123456789abcdef"), Some("sales")).is_err());
}

#[test]
fn service_provider_set_reads_stdin_only_with_a_terminal_hint_and_a_size_cap() {
    let hint = read_document(&mut Cursor::new(document(&provider("main", "https://llm.example/v1"))), true).unwrap_err().to_string();
    assert!(hint.contains("標準輸入") && hint.contains(r#""providers""#) && hint.contains("api_key") && !hint.contains(KEY), "{hint}");
    let exact = vec![b' '; MAX_DOCUMENT];
    assert_eq!(read_document(&mut Cursor::new(exact.clone()), false).unwrap().as_slice(), exact.as_slice());
    let over = read_document(&mut Cursor::new(vec![b' '; MAX_DOCUMENT + 1]), false).unwrap_err().to_string();
    assert!(over.contains("65,536"), "{over}");
    assert!(read_document(&mut Cursor::new(Vec::new()), false).unwrap_err().to_string().contains("沒有內容"));
    let read = read_document(&mut Cursor::new(document(&provider("main", "https://llm.example/v1"))), false).unwrap();
    assert_eq!(read.as_slice(), document(&provider("main", "https://llm.example/v1")).as_slice());
}

#[test]
fn service_provider_validation_failures_are_fixed_and_never_contain_the_input() {
    let long = format!(r#"{{"name":"main","api_key":"{KEY}","base_url":"https://llm.example/v1","extra":"{KEY}"}}"#);
    let many: Vec<String> = (0..33).map(|i| provider(&format!("p{i}"), "https://llm.example/v1")).collect();
    let bad: Vec<Vec<u8>> = vec![
        format!("not json {KEY}").into_bytes(),
        document(""),
        document(&provider("main", "http://llm.example/v1")),
        document(&provider("main", "https://user:pass@llm.example/v1")),
        document(&provider("main", "https://llm.example/v1?key=x")),
        document(&provider("main", "https://llm.example/v1#frag")),
        document(&format!("{},{}", provider("main", "https://a.example/v1"), provider("main", "https://b.example/v1"))),
        document(&long),
        document(&many.join(",")),
        format!(r#"{{"providers":[{{"name":"main","api_key":"{KEY}"}}]}}"#).into_bytes(),
    ];
    for document in bad {
        let error = validate(&document).unwrap_err().to_string();
        assert!(error.contains("格式不正確") && !error.contains(KEY) && !error.contains("llm.example"), "{error}");
    }
}

#[test]
fn service_provider_validation_agrees_with_the_runtime_select() {
    let fixtures = [
        document(&provider("main", "https://llm.example/v1")),
        document(&format!("{},{}", provider("a", "https://a.example/v1"), provider("b", "https://b.example:8443/v1"))),
        document(&format!("{},{}", provider("a", "https://a.example/v1"), provider("a", "https://b.example/v1"))),
        document(&provider("main", "http://llm.example/v1")),
        document(""),
    ];
    for fixture in fixtures {
        let ours = validate(&fixture).is_ok();
        let parsed: duduclaw_api_worker::Credentials = serde_json::from_slice(&fixture).unwrap();
        let first = parsed.providers.first().map(|c| c.name.clone());
        let theirs = parsed.select(first.as_deref().or(Some("none"))).is_ok();
        assert_eq!(ours, theirs, "{}", String::from_utf8_lossy(&fixture).replace(KEY, "<key>"));
    }
}

#[test]
fn service_provider_success_line_shows_names_and_origins_only() {
    let summary = validate(&document(&format!("{},{}", provider("main", "https://llm.example:8443/v1/private-path"),
        provider("backup", "https://backup.example/openai/v1")))).unwrap();
    assert_eq!(summary.providers, vec![("main".to_string(), "https://llm.example:8443".to_string()),
        ("backup".to_string(), "https://backup.example".to_string())]);
    let line = success_line(&Target::Label("sales".into()), Some(&summary));
    assert_eq!(line, "已寫入員工 sales 的模型連線資料：2 個提供者：main（https://llm.example:8443）、backup（https://backup.example）。");
    assert!(!line.contains(KEY) && !line.contains("private-path") && !line.contains("/v1") && !line.contains('\n'));
    let cleared = success_line(&Target::Id("0123456789abcdef0123456789abcdef".into()), None);
    assert_eq!(cleared, "已移除員工 0123456789abcdef0123456789abcdef 的模型連線資料。");
}

#[test]
fn service_provider_set_refuses_every_ai_session_marker_and_non_root() {
    for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
        let _session = crate::ai_session_guard::test_session(&[(marker, "")]);
        for clear in [false, true] {
            let error = handle(set(None, Some("sales"), clear)).unwrap_err().to_string();
            assert!(error.contains("AI 員工不能"), "{error}");
        }
        assert!(authorize(0, 0).unwrap_err().to_string().contains("AI 員工不能"));
    }
    for (uid, gid) in [(1000, 1000), (0, 1000), (1000, 0)] {
        assert!(authorize(uid, gid).unwrap_err().to_string().contains("root"), "{uid}:{gid}");
    }
    assert!(authorize(0, 0).is_ok());
}

#[cfg(target_os = "linux")]
#[test]
fn service_provider_refusal_lines_are_fixed() {
    use duduclaw_migration::allocation::providers::ProviderErrorCode as Code;
    let mut seen = std::collections::BTreeSet::new();
    for code in [Code::NotAllocated, Code::NotReady, Code::InvalidPayload, Code::HomeEntryUnsafe, Code::StorageUnavailable,
        Code::InvalidRequest, Code::IsolationUnavailable] {
        let line = refusal_line(code);
        assert!(seen.insert(line) && !line.is_empty() && !line.contains('{'), "{code:?}");
    }
}
