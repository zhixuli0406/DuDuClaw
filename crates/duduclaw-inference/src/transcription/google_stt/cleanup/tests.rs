use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn mock(statuses: Vec<u16>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for status in statuses {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0u8; 1024];
                let size = stream.read(&mut bytes).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&bytes[..size]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") { break; }
            }
            requests.push(String::from_utf8(request).unwrap());
            stream.write_all(format!("HTTP/1.1 {status} Mock\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        }
        requests
    });
    (base, task)
}

fn object() -> String { format!("duduclaw-meetings/{}.m4a", uuid::Uuid::new_v4()) }
fn engine(home: &Path, base: String) -> GoogleSttEngine {
    let mut engine = GoogleSttEngine::for_test(base, super::super::GoogleSttConfig { bucket: "meeting-audio".into(), ..Default::default() });
    engine.cleanup_home = home.into();
    engine
}

#[test]
fn transcription_google_cleanup_intent_is_durable_and_contains_no_credential() {
    let home = tempfile::tempdir().unwrap();
    let pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    let raw = fs::read_to_string(&pending.path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["bucket"], "meeting-audio");
    assert_eq!(value["state"], "pending");
    assert!(value["generation"].is_null());
    assert_eq!(value.as_object().unwrap().len(), 6);
    assert!(lock_record(&pending.path).unwrap().is_none());
    let path = pending.path.clone();
    drop(pending); // A crashed/cancelled task releases its lock, not its intent.
    assert!(lock_record(&path).unwrap().is_some());
    assert!(GoogleSttEngine::has_pending_cleanup(home.path()).unwrap());
}

#[test]
fn transcription_google_cleanup_missing_home_and_journal_are_empty() {
    let home = tempfile::tempdir().unwrap();
    assert!(!GoogleSttEngine::has_pending_cleanup(home.path()).unwrap());
    assert!(!GoogleSttEngine::has_pending_cleanup(&home.path().join("missing")).unwrap());
}

#[tokio::test]
async fn transcription_google_cleanup_restarts_retries_and_deletes_exact_generation() {
    let home = tempfile::tempdir().unwrap();
    let mut pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    pending.set_generation(Some("123456789")).unwrap();
    drop(pending);
    let (base, task) = mock(vec![503, 429, 204]).await;
    // Recovery is bound to the bucket in the current trusted config.
    assert_eq!(engine(home.path(), base).reconcile_cleanup().await.unwrap(), 0);
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests {
        assert!(request.starts_with("DELETE /storage/v1/b/meeting-audio/"));
        assert!(request.contains("generation=123456789"));
    }
    assert!(!GoogleSttEngine::has_pending_cleanup(home.path()).unwrap());
}

#[tokio::test]
async fn transcription_google_cleanup_preserves_failure_for_next_reconcile() {
    let home = tempfile::tempdir().unwrap();
    let mut pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    pending.set_generation(Some("123")).unwrap_or_else(|e| panic!("persist cleanup generation: {e:?}"));
    let path = pending.path.clone();
    drop(pending);
    let (base, task) = mock(vec![503, 503, 503]).await;
    assert_eq!(engine(home.path(), base).reconcile_cleanup().await.unwrap(), 1);
    assert_eq!(task.await.unwrap().len(), 3);
    let record: Record = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(record.attempts, 3);
    let (base, task) = mock(vec![404]).await;
    assert_eq!(engine(home.path(), base).reconcile_cleanup().await.unwrap(), 0);
    assert_eq!(task.await.unwrap().len(), 1);
}

#[tokio::test]
async fn transcription_google_cleanup_does_not_touch_active_uploads() {
    let home = tempfile::tempdir().unwrap();
    let mut pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    pending.set_generation(Some("123")).unwrap_or_else(|e| panic!("persist cleanup generation: {e:?}"));
    assert_eq!(engine(home.path(), "http://127.0.0.1:1".into()).reconcile_cleanup().await.unwrap(), 1);
    assert_eq!(pending.record.attempts, 0);
    drop(pending);
}

