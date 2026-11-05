//! Fixed program protocol for a root-registered employee helper.
//! Signature verification and namespace admission belong to spawnd/B2; this
//! helper accepts only their sealed fd 4 input, never host paths or commands.
use serde_json::{Value, json};

pub const PROGRAM_RUNTIME: &str = "duduclaw-program-v1";
pub const PROGRAM_ARGUMENT: &str = "--program-fd-v1";
pub const MAX_CODE_BYTES: usize = 64 * 1024;
pub const MAX_REQUEST_BYTES: usize = MAX_CODE_BYTES * 6 + 1024;
// Leave room for worst-case JSON escaping within the one-MiB result envelope.
pub const MAX_OUTPUT_BYTES: usize = 128 * 1024;
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;
pub const WORKSPACE: &str = "/work";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language { Python, Bash, JavaScript }

impl Language {
    fn name(self) -> &'static str {
        match self { Self::Python => "python", Self::Bash => "bash", Self::JavaScript => "javascript" }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "python" => Ok(Self::Python), "bash" => Ok(Self::Bash), "javascript" => Ok(Self::JavaScript),
            _ => Err("unsupported program language".into()),
        }
    }

    #[cfg(target_os = "linux")]
    fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Python => ("/runtime/python3", &["-I", "-"]),
            Self::Bash => ("/runtime/bash", &["--noprofile", "--norc", "-s", "--"]),
            Self::JavaScript => ("/runtime/node", &["--input-type=commonjs", "-"]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramRequest {
    pub language: Language,
    pub code: String,
    pub timeout_seconds: u64,
}

impl ProgramRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.code.is_empty() || self.code.len() > MAX_CODE_BYTES || !(1..=300).contains(&self.timeout_seconds) {
            return Err("invalid program bounds".into());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        serde_json::to_vec(&json!({"language":self.language.name(), "code":self.code,
            "timeout_seconds":self.timeout_seconds})).map_err(|_| "program encoding failed".into())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST_BYTES { return Err("program request too large".into()); }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid program JSON")?;
        let fields = value.as_object().ok_or("program request must be an object")?;
        if fields.len() != 3 || fields.keys().any(|key|
            !matches!(key.as_str(), "language" | "code" | "timeout_seconds")) {
            return Err("unknown or missing program field".into());
        }
        let request = Self {
            language: Language::parse(value["language"].as_str().ok_or("missing language")?)?,
            code: value["code"].as_str().ok_or("missing code")?.to_owned(),
            timeout_seconds: value["timeout_seconds"].as_u64().ok_or("invalid timeout")?,
        };
        request.validate()?;
        Ok(request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramStatus { Exited, TimedOut, StartFailed, IoFailed }

impl ProgramStatus {
    fn name(self) -> &'static str {
        match self {
            Self::Exited => "exited", Self::TimedOut => "timed_out",
            Self::StartFailed => "start_failed", Self::IoFailed => "io_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramResult {
    pub status: ProgramStatus,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

impl ProgramResult {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(&json!({"status":self.status.name(), "exit_code":self.exit_code,
            "signal":self.signal, "stdout":self.stdout, "stderr":self.stderr, "truncated":self.truncated}))
            .map_err(|_| "program result encoding failed")?;
        if bytes.len() > MAX_RESULT_BYTES { return Err("program result too large".into()); }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_RESULT_BYTES { return Err("program result too large".into()); }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid program result JSON")?;
        let fields = value.as_object().ok_or("program result must be an object")?;
        if fields.len() != 6 || fields.keys().any(|key| !matches!(key.as_str(),
            "status" | "exit_code" | "signal" | "stdout" | "stderr" | "truncated")) {
            return Err("unknown or missing program result field".into());
        }
        let number = |key: &str| -> Result<Option<i32>, String> {
            if value[key].is_null() { return Ok(None); }
            value[key].as_i64().and_then(|v| i32::try_from(v).ok()).map(Some)
                .ok_or_else(|| "invalid program exit status".into())
        };
        let result = Self {
            status: match value["status"].as_str() {
                Some("exited") => ProgramStatus::Exited, Some("timed_out") => ProgramStatus::TimedOut,
                Some("start_failed") => ProgramStatus::StartFailed, Some("io_failed") => ProgramStatus::IoFailed,
                _ => return Err("unknown program status".into()),
            },
            exit_code: number("exit_code")?, signal: number("signal")?,
            stdout: value["stdout"].as_str().ok_or("missing stdout")?.into(),
            stderr: value["stderr"].as_str().ok_or("missing stderr")?.into(),
            truncated: value["truncated"].as_bool().ok_or("missing truncation flag")?,
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), String> {
        if self.stdout.len().saturating_add(self.stderr.len()) > MAX_OUTPUT_BYTES
            || (self.status == ProgramStatus::Exited && self.exit_code.is_none() && self.signal.is_none())
            || (self.exit_code.is_some() && self.signal.is_some())
            || self.exit_code.is_some_and(|code| !(0..=255).contains(&code))
            || self.signal.is_some_and(|signal| !(1..=127).contains(&signal))
            || (self.status != ProgramStatus::Exited && (self.exit_code.is_some() || self.signal.is_some())) {
            return Err("invalid program result".into());
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct Capture { stdout: Vec<u8>, stderr: Vec<u8>, truncated: bool }

#[cfg(any(target_os = "linux", test))]
impl Capture {
    fn append(&mut self, stderr: bool, bytes: &[u8]) {
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(self.stdout.len() + self.stderr.len());
        let take = bytes.len().min(remaining);
        self.truncated |= take != bytes.len();
        let destination = if stderr { &mut self.stderr } else { &mut self.stdout };
        destination.extend(bytes.iter().take(take).copied());
    }

    fn finish(&self, status: ProgramStatus, exit_code: Option<i32>, signal: Option<i32>) -> ProgramResult {
        let mut truncated = self.truncated;
        let mut remaining = MAX_OUTPUT_BYTES;
        // Invalid UTF-8 replacement can expand the byte count; bound final text too.
        let mut decode = |bytes: &[u8]| {
            let mut text = String::from_utf8_lossy(bytes).into_owned();
            if text.len() > remaining {
                let mut end = remaining;
                while !text.is_char_boundary(end) { end -= 1; }
                text.truncate(end);
                truncated = true;
            }
            remaining -= text.len();
            text
        };
        let stdout = decode(&self.stdout);
        let stderr = decode(&self.stderr);
        ProgramResult { status, exit_code, signal, stdout, stderr, truncated }
    }
}

#[cfg(target_os = "linux")]
async fn execute(request: &ProgramRequest, cwd: &std::path::Path) -> ProgramResult {
    let (_program, arguments) = request.language.command();
    #[cfg(not(test))]
    let program = _program;
    // Explicit test seam: fixtures exercise bounded I/O using system tools,
    // never the production sealed-fd/signature entry.
    #[cfg(test)]
    let program = match request.language { Language::Python => "/usr/bin/python3", Language::Bash => "/usr/bin/bash", Language::JavaScript => "/usr/bin/node" };
    execute_command(program, arguments, request.code.as_bytes(), request.timeout_seconds, cwd).await
}

#[cfg(target_os = "linux")]
pub(crate) async fn execute_command(program: &str, arguments: &[&str], input: &[u8], timeout_seconds: u64,
    cwd: &std::path::Path) -> ProgramResult {
    use std::{io, process::Stdio, sync::{Arc, Mutex}, time::Duration};
    use std::os::unix::process::ExitStatusExt;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

    async fn drain(mut stream: impl AsyncRead + Unpin, stderr: bool, capture: Arc<Mutex<Capture>>) -> io::Result<()> {
        let mut buffer = [0u8; 8192];
        loop {
            let n = stream.read(&mut buffer).await?;
            if n == 0 { return Ok(()); }
            capture.lock().map_err(|_| io::Error::other("capture unavailable"))?.append(stderr, &buffer[..n]);
        }
    }

    let capture = Arc::new(Mutex::new(Capture::default()));
    let failure = |status| capture.lock().unwrap().finish(status, None, None);
    if timeout_seconds == 0 || timeout_seconds > 300 { return failure(ProgramStatus::StartFailed); }
    // This is an employee-side helper, never called from the gateway process.
    // B2 owns the namespace and tears down all descendants when the helper exits.
    let mut command = tokio::process::Command::new(program);
    command.args(arguments).env_clear().env("HOME", "/home/employee").env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", "/tmp").current_dir(cwd).stdin(Stdio::piped())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child, Err(_) => return failure(ProgramStatus::StartFailed),
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let writer = async {
        match stdin.write_all(input).await {
            Ok(()) => {}, Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {},
            Err(error) => return Err(error),
        }
        drop(stdin);
        Ok(())
    };
    let outcome = tokio::time::timeout(Duration::from_secs(timeout_seconds), async {
        tokio::try_join!(child.wait(), writer,
            drain(stdout, false, capture.clone()), drain(stderr, true, capture.clone()))
    }).await;
    match outcome {
        Ok(Ok((status, (), (), ()))) => capture.lock().unwrap().finish(ProgramStatus::Exited, status.code(), status.signal()),
        error => {
            // Even if the leader has exited while a descendant holds a pipe,
            // wait reaps the leader; returning exits the helper/B2 namespace.
            let _ = child.start_kill();
            let _ = child.wait().await;
            failure(if error.is_err() { ProgramStatus::TimedOut } else { ProgramStatus::IoFailed })
        }
    }
}

/// Binary entry only. Production callers cannot select a cwd or bypass fd/uid checks.
pub fn main_entry() -> i32 {
    #[cfg(target_os = "linux")]
    { linux_main() }
    #[cfg(not(target_os = "linux"))]
    { 64 }
}

#[cfg(target_os = "linux")]
fn linux_main() -> i32 {
    use std::io::Write;
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 4 || arguments[0] != PROGRAM_ARGUMENT || arguments.iter().skip(1).any(|digest| !valid_digest(digest)) { return 64; }
    let fixed: Vec<_> = arguments.iter().map(String::as_str).collect();
    let Ok(bytes) = helper_input_args(&fixed, MAX_REQUEST_BYTES) else { return 77; };
    let Ok(request) = ProgramRequest::decode(&bytes) else { return 64; };
    let index = match request.language { Language::Python => 1, Language::Bash => 2, Language::JavaScript => 3 };
    if verify_runtime_file(request.language.command().0, &arguments[index]).is_err() { return 77; };

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return 70; };
    let result = runtime.block_on(execute(&request, std::path::Path::new(WORKSPACE)));
    let Ok(bytes) = result.encode() else { return 70; };
    if std::io::stdout().lock().write_all(&bytes).is_err() { return 74; }
    0
}

pub fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The root-signed helper profile pins every interpreter digest in fixed argv.
/// Only immutable /runtime files are used; no OS/PATH interpreter fallback.
#[cfg(target_os = "linux")]
pub(crate) fn verify_runtime_file(path: &str, expected: &str) -> Result<(), ()> {
    use std::{io::Read, os::unix::fs::{MetadataExt, OpenOptionsExt}};
    use sha2::{Digest, Sha256};
    if !valid_digest(expected) { return Err(()); }
    let mut file = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path).map_err(|_| ())?;
    let metadata = file.metadata().map_err(|_| ())?;
    let maximum = 256 * 1024 * 1024;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1 || metadata.len() > maximum { return Err(()); }
    let mut digest = Sha256::new();
    let mut buffer = [0; 8192];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| ())?;
        if count == 0 { break; }
        total += count as u64;
        if total > maximum { return Err(()); }
        digest.update(&buffer[..count]);
    }
    let actual: String = digest.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected { return Err(()); }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn helper_input(argument: &str, limit: usize) -> Result<Vec<u8>, ()> {
    helper_input_args(&[argument], limit)
}
#[cfg(target_os = "linux")]
pub(crate) fn helper_input_args(arguments: &[&str], limit: usize) -> Result<Vec<u8>, ()> {
    use std::{io::Read, os::fd::{AsRawFd, FromRawFd}};
    if std::env::args_os().skip(1).collect::<Vec<_>>() != arguments.iter().map(std::ffi::OsString::from).collect::<Vec<_>>() { return Err(()); }
    // SAFETY: scalar process attribute queries do not dereference pointers.
    let (uid, effective_uid, nnp) = unsafe {
        (libc::getuid(), libc::geteuid(), libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0))
    };
    if uid == 0 || effective_uid != uid || nnp != 1
        || std::env::current_dir().ok().as_deref() != Some(std::path::Path::new(WORKSPACE)) { return Err(()); }
    // SAFETY: query only; reject a missing fd before taking sole ownership.
    if unsafe { libc::fcntl(4, libc::F_GETFD) } < 0 { return Err(()); }
    // SAFETY: fixed B2 input fd, binary entry takes sole ownership.
    let input = unsafe { std::fs::File::from_raw_fd(4) };
    // SAFETY: input is alive; scalar metadata query.
    let seals = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GET_SEALS) };
    let required = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if seals < 0 || seals & required != required { return Err(()); }
    let metadata = input.metadata().map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() > limit as u64 { return Err(()); }
    let mut bytes = Vec::new();
    input.take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() > limit { return Err(()); }
    Ok(bytes)
}

#[cfg(all(target_os = "linux", test))]
fn sealed_request(input: std::fs::File) -> Result<ProgramRequest, String> {
    use std::{io::Read, os::fd::AsRawFd};
    // SAFETY: input remains alive; F_GET_SEALS reads scalar descriptor metadata.
    let seals = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GET_SEALS) };
    let required = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if seals < 0 || seals & required != required { return Err("input is not sealed".into()); }
    let metadata = input.metadata().map_err(|_| "input metadata unavailable")?;
    if !metadata.is_file() || metadata.len() > MAX_REQUEST_BYTES as u64 { return Err("invalid input size".into()); }
    let mut bytes = Vec::new();
    input.take(MAX_REQUEST_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| "input read failed")?;
    ProgramRequest::decode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_helper_protocol_bounds_and_unknown_fields() {
        let request = ProgramRequest { language: Language::Python, code: "print('繁體中文')".into(), timeout_seconds: 3 };
        assert_eq!(ProgramRequest::decode(&request.encode().unwrap()).unwrap(), request);
        for extra in ["argv", "env", "cwd", "uid", "path"] {
            let mut value: Value = serde_json::from_slice(&request.encode().unwrap()).unwrap();
            value[extra] = json!("forged");
            assert!(ProgramRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for timeout in [0, 301, u64::MAX] {
            assert!(ProgramRequest { timeout_seconds: timeout, ..request.clone() }.validate().is_err());
        }
        assert!(ProgramRequest { code: "x".repeat(MAX_CODE_BYTES + 1), ..request.clone() }.validate().is_err());
        assert!(ProgramRequest { code: String::new(), ..request }.validate().is_err());
        for language in ["python3", "/usr/bin/bash", "Bash", "javascript --eval"] {
            assert!(Language::parse(language).is_err());
        }
    }

    #[test]
    fn program_helper_combined_output_and_json_envelope_are_bounded() {
        let mut capture = Capture::default();
        capture.append(false, &vec![0; MAX_OUTPUT_BYTES / 2]);
        capture.append(true, &vec![0xff; MAX_OUTPUT_BYTES]);
        let result = capture.finish(ProgramStatus::Exited, Some(0), None);
        assert!(result.truncated);
        assert!(result.stdout.len() + result.stderr.len() <= MAX_OUTPUT_BYTES);
        let encoded = result.encode().unwrap();
        assert!(encoded.len() <= MAX_RESULT_BYTES);
        assert_eq!(ProgramResult::decode(&encoded).unwrap(), result);
        let mut false_success = result;
        false_success.status = ProgramStatus::TimedOut;
        assert!(false_success.encode().is_err());
    }

    // Explicit test-only seam: these fixtures select a disposable cwd and never
    // invoke the production fd-4 entry point or claim namespace acceptance.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn program_helper_executes_fixed_interpreter_and_reports_exit() {
        let home = tempfile::tempdir().unwrap();
        let request = ProgramRequest { language: Language::Bash,
            code: "printf 'hello'; printf 'error' >&2; exit 7".into(), timeout_seconds: 3 };
        let result = execute(&request, home.path()).await;
        assert_eq!(result.status, ProgramStatus::Exited);
        assert_eq!(result.exit_code, Some(7));
        assert_eq!(result.stdout, "hello");
        assert_eq!(result.stderr, "error");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn program_helper_timeout_kills_and_reaps_interpreter() {
        let home = tempfile::tempdir().unwrap();
        let request = ProgramRequest { language: Language::Bash,
            code: "printf '%s' $$ > pid; while :; do :; done".into(), timeout_seconds: 1 };
        let result = execute(&request, home.path()).await;
        assert_eq!(result.status, ProgramStatus::TimedOut);
        assert_eq!(result.exit_code, None);
        let pid: i32 = std::fs::read_to_string(home.path().join("pid")).unwrap().parse().unwrap();
        // SAFETY: signal zero only probes the completed fixture process.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn program_helper_drains_both_streams_past_combined_limit() {
        let home = tempfile::tempdir().unwrap();
        let request = ProgramRequest { language: Language::Python,
            code: "import os\nfor _ in range(64):\n os.write(1,b'a'*8192)\n os.write(2,b'b'*8192)\n".into(), timeout_seconds: 3 };
        let result = execute(&request, home.path()).await;
        assert_eq!(result.status, ProgramStatus::Exited);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.truncated);
        assert_eq!(result.stdout.len() + result.stderr.len(), MAX_OUTPUT_BYTES);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn program_helper_rejects_unsealed_and_accepts_fully_sealed_input() {
        use std::{io::{Seek, Write}, os::fd::{AsRawFd, FromRawFd}};
        assert!(sealed_request(tempfile::tempfile().unwrap()).is_err());
        for sealed in [false, true] {
            // SAFETY: the static C name is NUL terminated; flags are kernel constants.
            let fd = unsafe { libc::memfd_create(c"program-fixture".as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
            assert!(fd >= 0);
            // SAFETY: memfd_create returned a fresh owned descriptor above.
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            let request = ProgramRequest { language: Language::Bash, code: "exit 0".into(), timeout_seconds: 1 };
            file.write_all(&request.encode().unwrap()).unwrap();
            file.rewind().unwrap();
            if sealed {
                let flags = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
                // SAFETY: the owned memfd remains alive; F_ADD_SEALS takes integer flags.
                assert_eq!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, flags) }, 0);
                assert_eq!(sealed_request(file).unwrap(), request);
            } else {
                assert!(sealed_request(file).is_err());
            }
        }
    }
}
