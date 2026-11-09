//! Local media preparation and deterministic, provider-independent output.
use std::{collections::BTreeSet, ffi::OsString, fs::{File, OpenOptions}, io::{Read, Write}, path::{Component, Path, PathBuf}, process::Stdio, time::Duration};
use super::{bounded_usage, Segment, Transcript, TranscriptionEngine, TranscriptionUsage};

mod snapshot;
pub use snapshot::{snapshot_attachment, verify_snapshot};

pub const CHUNK_SECONDS: u64 = 30 * 60;
const MAX_CHUNKS: usize = 96;
const MAX_SEGMENTS: usize = 200_000;
const MAX_TEXT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
const OUTPUT_LIMIT_ERROR: &str = "逐字稿內容超過安全上限，已停止後續轉錄；請將會議拆成較短錄音。";

/// Covers pretty JSON escaping, Markdown and per-segment timestamp/key overhead.
fn transcript_budget(segments: &[Segment]) -> Result<(usize, usize), String> {
    if segments.len() > MAX_SEGMENTS { return Err(OUTPUT_LIMIT_ERROR.into()); }
    let mut text = 0usize;
    for segment in segments {
        text = text.checked_add(segment.text.len()).and_then(|n| n.checked_add(segment.speaker.as_ref().map_or(0, String::len)))
            .ok_or(OUTPUT_LIMIT_ERROR)?;
    }
    let output = text.checked_mul(7).and_then(|n| segments.len().checked_mul(256).and_then(|overhead| n.checked_add(overhead)))
        .and_then(|n| n.checked_add(4096)).ok_or(OUTPUT_LIMIT_ERROR)?;
    if text > MAX_TEXT_BYTES || output > MAX_OUTPUT_BYTES { return Err(OUTPUT_LIMIT_ERROR.into()); }
    Ok((text, output))
}

/// Only async-signal-safe libc calls run after fork, before exec.
/// Darwin can reject both AS and DATA. The RSS watchdog below remains active
/// even when an address-space limit was installed, including on Linux.
#[cfg(test)]
fn restrict_media_process(command: &mut tokio::process::Command, cpu_seconds: u64) {
    restrict_media_process_with_memory_limit(command, cpu_seconds, DEFAULT_MEDIA_MEMORY_LIMIT_BYTES);
}

pub const DEFAULT_MEDIA_MEMORY_LIMIT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MEDIA_MEMORY_ERROR: &str = "音訊處理超過記憶體上限，已停止工作；請將錄音拆成較小檔案。";
const MEDIA_MEMORY_CHECK_ERROR: &str = "無法確認音訊處理的記憶體用量，已停止工作。";

fn restrict_media_process_with_memory_limit(command: &mut tokio::process::Command, cpu_seconds: u64, memory_bytes: u64) {
    #[cfg(unix)] unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "macos")]
            let memory_resources = [libc::RLIMIT_AS, libc::RLIMIT_DATA];
            #[cfg(not(target_os = "macos"))]
            let memory_resources = [libc::RLIMIT_AS];
            for resource in memory_resources {
                let mut current = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
                if libc::getrlimit(resource, &mut current) != 0 { return Err(std::io::Error::last_os_error()); }
                let limit = (memory_bytes as libc::rlim_t).min(current.rlim_max).min(current.rlim_cur);
                let limits = libc::rlimit { rlim_cur: limit, rlim_max: limit };
                if libc::setrlimit(resource, &limits) == 0 { break; }
                let error = std::io::Error::last_os_error();
                #[cfg(target_os = "macos")]
                if error.raw_os_error() == Some(libc::EINVAL) { continue; }
                return Err(error);
            }
            for (resource, requested) in [(libc::RLIMIT_CPU, cpu_seconds), (libc::RLIMIT_NOFILE, 256)] {
                let mut current = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
                if libc::getrlimit(resource, &mut current) != 0 { return Err(std::io::Error::last_os_error()); }
                let limit = (requested as libc::rlim_t).min(current.rlim_max).min(current.rlim_cur);
                let limits = libc::rlimit { rlim_cur: limit, rlim_max: limit };
                if libc::setrlimit(resource, &limits) != 0 { return Err(std::io::Error::last_os_error()); }
            }
            Ok(())
        });
    }
    #[cfg(not(unix))] let _ = (command, cpu_seconds, memory_bytes);
}
#[derive(Debug)]
enum MediaProcessError {
    Io(std::io::Error),
    TimedOut,
    MemoryLimit,
    MemoryUnavailable,
}
impl MediaProcessError {
    fn message(self, fallback: &str) -> String {
        match self {
            Self::MemoryLimit => MEDIA_MEMORY_ERROR.into(),
            Self::MemoryUnavailable => MEDIA_MEMORY_CHECK_ERROR.into(),
            _ => fallback.into(),
        }
    }
}

