use super::*;
use std::time::Duration;

fn row(tool: &str, state: &str, id: &str, now: i64) -> Value {
    serde_json::json!({"timestamp":chrono::DateTime::from_timestamp(now, 0).unwrap().to_rfc3339(),
        "tool_name":tool,"state":state,"success":true,"draft_id":id,"approval_id":id,
        "sent":false,"pending_tool":"sheets_append","session_id":"googlechat:space","turn_id":"turn-1","agent_id":"agent"})
}
fn row_in(session: &str, turn: &str, tool: &str, state: &str, id: &str, now: i64) -> Value {
    let mut r = row(tool, state, id, now);
    r["session_id"] = session.into();
    r["turn_id"] = turn.into();
    r
}
fn write_rows(home: &Path, rows: &[Value]) {
    let path = home.join("tool_calls.jsonl");
    duduclaw_core::with_file_lock(&path, || std::fs::write(&path,
        rows.iter().map(|v| format!("{v}\n")).collect::<String>())).unwrap();
}
fn item(id: &str, created_at: i64) -> Item {
    Item { kind: Kind::MailDraft, tool: "gmail_create_draft".into(), source_tool: "gmail_create_draft".into(), id: id.into(),
        related_id: None, summary: "draft".into(), created_at, expires_at: created_at + PENDING_TTL_SECONDS }
}
fn pending_dir(home: &Path) -> std::path::PathBuf { home.join(store::DIR_NAME) }
fn entries(home: &Path) -> Vec<String> {
    match std::fs::read_dir(pending_dir(home)) {
        Ok(rd) => { let mut v: Vec<_> = rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect(); v.sort(); v }
        Err(_) => Vec::new(),
    }
}
fn lock_path(home: &Path, session: &str, sender: &str) -> std::path::PathBuf {
    pending_dir(home).join(store::lock_name(&name_of(session, sender)))
}
/// The owner-prefixed ledger file of `(session, sender)` outside any WebChat
/// scope (where the key ignores the agent; fixtures use agent `"agent"`).
fn name_of(session: &str, sender: &str) -> String { ledger_name(session, sender, "agent").unwrap() }
/// [`super::reminder`] for the fixtures' agent `"agent"`.
fn reminder(home: &Path, session: &str, sender: &str, in_chat: bool) -> Option<String> {
    super::reminder(home, session, sender, "agent", in_chat)
}
/// Production per-principal quota with a test-sized global cap.
fn cap(global: usize) -> store::Limits { store::Limits { global, per_owner: store::MAX_LEDGERS_PER_OWNER } }
fn ledger_names(home: &Path) -> Vec<String> { entries(home).into_iter().filter(|n| store::is_ledger_name(n)).collect() }

// ── Pure ledger logic ──────────────────────────────────────────────────────

#[test]
fn only_successful_pending_write_outcomes_are_retained() {
    let now = 1_800_000_000;
    let mut items = vec![];
    for (tool, state) in [("gmail_create_draft", "draft"), ("mail_send", "pending_confirmation"), ("approval_request", "pending")] {
        apply(&mut items, &row(tool, state, tool, now), now);
    }
    assert_eq!(items.len(), 3);
    assert_eq!(items[2].tool, "sheets_append");
    let mut failed = row("gmail_create_draft", "draft", "failed", now);
    failed["success"] = false.into(); apply(&mut items, &failed, now);
    apply(&mut items, &row("gmail_read", "draft", "read", now), now);
    apply(&mut items, &row("gmail_create_draft", "completed", "unknown", now), now);
    let mut sent = row("mail_send", "pending_confirmation", "sent", now); sent["sent"] = true.into();
    apply(&mut items, &sent, now);
    assert_eq!(items.len(), 3);
}
#[test]
fn expiry_cap_and_terminal_receipts() {
    let now = 1_800_000_000;
    let mut items = vec![];
    for n in 0..12 { apply(&mut items, &row("gmail_create_draft", "draft", &format!("d-{n}"), now + n), now + n); }
    assert_eq!(items.len(), 10); assert_eq!(items[0].id, "d-2");
    let mut failure = row("gmail_create_draft", "completed", "d-2", now + 12);
    failure["success"] = false.into(); apply(&mut items, &failure, now + 12);
    assert_eq!(items.len(), 10);
    apply(&mut items, &row("gmail_create_draft", "cancelled", "d-2", now + 12), now + 12);
    assert_eq!(items.len(), 9);
    prune(&mut items, now + 11 + PENDING_TTL_SECONDS); assert!(items.is_empty());
}
#[test]
fn mail_alias_and_approval_completion_remove_items() {
    let now = 1_800_000_000; let mut items = vec![];
    let mut mail = row("mail_send", "pending_confirmation", "approval", now); mail["mail_id"] = "mail-1".into();
    apply(&mut items, &mail, now);
    let mut sent = row("mail_send", "sent", "unrelated", now); sent["mail_id"] = "mail-1".into();
    apply(&mut items, &sent, now); assert!(items.is_empty());
    apply(&mut items, &mail, now);
    apply(&mut items, &row("approval_request", "completed", "approval", now), now);
    assert!(items.is_empty());
}
#[test]
fn rejects_odd_ids_and_masks_cjk_summary() {
    let now = 1_800_000_000;
    for id in ["", "id\nINSTRUCTION", "<id>", "id with space", "中文", &"a".repeat(257)] {
        let mut items = vec![]; apply(&mut items, &row("gmail_create_draft", "draft", id, now), now);
        assert!(items.is_empty(), "{id:?}");
    }
    assert!(valid_id("r-ABC_123.@:/"));
    let summary = clean_summary(&"中文測試".repeat(50)); assert_eq!(summary.chars().count(), 120);
    assert!(!clean_summary("password=secret123456789").contains("secret123456789"));
}
#[test]
fn ledger_and_temp_names_are_strict() {
    let name = name_of("s", "u");
    assert!(store::is_ledger_name(&name));
    assert!(!store::is_ledger_name("x.json") && !store::is_ledger_name(&format!("{name}.lock")));
    assert!(!store::is_ledger_name(&name.to_uppercase()));
    assert!(store::is_temp_name(&crate::fs_safe::tmp_name(&name)));
    assert!(!store::is_temp_name(&name) && !store::is_temp_name(".other.tmp-0123"));
    // The fixed-length owner prefix is readable from the name alone, and the
    // lock sidecar and temp names carry it too.
    let owner = store::ledger_owner(&name).unwrap();
    assert_eq!(owner, store::owner_tag("s", "u"));
    assert_eq!(owner.len(), 32);
    assert!(store::lock_name(&name).starts_with(&format!("{owner}-")) && store::is_lock_name(&store::lock_name(&name)));
    assert!(crate::fs_safe::tmp_name(&name).starts_with(&format!(".{owner}-")));
    // Owner = channel + sender: one sender's sessions share it, channels do not.
    let owner_of = |session: &str, sender: &str| store::ledger_owner(&name_of(session, sender)).unwrap().to_string();
    assert_eq!(owner_of("googlechat:a", "u"), owner_of("googlechat:b", "u"));
    assert_ne!(owner_of("googlechat:a", "u"), owner_of("slack:a", "u"));
    assert_ne!(owner_of("googlechat:a", "u"), owner_of("googlechat:a", "v"));
    // The earlier format is recognised (for GC) but owned by nobody.
    let legacy = format!("{}.json", "ab".repeat(32));
    assert!(!store::is_ledger_name(&legacy) && store::is_legacy_ledger_name(&legacy) && store::ledger_owner(&legacy).is_none());
    assert!(store::is_lock_name(&store::lock_name(&legacy)) && store::is_temp_name(&crate::fs_safe::tmp_name(&legacy)));
}

