use super::*;

use duduclaw_security::action_claim_verifier::ActionClaim;

const SESSION: &str = "line:U1";
const REPLY: &str = "好的，已經處理好了。";

fn unbacked(claim_type: ClaimType, kind: UnbackedKind) -> UnbackedClaim {
    UnbackedClaim {
        claim: ActionClaim { claim_type, target_id: String::new(), matched_text: "已寄出".into() },
        kind,
        reason: "operator-only reason mentioning mail_send".into(),
    }
}

fn notice(
    mode: ClaimCheckMode,
    staff: bool,
    session: &str,
    reply: &str,
    list: &[UnbackedClaim],
    language: Option<&str>,
) -> Option<String> {
    notice_for_reply(mode, staff, session, reply, list, language)
}

#[test]
fn shadow_mode_never_adds_a_notice() {
    let list = [unbacked(ClaimType::CalendarEventCreated, UnbackedKind::NoRecord)];
    assert_eq!(notice(ClaimCheckMode::Shadow, true, SESSION, REPLY, &list, None), None);
}

#[test]
fn non_staff_never_gets_a_notice() {
    let list = [unbacked(ClaimType::MailSent, UnbackedKind::DraftOnly)];
    assert_eq!(notice(ClaimCheckMode::Notice, false, SESSION, REPLY, &list, None), None);
}

#[test]
fn blank_reply_gets_no_notice() {
    let list = [unbacked(ClaimType::SheetWritten, UnbackedKind::NoRecord)];
    for reply in ["", "  \n "] {
        assert_eq!(notice(ClaimCheckMode::Notice, true, SESSION, reply, &list, None), None);
        assert_eq!(append_notice(reply.to_string(), Some("x".into())), reply);
    }
}

#[test]
fn public_session_gets_no_notice_even_if_marked_staff() {
    let list = [unbacked(ClaimType::TaskCreated, UnbackedKind::NoRecord)];
    for session in ["odoolivechat:site:visitor-1", "odoolivechat:site:visitor-1#agent:sales"] {
        assert_eq!(notice(ClaimCheckMode::Notice, true, session, REPLY, &list, None), None, "{session}");
    }
}

#[test]
fn nothing_unbacked_gives_no_notice() {
    assert_eq!(notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &[], None), None);
    // Agent-management claims are never part of this notice.
    let list = [unbacked(ClaimType::AgentCreated, UnbackedKind::NoRecord)];
    assert_eq!(notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, None), None);
}

#[test]
fn no_record_wording_zh_tw_matches_the_spec() {
    let list = [unbacked(ClaimType::CalendarEventCreated, UnbackedKind::NoRecord)];
    let text = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some("zh-TW")).unwrap();
    assert_eq!(
        text,
        "（系統提醒：這則回覆提到已完成「建立行事曆活動」，但系統在這段對話中查不到對應的成功紀錄，請再確認是否真的完成。）"
    );
    // No language configured reads as zh-TW, like the failure notices.
    assert_eq!(notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, None).unwrap(), text);
}

#[test]
fn draft_only_wording() {
    let list = [unbacked(ClaimType::MailSent, UnbackedKind::DraftOnly)];
    let text = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some("zh-TW")).unwrap();
    assert_eq!(text, "（系統提醒：這封信目前只是草稿或待確認，尚未寄出；需要管理員在儀表板確認後才會寄出。）");
    // A mail claim with no row at all uses the no-record wording instead.
    let list = [unbacked(ClaimType::MailSent, UnbackedKind::NoRecord)];
    let text = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some("zh-TW")).unwrap();
    assert!(text.contains("「寄信」") && !text.contains("草稿"), "{text}");
}

