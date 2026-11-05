//! Bounded upload admission. Reservations survive crashes and are never undercounted.
use super::{ChatAttachment, ChatMessage};
use std::{path::Path, sync::{Arc, LazyLock}, time::{SystemTime, UNIX_EPOCH}};
use base64::Engine;
use serde::{Deserialize, de::{DeserializeSeed, Error, SeqAccess, Visitor}};

pub(super) const MAX_MESSAGE_BYTES: usize = 28 * 1024 * 1024;
const MAX_FILES: usize = 8;
const MAX_BYTES: usize = 20 * 1024 * 1024;
const MAX_ENCODED: usize = MAX_BYTES.div_ceil(3) * 4;
const USER_BYTES: u64 = 512 * 1024 * 1024;
const USER_FILES: u64 = 2048;
const GLOBAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const GLOBAL_FILES: u64 = 16384;
static WORK: LazyLock<Arc<tokio::sync::Semaphore>> = LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
const REFUSED: &str = "附件無法上傳，請減少檔案數量或大小，或稍後再試。";

pub(super) fn deserialize_attachments<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ChatAttachment>, D::Error> {
    struct Attachments;
    struct NoMore;
    impl<'de> DeserializeSeed<'de> for NoMore {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
            Err(D::Error::custom("too many attachments"))
        }
    }
    impl<'de> Visitor<'de> for Attachments {
        type Value = Vec<ChatAttachment>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str("at most eight attachments") }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut files = Vec::with_capacity(MAX_FILES);
            while files.len() < MAX_FILES {
                let Some(file) = seq.next_element::<ChatAttachment>()? else { return Ok(files); };
                if file.filename.len() > 255 || file.mime.as_ref().is_some_and(|m| m.len() > 128)
                    || file.data_base64.len() > MAX_ENCODED {
                    return Err(A::Error::custom("attachment field limit exceeded"));
                }
                files.push(file);
            }
            // SeqAccess checks for the closing bracket before invoking this seed.
            seq.next_element_seed(NoMore)?;
            Ok(files)
        }
    }
    d.deserialize_seq(Attachments)
}

pub(super) fn parse_message(text: &str) -> Result<ChatMessage, serde_json::Error> {
    // Internally tagged enums buffer Content before invoking field deserializers.
    // A directly deserialized struct enforces the sequence bound before that buffer.
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default, deserialize_with = "deserialize_attachments")]
        attachments: Vec<ChatAttachment>,
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(<serde_json::Error as serde::de::Error>::custom("message size limit exceeded"));
    }
    let envelope: Envelope = serde_json::from_str(text)?;
    drop(envelope.attachments);
    serde_json::from_str(text)
}

pub(super) struct Prepared {
    pub filename: String,
    pub mime: Option<String>,
    pub data: Vec<u8>,
}

fn validate(files: &[ChatAttachment]) -> Result<Vec<Prepared>, String> {
    if files.len() > MAX_FILES { return Err(REFUSED.into()); }
    let mut total = 0usize;
    let mut prepared = Vec::with_capacity(files.len());
    for file in files {
        if file.filename.len() > 255 || file.filename.chars().any(char::is_control)
            || file.mime.as_ref().is_some_and(|m| m.len() > 128)
            || file.data_base64.len() > MAX_ENCODED {
            return Err(REFUSED.into());
        }
        let data = base64::engine::general_purpose::STANDARD.decode(&file.data_base64)
            .map_err(|_| REFUSED.to_owned())?;
        total = total.checked_add(data.len()).ok_or_else(|| REFUSED.to_owned())?;
        if total > MAX_BYTES { return Err(REFUSED.into()); }
        prepared.push(Prepared {
            filename: if file.filename.trim().is_empty() { "upload.bin".into() } else { file.filename.clone() },
            mime: file.mime.clone().filter(|m| !m.is_empty()),
            data,
        });
    }
    Ok(prepared)
}