// ── Persisted ledger, sender isolation ─────────────────────────────────────

#[test]
fn persisted_turn_correlation_sender_isolation_and_public_visitors() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let mut wrong_turn = row("gmail_create_draft", "draft", "wrong-turn", now); wrong_turn["turn_id"] = "turn-2".into();
    let mut wrong_session = row("gmail_create_draft", "draft", "wrong-session", now); wrong_session["session_id"] = "other".into();
    let mut wrong_agent = row("gmail_create_draft", "draft", "wrong-agent", now); wrong_agent["agent_id"] = "other".into();
    write_rows(home.path(), &[row("gmail_create_draft", "draft", "draft-1", now), wrong_turn, wrong_session, wrong_agent]);
    finish_turn(home.path(), "googlechat:space", "alice", "agent", "turn-1");
    let block = reminder(home.path(), "googlechat:space", "alice", false).unwrap();
    assert!(block.contains("draft-1")); assert!(!block.contains("wrong-"));
    assert!(block.contains("待使用者確認的項目")); assert!(block.contains("管理員在儀表板"));
    assert!(block.contains("回覆「確認」不會核准")); assert!(block.contains("歷史框架的例外"));
    assert!(reminder(home.path(), "googlechat:space", "bob", false).is_none());
    // The lock sidecar is unlinked when the update finishes.
    assert!(!lock_path(home.path(), "googlechat:space", "alice").exists());
    finish_turn(home.path(), "odoolivechat:visitor", "alice", "agent", "turn-1");
    assert!(!path(home.path(), "odoolivechat:visitor", "alice").exists());
    assert!(reminder(home.path(), "odoolivechat:visitor", "alice", true).is_none());
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path(home.path(), "googlechat:space", "alice")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(pending_dir(home.path())).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
#[test]
fn empty_or_unreadable_ledger_has_no_reminder() {
    let home = tempfile::tempdir().unwrap(); store::open_dir(home.path(), true).unwrap().unwrap();
    assert!(render(&[], 0, true).is_none());
    let p = path(home.path(), "s", "alice"); std::fs::write(p, b"broken").unwrap();
    assert!(reminder(home.path(), "s", "alice", true).is_none());
}
#[test]
fn employee_file_guard_covers_ledger_and_lock() {
    use duduclaw_core::org_field_guard::{check_caller_scope, HookCaller};
    let home = tempfile::tempdir().unwrap();
    for p in [path(home.path(), "s", "u"), home.path().join("session_pending/test.json.lock")] {
        assert!(!check_caller_scope(&p, home.path(), &HookCaller::Agent("employee".into())).is_allowed());
    }
}
#[test]
fn channel_capability_and_wording() {
    for channel in ["telegram", "discord", "slack", "line"] {
        assert!(crate::approval::supports_text_decisions(channel));
    }
    for channel in ["googlechat", "webchat", "odoolivechat", "unknown"] {
        assert!(!crate::approval::supports_text_decisions(channel));
    }
    assert!(crate::approval::pending_confirmation_hint(true).contains("完整編號"));
    assert!(crate::approval::pending_confirmation_hint(false).contains("管理員在儀表板"));
}
#[tokio::test]
async fn public_widget_is_disabled_using_authenticated_principal() {
    let home = tempfile::tempdir().unwrap();
    crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope("widget-visitor:123".into(), async {
        assert!(public_visitor("webchat:webchat:opaque:socket"));
        finish_turn(home.path(), "webchat:webchat:opaque:socket", "sender", "agent", "turn");
        assert!(reminder(home.path(), "webchat:webchat:opaque:socket", "sender", true).is_none());
        assert!(!home.path().join("session_pending").exists());
    }).await;
    crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope("dashboard-account".into(), async {
        assert!(!public_visitor("webchat:webchat:opaque:socket"));
    }).await;
    assert!(public_visitor("webchat:webchat:opaque:socket"));
}

// ── H1: placeholder senders never get a ledger ─────────────────────────────

#[test]
fn placeholder_and_blank_senders_disable_the_ledger_for_write_and_read() {
    let session = "googlechat:space";
    for sender in ["anonymous", "unknown", "system", "Unknown", "  ANONYMOUS ", "", " ", "\t\n"] {
        let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
        write_rows(home.path(), &[row("gmail_create_draft", "draft", "draft-1", now)]);
        finish_turn(home.path(), session, sender, "agent", "turn-1");
        assert!(!pending_dir(home.path()).exists(), "write created state for {sender:?}");
        // Even a ledger already planted for that bucket is never injected.
        let dir = store::open_dir(home.path(), true).unwrap().unwrap();
        store::write_items(&dir, &name_of(session, sender), &[item("planted", now)]).unwrap();
        assert!(reminder(home.path(), session, sender, false).is_none(), "read injected for {sender:?}");
    }
}
#[test]
fn group_message_with_missing_sender_never_sees_another_callers_items() {
    // Slack `/ask` files a missing user_id under `unknown`; entry points
    // without user tracking use `anonymous`. Neither may share a bucket.
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let session = "slack:C0123:group";
    write_rows(home.path(), &[row_in(session, "turn-1", "approval_request", "pending", "req-alice", now)]);
    finish_turn(home.path(), session, "U-alice", "agent", "turn-1");
    assert!(reminder(home.path(), session, "U-alice", true).unwrap().contains("req-alice"));
    write_rows(home.path(), &[row_in(session, "turn-2", "approval_request", "pending", "req-anon", now)]);
    for missing in ["unknown", "anonymous", ""] {
        finish_turn(home.path(), session, missing, "agent", "turn-2");
        assert!(reminder(home.path(), session, missing, true).is_none(), "{missing:?}");
    }
    assert_eq!(entries(home.path()), vec![name_of(session, "U-alice")]);
}

// ── M1: nothing created without an item; empty ledgers removed ─────────────