#[cfg(unix)]
struct MediaProcessGroup(Option<duduclaw_core::execution_gate::RunControl>);
#[cfg(unix)]
impl MediaProcessGroup {
    fn kill(&self) {
        if let Some(control) = &self.0 {
            // The backend owns process-tree authority; a diagnostic PID can
            // already have been reaped while buffered output is still draining.
            let _ = control.start_kill();
        }
    }
}
#[cfg(unix)]
impl Drop for MediaProcessGroup {
    fn drop(&mut self) { self.kill(); }
}

#[cfg(target_os = "macos")]
fn media_rss(pid: u32) -> std::io::Result<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most the supplied buffer size. The whole
    // structure is initialized only when the complete expected size is returned.
    let actual = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
    if actual != size { return Err(std::io::Error::last_os_error()); }
    Ok(unsafe { info.assume_init() }.pti_resident_size)
}
#[cfg(target_os = "linux")]
fn media_rss(pid: u32) -> std::io::Result<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    let kilobytes = status.lines().find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next()).and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing process RSS"))?;
    kilobytes.checked_mul(1024).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "process RSS overflow"))
}
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn media_rss(_: u32) -> std::io::Result<u64> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "process RSS unavailable"))
}

#[cfg(unix)]
async fn media_output_with_rss(
    command: &mut tokio::process::Command, timeout: Duration, memory_bytes: u64,
    mut read_rss: impl FnMut(u32) -> std::io::Result<u64>,
) -> Result<std::process::Output, MediaProcessError> {
    command.process_group(0).kill_on_drop(true);
    let child = duduclaw_core::execution_gate::with_memory_limit(memory_bytes,
        duduclaw_core::execution_gate::spawn(command, duduclaw_core::execution_gate::ExecutionClass::ParseWorker)).await.map_err(MediaProcessError::Io)?;
    // Protected worker resource limits are enforced by its trusted policy;
    // remote run handles intentionally expose no host PID for RSS inspection.
    if duduclaw_core::execution_gate::isolation_required() {
        return tokio::time::timeout(timeout, child.wait_with_output()).await
            .map_err(|_| MediaProcessError::TimedOut)?.map_err(MediaProcessError::Io);
    }
    let pid = child.id().ok_or(MediaProcessError::MemoryUnavailable)?;
    let mut group = MediaProcessGroup(Some(child.control()));
    let output = child.wait_with_output();
    tokio::pin!(output);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            result = &mut output => {
                if result.is_ok() { group.0 = None; }
                return result.map_err(MediaProcessError::Io);
            }
            _ = &mut deadline => return Err(MediaProcessError::TimedOut),
            _ = poll.tick() => {
                match read_rss(pid) {
                    Ok(bytes) if bytes > memory_bytes => {
                        group.kill();
                        let _ = (&mut output).await;
                        group.0 = None;
                        return Err(MediaProcessError::MemoryLimit);
                    }
                    Ok(_) => (),
                    // A process can exit between select's wait and the RSS read.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ESRCH) => (),
                    Err(_) => return Err(MediaProcessError::MemoryUnavailable),
                }
            }
        }
    }
}

async fn media_output(command: &mut tokio::process::Command, timeout: Duration, memory_bytes: u64) -> Result<std::process::Output, MediaProcessError> {
    #[cfg(unix)] { media_output_with_rss(command, timeout, memory_bytes, media_rss).await }
    #[cfg(not(unix))] {
        tokio::time::timeout(timeout, duduclaw_core::execution_gate::with_memory_limit(memory_bytes,
            duduclaw_core::execution_gate::output(command, duduclaw_core::execution_gate::ExecutionClass::ParseWorker))).await.map_err(|_| MediaProcessError::TimedOut)?.map_err(MediaProcessError::Io)
    }
}

const FORMATS: &str = "mov,mp3,wav,flac,ogg,matroska,webm,aac,aiff,avi";

pub struct AudioChunk { pub path: PathBuf, pub offset_ms: u64 }
pub struct PreparedAudio {
    pub chunks: Vec<AudioChunk>,
    /// Zero means unavailable without ffprobe; segment end times remain available.
    pub duration_ms: u64,
    _temporary: tempfile::TempDir,
}