fn reserve(home: &Path, user: &str, files: usize, bytes: usize, now: u64) -> Result<(), String> {
    if user.trim().is_empty() { return Err(REFUSED.into()); }
    let mut db = crate::fs_safe::sqlite::open(home, "webchat_upload_quota.db").map_err(|_| REFUSED)?;
    db.busy_timeout(std::time::Duration::from_secs(2)).map_err(|_| REFUSED)?;
    db.execute_batch("PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS upload_usage (
            user_id TEXT PRIMARY KEY, bytes INTEGER NOT NULL, files INTEGER NOT NULL,
            window_start INTEGER NOT NULL, window_requests INTEGER NOT NULL,
            window_files INTEGER NOT NULL, window_bytes INTEGER NOT NULL
        );").map_err(|_| REFUSED)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| REFUSED)?;
    tx.execute("INSERT OR IGNORE INTO upload_usage VALUES (?1,0,0,?2,0,0,0)",
        rusqlite::params![user, now]).map_err(|_| REFUSED)?;
    let (used_bytes, used_files, since, requests, window_files, window_bytes): (u64,u64,u64,u64,u64,u64) = tx.query_row(
        "SELECT bytes,files,window_start,window_requests,window_files,window_bytes FROM upload_usage WHERE user_id=?1",
        [user], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).map_err(|_| REFUSED)?;
    let (all_bytes, all_files): (u64,u64) = tx.query_row(
        "SELECT COALESCE(SUM(bytes),0),COALESCE(SUM(files),0) FROM upload_usage", [],
        |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| REFUSED)?;
    let reset = now.saturating_sub(since) >= 60;
    let (since, requests, window_files, window_bytes) = if reset { (now,0,0,0) } else { (since,requests,window_files,window_bytes) };
    let bytes = bytes as u64;
    let files = files as u64;
    if used_bytes.saturating_add(bytes) > USER_BYTES || used_files.saturating_add(files) > USER_FILES
        || all_bytes.saturating_add(bytes) > GLOBAL_BYTES || all_files.saturating_add(files) > GLOBAL_FILES
        || requests >= 8 || window_files.saturating_add(files) > 32 || window_bytes.saturating_add(bytes) > 80 * 1024 * 1024 {
        return Err(REFUSED.into());
    }
    tx.execute("UPDATE upload_usage SET bytes=bytes+?2,files=files+?3,window_start=?4,
        window_requests=?5,window_files=?6,window_bytes=?7 WHERE user_id=?1",
        rusqlite::params![user,bytes,files,since,requests+1,window_files+files,window_bytes+bytes]).map_err(|_| REFUSED)?;
    tx.commit().map_err(|_| REFUSED.to_owned())
}

async fn bounded_work<T: Send + 'static>(slots: Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let permit = slots.try_acquire_owned().map_err(|_| REFUSED)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    }).await.map_err(|_| REFUSED.to_owned())?
}

/// Entire request is checked and durably charged before turn admission or file creation.
pub(super) async fn admit(home: &Path, user: &str, files: Vec<ChatAttachment>) -> Result<Vec<ChatAttachment>, String> {
    if files.is_empty() { return Ok(files); }
    let home = home.to_owned();
    let user = user.to_owned();
    bounded_work(WORK.clone(), move || {
        let prepared = validate(&files)?;
        let bytes = prepared.iter().map(|f| f.data.len()).sum();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| REFUSED)?.as_secs();
        reserve(&home, &user, files.len(), bytes, now)?;
        Ok(files)
    }).await
}

