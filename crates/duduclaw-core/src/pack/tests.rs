//! Unit tests for [`super`], moved verbatim out of the former `pack.rs`.

use super::*;

const CANONICAL: &str = r#"
[pack]
schema = 1
id = "clinic-team"
kind = "team"
tier = "premium"
version = "1.2.0"
description = "clinic team"
category = "health"
tags = ["clinic"]
examples = ["擬一份術後關懷訊息"]

[pack.display_name]
"zh-TW" = "醫美／牙醫診所"
"en" = "Clinic"

[pack.requires]
env = ["ODOO_URL"]
bins = ["soffice"]

[pack.prompts]
recommended = ["幫我排今天的回診提醒"]

[pack.channels]
suggested = ["line"]

[[pack.agents]]
name = "clinic-assistant"
role = "front_desk"
display_name = "診所前台助理"

[[pack.agents]]
name = "clinic-billing"
role = "worker"
reports_to = "clinic-assistant"
department = "帳務"
overlay = ["不得做任何醫療診斷"]

[[pack.humans]]
title = "院長"
summary = "最終醫療決策"

[[pack.excluded]]
kit = "hr-admin"
reason = "涉及個資"

[pack.eval_suites]
clinic-assistant = "evals/clinic-assistant"

[[pack.autopilot_rules]]
name = "新客詢問自動分派"
trigger_event = "channel_message"
"#;

const LEGACY_EXPERT: &str = r#"
[expert]
name = "clinic-team"
description = "clinic team"
version = "1.2.0"
category = "health"
tags = ["clinic"]

[expert.display_name]
"zh-TW" = "醫美／牙醫診所"
"en" = "Clinic"

[expert.requires]
env = ["ODOO_URL"]
bins = ["soffice"]

[expert.prompts]
recommended = ["幫我排今天的回診提醒"]

[expert.channels]
suggested = ["line"]

[[expert.agents]]
name = "clinic-assistant"
role = "front_desk"
display_name = "診所前台助理"

[[expert.agents]]
name = "clinic-billing"
role = "worker"
reports_to = "clinic-assistant"
department = "帳務"
"#;

const LEGACY_TEAM: &str = r#"
schema = 1
industry = "clinic"
pack = "clinic-pro"
label = "醫美／牙醫診所"
examples = ["擬一份術後關懷訊息"]

[front_desk]
name = "clinic-assistant"
display_name = "診所前台助理"
summary = "對外唯一窗口"

[[workers]]
kit = "billing-admin"
name = "clinic-billing"
display_name = "帳務請款助理"
trigger = "clinic-billing"
summary = "自費請款通知"
overlay = ["不得做任何醫療診斷"]

[[humans]]
title = "院長"
summary = "最終醫療決策"

[[excluded]]
kit = "hr-admin"
reason = "涉及個資"
"#;

const LEGACY_PRESET: &str = r#"
[preset]
version = "1.0.0"
label = "帳務請款助理"
description = "共用部門職務組合"

[model]
preferred = "claude-haiku-4-5"

[capabilities]
allowed_tools = []
"#;

// ── canonical ──

#[test]
fn canonical_round_trips_every_section() {
    let p = parse_pack("clinic-team", CANONICAL).unwrap();
    assert_eq!(p.id, "clinic-team");
    assert_eq!(p.kind, PackKind::Team);
    assert_eq!(p.tier, PackTier::Premium);
    assert_eq!(p.version, "1.2.0");
    assert_eq!(p.display("en"), "Clinic");
    assert_eq!(p.display("de"), "醫美／牙醫診所", "locale → zh-TW fallback");
    assert_eq!(p.category, "health");
    assert_eq!(p.tags, vec!["clinic"]);
    assert_eq!(p.examples, vec!["擬一份術後關懷訊息"]);
    assert_eq!(p.requires.env, vec!["ODOO_URL"]);
    assert_eq!(p.requires.bins, vec!["soffice"]);
    assert_eq!(p.prompts.len(), 1);
    assert_eq!(p.channels, vec!["line"]);
    assert_eq!(p.agents.len(), 2);
    assert_eq!(p.agents[1].overlay, vec!["不得做任何醫療診斷"]);
    assert_eq!(p.humans[0].title, "院長");
    assert_eq!(p.excluded[0].kit, "hr-admin");
    assert_eq!(
        p.eval_suites.get("clinic-assistant").map(String::as_str),
        Some("evals/clinic-assistant")
    );
    assert_eq!(p.autopilot_rules.len(), 1);
    assert_eq!(p.source, PackSource::Canonical);
    assert!(p.lint().is_empty(), "lint: {:?}", p.lint());
}

