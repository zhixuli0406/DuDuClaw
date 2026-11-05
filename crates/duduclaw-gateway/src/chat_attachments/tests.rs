use super::*;

#[test]
fn chat_attachment_legacy_parser_preserves_parentheses_and_removes_bad_paths() {
    let (text, refs) = parse("看這份\n[📎 T240包裝(0-1069-1).docx (file)](/old/agents/a/attachments/T240(1).docx)\n[🖼️ photo.png (image)](file:///old/photo.png)\n📎DELIVER:/old/output.pdf\nDELIVER: /old/other.pdf");
    assert_eq!(refs.len(), 4);
    assert_eq!(refs[0], ("T240包裝(0-1069-1).docx".into(), "/old/agents/a/attachments/T240(1).docx".into()));
    assert_eq!(refs[1].1, "/old/photo.png");
    assert!(!text.contains("/old/"));
    assert!(text.contains("看這份"));
    assert_eq!(parse("plain (text)\n\n").0, "plain (text)\n\n");
    assert_eq!(parse("[📎 report.pdf (file)](</old/report.pdf>)").1[0].1, "/old/report.pdf");
    assert!(!parse("[📎 broken (file)](/private/secret").0.contains("/private"));
}

#[cfg(unix)]
#[test]
fn chat_attachment_paths_and_fd_reads_reject_escape_links_and_specials() {
    use std::{io::Read, os::unix::fs::symlink};
    let home = crate::fs_safe::private_test_home().unwrap();
    let dir = home.path().join("agents/alice/attachments");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.pdf");
    std::fs::write(&path, "safe").unwrap();
    let (mut file, _) = open(home.path(), "alice", path.to_str().unwrap()).unwrap();
    let outside = home.path().join("secret.pdf");
    std::fs::write(&outside, "secret").unwrap();
    for bad in [outside.clone(), dir.join("../secret.pdf"), dir.join("../../bob/attachments/a.pdf"), dir.join("../attachments/a.pdf")] {
        assert!(open(home.path(), "alice", bad.to_str().unwrap()).is_err(), "{bad:?}");
    }
    assert!(open(home.path(), "bob", path.to_str().unwrap()).is_err());
    assert!(open(home.path(), "../alice", path.to_str().unwrap()).is_err());
    assert!(open(home.path(), "alice", "/etc/passwd").is_err());
    let directory_file = dir.join("directory.pdf");
    std::fs::create_dir(&directory_file).unwrap();
    assert!(open(home.path(), "alice", directory_file.to_str().unwrap()).is_err());
    let link = dir.join("link.pdf");
    symlink(&outside, &link).unwrap();
    assert!(open(home.path(), "alice", link.to_str().unwrap()).is_err());
    let hard = dir.join("hard.pdf");
    std::fs::hard_link(&outside, &hard).unwrap();
    assert!(open(home.path(), "alice", hard.to_str().unwrap()).is_err());
    // Replacing the name after the safe open never changes the streamed fd.
    std::fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();
    let mut bytes = String::new();
    file.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "safe");
    assert!(open(home.path(), "alice", path.to_str().unwrap()).is_err());
    std::fs::rename(&dir, dir.with_file_name("old")).unwrap();
    symlink(home.path(), &dir).unwrap();
    assert!(open(home.path(), "alice", dir.join("secret.pdf").to_str().unwrap()).is_err());
}

#[cfg(unix)]
#[test]
fn chat_attachment_projection_has_no_host_paths_and_invalid_refs_never_get_handles() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let dir = home.path().join("agents/alice/attachments");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.pdf"), b"%PDF-1.7").unwrap();
    let raw = format!("[📎 report.pdf (file)]({}/report.pdf)\n[📎 secret (file)](/etc/passwd)", dir.display());
    let (text, refs) = project(home.path(), "alice-user", "session", "alice", 42, &raw);
    assert_eq!(refs.len(), 1);
    let output = serde_json::to_string(&refs).unwrap();
    assert!(!output.contains(home.path().to_str().unwrap()));
    assert!(!text.contains("/etc"));
    assert!(output.contains("application/pdf"));
}