#[test]
fn several_claims_give_one_notice_with_at_most_three_kinds() {
    let list = [
        unbacked(ClaimType::TaskCreated, UnbackedKind::NoRecord),
        unbacked(ClaimType::DocWritten, UnbackedKind::NoRecord),
        unbacked(ClaimType::SheetWritten, UnbackedKind::NoRecord),
        unbacked(ClaimType::CalendarEventCreated, UnbackedKind::NoRecord),
        unbacked(ClaimType::MailSent, UnbackedKind::DraftOnly),
    ];
    let text = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some("zh-TW")).unwrap();
    // One paragraph: the draft sentence and one no-record sentence naming
    // the next two kinds in a stable order; the last two kinds are dropped.
    assert!(!text.contains('\n'), "{text}");
    assert_eq!(text.matches("系統提醒").count(), 2, "{text}");
    assert!(text.contains("「建立行事曆活動」、「寫入試算表」"), "{text}");
    assert!(text.contains("尚未寄出"), "{text}");
    assert!(!text.contains("寫入文件") && !text.contains("建立任務"), "{text}");

    let reply = append_notice(REPLY.to_string(), Some(text.clone()));
    assert_eq!(reply, format!("{REPLY}\n\n{text}"));
    assert_eq!(reply.matches("\n\n").count(), 1);
}

#[test]
fn other_languages() {
    let list = [
        unbacked(ClaimType::MailSent, UnbackedKind::DraftOnly),
        unbacked(ClaimType::SheetWritten, UnbackedKind::NoRecord),
    ];
    let en = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some("en-US")).unwrap();
    assert!(en.starts_with("(System note:") && en.contains("writing to the spreadsheet") && en.contains("has not been sent"), "{en}");
    let ja = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some("ja-JP")).unwrap();
    assert!(ja.contains("スプレッドシートへの書き込み") && ja.contains("送信されていません"), "{ja}");
    let cn = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some("zh-CN")).unwrap();
    assert!(cn.contains("系统提醒") && cn.contains("「写入电子表格」"), "{cn}");
}

#[test]
fn notice_carries_no_internal_terms() {
    let list = [
        unbacked(ClaimType::MailSent, UnbackedKind::DraftOnly),
        unbacked(ClaimType::CalendarEventCreated, UnbackedKind::NoRecord),
        unbacked(ClaimType::TaskCreated, UnbackedKind::NoRecord),
    ];
    for language in ["zh-TW", "zh-CN", "en", "ja-JP"] {
        let text = notice(ClaimCheckMode::Notice, true, SESSION, REPLY, &list, Some(language)).unwrap();
        for banned in ["mail_send", "calendar_create_event", "tasks_create", "tool", ".jsonl", "_", "/", "id="] {
            assert!(!text.contains(banned), "{language}: {banned:?} in {text}");
        }
    }
}

#[test]
fn mode_values() {
    assert_eq!(ClaimCheckMode::from_value(None), ClaimCheckMode::Notice);
    let s = |v: &str| toml::Value::String(v.into());
    assert_eq!(ClaimCheckMode::from_value(Some(&s("notice"))), ClaimCheckMode::Notice);
    assert_eq!(ClaimCheckMode::from_value(Some(&s(" Shadow "))), ClaimCheckMode::Shadow);
    assert_eq!(ClaimCheckMode::from_value(Some(&s("enforce"))), ClaimCheckMode::Shadow);
    assert_eq!(ClaimCheckMode::from_value(Some(&toml::Value::Boolean(true))), ClaimCheckMode::Shadow);
}

#[test]
fn mode_from_home() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(ClaimCheckMode::from_home(home.path()), ClaimCheckMode::Notice, "no config file");
    let write = |body: &str| std::fs::write(home.path().join("config.toml"), body).unwrap();
    write("[general]\nname = \"x\"\n");
    assert_eq!(ClaimCheckMode::from_home(home.path()), ClaimCheckMode::Notice, "no section");
    write("[action_claims]\n");
    assert_eq!(ClaimCheckMode::from_home(home.path()), ClaimCheckMode::Notice, "no key");
    write("[action_claims]\nmode = \"shadow\"\n");
    assert_eq!(ClaimCheckMode::from_home(home.path()), ClaimCheckMode::Shadow);
    write("[action_claims]\nmode = \"typo\"\n");
    assert_eq!(ClaimCheckMode::from_home(home.path()), ClaimCheckMode::Shadow);
    write("this is not toml = = =");
    assert_eq!(ClaimCheckMode::from_home(home.path()), ClaimCheckMode::Shadow, "unparsable");
}

// ── Recording (H1) ───────────────────────────────────────

