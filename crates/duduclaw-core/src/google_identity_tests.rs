use super::*;

#[test]
fn selector_values_parse_and_round_trip() {
    assert_eq!(Selector::parse(" HOME ").unwrap(), Selector::Home);
    assert_eq!(Selector::parse("none").unwrap(), Selector::None);
    assert_eq!(Selector::parse("owner").unwrap(), Selector::Owner);
    assert_eq!(
        Selector::parse("Delegate: Alice@Customer.COM ").unwrap(),
        Selector::Delegate("alice@customer.com".into())
    );
    assert_eq!(
        Selector::parse("user:u_7f3c").unwrap(),
        Selector::User("u_7f3c".into())
    );
    for sel in [
        Selector::Home,
        Selector::None,
        Selector::Delegate("a@b.co".into()),
        Selector::Owner,
        Selector::User("u1".into()),
    ] {
        assert_eq!(Selector::parse(&sel.to_config_value()).unwrap(), sel);
    }
}

#[test]
fn bad_selectors_are_errors() {
    for bad in [
        "",
        "delegate:",
        "delegate:alice",
        "delegate:@customer.com",
        "delegate:a@b@c.com",
        "delegate:a@localhost",
        "delegate:a b@c.com",
        "delegate:a@c..com",
        "delegate:a@-c.com",
        "user:",
        "user:a@b.com",
        "boss@customer.com",
        "公司",
        "delegate:a@c.com\u{0}",
    ] {
        assert!(Selector::parse(bad).is_err(), "{bad:?} must not parse");
    }
}

#[test]
fn multibyte_input_never_panics() {
    assert!(Selector::parse("委派:alice@c.com").is_err());
    assert!(Selector::parse("de").is_err());
    assert!(Selector::parse("delegate：a@c.com").is_err());
}

#[test]
fn section_is_strict() {
    assert_eq!(
        parse_agent_section("[agent]\nname='a'\n"),
        SectionRead::Absent
    );
    assert!(matches!(
        parse_agent_section("not toml ["),
        SectionRead::Invalid(_)
    ));
    assert!(matches!(
        parse_agent_section("google = 'x'\n"),
        SectionRead::Invalid(_)
    ));
    assert!(matches!(
        parse_agent_section("[google]\nidentiy = 'home'\n"),
        SectionRead::Invalid(_)
    ));
    assert!(matches!(
        parse_agent_section("[google]\nidentity = 3\n"),
        SectionRead::Invalid(_)
    ));
    assert!(matches!(
        parse_agent_section("[google]\nfallback = 'yes'\n"),
        SectionRead::Invalid(_)
    ));
    assert!(matches!(
        parse_agent_section("[google]\nrequesters = 'all'\n"),
        SectionRead::Invalid(_)
    ));
    let SectionRead::Present(s) = parse_agent_section(
        "[google]\nidentity = 'delegate:a@c.com'\nfallback = 'home'\nrequesters = 'owner_only'\n",
    ) else {
        panic!()
    };
    assert_eq!(s.identity, Some(Selector::Delegate("a@c.com".into())));
    assert_eq!(s.fallback, Fallback::Home);
    assert_eq!(s.requesters, Requesters::OwnerOnly);
    let SectionRead::Present(empty) = parse_agent_section("[google]\n") else {
        panic!()
    };
    assert_eq!(empty, AgentGoogleSection::default());
}

#[test]
fn typed_sections_isolate_bad_google_and_honor_the_effective_artifact() {
    let sections =
        crate::agent_toml::parse("[google]\nidentity=99\n[model]\npreferred='known-model'\n");
    assert!(matches!(sections.google, SectionRead::Invalid(_)));
    assert_eq!(sections.model.preferred.as_deref(), Some("known-model"));
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/bot");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "[google]\nidentity='home'\n").unwrap();
    let artifact = crate::preset::agent_resolved_path(home.path(), "bot");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, "[google]\nidentity='none'\n").unwrap();
    assert_eq!(
        effective_for(home.path(), Some("bot")).effective,
        Effective::Disabled
    );
    std::fs::write(&artifact, "[google]\nidentity=42\n").unwrap();
    assert!(matches!(
        effective_for(home.path(), Some("bot")).effective,
        Effective::Invalid(_)
    ));
}