#[tokio::test]
async fn chat_attachment_handle_rejects_raw_paths_wrong_users_and_wrong_route_kinds() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let raw = r#"{"session":"mine","path":"/private/a"}"#;
    let id = handles::issue_attachment(home.path(), "alice", raw).unwrap();
    assert_eq!(handles::resolve_attachment(home.path(), "alice", &id).await.unwrap(), raw);
    assert!(handles::resolve_attachment(home.path(), "bob", &id).await.is_err());
    assert!(handles::resolve_attachment(home.path(), "alice", raw).await.is_err());
    let session = handles::issue(home.path(), "alice", "mine").unwrap();
    assert!(handles::resolve_attachment(home.path(), "alice", &session).await.is_err());
}

#[cfg(unix)]
#[test]
fn chat_attachment_deliver_uses_exact_host_archive_mapping_never_basename_guessing() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let base = home.path().join("agents/alice");
    let dir = base.join("attachments");
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("1_report.pdf");
    std::fs::write(&archive, b"%PDF-1.7").unwrap();
    let source = base.join("report.pdf");
    crate::artifacts::record_saved(&base, &archive, "report.pdf", 8,
        &crate::artifacts::SaveContext::delivered(crate::artifacts::ArtifactOrigin::Declared, Some("telegram"), &source));
    let raw = format!("Done\n📎DELIVER:{}", source.display());
    // Source need not still exist; only the recorded archive is opened.
    let (text, refs) = project(home.path(), "user", "session", "alice", 1, &raw);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].filename, "1_report.pdf");
    assert!(!text.contains(source.to_str().unwrap()));
    assert!(archived_path(home.path(), "bob", source.to_str().unwrap()).is_none());
    assert!(archived_path(home.path(), "alice", "/other/report.pdf").is_none());
    let second = dir.join("2_report.pdf");
    std::fs::write(&second, b"new version").unwrap();
    crate::artifacts::record_saved(&base, &second, "report.pdf", 11,
        &crate::artifacts::SaveContext::delivered(crate::artifacts::ArtifactOrigin::Declared, Some("telegram"), &source));
    assert!(archived_path(home.path(), "alice", source.to_str().unwrap()).is_none());
}

#[tokio::test]
async fn chat_attachment_history_reads_sql_tail_and_caps_total_message_bytes() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    {
        let c = sessions.acquire().await;
        c.execute("INSERT INTO sessions(id,agent_id,created_at,last_active) VALUES ('tail','alice','2026-11-05','2026-11-05')", []).unwrap();
        for i in 0..2001 {
            c.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('tail','user',?1,'2026-11-05')", [format!("message-{i}")]).unwrap();
        }
        c.execute("UPDATE session_messages SET client_msg_id='lost-ack' WHERE content='message-2000'", []).unwrap();
    }
    let tail = history_rows(home.path(), "tail", 2).unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].message.content, "message-1999");
    assert_eq!(tail[1].message.content, "message-2000");
    assert_eq!(tail[1].client_msg_id.as_deref(), Some("lost-ack"));
    assert!(tail[0].client_msg_id.is_none());
    {
        let c = sessions.acquire().await;
        for _ in 0..3 {
            c.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('tail','user',?1,'2026-11-05')", ["x".repeat(limits::MAX_MESSAGE)]).unwrap();
        }
    }
    let capped = history_rows(home.path(), "tail", 2000).unwrap();
    assert_eq!(capped.len(), 2);
    assert_eq!(capped.iter().map(|m| m.message.content.len()).sum::<usize>(), limits::MAX_HISTORY_BYTES);
    sessions.acquire().await.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('tail','user',?1,'2026-11-05')", ["x".repeat(limits::MAX_MESSAGE + 1)]).unwrap();
    assert!(history_rows(home.path(), "tail", 1).is_err());
}