#[test]
fn turn_without_pending_receipt_creates_nothing() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let mut bad_id = row("gmail_create_draft", "draft", "id with space", now); bad_id["turn_id"] = "turn-1".into();
    write_rows(home.path(), &[row("gmail_read", "draft", "read", now), bad_id,
        row("gmail_create_draft", "cancelled", "never-existed", now)]);
    finish_turn(home.path(), "googlechat:space", "alice", "agent", "turn-1");
    finish_turn(home.path(), "googlechat:space", "alice", "agent", "no-such-turn");
    assert!(!pending_dir(home.path()).exists());
}
#[test]
fn ledger_and_lock_are_removed_when_the_last_item_completes() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-1", now)]);
    finish_turn(home.path(), "googlechat:space", "alice", "agent", "turn-1");
    assert_eq!(entries(home.path()).len(), 1);
    write_rows(home.path(), &[row_in("googlechat:space", "turn-2", "gmail_create_draft", "cancelled", "d-1", now)]);
    finish_turn(home.path(), "googlechat:space", "alice", "agent", "turn-2");
    assert!(entries(home.path()).is_empty(), "{:?}", entries(home.path()));
    assert!(reminder(home.path(), "googlechat:space", "alice", false).is_none());
}
// ── M1 round 2: cap admission ──────────────────────────────────────────────

fn backdate(home: &Path, session: &str, sender: &str, by: Duration) {
    std::fs::OpenOptions::new().write(true).open(path(home, session, sender)).unwrap()
        .set_modified(std::time::SystemTime::now() - by).unwrap();
}
fn sorted(mut names: Vec<String>) -> Vec<String> { names.sort(); names }

#[test]
fn at_the_cap_existing_ledgers_update_in_place_and_only_the_owner_replaces_its_own() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let s = "googlechat:space";
    write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-1", now)]);
    update(home.path(), s, "alice", "agent", "turn-1", cap(1)).unwrap();
    write_rows(home.path(), &[row_in(s, "turn-2", "gmail_create_draft", "draft", "d-2", now)]);
    update(home.path(), s, "alice", "agent", "turn-2", cap(1)).unwrap();
    let block = reminder(home.path(), s, "alice", false).unwrap();
    assert!(block.contains("d-1") && block.contains("d-2"));
    assert_eq!(entries(home.path()), vec![name_of(s, "alice")]);
    // A stranger at the full directory never evicts alice's live ledger.
    write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-3", now)]);
    update(home.path(), s, "bob", "agent", "turn-1", cap(1)).unwrap();
    assert!(reminder(home.path(), s, "bob", false).is_none());
    assert!(reminder(home.path(), s, "alice", false).unwrap().contains("d-1"));
    // Alice's own new conversation replaces her own oldest ledger.
    let other = "googlechat:other-space";
    write_rows(home.path(), &[row_in(other, "turn-1", "gmail_create_draft", "draft", "d-4", now)]);
    update(home.path(), other, "alice", "agent", "turn-1", cap(1)).unwrap();
    assert!(reminder(home.path(), other, "alice", false).unwrap().contains("d-4"));
    assert!(reminder(home.path(), s, "alice", false).is_none());
    // No admission lock or ledger sidecar is left behind.
    assert_eq!(entries(home.path()), vec![name_of(other, "alice")]);
}

// ── M-A: per-principal quota; never another principal's live ledger ─────────

#[test]
fn one_principal_with_many_sessions_never_removes_another_principals_live_ledger() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let s = "googlechat:space";
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    // Alice and bob are live and the least recently written ledgers.
    for victim in ["alice", "bob"] {
        store::write_items(&dir, &name_of(s, victim), &[item(&format!("live-{victim}"), now)]).unwrap();
        backdate(home.path(), s, victim, Duration::from_secs(60 * 60));
    }
    let rooms: Vec<String> = (0..10).map(|n| format!("googlechat:room-{n}")).collect();
    write_rows(home.path(), &rooms.iter().enumerate()
        .map(|(n, room)| row_in(room, "turn-1", "gmail_create_draft", "draft", &format!("m-{n}"), now)).collect::<Vec<_>>());
    let limits = store::Limits { global: 4, per_owner: store::MAX_LEDGERS_PER_OWNER };
    for room in &rooms {
        update(home.path(), room, "mallory", "agent", "turn-1", limits).unwrap();
    }
    for victim in ["alice", "bob"] {
        assert!(reminder(home.path(), s, victim, false).unwrap().contains(&format!("live-{victim}")), "{victim}");
    }
    // Mallory churned through her own two slots; her newest session is kept.
    assert!(reminder(home.path(), &rooms[9], "mallory", false).unwrap().contains("m-9"));
    let mallory = store::owner_tag("googlechat", "mallory");
    let names = ledger_names(home.path());
    assert_eq!(names.iter().filter(|n| store::ledger_owner(n) == Some(mallory.as_str())).count(), 2, "{names:?}");
    assert_eq!(names.len(), 4, "{names:?}");
}

#[test]
fn at_its_quota_a_principal_removes_its_own_expired_then_its_own_oldest_ledger() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let rooms: Vec<String> = (1..=4).map(|n| format!("googlechat:room-{n}")).collect();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    // alice: room-1 live but least recently written, room-2 expired but recent.
    store::write_items(&dir, &name_of(&rooms[0], "alice"), &[item("a-1", now)]).unwrap();
    backdate(home.path(), &rooms[0], "alice", Duration::from_secs(2 * 60 * 60));
    store::write_items(&dir, &name_of(&rooms[1], "alice"), &[item("a-2", now - 2 * PENDING_TTL_SECONDS)]).unwrap();
    // bob: older than everything, live — never touched by alice's quota.
    store::write_items(&dir, &name_of(&rooms[0], "bob"), &[item("b-1", now)]).unwrap();
    backdate(home.path(), &rooms[0], "bob", Duration::from_secs(3 * 60 * 60));
    write_rows(home.path(), &[row_in(&rooms[2], "turn-1", "gmail_create_draft", "draft", "a-3", now),
        row_in(&rooms[3], "turn-1", "gmail_create_draft", "draft", "a-4", now)]);
    let limits = store::Limits { global: 100, per_owner: 2 };

    // At the quota her own expired ledger goes first.
    update(home.path(), &rooms[2], "alice", "agent", "turn-1", limits).unwrap();
    assert_eq!(ledger_names(home.path()), sorted(vec![name_of(&rooms[0], "alice"), name_of(&rooms[2], "alice"), name_of(&rooms[0], "bob")]));
    // Nothing of hers has expired any more: her own oldest ledger is evicted.
    update(home.path(), &rooms[3], "alice", "agent", "turn-1", limits).unwrap();
    assert_eq!(ledger_names(home.path()), sorted(vec![name_of(&rooms[2], "alice"), name_of(&rooms[3], "alice"), name_of(&rooms[0], "bob")]));
    assert!(reminder(home.path(), &rooms[0], "bob", false).unwrap().contains("b-1"));
    assert!(reminder(home.path(), &rooms[3], "alice", false).unwrap().contains("a-4"));
}

