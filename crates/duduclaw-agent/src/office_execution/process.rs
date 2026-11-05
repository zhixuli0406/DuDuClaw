//! Bounded office subprocess execution, shared with the MCP entry point.
use std::io;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use duduclaw_core::process_tree::ManagedChild;
use super::{isolation, manifest, PreparedOffice};

pub const MAX_PIPE_BYTES: usize = 32_768;
const TIMEOUT: Duration = Duration::from_secs(180);

// The fallback must not silently bypass the PEP 723 direct dependency pins.
const PYTHON_BOOTSTRAP: &str = r#"import importlib.metadata, json, runpy, sys
script, pins = sys.argv[1], json.loads(sys.argv[2])
for pin in pins:
    name, expected = pin.split('==')
    actual = importlib.metadata.version(name)
    if actual != expected:
        raise RuntimeError(f'Office dependency {name}: expected {expected}, found {actual}; install uv or the exact pinned dependencies')
sys.argv = [script] + sys.argv[3:]
runpy.run_path(script, run_name='__main__')
"#;

pub struct OfficeOutput {
    pub used: &'static str,
    pub output: Output,
}

pub(super) fn command(prepared: &PreparedOffice, cmd: Command) -> Command {
    command_at(prepared.workspace(), prepared.run_cache(), cmd)
}

fn command_at(workspace: &std::path::Path, cache: &std::path::Path, mut cmd: Command) -> Command {
    cmd.current_dir(workspace).env_clear()
        .env("HOME", workspace).env("TMPDIR", workspace)
        .env("UV_CACHE_DIR", cache).env("PYTHONNOUSERSITE", "1")
        .env("UV_NO_CONFIG", "1").env("UV_NO_BUILD", "1")
        .env("UV_PYTHON_DOWNLOADS", "never")
        // Bound uv's worker pools on large hosts as well as its child processes.
        .env("UV_CONCURRENT_DOWNLOADS", "2").env("UV_CONCURRENT_INSTALLS", "2")
        .env("RAYON_NUM_THREADS", "2");
    if let Some(path) = std::env::var_os("PATH") { cmd.env("PATH", path); }
    cmd
}

fn uv_args(cmd: &mut Command) {
    cmd.args(["--no-config", "run", "--no-project", "--no-env-file", "--no-build",
        "--no-python-downloads", "--exclude-newer", manifest::EXCLUDE_NEWER, "--script"]);
}

