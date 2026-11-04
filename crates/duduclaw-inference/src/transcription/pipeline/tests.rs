use super::*;
use crate::transcription::{TranscriptionError, TranscriptionResult};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};

fn segment(start_ms: u64, end_ms: u64, speaker: Option<&str>, text: &str) -> Segment {
    Segment { start_ms, end_ms, speaker: speaker.map(str::to_owned), text: text.into() }
}

fn sample() -> Transcript {
    Transcript { language: "zh-TW".into(), segments: vec![segment(1000, 2000, Some("講者 1"), "會議開始。")] }
}

#[test]
fn transcription_offsets_merge_chunks_and_reject_invalid_times() {
    let mut target = vec![segment(0, 1000, Some("講者 1"), "第一段")];
    append_offset(&mut target, vec![segment(500, 1500, Some("講者 2"), "第二段")], CHUNK_SECONDS * 1000).unwrap();
    assert_eq!(target[1].start_ms, 1_800_500);
    assert_eq!(target[1].end_ms, 1_801_500);
    assert_eq!(target[1].speaker.as_deref(), Some("講者 2"));
    assert!(append_offset(&mut vec![], vec![segment(2, 1, None, "錯誤")], 0).is_err());
    assert!(append_offset(&mut vec![], vec![segment(1, 2, None, "溢位")], u64::MAX).is_err());
}

#[test]
fn transcription_ffmpeg_arguments_preserve_paths_without_shell() {
    let source = Path::new("/safe/錄音 $(touch sentinel); with space.mp4");
    let destination = Path::new("/safe/chunk-%03d.m4a");
    let args = ffmpeg_arguments(source, destination);
    let input = args.iter().position(|arg| arg == "-i").unwrap();
    assert_eq!(args[input + 1], source.as_os_str());
    assert_eq!(args.last().unwrap(), destination.as_os_str());
    for (flag, value) in [("-protocol_whitelist", "file,pipe"), ("-ac", "1"),
        ("-ar", "16000"), ("-segment_time", "1800"), ("-reset_timestamps", "1")] {
        let index = args.iter().position(|arg| arg == flag).unwrap();
        assert_eq!(args[index + 1], value);
    }
    assert!(args.iter().any(|arg| arg == "-nostdin"));
    assert!(args.iter().any(|arg| arg == "-vn"));
    assert!(!args.iter().any(|arg| arg == "-y"));
}

fn attachments() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let agent = root.join("alice");
    let other = root.join("bob");
    std::fs::create_dir_all(agent.join("attachments")).unwrap();
    std::fs::create_dir_all(other.join("attachments")).unwrap();
    std::fs::write(agent.join("attachments/meeting.m4a"), b"audio").unwrap();
    std::fs::write(other.join("attachments/meeting.m4a"), b"other audio").unwrap();
    (dir, agent, other)
}

#[test]
fn transcription_attachment_path_rejects_traversal_and_other_employee() {
    let (_dir, agent, other) = attachments();
    let accepted = validate_attachment(&agent, Path::new("meeting.m4a"), 100).unwrap();
    assert_eq!(accepted, agent.join("attachments/meeting.m4a"));
    assert_eq!(validate_attachment(&agent, &accepted, 100).unwrap(), accepted);
    for path in [PathBuf::from("../attachments/meeting.m4a"),
        agent.join("attachments/../attachments/meeting.m4a"), other.join("attachments/meeting.m4a"),
        PathBuf::from("missing.m4a"), agent.clone(), PathBuf::from(".")] {
        assert!(validate_attachment(&agent, &path, 100).is_err(), "{}", path.display());
    }
    assert!(validate_attachment(&agent, Path::new("meeting.m4a"), 1).is_err());
    std::fs::write(agent.join("attachments/empty.m4a"), b"").unwrap();
    assert!(validate_attachment(&agent, Path::new("empty.m4a"), 100).is_err());
}

#[test]
#[cfg(unix)]
fn transcription_attachment_rejects_symlinks_hardlinks_and_fifo() {
    use std::os::unix::{fs::symlink, ffi::OsStrExt};
    let (_dir, agent, other) = attachments();
    let root = agent.join("attachments");
    symlink(other.join("attachments/meeting.m4a"), root.join("linked.m4a")).unwrap();
    symlink(other.join("attachments"), root.join("nested")).unwrap();
    std::fs::hard_link(other.join("attachments/meeting.m4a"), root.join("hard.m4a")).unwrap();
    let fifo = root.join("pipe.m4a");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for path in ["linked.m4a", "nested/meeting.m4a", "hard.m4a", "pipe.m4a"] {
        assert!(validate_attachment(&agent, Path::new(path), 100).is_err(), "{path}");
    }
    std::fs::rename(&root, agent.join("real-attachments")).unwrap();
    symlink(agent.join("real-attachments"), &root).unwrap();
    assert!(validate_attachment(&agent, Path::new("meeting.m4a"), 100).is_err());
}

