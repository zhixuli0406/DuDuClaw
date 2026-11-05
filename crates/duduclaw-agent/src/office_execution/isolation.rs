//! Fail-closed host sandbox selection. Never retry an executed document unsandboxed.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;
use tokio::process::Command;
use duduclaw_core::process_tree::ManagedChild;
use super::{process, PreparedOffice};

#[derive(Clone, Copy, Debug)]
struct Capabilities { filesystem: bool, network: bool, memory: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision { Isolated, Unsandboxed, Refuse }

fn decide(caps: Capabilities, opt_out: bool) -> Decision {
    if caps.filesystem && caps.network && caps.memory { Decision::Isolated }
    else if opt_out { Decision::Unsandboxed }
    else { Decision::Refuse }
}

// Same key and fail-closed parsing as task_sandbox::settings::script_when_unavailable.
// Do not use the task's `when_unavailable` or employee `sandbox_enabled` switch.
fn opt_out(config: &str) -> bool {
    let Ok(value) = config.parse::<toml::Table>() else { return false; };
    value.get("container").and_then(toml::Value::as_table)
        .and_then(|v| v.get("sandbox")).and_then(toml::Value::as_table)
        .and_then(|v| v.get("script_when_unavailable")).and_then(toml::Value::as_str)
        == Some("run_unsandboxed")
}

pub(super) fn resolve_program(program: &str) -> io::Result<PathBuf> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        // Relative PATH entries would resolve against a different cwd after spawn.
        if !dir.is_absolute() { continue; }
        let path = dir.join(program);
        if let Ok(meta) = std::fs::metadata(&path) {
            #[cfg(unix)]
            let executable = meta.permissions().mode() & 0o111 != 0;
            #[cfg(not(unix))]
            let executable = true;
            if meta.is_file() && executable { return path.canonicalize(); }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("Office runtime {program} not found")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MemoryLimit {
    #[cfg(not(target_os = "macos"))]
    AddressSpace,
    #[cfg(target_os = "macos")]
    Data,
    #[cfg(target_os = "macos")]
    Rss,
}

impl MemoryLimit {
    #[cfg(unix)]
    pub(super) fn install(self, apply: impl FnOnce(MemoryResource) -> io::Result<()>) -> io::Result<()> {
        let resource = match self {
            #[cfg(not(target_os = "macos"))]
            Self::AddressSpace => libc::RLIMIT_AS,
            #[cfg(target_os = "macos")]
            Self::Data => libc::RLIMIT_DATA,
            #[cfg(target_os = "macos")]
            Self::Rss => libc::RLIMIT_RSS,
        };
        apply(resource)
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
type MemoryResource = libc::__rlimit_resource_t;
#[cfg(all(unix, not(all(target_os = "linux", target_env = "gnu"))))]
type MemoryResource = libc::c_int;

pub(super) struct Sandbox {
    wrapper: Option<PathBuf>,
    memory: Option<MemoryLimit>,
    // Keep the profile in an argument, not an attacker-reopenable temporary file.
    profile: Option<String>,
}

impl Sandbox {
    pub(super) fn memory(&self) -> Option<MemoryLimit> { self.memory }

    pub(super) fn is_isolated(&self) -> bool { self.wrapper.is_some() }

    pub(super) fn command(&self, prepared: &PreparedOffice, program: &Path) -> io::Result<Command> {
        let Some(wrapper) = &self.wrapper else { return Ok(Command::new(program)); };
        let mut cmd = Command::new(wrapper);
        if let Some(profile) = &self.profile {
            cmd.arg("-p").arg(profile).arg(program);
        } else {
            let paths = SandboxPaths::resolve(prepared, program, LINUX_RUNTIME_ROOTS)?;
            cmd.args(bwrap_args(&paths)?);
        }
        Ok(cmd)
    }
}

// SBPL strings are not shell strings. Reject non-UTF8/control characters and
// escape BOTH backslash and quote, so paths can never become profile clauses.
fn quote_path(path: &Path) -> io::Result<String> {
    let value = path.to_str().filter(|s| !s.chars().any(char::is_control))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Unsupported sandbox path"))?;
    if !path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Sandbox path must be absolute and normalized"));
    }
    Ok(format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
}

const LINUX_RUNTIME_ROOTS: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64",
    "/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/fonts"];
const MACOS_RUNTIME_ROOTS: &[&str] = &["/System", "/usr", "/bin", "/sbin", "/Library/Frameworks",
    "/Library/Fonts", "/opt/homebrew", "/Applications/LibreOffice.app", "/private/var/db/dyld"];

// Builders consume resolved paths so both platforms can be tested without a sandbox.
struct SandboxPaths {
    home: PathBuf,
    workspace: PathBuf,
    base_cache: PathBuf,
    run_cache: PathBuf,
    script: PathBuf,
    input: PathBuf,
    program: PathBuf,
    // Keep the original destination/allow path AND its canonical target. Both must
    // be disjoint from private state (e.g. /bin may resolve to /usr/bin).
    roots: Vec<(PathBuf, PathBuf)>,
}

impl SandboxPaths {
    fn resolve(prepared: &PreparedOffice, program: &Path, roots: &[&str]) -> io::Result<Self> {
        let mut resolved = Vec::new();
        for root in roots {
            let path = Path::new(root);
            match std::fs::symlink_metadata(path) {
                // Optional absent runtime paths grant nothing. A dangling symlink
                // or any other canonicalization error is NOT an absent root.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
                Ok(_) => resolved.push((path.to_path_buf(), path.canonicalize()?)),
            }
        }
        Ok(Self {
            home: prepared.home.canonicalize()?,
            workspace: prepared.workspace().canonicalize()?,
            base_cache: prepared.base_cache().canonicalize()?,
            run_cache: prepared.run_cache().canonicalize()?,
            script: prepared.script().canonicalize()?,
            input: prepared.input().canonicalize()?,
            program: program.canonicalize()?,
            roots: resolved,
        })
    }

    fn validate(&self) -> io::Result<()> {
        // The private workspace is also the staged output directory. Check each
        // grant separately so a relocated cache/input cannot bypass the home check.
        for private in [&self.home, &self.workspace, &self.base_cache, &self.run_cache, &self.script, &self.input] {
            for (alias, target) in &self.roots {
                for root in [alias, target] {
                    if private.starts_with(root) || root.starts_with(private) {
                        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!(
                            "Office isolation path overlap: private path {} and runtime allow-root {} ({}) must be disjoint",
                            private.display(), alias.display(), target.display())));
                    }
                }
            }
        }
        // The workspace is the ONLY writable host mount/grant. The disposable
        // cache must be strictly inside it, and no writable ancestor may cover
        // the persistent base (nor may the base expose a run through reads).
        if self.workspace.starts_with(&self.base_cache) || self.base_cache.starts_with(&self.workspace)
            || self.run_cache == self.workspace || !self.run_cache.starts_with(&self.workspace)
            || self.script.starts_with(&self.run_cache) || self.input.starts_with(&self.run_cache)
            || !self.script.starts_with(&self.workspace) || !self.input.starts_with(&self.workspace) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied,
                "Office isolation path overlap: base cache must be disjoint from the workspace; run cache must be a private workspace child"));
        }
        Ok(())
    }
}