fn audit_rows(home: &Path) -> Vec<serde_json::Value> {
    match std::fs::read_to_string(home.join("security_audit.jsonl")) {
        Ok(body) => body
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|row| row["event_type"] == "tool_hallucination")
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn record_decision_is_audience_first() {
    let now = Instant::now();
    let recent = Some(now);
    let old = now.checked_sub(AUDIT_WINDOW + Duration::from_secs(1));
    // Non-staff: counter only, whatever the limiter state.
    for last in [None, recent, old] {
        assert_eq!(record_decision(false, last, now), Record::CountOnly);
    }
    assert_eq!(record_decision(true, None, now), Record::Audit);
    assert_eq!(record_decision(true, recent, now), Record::Suppressed);
    assert_eq!(record_decision(true, Some(now), now + AUDIT_WINDOW - Duration::from_secs(1)), Record::Suppressed);
    assert_eq!(record_decision(true, Some(now), now + AUDIT_WINDOW), Record::Audit);
}

/// `decide` as (outcome, count reported on the row).
fn view(decision: Decision) -> (Record, u64) {
    match decision {
        Decision::Audit(ticket) => (Record::Audit, ticket.reported),
        Decision::Suppressed => (Record::Suppressed, 0),
        Decision::Overflow => (Record::Overflow, 0),
    }
}

/// The ticket of an `Audit` decision (panics otherwise).
fn ticket(decision: Decision) -> AuditTicket {
    match decision {
        Decision::Audit(ticket) => ticket,
        other => panic!("expected Audit, got {other:?}"),
    }
}

/// `decide`, then `commit` on `Audit` (a row that was written).
fn admit(limiter: &mut ClaimLimiter, agent: &str, family: &ClaimType, at: Instant) -> (Record, u64) {
    let decision = limiter.decide(agent, family, at);
    if let Decision::Audit(ticket) = decision {
        assert!(limiter.commit(agent, family, ticket, at));
    }
    view(decision)
}

#[test]
fn limiter_audits_once_per_window_and_reports_suppressed() {
    let mut limiter = ClaimLimiter::default();
    let now = Instant::now();
    let mail = ClaimType::MailSent;
    assert_eq!(admit(&mut limiter, "sales", &mail, now), (Record::Audit, 0));
    assert_eq!(admit(&mut limiter, "sales", &mail, now + Duration::from_secs(60)), (Record::Suppressed, 0));
    assert_eq!(admit(&mut limiter, "sales", &mail, now + Duration::from_secs(120)), (Record::Suppressed, 0));
    // Another family or another employee has its own window.
    assert_eq!(admit(&mut limiter, "sales", &ClaimType::TaskCreated, now), (Record::Audit, 0));
    assert_eq!(admit(&mut limiter, "ops", &mail, now), (Record::Audit, 0));
    // After the window: audited again, with the two suppressed occurrences.
    assert_eq!(admit(&mut limiter, "sales", &mail, now + AUDIT_WINDOW), (Record::Audit, 2));
    assert_eq!(admit(&mut limiter, "sales", &mail, now + AUDIT_WINDOW + Duration::from_secs(1)), (Record::Suppressed, 0));
}

#[test]
fn limiter_decide_alone_starts_no_window() {
    let mut limiter = ClaimLimiter::default();
    let now = Instant::now();
    let mail = ClaimType::MailSent;
    // Phase one reserves the key; a release (the row was never written)
    // leaves nothing tracked and starts no window.
    let first = ticket(limiter.decide("sales", &mail, now));
    assert_eq!(first.reported, 0);
    assert_eq!(limiter.len(), 1, "reserved while the write is in flight");
    limiter.release("sales", &mail, first);
    assert_eq!(limiter.len(), 0);
    assert_eq!(view(limiter.decide("sales", &mail, now + Duration::from_secs(1))), (Record::Audit, 0));
}

#[test]
fn concurrent_decisions_for_one_key_give_exactly_one_audit() {
    let mut limiter = ClaimLimiter::default();
    let now = Instant::now();
    let mail = ClaimType::MailSent;
    // Four staff checks decide for the same key before any of them commits.
    let decisions: Vec<Decision> =
        (0..4u64).map(|i| limiter.decide("sales", &mail, now + Duration::from_millis(i))).collect();
    let audits: Vec<AuditTicket> =
        decisions.iter().filter_map(|d| if let Decision::Audit(t) = d { Some(*t) } else { None }).collect();
    assert_eq!(audits.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0], Decision::Audit(audits[0]));
    assert!(decisions[1..].iter().all(|d| *d == Decision::Suppressed), "{decisions:?}");
    // Other keys are not blocked by this reservation.
    assert_eq!(view(limiter.decide("sales", &ClaimType::TaskCreated, now)), (Record::Audit, 0));
    assert_eq!(view(limiter.decide("ops", &mail, now)), (Record::Audit, 0));
    // The one writer commits; the window applies from then on.
    assert!(limiter.commit("sales", &mail, audits[0], now + Duration::from_secs(1)));
    assert_eq!(view(limiter.decide("sales", &mail, now + Duration::from_secs(2))), (Record::Suppressed, 0));
    // A second commit with the same (now used) ticket changes nothing.
    assert!(!limiter.commit("sales", &mail, audits[0], now + Duration::from_secs(3)));
    // The three in-flight occurrences and the later one are all reported.
    let after = now + Duration::from_secs(1) + AUDIT_WINDOW;
    assert_eq!(view(limiter.decide("sales", &mail, after)), (Record::Audit, 4));
}

