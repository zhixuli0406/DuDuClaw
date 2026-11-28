use super::config::*;

fn write_settings(home: &std::path::Path, actor: &str, update: &SettingsUpdate,
    ack: bool, now: &str) -> Result<(GoogleSigninSettings, GoogleSigninSettings), String> {
    let hash = read_with_hash(home)?.1;
    super::config::write_settings(home, actor, update, ack, now, &hash).map(|(before, after, _)| (before, after))
}

fn disable_settings(home: &std::path::Path, actor: &str, now: &str)
    -> Result<(GoogleSigninSettings, GoogleSigninSettings), String> {
    let hash = read_with_hash(home)?.1;
    super::config::disable_settings(home, actor, now, &hash).map(|(before, after, _)| (before, after))
}

const NOW: &str = "2026-11-27T00:00:00Z";
const ACTIVE: &str = "[google_signin]\nenabled=true\nallowed_hd=['example.com']\nenabled_ack={by='admin',at='2026-11-27T00:00:00Z'}\n";

fn read(body: &str) -> GoogleSigninSettings {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), body).unwrap();
    read_strict(home.path())
}

fn update() -> SettingsUpdate {
    SettingsUpdate {
        enabled: true, allowed_hd: vec!["EXAMPLE.COM".into()], approval: Approval::Admin,
        daily_new_accounts: 20, avatar_template: AvatarTemplate::Blank,
        approval_grants_mfa_enrollment: false,
    }
}

#[test]
fn unreadable_and_malformed_settings_are_disabled() {
    let home = tempfile::tempdir().unwrap();
    assert!(!read_strict(home.path()).enabled);
    std::fs::create_dir(home.path().join("config.toml")).unwrap();
    assert!(!read_strict(home.path()).enabled);
    for text in ["broken=[", "google_signin=true", "google_signin='bad'"] {
        let settings = read(text);
        assert!(!settings.enabled);
        assert!(!settings.enabled_requested);
        assert!(settings.allowed_hd.is_empty());
        assert_eq!(settings.approval, Approval::Admin);
        assert_eq!(settings.daily_new_accounts, 20);
    }
}

#[test]
fn enable_requires_boolean_ack_and_domains() {
    for text in [
        "[google_signin]\nenabled=true\nallowed_hd=['example.com']",
        "[google_signin]\nenabled=true\nenabled_ack={by='admin',at='now'}\nallowed_hd=[]",
        "[google_signin]\nenabled=true\nenabled_ack={by='',at='now'}\nallowed_hd=['example.com']",
        "[google_signin]\nenabled=true\nenabled_ack={by='admin',at=1}\nallowed_hd=['example.com']",
        "[google_signin]\nenabled='true'\nenabled_ack={by='admin',at='now'}\nallowed_hd=['example.com']",
    ] { assert!(!read(text).enabled, "{text}"); }
    assert!(read(ACTIVE).enabled);
}

#[test]
fn domains_are_all_or_nothing_normalized_and_deduplicated() {
    for domains in ["['example.com','bad/domain']", "['example.com',1]", "'example.com'", "['例子.tw']"] {
        let settings = read(&format!("[google_signin]\nallowed_hd={domains}"));
        assert!(settings.allowed_hd.is_empty());
        assert!(!settings.enabled);
    }
    assert_eq!(read("[google_signin]\nallowed_hd=['EXAMPLE.COM','example.com','other.com']").allowed_hd,
        vec!["example.com", "other.com"]);
}