#[tokio::test]
async fn transcription_google_cleanup_unknown_generation_survives_early_404() {
    let home = tempfile::tempdir().unwrap();
    let mut pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    let (base, task) = mock(vec![404]).await;
    assert!(engine(home.path(), base).cleanup_object(&mut pending, "test-token").await.unwrap());
    assert!(pending.path.exists());
    task.await.unwrap();
    let path = pending.path.clone();
    drop(pending);
    // Startup cannot accept an early 404 while the upload may still be landing.
    assert_eq!(engine(home.path(), "http://127.0.0.1:1".into()).reconcile_cleanup().await.unwrap(), 1);
    let mut record: Record = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record.settle_after = 0;
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let (base, task) = mock(vec![404]).await;
    assert_eq!(engine(home.path(), base).reconcile_cleanup().await.unwrap(), 0);
    let requests = task.await.unwrap();
    assert!(!requests[0].contains("generation="));
}

#[tokio::test]
async fn transcription_google_cleanup_corrupt_state_is_retained_and_reported() {
    let home = tempfile::tempdir().unwrap();
    let pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    let path = pending.path.clone();
    drop(pending);
    fs::write(&path, b"malformed").unwrap();
    assert!(engine(home.path(), "http://127.0.0.1:1".into()).reconcile_cleanup().await.is_err());
    assert!(path.exists());
    assert!(GoogleSttEngine::has_pending_cleanup(home.path()).unwrap());
    let invalid_home = home.path().join("not-a-directory");
    fs::write(&invalid_home, b"x").unwrap();
    assert!(GoogleSttEngine::has_pending_cleanup(&invalid_home).is_err());
}

#[test]
fn transcription_google_cleanup_refuses_upload_if_journal_cannot_be_created() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join(DIRECTORY), b"not-a-directory").unwrap();
    assert!(PendingObject::create(home.path(), "meeting-audio", &object()).is_err());
}

#[tokio::test]
async fn transcription_google_cleanup_journal_precedes_actual_upload_request() {
    use crate::transcription::TranscriptionEngine;
    let home = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state_home = home.path().to_path_buf();
    let task = tokio::spawn(async move {
        for status in [403, 204] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0u8; 8192];
                let size = stream.read(&mut bytes).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&bytes[..size]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers.lines().filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap()).unwrap_or(0);
                    if request.len() >= end + 4 + length { break; }
                }
            }
            let records = paths(&state_home).unwrap();
            assert_eq!(records.len(), 1);
            let record: Record = serde_json::from_slice(&fs::read(&records[0]).unwrap()).unwrap();
            assert_eq!(record.bucket, "meeting-audio");
            assert!(record.generation.is_none());
            assert!(lock_record(&records[0]).unwrap().is_none());
            stream.write_all(format!("HTTP/1.1 {status} Mock\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes()).await.unwrap();
        }
    });
    let mut engine = engine(home.path(), base);
    engine.config.bucket = "meeting-audio".into();
    let audio = home.path().join("audio.m4a");
    fs::write(&audio, b"audio").unwrap();
    let error = engine.transcribe(&audio, "zh-TW", &[]).await.unwrap_err();
    assert!(error.detail.is_none(), "upload stopped at cleanup journal: {error:?}");
    tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
    assert!(GoogleSttEngine::has_pending_cleanup(home.path()).unwrap());
}

#[tokio::test]
async fn transcription_google_cleanup_corrupt_filename_does_not_block_valid_records() {
    let home = tempfile::tempdir().unwrap();
    let mut pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    pending.set_generation(Some("123")).unwrap_or_else(|e| panic!("persist cleanup generation: {e:?}"));
    let path = pending.path.clone();
    drop(pending);
    let corrupt = home.path().join(DIRECTORY).join("000-invalid.json");
    fs::write(&corrupt, b"{}").unwrap();
    let (base, task) = mock(vec![204]).await;
    assert!(engine(home.path(), base).reconcile_cleanup().await.is_err());
    assert_eq!(task.await.unwrap().len(), 1);
    assert!(!path.exists());
    assert!(corrupt.exists());
}