#[test]
fn occurrences_during_the_write_are_reported_once_by_the_next_row() {
    let mut limiter = ClaimLimiter::default();
    let t0 = Instant::now();
    let mail = ClaimType::MailSent;
    assert_eq!(admit(&mut limiter, "sales", &mail, t0), (Record::Audit, 0));
    // Two occurrences inside the first window.
    for s in [1, 2] {
        assert_eq!(admit(&mut limiter, "sales", &mail, t0 + Duration::from_secs(s)).0, Record::Suppressed);
    }
    // The second row reports those two; three more arrive while it is written.
    let t1 = t0 + AUDIT_WINDOW;
    let second = ticket(limiter.decide("sales", &mail, t1));
    assert_eq!(second.reported, 2);
    for ms in [1, 2, 3] {
        assert_eq!(limiter.decide("sales", &mail, t1 + Duration::from_millis(ms)), Decision::Suppressed);
    }
    let committed = t1 + Duration::from_secs(1);
    assert!(limiter.commit("sales", &mail, second, committed));
    // Not lost and not double-reported: the third row reports exactly the
    // three that arrived during the second write, the fourth reports none.
    let t2 = committed + AUDIT_WINDOW;
    assert_eq!(admit(&mut limiter, "sales", &mail, t2), (Record::Audit, 3));
    assert_eq!(admit(&mut limiter, "sales", &mail, t2 + AUDIT_WINDOW), (Record::Audit, 0));
}

#[test]
fn failed_write_releases_the_reservation_and_keeps_the_count() {
    let mut limiter = ClaimLimiter::default();
    let t0 = Instant::now();
    let mail = ClaimType::MailSent;
    assert_eq!(admit(&mut limiter, "sales", &mail, t0), (Record::Audit, 0));
    for s in [1, 2] {
        assert_eq!(admit(&mut limiter, "sales", &mail, t0 + Duration::from_secs(s)).0, Record::Suppressed);
    }
    let t1 = t0 + AUDIT_WINDOW;
    let failed = ticket(limiter.decide("sales", &mail, t1));
    assert_eq!(failed.reported, 2);
    // One more occurrence while the (failing) write is in flight.
    assert_eq!(limiter.decide("sales", &mail, t1 + Duration::from_millis(1)), Decision::Suppressed);
    limiter.release("sales", &mail, failed);
    // No window started: the next occurrence audits and reports everything
    // still pending (the preserved two plus the in-flight one).
    let retry = ticket(limiter.decide("sales", &mail, t1 + Duration::from_secs(1)));
    assert_eq!(retry.reported, 3);
    // The failed writer's ticket can no longer release or commit.
    limiter.release("sales", &mail, failed);
    assert!(!limiter.commit("sales", &mail, failed, t1 + Duration::from_secs(1)));
    assert_eq!(limiter.decide("sales", &mail, t1 + Duration::from_secs(1)), Decision::Suppressed, "still reserved");
    assert!(limiter.commit("sales", &mail, retry, t1 + Duration::from_secs(2)));
    assert_eq!(view(limiter.decide("sales", &mail, t1 + Duration::from_secs(2) + AUDIT_WINDOW)), (Record::Audit, 1));
}