#[test]
fn approval_daily_and_mfa_types_fail_closed() {
    assert_eq!(read("[google_signin]").daily_new_accounts, 20);
    for value in ["0", "201", "'20'", "true", "-1"] {
        assert_eq!(read(&format!("[google_signin]\ndaily_new_accounts={value}")).daily_new_accounts, 0);
    }
    for value in ["1", "200"] {
        assert_eq!(read(&format!("[google_signin]\ndaily_new_accounts={value}")).daily_new_accounts, value.parse::<u32>().unwrap());
    }
    for value in ["'unknown'", "'Direct'", "true"] {
        assert_eq!(read(&format!("[google_signin]\napproval={value}")).approval, Approval::Admin);
    }
    assert_eq!(read("[google_signin]\napproval='direct'").approval, Approval::Direct);
    assert!(!read("[google_signin]\napproval_grants_mfa_enrollment='true'").approval_grants_mfa_enrollment);
    assert!(read("[google_signin]\napproval_grants_mfa_enrollment=true").approval_grants_mfa_enrollment);
}

#[test]
fn templates_require_valid_company_key_and_positive_revision() {
    for fields in ["", "avatar_template_revision=0", "avatar_template=''\navatar_template_revision='bad'"] {
        assert_eq!(read(&format!("[google_signin]\n{fields}")).avatar_template, AvatarTemplate::Blank);
    }
    for fields in [
        "avatar_template='company:sales'", "avatar_template='company:sales'\navatar_template_revision=0",
        "avatar_template='company:../sales'\navatar_template_revision=1",
        "avatar_template='company:Sales'\navatar_template_revision=1",
        "avatar_template='builtin:sales'\navatar_template_revision=1",
        "avatar_template='company:sales'\navatar_template_revision='1'",
        "avatar_template=true",
    ] { assert_eq!(read(&format!("[google_signin]\n{fields}")).avatar_template, AvatarTemplate::Invalid); }
    assert_eq!(read("[google_signin]\navatar_template='company:sales-1'\navatar_template_revision=3").avatar_template,
        AvatarTemplate::Company { key: "sales-1".into(), revision: 3 });
}

#[test]
fn revision_is_canonical_and_covers_every_key() {
    let a = read("[google_signin]\nenabled=true\nallowed_hd=['example.com']\nenabled_ack={by='admin',at='now'}");
    let b = read("# comment\n[google_signin]\nenabled_ack={at='now',by='admin'}\nallowed_hd = [ 'example.com' ]\nenabled = true");
    assert_eq!(a.revision, b.revision);
    assert_eq!(a.revision.len(), 64);
    assert!(a.revision.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    for change in ["enabled=false", "allowed_hd=['other.com']", "enabled_ack={by='other',at='now'}",
        "approval='direct'", "daily_new_accounts=21", "avatar_template=''", "avatar_template_revision=1",
        "approval_grants_mfa_enrollment=true", "future_key='value'"] {
        let mut root: toml::Table = "[google_signin]\nenabled=true\nallowed_hd=['example.com']\nenabled_ack={by='admin',at='now'}".parse().unwrap();
        let extra: toml::Table = change.parse().unwrap();
        root.get_mut("google_signin").unwrap().as_table_mut().unwrap().extend(extra);
        assert_ne!(a.revision, read(&toml::to_string(&root).unwrap()).revision, "{change}");
    }
    assert_eq!(read("[google_signin]\nfuture={z=1,a={y=2,b=3}}").revision,
        read("[google_signin]\nfuture={a={b=3,y=2},z=1}").revision);
}

#[test]
fn writes_require_ack_record_actor_and_remove_ack_when_disabled() {
    let home = tempfile::tempdir().unwrap();
    let mut update = update();
    assert!(write_settings(home.path(), "admin", &update, false, NOW).is_err());
    assert!(write_settings(home.path(), "", &update, true, NOW).is_err());
    let (before, after) = write_settings(home.path(), "admin", &update, true, NOW).unwrap();
    assert!(!before.enabled);
    assert!(after.enabled);
    assert_eq!(after.ack, Some(("admin".into(), NOW.into())));
    assert_eq!(after.allowed_hd, vec!["example.com"]);
    assert_eq!(after, read_strict(home.path()));
    assert!(write_settings(home.path(), "admin", &update, false, NOW).is_ok());
    update.approval = Approval::Direct;
    assert!(write_settings(home.path(), "admin", &update, false, NOW).is_err());
    let (_, direct) = write_settings(home.path(), "second-admin", &update, true, NOW).unwrap();
    assert_eq!(direct.ack.unwrap().0, "second-admin");
    assert!(write_settings(home.path(), "admin", &update, false, NOW).is_ok());
    update.approval_grants_mfa_enrollment = true;
    assert!(write_settings(home.path(), "admin", &update, false, NOW).is_err());
    write_settings(home.path(), "admin", &update, true, NOW).unwrap();
    update.enabled = false;
    let (_, after) = write_settings(home.path(), "admin", &update, false, NOW).unwrap();
    assert!(!after.enabled);
    assert!(after.ack.is_none());
    let saved = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!saved.parse::<toml::Table>().unwrap()["google_signin"].as_table().unwrap().contains_key("enabled_ack"));
}