/// Reject traversal before normalization. Every existing component must be real.
fn no_links(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) { return Err("附件路徑不可包含上層目錄".into()); }
        current.push(component);
        if matches!(component, Component::Prefix(_)) { continue; }
        let meta = std::fs::symlink_metadata(&current).map_err(|_| "無法讀取附件路徑".to_string())?;
        if meta.file_type().is_symlink() { return Err("附件路徑不可使用符號連結".into()); }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 { return Err("附件路徑不可使用重新解析點".into()); }
        }
    }
    Ok(())
}

fn check_file(file: &File, max_bytes: u64) -> Result<(), String> {
    let meta = file.metadata().map_err(|_| "無法讀取附件資訊".to_string())?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > max_bytes {
        return Err("請提供大小在設定上限內的非空音訊或影片檔案".into());
    }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 { return Err("附件不可使用硬連結".into()); }
    }
    #[cfg(windows)] {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct HandleInfo {
            attributes: u32, creation: [u32; 2], access: [u32; 2], write: [u32; 2],
            volume: u32, size_high: u32, size_low: u32, links: u32, index_high: u32, index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut HandleInfo) -> i32;
        }
        let mut info = std::mem::MaybeUninit::<HandleInfo>::uninit();
        // SAFETY: the handle is borrowed from a live File, and the Win32
        // structure has the documented 13 DWORD layout (including FILETIMEs).
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err("無法確認附件連結資訊".into());
        }
        let info = unsafe { info.assume_init() };
        if info.links != 1 || info.attributes & 0x400 != 0 {
            return Err("附件不可使用硬連結或重新解析點".into());
        }
    }
    Ok(())
}

pub fn validate_attachment(agent_dir: &Path, input: &Path, max_bytes: u64) -> Result<PathBuf, String> {
    if input.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("附件路徑不可包含上層目錄".into());
    }
    if !agent_dir.is_absolute() { return Err("員工目錄必須是完整路徑".into()); }
    let root = agent_dir.join("attachments");
    no_links(&root)?;
    let candidate = if input.is_absolute() { input.to_path_buf() } else { root.join(input) };
    // The lexical containment check also forbids crossing a symlink before canonicalization.
    if !candidate.starts_with(&root) { return Err("只能轉錄目前員工自己的附件".into()); }
    no_links(&candidate)?;
    check_file(&open_read(&candidate)?, max_bytes)?;
    Ok(candidate)
}

/// Read a bounded attachment through held directory descriptors. Re-check the
/// opened file after validation to reject links and growth before allocation.
pub fn read_attachment(agent_dir: &Path, input: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let path = validate_attachment(agent_dir, input, max_bytes)?;
    let file = open_read(&path)?;
    check_file(&file, max_bytes)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| "無法讀取附件")?;
    if bytes.is_empty() || bytes.len() as u64 > max_bytes { return Err("附件超過大小上限".into()); }
    Ok(bytes)
}

// Walk relative to held directory descriptors, so replacing an ancestor with a
// symlink between validation and open cannot redirect a read/write (Unix).
#[cfg(unix)]
fn directory_handle(path: &Path) -> Result<File, String> {
    use std::os::{fd::{AsRawFd, FromRawFd}, unix::ffi::OsStrExt};
    let mut dir = File::open("/").map_err(|_| "無法開啟附件目錄".to_string())?;
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| "目錄名稱無效".to_string())?;
                let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
                if fd < 0 { return Err("無法安全開啟附件目錄".into()); }
                dir = unsafe { File::from_raw_fd(fd) };
            }
            _ => return Err("附件目錄必須是完整路徑".into()),
        }
    }
    Ok(dir)
}

/// Windows has no openat in std: pin every ancestor against rename/deletion,
/// opening reparse points themselves and rejecting them before walking on.
#[cfg(windows)]
fn windows_directories(path: &Path) -> Result<Vec<File>, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let mut current = PathBuf::new();
    let mut handles = Vec::new();
    for part in path.components() {
        match part {
            Component::Prefix(prefix) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)) => { current.push(part); continue; }
            Component::RootDir | Component::Normal(_) => current.push(part),
            Component::CurDir => continue,
            _ => return Err("附件必須位於本機磁碟的完整路徑".into()),
        }
        let file = OpenOptions::new().read(true).share_mode(3)
            .custom_flags(0x02000000 | 0x00200000).open(&current)
            .map_err(|_| "無法安全開啟附件目錄")?;
        let meta = file.metadata().map_err(|_| "無法讀取附件目錄資訊")?;
        if !meta.is_dir() || meta.file_attributes() & 0x400 != 0 { return Err("附件目錄不可使用重新解析點".into()); }
        handles.push(file);
    }
    if handles.is_empty() { return Err("附件目錄必須是完整路徑".into()); }
    Ok(handles)
}