#[test]
fn stale_reservation_expires() {
    let mut limiter = ClaimLimiter::default();
    let t0 = Instant::now();
    let mail = ClaimType::MailSent;
    // A writer that never resolves its reservation (its task panicked).
    let lost = ticket(limiter.decide("sales", &mail, t0));
    let almost = t0 + RESERVATION_TIMEOUT - Duration::from_secs(1);
    assert_eq!(limiter.decide("sales", &mail, almost), Decision::Suppressed);
    // Past the bound the key is free again, with the pending count kept.
    let expired = t0 + RESERVATION_TIMEOUT;
    let fresh = ticket(limiter.decide("sales", &mail, expired));
    assert_eq!(fresh.reported, 1);
    assert_ne!(fresh, lost);
    // The lost writer resurfacing is ignored; the current one commits.
    assert!(!limiter.commit("sales", &mail, lost, expired));
    limiter.release("sales", &mail, lost);
    assert_eq!(limiter.decide("sales", &mail, expired), Decision::Suppressed, "fresh reservation intact");
    assert!(limiter.commit("sales", &mail, fresh, expired));
    assert_eq!(view(limiter.decide("sales", &mail, expired + AUDIT_WINDOW)), (Record::Audit, 1));
}

#[test]
fn live_reservations_count_as_live_entries_until_they_expire() {
    let mut limiter = ClaimLimiter::default();
    let start = Instant::now();
    // A table full of reservations only (writes in flight, nothing committed).
    for i in 0..LIMITER_MAX_KEYS {
        assert_eq!(view(limiter.decide(&format!("emp-{i}"), &ClaimType::SheetWritten, start)), (Record::Audit, 0));
    }
    assert_eq!(limiter.len(), LIMITER_MAX_KEYS);
    let soon = start + Duration::from_secs(5);
    assert_eq!(limiter.decide("new", &ClaimType::SheetWritten, soon), Decision::Overflow);
    assert_eq!(limiter.len(), LIMITER_MAX_KEYS, "no reserved entry dropped");
    // Expired reservations with nothing pending are reclaimed.
    let expired = start + RESERVATION_TIMEOUT;
    assert_eq!(view(limiter.decide("new", &ClaimType::SheetWritten, expired)), (Record::Audit, 0));
    assert_eq!(limiter.len(), 1);
}

fn fill_with_live_keys(limiter: &mut ClaimLimiter, at: Instant) {
    for i in 0..LIMITER_MAX_KEYS {
        assert_eq!(admit(limiter, &format!("emp-{i}"), &ClaimType::SheetWritten, at).0, Record::Audit);
    }
    assert_eq!(limiter.len(), LIMITER_MAX_KEYS);
}

#[test]
fn full_table_of_in_window_keys_does_not_audit_a_new_key() {
    let mut limiter = ClaimLimiter::default();
    let start = Instant::now();
    fill_with_live_keys(&mut limiter, start);
    let later = start + Duration::from_secs(5);
    for i in 0..50 {
        let key = format!("new-{i}");
        assert_eq!(limiter.decide(&key, &ClaimType::SheetWritten, later), Decision::Overflow);
        assert_eq!(limiter.len(), LIMITER_MAX_KEYS);
    }
    // No existing entry was removed: the oldest and the newest are still in
    // their window.
    assert_eq!(admit(&mut limiter, "emp-0", &ClaimType::SheetWritten, later).0, Record::Suppressed);
    let newest = format!("emp-{}", LIMITER_MAX_KEYS - 1);
    assert_eq!(admit(&mut limiter, &newest, &ClaimType::SheetWritten, later).0, Record::Suppressed);
    // Their pending counts were kept too.
    let after = start + AUDIT_WINDOW;
    assert_eq!(view(limiter.decide("emp-0", &ClaimType::SheetWritten, after)), (Record::Audit, 1));
}