#[test]
fn disable_settings_keeps_other_fields() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    let text = "# preserve formatting and unknown keys\n\
[google_signin]\n\
enabled = true # emergency switch\n\
enabled_ack={by='admin',at='2026-11-27T00:00:00Z'}\n\
allowed_hd = [ 'EXAMPLE.COM', 'other.com' ] # keep original case\n\
approval = 'direct'\n\
daily_new_accounts = 1_00\n\
avatar_template = 'company:sales'\n\
avatar_template_revision = 7\n\
approval_grants_mfa_enrollment = true\n\
future_key = { label = '保留', count = 3 }\n\
[general]\n\
name = 'unchanged'\n";
    std::fs::write(&path, text).unwrap();
    let original = read_strict(home.path());
    assert!(original.enabled);
    let (before, after) = disable_settings(home.path(), "other-admin", NOW).unwrap();
    assert_eq!(before, original);
    assert!(!after.enabled);
    assert!(!after.enabled_requested);
    assert!(after.ack.is_none());
    assert_eq!(after, read_strict(home.path()));
    let expected = text.replace("enabled = true", "enabled = false")
        .replace("enabled_ack={by='admin',at='2026-11-27T00:00:00Z'}\n", "");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    let mut expected_root: toml::Table = text.parse().unwrap();
    let section = expected_root.get_mut("google_signin").unwrap().as_table_mut().unwrap();
    section.insert("enabled".into(), false.into());
    section.remove("enabled_ack");
    assert_eq!(std::fs::read_to_string(&path).unwrap().parse::<toml::Table>().unwrap(), expected_root);
    let (again_before, again_after) = disable_settings(home.path(), "other-admin", NOW).unwrap();
    assert_eq!(again_before, after);
    assert_eq!(again_after, after);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
}

#[test]
fn disable_settings_no_section_is_harmless() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    let text = "# no Google sign-in configured\n[general]\nname = 'unchanged'\n";
    std::fs::write(&path, text).unwrap();
    let original = read_strict(home.path());
    let (before, after) = disable_settings(home.path(), "admin", NOW).unwrap();
    assert_eq!(before, original);
    assert_eq!(after, original);
    assert!(!after.enabled);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert!(!text.parse::<toml::Table>().unwrap().contains_key("google_signin"));
}

#[test]
fn invalid_updates_never_write() {
    let home = tempfile::tempdir().unwrap();
    for list in [vec![], vec!["bad/domain".into()], vec!["example.com".into(), "bad/domain".into()], vec!["example.com".into(); 51]] {
        let mut update = update();
        update.allowed_hd = list;
        assert!(write_settings(home.path(), "admin", &update, true, NOW).is_err());
    }
    for daily in [0, 201] {
        let mut update = update();
        update.daily_new_accounts = daily;
        assert!(write_settings(home.path(), "admin", &update, true, NOW).is_err());
    }
    for template in [AvatarTemplate::Invalid, AvatarTemplate::Company { key: "bad/key".into(), revision: 1 },
        AvatarTemplate::Company { key: "sales".into(), revision: 0 },
        AvatarTemplate::Company { key: "sales".into(), revision: u64::MAX }] {
        let mut update = update();
        update.avatar_template = template;
        assert!(write_settings(home.path(), "admin", &update, true, NOW).is_err());
    }
    assert!(!home.path().join("config.toml").exists());
}