fn open_read(path: &Path) -> Result<File, String> {
    #[cfg(unix)] {
        use std::os::{fd::{AsRawFd, FromRawFd}, unix::ffi::OsStrExt};
        let dir = directory_handle(path.parent().ok_or("附件路徑無效")?)?;
        let name = std::ffi::CString::new(path.file_name().ok_or("附件名稱無效")?.as_bytes()).map_err(|_| "附件名稱無效")?;
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 { return Err("無法安全開啟附件".into()); }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    #[cfg(windows)] {
        use std::os::windows::fs::OpenOptionsExt;
        let _parents = windows_directories(path.parent().ok_or("附件路徑無效")?)?;
        OpenOptions::new().read(true).share_mode(1).custom_flags(0x00200000)
            .open(path).map_err(|_| "無法安全開啟附件".to_string())
    }
    #[cfg(not(any(unix, windows)))] {
        Err("此平台尚未支援安全的會議附件存取".into())
    }
}

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}
fn is_audio(ext: &str) -> bool { matches!(ext, "mp3" | "mpga" | "m4a" | "wav" | "flac" | "ogg" | "opus" | "aac" | "aiff" | "aif") }
fn is_video(ext: &str) -> bool { matches!(ext, "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v") }

/// Arguments are individual OS strings; no shell interpolation, remote protocols,
/// playlist demuxers or interactive stdin. Encoded chunks stay below Whisper's cap.
pub fn ffmpeg_arguments(source: &Path, output_pattern: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-protocol_whitelist", "file,pipe", "-format_whitelist", FORMATS, "-i"]
        .into_iter().map(Into::into).collect();
    args.push(source.as_os_str().to_owned());
    args.extend(["-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000", "-c:a", "aac", "-b:a", "48k", "-f", "segment", "-segment_time", "1800", "-reset_timestamps", "1", "-segment_format", "mp4", "-t", "172800"].into_iter().map(Into::into));
    args.push(output_pattern.as_os_str().to_owned());
    args
}