#[test]
fn full_table_reclaims_only_expired_zero_count_entries() {
    let mut limiter = ClaimLimiter::default();
    let start = Instant::now();
    fill_with_live_keys(&mut limiter, start);
    // emp-0 has a pending suppressed count; the others have none.
    assert_eq!(admit(&mut limiter, "emp-0", &ClaimType::SheetWritten, start + Duration::from_secs(1)).0, Record::Suppressed);
    let expired = start + AUDIT_WINDOW + Duration::from_secs(1);
    assert_eq!(admit(&mut limiter, "new-0", &ClaimType::SheetWritten, expired), (Record::Audit, 0));
    // Every expired zero-count entry was reclaimed; emp-0 was kept.
    assert_eq!(limiter.len(), 2);
    assert_eq!(view(limiter.decide("emp-0", &ClaimType::SheetWritten, expired)), (Record::Audit, 1));
}

#[test]
fn full_limiter_table_counts_overflow_and_reports_it_on_the_next_row() {
    let home = tempfile::tempdir().unwrap();
    let limiter = Mutex::new(ClaimLimiter::default());
    let skips = SkipLog::new();
    let start = Instant::now();
    fill_with_live_keys(&mut limiter.lock().unwrap(), start);
    let list = [unbacked(ClaimType::SheetWritten, UnbackedKind::NoRecord)];
    let writes = std::cell::Cell::new(0u32);
    let counting = |_: &UnbackedClaim, _: u64, _: u64| -> std::io::Result<()> {
        writes.set(writes.get() + 1);
        Ok(())
    };
    let later = start + Duration::from_secs(5);
    assert_eq!(record_unbacked("new", SESSION, &list, &limiter, &skips, later, &counting), vec![Record::Overflow]);
    assert_eq!(writes.get(), 0, "an overflowing key is not audited");
    assert_eq!(skips.total(Skip::LimiterFull), 1);
    assert_eq!(limiter.lock().unwrap().len(), LIMITER_MAX_KEYS, "no existing entry removed");
    // The next written row carries the overflow total.
    let after = start + AUDIT_WINDOW;
    let write = audit_writer(home.path(), "emp-7");
    assert_eq!(record_unbacked("emp-7", SESSION, &list, &limiter, &skips, after, &write), vec![Record::Audit]);
    let rows = audit_rows(home.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["details"]["limiter_overflow_total"], 1);
}

#[test]
fn failed_audit_write_does_not_suppress_the_next_occurrence() {
    let home = tempfile::tempdir().unwrap();
    let limiter = Mutex::new(ClaimLimiter::default());
    let skips = SkipLog::new();
    let list = [unbacked(ClaimType::MailSent, UnbackedKind::DraftOnly)];
    let now = Instant::now();
    let failing = |_: &UnbackedClaim, _: u64, _: u64| -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "lock_wait_timeout"))
    };
    assert_eq!(record_unbacked("sales", SESSION, &list, &limiter, &skips, now, &failing), vec![Record::WriteFailed]);
    assert_eq!(skips.total(Skip::AuditWriteFailed), 1);
    assert_eq!(limiter.lock().unwrap().len(), 0, "no suppression window started");
    let write = audit_writer(home.path(), "sales");
    let soon = now + Duration::from_secs(1);
    assert_eq!(record_unbacked("sales", SESSION, &list, &limiter, &skips, soon, &write), vec![Record::Audit]);
    assert_eq!(audit_rows(home.path()).len(), 1);
    // Once written, the window applies.
    assert_eq!(record_unbacked("sales", SESSION, &list, &limiter, &skips, soon, &write), vec![Record::Suppressed]);
    assert_eq!(audit_rows(home.path()).len(), 1);
}