#[test]
fn transcription_s2twp_uses_taiwan_phrases() {
    let mut transcript = Transcript { language: "zh".into(), segments: vec![
        segment(0, 1000, Some("讲者 1"), "软件 信息 视频"),
    ]};
    convert_taiwan(&mut transcript).unwrap();
    assert_eq!(transcript.segments[0].text, "軟體 資訊 影片");
    assert_eq!(transcript.segments[0].speaker.as_deref(), Some("講者 1"));
    assert_eq!(transcript.language, "zh");
}

#[test]
fn transcription_markdown_has_metadata_and_normalized_lines() {
    let transcript = Transcript { language: "zh-TW".into(), segments: vec![
        segment(3_661_999, 3_662_000, Some("講者 1"), "第一行\n第二行"),
        segment(3_663_000, 3_664_000, None, "未知講者"),
    ]};
    let md = markdown("例會\n錄音", "2026-11-03", "gemini", 3_700_000, &transcript);
    assert!(md.starts_with("# 例會 錄音\n\n"));
    assert!(md.contains("日期：2026-11-03"));
    assert!(md.contains("轉錄引擎：gemini"));
    assert!(md.contains("錄音長度：01:01:40"));
    assert!(md.contains("[01:01:01] 講者 1：第一行 第二行"));
    assert!(md.contains("[01:01:03] 講者未辨識：未知講者"));
    assert!(markdown("a", "d", "e", 0, &transcript).contains("錄音長度：未取得"));
}

#[test]
fn transcription_outputs_round_trip_and_do_not_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let transcript = sample();
    let first = save_outputs(&root, "例會", "whisper", 2000, &transcript).unwrap();
    let second = save_outputs(&root, "例會", "whisper", 2000, &transcript).unwrap();
    assert_ne!(first.json, second.json);
    let decoded: Transcript = serde_json::from_slice(&std::fs::read(&first.json).unwrap()).unwrap();
    assert_eq!(decoded, transcript);
    assert!(std::fs::read_to_string(&first.markdown).unwrap().contains("[00:00:01] 講者 1：會議開始。"));
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(first.json).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(first.markdown).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[derive(Default)]
struct UsageLog(Mutex<Vec<TranscriptionUsage>>);
impl UsageLog {
    fn models(&self) -> Vec<String> { self.0.lock().unwrap().iter().map(|u| u.model.clone()).collect() }
}
#[async_trait]
impl UsageObserver for UsageLog {
    async fn record(&self, usage: &TranscriptionUsage) -> Result<(), String> {
        self.0.lock().unwrap().push(usage.clone());
        Ok(())
    }
}

#[derive(Default)]
struct MockEngine {
    results: Mutex<VecDeque<Result<TranscriptionResult, TranscriptionError>>>,
    contexts: Mutex<Vec<Vec<String>>>,
    active: AtomicUsize,
    maximum_active: AtomicUsize,
}

#[async_trait]
impl TranscriptionEngine for MockEngine {
    async fn transcribe(&self, _path: &Path, hint: &str, speakers: &[String]) -> Result<TranscriptionResult, TranscriptionError> {
        assert_eq!(hint, "zh-TW");
        self.contexts.lock().unwrap().push(speakers.to_vec());
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        tokio::task::yield_now().await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.results.lock().unwrap().pop_front().unwrap()
    }
}

fn prepared(count: usize) -> PreparedAudio {
    let temporary = tempfile::tempdir().unwrap();
    PreparedAudio {
        chunks: (0..count).map(|i| AudioChunk { path: temporary.path().join(format!("chunk-{i}.m4a")), offset_ms: i as u64 * CHUNK_SECONDS * 1000 }).collect(),
        duration_ms: count as u64 * CHUNK_SECONDS * 1000,
        _temporary: temporary,
    }
}

fn success(model: &str) -> Result<TranscriptionResult, TranscriptionError> {
    Ok(TranscriptionResult { transcript: sample(), usage: vec![TranscriptionUsage {
        model: model.into(), input_tokens: 100, output_tokens: 20, ..Default::default()
    }] })
}