#[test]
fn at_the_global_cap_expired_ledgers_anywhere_are_reclaimed_before_any_refusal() {
    // More live ledgers than the old 64 expiry probes, all older than the
    // expired ones: the sweep must still reach the expired ledgers.
    const LIVE: usize = 70;
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let s = "googlechat:space";
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    let live: Vec<String> = (0..LIVE).map(|n| format!("live-{n}")).collect();
    for sender in &live {
        store::write_items(&dir, &name_of(s, sender), &[item("live", now)]).unwrap();
        backdate(home.path(), s, sender, Duration::from_secs(60 * 60));
    }
    // Two other principals whose items have all expired, written recently.
    store::write_items(&dir, &name_of(s, "bob"), &[item("old", now - 2 * PENDING_TTL_SECONDS)]).unwrap();
    store::write_items(&dir, &name_of("googlechat:room", "carol"), &[item("old", now - 2 * PENDING_TTL_SECONDS)]).unwrap();
    write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-new", now)]);
    let limits = cap(LIVE + 2);

    // Newcomers without ledgers of their own are admitted by reclaiming them.
    for newcomer in ["dave", "erin"] {
        update(home.path(), s, newcomer, "agent", "turn-1", limits).unwrap();
        assert!(reminder(home.path(), s, newcomer, false).unwrap().contains("d-new"), "{newcomer}");
    }
    let names = ledger_names(home.path());
    assert!(!names.contains(&name_of(s, "bob")) && !names.contains(&name_of("googlechat:room", "carol")), "{names:?}");
    assert!(live.iter().all(|sender| names.contains(&name_of(s, sender))), "a live ledger was removed");
    assert_eq!(names.len(), LIVE + 2);
}

#[test]
fn a_newcomer_without_ledgers_at_a_full_directory_of_live_ledgers_is_refused_and_nothing_is_deleted() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let s = "googlechat:space";
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    for (n, sender) in ["alice", "bob", "carol"].into_iter().enumerate() {
        store::write_items(&dir, &name_of(s, sender), &[item(&format!("live-{sender}"), now)]).unwrap();
        backdate(home.path(), s, sender, Duration::from_secs(60 * 60 * (n as u64 + 1)));
    }
    let before: Vec<(String, Vec<u8>)> = entries(home.path()).into_iter()
        .map(|name| { let bytes = std::fs::read(pending_dir(home.path()).join(&name)).unwrap(); (name, bytes) }).collect();
    write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-dave", now)]);
    update(home.path(), s, "dave", "agent", "turn-1", cap(3)).unwrap();
    assert!(reminder(home.path(), s, "dave", false).is_none());
    let after: Vec<(String, Vec<u8>)> = entries(home.path()).into_iter()
        .map(|name| { let bytes = std::fs::read(pending_dir(home.path()).join(&name)).unwrap(); (name, bytes) }).collect();
    assert_eq!(after, before);
    assert_eq!(after.len(), 3);
}

#[tokio::test]
async fn webchat_quota_counts_the_authenticated_account_not_sockets_or_conversations() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    // Each conversation (`conv` nonce) on its own socket: session and sender differ every time.
    let sessions: Vec<String> = (0..5).map(|n| format!("webchat:webchat:tag:sock-{n}#conv-{n}")).collect();
    write_rows(home.path(), &sessions.iter().enumerate()
        .map(|(n, s)| row_in(s, "turn-1", "gmail_create_draft", "draft", &format!("d-{n}"), now)).collect::<Vec<_>>());
    let limits = store::Limits { global: 100, per_owner: 2 };
    let principal = &crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL;
    principal.scope("acct-other".into(), async {
        update(home.path(), &sessions[0], "webchat:other:sock", "agent", "turn-1", limits).unwrap();
    }).await;
    principal.scope("acct-1".into(), async {
        for (n, s) in sessions.iter().enumerate() {
            update(home.path(), s, &format!("webchat:tag:sock-{n}"), "agent", "turn-1", limits).unwrap();
        }
        assert!(reminder(home.path(), &sessions[4], "webchat:tag:sock-4", true).unwrap().contains("d-4"));
    }).await;
    let account = store::owner_tag("webchat-account", "acct-1");
    let names = ledger_names(home.path());
    assert_eq!(names.iter().filter(|n| store::ledger_owner(n) == Some(account.as_str())).count(), 2, "{names:?}");
    assert_eq!(names.len(), 3, "{names:?}");
    principal.scope("acct-other".into(), async {
        assert!(reminder(home.path(), &sessions[0], "webchat:other:sock", true).unwrap().contains("d-0"));
    }).await;
}

// ── Reconnect: a WebChat account's ledger is keyed on stable values ─────────

/// A stored conversation as the browser resumes it: the session id is sent
/// back verbatim (its original socket part, `#agent:` and `#conv:` included).
const STORED: &str = "webchat:webchat:0123456789ab:1a2b3c4d#agent:sales#conv:c-42";
/// The WebChat sender id of two connections of one account
/// (`webchat:<owner_tag>:<per-connection nonce>`).
const FIRST_SOCKET: &str = "webchat:0123456789ab:1a2b3c4d";
const SECOND_SOCKET: &str = "webchat:0123456789ab:9f8e7d6c";

async fn as_account<T>(account: &str, f: impl std::future::Future<Output = T>) -> T {
    crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(account.into(), f).await
}

#[tokio::test]
async fn webchat_same_account_and_conversation_on_a_new_connection_keeps_its_ledger() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    write_rows(home.path(), &[row_in(STORED, "turn-1", "approval_request", "pending", "req-1", now)]);
    as_account("acct-1", async {
        finish_turn(home.path(), STORED, FIRST_SOCKET, "agent", "turn-1");
        assert!(reminder(home.path(), STORED, FIRST_SOCKET, true).unwrap().contains("req-1"));
        // Page refresh: a new socket, so a new nonce in the sender id.
        assert_eq!(ledger_name(STORED, FIRST_SOCKET, "agent"), ledger_name(STORED, SECOND_SOCKET, "agent"));
        let block = reminder(home.path(), STORED, SECOND_SOCKET, true).unwrap();
        assert!(block.contains("req-1") && block.contains("待使用者確認的項目"), "{block}");
    }).await;
    // One ledger, owned by the account (the round-3 owner), fixed-length hex name.
    let names = ledger_names(home.path());
    let account = store::owner_tag("webchat-account", "acct-1");
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(store::ledger_owner(&names[0]), Some(account.as_str()));
    // Completing the item on the new connection updates that same ledger.
    write_rows(home.path(), &[row_in(STORED, "turn-2", "approval_request", "completed", "req-1", now)]);
    as_account("acct-1", async {
        finish_turn(home.path(), STORED, SECOND_SOCKET, "agent", "turn-2");
        assert!(reminder(home.path(), STORED, FIRST_SOCKET, true).is_none());
    }).await;
    assert!(entries(home.path()).is_empty(), "{:?}", entries(home.path()));
}