fn bwrap_args(paths: &SandboxPaths) -> io::Result<Vec<std::ffi::OsString>> {
    paths.validate()?;
    let mut args: Vec<std::ffi::OsString> = ["--unshare-all", "--unshare-net", "--die-with-parent",
        "--new-session", "--cap-drop", "ALL"].into_iter().map(Into::into).collect();
    for (path, _) in &paths.roots {
        args.extend(["--ro-bind".into(), path.as_os_str().into(), path.as_os_str().into()]);
    }
    for (flag, path) in [("--ro-bind", &paths.program), ("--bind", &paths.workspace),
        ("--ro-bind", &paths.base_cache), ("--ro-bind", &paths.script), ("--ro-bind", &paths.input)] {
        args.extend([flag.into(), path.as_os_str().into(), path.as_os_str().into()]);
    }
    args.extend(["--proc".into(), "/proc".into(), "--dev".into(), "/dev".into(),
        "--chdir".into(), paths.workspace.as_os_str().into(), "--".into(), paths.program.as_os_str().into()]);
    Ok(args)
}

fn macos_profile(paths: &SandboxPaths) -> io::Result<String> {
    paths.validate()?;
    let SandboxPaths { workspace, base_cache, script, input, program, .. } = paths;
    let mut profile = String::from("(version 1)\n(deny default)\n(deny network*)\n\
        (allow process-exec process-fork)\n(allow sysctl-read)\n");
    for (path, _) in &paths.roots {
        profile.push_str(&format!("(allow file-read* (subpath {}))\n", quote_path(path)?));
    }
    for path in ["/dev/null", "/dev/random", "/dev/urandom"] {
        profile.push_str(&format!("(allow file-read* (literal {}))\n", quote_path(Path::new(path))?));
    }
    for path in [workspace, base_cache] {
        profile.push_str(&format!("(allow file-read* (subpath {}))\n", quote_path(path)?));
    }
    profile.push_str(&format!("(allow file-read* (literal {}))\n", quote_path(program)?));
    // The input and immutable wrapper stay read-only even inside the private cwd.
    profile.push_str(&format!("(allow file-write* (require-all (subpath {}) (require-not (literal {})) (require-not (literal {}))))\n",
        quote_path(workspace)?, quote_path(script)?, quote_path(input)?));
    Ok(profile)
}