#[cfg(unix)]
pub(super) async fn warm_base(prepared: &PreparedOffice, uv: &std::path::Path) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    // A separate host workspace contains only compiled, verified wrapper bytes.
    // No user file, argument, HOME, cwd or input snapshot is passed to this step.
    let warm = tempfile::Builder::new().prefix("warm-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(prepared.base_cache().parent().unwrap())?;
    let dir = super::SafeDir::open(warm.path())?;
    super::private(&dir)?;
    let bytes = manifest::embedded_script(&prepared.skill, &prepared.stem)?;
    let mut script = dir.create_new("script.py", 0o500)?;
    script.write_all(bytes)?;
    script.sync_all()?;
    let mut cmd = command_at(warm.path(), prepared.base_cache(), Command::new(uv));
    cmd.env("UV_LINK_MODE", "copy");
    uv_args(&mut cmd);
    cmd.arg(warm.path().join("script.py")).arg("--help");
    configure(&mut cmd, None)?;
    let output = collect(ManagedChild::spawn(&mut cmd)?, TIMEOUT).await?;
    if !output.status.success() {
        return Err(io::Error::other(format!("Office dependency warm failed ({}): {}",
            output.status, String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}

/// No retry after script execution starts. Only an absent uv binary selects Python.
pub async fn run(prepared: &PreparedOffice) -> io::Result<OfficeOutput> {
    // Resolve before wrapping: a wrapper's ENOENT must never select a fallback.
    let uv = isolation::resolve_program("uv");
    let (used, program) = match uv {
        Ok(path) => ("uv run", path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let py = duduclaw_core::platform::python3_command();
            (py, isolation::resolve_program(py)?)
        }
        Err(error) => return Err(error),
    };
    let sandbox = isolation::select(prepared, &program).await?;
    if used == "uv run" && sandbox.is_isolated() {
        prepared.seed_cache(&program).await?;
    }
    let mut cmd = command(prepared, sandbox.command(prepared, &program)?);
    if sandbox.is_isolated() {
        cmd.env("UV_LINK_MODE", "copy");
    } else {
        // The explicit opt-out retains its legacy cache/download behavior, but
        // that parser-writable cache is never trusted by the isolated path.
        cmd.env("UV_CACHE_DIR", prepared.cache());
    }
    if used == "uv run" {
        uv_args(&mut cmd);
        cmd.arg(prepared.script()).args(prepared.args());
        if sandbox.is_isolated() || prepared.cache_is_warm()? { cmd.env("UV_OFFLINE", "1"); }
    } else {
        cmd.args(["-I", "-c", PYTHON_BOOTSTRAP]).arg(prepared.script())
            .arg(serde_json::to_string(&prepared.dependency_pins()?).map_err(io::Error::other)?)
            .args(prepared.args());
    }
    configure(&mut cmd, sandbox.memory())?;
    let verified = prepared.verify_before_spawn()?;
    let child = ManagedChild::spawn(&mut cmd).map_err(|error|
        io::Error::new(error.kind(), format!("office_script isolation/launch failed: {error}")))?;
    drop(verified);
    let output = collect(child, TIMEOUT).await?;
    if used == "uv run" && !sandbox.is_isolated() && output.status.success() { prepared.mark_cache_warm()?; }
    Ok(OfficeOutput { used, output })
}

/// Only async-signal-safe syscalls run after fork. Limits are reinstalled on
/// the actual launch, not just the probe. ManagedChild installs the process group.
#[cfg(unix)]
pub(super) fn configure(cmd: &mut Command, memory: Option<isolation::MemoryLimit>) -> io::Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(move || {
            macro_rules! limit {
                ($resource:expr, $ceiling:expr) => {{
                    let mut existing = std::mem::MaybeUninit::<libc::rlimit>::uninit();
                    if libc::getrlimit($resource, existing.as_mut_ptr()) != 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        let existing = existing.assume_init();
                        let cap = ($ceiling as libc::rlim_t).min(existing.rlim_cur).min(existing.rlim_max);
                        let bound = libc::rlimit { rlim_cur: cap, rlim_max: cap };
                        if libc::setrlimit($resource, &bound) == 0 { Ok(()) }
                        else { Err(io::Error::last_os_error()) }
                    }
                }};
            }
            // A probe-selected backend must not change at launch. A new
            // installation failure refuses spawn even if another limit might work.
            if let Some(memory) = memory {
                memory.install(|resource| limit!(resource, 2 * 1024 * 1024 * 1024_u64))?;
            } else {
                // Explicitly unsandboxed/probe fixtures retain best-effort memory
                // limits; no availability decision is inferred from this branch.
                #[cfg(target_os = "macos")]
                let _ = limit!(libc::RLIMIT_DATA, 2 * 1024 * 1024 * 1024_u64)
                    .or_else(|_| limit!(libc::RLIMIT_RSS, 2 * 1024 * 1024 * 1024_u64));
                #[cfg(not(target_os = "macos"))]
                let _ = limit!(libc::RLIMIT_AS, 2 * 1024 * 1024 * 1024_u64);
            }
            for (resource, ceiling) in [
                (libc::RLIMIT_CPU, 120_u64),
                (libc::RLIMIT_FSIZE, 256 * 1024 * 1024),
                (libc::RLIMIT_NPROC, 512),
                (libc::RLIMIT_NOFILE, 256),
                (libc::RLIMIT_CORE, 0),
            ] { limit!(resource, ceiling)?; }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn configure(_: &mut Command, _: Option<isolation::MemoryLimit>) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Secure office execution requires Unix"))
}

async fn read_bounded(mut pipe: impl AsyncRead + Unpin, name: &str) -> io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(MAX_PIPE_BYTES);
    let mut chunk = [0; 4096];
    loop {
        let count = pipe.read(&mut chunk).await?;
        if count == 0 { return Ok(output); }
        if count > MAX_PIPE_BYTES - output.len() {
            return Err(io::Error::other(format!("Office {name} exceeded {MAX_PIPE_BYTES} bytes")));
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

pub(super) async fn collect(mut child: ManagedChild, timeout: Duration) -> io::Result<Output> {
    let stdout = child.child().stdout.take().ok_or_else(|| io::Error::other("Missing stdout pipe"))?;
    let stderr = child.child().stderr.take().ok_or_else(|| io::Error::other("Missing stderr pipe"))?;
    let result = tokio::time::timeout(timeout, async {
        let status = async {
            let status = child.child().wait().await?;
            // A descendant must not retain the pipes or keep working after its leader exits.
            child.leader_exited();
            Ok::<_, io::Error>(status)
        };
        let (stdout, stderr, status) = tokio::try_join!(
            read_bounded(stdout, "stdout"), read_bounded(stderr, "stderr"), status)?;
        Ok::<_, io::Error>(Output { status, stdout, stderr })
    }).await;
    match result {
        Ok(Ok(output)) => { child.disarm(); Ok(output) }
        failure => {
            child.kill_and_reap().await;
            match failure {
                Ok(Err(error)) => Err(error),
                Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut,
                    format!("Office script timed out after {}s", timeout.as_secs()))),
                Ok(Ok(_)) => unreachable!(),
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
