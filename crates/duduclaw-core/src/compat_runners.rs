//! `compat.d` declarative app-compatibility runner registry — CP-1/A3.
//!
//! # Why this exists
//!
//! `commercial/docs/DESIGN-app-compat-layer-2026-08.md` §1 specifies a
//! SteamOS-style compat layer, copied verbatim in shape: a **discovery
//! directory** (scan `compat.d/` for declaration files), a **declaration**
//! (`from_os → to_os` capability claim), and an **entry point** (execution
//! command + a `require_tool` layer of PATH-resolvable dependencies). This
//! module is the discovery + declaration half — [`discover_runners`] scans
//! and reports; nothing here executes an `entrypoint` (that is a later
//! wave, see `commercial/docs/TODO-compat-cp1-2026-08.md`'s "整合" row).
//!
//! Third-party runners (a GE-Proton-style community package) plug into the
//! exact same interface by dropping a `.toml` file into either scan layer —
//! hardcoding a fixed runner list anywhere in this crate is exactly what
//! this module exists to avoid.
//!
//! # Two scan layers, one precedence rule
//!
//! - **Shipped layer** — [`SHIPPED_COMPAT_DIR`]
//!   (`/usr/share/duduclaw/compat.d`), populated by the
//!   `duduclaw-compat-runners` recipe at image-build time.
//! - **Data layer** — `<duduclaw_home>/compat.d`
//!   ([`DATA_COMPAT_SUBDIR`]), operator/dashboard-writable.
//!
//! **The data layer overrides the shipped layer for the same `id`** — the
//! same precedence convention as every other config layer in this crate
//! (`org_store`, `preset`): an explicit local override always wins over
//! whatever the image shipped.
//!
//! # Honesty over convenience: fail-open per file, not per scan
//!
//! A single malformed declaration (bad TOML, or a `from_os` value outside
//! the fixed enum below) must not hide every *other* runner from the
//! scan — but it also must not be silently dropped. Each file is parsed
//! independently; failures are collected as [`RunnerStatus::Malformed`]
//! entries alongside the successfully parsed ones (CLAUDE.md "security
//! gates fail closed" convention applied to the un-security-flavoured case:
//! a config-loading gate should degrade to "reported" not "vanished").
//!
//! Likewise, a `require_tool` entry that doesn't resolve on `$PATH` is not
//! a scan error — [`RunnerStatus::Ok`]'s `missing_tools` field carries it
//! as an honest status ("this runner IS declared, it just isn't usable
//! yet"), which is the whole point of `require_tool`'s semantics and is
//! exactly what CP-1's Waydroid declaration is expected to surface before
//! the kernel binder module lands (A1, a parallel wave).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── Scan roots ──────────────────────────────────────────────────────────

/// Shipped-layer scan root: where the appliance image installs its
/// pre-baked `compat.d/*.toml` declarations (the `duduclaw-compat-runners`
/// recipe's `do_install` target).
pub const SHIPPED_COMPAT_DIR: &str = "/usr/share/duduclaw/compat.d";

/// Data-layer scan root, relative to [`crate::platform::duduclaw_home`].
pub const DATA_COMPAT_SUBDIR: &str = "compat.d";

/// Test-only override for the full scan root list: a `:`-separated list of
/// absolute directories, given in low → high precedence (later entries
/// override earlier ones on an `id` collision — the same shape as the two
/// default roots above). Always literally `:`-split regardless of host OS
/// path-separator conventions — this is DuDuClaw's own env var, not a raw
/// `$PATH`-style value, and the appliance target is Linux-only.
///
/// Production code never needs this — [`compat_roots`] only consults it so
/// unit tests can drive [`discover_runners`] against a temp directory
/// instead of the real `/usr/share/duduclaw` + `duduclaw_home()` pair.
pub const COMPAT_DIRS_ENV: &str = "DUDUCLAW_COMPAT_DIRS";

// ── Declaration schema ──────────────────────────────────────────────────

