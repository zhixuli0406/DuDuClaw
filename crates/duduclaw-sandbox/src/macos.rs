//! macOS confinement via Seatbelt (`sandbox-exec` + a generated SBPL profile).
//!
//! Custom SBPL profiles can only be applied to a process via `sandbox-exec`
//! (the public `sandbox_init` API accepts *named* profiles only). Since
//! [`std::process::Command`] cannot have its program rewritten in place, we keep
//! the trait's `&mut Command` seam and re-point the child at `sandbox-exec`
//! **inside a `pre_exec` closure**: after `fork`, before the original `exec`,
//! we `execve("/usr/bin/sandbox-exec", ["sandbox-exec","-f",<profile>,
//! <orig-program>,<orig-args>...], <child-env>)`. On success the image is
//! replaced by `sandbox-exec`, which applies the profile and execs the original
//! program under Seatbelt; the original `exec` therefore never runs unconfined.
//! If the `execve` fails, the closure returns an error and the child aborts
//! before running the original program — **fail-closed**.

use crate::{Availability, Confinement, NativeSandbox, SandboxError, SandboxSpec};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub struct MacosSandbox;

impl MacosSandbox {
    pub fn new() -> Self {
        MacosSandbox
    }
}

impl NativeSandbox for MacosSandbox {
    fn confine(
        &self,
        cmd: &mut Command,
        spec: &SandboxSpec,
    ) -> Result<Confinement, SandboxError> {
        if spec.unconfined {
            return Ok(Confinement::Skipped);
        }
        if !Path::new(SANDBOX_EXEC).exists() {
            // No Seatbelt tool → cannot enforce → fail-closed for a required
            // sandbox. The caller treats Refused as "do not spawn".
            return Ok(Confinement::Refused);
        }

        // 1) Render + persist the SBPL profile (0600, agent-scoped tmp dir).
        //    Canonicalize paths first: Seatbelt resolves symlinks, and on macOS
        //    `/var` and `/tmp` are symlinks to `/private/...`. A subpath written
        //    with the un-resolved form would never match the real path, silently
        //    denying writes inside the workspace.
        let canon = canonicalize_spec(spec);
        let profile = render_sbpl_profile(&canon);
        let profile_path = write_profile(&profile)?;

        // 2) Capture the original program + args and the resolved child env
        //    BEFORE fork (all allocation happens here, never in `pre_exec`).
        let wrapped_argv = build_wrapped_argv(cmd, &profile_path)?;
        let envp = build_envp(cmd)?;
        let plan = ExecPlan::new(wrapped_argv, envp)?;

        // 3) Install the post-fork re-exec. SAFETY: the closure performs only
        //    async-signal-safe work — a single `libc::execve` with pointer
        //    arrays that were fully materialised before `fork` (no allocation,
        //    no locks, no Rust `Vec` growth inside the child). `errno` read via
        //    `io::Error::last_os_error` does not allocate. On `execve` success
        //    the closure never returns; on failure it returns an error and the
        //    child aborts (fail-closed).
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(move || {
                let rc = libc::execve(plan.path(), plan.argv_ptr(), plan.envp_ptr());
                debug_assert_eq!(rc, -1, "execve only returns on failure");
                let _ = rc;
                Err(std::io::Error::last_os_error())
            });
        }
        Ok(Confinement::Applied)
    }

    fn availability(&self) -> Availability {
        if Path::new(SANDBOX_EXEC).exists() {
            Availability::Enforcing
        } else {
            Availability::Unsupported
        }
    }
}

/// Resolve a spec's paths to their canonical (symlink-free) form so SBPL
/// `subpath` values match the paths Seatbelt actually sees. Paths that cannot be
/// canonicalized (e.g. not yet created) are kept as-is.
fn canonicalize_spec(spec: &SandboxSpec) -> SandboxSpec {
    fn canon(paths: &[PathBuf]) -> Vec<PathBuf> {
        paths
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect()
    }
    SandboxSpec {
        readable_paths: canon(&spec.readable_paths),
        writable_paths: canon(&spec.writable_paths),
        allow_network: spec.allow_network,
        unconfined: spec.unconfined,
    }
}

/// Escape a path for embedding inside an SBPL `(subpath "...")` string literal.
/// Backslash MUST be escaped before the double-quote so `\` in the path does not
/// swallow a following escaped quote.
pub(crate) fn escape_sbpl_subpath(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render a deterministic SBPL profile (`deny default` allowlist) for `spec`.
///
/// - `(allow process*)` so the sandboxed target can exec/fork.
/// - `(allow file-read* (subpath ...))` for each readable subtree — the coarse
///   [`crate::SandboxLevel`] model confines *writes*, and the dynamic linker
///   needs to read system libraries for the target to start at all.
/// - `(allow file-write* (subpath ...))` for each writable subtree.
/// - `(allow network*)` only when `spec.allow_network` (default deny otherwise).
pub(crate) fn render_sbpl_profile(spec: &SandboxSpec) -> String {
    let mut p = String::new();
    p.push_str("(version 1)\n");
    p.push_str("(deny default)\n");
    p.push_str("(allow process*)\n");
    p.push_str("(allow sysctl-read)\n");
    p.push_str("(allow mach-lookup)\n");
    for r in &spec.readable_paths {
        p.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl_subpath(r)
        ));
    }
    for w in &spec.writable_paths {
        p.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            escape_sbpl_subpath(w)
        ));
    }
    if spec.allow_network {
        p.push_str("(allow network*)\n");
    }
    p
}