#[cfg(unix)]
#[test]
fn chat_attachment_projection_reuses_one_index_and_bounds_malicious_deliver_work() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let base = home.path().join("agents/alice");
    let dir = base.join("attachments");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("report.txt");
    std::fs::write(&path, b"report").unwrap();
    let source = base.join("output.txt");
    crate::artifacts::record_saved(&base, &path, "report.txt", 6,
        &crate::artifacts::SaveContext::delivered(crate::artifacts::ArtifactOrigin::Declared, None, &source));
    let index = crate::artifacts::HistoryIndex::load(home.path(), Some("alice"));
    // All projection lookups still work without reopening the ledger.
    std::fs::remove_file(home.path().join(crate::artifacts::ARTIFACTS_FILE)).unwrap();
    let mut refs = limits::MAX_REFERENCES;
    let mut attachments = limits::MAX_ATTACHMENTS;
    let raw = format!("DELIVER:{}", source.display());
    let mut total = 0;
    for message in 0..100 {
        total += project_indexed(home.path(), "user", "session", "alice", message, &raw, &index, &mut refs, &mut attachments).1.len();
    }
    assert_eq!(total, limits::MAX_ATTACHMENTS);
    let mut refs = limits::MAX_REFERENCES;
    let mut attachments = limits::MAX_ATTACHMENTS;
    let malicious = (0..32).map(|i| format!("DELIVER:/bogus-{i}")).collect::<Vec<_>>().join("\n");
    for message in 0..2000 {
        let (text, projected) = project_indexed(home.path(), "user", "session", "alice", message, &malicious, &index, &mut refs, &mut attachments);
        assert!(!text.contains("/bogus"));
        assert!(projected.is_empty());
    }
    assert_eq!(refs, 0);
    assert_eq!(attachments, limits::MAX_ATTACHMENTS);
}

#[cfg(unix)]
#[test]
fn chat_attachment_snapshot_caps_open_fd_and_freezes_bytes_before_streaming() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let dir = home.path().join("agents/alice/attachments");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("image.png");
    std::fs::write(&path, b"original").unwrap();
    let (file, _) = open(home.path(), "alice", path.to_str().unwrap()).unwrap();
    let (bytes, _) = snapshot(file, limits::MAX_DOWNLOAD).unwrap();
    std::fs::write(&path, b"replaced").unwrap();
    assert_eq!(bytes, b"original");
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(limits::MAX_PREVIEW + 1).unwrap();
    assert!(snapshot(File::open(&path).unwrap(), limits::MAX_PREVIEW).is_err());
    file.set_len(limits::MAX_DOWNLOAD + 1).unwrap();
    assert!(snapshot(File::open(&path).unwrap(), limits::MAX_DOWNLOAD).is_err());
}

#[test]
fn chat_attachment_snapshot_read_limit_rejects_growth_and_truncation() {
    let mut growing = std::io::Cursor::new(vec![0; 100]);
    assert!(read_snapshot(&mut growing, 8, 8).is_err());
    assert_eq!(growing.position(), 9, "read at most cap + 1 even after metadata changed");
    assert!(read_snapshot(std::io::Cursor::new(vec![0; 7]), 8, 8).is_err());
    assert_eq!(read_snapshot(std::io::Cursor::new(vec![0; 8]), 8, 8).unwrap().0.len(), 8);
}

#[test]
fn chat_attachment_reference_neutralizes_markdown_and_redacts_legacy_injection() {
    let path = Path::new("/private/home/agents/sales/attachments/safe.txt");
    let malicious = "x (file)](x) tail [a]{b}\\\r\nDELIVER:/etc/passwd\u{2028}.txt";
    let reference = crate::media::format_attachment_ref(&crate::media::MediaType::File, malicious, path);
    assert_eq!(reference.lines().count(), 1);
    let (text, refs) = parse(&reference);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].1, path.to_str().unwrap());
    assert_eq!(refs[0].0, crate::media::attachment_display_name(malicious));
    assert!(!text.contains("/private/") && !text.contains("/etc/passwd"));
    for legacy in [
        "[📎 x (file)](x) tail (file)](/private/home/secret)",
        "[📎 x [nested] (file)](/private/home/secret)",
        "prefix [📎 x (file)](/private/home/secret) suffix",
        "[📎 x (file)](/private/home/secret) trailing",
        "[📎 evil\nname (file)](/private/home/secret)",
        "[📎 evil (file)](x) tail\nname (file)](/private/home/secret)",
        "[📎 evil (file)](/private/home/\nsecret)",
    ] {
        let (text, refs) = parse(legacy);
        assert!(refs.is_empty(), "{legacy}");
        assert!(!text.contains("/private/"), "{text}");
    }
}