#[test]
fn raw_editor_only_accepts_explicit_narrowing() {
    let before: toml::Table = format!("{ACTIVE}approval='admin'\ndaily_new_accounts=20\navatar_template=''\n").parse().unwrap();
    assert!(refuse_google_signin_widening(&before, &toml::Table::new()).is_ok());
    assert!(refuse_google_signin_widening(&before, &before).is_ok());
    for change in ["enabled=false", "remove-ack", "both"] {
        let mut after = before.clone();
        let t = after.get_mut("google_signin").unwrap().as_table_mut().unwrap();
        if change != "remove-ack" { t.insert("enabled".into(), false.into()); }
        if change != "enabled=false" { t.remove("enabled_ack"); }
        assert!(refuse_google_signin_widening(&before, &after).is_ok());
    }
    for change in ["enabled=true", "enabled_ack={by='other',at='now'}", "allowed_hd=['example.com','other.com']",
        "approval='direct'", "daily_new_accounts=21", "avatar_template='company:sales'", "future=true"] {
        let mut old = before.clone();
        old.get_mut("google_signin").unwrap().as_table_mut().unwrap().insert("enabled".into(), false.into());
        let mut after = old.clone();
        let extra: toml::Table = change.parse().unwrap();
        after.get_mut("google_signin").unwrap().as_table_mut().unwrap().extend(extra);
        assert!(refuse_google_signin_widening(&old, &after).is_err(), "{change}");
    }
    let mut no_ack = before.clone();
    no_ack.get_mut("google_signin").unwrap().as_table_mut().unwrap().remove("enabled_ack");
    assert!(refuse_google_signin_widening(&no_ack, &before).is_err());
    assert!(refuse_google_signin_widening(&toml::Table::new(), &before).is_err());
    let mut removed_key = before.clone();
    removed_key.get_mut("google_signin").unwrap().as_table_mut().unwrap().remove("daily_new_accounts");
    assert!(refuse_google_signin_widening(&before, &removed_key).is_err());
}

#[test]
fn writes_preserve_unrelated_values_comments_and_company_revision() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    std::fs::write(&path, "# keep this comment\n[general]\nname='test'\n").unwrap();
    let mut update = update();
    update.avatar_template = AvatarTemplate::Company { key: "sales".into(), revision: 7 };
    let (_, after) = write_settings(home.path(), "admin", &update, true, NOW).unwrap();
    assert_eq!(after.avatar_template, update.avatar_template);
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("# keep this comment"));
    assert_eq!(text.parse::<toml::Table>().unwrap()["general"]["name"].as_str(), Some("test"));
}


#[test]
fn client_hash_refuses_stale_write_and_disable_without_changing_text() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    std::fs::write(&path, ACTIVE).unwrap();
    let stale = read_with_hash(home.path()).unwrap().1;
    let changed = format!("{ACTIVE}# another admin saved\n");
    std::fs::write(&path, &changed).unwrap();
    assert_eq!(super::config::write_settings(home.path(), "admin", &update(), true, NOW, &stale).unwrap_err(),
        "設定已被其他人變更，請重新整理後再儲存。");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
    assert_eq!(super::config::disable_settings(home.path(), "admin", NOW, &stale).unwrap_err(),
        "設定已被其他人變更，請重新整理後再儲存。");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
    let fresh = read_with_hash(home.path()).unwrap().1;
    let (_, after, hash) = super::config::disable_settings(home.path(), "admin", NOW, &fresh).unwrap();
    assert!(!after.enabled);
    assert_eq!(hash, read_with_hash(home.path()).unwrap().1);
}
