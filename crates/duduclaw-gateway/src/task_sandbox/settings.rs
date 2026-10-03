//! `config.toml [container.sandbox]` — operator settings of the task sandbox.
//!
//! Read fresh on every sandboxed dispatch (no cache), so an edit takes effect
//! on the next task. Validation is strict: an unknown key, a wrong type or an
//! out-of-range value makes the whole section invalid, and an invalid section
//! means the sandbox is unavailable (fail closed) — never "use the defaults".
//! The exceptions are the two escape hatches, `when_unavailable` (task
//! sandbox) and `script_when_unavailable` (script sandbox: PTC
//! `execute_program`), each read on its own so it keeps working while
//! another key is being fixed. They are deliberately separate keys: letting
//! delegated tasks run unisolated must not also let arbitrary scripts run on
//! the host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::discovery::attempt_adapter::RuntimeFamily;

/// Every family the sandbox can run, in a fixed order.
pub const FAMILIES: [RuntimeFamily; 6] = [
    RuntimeFamily::Claude,
    RuntimeFamily::Codex,
    RuntimeFamily::Gemini,
    RuntimeFamily::Antigravity,
    RuntimeFamily::Grok,
    RuntimeFamily::OpenAiCompat,
];

/// What happens when `sandbox_enabled = true` but the sandbox cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhenUnavailable {
    /// The task fails (default).
    Fail,
    /// The task runs on the host without isolation; audited every time.
    RunUnsandboxed,
}

impl WhenUnavailable {
    /// Unknown or missing ⇒ `Fail`.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("run_unsandboxed") => Self::RunUnsandboxed,
            _ => Self::Fail,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fail => "fail",
            Self::RunUnsandboxed => "run_unsandboxed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSettings {
    pub image: String,
    /// In-image executable per family (keyed by [`RuntimeFamily::name`]).
    pub executables: BTreeMap<&'static str, PathBuf>,
    pub memory_bytes: u64,
    pub pids: u32,
    pub cpu_millis: u32,
    pub tmp_bytes: u64,
    /// Size cap of the per-task workspace (a tmpfs, discarded with the task).
    pub workspace_bytes: u64,
    pub max_turns: u32,
    pub when_unavailable: WhenUnavailable,
}

/// `ghcr.io/zhixuli0406/duduclaw:v<gateway version>` — the platform's own
/// published image, which carries all five CLIs and `python3`. Shared with
/// the script sandbox through [`duduclaw_core::sandbox_image::platform_image`].
pub fn default_image() -> String {
    duduclaw_core::sandbox_image::platform_image(env!("CARGO_PKG_VERSION"))
}

/// Default in-image location of each family's executable.
pub fn default_executable(family: RuntimeFamily) -> &'static str {
    match family {
        RuntimeFamily::Claude => "/usr/bin/claude",
        RuntimeFamily::Codex => "/usr/bin/codex",
        RuntimeFamily::Gemini => "/usr/bin/gemini",
        RuntimeFamily::Antigravity => "/usr/local/bin/agy",
        RuntimeFamily::Grok => "/usr/local/bin/grok",
        RuntimeFamily::OpenAiCompat => "/usr/local/bin/python3",
    }
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            image: default_image(),
            executables: FAMILIES
                .iter()
                .map(|f| (f.name(), PathBuf::from(default_executable(*f))))
                .collect(),
            memory_bytes: 4 * 1024 * 1024 * 1024,
            pids: 128,
            cpu_millis: 1000,
            tmp_bytes: 256 * 1024 * 1024,
            workspace_bytes: 512 * 1024 * 1024,
            max_turns: 30,
            when_unavailable: WhenUnavailable::Fail,
        }
    }
}

impl SandboxSettings {
    pub fn executable(&self, family: RuntimeFamily) -> &Path {
        // Every family is populated by `Default` and only overridden by parse.
        self.executables
            .get(family.name())
            .map(PathBuf::as_path)
            .unwrap_or_else(|| Path::new(default_executable(family)))
    }
}

/// Upper bounds that stop an obvious typo (an extra zero) from becoming a
/// resource grant.
const MAX_TURNS_CEILING: u32 = 1000;
const MAX_PIDS: u32 = 65_536;
const MAX_CPU_MILLIS: u32 = 256_000;
/// 64 GiB: no sandboxed CLI task needs more, and a larger value is a typo.
const MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// 16 GiB each for the two tmpfs mounts (`/tmp` and the workspace).
const MAX_TMP_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_WORKSPACE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