/// Directory holding generated SBPL profiles (0700, under the system temp dir).
fn profile_dir() -> PathBuf {
    std::env::temp_dir().join("duduclaw-sandbox")
}

/// Write `profile` to a fresh 0600 file and best-effort prune stale ones.
fn write_profile(profile: &str) -> Result<PathBuf, SandboxError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let dir = profile_dir();
    std::fs::create_dir_all(&dir)?;
    // Keep the profile dir private (0700). Best-effort.
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    prune_stale_profiles(&dir);

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Monotonic counter guarantees uniqueness even when two confinements land in
    // the same nanosecond within one process (parallel spawns / tests).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("sbpl-{}-{}-{}.sb", std::process::id(), stamp, seq));

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    f.write_all(profile.as_bytes())?;
    f.flush()?;
    Ok(path)
}

/// Remove profile files older than one hour (best-effort; ignores errors).
fn prune_stale_profiles(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sb") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = now.duration_since(modified) {
                    if age.as_secs() > 3600 {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

/// Build the `sandbox-exec` wrapper argv as C strings (NUL-checked).
fn build_wrapped_argv(cmd: &Command, profile_path: &Path) -> Result<Vec<CString>, SandboxError> {
    let mut argv: Vec<CString> = Vec::new();
    argv.push(cstr_from_bytes(SANDBOX_EXEC.as_bytes())?);
    argv.push(cstr_from_bytes(b"-f")?);
    argv.push(cstr_from_bytes(profile_path.as_os_str().as_bytes())?);
    argv.push(cstr_from_bytes(cmd.get_program().as_bytes())?);
    for a in cmd.get_args() {
        argv.push(cstr_from_bytes(a.as_bytes())?);
    }
    Ok(argv)
}

/// Resolve the child environment (inherited parent env + the command's
/// overrides) into `KEY=VALUE` C strings. `pre_exec`'s `execve` bypasses std's
/// own env application, so we reproduce it here.
fn build_envp(cmd: &Command) -> Result<Vec<CString>, SandboxError> {
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};

    let mut env: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (k, v) in cmd.get_envs() {
        match v {
            Some(val) => {
                env.insert(k.to_os_string(), val.to_os_string());
            }
            None => {
                env.remove(k);
            }
        }
    }
    let mut out = Vec::with_capacity(env.len());
    for (k, v) in env {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(<OsStr as OsStrExt>::as_bytes(&k));
        bytes.push(b'=');
        bytes.extend_from_slice(<OsStr as OsStrExt>::as_bytes(&v));
        out.push(cstr_from_bytes(&bytes)?);
    }
    Ok(out)
}

fn cstr_from_bytes(bytes: &[u8]) -> Result<CString, SandboxError> {
    CString::new(bytes)
        .map_err(|_| SandboxError::Profile("argument contains interior NUL byte".into()))
}

/// Owns the C-string storage plus pre-computed NUL-terminated pointer arrays for
/// `execve`. Built entirely in the parent before `fork`; the child inherits the
/// identical address space, so the raw pointers stay valid after `fork`.
struct ExecPlan {
    // Storage kept alive for the lifetime of the plan; pointers below borrow it.
    _argv_storage: Vec<CString>,
    _envp_storage: Vec<CString>,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
}

// SAFETY: the raw pointers reference heap buffers owned by `_argv_storage` /
// `_envp_storage` (also moved into this struct). After `fork` the child has a
// byte-identical copy of this address space, so the pointers remain valid. The
// struct is only ever read (never mutated) after construction.
unsafe impl Send for ExecPlan {}
unsafe impl Sync for ExecPlan {}

impl ExecPlan {
    fn new(argv_storage: Vec<CString>, envp_storage: Vec<CString>) -> Result<Self, SandboxError> {
        if argv_storage.is_empty() {
            return Err(SandboxError::Profile("empty argv".into()));
        }
        let mut argv: Vec<*const libc::c_char> =
            argv_storage.iter().map(|c| c.as_ptr()).collect();
        argv.push(std::ptr::null());
        let mut envp: Vec<*const libc::c_char> =
            envp_storage.iter().map(|c| c.as_ptr()).collect();
        envp.push(std::ptr::null());
        Ok(ExecPlan {
            _argv_storage: argv_storage,
            _envp_storage: envp_storage,
            argv,
            envp,
        })
    }

    fn path(&self) -> *const libc::c_char {
        // argv[0] is the sandbox-exec path.
        self.argv[0]
    }
    fn argv_ptr(&self) -> *const *const libc::c_char {
        self.argv.as_ptr()
    }
    fn envp_ptr(&self) -> *const *const libc::c_char {
        self.envp.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SandboxLevel;

    fn spec_with(writable: &[&str], allow_network: bool) -> SandboxSpec {
        SandboxSpec {
            readable_paths: vec![PathBuf::from("/")],
            writable_paths: writable.iter().map(PathBuf::from).collect(),
            allow_network,
            unconfined: false,
        }
    }

    #[test]
    fn profile_denies_by_default_and_allows_process() {
        let p = render_sbpl_profile(&spec_with(&["/tmp/ws"], true));
        assert!(p.contains("(version 1)"));
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow process*)"));
        assert!(p.contains("(allow file-read* (subpath \"/\"))"));
        assert!(p.contains("(allow file-write* (subpath \"/tmp/ws\"))"));
    }

    #[test]
    fn profile_omits_network_when_denied() {
        let denied = render_sbpl_profile(&spec_with(&["/tmp/ws"], false));
        assert!(!denied.contains("(allow network*)"));
        let allowed = render_sbpl_profile(&spec_with(&["/tmp/ws"], true));
        assert!(allowed.contains("(allow network*)"));
    }

    #[test]
    fn subpath_escaping_handles_quote_and_backslash() {
        // A path with a backslash and a double-quote must be escaped so the SBPL
        // string literal stays well-formed.
        let weird = PathBuf::from(r#"/tmp/a\b"c"#);
        let esc = escape_sbpl_subpath(&weird);
        // backslash doubled, quote backslash-escaped
        assert_eq!(esc, r#"/tmp/a\\b\"c"#);
        let p = render_sbpl_profile(&SandboxSpec {
            readable_paths: vec![PathBuf::from("/")],
            writable_paths: vec![weird],
            allow_network: false,
            unconfined: false,
        });
        assert!(p.contains(r#"(allow file-write* (subpath "/tmp/a\\b\"c"))"#));
    }

    #[test]
    fn availability_is_enforcing_on_this_mac() {
        assert_eq!(MacosSandbox::new().availability(), Availability::Enforcing);
    }

    #[test]
    fn full_access_spec_skips() {
        let sb = MacosSandbox::new();
        let spec = SandboxSpec::from_level(SandboxLevel::FullAccess, Path::new("/tmp"));
        let mut cmd = Command::new("true");
        assert_eq!(sb.confine(&mut cmd, &spec).unwrap(), Confinement::Skipped);
    }

    // ── Live escape tests (real Seatbelt on this macOS box) ─────────────

    /// Direct `sandbox-exec` run: a generated profile must block a write to a
    /// path OUTSIDE `writable_paths` while the process still starts.
    #[test]
    fn live_sandbox_exec_blocks_write_outside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize so the `/var`→`/private/var` symlink does not defeat the
        // Seatbelt subpath match (production goes through `confine`, which
        // canonicalizes internally).
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let allowed = root.join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let outside = root.join("outside.txt");

        let spec = SandboxSpec {
            readable_paths: vec![PathBuf::from("/")],
            writable_paths: vec![allowed.clone()],
            allow_network: true,
            unconfined: false,
        };
        let profile = render_sbpl_profile(&spec);
        let profile_path = write_profile(&profile).unwrap();

        let status = Command::new(SANDBOX_EXEC)
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo x > {}", outside.display()))
            .status()
            .unwrap();

        assert!(
            !status.success(),
            "write outside workspace should be denied by Seatbelt"
        );
        assert!(
            !outside.exists(),
            "the outside file must not have been created"
        );

        // Sanity: writing INSIDE the workspace under the same profile succeeds.
        let inside = allowed.join("inside.txt");
        let ok = Command::new(SANDBOX_EXEC)
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo x > {}", inside.display()))
            .status()
            .unwrap();
        assert!(ok.success(), "write inside workspace should be allowed");
        assert!(inside.exists());
    }

    /// End-to-end `confine` path: build a plain `Command`, confine it with a
    /// spec whose writable set does NOT include the target, spawn, and assert
    /// the write was blocked by the `pre_exec` → `sandbox-exec` wrapper.
    #[test]
    fn live_confine_blocks_write_via_pre_exec_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let outside = dir.path().join("blocked.txt");

        let spec = SandboxSpec {
            readable_paths: vec![PathBuf::from("/")],
            writable_paths: vec![allowed.clone()],
            allow_network: true,
            unconfined: false,
        };

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!("echo x > {}", outside.display()));

        let sb = MacosSandbox::new();
        let confinement = sb.confine(&mut cmd, &spec).unwrap();
        assert_eq!(confinement, Confinement::Applied);

        let status = cmd.status().unwrap();
        assert!(
            !status.success(),
            "confined command should fail to write outside workspace"
        );
        assert!(!outside.exists(), "outside file must not exist");
    }
}