async fn probe(prepared: &PreparedOffice, cmd: Command, memory: Option<MemoryLimit>) -> io::Result<()> {
    let mut cmd = process::command(prepared, cmd);
    process::configure(&mut cmd, memory)?;
    let output = process::collect(ManagedChild::spawn(&mut cmd)?, Duration::from_secs(10)).await?;
    if output.status.success() { Ok(()) }
    else { Err(io::Error::other(format!("probe exited {}: {}", output.status,
        String::from_utf8_lossy(&output.stderr)))) }
}

#[cfg(target_os = "macos")]
async fn memory_probe(prepared: &PreparedOffice) -> io::Result<MemoryLimit> {
    // Darwin may accept RSS/DATA without bounding mmap-backed allocations.
    // Retain the exact backend that passed; never substitute one at launch.
    let python = resolve_program(duduclaw_core::platform::python3_command())?;
    let mut failure = io::Error::other("no effective DATA/RSS memory bound");
    for (memory, name) in [(MemoryLimit::Data, "DATA"), (MemoryLimit::Rss, "RSS")] {
        let mut cmd = Command::new(&python);
        cmd.args(["-I", "-c", r#"import resource, sys
kind = getattr(resource, 'RLIMIT_' + sys.argv[1])
soft, hard = resource.getrlimit(kind)
if not (0 <= soft <= 2 * 1024**3 and 0 <= hard <= 2 * 1024**3):
    raise RuntimeError('no installed memory bound')
resource.setrlimit(kind, (min(soft, 32 * 1024**2), min(hard, 32 * 1024**2)))
try:
    data = bytearray(64 * 1024**2)
except MemoryError:
    sys.exit(0)
raise RuntimeError('Darwin accepted the memory rlimit but did not enforce allocation denial')
"#, name]);
        match probe(prepared, cmd, Some(memory)).await {
            Ok(()) => return Ok(memory),
            Err(error) => failure = error,
        }
    }
    Err(failure)
}

#[cfg(not(target_os = "macos"))]
async fn memory_probe(prepared: &PreparedOffice) -> io::Result<MemoryLimit> {
    probe(prepared, Command::new("/usr/bin/true"), Some(MemoryLimit::AddressSpace)).await?;
    Ok(MemoryLimit::AddressSpace)
}

pub(super) async fn select(prepared: &PreparedOffice, program: &Path) -> io::Result<Sandbox> {
    let candidate: io::Result<Sandbox> = (|| {
        if cfg!(target_os = "macos") {
            let paths = SandboxPaths::resolve(prepared, program, MACOS_RUNTIME_ROOTS)?;
            Ok(Sandbox { memory: None, wrapper: Some(PathBuf::from("/usr/bin/sandbox-exec")),
                profile: Some(macos_profile(&paths)?) })
        } else if cfg!(target_os = "linux") {
            let paths = SandboxPaths::resolve(prepared, program, LINUX_RUNTIME_ROOTS)?;
            bwrap_args(&paths)?;
            Ok(Sandbox { memory: None, wrapper: Some(resolve_program("bwrap").unwrap_or_else(|_| PathBuf::from("/usr/bin/bwrap"))), profile: None })
        } else {
            Err(io::Error::new(io::ErrorKind::Unsupported, "Office isolation requires Linux or macOS"))
        }
    })();
    let filesystem_network = match &candidate {
        Ok(candidate) => match candidate.command(prepared, Path::new("/usr/bin/true")) {
            Ok(cmd) => probe(prepared, cmd, None).await,
            Err(error) => Err(error),
        },
        Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
    };
    let memory = memory_probe(prepared).await;
    let caps = Capabilities { filesystem: filesystem_network.is_ok(), network: filesystem_network.is_ok(), memory: memory.is_ok() };
    let config = std::fs::read_to_string(prepared.home.join("config.toml")).unwrap_or_default();
    match decide(caps, opt_out(&config)) {
        Decision::Isolated => Ok(Sandbox { memory: Some(memory?), ..candidate? }),
        decision => {
            let reason = format!("filesystem/network: {}; memory: {}",
                filesystem_network.err().map(|e| e.to_string()).unwrap_or_else(|| "available".into()),
                memory.err().map(|e| e.to_string()).unwrap_or_else(|| "available".into()));
            if decision == Decision::Refuse {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!(
                    "office_script refused: full isolation unavailable ({reason}). Requires filesystem/network sandbox and memory bound; config.toml [container.sandbox] script_when_unavailable = \"run_unsandboxed\" is the explicit operator opt-out.")));
            }
            static WARN: Once = Once::new();
            WARN.call_once(|| tracing::warn!(event = "office_script_unsandboxed", %reason,
                "office_script running UNSANDBOXED: [container.sandbox] script_when_unavailable = run_unsandboxed; filesystem/network/memory isolation is not guaranteed"));
            Ok(Sandbox { wrapper: None, memory: None, profile: None })
        }
    }
}

#[cfg(test)]
mod tests;