#[test]
fn record_unbacked_suppresses_occurrences_during_its_write_without_holding_the_lock() {
    let limiter = Mutex::new(ClaimLimiter::default());
    let skips = SkipLog::new();
    let list = [unbacked(ClaimType::DocWritten, UnbackedKind::NoRecord)];
    let now = Instant::now();
    let reported = std::cell::RefCell::new(Vec::new());
    // While the row is being written, another check for the same key decides
    // (the lock must be free, so `try_lock` must succeed) and is suppressed.
    let racing = |_: &UnbackedClaim, suppressed: u64, _: u64| -> std::io::Result<()> {
        let mut guard = limiter.try_lock().expect("limiter lock not held across the write");
        assert_eq!(guard.decide("sales", &ClaimType::DocWritten, now), Decision::Suppressed);
        reported.borrow_mut().push(suppressed);
        Ok(())
    };
    assert_eq!(record_unbacked("sales", SESSION, &list, &limiter, &skips, now, &racing), vec![Record::Audit]);
    let plain = |_: &UnbackedClaim, suppressed: u64, _: u64| -> std::io::Result<()> {
        reported.borrow_mut().push(suppressed);
        Ok(())
    };
    let soon = now + Duration::from_secs(1);
    assert_eq!(record_unbacked("sales", SESSION, &list, &limiter, &skips, soon, &plain), vec![Record::Suppressed]);
    let after = now + AUDIT_WINDOW;
    assert_eq!(record_unbacked("sales", SESSION, &list, &limiter, &skips, after, &plain), vec![Record::Audit]);
    // First row 0; the next row reports the in-flight one and the later one.
    assert_eq!(*reported.borrow(), vec![0, 2]);
}

#[test]
fn staff_audit_row_is_rate_limited() {
    let home = tempfile::tempdir().unwrap();
    let limiter = Mutex::new(ClaimLimiter::default());
    let skips = SkipLog::new();
    let write = audit_writer(home.path(), "sales");
    let list = [unbacked(ClaimType::CalendarEventCreated, UnbackedKind::NoRecord)];
    let now = Instant::now();
    let first = record_unbacked("sales", SESSION, &list, &limiter, &skips, now, &write);
    assert_eq!(first, vec![Record::Audit]);
    assert_eq!(audit_rows(home.path()).len(), 1);
    let second = record_unbacked("sales", SESSION, &list, &limiter, &skips, now + Duration::from_secs(60), &write);
    assert_eq!(second, vec![Record::Suppressed]);
    assert_eq!(audit_rows(home.path()).len(), 1, "a second occurrence inside the window writes no row");
    let third = record_unbacked("sales", SESSION, &list, &limiter, &skips, now + AUDIT_WINDOW + Duration::from_secs(60), &write);
    assert_eq!(third, vec![Record::Audit]);
    let rows = audit_rows(home.path());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["details"]["suppressed_since_last_row"], 0);
    assert_eq!(rows[1]["details"]["suppressed_since_last_row"], 1);
    assert_eq!(rows[1]["details"]["limiter_overflow_total"], 0);
    assert_eq!(rows[1]["severity"], "critical");
    assert_eq!(rows[1]["agent_id"], "sales");
}

// ── Admission and the blocking task (H) ─────────────────

fn claims() -> Vec<ActionClaim> {
    vec![ActionClaim { claim_type: ClaimType::MailSent, target_id: String::new(), matched_text: "已寄出".into() }]
}

