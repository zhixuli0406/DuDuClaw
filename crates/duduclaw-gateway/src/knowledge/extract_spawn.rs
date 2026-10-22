//! Run `duduclaw knowledge-extract` as a child process.
//!
//! Document parsers read customer files, and a panic, stack overflow or
//! runaway allocation inside one would take the whole gateway (every
//! channel, every schedule) down with it. So the parser runs in a child:
//!
//! - wall-clock timeout (`[knowledge] extract_timeout_secs`), then the whole
//!   process group is killed;
//! - Unix: `RLIMIT_CPU` (timeout + 5 s) and no core dumps; Linux also
//!   `RLIMIT_AS` = `extract_memory_mb`. macOS and Windows have no reliable
//!   address-space limit, only the timeout;
//! - macOS Seatbelt / Linux Landlock through `duduclaw-sandbox`: no writes
//!   anywhere, no network, and reads limited to the input file, the
//!   extractor binary and the system library directories ([`readable_paths`]).
//!   Where the platform cannot confine (Windows) or confinement fails, the
//!   file is refused (`sandbox_unavailable`) unless the operator set
//!   `[knowledge] extract_when_unavailable = "run_unsandboxed"`; such a run
//!   says `sandboxed: false`;
//! - empty environment, stdin closed, stdout capped at
//!   `max_chars × 4 + 64 KiB` bytes (killed when exceeded).
//!
//! Exit 0 ⇒ the last stdout line is an [`ExtractOutput`]; exit 2 ⇒ the last
//! stderr line is an [`ExtractError`]; anything else maps to
//! `extract_crashed` / `extract_oom` / `extract_timeout`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use duduclaw_knowledge::extract::{ExtractError, ExtractOutput, Format};
use tokio::io::{AsyncRead, AsyncReadExt};

/// The program and leading arguments that start the extractor.
#[derive(Debug, Clone)]
pub struct ExtractorCmd {
    pub program: PathBuf,
    pub leading_args: Vec<String>,
}

impl ExtractorCmd {
    /// This binary's own `knowledge-extract` subcommand.
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            leading_args: vec!["knowledge-extract".to_string()],
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SpawnLimits {
    pub timeout: Duration,
    pub memory_mb: u64,
    pub max_chars: usize,
    /// Run without the OS sandbox where it is unavailable (explicit opt-in).
    pub allow_unsandboxed: bool,
}

/// A successful extraction.
#[derive(Debug, Clone)]
pub struct Spawned {
    pub output: ExtractOutput,
    pub sandboxed: bool,
}

const STDERR_CAP: usize = 64 * 1024;

fn crashed() -> ExtractError {
    ExtractError::new(
        "extract_crashed",
        "解析這份文件時發生錯誤，檔案可能已損毀或格式特殊。",
    )
}

fn oom() -> ExtractError {
    ExtractError::new(
        "extract_oom",
        "解析這份文件需要的記憶體超過上限，已停止。請拆成較小的檔案後再上傳。",
    )
}

fn timeout(secs: u64) -> ExtractError {
    ExtractError::new(
        "extract_timeout",
        &format!("解析這份文件超過 {secs} 秒仍未完成，已停止。請拆成較小的檔案後再上傳。"),
    )
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn apply_rlimits(cmd: &mut std::process::Command, limits: SpawnLimits) {
    use std::os::unix::process::CommandExt;
    let cpu = limits.timeout.as_secs().saturating_add(5);
    #[cfg(target_os = "linux")]
    let address = limits.memory_mb.saturating_mul(1024 * 1024);
    cmd.process_group(0);
    // SAFETY: only async-signal-safe libc calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            let cpu = libc::rlimit {
                rlim_cur: cpu as libc::rlim_t,
                rlim_max: cpu as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &core) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            {
                let a = libc::rlimit {
                    rlim_cur: address as libc::rlim_t,
                    rlim_max: address as libc::rlim_t,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &a) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn apply_rlimits(_cmd: &mut std::process::Command, _limits: SpawnLimits) {}

/// What the extraction child may read: its input, its own binary, and the
/// system library locations the binary loads from. Missing paths are left
/// out (Landlock refuses a rule for a path that does not exist).
pub fn readable_paths(program: &Path, input: &Path) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    const SYSTEM: &[&str] = &["/usr/lib", "/System", "/dev"];
    #[cfg(not(target_os = "macos"))]
    const SYSTEM: &[&str] = &[
        "/lib",
        "/lib64",
        "/usr/lib",
        "/usr/lib64",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
        "/dev/null",
        "/dev/urandom",
    ];
    let mut out: Vec<PathBuf> = vec![input.to_path_buf(), program.to_path_buf()];
    // Resolve symlinks too: the sandbox checks the real path.
    for p in [input, program] {
        if let Ok(real) = std::fs::canonicalize(p) {
            out.push(real);
        }
    }
    out.extend(SYSTEM.iter().map(PathBuf::from));
    out.retain(|p| p.exists());
    out.dedup();
    out
}

fn confine(cmd: &mut std::process::Command, program: &Path, input: &Path) -> bool {
    let spec = duduclaw_sandbox::SandboxSpec {
        readable_paths: readable_paths(program, input),
        writable_paths: Vec::new(),
        allow_network: false,
        unconfined: false,
    };
    match duduclaw_sandbox::platform_sandbox().confine(cmd, &spec) {
        Ok(duduclaw_sandbox::Confinement::Applied) => true,
        Ok(other) => {
            tracing::warn!(
                ?other,
                "knowledge-extract runs without an OS sandbox on this platform"
            );
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "knowledge-extract sandbox unavailable; running with timeout and limits only");
            false
        }
    }
}

async fn read_capped<R: AsyncRead + Unpin>(mut r: R, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return (out, false),
            Ok(n) => {
                if out.len() + n > cap {
                    return (out, true);
                }
                out.extend_from_slice(&buf[..n]);
            }
        }
    }
}