async fn duration(path: &Path, memory_bytes: u64) -> Result<Option<u64>, String> {
    let mut command = tokio::process::Command::new("ffprobe");
    command.args(["-v", "error", "-protocol_whitelist", "file,pipe", "-format_whitelist", FORMATS, "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    restrict_media_process_with_memory_limit(&mut command, 30, memory_bytes);
    let output = match media_output(&mut command, Duration::from_secs(30), memory_bytes).await {
        Ok(output) => output,
        Err(MediaProcessError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.message("無法讀取錄音長度")),
    };
    if !output.status.success() { return Err("無法辨識錄音，請確認媒體格式".into()); }
    let seconds = String::from_utf8_lossy(&output.stdout).trim().parse::<f64>().map_err(|_| "無法讀取錄音長度")?;
    if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_CHUNKS as f64 * CHUNK_SECONDS as f64 {
        return Err("錄音長度須大於零且不可超過 48 小時".into());
    }
    Ok(Some((seconds * 1000.0).round() as u64))
}

fn unsegmented_duration(duration_ms: Option<u64>) -> Result<u64, String> {
    let duration_ms = duration_ms.filter(|ms| *ms > 0)
        .ok_or("計算轉錄費用需要本機錄音長度，請先安裝 ffprobe 或完整 ffmpeg 套件。")?;
    if duration_ms > CHUNK_SECONDS * 1000 {
        return Err("超過 30 分鐘的錄音需要 ffmpeg 分段，請先安裝完整 ffmpeg 套件。".into());
    }
    Ok(duration_ms)
}

pub async fn prepare_audio(source: &Path, max_bytes: u64) -> Result<PreparedAudio, String> {
    prepare_audio_in(source, max_bytes, None).await
}

/// Gateway jobs keep every scratch file under their reserved staging directory.
pub async fn prepare_audio_in(source: &Path, max_bytes: u64, staging_root: Option<&Path>) -> Result<PreparedAudio, String> {
    prepare_audio_bound(source,max_bytes,staging_root,None).await
}

pub async fn prepare_audio_bound(source: &Path, max_bytes: u64, staging_root: Option<&Path>, binding: Option<(&str,u64)>) -> Result<PreparedAudio, String> {
    prepare_audio_bound_with_memory_limit(source, max_bytes, staging_root, binding, DEFAULT_MEDIA_MEMORY_LIMIT_BYTES).await
}

pub async fn prepare_audio_bound_with_memory_limit(source: &Path, max_bytes: u64, staging_root: Option<&Path>, binding: Option<(&str,u64)>, memory_bytes: u64) -> Result<PreparedAudio, String> {
    if memory_bytes == 0 || memory_bytes > i64::MAX as u64 { return Err("音訊處理記憶體上限須為有效正整數".into()); }
    let source = if source.is_absolute() { source.to_owned() } else {
        std::env::current_dir().map_err(|_| "無法讀取目前目錄")?.join(source)
    };
    let ext = extension(&source);
    if !is_audio(&ext) && !is_video(&ext) { return Err("不支援此錄音格式，請提供音訊或影片檔案".into()); }
    let file = open_read(&source)?;
    check_file(&file, max_bytes)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("duduclaw-transcription-");
    let temporary = match staging_root {
        Some(root) => { no_links(root)?; builder.tempdir_in(root) },
        None => builder.tempdir(),
    }.map_err(|_| "無法建立轉錄暫存目錄")?;
    let snapshot = temporary.path().join(format!("source.{ext}"));
    let snapshot_copy = snapshot.clone();
    // Snapshot from the held descriptor, with a cap even if another writer grows it.
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut target = OpenOptions::new().write(true).create_new(true).open(snapshot_copy).map_err(|_| "無法建立轉錄暫存檔")?;
        let copied = std::io::copy(&mut file.take(max_bytes.saturating_add(1)), &mut target).map_err(|_| "無法複製錄音")?;
        if copied == 0 || copied > max_bytes { return Err("錄音超過設定的大小上限".into()); }
        Ok(())
    }).await.map_err(|_| "準備錄音失敗")??;
    // Verify the private copy that ffmpeg actually consumes, not an earlier pathname read.
    if let Some((digest,size))=binding { verify_snapshot(&snapshot,digest,size)?; }
    if duduclaw_parse_worker::client::required() {
        // Protected jobs never invoke gateway-uid ffmpeg/ffprobe. E inspects
        // the exact private snapshot that the host later sends to the provider.
        // Large/multi-chunk recordings await a real isolated media producer.
        let input = snapshot.clone();
        let (format, duration_ms) = tokio::task::spawn_blocking(move || {
            let mut file = open_read(&input)?;
            duduclaw_parse_worker::adapters::inspect_audio_file(&mut file)
                .map_err(|_| "隔離服務無法辨識錄音，已停止轉錄。".to_string())
        }).await.map_err(|_| "隔離音訊處理失敗。")??;
        let expected = match format {
            duduclaw_parse_worker::worker::AudioFormat::Wav => "wav",
            duduclaw_parse_worker::worker::AudioFormat::Mp3 => "mp3",
        };
        if ext != expected || duration_ms > CHUNK_SECONDS * 1000 {
            return Err("隔離轉錄目前支援 30 分鐘內、25 MiB 以下的 WAV／MP3；請先分割錄音。".into());
        }
        return Ok(PreparedAudio { chunks: vec![AudioChunk { path: snapshot, offset_ms: 0 }],
            duration_ms, _temporary: temporary });
    }
    let mut probe = tokio::process::Command::new("ffmpeg");
    probe.arg("-version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    restrict_media_process_with_memory_limit(&mut probe, 10, memory_bytes);
    let available = match media_output(&mut probe, Duration::from_secs(10), memory_bytes).await {
        Ok(output) => output.status.success(),
        Err(MediaProcessError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.message("無法啟動 ffmpeg")),
    };
    if !available {
        if is_video(&ext) { return Err("轉錄影片需要 ffmpeg，請先安裝 ffmpeg，或改上傳音訊檔案".into()); }
        let duration_ms = unsegmented_duration(duration(&snapshot, memory_bytes).await?)?;
        return Ok(PreparedAudio { chunks: vec![AudioChunk { path: snapshot, offset_ms: 0 }], duration_ms, _temporary: temporary });
    }
    let total_duration = duration(&snapshot, memory_bytes).await?.ok_or("分段錄音需要 ffprobe，請安裝完整 ffmpeg 套件")?;
    let output_pattern = temporary.path().join("chunk-%03d.m4a");
    let mut command = tokio::process::Command::new("ffmpeg");
    command.args(ffmpeg_arguments(&snapshot, &output_pattern)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    restrict_media_process_with_memory_limit(&mut command, 3600, memory_bytes);
    let output = media_output(&mut command, Duration::from_secs(3600), memory_bytes).await
        .map_err(|error| match error { MediaProcessError::TimedOut => "音訊處理逾時".into(), other => other.message("無法處理錄音") })?;
    if !output.status.success() { return Err("無法擷取音訊，請確認檔案包含音軌且格式正確".into()); }
    let mut chunks = Vec::new();
    for index in 0..=MAX_CHUNKS {
        let path = temporary.path().join(format!("chunk-{index:03}.m4a"));
        if !path.exists() { break; }
        if index == MAX_CHUNKS { return Err("錄音分段數量超過上限".into()); }
        chunks.push(AudioChunk { path, offset_ms: index as u64 * CHUNK_SECONDS * 1000 });
    }
    if chunks.is_empty() { return Err("錄音沒有可轉錄的音軌".into()); }
    Ok(PreparedAudio { chunks, duration_ms: total_duration, _temporary: temporary })
}

pub fn append_offset(target: &mut Vec<Segment>, segments: Vec<Segment>, offset_ms: u64) -> Result<(), String> {
    let count = target.len().checked_add(segments.len()).ok_or(OUTPUT_LIMIT_ERROR)?;
    if count > MAX_SEGMENTS { return Err(OUTPUT_LIMIT_ERROR.into()); }
    let (old_text, _) = transcript_budget(target)?;
    let (new_text, _) = transcript_budget(&segments)?;
    let text = old_text.checked_add(new_text).ok_or(OUTPUT_LIMIT_ERROR)?;
    let output = text.checked_mul(7).and_then(|n| count.checked_mul(256).and_then(|overhead| n.checked_add(overhead)))
        .and_then(|n| n.checked_add(4096)).ok_or(OUTPUT_LIMIT_ERROR)?;
    if text > MAX_TEXT_BYTES || output > MAX_OUTPUT_BYTES { return Err(OUTPUT_LIMIT_ERROR.into()); }
    for segment in &segments {
        if segment.end_ms < segment.start_ms { return Err("逐字稿時間戳錯誤".into()); }
        segment.end_ms.checked_add(offset_ms).ok_or("逐字稿時間戳超出範圍")?;
    }
    for mut segment in segments {
        segment.start_ms += offset_ms;
        segment.end_ms += offset_ms;
        target.push(segment);
    }
    Ok(())
}

/// Persist each completed provider attempt before the next chunk can start.
#[async_trait::async_trait]
pub trait UsageObserver: Send + Sync {
    async fn record(&self, usage: &TranscriptionUsage) -> Result<(), String>;
    /// Called immediately before each provider attempt; may refuse a cancelled job.
    async fn before_chunk(&self, _index: usize, _total: usize) -> Result<(), String> { Ok(()) }
}

pub async fn transcribe_chunks(engine: &dyn TranscriptionEngine, prepared: &PreparedAudio, language: &str, observer: &dyn UsageObserver) -> Result<Transcript, String> {
    if language.is_empty() || language.len() > 32 || !language.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("語言提示格式錯誤".into());
    }
    let mut transcript = Transcript { language: String::new(), segments: Vec::new() };
    let mut speakers = BTreeSet::new();
    for (index, chunk) in prepared.chunks.iter().enumerate() {
        observer.before_chunk(index + 1, prepared.chunks.len()).await?;
        tracing::info!(chunk = index + 1, total = prepared.chunks.len(), "transcribing meeting chunk");
        let labels = speakers.iter().cloned().collect::<Vec<String>>();
        let chunk_ms = (prepared.duration_ms > 0).then(|| prepared.duration_ms.saturating_sub(chunk.offset_ms).min(CHUNK_SECONDS * 1000));
        let result = match engine.transcribe(&chunk.path, language, &labels).await {
            Ok(result) => result,
            Err(error) => {
                for usage in &error.usage {
                    if let Err(accounting) = observer.record(&bounded_usage(usage, chunk_ms)).await {
                        return Err(format!("{}；{}", error.message, accounting));
                    }
                }
                return Err(error.message);
            }
        };
        for usage in &result.usage { observer.record(&bounded_usage(usage, chunk_ms)).await?; }
        if transcript.language.is_empty() { transcript.language = result.transcript.language; }
        let limit = prepared.duration_ms.saturating_sub(chunk.offset_ms).min(CHUNK_SECONDS * 1000).saturating_add(1000);
        if prepared.duration_ms > 0 && result.transcript.segments.iter().any(|s| s.end_ms > limit) {
            return Err("逐字稿時間戳超過錄音分段長度".into());
        }
        for label in result.transcript.segments.iter().filter_map(|s| s.speaker.clone()) {
            if speakers.len() < 100 { speakers.insert(label); }
        }
        append_offset(&mut transcript.segments, result.transcript.segments, chunk.offset_ms)?;
    }
    Ok(transcript)
}

pub fn convert_taiwan(transcript: &mut Transcript) -> Result<(), String> {
    let converter = ferrous_opencc::OpenCC::from_config(ferrous_opencc::config::BuiltinConfig::S2twp)
        .map_err(|_| "無法載入臺灣繁體轉換字典".to_string())?;
    for segment in &mut transcript.segments {
        segment.text = converter.convert(&segment.text);
        segment.speaker = segment.speaker.as_ref().map(|s| converter.convert(s));
    }
    Ok(())
}

fn one_line(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}
fn timestamp(ms: u64) -> String { let sec = ms / 1000; format!("{:02}:{:02}:{:02}", sec / 3600, sec / 60 % 60, sec % 60) }
pub fn markdown(source: &str, date: &str, engine: &str, duration_ms: u64, transcript: &Transcript) -> String {
    let duration = if duration_ms == 0 { "未取得".to_string() } else { timestamp(duration_ms) };
    let mut result = format!("# {}\n\n日期：{}  \n轉錄引擎：{}  \n錄音長度：{}  \n偵測語言：{}\n\n", one_line(source), one_line(date), one_line(engine), duration, one_line(&transcript.language));
    for segment in &transcript.segments {
        let speaker = segment.speaker.as_deref().unwrap_or("講者未辨識");
        result.push_str(&format!("[{}] {}：{}\n\n", timestamp(segment.start_ms), one_line(speaker), one_line(&segment.text)));
    }
    result
}

pub struct OutputPaths { pub markdown: PathBuf, pub json: PathBuf, pub bytes: u64 }

/// Check output access before any billable request, using an empty private probe.
pub fn preflight_output(out: &Path) -> Result<(), String> {
    let out = if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir().map_err(|_| "無法讀取目前目錄")?.join(out) };
    no_links(&out)?;
    #[cfg(unix)] {
        use std::os::fd::{AsRawFd, FromRawFd};
        let directory = directory_handle(&out)?;
        let name = std::ffi::CString::new(format!(".transcription-{}", uuid::Uuid::new_v4())).map_err(|_| "輸出名稱無效")?;
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
        if fd < 0 { return Err("逐字稿輸出目錄無法寫入".into()); }
        let file = unsafe { File::from_raw_fd(fd) };
        let removed = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        drop(file);
        if removed != 0 { return Err("無法完成逐字稿目錄檢查".into()); }
        Ok(())
    }
    #[cfg(windows)] {
        let _parents = windows_directories(&out)?;
        let probe = tempfile::NamedTempFile::new_in(&out).map_err(|_| "逐字稿輸出目錄無法寫入")?;
        probe.close().map_err(|_| "無法完成逐字稿目錄檢查".into())
    }
    #[cfg(not(any(unix, windows)))] { Err("此平台尚未支援會議逐字稿".into()) }
}