/// The `from_os` value domain (design §1). Every runner bridges one of
/// these origin ecosystems into `to_os = "duduclaw-os"`.
///
/// Serialized/deserialized as kebab-case in the TOML declaration file
/// (`windows-game` / `windows-app` / `android` / `macos-remote`).
///
/// **This enum, not a free string, is the enforcement mechanism** behind
/// "an unknown `from_os` value must reject the whole file, not silently
/// skip the field": serde's default behaviour for an enum with no variant
/// matching the input is a hard deserialize error, which
/// [`parse_runner_file`] turns into one [`RunnerStatus::Malformed`] entry
/// for that file. A new origin ecosystem must be added here explicitly —
/// there is deliberately no fallback "other" variant that would let a
/// typo'd or half-implemented `from_os` slip through as if it were
/// recognized.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FromOs {
    /// Windows games via the Proton compat layer (already shipped, S8 —
    /// carried here so the value domain is complete, even though CP-1
    /// doesn't add any new `windows-game` declarations itself).
    WindowsGame,
    /// Windows desktop apps — Bottles (translation) or, in a later wave,
    /// a self-packaged VM+RemoteApp bridge (design §2.3).
    WindowsApp,
    /// Android apps via Waydroid (design §2.4).
    Android,
    /// A "connect back to your own Mac" remote client — not actually
    /// executing macOS code locally, but sharing the same install/
    /// grading/entry-point surface as the other three (design §2.5).
    MacosRemote,
}

impl FromOs {
    /// Canonical kebab-case string — the inverse of the serde encoding.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WindowsGame => "windows-game",
            Self::WindowsApp => "windows-app",
            Self::Android => "android",
            Self::MacosRemote => "macos-remote",
        }
    }
}

impl std::fmt::Display for FromOs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn default_to_os() -> String {
    "duduclaw-os".to_string()
}

/// One `compat.d/*.toml` declaration.
///
/// Declaration-only for CP-1/A3: nothing in this module invokes
/// `entrypoint`. A later wave is expected to add the actual launch path;
/// this struct's shape is already what that wave needs, so it should not
/// require a schema change to start executing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerDecl {
    /// Slug identifier — validated against [`is_valid_runner_id`]. Used as
    /// the merge key across scan layers (see [`discover_runners`]) and as
    /// the CLI-facing selector.
    pub id: String,
    /// Human-readable name for the app store / launcher surface. Not
    /// validated beyond "present" — free text, may be zh-TW.
    pub display_name: String,
    /// Origin ecosystem this runner bridges from.
    pub from_os: FromOs,
    /// Target OS. Currently always `"duduclaw-os"` — kept as a free
    /// string rather than an enum (design §1): unlike `from_os`, this side
    /// of the mapping has no value domain to enumerate yet.
    #[serde(default = "default_to_os")]
    pub to_os: String,
    /// Execution entry point (a shell command / binary invocation).
    /// Declared, not executed, in this wave.
    pub entrypoint: String,
    /// Tool names that must resolve on `$PATH` for this runner to be
    /// usable. Checked at scan time — see [`RunnerStatus::Ok::missing_tools`].
    #[serde(default)]
    pub require_tool: Vec<String>,
    /// One-line guidance shown to the user when a required tool is
    /// missing (e.g. where to install it from).
    #[serde(default)]
    pub install_hint: Option<String>,
    /// Free-form notes — scope caveats, upstream links, Verified-grade
    /// caveats, etc.
    #[serde(default)]
    pub notes: Option<String>,
}

/// Validate a runner id slug: `^[a-z0-9][a-z0-9-]*$`, ≤64 chars, no
/// trailing hyphen.
///
/// Delegates to [`crate::is_valid_new_agent_id`] instead of re-implementing
/// the same lowercase-slug rule a third time in this crate (agent ids and
/// preset ids already share it) — a compat runner id is used the exact
/// same way (path component under `compat.d/`, CLI positional/selector, log
/// field), so the same safety properties apply, and the codebase's
/// multi-wave agent-id-validator consolidation effort (see CLAUDE.md's
/// "第五波"/"第十波" notes) is the precedent for reusing rather than
/// forking this rule. If runner-id rules ever need to diverge from
/// agent-id rules, this is the one place to change.
pub fn is_valid_runner_id(s: &str) -> bool {
    crate::is_valid_new_agent_id(s)
}

// ── Scan result ─────────────────────────────────────────────────────────