#[tokio::test]
async fn transcription_chunks_are_sequential_offset_and_reuse_speaker_labels() {
    let engine = MockEngine { results: Mutex::new(VecDeque::from([success("first"), success("second")])), ..Default::default() };
    let usage = UsageLog::default();
    let transcript = transcribe_chunks(&engine, &prepared(2), "zh-TW", &usage).await.unwrap();
    assert_eq!(transcript.segments.len(), 2);
    assert_eq!(transcript.segments[1].start_ms, 1_801_000);
    assert_eq!(engine.maximum_active.load(Ordering::SeqCst), 1);
    assert_eq!(*engine.contexts.lock().unwrap(), vec![Vec::<String>::new(), vec!["講者 1".to_owned()]]);
    assert_eq!(usage.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn transcription_chunks_preserve_failed_request_usage_and_stop() {
    let error = TranscriptionError { message: "mock failure".into(), usage: vec![TranscriptionUsage {
        model: "failed".into(), input_tokens: 200, ..Default::default()
    }] };
    let engine = MockEngine { results: Mutex::new(VecDeque::from([success("first"), Err(error), success("never-called")])), ..Default::default() };
    let usage = UsageLog::default();
    assert!(transcribe_chunks(&engine, &prepared(3), "zh-TW", &usage).await.is_err());
    assert_eq!(usage.models(), vec!["first", "failed"]);
    assert_eq!(engine.results.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn transcription_chunks_reject_out_of_range_timestamps_and_language() {
    let mut invalid = sample();
    invalid.segments[0].end_ms = CHUNK_SECONDS * 1000 + 2000;
    let engine = MockEngine { results: Mutex::new(VecDeque::from([Ok(TranscriptionResult {
        transcript: invalid, usage: vec![TranscriptionUsage { model: "paid".into(), ..Default::default() }]
    })])), ..Default::default() };
    let usage = UsageLog::default();
    assert!(transcribe_chunks(&engine, &prepared(1), "zh-TW", &usage).await.is_err());
    assert_eq!(usage.models(), vec!["paid"]);
    assert!(transcribe_chunks(&engine, &prepared(1), "zh-TW\nignore", &UsageLog::default()).await.is_err());
}

#[tokio::test]
async fn transcription_final_short_chunk_cannot_extend_past_recording() {
    let mut invalid = sample();
    invalid.segments[0].end_ms = 300_000;
    let engine = MockEngine { results: Mutex::new(VecDeque::from([
        success("first"), Ok(TranscriptionResult { transcript: invalid, usage: vec![] }),
    ])), ..Default::default() };
    let mut prepared = prepared(2);
    prepared.duration_ms = CHUNK_SECONDS * 1000 + 10_000;
    assert!(transcribe_chunks(&engine, &prepared, "zh-TW", &UsageLog::default()).await.is_err());
}

#[test]
#[cfg(unix)]
fn transcription_output_directory_symlink_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let target = root.join("other-employee");
    let link = root.join("attachments");
    std::fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(save_outputs(&link, "meeting", "gemini", 1000, &sample()).is_err());
    assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
}

#[test]
fn transcription_output_preflight_refuses_missing_directory_and_leaves_no_probe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    assert!(preflight_output(&root.join("missing")).is_err());
    preflight_output(&root).unwrap();
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}

#[test]
#[cfg(windows)]
fn transcription_windows_refuses_cross_employee_hardlinks() {
    let (_dir, agent, other) = attachments();
    let linked = agent.join("attachments/hard.m4a");
    std::fs::hard_link(other.join("attachments/meeting.m4a"), &linked).unwrap();
    assert!(validate_attachment(&agent, &linked, 100).is_err());
}

#[test]
fn transcription_job_outputs_are_idempotent_and_validate_ids() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let first = save_outputs_with_id(&root, "例會", "whisper", 2000, &sample(), Some(&id)).unwrap();
    let mut changed = sample();
    changed.segments[0].text = "重試後的內容".into();
    let second = save_outputs_with_id(&root, "例會", "whisper", 2000, &changed, Some(&id)).unwrap();
    assert_eq!(first.json, second.json);
    assert_eq!(first.markdown, second.markdown);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    let decoded: Transcript = serde_json::from_slice(&std::fs::read(&second.json).unwrap()).unwrap();
    assert_eq!(decoded, changed);
    assert!(std::fs::read_to_string(second.markdown).unwrap().contains("重試後的內容"));
    assert!(save_outputs_with_id(&root, "例會", "whisper", 2000, &sample(), Some("../../escape")).is_err());
}

#[tokio::test]
async fn transcription_cancel_fence_stops_before_next_provider_call() {
    struct CancelAfterFirst { progress: Mutex<Vec<(usize, usize)>> }
    #[async_trait]
    impl UsageObserver for CancelAfterFirst {
        async fn record(&self, _: &TranscriptionUsage) -> Result<(), String> { Ok(()) }
        async fn before_chunk(&self, index: usize, total: usize) -> Result<(), String> {
            self.progress.lock().unwrap().push((index, total));
            if index > 1 { return Err("已取消".into()); }
            Ok(())
        }
    }
    let engine = MockEngine { results: Mutex::new(VecDeque::from([success("first"), success("never-called")])), ..Default::default() };
    let control = CancelAfterFirst { progress: Mutex::new(vec![]) };
    let error = transcribe_chunks(&engine, &prepared(2), "zh-TW", &control).await.unwrap_err();
    assert_eq!(error, "已取消");
    assert_eq!(*control.progress.lock().unwrap(), vec![(1, 2), (2, 2)]);
    assert_eq!(engine.results.lock().unwrap().len(), 1);
}

#[test]
#[cfg(unix)]
fn transcription_idempotent_output_replaces_symlink_without_writing_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let sentinel = root.join("private.json");
    std::fs::write(&sentinel, "unchanged").unwrap();
    std::os::unix::fs::symlink(&sentinel, root.join(format!("transcript-{id}.json"))).unwrap();
    let paths = save_outputs_with_id(&root, "例會", "whisper", 2000, &sample(), Some(&id)).unwrap();
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "unchanged");
    assert!(!std::fs::symlink_metadata(paths.json).unwrap().file_type().is_symlink());
}