fn last_line(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()?
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    #[allow(unsafe_code)]
    if let Some(pid) = pid {
        // SAFETY: plain kill(2) on the child's own process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

/// Map a non-zero, non-2 exit to a closed-set error.
fn classify(status: std::process::ExitStatus, stderr: &[u8]) -> ExtractError {
    let text = String::from_utf8_lossy(stderr);
    if text.contains("memory allocation of") || text.contains("out of memory") {
        return oom();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        match status.signal() {
            Some(libc::SIGKILL) => return oom(),
            Some(libc::SIGXCPU) => {
                return ExtractError::new(
                    "extract_timeout",
                    "解析這份文件用掉的運算時間超過上限，已停止。",
                );
            }
            _ => {}
        }
    }
    let _ = status;
    crashed()
}

/// Extract `input` of `format` in a child process.
pub async fn run(
    cmd: &ExtractorCmd,
    format: Format,
    input: &Path,
    limits: SpawnLimits,
) -> Result<Spawned, ExtractError> {
    let mut std_cmd = std::process::Command::new(&cmd.program);
    std_cmd
        .args(&cmd.leading_args)
        .arg("--format")
        .arg(format.as_str())
        .arg("--input")
        .arg(input)
        .arg("--max-chars")
        .arg(limits.max_chars.to_string())
        .env_clear()
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Order matters: the rlimit hook must run before the sandbox hook, which
    // on macOS replaces the process image.
    apply_rlimits(&mut std_cmd, limits);
    let sandboxed = confine(&mut std_cmd, &cmd.program, input);
    if !sandboxed && !limits.allow_unsandboxed {
        return Err(ExtractError::new(
            "sandbox_unavailable",
            "這台主機無法在隔離環境中解析文件，為了安全已拒絕。管理員可在 config.toml 設 [knowledge] extract_when_unavailable = \"run_unsandboxed\" 改為只靠時間與資源上限解析。",
        ));
    }
    let mut child = tokio::process::Command::from(std_cmd)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            tracing::error!(error = %e, "knowledge-extract spawn failed");
            crashed()
        })?;
    let pid = child.id();
    let stdout_cap = limits.max_chars.saturating_mul(4).saturating_add(64 * 1024);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_task = tokio::spawn(async move {
        match stdout {
            Some(s) => read_capped(s, stdout_cap).await,
            None => (Vec::new(), false),
        }
    });
    let err_task = tokio::spawn(async move {
        match stderr {
            Some(s) => read_capped(s, STDERR_CAP).await.0,
            None => Vec::new(),
        }
    });
    let waited = tokio::time::timeout(limits.timeout, child.wait()).await;
    let status = match waited {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            kill_group(pid);
            return Err(crashed());
        }
        Err(_) => {
            kill_group(pid);
            let _ = child.kill().await;
            return Err(timeout(limits.timeout.as_secs()));
        }
    };
    let (stdout, overflow) = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();
    if overflow {
        return Err(ExtractError::too_many_chars(limits.max_chars));
    }
    match status.code() {
        Some(0) => last_line(&stdout)
            .and_then(|l| serde_json::from_str::<ExtractOutput>(l).ok())
            .map(|output| Spawned { output, sandboxed })
            .ok_or_else(crashed),
        Some(2) => Err(last_line(&stderr)
            .and_then(|l| serde_json::from_str::<ExtractError>(l).ok())
            .unwrap_or_else(crashed)),
        _ => Err(classify(status, &stderr)),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> ExtractorCmd {
        ExtractorCmd {
            program: PathBuf::from("/bin/bash"),
            leading_args: vec!["-c".into(), script.into(), "sh".into()],
        }
    }

    fn limits(secs: u64) -> SpawnLimits {
        SpawnLimits {
            timeout: Duration::from_secs(secs),
            memory_mb: 512,
            max_chars: 1_000,
            // These tests drive /bin/sh (shell builtins only, so they also
            // run inside the sandbox) and test process handling.
            allow_unsandboxed: true,
        }
    }

    #[test]
    fn sandbox_reads_only_input_binary_and_system_libraries() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("blob");
        std::fs::write(&input, b"x").unwrap();
        let paths = readable_paths(Path::new("/bin/sh"), &input);
        assert!(paths.contains(&input));
        assert!(paths.contains(&PathBuf::from("/bin/sh")));
        assert!(!paths.contains(&PathBuf::from("/")));
        assert!(
            !paths.iter().any(|p| p == dir.path()),
            "not the whole blob directory"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.starts_with("/Users") || p.starts_with("/home")),
            "{paths:?}"
        );
    }

    #[tokio::test]
    async fn success_parses_last_stdout_line() {
        let out =
            r#"{"segments":[{"kind":"text","text":"hi"}],"warnings":[],"extractor_version":"x"}"#;
        let cmd = sh(&format!("echo noise; echo '{out}'"));
        let r = run(&cmd, Format::Txt, Path::new("/dev/null"), limits(10))
            .await
            .unwrap();
        assert_eq!(r.output.segments[0].text, "hi");
    }

    #[tokio::test]
    async fn expected_failure_uses_stderr_code() {
        let cmd = sh(r#"echo '{"code":"encrypted","message":"m"}' >&2; exit 2"#);
        let e = run(&cmd, Format::Pdf, Path::new("/dev/null"), limits(10))
            .await
            .unwrap_err();
        assert_eq!(e.code, "encrypted");
    }

    #[tokio::test]
    async fn timeout_kills_the_child() {
        let started = std::time::Instant::now();
        let e = run(
            &sh("while :; do :; done"),
            Format::Pdf,
            Path::new("/dev/null"),
            limits(1),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "extract_timeout");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn crash_and_kill_map_to_codes() {
        let e = run(
            &sh("exit 101"),
            Format::Pdf,
            Path::new("/dev/null"),
            limits(10),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "extract_crashed");
        let e = run(
            &sh("kill -9 $$"),
            Format::Pdf,
            Path::new("/dev/null"),
            limits(10),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "extract_oom");
        let e = run(
            &sh("echo 'memory allocation of 99 bytes failed' >&2; kill -6 $$"),
            Format::Pdf,
            Path::new("/dev/null"),
            limits(10),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "extract_oom");
        let e = run(
            &sh("echo not-json"),
            Format::Pdf,
            Path::new("/dev/null"),
            limits(10),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "extract_crashed");
    }

    #[tokio::test]
    async fn output_over_cap_is_refused() {
        let e = run(
            &sh("while :; do echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; done"),
            Format::Txt,
            Path::new("/dev/null"),
            limits(10),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "too_many_chars");
    }
}