pub(super) async fn prepare(files: &[ChatAttachment]) -> Result<Vec<Prepared>, String> {
    let files = files.to_vec();
    bounded_work(WORK.clone(), move || validate(&files)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn webchat_upload_quota_refuses_symlink_database_and_creates_private_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = crate::fs_safe::private_test_home().unwrap();
        let home = root.path().canonicalize().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"untouched").unwrap();
        symlink(outside.path(), home.join("webchat_upload_quota.db")).unwrap();
        assert!(reserve(&home, "alice", 1, 1, 100).is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"untouched");
        std::fs::remove_file(home.join("webchat_upload_quota.db")).unwrap();
        reserve(&home, "alice", 1, 1, 100).unwrap();
        assert_eq!(std::fs::metadata(home.join("webchat_upload_quota.db")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn webchat_upload_filename_cannot_forge_a_reference() {
        let root = crate::fs_safe::private_test_home().unwrap();
        let home = root.path().canonicalize().unwrap();
        let mut malicious = file(b"data");
        malicious.filename = "x (file)](x) tail [a]{b}.txt".into();
        let reference = super::super::save_webchat_attachments(&home, &[malicious]).await.unwrap();
        let (text, references) = crate::chat_attachments::parse(&reference.join("\n"));
        assert_eq!(references.len(), 1);
        assert!(!text.contains(home.to_str().unwrap()));
        assert!(std::path::Path::new(&references[0].1).starts_with(home.join("attachments")));
    }

    #[cfg(not(unix))]
    #[test]
    fn webchat_upload_quota_fails_closed_without_creating_database() {
        let home = crate::fs_safe::private_test_home().unwrap();
        assert!(reserve(home.path(), "alice", 1, 1, 100).is_err());
        assert!(!home.path().join("webchat_upload_quota.db").exists());
    }

    fn file(data: &[u8]) -> ChatAttachment {
        ChatAttachment { filename: "report.txt".into(), mime: None,
            data_base64: base64::engine::general_purpose::STANDARD.encode(data) }
    }
    #[tokio::test]
    async fn webchat_upload_cancelled_work_holds_slot_until_blocking_work_finishes() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(bounded_work(slots.clone(), move || {
            let _ = started.send(());
            wait.recv().unwrap();
            Ok(())
        }));
        ready.await.unwrap();
        task.abort();
        let _ = task.await;
        let still_held = slots.clone().try_acquire_owned().is_err();
        release.send(()).unwrap();
        assert!(still_held);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(5), slots.acquire_owned()).await.unwrap().unwrap();
        drop(permit);
    }
    #[test]
    fn webchat_upload_bounded_parser_rejects_mass_empty_files() {
        let files = vec![serde_json::json!({"filename":"x","data_base64":""}); 20_000];
        let text = serde_json::json!({"type":"user_message","content":"hello","attachments":files}).to_string();
        assert!(parse_message(&text).is_err());
        let valid = serde_json::json!({"type":"user_message","content":"hi","attachments":vec![file(b"");MAX_FILES]}).to_string();
        assert!(parse_message(&valid).is_ok());
    }
    #[tokio::test]
    async fn webchat_upload_invalid_batch_leaves_no_partial_files() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let mut invalid = file(b"valid");
        invalid.data_base64 = "not base64!".into();
        for files in [vec![file(b"");20_000],vec![file(b"valid"),invalid],
            vec![file(&vec![1;MAX_BYTES/2+1]),file(&vec![2;MAX_BYTES/2+1])]] {
            assert!(super::super::save_webchat_attachments(home.path(), &files).await.is_err());
            assert!(!home.path().join("attachments").exists());
            assert!(admit(home.path(), "alice", files).await.is_err());
            assert!(!home.path().join("webchat_upload_quota.db").exists());
        }
    }
    #[test]
    fn webchat_upload_checks_name_and_encoded_lengths_before_decode() {
        let mut long_name = file(b"ok");
        long_name.filename = "界".repeat(86);
        assert!(validate(&[long_name]).is_err());
        let mut encoded = file(b"");
        encoded.data_base64 = "A".repeat(MAX_ENCODED + 1);
        assert!(validate(&[encoded]).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn webchat_upload_quota_survives_reopen_and_isolates_users() {
        let home = crate::fs_safe::private_test_home().unwrap();
        for _ in 0..8 { reserve(home.path(), "alice", 1, 0, 100).unwrap(); }
        assert!(reserve(home.path(), "alice", 1, 0, 100).is_err());
        reserve(home.path(), "bob", 1, 0, 100).unwrap();
        reserve(home.path(), "alice", 1, 0, 160).unwrap();
        let db = rusqlite::Connection::open(home.path().join("webchat_upload_quota.db")).unwrap();
        db.execute("UPDATE upload_usage SET files=?1 WHERE user_id='alice'", [USER_FILES]).unwrap();
        assert!(reserve(home.path(), "alice", 1, 0, 220).is_err());
        reserve(home.path(), "bob", 1, 0, 220).unwrap();
        db.execute("UPDATE upload_usage SET bytes=?1 WHERE user_id='bob'", [USER_BYTES]).unwrap();
        assert!(reserve(home.path(), "bob", 1, 1, 280).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn webchat_upload_concurrent_reservations_cannot_exceed_quota() {
        let home = crate::fs_safe::private_test_home().unwrap();
        reserve(home.path(), "alice", 1, 1, 100).unwrap();
        let db = rusqlite::Connection::open(home.path().join("webchat_upload_quota.db")).unwrap();
        db.execute("UPDATE upload_usage SET files=?1", [USER_FILES - 1]).unwrap();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| reserve(home.path(), "alice", 1, 1, 100));
            let second = scope.spawn(|| reserve(home.path(), "alice", 1, 1, 100));
            let allowed = [first.join().unwrap(), second.join().unwrap()].into_iter().filter(Result::is_ok).count();
            assert_eq!(allowed, 1);
        });
    }
    #[cfg(unix)]
    #[test]
    fn webchat_upload_window_caps_files_and_bytes_and_unreadable_quota_refuses() {
        let home = crate::fs_safe::private_test_home().unwrap();
        for _ in 0..4 { reserve(home.path(), "alice", 8, MAX_BYTES, 100).unwrap(); }
        assert!(reserve(home.path(), "alice", 1, 0, 100).is_err());
        for _ in 0..4 { reserve(home.path(), "bob", 1, MAX_BYTES, 100).unwrap(); }
        assert!(reserve(home.path(), "bob", 1, 1, 100).is_err());
        let broken = crate::fs_safe::private_test_home().unwrap();
        std::fs::create_dir(broken.path().join("webchat_upload_quota.db")).unwrap();
        assert!(reserve(broken.path(), "alice", 1, 1, 100).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn webchat_upload_global_quota_is_shared_across_principals() {
        let home = crate::fs_safe::private_test_home().unwrap();
        reserve(home.path(), "alice", 1, 1, 100).unwrap();
        let db = rusqlite::Connection::open(home.path().join("webchat_upload_quota.db")).unwrap();
        db.execute("UPDATE upload_usage SET bytes=?1", [GLOBAL_BYTES]).unwrap();
        assert!(reserve(home.path(), "bob", 1, 1, 100).is_err());
        db.execute("UPDATE upload_usage SET bytes=0,files=?1", [GLOBAL_FILES]).unwrap();
        assert!(reserve(home.path(), "bob", 1, 0, 100).is_err());
    }
}