#[test]
fn home_config_defaults_and_errors() {
    assert_eq!(
        parse_home_config("").unwrap(),
        HomeIdentityConfig::default()
    );
    let cfg = parse_home_config(
        "[integrations.google_identity]\ndefault_identity='none'\nallowed_subject_domains=['Customer.com']\n",
    )
    .unwrap();
    assert_eq!(cfg.default_identity, Selector::None);
    assert_eq!(
        cfg.allowed_subject_domains,
        vec!["customer.com".to_string()]
    );
    for bad in [
        "[integrations.google_identity]\ndefault_identity='delegate:a@c.com'\n",
        "[integrations.google_identity]\ndefault_identity='owner'\n",
        "[integrations.google_identity]\nallowed_subject_domains='c.com'\n",
        "[integrations.google_identity]\nallowed_subject_domains=['not a domain']\n",
        "[integrations.google_identity]\nextra=1\n",
        "[integrations]\ngoogle_identity = 1\n",
        "broken [",
    ] {
        assert!(parse_home_config(bad).is_err(), "{bad}");
    }
}

#[test]
fn domain_rule_is_exact() {
    let open = HomeIdentityConfig::default();
    assert!(
        !delegate_domain_allowed("a@c.com", &open, None),
        "no list, no subject: refuse"
    );
    assert!(delegate_domain_allowed(
        "a@c.com",
        &open,
        Some("Boss@C.com")
    ));
    assert!(!delegate_domain_allowed(
        "a@evil-c.com",
        &open,
        Some("boss@c.com")
    ));
    let listed = HomeIdentityConfig {
        allowed_subject_domains: vec!["c.com".into()],
        ..Default::default()
    };
    assert!(delegate_domain_allowed(
        "a@c.com",
        &listed,
        Some("boss@other.com")
    ));
    assert!(!delegate_domain_allowed("a@sub.c.com", &listed, None));
    assert!(!delegate_domain_allowed("a@c.com.evil.io", &listed, None));
}

#[test]
fn combine_follows_the_resolution_order() {
    let home_default = Ok(HomeIdentityConfig::default());
    let home_none = Ok(HomeIdentityConfig {
        default_identity: Selector::None,
        ..Default::default()
    });
    // No section: the home default.
    assert_eq!(
        combine(&SectionRead::Absent, &home_default).effective,
        Effective::Home
    );
    assert_eq!(
        combine(&SectionRead::Absent, &home_none).effective,
        Effective::Disabled
    );
    // A section without identity inherits the default but keeps its knobs.
    let s = SectionRead::Present(AgentGoogleSection {
        fallback: Fallback::Home,
        ..Default::default()
    });
    let e = combine(&s, &home_none);
    assert_eq!(e.effective, Effective::Disabled);
    assert!(!e.explicit);
    assert_eq!(e.fallback, Fallback::Home);
    // Explicit wins over the default.
    let d = SectionRead::Present(AgentGoogleSection {
        identity: Some(Selector::Delegate("a@c.com".into())),
        ..Default::default()
    });
    assert_eq!(
        combine(&d, &home_none).effective,
        Effective::Delegate {
            email: "a@c.com".into()
        }
    );
    // Invalid anywhere is Invalid, never Home.
    assert!(matches!(
        combine(&SectionRead::Invalid("x".into()), &home_default).effective,
        Effective::Invalid(_)
    ));
    assert!(matches!(
        combine(&SectionRead::Absent, &Err("x".into())).effective,
        Effective::Invalid(_)
    ));
    // B2b selectors reach the gateway's person gates.
    let o = SectionRead::Present(AgentGoogleSection {
        identity: Some(Selector::Owner),
        ..Default::default()
    });
    assert_eq!(
        combine(&o, &home_default).effective,
        Effective::Personal { selector: Selector::Owner }
    );
}

#[test]
fn listing_hides_only_none() {
    assert!(Effective::Home.tools_listed());
    assert!(
        Effective::Delegate {
            email: "a@c.com".into()
        }
        .tools_listed()
    );
    assert!(Effective::Invalid("x".into()).tools_listed());
    assert!(!Effective::Disabled.tools_listed());
}

#[test]
fn effective_for_reads_files() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("bot");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        "[google]\nidentity='delegate:a@c.com'\n",
    )
    .unwrap();
    assert_eq!(
        effective_for(home.path(), Some("bot")).effective,
        Effective::Delegate {
            email: "a@c.com".into()
        }
    );
    assert_eq!(
        effective_for(home.path(), Some("ghost")).effective,
        Effective::Home
    );
    assert_eq!(effective_for(home.path(), None).effective, Effective::Home);
    assert!(matches!(
        effective_for(home.path(), Some("../x")).effective,
        Effective::Invalid(_)
    ));
    std::fs::write(
        home.path().join("config.toml"),
        "[integrations.google_identity]\ndefault_identity='none'\n",
    )
    .unwrap();
    assert_eq!(
        effective_for(home.path(), None).effective,
        Effective::Disabled
    );
}