const KNOWN_KEYS: &[&str] = &[
    "image", "executables", "memory_bytes", "pids", "cpu_millis", "tmp_bytes", "workspace_bytes",
    "max_turns", "when_unavailable", "script_when_unavailable",
];

/// The `[container.sandbox]` table of a parsed `config.toml`, if present.
fn section(config: &toml::Table) -> Result<Option<&toml::Table>, String> {
    match config.get("container") {
        None => Ok(None),
        Some(toml::Value::Table(container)) => match container.get("sandbox") {
            None => Ok(None),
            Some(toml::Value::Table(sandbox)) => Ok(Some(sandbox)),
            Some(_) => Err("[container.sandbox] must be a table".into()),
        },
        Some(_) => Err("[container] must be a table".into()),
    }
}

/// One escape-hatch key alone. Anything but `"run_unsandboxed"` ⇒ `Fail`,
/// including a malformed `[container]` / `[container.sandbox]`.
fn escape_hatch(config: &toml::Table, key: &str) -> WhenUnavailable {
    let value = section(config).ok().flatten().and_then(|s| s.get(key)).and_then(toml::Value::as_str);
    WhenUnavailable::parse(value)
}

/// `when_unavailable` alone: the **task** sandbox's escape hatch.
pub fn when_unavailable(config: &toml::Table) -> WhenUnavailable {
    escape_hatch(config, "when_unavailable")
}

/// `script_when_unavailable` alone: the **script** sandbox's escape hatch
/// (PTC `execute_program`). Independent of `when_unavailable`.
pub fn script_when_unavailable(config: &toml::Table) -> WhenUnavailable {
    escape_hatch(config, "script_when_unavailable")
}

fn positive_u64(table: &toml::Table, key: &str, default: u64) -> Result<u64, String> {
    match table.get(key) {
        None => Ok(default),
        Some(toml::Value::Integer(n)) if *n > 0 => Ok(*n as u64),
        Some(_) => Err(format!("[container.sandbox] {key} must be a positive integer")),
    }
}

fn bounded_u64(table: &toml::Table, key: &str, default: u64, max: u64) -> Result<u64, String> {
    let value = positive_u64(table, key, default)?;
    if value > max {
        return Err(format!("[container.sandbox] {key} must be at most {max}"));
    }
    Ok(value)
}

fn bounded_u32(table: &toml::Table, key: &str, default: u32, max: u32) -> Result<u32, String> {
    let value = positive_u64(table, key, u64::from(default))?;
    u32::try_from(value)
        .ok()
        .filter(|v| *v <= max)
        .ok_or_else(|| format!("[container.sandbox] {key} must be at most {max}"))
}

/// An image reference: non-empty, at most 256 bytes, no whitespace, no NUL,
/// and never something docker could read as an option.
pub fn valid_image(image: &str) -> bool {
    duduclaw_core::sandbox_image::valid_image(image)
}

/// An in-image executable: absolute, no `..` component, no NUL, no comma or
/// line break, at most 256 bytes.
///
/// The path names a file inside the (Linux) sandbox image, so it is judged
/// with POSIX rules on every host: `Path::is_absolute` on a Windows gateway
/// would call `/usr/bin/claude` relative (no drive letter) and refuse every
/// valid `[container.sandbox.executables]` entry.
pub fn valid_executable(path: &Path) -> bool {
    let Some(text) = path.to_str() else { return false };
    text.starts_with('/')
        && text.len() <= 256
        && !text.bytes().any(|c| matches!(c, 0 | b',' | b'\n' | b'\r'))
        && !text.split('/').any(|segment| segment == "..")
}