#[tokio::test]
async fn webchat_another_account_on_the_same_conversation_gets_a_different_ledger() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    write_rows(home.path(), &[row_in(STORED, "turn-1", "approval_request", "pending", "req-1", now)]);
    let first = as_account("acct-1", async {
        finish_turn(home.path(), STORED, FIRST_SOCKET, "agent", "turn-1");
        ledger_name(STORED, FIRST_SOCKET, "agent").unwrap()
    }).await;
    let second = as_account("acct-2", async {
        // Same session and even the same sender string: still not acct-1's ledger.
        assert!(reminder(home.path(), STORED, FIRST_SOCKET, true).is_none());
        assert!(reminder(home.path(), STORED, SECOND_SOCKET, true).is_none());
        ledger_name(STORED, FIRST_SOCKET, "agent").unwrap()
    }).await;
    assert_ne!(first, second);
    assert_ne!(store::ledger_owner(&first), store::ledger_owner(&second));
    // acct-2's own item lands in its own ledger; neither sees the other's.
    write_rows(home.path(), &[row_in(STORED, "turn-2", "approval_request", "pending", "req-2", now)]);
    as_account("acct-2", async {
        finish_turn(home.path(), STORED, FIRST_SOCKET, "agent", "turn-2");
        let block = reminder(home.path(), STORED, FIRST_SOCKET, true).unwrap();
        assert!(block.contains("req-2") && !block.contains("req-1"), "{block}");
    }).await;
    as_account("acct-1", async {
        let block = reminder(home.path(), STORED, SECOND_SOCKET, true).unwrap();
        assert!(block.contains("req-1") && !block.contains("req-2"), "{block}");
    }).await;
    assert_eq!(sorted(ledger_names(home.path())), sorted(vec![first, second]));
}

#[tokio::test]
async fn webchat_a_different_conversation_or_employee_gets_a_different_ledger() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let other_conv = "webchat:webchat:0123456789ab:1a2b3c4d#agent:sales#conv:c-43";
    let other_stored = "webchat:webchat:0123456789ab:5e6f7a8b#agent:sales#conv:c-42";
    write_rows(home.path(), &[row_in(STORED, "turn-1", "approval_request", "pending", "req-1", now)]);
    as_account("acct-1", async {
        finish_turn(home.path(), STORED, FIRST_SOCKET, "agent", "turn-1");
        let name = ledger_name(STORED, FIRST_SOCKET, "agent");
        for session in [other_conv, other_stored] {
            assert_ne!(ledger_name(session, FIRST_SOCKET, "agent"), name, "{session}");
            assert!(reminder(home.path(), session, FIRST_SOCKET, true).is_none(), "{session}");
            assert!(reminder(home.path(), session, SECOND_SOCKET, true).is_none(), "{session}");
        }
        // Another employee on the same stored session id is another ledger too.
        assert_ne!(ledger_name(STORED, FIRST_SOCKET, "other-agent"), name);
        assert!(super::reminder(home.path(), STORED, SECOND_SOCKET, "other-agent", true).is_none());
        assert!(reminder(home.path(), STORED, SECOND_SOCKET, true).unwrap().contains("req-1"));
    }).await;
    assert_eq!(ledger_names(home.path()).len(), 1);
}

#[tokio::test]
async fn webchat_widget_visitor_or_unidentified_scope_still_gets_no_ledger() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    write_rows(home.path(), &[row_in(STORED, "turn-1", "approval_request", "pending", "req-1", now)]);
    for scope in ["widget-visitor:123", "", "  "] {
        as_account(scope, async {
            assert!(ledger_name(STORED, FIRST_SOCKET, "agent").is_none(), "{scope:?}");
            finish_turn(home.path(), STORED, FIRST_SOCKET, "agent", "turn-1");
            assert!(reminder(home.path(), STORED, FIRST_SOCKET, true).is_none(), "{scope:?}");
        }).await;
    }
    assert!(!pending_dir(home.path()).exists());
    // An account's ledger on the same session is never shown to a visitor.
    as_account("acct-1", async { finish_turn(home.path(), STORED, FIRST_SOCKET, "agent", "turn-1") }).await;
    assert_eq!(ledger_names(home.path()).len(), 1);
    as_account("widget-visitor:123", async {
        assert!(reminder(home.path(), STORED, FIRST_SOCKET, true).is_none());
        assert!(reminder(home.path(), STORED, SECOND_SOCKET, true).is_none());
    }).await;
}

#[test]
fn other_channels_keep_the_session_and_sender_key() {
    // Outside a WebChat scope the key is unchanged and ignores the agent.
    let owner = store::owner_tag("googlechat", "alice");
    let expected = store::ledger_file(&owner, "googlechat:space", "alice");
    assert_eq!(ledger_name("googlechat:space", "alice", "agent").as_deref(), Some(expected.as_str()));
    assert_eq!(ledger_name("googlechat:space", "alice", "other-agent").as_deref(), Some(expected.as_str()));
    assert_ne!(ledger_name("googlechat:space", "bob", "agent").as_deref(), Some(expected.as_str()));
}

// ── L-A: the eviction decision is re-checked under the victim's lock ─────────

#[test]
fn eviction_skips_a_ledger_changed_since_its_snapshot_and_takes_it_by_the_fresh_one() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    let name = name_of("googlechat:space", "alice");
    store::write_items(&dir, &name, &[item("d-1", now)]).unwrap();
    let stale = store::Snapshot { modified: dir.modified(&name).unwrap().unwrap(), len: dir.entry(&name).unwrap().unwrap().size };
    // Rewritten after the decision was taken (a new file of another size).
    store::write_items(&dir, &name, &[item("d-1", now), item("d-2", now)]).unwrap();
    let fresh = match store::try_evict(&dir, &store::Candidate { snapshot: stale, name: &name }, None) {
        store::Evicted::Changed(fresh) => fresh,
        other => panic!("a ledger changed since its snapshot was not skipped: {other:?}"),
    };
    assert_ne!(fresh, stale);
    assert_eq!(entries(home.path()), vec![name.clone()], "nothing is removed and the lock is released");
    // Picked again by its fresh snapshot, the same ledger is evicted.
    assert!(matches!(store::try_evict(&dir, &store::Candidate { snapshot: fresh, name: &name }, None), store::Evicted::Freed));
    assert!(entries(home.path()).is_empty());
}