/// Outcome of loading and cross-checking one `compat.d` declaration, after
/// scan-layer precedence has already been resolved (an `Ok` entry's
/// `source` is always the winning file for that `id`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunnerStatus {
    /// Parsed and id-validated successfully.
    Ok {
        decl: RunnerDecl,
        /// The file this declaration was loaded from (the winning layer,
        /// after precedence — see [`discover_runners`]).
        source: PathBuf,
        /// `require_tool` entries that do NOT currently resolve on
        /// `$PATH`. Empty means every dependency is present — this is the
        /// "ready" state, not an "unchecked" state; the check always runs.
        missing_tools: Vec<String>,
    },
    /// The file's TOML, or its `id` slug, failed to parse/validate. Kept
    /// per-file (not surfaced as a scan-wide error) so one bad declaration
    /// never hides the rest — see the module docs' "fail-open per file"
    /// section.
    Malformed { path: PathBuf, error: String },
}

// ── Discovery ───────────────────────────────────────────────────────────

/// Discover every `compat.d` declaration across the shipped layer and the
/// data layer, in that precedence order (data layer wins on an `id`
/// collision — see the module docs).
///
/// A missing scan root (normal on a dev machine without
/// `/usr/share/duduclaw`, or before the data-layer directory has ever been
/// created) is silently skipped — it is not an error condition, just an
/// empty layer.
///
/// Scan roots can be overridden wholesale via the `DUDUCLAW_COMPAT_DIRS`
/// env var (test-only — see [`COMPAT_DIRS_ENV`]).
pub fn discover_runners() -> Vec<RunnerStatus> {
    discover_runners_from(&compat_roots())
}

/// Same as [`discover_runners`] but with an explicit, caller-supplied root
/// list (low → high precedence). [`discover_runners`] is a thin wrapper
/// around this so tests can drive scanning deterministically against temp
/// directories without touching `DUDUCLAW_COMPAT_DIRS` or the real
/// filesystem layout.
pub fn discover_runners_from(roots: &[PathBuf]) -> Vec<RunnerStatus> {
    // Keyed by id so a later root's entry overwrites an earlier root's
    // entry for the same id — this IS the "data layer overrides shipped
    // layer" precedence rule, expressed as plain map-insertion order.
    // BTreeMap also gives deterministic id-sorted output for free.
    let mut by_id: BTreeMap<String, (RunnerDecl, PathBuf)> = BTreeMap::new();
    let mut malformed: Vec<(PathBuf, String)> = Vec::new();

    for root in roots {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(root) {
            Ok(read_dir) => read_dir
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
                .collect(),
            // A layer that doesn't exist yet is normal, not an error — the
            // data layer in particular is only created once an operator
            // (or the dashboard) drops the first override into it.
            Err(_) => continue,
        };
        // readdir order is unspecified; sort for deterministic output.
        entries.sort();

        for path in entries {
            match parse_runner_file(&path) {
                Ok(decl) => {
                    by_id.insert(decl.id.clone(), (decl, path));
                }
                Err(error) => malformed.push((path, error)),
            }
        }
    }

    let mut out: Vec<RunnerStatus> = by_id
        .into_values()
        .map(|(decl, source)| {
            let missing_tools: Vec<String> =
                decl.require_tool.iter().filter(|tool| !tool_on_path(tool)).cloned().collect();
            RunnerStatus::Ok { decl, source, missing_tools }
        })
        .collect();
    out.extend(malformed.into_iter().map(|(path, error)| RunnerStatus::Malformed { path, error }));
    out
}

/// Resolve the default two-layer root list, or the `DUDUCLAW_COMPAT_DIRS`
/// test override when set to a non-empty value.
fn compat_roots() -> Vec<PathBuf> {
    if let Ok(raw) = std::env::var(COMPAT_DIRS_ENV) {
        let roots: Vec<PathBuf> =
            raw.split(':').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    vec![PathBuf::from(SHIPPED_COMPAT_DIR), crate::platform::duduclaw_home().join(DATA_COMPAT_SUBDIR)]
}

/// Parse and id-validate one declaration file. Any failure — read error,
/// TOML syntax error, an unrecognized `from_os` value (a deserialize error
/// on [`FromOs`]), or an invalid `id` slug — rejects the **whole file**,
/// never a subset of its fields.
fn parse_runner_file(path: &Path) -> Result<RunnerDecl, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))?;
    let decl: RunnerDecl = toml::from_str(&raw).map_err(|e| format!("TOML parse failed: {e}"))?;
    if !is_valid_runner_id(&decl.id) {
        return Err(format!("invalid id slug (want ^[a-z0-9][a-z0-9-]*$): {:?}", decl.id));
    }
    Ok(decl)
}