/// Parse `config.toml` (already parsed as a table). A missing section is the
/// default configuration; anything malformed is an error.
pub fn parse(config: &toml::Table) -> Result<SandboxSettings, String> {
    let mut out = SandboxSettings::default();
    let Some(table) = section(config)? else { return Ok(out) };
    if let Some(unknown) = table.keys().find(|k| !KNOWN_KEYS.contains(&k.as_str())) {
        return Err(format!("[container.sandbox] has an unknown key `{unknown}`"));
    }
    // An unknown value is the safe mode ("fail"), not an invalid section: a
    // typo must not take the sandbox down, and it can never widen to bypass.
    out.when_unavailable = when_unavailable(config);
    for key in ["when_unavailable", "script_when_unavailable"] {
        if table.get(key).is_some_and(|v| !matches!(v.as_str(), Some("fail" | "run_unsandboxed"))) {
            tracing::warn!("[container.sandbox] {key} is not \"fail\" or \"run_unsandboxed\"; using \"fail\"");
        }
    }
    match table.get("image") {
        None => {}
        Some(toml::Value::String(image)) if valid_image(image.trim()) => {
            out.image = image.trim().to_string();
        }
        Some(_) => return Err("[container.sandbox] image is not a valid image reference".into()),
    }
    out.memory_bytes = bounded_u64(table, "memory_bytes", out.memory_bytes, MAX_MEMORY_BYTES)?;
    out.tmp_bytes = bounded_u64(table, "tmp_bytes", out.tmp_bytes, MAX_TMP_BYTES)?;
    out.workspace_bytes = bounded_u64(table, "workspace_bytes", out.workspace_bytes, MAX_WORKSPACE_BYTES)?;
    // Both tmpfs mounts are charged to the container's memory cgroup, so
    // together they may not promise more than the memory limit.
    if out.tmp_bytes.checked_add(out.workspace_bytes).is_none_or(|sum| sum > out.memory_bytes) {
        return Err("[container.sandbox] tmp_bytes + workspace_bytes must not exceed memory_bytes".into());
    }
    out.pids = bounded_u32(table, "pids", out.pids, MAX_PIDS)?;
    out.cpu_millis = bounded_u32(table, "cpu_millis", out.cpu_millis, MAX_CPU_MILLIS)?;
    out.max_turns = bounded_u32(table, "max_turns", out.max_turns, MAX_TURNS_CEILING)?;
    match table.get("executables") {
        None => {}
        Some(toml::Value::Table(executables)) => {
            for (key, value) in executables {
                let family = RuntimeFamily::parse(key).map_err(|_| {
                    format!("[container.sandbox.executables] has an unknown runtime `{key}`")
                })?;
                let path = value.as_str().map(PathBuf::from).filter(|p| valid_executable(p));
                let Some(path) = path else {
                    return Err(format!(
                        "[container.sandbox.executables] {key} must be an absolute path without `..`"
                    ));
                };
                out.executables.insert(family.name(), path);
            }
        }
        Some(_) => return Err("[container.sandbox.executables] must be a table".into()),
    }
    Ok(out)
}

/// The image the **script sandbox** (`duduclaw-container`: PTC scripts and
/// the `secaudit` PoC step) runs: exactly the task sandbox's image — the same
/// `config.toml [container.sandbox] image` key, the same default
/// ([`default_image`]). One image to pull, one key to document.
///
/// Fails closed like the task sandbox: an unreadable `config.toml` or an
/// invalid `[container.sandbox]` section is an error (the caller treats it as
/// "sandbox unavailable"), never a silent fallback to the default image.
pub fn script_sandbox_image(home: &Path) -> Result<String, String> {
    load(home).0.map(|s| s.image)
}

/// The settings and the task sandbox's escape hatch (`when_unavailable`),
/// read from `<home>/config.toml`. A missing file is the default
/// configuration; an unreadable or unparsable one is an error (with the
/// escape hatch = `Fail`).
pub fn load(home: &Path) -> (Result<SandboxSettings, String>, WhenUnavailable) {
    load_with(home, when_unavailable)
}

/// [`load`] for the script sandbox: the same settings, but the escape hatch
/// is `script_when_unavailable` (never `when_unavailable`).
pub fn load_for_scripts(home: &Path) -> (Result<SandboxSettings, String>, WhenUnavailable) {
    load_with(home, script_when_unavailable)
}

fn load_with(
    home: &Path,
    escape: fn(&toml::Table) -> WhenUnavailable,
) -> (Result<SandboxSettings, String>, WhenUnavailable) {
    let path = home.join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Ok(SandboxSettings::default()), WhenUnavailable::Fail);
        }
        Err(_) => return (Err("config.toml could not be read".into()), WhenUnavailable::Fail),
    };
    match text.parse::<toml::Table>() {
        Ok(table) => (parse(&table), escape(&table)),
        Err(_) => (Err("config.toml is not valid TOML".into()), WhenUnavailable::Fail),
    }
}