#[test]
fn canonical_refuses_future_schema_instead_of_guessing() {
    let src = CANONICAL.replace("schema = 1", "schema = 99");
    assert_eq!(
        parse_pack("clinic-team", &src).unwrap_err(),
        PackError::SchemaTooNew {
            found: 99,
            supported: PACK_SCHEMA
        }
    );
}

#[test]
fn canonical_refuses_unknown_kind() {
    let src = CANONICAL.replace(r#"kind = "team""#, r#"kind = "spaceship""#);
    assert!(matches!(
        parse_pack("x", &src).unwrap_err(),
        PackError::Invalid(_)
    ));
}

#[test]
fn unknown_tier_fails_closed_to_premium() {
    assert_eq!(PackTier::parse("free"), PackTier::Free);
    assert_eq!(PackTier::parse("premium"), PackTier::Premium);
    assert_eq!(PackTier::parse(""), PackTier::Premium);
    assert_eq!(PackTier::parse("gratis"), PackTier::Premium);
}

#[test]
fn canonical_preset_refuses_org_fields_whole_pack() {
    let src = r#"
[pack]
schema = 1
kind = "preset"
tier = "free"
version = "1.0.0"
label = "x"

[pack.config.agent]
reports_to = "new-boss"

[pack.config.model]
preferred = "claude-haiku-4-5"
"#;
    assert!(matches!(
        parse_pack("evil", src).unwrap_err(),
        PackError::OrgFieldsRejected(_)
    ));
}

#[test]
fn canonical_preset_strips_secret_tables() {
    let src = r#"
[pack]
schema = 1
kind = "preset"
tier = "free"
version = "1.0.0"
label = "x"

[pack.config.channels.discord]
bot_token = "secret"

[pack.config.model]
preferred = "claude-haiku-4-5"
"#;
    let p = parse_pack("p", src).unwrap();
    assert!(!p.config.contains_key("channels"), "secrets never survive");
    assert!(p.config.contains_key("model"));
}

#[test]
fn directory_name_wins_over_declared_id_and_is_reported() {
    let src = CANONICAL.replace(r#"id = "clinic-team""#, r#"id = "clinic-assistant""#);
    let p = parse_pack("clinic-team", &src).unwrap();
    assert_eq!(p.id, "clinic-team");
    assert_eq!(p.id_mismatch.as_deref(), Some("clinic-assistant"));
    assert!(p.lint().iter().any(|l| l.contains("不符")));
}

// ── legacy → canonical equivalence ──

/// Fields the canonical and legacy-expert fixtures both declare must come
/// out identical; only `source` (and the legacy-only absence of examples /
/// tier / humans) may differ.
#[test]
fn legacy_expert_matches_the_canonical_fixture_field_by_field() {
    let canonical = parse_pack("clinic-team", CANONICAL).unwrap();
    let legacy = parse_legacy_expert("clinic-team", LEGACY_EXPERT).unwrap();

    assert_eq!(legacy.source, PackSource::LegacyExpert);
    assert_eq!(legacy.kind, canonical.kind);
    assert_eq!(legacy.id, canonical.id);
    assert_eq!(legacy.version, canonical.version);
    assert_eq!(legacy.display_name, canonical.display_name);
    assert_eq!(legacy.description, canonical.description);
    assert_eq!(legacy.category, canonical.category);
    assert_eq!(legacy.tags, canonical.tags);
    assert_eq!(legacy.requires, canonical.requires);
    assert_eq!(legacy.prompts, canonical.prompts);
    assert_eq!(legacy.channels, canonical.channels);
    assert_eq!(legacy.agents.len(), canonical.agents.len());
    for (l, c) in legacy.agents.iter().zip(canonical.agents.iter()) {
        assert_eq!(l.name, c.name);
        assert_eq!(l.role, c.role);
        assert_eq!(l.display_name, c.display_name);
        assert_eq!(l.reports_to, c.reports_to);
        assert_eq!(l.department, c.department);
    }
    assert!(legacy.lint().is_empty(), "lint: {:?}", legacy.lint());
}

#[test]
fn legacy_team_builds_a_front_desk_rooted_roster() {
    let p = parse_legacy_team("clinic-team", LEGACY_TEAM).unwrap();
    assert_eq!(p.source, PackSource::LegacyTeam);
    assert_eq!(p.kind, PackKind::Team);
    assert_eq!(p.tier, PackTier::Premium, "team playbooks are premium-only");
    assert_eq!(p.display("zh-TW"), "醫美／牙醫診所");
    assert_eq!(p.examples, vec!["擬一份術後關懷訊息"]);
    assert_eq!(p.tags, vec!["clinic", "clinic-pro"]);

    assert_eq!(p.agents.len(), 2);
    let fd = &p.agents[0];
    assert_eq!(fd.name, "clinic-assistant");
    assert_eq!(fd.role, "front_desk");
    assert_eq!(fd.reports_to, "");
    assert_eq!(fd.rank, "manager");
    assert_eq!(fd.trigger, "@診所前台助理");
    assert_eq!(
        fd.department, "",
        "convert-teams omits the front desk's department — parity, not an oversight"
    );

    let w = &p.agents[1];
    assert_eq!(w.name, "clinic-billing");
    assert_eq!(w.role, "worker");
    assert_eq!(
        w.reports_to, "clinic-assistant",
        "workers report to the desk"
    );
    assert_eq!(w.rank, "staff");
    assert_eq!(w.trigger, "clinic-billing");
    assert_eq!(w.kit, "billing-admin");
    assert_eq!(w.overlay, vec!["不得做任何醫療診斷"]);
    assert_eq!(
        w.department,
        org::department_for_kit("billing-admin").unwrap(),
        "kit → department fallback matches the converter"
    );

    assert_eq!(p.humans[0].title, "院長");
    assert_eq!(p.excluded[0].kit, "hr-admin");
    assert_eq!(p.roots().count(), 1);
}

#[test]
fn legacy_team_worker_department_override_wins_over_kit_default() {
    let src = LEGACY_TEAM.replace(
        r#"kit = "billing-admin""#,
        "kit = \"billing-admin\"\ndepartment = \"財務\"",
    );
    let p = parse_legacy_team("clinic-team", &src).unwrap();
    assert_eq!(p.agents[1].department, "財務");
}

#[test]
fn legacy_team_blank_display_and_trigger_fall_back_to_name() {
    let src = r#"
industry = "x"
label = "X"

[front_desk]
name = "x-assistant"

[[workers]]
kit = "inventory"
name = "x-stock"
"#;
    let p = parse_legacy_team("x-team", src).unwrap();
    assert_eq!(p.agents[0].display_name, "x-assistant");
    assert_eq!(p.agents[0].trigger, "@x-assistant");
    assert_eq!(p.agents[1].display_name, "x-stock");
    assert_eq!(p.agents[1].trigger, "x-stock");
}

#[test]
fn legacy_team_without_front_desk_is_refused() {
    assert!(matches!(
        parse_legacy_team("x", "industry = \"a\"\n").unwrap_err(),
        PackError::Invalid(_)
    ));
}

#[test]
fn legacy_preset_reuses_the_preset_sanitizer() {
    let p = parse_legacy_preset("billing-admin", LEGACY_PRESET).unwrap();
    assert_eq!(p.source, PackSource::LegacyPreset);
    assert_eq!(p.kind, PackKind::Preset);
    assert_eq!(p.tier, PackTier::Free);
    assert_eq!(p.version, "1.0.0");
    assert_eq!(p.display("zh-TW"), "帳務請款助理");
    assert!(p.agents.is_empty());
    assert!(p.config.contains_key("model"));
    assert!(!p.config.contains_key("preset"), "metadata never leaks");
    assert!(p.lint().is_empty(), "lint: {:?}", p.lint());
}

#[test]
fn canonical_preset_emits_a_loadable_legacy_preset_toml() {
    let src = r#"
[pack]
schema = 1
kind = "preset"
tier = "free"
version = "2.0.0"
label = "帳務請款助理"
description = "共用部門職務組合"

[pack.config.model]
preferred = "claude-haiku-4-5"

[pack.config.capabilities]
allowed_tools = []
"#;
    let canonical = parse_pack("billing-admin", src).unwrap();
    let emitted = canonical.to_legacy_preset_toml().unwrap();

    // Round-trips through the *live* preset reader, not a mirror of it.
    let reread = crate::preset::parse_preset("billing-admin", &emitted).unwrap();
    assert_eq!(reread.meta.version, "2.0.0");
    assert_eq!(reread.meta.label, "帳務請款助理");
    assert_eq!(reread.config, canonical.config);

    // …and back through this module to the same Pack.
    let back = parse_legacy_preset("billing-admin", &emitted).unwrap();
    assert_eq!(back.version, canonical.version);
    assert_eq!(back.display_name, canonical.display_name);
    assert_eq!(back.config, canonical.config);

    assert!(
        parse_legacy_expert("x", LEGACY_EXPERT)
            .unwrap()
            .to_legacy_preset_toml()
            .is_none(),
        "a team pack renders no preset.toml"
    );
}

#[test]
fn legacy_preset_org_fields_are_refused_as_a_whole_pack() {
    let src = format!("{LEGACY_PRESET}\n[agent]\nreports_to = \"boss\"\n");
    assert!(matches!(
        parse_legacy_preset("evil", &src).unwrap_err(),
        PackError::OrgFieldsRejected(_)
    ));
}

#[test]
fn legacy_industry_prefers_manifest_label_and_fails_closed_to_fallback() {
    let good = parse_legacy_industry(
        "logistics-pro",
        Some("label = \"物流貨運 (Pro)\"\n"),
        "logistics-pro (Pro)",
    )
    .unwrap();
    assert_eq!(good.kind, PackKind::Template);
    assert_eq!(good.tier, PackTier::Premium);
    assert_eq!(good.display("zh-TW"), "物流貨運 (Pro)");

    // Control characters / oversize / empty → the caller's table wins.
    for bad in [
        "label = \"\"\n",
        "label = \"bad\\u0007label\"\n",
        &format!("label = \"{}\"\n", "x".repeat(200)),
    ] {
        let p =
            parse_legacy_industry("logistics-pro", Some(bad), "logistics-pro (Pro)").unwrap();
        assert_eq!(p.display("zh-TW"), "logistics-pro (Pro)", "for {bad:?}");
    }

    let none = parse_legacy_industry("x-pro", None, "x-pro (Pro)").unwrap();
    assert_eq!(none.display("zh-TW"), "x-pro (Pro)");
}

// ── directory dispatch ──

#[test]
fn detect_and_load_dispatch_by_manifest_precedence() {
    let tmp = tempfile::tempdir().unwrap();

    let mk = |name: &str, files: &[(&str, &str)]| {
        let d = tmp.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        for (f, body) in files {
            std::fs::write(d.join(f), body).unwrap();
        }
        d
    };

    let canon = mk("clinic-team", &[("pack.toml", CANONICAL)]);
    assert_eq!(detect_dir(&canon), Some(PackSource::Canonical));
    assert_eq!(load_dir(&canon, "").unwrap().source, PackSource::Canonical);

    let exp = mk("expert-pack", &[("expert.toml", LEGACY_EXPERT)]);
    assert_eq!(load_dir(&exp, "").unwrap().source, PackSource::LegacyExpert);

    let team = mk("some-team", &[("team.toml", LEGACY_TEAM)]);
    assert_eq!(load_dir(&team, "").unwrap().source, PackSource::LegacyTeam);

    let pre = mk("billing-admin", &[("preset.toml", LEGACY_PRESET)]);
    assert_eq!(load_dir(&pre, "").unwrap().source, PackSource::LegacyPreset);

    let ind = mk("logistics-pro", &[("SOUL.md", "# persona")]);
    assert_eq!(
        load_dir(&ind, "物流 (Pro)").unwrap().display("zh-TW"),
        "物流 (Pro)"
    );

    // Canonical wins when both are present (the migration direction).
    let both = mk(
        "dual",
        &[("pack.toml", CANONICAL), ("expert.toml", LEGACY_EXPERT)],
    );
    assert_eq!(detect_dir(&both), Some(PackSource::Canonical));

    let empty = mk("nothing", &[("README.md", "hi")]);
    assert_eq!(detect_dir(&empty), None);
    assert!(matches!(
        load_dir(&empty, "").unwrap_err(),
        PackError::NotFound(_)
    ));

    let listed = list_dir(tmp.path());
    assert_eq!(
        listed.len(),
        6,
        "every manifest-bearing dir listed, README-only skipped: {listed:?}"
    );
}

// ── Golden: this loader agrees with `expert convert-teams` output ──
//
// `commercial/templates-premium/experts/<industry>-team/expert.toml` IS
// the converter's committed output (its header says so) and is what the
// built-in catalog installs. Reading the *source*
// `teams/<industry>-team/team.toml` through `parse_legacy_team` must land
// on the same roster on every field that decides what an installed agent
// directory looks like — otherwise "the pack you inspect" and "the pack
// you install" disagree.
//
// The premium tree is gitignored, so this skips loudly when it is absent
// instead of passing vacuously. Resolution is anchored on
// `CARGO_MANIFEST_DIR` (cargo runs tests with cwd = the package root, not
// the workspace root).

fn premium_tree_for_tests() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
    let p = root.join("commercial").join("templates-premium");
    p.is_dir().then_some(p)
}

#[test]
fn builtin_team_packs_match_convert_teams_output() {
    let Some(premium) = premium_tree_for_tests() else {
        eprintln!("SKIP builtin_team_packs_match_convert_teams_output: premium tree absent");
        return;
    };
    let mut checked = 0usize;
    for industry in ["clinic", "lawfirm", "pharmacy"] {
        let src = premium.join("teams").join(format!("{industry}-team"));
        let converted = premium.join("experts").join(format!("{industry}-team"));
        if !src.join(LEGACY_TEAM_FILE).is_file()
            || !converted.join(LEGACY_EXPERT_FILE).is_file()
        {
            eprintln!("SKIP {industry}: source or converted pack missing");
            continue;
        }
        let t = load_dir(&src, "").unwrap_or_else(|e| panic!("{industry} team.toml: {e}"));
        let c =
            load_dir(&converted, "").unwrap_or_else(|e| panic!("{industry} expert.toml: {e}"));
        assert_eq!(t.source, PackSource::LegacyTeam, "{industry}");
        assert_eq!(c.source, PackSource::LegacyExpert, "{industry}");
        assert_eq!(t.kind, PackKind::Team, "{industry}");
        assert_eq!(c.kind, PackKind::Team, "{industry}");

        // Compare by NAME, not index/length: the committed converter
        // output is a cache that can lag its source (as of 2026-09-29
        // `lawfirm-team/team.toml` declares 5 workers while the committed
        // `experts/lawfirm-team/expert.toml` has 3 — the playbook grew
        // after the last `convert-teams` run). That is a staleness fact
        // about the content tree, not a loader disagreement, so what is
        // pinned here is: every member the converter emitted exists in the
        // team.toml reading with identical install-determining fields.
        let by_name: BTreeMap<&str, &PackAgent> =
            t.agents.iter().map(|a| (a.name.as_str(), a)).collect();
        let mut matched = 0usize;
        for b in &c.agents {
            let a = by_name.get(b.name.as_str()).unwrap_or_else(|| {
                panic!(
                    "{industry}: convert-teams emitted '{}' but team.toml has no such member",
                    b.name
                )
            });
            assert_eq!(a.role, b.role, "{industry} / {}", a.name);
            assert_eq!(a.display_name, b.display_name, "{industry} / {}", a.name);
            assert_eq!(a.reports_to, b.reports_to, "{industry} / {}", a.name);
            assert_eq!(a.department, b.department, "{industry} / {}", a.name);
            assert_eq!(a.rank, b.rank, "{industry} / {}", a.name);
            assert_eq!(a.trigger, b.trigger, "{industry} / {}", a.name);
            matched += 1;
        }
        assert!(
            matched >= 2,
            "{industry}: expected at least a front desk and one worker, matched {matched}"
        );
        if t.agents.len() != c.agents.len() {
            eprintln!(
                "NOTE {industry}: experts/{industry}-team/expert.toml is stale ({} members) \
                     vs teams/{industry}-team/team.toml ({} members) — re-run `convert-teams`",
                c.agents.len(),
                t.agents.len()
            );
        }

        // Two deliberate asymmetries, both missing *input* rather than a
        // lossy read: the front desk's dispatch skill is mined from
        // TEAM.md (a sibling file team.toml never references), and
        // convert-teams stamps a constant version team.toml itself does
        // not declare. The loader invents neither.
        assert!(
            t.agents[0].skills.is_empty(),
            "{industry}: team.toml carries no skill list"
        );
        let lint = t.lint();
        assert!(
            lint.iter().all(|l| l.contains("version")),
            "{industry} team.toml lint beyond the known absent version: {lint:?}"
        );
        assert!(c.lint().is_empty(), "{industry} converted lint: {:?}", c.lint());
        checked += 1;
    }
    assert!(checked >= 3, "expected 3 sampled teams, checked {checked}");
}

#[test]
fn builtin_standalone_expert_packs_load_as_team_packs() {
    let Some(premium) = premium_tree_for_tests() else {
        eprintln!("SKIP builtin_standalone_expert_packs_load_as_team_packs: tree absent");
        return;
    };
    let mut checked = 0usize;
    for slug in ["cad-drafter", "marketing-designer"] {
        let dir = premium.join("experts").join(slug);
        if !dir.join(LEGACY_EXPERT_FILE).is_file() {
            eprintln!("SKIP {slug}: not present");
            continue;
        }
        let p = load_dir(&dir, "").unwrap_or_else(|e| panic!("{slug}: {e}"));
        assert_eq!(p.id, slug);
        assert_eq!(p.kind, PackKind::Team, "{slug}");
        assert_eq!(p.source, PackSource::LegacyExpert, "{slug}");
        assert!(!p.agents.is_empty(), "{slug} has a roster");
        assert!(p.lint().is_empty(), "{slug} lint: {:?}", p.lint());
        checked += 1;
    }
    assert_eq!(checked, 2, "expected both standalone experts present");
}

#[test]
fn pharmacy_pro_slug_mismatch_is_reported_never_silently_dropped() {
    let Some(premium) = premium_tree_for_tests() else {
        eprintln!("SKIP pharmacy_pro_slug_mismatch: premium tree absent");
        return;
    };
    let dir = premium.join("experts").join("pharmacy-pro");
    if !dir.join(LEGACY_EXPERT_FILE).is_file() {
        eprintln!("SKIP pharmacy_pro_slug_mismatch: pack not present");
        return;
    }
    let p = load_dir(&dir, "").unwrap();
    assert_eq!(p.id, "pharmacy-pro", "the directory name is the install key");
    if let Some(declared) = &p.id_mismatch {
        assert_ne!(declared, "pharmacy-pro");
        assert!(
            p.lint().iter().any(|l| l.contains("不符")),
            "a mismatch must surface as a warning, never a silent drop: {:?}",
            p.lint()
        );
    }
}

#[test]
fn lint_catches_roster_shape_problems() {
    let src = r#"
[pack]
schema = 1
kind = "team"
tier = "free"
version = "1.0.0"
label = "x"

[[pack.agents]]
name = "a"
reports_to = "ghost"

[[pack.agents]]
name = "a"
"#;
    let p = parse_pack("dup", src).unwrap();
    let lint = p.lint();
    assert!(lint.iter().any(|l| l.contains("重複")), "{lint:?}");
    assert!(lint.iter().any(|l| l.contains("ghost")), "{lint:?}");
}

#[test]
fn effective_rank_derives_from_role_when_absent() {
    let a = PackAgent {
        role: "ceo".into(),
        ..PackAgent::default()
    };
    assert_eq!(a.effective_rank(), org::rank_for_role("ceo").as_str());
    let b = PackAgent {
        role: "ceo".into(),
        rank: "staff".into(),
        ..PackAgent::default()
    };
    assert_eq!(b.effective_rank(), "staff", "explicit rank wins");
}