#[test]
fn concurrent_admissions_never_exceed_the_cap() {
    const CAP: usize = 4;
    const WRITERS: usize = 16;
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    store::open_dir(home.path(), true).unwrap().unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
    let threads: Vec<_> = (0..WRITERS).map(|n| {
        let home = home.path().to_path_buf();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let dir = store::open_dir(&home, false).unwrap().unwrap();
            let name = name_of("googlechat:space", &format!("user-{n}"));
            barrier.wait();
            store::create_ledger(&dir, &name, &[item(&format!("d-{n}"), now)], cap(CAP), now,
                store::Deadline::after(store::TURN_LOCK_WAIT))
        })
    }).collect();
    let mut timed_out = 0;
    for thread in threads {
        // A turn that cannot get the admission lock in time skips its ledger.
        if let Err(e) = thread.join().unwrap() {
            assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
            timed_out += 1;
        }
    }
    let names = entries(home.path());
    let ledgers = names.iter().filter(|n| store::is_ledger_name(n)).count();
    assert!((1..=CAP).contains(&ledgers), "{names:?}");
    if timed_out == 0 {
        // The directory filled up; later strangers were refused, never
        // admitted by evicting another principal's live ledger.
        assert_eq!(ledgers, CAP, "{names:?}");
    }
    // Unix unlinks every sidecar while held; Windows may leave an orphan for GC.
    #[cfg(unix)]
    assert_eq!(ledgers, names.len(), "only ledgers remain: {names:?}");
}

#[test]
fn bounded_listing_never_returns_more_than_the_bound() {
    let home = tempfile::tempdir().unwrap();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    for n in 0..10 { dir.create_file_new(&format!("f-{n}"), b"").unwrap(); }
    for limit in [0, 1, 3, 9] {
        let (listed, truncated) = dir.list_bounded(limit).unwrap();
        assert_eq!(listed.len(), limit);
        assert!(truncated, "limit {limit}");
    }
    for limit in [10, 11, usize::MAX] {
        let (listed, truncated) = dir.list_bounded(limit).unwrap();
        assert_eq!(listed.len(), 10);
        assert!(!truncated, "limit {limit}");
    }
    // The unbounded listing callers already use is unchanged.
    assert_eq!(dir.list().unwrap(), dir.list_bounded(usize::MAX).unwrap().0);
    assert_eq!(dir.list().unwrap().len(), 10);
    // The resumable listing returns the next names after the cursor, in order.
    let names = |listed: &[crate::fs_safe::EntryInfo]| listed.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    let (first, next) = dir.list_after(None, 4).unwrap();
    assert_eq!(names(&first), ["f-0", "f-1", "f-2", "f-3"]);
    assert_eq!(next.as_deref(), Some("f-3"));
    let (second, next) = dir.list_after(next.as_deref(), 4).unwrap();
    assert_eq!(names(&second), ["f-4", "f-5", "f-6", "f-7"]);
    let (third, next) = dir.list_after(next.as_deref(), 4).unwrap();
    assert_eq!(names(&third), ["f-8", "f-9"]);
    assert_eq!(next, None, "the end of the directory was reached");
    let (all, next) = dir.list_after(None, 10).unwrap();
    assert_eq!((all, next), (dir.list().unwrap(), None));
}

// ── M-new-1: one hard deadline per lock acquisition ────────────────────────

#[test]
fn lock_deadline_budget_is_the_time_left_and_expires_once() {
    let start = std::time::Instant::now();
    let deadline = store::Deadline::starting_at(start, Duration::from_secs(2));
    assert_eq!(deadline.budget(start, true).unwrap(), Duration::from_secs(2));
    assert_eq!(deadline.budget(start + Duration::from_millis(1500), false).unwrap(), Duration::from_millis(500));
    for late in [Duration::from_secs(2), Duration::from_secs(5)] {
        assert_eq!(deadline.budget(start + late, false).unwrap_err().kind(), io::ErrorKind::TimedOut);
        // A first round after expiry is still one non-blocking try, never a wait.
        assert_eq!(deadline.budget(start + late, true).unwrap(), Duration::ZERO);
    }
    let zero = store::Deadline::starting_at(start, Duration::ZERO);
    assert_eq!(zero.budget(start, true).unwrap(), Duration::ZERO);
    assert_eq!(zero.budget(start, false).unwrap_err().kind(), io::ErrorKind::TimedOut);
}

#[test]
fn locked_reader_reads_under_the_ledger_lock_and_skips_when_busy() {
    // The Windows reminder path, exercised on every platform.
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    let name = name_of("googlechat:space", "alice");
    assert!(store::read_shared(&dir, &name, Some(Duration::ZERO)).unwrap().is_none());
    assert!(entries(home.path()).is_empty(), "an absent ledger is not locked");
    store::write_items(&dir, &name, &[item("d-1", now)]).unwrap();
    let items = store::read_shared(&dir, &name, Some(Duration::ZERO)).unwrap().unwrap();
    assert_eq!(items[0].id, "d-1");
    assert_eq!(entries(home.path()), vec![name.clone()], "the reader's sidecar is removed");
    let held = store::lock(&dir, &store::lock_name(&name), true, Duration::ZERO).unwrap().unwrap();
    let busy = store::read_shared(&dir, &name, Some(Duration::ZERO)).unwrap_err();
    assert_eq!(busy.kind(), io::ErrorKind::TimedOut);
    held.release(&dir);
}