pub fn save_outputs(out: &Path, source: &str, engine: &str, duration_ms: u64, transcript: &Transcript) -> Result<OutputPaths, String> {
    save_outputs_with_id(out, source, engine, duration_ms, transcript, None)
}

/// A gateway job UUID gives retries the same output names. Files are replaced
/// atomically from private temporary files after conversion and redaction.
pub fn save_outputs_with_id(out: &Path, source: &str, engine: &str, duration_ms: u64, transcript: &Transcript, output_id: Option<&str>) -> Result<OutputPaths, String> {
    let out = if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir().map_err(|_| "無法讀取目前目錄")?.join(out) };
    no_links(&out)?;
    let now = chrono::Utc::now();
    let stem = match output_id {
        Some(id) => format!("transcript-{}", uuid::Uuid::parse_str(id).map_err(|_| "逐字稿工作編號無效")?),
        None => format!("transcript-{}-{}", now.format("%Y%m%dT%H%M%S%.3fZ"), uuid::Uuid::new_v4().simple()),
    };
    transcript_budget(&transcript.segments)?;
    let mut paths = OutputPaths { markdown: out.join(format!("{stem}.md")), json: out.join(format!("{stem}.json")), bytes: 0 };
    let md = markdown(source, &now.to_rfc3339(), engine, duration_ms, transcript);
    let json = serde_json::to_vec_pretty(transcript).map_err(|_| "無法產生逐字稿 JSON")?;
    let output_bytes = md.len().checked_add(json.len()).ok_or(OUTPUT_LIMIT_ERROR)?;
    if output_bytes > MAX_OUTPUT_BYTES { return Err(OUTPUT_LIMIT_ERROR.into()); }
    paths.bytes = output_bytes as u64;
    #[cfg(unix)] let directory = directory_handle(&out)?;
    #[cfg(windows)] let _directories = windows_directories(&out)?;
    let create = |path: &Path| -> Result<File, String> {
        #[cfg(unix)] {
            use std::os::{fd::{AsRawFd, FromRawFd}, unix::ffi::OsStrExt};
            let name = std::ffi::CString::new(path.file_name().ok_or("輸出名稱無效")?.as_bytes()).map_err(|_| "輸出名稱無效")?;
            let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
            if fd < 0 { return Err("無法安全建立逐字稿".into()); }
            Ok(unsafe { File::from_raw_fd(fd) })
        }
        #[cfg(windows)] {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new().write(true).create_new(true).share_mode(0).custom_flags(0x00200000)
                .open(path).map_err(|_| "無法建立逐字稿".into())
        }
        #[cfg(not(any(unix, windows)))] { Err("此平台尚未支援會議逐字稿".into()) }
    };
    let temporary_json = out.join(format!(".transcript-{}.json", uuid::Uuid::new_v4()));
    let temporary_md = out.join(format!(".transcript-{}.md", uuid::Uuid::new_v4()));
    struct Cleanup<F: Fn()>(F);
    impl<F: Fn()> Drop for Cleanup<F> { fn drop(&mut self) { (self.0)(); } }
    let _cleanup = Cleanup(|| {
        for path in [&temporary_json, &temporary_md] {
            #[cfg(unix)] {
                use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
                if let Some(name) = path.file_name().and_then(|name| std::ffi::CString::new(name.as_bytes()).ok()) {
                    unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0); }
                }
            }
            #[cfg(windows)] { let _ = std::fs::remove_file(path); }
        }
    });
    let rename = |from: &Path, to: &Path| -> Result<(), String> {
        #[cfg(unix)] {
            use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
            let from = std::ffi::CString::new(from.file_name().ok_or("輸出名稱無效")?.as_bytes()).map_err(|_| "輸出名稱無效")?;
            let to = std::ffi::CString::new(to.file_name().ok_or("輸出名稱無效")?.as_bytes()).map_err(|_| "輸出名稱無效")?;
            if unsafe { libc::renameat(directory.as_raw_fd(), from.as_ptr(), directory.as_raw_fd(), to.as_ptr()) } != 0 {
                return Err("無法儲存逐字稿".into());
            }
            Ok(())
        }
        #[cfg(windows)] { std::fs::rename(from, to).map_err(|_| "無法儲存逐字稿".into()) }
        #[cfg(not(any(unix, windows)))] { Err("此平台尚未支援會議逐字稿".into()) }
    };
    // Both documents are fully converted and redacted before either is published.
    let mut json_file = create(&temporary_json)?;
    json_file.write_all(&json).and_then(|_| json_file.sync_all()).map_err(|_| "無法儲存逐字稿 JSON")?;
    drop(json_file);
    let mut md_file = create(&temporary_md)?;
    md_file.write_all(md.as_bytes()).and_then(|_| md_file.sync_all()).map_err(|_| "無法儲存逐字稿 Markdown")?;
    drop(md_file);
    rename(&temporary_json, &paths.json)?;
    rename(&temporary_md, &paths.markdown)?;
    #[cfg(unix)] directory.sync_all().map_err(|_| "無法同步逐字稿目錄")?;
    Ok(paths)
}

#[cfg(test)]
mod tests;

/// Remove only a generated attachment while pinning its parent directory.
pub fn remove_attachment(agent_dir: &Path, path: &Path) -> Result<(), String> {
    if !path.starts_with(agent_dir.join("attachments")) {return Err("只能清理員工自己的會議附件".into());}
    #[cfg(unix)] {
        use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
        let dir=directory_handle(path.parent().ok_or("附件路徑無效")?)?;
        let name=std::ffi::CString::new(path.file_name().ok_or("附件路徑無效")?.as_bytes()).map_err(|_|"附件路徑無效")?;
        if unsafe {libc::unlinkat(dir.as_raw_fd(),name.as_ptr(),0)} != 0 && std::io::Error::last_os_error().kind()!=std::io::ErrorKind::NotFound {
            return Err("無法清理會議附件".into());
        }
        Ok(())
    }
    #[cfg(windows)] {
        let _parents=windows_directories(path.parent().ok_or("附件路徑無效")?)?;
        match std::fs::remove_file(path) {Ok(())=>Ok(()),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(()),Err(_)=>Err("無法清理會議附件".into())}
    }
    #[cfg(not(any(unix,windows)))] {Err("此平台無法安全清理會議附件".into())}
}