async fn wait_for_all_permits(slots: &Arc<Semaphore>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while slots.available_permits() < CLAIM_CHECK_PERMITS {
        assert!(Instant::now() < deadline, "permit not released");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn non_staff_reply_performs_no_lookup() {
    let slots = Arc::new(Semaphore::new(CLAIM_CHECK_PERMITS));
    let skips = SkipLog::new();
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let before = NON_STAFF_CLAIMS.load(Ordering::Relaxed);
    let out = run_check(false, claims(), &slots, &skips, CLAIM_CHECK_DEADLINE, move |_, _| {
        flag.store(true, Ordering::SeqCst);
    })
    .await;
    assert!(out.is_none());
    assert!(!ran.load(Ordering::SeqCst), "no lookup / blocking task for a non-staff audience");
    assert_eq!(slots.available_permits(), CLAIM_CHECK_PERMITS, "no permit taken");
    assert!(NON_STAFF_CLAIMS.load(Ordering::Relaxed) > before, "counted");
    for skip in [Skip::Busy, Skip::NoAnswer, Skip::TaskFailed] {
        assert_eq!(skips.total(skip), 0);
    }
}

#[tokio::test]
async fn staff_reply_is_skipped_when_all_permits_are_taken() {
    let slots = Arc::new(Semaphore::new(CLAIM_CHECK_PERMITS));
    let skips = SkipLog::new();
    let held = Arc::clone(&slots).try_acquire_many_owned(CLAIM_CHECK_PERMITS as u32).unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let out = run_check(true, claims(), &slots, &skips, CLAIM_CHECK_DEADLINE, move |_, tx| {
        flag.store(true, Ordering::SeqCst);
        let _ = tx.send(Some(Vec::new()));
    })
    .await;
    assert!(out.is_none(), "no notice");
    assert!(!ran.load(Ordering::SeqCst), "the check did not run");
    assert_eq!(skips.total(Skip::Busy), 1);
    drop(held);
    assert_eq!(slots.available_permits(), CLAIM_CHECK_PERMITS);
}

#[tokio::test]
async fn permit_is_released_only_when_the_blocking_work_ends() {
    let slots = Arc::new(Semaphore::new(CLAIM_CHECK_PERMITS));
    let skips = SkipLog::new();
    let (release, released) = std::sync::mpsc::channel::<()>();
    // The work outlives the reply's wait.
    let out = run_check(true, claims(), &slots, &skips, Duration::from_millis(50), move |_, _tx| {
        let _ = released.recv_timeout(Duration::from_secs(10));
    })
    .await;
    assert!(out.is_none());
    assert_eq!(skips.total(Skip::NoAnswer), 1);
    assert_eq!(slots.available_permits(), CLAIM_CHECK_PERMITS - 1, "held after the reply stopped waiting");
    release.send(()).unwrap();
    wait_for_all_permits(&slots).await;

    // A normal check: the evidence reaches the reply, then the permit is back.
    let sent = vec![unbacked(ClaimType::TaskCreated, UnbackedKind::NoRecord)];
    let out = run_check(true, claims(), &slots, &skips, CLAIM_CHECK_DEADLINE, move |_, tx| {
        let _ = tx.send(Some(sent));
    })
    .await
    .expect("evidence");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].claim.claim_type, ClaimType::TaskCreated);
    wait_for_all_permits(&slots).await;
}

#[tokio::test]
async fn task_ending_without_an_answer_is_counted() {
    let slots = Arc::new(Semaphore::new(CLAIM_CHECK_PERMITS));
    let skips = SkipLog::new();
    let out = run_check(true, claims(), &slots, &skips, CLAIM_CHECK_DEADLINE, move |_, tx| drop(tx)).await;
    assert!(out.is_none());
    assert_eq!(skips.total(Skip::TaskFailed), 1);
    wait_for_all_permits(&slots).await;
}

#[test]
fn skip_aggregation_warns_at_most_once_per_window() {
    let skips = SkipLog::new();
    let now = Instant::now();
    let kinds = [Skip::Busy, Skip::LockTimeout, Skip::ReadError, Skip::AuditWriteFailed, Skip::NoAnswer];
    let emitted = (0..100).filter(|i| skips.note(kinds[i % kinds.len()], now)).count();
    assert_eq!(emitted, 1);
    assert!(!skips.note(Skip::TaskFailed, now + SKIP_LOG_INTERVAL - Duration::from_secs(1)));
    assert!(skips.note(Skip::LimiterFull, now + SKIP_LOG_INTERVAL));
    assert_eq!(skips.total(Skip::Busy), 20);
    assert_eq!(skips.total(Skip::TaskFailed), 1);
    assert_eq!(skips.total(Skip::LimiterFull), 1);
}

#[test]
fn staff_audience_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    for (conversation, user) in [
        ("odoolivechat:site:visitor-1", "visitor-1"),
        ("cron:daily", "system"),
        ("dispatch:run-1", "alice"),
        ("unknown:x", "alice"),
        ("default", "alice"),
        // A group chat never gets the notice, even from staff.
        ("telegram:-1001", "42"),
        ("slack:group:C1", "U1"),
        // WebChat without a signed-in dashboard account.
        ("webchat:socket-1", "socket-1"),
        // A 1:1 chat whose sender has no verified identity (no identity store).
        ("telegram:42", "42"),
    ] {
        assert!(!staff_audience(home.path(), conversation, conversation, user, "sales"), "{conversation}");
    }
}