#[cfg(unix)]
#[test]
fn lock_deadline_holds_under_repeated_inode_changes() {
    use std::cell::{Cell, RefCell};
    let home = tempfile::tempdir().unwrap();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    let name = store::lock_name(&name_of("googlechat:space", "alice"));
    let start = std::time::Instant::now();
    let deadline = store::Deadline::starting_at(start, Duration::from_secs(2));
    // A fake clock: each round observes 900 ms more than the previous one.
    let calls = Cell::new(0u32);
    let clock = || {
        let k = calls.get();
        calls.set(k + 1);
        start + Duration::from_millis(900) * k
    };
    // After every successful lock the sidecar is replaced by a new inode, so
    // the re-check fails and the loop must retry within what is left.
    let budgets = RefCell::new(Vec::new());
    let swap = |budget: Duration| {
        budgets.borrow_mut().push(budget);
        dir.remove_file(&name).unwrap();
        dir.create_file_new(&name, b"").unwrap();
    };
    let error = store::lock_rounds(&dir, &name, true, deadline, &clock, &swap).err().expect("must time out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    // Each round only got the remaining time; none restarted the full 2 s.
    assert_eq!(*budgets.borrow(), vec![Duration::from_millis(2000), Duration::from_millis(1100), Duration::from_millis(200)]);
}

/// Install a [`store::AFTER_SIDECAR_OPEN`] hook that unlinks the opened
/// sidecar once — what a releasing holder does between a waiter's open and its
/// check. Returns how often it unlinked.
#[cfg(unix)]
fn unlink_sidecar_once_after_open(home: &Path) -> std::rc::Rc<std::cell::Cell<u32>> {
    let pending = pending_dir(home);
    let unlinks = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let seen = unlinks.clone();
    let hook: Box<dyn Fn(&str)> = Box::new(move |lock: &str| {
        if seen.get() == 0 {
            seen.set(1);
            std::fs::remove_file(pending.join(lock)).unwrap();
        }
    });
    store::AFTER_SIDECAR_OPEN.with(|slot| *slot.borrow_mut() = Some(hook));
    unlinks
}

#[cfg(unix)]
#[test]
fn lock_retries_a_sidecar_its_holder_unlinked_between_open_and_check() {
    let home = tempfile::tempdir().unwrap();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    // The sidecar is at the name (another turn holds it), so the waiter opens
    // the existing one; the holder releases — unlinking it — before the check.
    dir.create_file_new(store::ADMISSION_LOCK, b"").unwrap();
    let unlinks = unlink_sidecar_once_after_open(home.path());
    let held = store::lock(&dir, store::ADMISSION_LOCK, true, store::TURN_LOCK_WAIT);
    store::AFTER_SIDECAR_OPEN.with(|hook| hook.borrow_mut().take());
    assert_eq!(unlinks.get(), 1, "the window was hit");
    let held = match held {
        Ok(Some(held)) => held,
        Ok(None) => panic!("a creating lock returned no lock"),
        Err(e) => panic!("an unlinked sidecar must be retried, not refused: {e}"),
    };
    held.release(&dir);
    assert!(entries(home.path()).is_empty(), "the retried sidecar is unlinked on release");

    // A non-creating try (GC's orphan check) treats it as gone.
    dir.create_file_new(store::ADMISSION_LOCK, b"").unwrap();
    let unlinks = unlink_sidecar_once_after_open(home.path());
    let gone = store::lock(&dir, store::ADMISSION_LOCK, false, Duration::ZERO);
    store::AFTER_SIDECAR_OPEN.with(|hook| hook.borrow_mut().take());
    assert_eq!(unlinks.get(), 1, "the window was hit");
    assert!(matches!(gone, Ok(None)), "an unlinked sidecar is absent, not refused");
    assert!(entries(home.path()).is_empty());
}

// ── M1: GC ─────────────────────────────────────────────────────────────────

#[test]
fn gc_removes_expired_ledgers_orphan_locks_and_stale_temps_but_not_live_ones() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    let live = name_of("s", "alice");
    let expired = name_of("s", "bob");
    let partly = name_of("s", "erin");
    store::write_items(&dir, &live, &[item("live", now)]).unwrap();
    store::write_items(&dir, &expired, &[item("old", now - 2 * PENDING_TTL_SECONDS)]).unwrap();
    store::write_items(&dir, &partly, &[item("old", now - 2 * PENDING_TTL_SECONDS), item("new", now)]).unwrap();
    let orphan = store::lock_name(&name_of("s", "carol"));
    dir.create_file_new(&orphan, b"").unwrap();
    let held_name = store::lock_name(&name_of("s", "dave"));
    let held = store::lock(&dir, &held_name, true, Duration::ZERO).unwrap().unwrap();
    let stale_tmp = crate::fs_safe::tmp_name(&live);
    let fresh_tmp = crate::fs_safe::tmp_name(&expired);
    dir.create_file_new(&stale_tmp, b"[]").unwrap();
    dir.create_file_new(&fresh_tmp, b"[]").unwrap();
    std::fs::OpenOptions::new().write(true).open(pending_dir(home.path()).join(&stale_tmp)).unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60)).unwrap();

    // Not the gateway-lock holder: the maintenance entry does nothing.
    assert_eq!(maintenance(home.path()).unwrap(), GcReport::default());
    assert!(pending_dir(home.path()).join(&expired).exists());

    let report = gc::collect(home.path(), now).unwrap();
    let names = entries(home.path());
    assert!(names.contains(&live) && names.contains(&partly), "{names:?}");
    assert!(!names.contains(&expired) && !names.contains(&orphan) && !names.contains(&stale_tmp), "{names:?}");
    assert!(names.contains(&held_name) && names.contains(&fresh_tmp), "{names:?}");
    assert_eq!((report.removed_ledgers, report.rewritten_ledgers, report.removed_locks, report.removed_temps, report.busy),
        (1, 1, 1, 1, 1));
    // No lock files are left for the ledgers GC itself visited.
    assert!(!names.contains(&store::lock_name(&live)) && !names.contains(&store::lock_name(&partly)));
    let kept = store::read_items(&dir, &partly).unwrap().unwrap();
    assert_eq!(kept.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["new"]);

    held.release(&dir);
    gc::collect(home.path(), now + PENDING_TTL_SECONDS + 1).unwrap();
    assert_eq!(entries(home.path()), vec![fresh_tmp]);
}

// ── L-B: successive GC passes cover the whole directory ────────────────────

#[test]
fn repeated_gc_passes_remove_every_expired_entry_beyond_one_pass() {
    const LIMIT: usize = 3;
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    for n in 0..10 {
        store::write_items(&dir, &name_of("s", &format!("old-{n}")), &[item("old", now - 2 * PENDING_TTL_SECONDS)]).unwrap();
    }
    let orphan = store::lock_name(&name_of("s", "gone"));
    dir.create_file_new(&orphan, b"").unwrap();
    let live = name_of("s", "alice");
    store::write_items(&dir, &live, &[item("live", now)]).unwrap();
    // One pass handles at most LIMIT entries and says where to resume.
    let (first, mut cursor) = gc::pass(home.path(), now, LIMIT, None).unwrap();
    assert!(first.removed_ledgers + first.removed_locks <= LIMIT, "{first:?}");
    assert!(cursor.is_some());
    let (mut ledgers, mut locks, mut passes) = (first.removed_ledgers, first.removed_locks, 1);
    while let Some(start) = cursor.take() {
        assert!(passes < 12, "passes never reached the end of the directory");
        let (report, next) = gc::pass(home.path(), now, LIMIT, Some(&start)).unwrap();
        ledgers += report.removed_ledgers;
        locks += report.removed_locks;
        cursor = next;
        passes += 1;
    }
    assert_eq!((ledgers, locks), (10, 1));
    assert_eq!(entries(home.path()), vec![live.clone()]);
    // The next pass starts from the beginning again.
    let (report, next) = gc::pass(home.path(), now, LIMIT, None).unwrap();
    assert_eq!((report.removed_ledgers, next), (0, None));
}