#[tokio::test]
async fn transcription_google_cleanup_bucket_mismatch_keeps_record_without_network() {
    for configured in ["meeting-audio", ""] {
        let home = tempfile::tempdir().unwrap();
        let mut pending = PendingObject::create(home.path(), "other-bucket", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
        pending.set_generation(Some("123")).unwrap_or_else(|e| panic!("persist cleanup generation: {e:?}"));
        let path = pending.path.clone();
        let before = fs::read(&path).unwrap();
        let mut engine = engine(home.path(), "http://127.0.0.1:1".into());
        engine.config.bucket = configured.into();
        // Pin the direct deletion path too, before journaling an attempt.
        assert!(engine.cleanup_object(&mut pending, "test-token").await.is_err());
        drop(pending);
        // No test token: accidental authentication would fail before any DELETE.
        engine.test_token = None;
        assert_eq!(engine.reconcile_cleanup().await.unwrap(), 1);
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[cfg(unix)]
#[test]
fn transcription_google_cleanup_private_permissions_and_legacy_modes() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let mut pending = PendingObject::create(home.path(), "meeting-audio", &object()).unwrap_or_else(|e| panic!("create cleanup journal: {e:?}"));
    let dir = home.path().join(DIRECTORY);
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        drop(private_file(&path, false).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    pending.set_generation(Some("123")).unwrap_or_else(|e| panic!("persist cleanup generation: {e:?}"));
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(&pending.path).unwrap().permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[tokio::test]
async fn transcription_google_cleanup_rejects_links_and_special_files() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let dir = home.path().join(DIRECTORY);
    symlink(outside.path(), &dir).unwrap();
    assert!(PendingObject::create(home.path(), "meeting-audio", &object()).is_err());
    assert!(GoogleSttEngine::has_pending_cleanup(home.path()).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    fs::remove_file(&dir).unwrap();
    private_dir(&dir, true).unwrap();
    let target = outside.path().join("canary");
    fs::write(&target, b"unchanged").unwrap();
    let path = dir.join("00000000-0000-0000-0000-000000000000.json");
    let lock_path = dir.join("00.lock");
    symlink(&target, &lock_path).unwrap();
    assert!(lock_record(&path).is_err());
    fs::remove_file(&lock_path).unwrap();
    fs::create_dir(&lock_path).unwrap();
    assert!(lock_record(&path).is_err());
    fs::remove_dir(&lock_path).unwrap();
    symlink(&target, &path).unwrap();
    assert!(private_file(&path, false).is_err());
    assert!(engine(home.path(), "http://127.0.0.1:1".into()).reconcile_cleanup().await.is_err());
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
    fs::remove_file(&path).unwrap();
    fs::hard_link(&target, &path).unwrap();
    assert!(private_file(&path, false).is_err());
    fs::remove_file(&path).unwrap();
    let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(private_file(&path, false).is_err());
    assert!(engine(home.path(), "http://127.0.0.1:1".into()).reconcile_cleanup().await.is_err());
    assert_eq!(fs::read(&target).unwrap(), b"unchanged");
}

#[test]
fn transcription_google_cleanup_io_diagnostics_keep_public_message_sanitized() {
    let error = io_error("open_journal_dir", io::Error::from_raw_os_error(5));
    assert_eq!(error.to_string(), "無法讀寫雲端錄音清理紀錄；請檢查資料目錄。");
    let diagnostic = format!("{error:?}");
    assert!(diagnostic.contains("step=open_journal_dir"));
    assert!(diagnostic.contains("os_error=Some(5)"));
    assert!(!error.message.contains("open_journal_dir"));
}
