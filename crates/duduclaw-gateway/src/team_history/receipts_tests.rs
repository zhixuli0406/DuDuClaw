use super::*;
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde_json::json;

fn event(home: &Path, report: &str, kind: &str, class: &str, viewer: &str) {
    let mut row = AuditEvent::new(
        kind,
        "sales",
        Severity::Info,
        json!({"viewer":viewer,"viewer_role":"manager","agent":"sales","report":report,"class":class,"via_depth":1,"session":"SECRET DIGEST","raw":"MUST NOT RETURN"}),
    );
    row.timestamp = "2026-10-23T10:00:00.123456Z".into();
    try_append_audit_event(home, &row).unwrap();
}
#[test]
fn receipt_reader_only_projects_own_closed_webchat_events() {
    let h = tempfile::tempdir().unwrap();
    event(
        h.path(),
        "me",
        "team_history_viewed",
        "report_webchat",
        "manager",
    );
    event(
        h.path(),
        "other",
        "team_history_viewed",
        "report_webchat",
        "manager",
    );
    event(h.path(), "me", "team_history_viewed", "external", "manager");
    event(
        h.path(),
        "me",
        "computer_team_snapshot",
        "report_webchat",
        "manager",
    );
    event(
        h.path(),
        "me",
        "team_history_listed",
        "report_webchat",
        "manager",
    );
    let page = read(h.path(), "me", None, 50).unwrap();
    assert_eq!(page.views.len(), 1);
    assert_eq!(page.views[0].viewer, "manager");
    assert_eq!(page.views[0].agent, "sales");
    assert!(page.next_before.is_none());
}
#[test]
fn receipt_cursor_is_bound_to_self_home_and_snapshot_and_preserves_equal_timestamps() {
    let h = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        event(
            h.path(),
            "me",
            "team_history_viewed",
            "report_webchat",
            "manager",
        );
    }
    let p = read(h.path(), "me", None, 1).unwrap();
    let c = p.next_before.unwrap();
    assert!(read(h.path(), "other", Some(&c), 1).is_err());
    let other = tempfile::tempdir().unwrap();
    assert!(read(other.path(), "me", Some(&c), 1).is_err());
    let p = read(h.path(), "me", Some(&c), 2).unwrap();
    assert_eq!(p.views.len(), 2);
    assert!(p.next_before.is_none());
    std::fs::write(h.path().join("security_audit.jsonl"), "{}\n").unwrap();
    assert!(read(h.path(), "me", Some(&c), 1).is_err());
}
#[test]
fn receipt_reader_distinguishes_missing_log_from_corrupt_or_unreadable_log() {
    let h = tempfile::tempdir().unwrap();
    assert!(read(h.path(), "me", None, 5).unwrap().views.is_empty());
    let p = h.path().join("security_audit.jsonl");
    std::fs::write(&p, "bad json\n").unwrap();
    assert!(read(h.path(), "me", None, 5).is_err());
    std::fs::remove_file(&p).unwrap();
    std::fs::create_dir(&p).unwrap();
    assert!(read(h.path(), "me", None, 5).is_err());
}
#[test]
fn receipt_scan_budget_can_advance_without_reporting_unrelated_counts() {
    let h = tempfile::tempdir().unwrap();
    event(
        h.path(),
        "me",
        "team_history_viewed",
        "report_webchat",
        "manager",
    );
    for _ in 0..12 {
        try_append_audit_event(
            h.path(),
            &AuditEvent::new(
                "unrelated",
                "sales",
                Severity::Info,
                json!({"padding":"x".repeat(500)}),
            ),
        )
        .unwrap();
    }
    let p = read_with_budget(h.path(), "me", None, 5, 2048).unwrap();
    assert!(p.views.is_empty());
    let c = p.next_before.unwrap();
    let p = read(h.path(), "me", Some(&c), 5).unwrap();
    assert_eq!(p.views.len(), 1);
}

#[test]
fn receipt_exact_chunk_boundary_never_skips_a_row_and_appends_preserve_snapshot() {
    let h = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        event(
            h.path(),
            "me",
            "team_history_viewed",
            "report_webchat",
            "manager",
        );
    }
    let bytes = std::fs::read(h.path().join("security_audit.jsonl")).unwrap();
    let budget = bytes.rsplit(|b| *b == b'\n').nth(1).unwrap().len() + 1;
    let first = read_with_budget(h.path(), "me", None, 50, budget).unwrap();
    assert_eq!(first.views.len(), 1);
    event(
        h.path(),
        "me",
        "team_history_viewed",
        "report_webchat",
        "new-manager",
    );
    let second = read(h.path(), "me", first.next_before.as_deref(), 50).unwrap();
    assert_eq!(second.views.len(), 2);
    assert!(second.views.iter().all(|v| v.viewer == "manager"));
    assert!(second.next_before.is_none());
}

#[test]
fn receipt_reader_rejects_corrupt_chain_partial_line_and_oversized_line() {
    for mode in 0..3 {
        let h = tempfile::tempdir().unwrap();
        event(
            h.path(),
            "me",
            "team_history_viewed",
            "report_webchat",
            "manager",
        );
        event(
            h.path(),
            "me",
            "team_history_viewed",
            "report_webchat",
            "manager",
        );
        let p = h.path().join("security_audit.jsonl");
        let mut s = std::fs::read_to_string(&p).unwrap();
        match mode {
            0 => s = s.replacen("MUST NOT RETURN", "BROKEN ORIGINAL", 1),
            1 => {
                s.pop();
            }
            _ => s = format!("{}\n", "x".repeat(MAX_LINE + 1)),
        }
        std::fs::write(p, s).unwrap();
        assert!(read(h.path(), "me", None, 50).is_err());
    }
}

#[test]
fn receipt_reader_refuses_expired_cursor_and_invalid_limits() {
    let h = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        event(
            h.path(),
            "me",
            "team_history_viewed",
            "report_webchat",
            "manager",
        );
    }
    let c = read(h.path(), "me", None, 1).unwrap().next_before.unwrap();
    cursors().lock().unwrap().get_mut(&c).unwrap().issued =
        Instant::now() - CURSOR_TTL - Duration::from_secs(1);
    assert!(read(h.path(), "me", Some(&c), 1).is_err());
    for limit in [0, 101] {
        assert!(read(h.path(), "me", None, limit).is_err());
    }
}

#[test]
fn receipt_reader_never_treats_a_malformed_known_record_as_no_views() {
    for details in [
        json!({}),
        json!({"class":"report_webchat"}),
        json!("not an object"),
    ] {
        let h = tempfile::tempdir().unwrap();
        try_append_audit_event(
            h.path(),
            &AuditEvent::new("team_history_viewed", "sales", Severity::Info, details),
        )
        .unwrap();
        assert!(read(h.path(), "me", None, 50).is_err());
    }
}

#[cfg(unix)]
#[test]
fn receipt_reader_refuses_a_symlink_or_busy_writer_without_waiting() {
    let h = tempfile::tempdir().unwrap();
    event(
        h.path(),
        "me",
        "team_history_viewed",
        "report_webchat",
        "manager",
    );
    let p = h.path().join("security_audit.jsonl");
    let file = File::open(&p).unwrap();
    fs2::FileExt::lock_exclusive(&file).unwrap();
    assert!(read(h.path(), "me", None, 50).is_err());
    drop(file);
    std::fs::rename(&p, h.path().join("parked")).unwrap();
    std::os::unix::fs::symlink(h.path().join("parked"), p).unwrap();
    assert!(read(h.path(), "me", None, 50).is_err());
}