#[test]
fn gc_removes_legacy_format_ledgers_and_their_orphan_locks() {
    let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
    let dir = store::open_dir(home.path(), true).unwrap().unwrap();
    // Names of the earlier format: owned by nobody, read by no code path.
    let legacy = format!("{}.json", "ab".repeat(32));
    store::write_items(&dir, &legacy, &[item("unreachable", now)]).unwrap();
    let legacy_lock = store::lock_name(&format!("{}.json", "cd".repeat(32)));
    dir.create_file_new(&legacy_lock, b"").unwrap();
    let live = name_of("s", "alice");
    store::write_items(&dir, &live, &[item("live", now)]).unwrap();
    let report = gc::collect(home.path(), now).unwrap();
    assert_eq!((report.removed_ledgers, report.removed_locks), (1, 1), "{report:?}");
    assert_eq!(entries(home.path()), vec![live]);
}

// ── H2 / M2: safe open (Unix-specific kernel behaviour) ────────────────────

#[cfg(unix)]
mod unix_safe_open {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn mode(p: &Path) -> u32 { std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777 }

    #[test]
    fn symlinked_ledger_is_refused_for_read_and_write_without_touching_target() {
        let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
        let dir = store::open_dir(home.path(), true).unwrap().unwrap();
        // A valid ledger outside the directory: if followed it would be injected.
        store::write_items(&dir, "victim.json", &[item("victim-id", now)]).unwrap();
        let victim = home.path().join("victim.json");
        std::fs::rename(pending_dir(home.path()).join("victim.json"), &victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        let before = std::fs::read(&victim).unwrap();
        let ledger = path(home.path(), "googlechat:space", "alice");
        symlink(&victim, &ledger).unwrap();

        assert!(reminder(home.path(), "googlechat:space", "alice", false).is_none());
        write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-1", now)]);
        assert!(update(home.path(), "googlechat:space", "alice", "agent", "turn-1", cap(10)).is_err());

        assert_eq!(std::fs::read(&victim).unwrap(), before);
        assert_eq!(mode(&victim), 0o644, "target permissions must not be changed");
        assert!(std::fs::symlink_metadata(&ledger).unwrap().file_type().is_symlink());
        assert!(!lock_path(home.path(), "googlechat:space", "alice").exists());
    }

    #[test]
    fn symlinked_lock_sidecar_and_directory_are_refused() {
        let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
        store::open_dir(home.path(), true).unwrap().unwrap();
        let victim = home.path().join("victim");
        std::fs::write(&victim, b"KEEP").unwrap();
        symlink(&victim, lock_path(home.path(), "googlechat:space", "alice")).unwrap();
        // A dangling lock symlink must not create its target either.
        let dangling = home.path().join("created-through-link");
        symlink(&dangling, lock_path(home.path(), "googlechat:space", "bob")).unwrap();
        write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-1", now)]);
        assert!(update(home.path(), "googlechat:space", "alice", "agent", "turn-1", cap(10)).is_err());
        assert!(update(home.path(), "googlechat:space", "bob", "agent", "turn-1", cap(10)).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"KEEP");
        assert!(!dangling.exists());
        assert!(!path(home.path(), "googlechat:space", "alice").exists());

        let other = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), pending_dir(other.path())).unwrap();
        write_rows(other.path(), &[row("gmail_create_draft", "draft", "d-1", now)]);
        assert!(update(other.path(), "googlechat:space", "alice", "agent", "turn-1", cap(10)).is_err());
        assert!(reminder(other.path(), "googlechat:space", "alice", false).is_none());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn fifo_at_ledger_path_does_not_block_read_or_write() {
        let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
        store::open_dir(home.path(), true).unwrap().unwrap();
        let ledger = path(home.path(), "googlechat:space", "alice");
        nix::unistd::mkfifo(&ledger, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
        write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-1", now)]);
        let (tx, rx) = std::sync::mpsc::channel();
        let home_path = home.path().to_path_buf();
        std::thread::spawn(move || {
            let read = reminder(&home_path, "googlechat:space", "alice", false);
            let write = update(&home_path, "googlechat:space", "alice", "agent", "turn-1", cap(10));
            let _ = tx.send((read.is_none(), write.is_err()));
        });
        let (no_reminder, refused) = rx.recv_timeout(Duration::from_secs(10)).expect("FIFO blocked the ledger");
        assert!(no_reminder && refused);
        use std::os::unix::fs::FileTypeExt;
        assert!(std::fs::symlink_metadata(&ledger).unwrap().file_type().is_fifo());
    }

    #[test]
    fn hard_linked_ledger_is_refused_for_read_and_write() {
        let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
        write_rows(home.path(), &[row("gmail_create_draft", "draft", "d-1", now)]);
        finish_turn(home.path(), "googlechat:space", "alice", "agent", "turn-1");
        let ledger = path(home.path(), "googlechat:space", "alice");
        let other = home.path().join("other-link");
        std::fs::hard_link(&ledger, &other).unwrap();
        let before = std::fs::read(&other).unwrap();
        assert!(reminder(home.path(), "googlechat:space", "alice", false).is_none());
        write_rows(home.path(), &[row_in("googlechat:space", "turn-2", "gmail_create_draft", "draft", "d-2", now)]);
        assert!(update(home.path(), "googlechat:space", "alice", "agent", "turn-2", cap(10)).is_err());
        assert_eq!(std::fs::read(&other).unwrap(), before);
        assert_eq!(std::fs::read(&ledger).unwrap(), before);
    }

    #[test]
    fn hard_linked_lock_sidecar_is_still_refused() {
        // The retry for a sidecar unlinked in flight never accepts one that
        // has several links at rest.
        let home = tempfile::tempdir().unwrap();
        let dir = store::open_dir(home.path(), true).unwrap().unwrap();
        dir.create_file_new(store::ADMISSION_LOCK, b"").unwrap();
        std::fs::hard_link(pending_dir(home.path()).join(store::ADMISSION_LOCK), home.path().join("other-link")).unwrap();
        for create in [true, false] {
            let error = store::lock(&dir, store::ADMISSION_LOCK, create, store::TURN_LOCK_WAIT).err().expect("must be refused");
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied, "{error}");
        }
        assert_eq!(entries(home.path()), vec![store::ADMISSION_LOCK.to_string()], "the linked sidecar is left alone");
    }

    #[test]
    fn first_read_tightens_restored_directory_and_ledger_permissions() {
        let home = tempfile::tempdir().unwrap(); let now = chrono::Utc::now().timestamp();
        let dir = store::open_dir(home.path(), true).unwrap().unwrap();
        let name = name_of("googlechat:space", "alice");
        store::write_items(&dir, &name, &[item("d-1", now)]).unwrap();
        drop(dir);
        let ledger = path(home.path(), "googlechat:space", "alice");
        std::fs::set_permissions(pending_dir(home.path()), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(reminder(home.path(), "googlechat:space", "alice", false).unwrap().contains("d-1"));
        assert_eq!(mode(&pending_dir(home.path())), 0o700);
        assert_eq!(mode(&ledger), 0o600);
    }
}