/// Whether `tool` resolves to an existing, (on Unix) executable file
/// somewhere on the current `$PATH`.
///
/// Deliberately a plain PATH walk (`std::env::var_os("PATH")` +
/// `std::env::split_paths` + `is_file` + the Unix executable-bit check)
/// rather than shelling out to `which`/`where`: this scanner's entire
/// point is to report honestly when a tool is missing (`require_tool`'s
/// whole semantics), so it must not itself depend on an external `which`
/// binary that could be the very thing that's missing on a minimal image.
/// No new dependency either way — `std::env::split_paths` is std.
fn tool_on_path(tool: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(tool);
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::metadata(&candidate) {
                Ok(meta) if meta.permissions().mode() & 0o111 != 0 => return true,
                _ => continue,
            }
        }
        #[cfg(not(unix))]
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serialize the few env-mutating tests so they don't race each other —
    // matches the `ENV_LOCK` idiom already used in `platform.rs` /
    // `config.rs` / `provider_env.rs` / `spawn_env.rs`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const VALID_BOTTLES: &str = r#"
id = "bottles"
display_name = "Bottles"
from_os = "windows-app"
entrypoint = "flatpak run com.usebottles.bottles"
require_tool = ["flatpak"]
"#;

    fn write_decl(dir: &Path, filename: &str, content: &str) {
        std::fs::write(dir.join(filename), content).unwrap();
    }

    fn make_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&path, perm).unwrap();
        }
        path
    }

    /// RAII `$PATH` override for tests. Callers must hold [`ENV_LOCK`] for
    /// the guard's lifetime — `PATH` is process-global.
    struct PathOverride {
        prev: Option<std::ffi::OsString>,
    }
    impl PathOverride {
        fn set(dir: &Path) -> Self {
            let prev = std::env::var_os("PATH");
            unsafe { std::env::set_var("PATH", dir) };
            Self { prev }
        }
    }
    impl Drop for PathOverride {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("PATH", v),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    #[test]
    fn parses_a_valid_declaration() {
        let dir = tempfile::tempdir().unwrap();
        write_decl(dir.path(), "bottles.toml", VALID_BOTTLES);

        let statuses = discover_runners_from(&[dir.path().to_path_buf()]);
        assert_eq!(statuses.len(), 1);
        match &statuses[0] {
            RunnerStatus::Ok { decl, .. } => {
                assert_eq!(decl.id, "bottles");
                assert_eq!(decl.from_os, FromOs::WindowsApp);
                assert_eq!(decl.to_os, "duduclaw-os", "to_os default must apply");
                assert_eq!(decl.entrypoint, "flatpak run com.usebottles.bottles");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn unknown_from_os_rejects_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        write_decl(
            dir.path(),
            "bad.toml",
            "id = \"bad\"\ndisplay_name = \"Bad\"\nfrom_os = \"atari-2600\"\nentrypoint = \"nope\"\n",
        );

        let statuses = discover_runners_from(&[dir.path().to_path_buf()]);
        assert_eq!(statuses.len(), 1);
        match &statuses[0] {
            RunnerStatus::Malformed { path, error } => {
                assert!(path.ends_with("bad.toml"));
                assert!(!error.is_empty());
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn malformed_toml_does_not_abort_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        write_decl(dir.path(), "broken.toml", "this is not valid toml {{{");
        write_decl(dir.path(), "bottles.toml", VALID_BOTTLES);

        let statuses = discover_runners_from(&[dir.path().to_path_buf()]);
        assert_eq!(statuses.len(), 2);
        let ok_count = statuses.iter().filter(|s| matches!(s, RunnerStatus::Ok { .. })).count();
        let bad_count = statuses.iter().filter(|s| matches!(s, RunnerStatus::Malformed { .. })).count();
        assert_eq!(ok_count, 1);
        assert_eq!(bad_count, 1);
    }

    #[test]
    fn data_layer_overrides_shipped_layer_by_id() {
        let shipped = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write_decl(
            shipped.path(),
            "waydroid.toml",
            "id = \"waydroid\"\ndisplay_name = \"Waydroid (shipped)\"\nfrom_os = \"android\"\nentrypoint = \"waydroid\"\n",
        );
        write_decl(
            data.path(),
            "waydroid.toml",
            "id = \"waydroid\"\ndisplay_name = \"Waydroid (override)\"\nfrom_os = \"android\"\nentrypoint = \"waydroid session start\"\n",
        );

        // Shipped layer first, data layer second — matches
        // `compat_roots()`'s real ordering.
        let statuses =
            discover_runners_from(&[shipped.path().to_path_buf(), data.path().to_path_buf()]);
        assert_eq!(statuses.len(), 1, "same id across layers must merge to one entry");
        match &statuses[0] {
            RunnerStatus::Ok { decl, source, .. } => {
                assert_eq!(decl.display_name, "Waydroid (override)");
                assert!(source.starts_with(data.path()), "source must be the winning (data) file");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn require_tool_reports_missing_tools_honestly() {
        let _g = ENV_LOCK.lock().unwrap();
        let bindir = tempfile::tempdir().unwrap();
        make_executable(bindir.path(), "present-tool");
        let _path_guard = PathOverride::set(bindir.path());

        let dir = tempfile::tempdir().unwrap();
        write_decl(
            dir.path(),
            "x.toml",
            "id = \"x\"\ndisplay_name = \"X\"\nfrom_os = \"windows-app\"\nentrypoint = \"x\"\n\
             require_tool = [\"present-tool\", \"definitely-missing-tool-xyz\"]\n",
        );

        let statuses = discover_runners_from(&[dir.path().to_path_buf()]);
        match &statuses[0] {
            RunnerStatus::Ok { missing_tools, .. } => {
                assert_eq!(missing_tools, &vec!["definitely-missing-tool-xyz".to_string()]);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn ready_when_every_require_tool_present() {
        let _g = ENV_LOCK.lock().unwrap();
        let bindir = tempfile::tempdir().unwrap();
        make_executable(bindir.path(), "present-tool");
        let _path_guard = PathOverride::set(bindir.path());

        let dir = tempfile::tempdir().unwrap();
        write_decl(
            dir.path(),
            "x.toml",
            "id = \"x\"\ndisplay_name = \"X\"\nfrom_os = \"windows-app\"\nentrypoint = \"x\"\n\
             require_tool = [\"present-tool\"]\n",
        );

        let statuses = discover_runners_from(&[dir.path().to_path_buf()]);
        match &statuses[0] {
            RunnerStatus::Ok { missing_tools, .. } => {
                assert!(missing_tools.is_empty(), "{missing_tools:?}")
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn id_slug_validation() {
        assert!(is_valid_runner_id("bottles"));
        assert!(is_valid_runner_id("bottles-2"));
        assert!(is_valid_runner_id("a"));
        assert!(!is_valid_runner_id("Bottles"), "uppercase must be rejected");
        assert!(!is_valid_runner_id("-bottles"), "leading hyphen must be rejected");
        assert!(!is_valid_runner_id("bottles-"), "trailing hyphen must be rejected");
        assert!(!is_valid_runner_id(""), "empty must be rejected");
    }

    #[test]
    fn invalid_id_slug_marks_file_malformed() {
        let dir = tempfile::tempdir().unwrap();
        write_decl(
            dir.path(),
            "bad-id.toml",
            "id = \"Not_A_Slug\"\ndisplay_name = \"Bad Id\"\nfrom_os = \"android\"\nentrypoint = \"x\"\n",
        );

        let statuses = discover_runners_from(&[dir.path().to_path_buf()]);
        assert!(matches!(&statuses[0], RunnerStatus::Malformed { .. }));
    }

    #[test]
    fn missing_root_directory_is_not_an_error() {
        let statuses =
            discover_runners_from(&[PathBuf::from("/definitely/does/not/exist/compat.d")]);
        assert!(statuses.is_empty());
    }

    #[test]
    fn discover_runners_honours_compat_dirs_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_decl(dir.path(), "bottles.toml", VALID_BOTTLES);

        let prev = std::env::var_os(COMPAT_DIRS_ENV);
        unsafe { std::env::set_var(COMPAT_DIRS_ENV, dir.path()) };
        let statuses = discover_runners();
        unsafe {
            match &prev {
                Some(v) => std::env::set_var(COMPAT_DIRS_ENV, v),
                None => std::env::remove_var(COMPAT_DIRS_ENV),
            }
        }

        assert_eq!(statuses.len(), 1);
    }
}