#[test]
fn transcription_attachment_read_is_bounded_and_owner_scoped() {
    let (_dir, agent, other) = attachments();
    assert_eq!(read_attachment(&agent, Path::new("meeting.m4a"), 5).unwrap(), b"audio");
    assert!(read_attachment(&agent, Path::new("meeting.m4a"), 4).is_err());
    assert!(read_attachment(&agent, &other.join("attachments/meeting.m4a"), 100).is_err());
}

#[test]
fn transcription_cumulative_limits_refuse_before_mutating_the_result() {
    let mut target = vec![segment(0, 1, None, "a"); 100_000];
    assert!(append_offset(&mut target, vec![segment(0, 1, None, "a"); 100_001], 0).is_err());
    assert_eq!(target.len(), 100_000);
    let mut target = vec![segment(0, 1, None, &"a".repeat(MAX_TEXT_BYTES / 2))];
    let extra = vec![segment(0, 1, Some("講者"), &"b".repeat(MAX_TEXT_BYTES / 2))];
    assert!(append_offset(&mut target, extra, 0).is_err());
    assert_eq!(target.len(), 1);
    let oversized_output = vec![segment(0, 1, None, &"a".repeat(220)); 150_000];
    assert!(transcript_budget(&oversized_output).is_err());
}

#[tokio::test]
async fn transcription_cumulative_segment_cap_stops_further_provider_calls() {
    let results = (0..8).map(|_| Ok(TranscriptionResult {
        transcript: Transcript { language: "zh-TW".into(), segments: vec![segment(0, 1, None, "a"); 30_000] },
        usage: vec![],
    })).collect();
    let engine = MockEngine { results: Mutex::new(results), ..Default::default() };
    let error = transcribe_chunks(&engine, &prepared(8), "zh-TW", &UsageLog::default()).await.unwrap_err();
    assert_eq!(error, OUTPUT_LIMIT_ERROR);
    assert_eq!(engine.results.lock().unwrap().len(), 1);
}

#[test]
fn transcription_output_byte_count_matches_both_actual_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let paths = save_outputs(&root, "會議", "gemini", 2000, &sample()).unwrap();
    let actual = std::fs::metadata(&paths.markdown).unwrap().len() + std::fs::metadata(&paths.json).unwrap().len();
    assert_eq!(paths.bytes, actual);
}

#[test]
#[cfg(unix)]
fn transcription_media_limits_are_applied_in_the_child_only() {
    fn parent_limit(resource: u32) -> (u64, u64) {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        assert_eq!(unsafe { libc::getrlimit(resource as _, &mut limit) }, 0);
        (limit.rlim_cur as u64, limit.rlim_max as u64)
    }
    let before = parent_limit(libc::RLIMIT_NOFILE as u32);
    let mut command = tokio::process::Command::new("sh");
    #[cfg(target_os = "macos")]
    let check = "test \"$(ulimit -n)\" = 256 && test \"$(ulimit -t)\" = 30";
    #[cfg(not(target_os = "macos"))]
    let check = "test \"$(ulimit -n)\" = 256 && test \"$(ulimit -t)\" = 30 && test \"$(ulimit -v)\" = 2097152";
    command.args(["-c", check]);
    restrict_media_process(&mut command, 30);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    assert!(runtime.block_on(async { command.status().await }).unwrap().success());
    assert_eq!(parent_limit(libc::RLIMIT_NOFILE as u32), before);
}

#[tokio::test]
async fn transcription_underreported_usage_is_bounded_before_accounting_success_and_failure() {
    let whisper = TranscriptionUsage { model: "openai/whisper-1".into(), audio_seconds: Some(0.001), ..Default::default() };
    let gemini = TranscriptionUsage { model: "gemini/gemini-3.5-flash".into(), input_tokens: 1, input_audio_tokens: 1, ..Default::default() };
    let engine = MockEngine { results: Mutex::new(VecDeque::from([
        Ok(TranscriptionResult { transcript: sample(), usage: vec![whisper] }),
        Err(TranscriptionError { message: "refused".into(), usage: vec![gemini] }),
    ])), ..Default::default() };
    let usage = UsageLog::default();
    let mut audio = prepared(2);
    audio.duration_ms = CHUNK_SECONDS * 1000 + 600_000;
    assert!(transcribe_chunks(&engine, &audio, "zh-TW", &usage).await.is_err());
    let recorded = usage.0.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].audio_seconds, Some(CHUNK_SECONDS as f64));
    assert_eq!(recorded[1].input_audio_tokens, 19_200);
    assert_eq!(recorded[1].input_tokens, 23_296);
    assert!(recorded.iter().all(|item| item.estimated));
    let registry = duduclaw_llm::ModelRegistry::vendored();
    let model = registry.get(&recorded[1].model).unwrap();
    assert!(registry.cost_audio_millicents(&duduclaw_llm::NormalizedUsage {
        input_tokens: recorded[1].input_tokens, output_tokens: recorded[1].output_tokens,
        ..Default::default()
    }, recorded[1].input_audio_tokens, model) > 0);
}


#[tokio::test]
#[cfg(unix)]
async fn transcription_rss_watchdog_kills_group_above_configured_limit() {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "sleep 30 & wait"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut reads = 0;
    let start = std::time::Instant::now();
    let error = media_output_with_rss(&mut command, Duration::from_secs(5), 4096, |pid| {
        assert_eq!(unsafe { libc::getpgid(pid as libc::pid_t) }, pid as libc::pid_t);
        reads += 1;
        Ok(if reads == 1 { 4096 } else { 4097 })
    }).await.unwrap_err();
    assert!(matches!(error, MediaProcessError::MemoryLimit));
    assert_eq!(error.message("fallback"), MEDIA_MEMORY_ERROR);
    assert_eq!(reads, 2, "RSS is checked again after the 500 ms polling interval");
    assert!(start.elapsed() >= Duration::from_millis(500));
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
#[cfg(unix)]
async fn transcription_rss_watchdog_allows_process_below_limit() {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "sleep 0.6"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let output = media_output_with_rss(&mut command, Duration::from_secs(5), 4096, |_| Ok(4096)).await.unwrap();
    assert!(output.status.success());
}

#[tokio::test]
#[cfg(unix)]
async fn transcription_rss_watchdog_fails_closed_when_inspection_is_refused() {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "sleep 30"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let error = media_output_with_rss(&mut command, Duration::from_secs(5), 4096, |_| {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }).await.unwrap_err();
    assert!(matches!(error, MediaProcessError::MemoryUnavailable));
    assert_eq!(error.message("fallback"), MEDIA_MEMORY_CHECK_ERROR);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn transcription_rss_reader_measures_resident_memory() {
    assert!(media_rss(std::process::id()).unwrap() > 0);
}

#[test]
fn transcription_unsegmented_audio_requires_known_bounded_local_duration() {
    assert!(unsegmented_duration(None).unwrap_err().contains("ffprobe"));
    assert!(unsegmented_duration(Some(0)).is_err());
    assert!(unsegmented_duration(Some(CHUNK_SECONDS * 1000 + 1)).unwrap_err().contains("ffmpeg"));
    assert_eq!(unsegmented_duration(Some(CHUNK_SECONDS * 1000)).unwrap(), CHUNK_SECONDS * 1000);
    assert_eq!(unsegmented_duration(Some(1500)).unwrap(), 1500);
}
