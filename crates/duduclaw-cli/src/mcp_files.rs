//! RFC-23 §14.2 (WP-F2): agent-facing tools for reading **local data files**.
//!
//! Three tools — `file_read` / `csv_read` / `xlsx_read` — exist for one
//! reason: an agent that reads `customers.csv` with the Claude CLI's built-in
//! `Read` (or `head` via `Bash`) puts every raw cell straight into the model's
//! context, because built-in tools are not MCP tools and therefore never pass
//! DuDuClaw's redaction choke point (`mcp_dispatch.rs`). Routing the same read
//! through an MCP tool is what makes the RFC-23 pipeline able to see, and
//! de-identify, the rows. The companion `data-file-guard` PreToolUse hook
//! (§14.4) blocks the built-in route so the model actually takes this one.
//!
//! ## What bounds these tools
//!
//! 1. **A path fence** — [`vet_path`]: canonicalize (which also resolves
//!    symlinks and proves existence), then require containment in one of the
//!    allowed roots. `..` is refused before canonicalization so the error names
//!    the real problem instead of a confusing "outside the roots".
//! 2. **Byte caps checked before parsing** — the file's metadata length is
//!    compared against the cap and the read is refused, so a mistyped path at a
//!    20 GB log never reaches the parser.
//! 3. **Row caps** — `limit` defaults to 200 and is clamped to
//!    [`ROW_LIMIT_MAX`]; `truncated` tells the model there is more.
//!
//! There is deliberately **no per-agent capability gate** (unlike the
//! `db_*` family): every root in the fence is either the caller's own agent
//! directory or an operator-declared `[files] allowed_roots` entry, so the
//! fence already answers "whose data is this?". `tools/list` therefore always
//! advertises all three.
//!
//! ## Directory trees (`code_map`, v1.81)
//!
//! [`vet_dir`] is the same fence for a directory (default: the employee's own
//! directory, [`employee_dir`] — a role member's is its `.ephemeral/<id>`
//! scaffold), and [`ContainedTree`] opens it descriptor-relatively from the
//! trusted allowed root, then reads files under it, through a per-component
//! no-follow descriptor walk that refuses symlinks, mount crossings (Linux:
//! mount id, not just `st_dev`), non-regular and multiply-linked files.
//!
//! ## Audit
//!
//! Every call appends to `tool_calls.jsonl` with `path` / `table` /
//! `row_count` — the *shape* of the read. Cell content is never written
//! there: the audit log is not where customer data belongs (same rule as
//! `mcp_db.rs`'s filter values).
//!
//! Native isolation uses a separate path below: immutable request identity,
//! granted workspace dirfds and openat2. CSV/workbook bytes go only to E's
//! fd-only parser adapter; an absent adapter refuses without a legacy fallback.
//! The canonicalized path and in-process parsers described above remain legacy
//! transport behavior only.
//!
//! Package H (H11): the `employee-work` workspace has no gateway dirfd. The
//! boundary's `authorize_file_with_refusal` asks root's work broker for the
//! file of the caller's live run and hands back root's sealed read-only memfd
//! snapshot (nlink 0, tmpfs). These tools read only that descriptor, through
//! the same byte/row caps, and never reopen anything by path; csv/xlsx bytes
//! go to the parse worker's ReadTable op through the installed `FileParser`.
//! A refusal shows root's fixed zh-TW reason (`WorkBrokerError::user_message`)
//! or the tool's generic text — never a path, file name or host location.

use std::path::{Path, PathBuf};

use duduclaw_mcp_boundary::resources::FileRefusal;
use serde_json::{Value, json};

// ── Caps (§14.2) ────────────────────────────────────────────────────────────

/// `file_read`: plain-text files only, 512 KiB.
pub const FILE_READ_MAX_BYTES: u64 = 512 * 1024;
/// `csv_read`: 64 MiB on disk.
pub const CSV_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// `xlsx_read`: 32 MiB on disk (a spreadsheet parser expands far past its
/// on-disk size, so this cap is deliberately tighter than the CSV one).
pub const XLSX_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Extensions `file_read` refuses, redirecting to the structured reader.
///
/// §14.2 scopes `file_read` to plain text. Letting it swallow a spreadsheet
/// would be a redaction bypass, not a convenience: a `db_field` rule binds to
/// `$.rows[*].<column>` (see `duduclaw_redaction::data_source`'s
/// `duduclaw_files` built-in, which deliberately binds only `csv_read` /
/// `xlsx_read`), so the same file returned as one text blob would never meet
/// its column rules — only the pattern rules would fire, and `客戶清單.xlsx.地址`
/// would sail through in the clear.
const STRUCTURED_EXTENSIONS: &[&str] = &["csv", "tsv", "xlsx", "xlsm", "xls", "ods"];

/// Default number of data rows returned by `csv_read` / `xlsx_read`.
pub const ROW_LIMIT_DEFAULT: usize = 200;
/// Hard ceiling on `limit`, whatever the caller asks for.
pub const ROW_LIMIT_MAX: usize = 2000;

// ── Result helpers (per-submodule convention, see mcp_db.rs) ────────────────

fn files_text(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn files_json(value: &Value) -> Value {
    files_text(&serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()))
}

fn files_error(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
}

// ── Path fence ──────────────────────────────────────────────────────────────

/// The directories a data-file tool may read from, in the order they are
/// reported to the caller when a path is refused.
///
/// `<agent_dir>/attachments` is listed separately even though it sits inside
/// `<agent_dir>`: channel attachments land there (`media::save_attachment_in_base`)
/// and naming it explicitly is what makes the refusal message actionable.
///
/// An empty / malformed `agent_id` contributes no agent roots — fail-closed:
/// an unidentified caller gets only `<home>/attachments` plus whatever the
/// operator declared.
pub fn allowed_roots(home_dir: &Path, agent_id: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if duduclaw_core::is_valid_agent_id(agent_id) {
        let agent_dir = home_dir.join("agents").join(agent_id);
        roots.push(agent_dir.join("attachments"));
        roots.push(agent_dir);
    }
    roots.push(home_dir.join("attachments"));
    roots.extend(configured_roots(home_dir));
    roots
}

/// `config.toml [files] allowed_roots = ["/srv/exports", ...]`.
///
/// Read raw (not through a typed config struct) for the same reason the
/// redaction spawn predicate does: this is one operator-set list on a path
/// that must not grow a new crate-level config type. Non-absolute entries are
/// dropped — a relative root would resolve against whatever cwd the MCP
/// server happened to inherit, which is not a fence.
fn configured_roots(home_dir: &Path) -> Vec<PathBuf> {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return Vec::new();
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return Vec::new();
    };
    doc.get("files")
        .and_then(|v| v.get("allowed_roots"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

// ── Guarded filesystem calls (RM-155) ───────────────────────────────────────

/// Whether `path` is in a macOS-protected (TCC) location on this Mac. Always
/// `false` off macOS, where `EPERM` means something else entirely.
fn tcc_protected(path: &Path) -> bool {
    use duduclaw_gateway::macos_folder_access as folder_access;
    if !cfg!(target_os = "macos") {
        return false;
    }
    let Some(home) = folder_access::user_home() else {
        return false;
    };
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return false,
        }
    };
    folder_access::is_tcc_protected(&absolute, &home)
}

/// Run one filesystem call for a data-file tool on a dedicated thread with a
/// bounded wait.
///
/// After an update macOS asks again for folder access; until someone clicks
/// Allow, a read in `~/Documents` (or iCloud, `/Volumes/*`…) fails with
/// `EPERM` — or, while the consent prompt is on screen, blocks. Without this
/// the tool call hung until the whole reply timed out. Now a protected-path
/// `EPERM` names the macOS permission and the steps, a timeout says macOS is
/// waiting for a click, and every other error keeps the caller's `generic`
/// wording.
fn guarded_fs<T, F>(
    raw: &str,
    path: &Path,
    generic: impl FnOnce(&std::io::Error) -> String,
    op: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(PathBuf) -> std::io::Result<T> + Send + 'static,
{
    guarded_fs_with(
        raw,
        path,
        tcc_protected(path),
        duduclaw_gateway::macos_folder_access::FILE_OP_TIMEOUT,
        generic,
        op,
    )
}

/// [`guarded_fs`] with the protection verdict and the wait injected (tests).
fn guarded_fs_with<T, F, G>(
    raw: &str,
    path: &Path,
    protected: bool,
    timeout: std::time::Duration,
    generic: G,
    op: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(PathBuf) -> std::io::Result<T> + Send + 'static,
    G: FnOnce(&std::io::Error) -> String,
{
    use duduclaw_gateway::macos_folder_access::{self as folder_access, ProbeOutcome};
    let owned = path.to_path_buf();
    // Shared, bounded runner: one call in flight per path and a global cap,
    // so repeated calls into a stuck consent prompt never pile up threads.
    match folder_access::run_guarded(path, timeout, move || op(owned)) {
        ProbeOutcome::Completed(Ok(value)) => Ok(value),
        ProbeOutcome::Completed(Err(e)) => {
            let fallback = generic(&e);
            Err(folder_access::file_error_text(raw, &e, protected, &fallback))
        }
        ProbeOutcome::TimedOut => Err(folder_access::file_timeout_text(raw, protected, timeout)),
        ProbeOutcome::Pending => Err(folder_access::file_pending_text(raw, protected)),
    }
}

fn render_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("、")
}

/// Resolve `raw` to a real regular file inside one of the allowed roots.
///
/// Every rejection carries the reason and the root list, never file content.
pub fn vet_path(raw: &str, home_dir: &Path, agent_id: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("缺少 path 參數（要讀取的檔案路徑）。".to_string());
    }
    let candidate = Path::new(raw);
    // Refused before canonicalization so the message names the real problem.
    // `canonicalize` would resolve `..` away and the path might then land
    // inside a root, which is exactly the traversal this rejects.
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "路徑「{raw}」含有 `..`，不接受相對上層路徑，請改用完整路徑。"
        ));
    }

    let roots = allowed_roots(home_dir, agent_id);
    // Canonicalize resolves symlinks (so a symlink pointing outside a root is
    // caught by the containment test below) and proves the file exists.
    let real = guarded_fs(
        raw,
        candidate,
        |e| format!("路徑「{raw}」無法讀取：{e}"),
        |p| p.canonicalize(),
    )?;

    let contained = roots.iter().any(|root| {
        // `real` is canonical, so a root that is already a lexical prefix of
        // it is itself symlink-free there — no need to touch the root (which
        // may be a protected folder still waiting on a macOS prompt).
        if real.starts_with(root) {
            return true;
        }
        // Canonicalize the roots too — on macOS `/var` and `/tmp` are
        // themselves symlinks, so an uncanonicalized prefix test would reject
        // legitimate paths on a developer machine. Guarded: an unanswered
        // prompt on one root must not hang the read of another.
        let display = root.display().to_string();
        let root = guarded_fs(&display, root, |e| e.to_string(), |p| p.canonicalize())
            .unwrap_or_else(|_| root.clone());
        real.starts_with(&root)
    });
    if !contained {
        return Err(format!(
            "路徑「{raw}」不在允許的目錄範圍內。允許的根目錄：{}。如需讀取其他位置，請在 config.toml 的 [files] allowed_roots 加入該目錄。",
            render_roots(&roots)
        ));
    }

    let meta = guarded_fs(
        raw,
        &real,
        |e| format!("路徑「{raw}」無法讀取：{e}"),
        |p| std::fs::metadata(p),
    )?;
    if !meta.is_file() {
        return Err(format!("路徑「{raw}」不是一般檔案。"));
    }
    Ok(real)
}

// ── Directory fence + contained tree (v1.81, S1: `code_map`) ────────────────

/// The own directory of the AI employee `agent_id`, or `None` when the id
/// does not resolve to one (v1.81, S2).
///
/// - a role member / ephemeral (`eph-…`) resolves through
///   `duduclaw_gateway::ephemeral::resolve_agent_dir` to its
///   `<home>/agents/.ephemeral/<id>` scaffold (canonical, contained in the
///   ephemeral namespace, with an `agent.toml`);
/// - any other id must be a valid agent id with `<home>/agents/<id>/agent.toml`
///   (`mcp_namespace::client_is_agent`).
///
/// `code_map`'s identity gate, its default root and its `tools/list` entry all
/// read this one resolver, so "listed ⇒ callable".
pub fn employee_dir(home_dir: &Path, agent_id: &str) -> Option<PathBuf> {
    if duduclaw_gateway::ephemeral::is_ephemeral_id(agent_id) {
        return duduclaw_gateway::ephemeral::resolve_agent_dir(home_dir, agent_id);
    }
    crate::mcp_namespace::client_is_agent(home_dir, agent_id)
        .then(|| home_dir.join("agents").join(agent_id))
}

/// A directory [`vet_dir`] accepted, plus how [`ContainedTree::open`] must
/// reach it: from the trusted allowed root `anchor`, one no-follow component
/// at a time (`rel`), never leaving `anchor`'s mount.
#[derive(Debug, Clone)]
pub struct VettedDir {
    /// Canonical directory to scan.
    path: PathBuf,
    /// Canonical allowed root the descriptor walk starts from: the trusted
    /// `<home>/agents/` directory whenever the target is the employee's own
    /// directory or below it (`rel` then starts with `<id>`, or with
    /// `.ephemeral`, `<id>` for a role member), else the operator-declared
    /// root that contains it.
    anchor: PathBuf,
    /// Normal components from `anchor` to `path`.
    rel: Vec<String>,
    /// Unix `(st_dev, st_ino)` of `path` as vetted: a directory replaced
    /// between vetting and opening is refused.
    identity: Option<(u64, u64)>,
}

impl VettedDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolve the directory `code_map` may scan for `agent_id`.
///
/// - an id that does not resolve to an employee directory ([`employee_dir`])
///   is refused (fail closed);
/// - an empty `raw` is the employee's own directory (`<home>/agents/<id>`, or
///   a role member's `<home>/agents/.ephemeral/<id>`);
/// - a relative `raw` resolves under that own directory (never the MCP
///   server's working directory);
/// - `..` is refused before canonicalization, like [`vet_path`];
/// - the canonical directory must sit inside the own directory,
///   `<home>/attachments` or an operator `[files] allowed_roots` entry.
///
/// The result names the trusted root [`ContainedTree::open`] walks from and
/// the identity of the directory as vetted.
pub fn vet_dir(raw: &str, home_dir: &Path, agent_id: &str) -> Result<VettedDir, String> {
    let Some(own) = employee_dir(home_dir, agent_id) else {
        return Err("無法辨識呼叫者是哪一位 AI 員工，沒有可掃描的目錄。".to_string());
    };
    let raw = raw.trim();
    let candidate = if raw.is_empty() {
        own.clone()
    } else {
        let p = Path::new(raw);
        if p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!(
                "目錄「{raw}」含有 `..`，不接受相對上層路徑，請改用完整路徑。"
            ));
        }
        if p.is_absolute() { p.to_path_buf() } else { own.join(p) }
    };
    // Operator-trusted roots besides the own directory. The own directory's
    // `attachments` is deliberately not one of them: it is reached through
    // the own directory (and so through its mount check), never on its own.
    let mut operator_roots = vec![home_dir.join("attachments")];
    operator_roots.extend(configured_roots(home_dir));
    let shown_roots = {
        let mut r = vec![own.join("attachments"), own.clone()];
        r.extend(operator_roots.iter().cloned());
        r
    };
    let shown = if raw.is_empty() { candidate.display().to_string() } else { raw.to_string() };
    let outside = || {
        format!(
            "目錄「{shown}」不在允許的目錄範圍內。允許的根目錄：{}。如需掃描其他位置，請在 config.toml 的 [files] allowed_roots 加入該目錄。",
            render_roots(&shown_roots)
        )
    };
    let real = candidate
        .canonicalize()
        .map_err(|e| format!("目錄「{shown}」無法讀取：{e}"))?;
    let own_real = own
        .canonicalize()
        .map_err(|e| format!("目錄「{}」無法讀取：{e}", own.display()))?;
    let anchor = if real.starts_with(&own_real) {
        // The own directory or anything below it — for an employee
        // `agents/<id>/…`, for a role member `agents/.ephemeral/<id>/…` — is
        // always walked from the trusted `agents/` directory itself, so every
        // component on the way (`.ephemeral`, `<id>`, then the sub-root) is
        // opened no-follow and compared with the `agents/` mount. Anchoring
        // at the own directory (or at `.ephemeral`) would make a bind-mounted
        // own directory (or `.ephemeral`) the mount baseline (Daybreak Low).
        let agents_real = home_dir
            .join("agents")
            .canonicalize()
            .map_err(|e| format!("目錄「{}」無法讀取：{e}", home_dir.join("agents").display()))?;
        if own_real == agents_real || !own_real.starts_with(&agents_real) {
            return Err(outside());
        }
        agents_real
    } else {
        operator_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .filter(|root| real.starts_with(root))
            .max_by_key(|root| root.components().count())
            .ok_or_else(outside)?
    };
    let meta = std::fs::metadata(&real).map_err(|e| format!("目錄「{shown}」無法讀取：{e}"))?;
    if !meta.is_dir() {
        return Err(format!("「{shown}」不是目錄。"));
    }
    let rel = real
        .strip_prefix(&anchor)
        .map_err(|_| outside())?
        .components()
        .map(|c| match c {
            std::path::Component::Normal(part) => part
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| format!("目錄「{shown}」的路徑不是 UTF-8。")),
            _ => Err(outside()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    };
    #[cfg(not(unix))]
    let identity = None;
    Ok(VettedDir { path: real, anchor, rel, identity })
}

/// Identity of the mount a descriptor lives on, for "did this walk cross a
/// mount?" (v1.81, S1 review Low).
///
/// `st_dev` alone is not enough: a bind mount of another directory **on the
/// same filesystem** has the same `st_dev` as the tree it is mounted into.
/// On Linux the per-mount id (`statx` `STATX_MNT_ID`, or `mnt_id:` in
/// `/proc/self/fdinfo/<fd>` on kernels before 5.8) tells them apart. Other
/// Unixes have no such id; there the walk relies on `st_dev` plus the
/// per-component `O_NOFOLLOW` open.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountId {
    /// The platform has no per-mount id (macOS / BSD): `st_dev` only.
    NotTracked,
    /// Linux: the kernel's mount id for the descriptor.
    Known(u64),
    /// Linux, but neither `statx` nor `/proc` reported a mount id.
    Unknown,
}

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountKey {
    dev: u64,
    mnt: MountId,
}

/// `true` when `child` is not on the same mount as `root` — the walk must
/// refuse it. Fail closed: a different device, a different mount id, or any
/// mount id the kernel would not report all count as crossing.
#[cfg_attr(not(unix), allow(dead_code))]
fn crosses_mount(root: MountKey, child: MountKey) -> bool {
    if root.dev != child.dev {
        return true;
    }
    match (root.mnt, child.mnt) {
        (MountId::NotTracked, MountId::NotTracked) => false,
        (MountId::Known(a), MountId::Known(b)) => a != b,
        _ => true,
    }
}

fn contained_refusal(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, message)
}

/// A directory tree opened once for descriptor-relative, contained reads
/// (v1.81, S1). Built from a [`vet_dir`] result.
///
/// [`Self::read_to_string`] walks `rel` one component at a time from the
/// root's descriptor, so nothing reachable through a path swap after vetting
/// is ever opened. Refused, on every component:
///
/// - a symlink (Unix: `O_NOFOLLOW`, plus `openat2` `RESOLVE_NO_SYMLINKS` on
///   Linux; Windows: reparse points, through `duduclaw_agent`'s `SafeDir`);
/// - a mount crossing: a different `st_dev`, and on Linux a different mount
///   id, including a same-filesystem bind mount (`openat2`
///   `RESOLVE_NO_XDEV` where the kernel has it, else the `statx` /
///   `/proc/self/fdinfo` mount id comparison — both run when both exist);
///
/// and on the final file: not a regular file, more than one hard link, or
/// larger than the cap. The content is read from the same descriptor.
pub struct ContainedTree {
    root: PathBuf,
    inner: contained_imp::Root,
}

impl ContainedTree {
    /// Open a [`vet_dir`] result. The trusted allowed root (`anchor` —
    /// `<home>/agents/` for anything in the employee's own directory) is
    /// opened from `/` with `O_NOFOLLOW` on every component (the path to it
    /// may cross mounts); the scan root is then opened **descriptor-relative
    /// from it**, component by component (`<id>`, or `.ephemeral` and `<id>`,
    /// included), under the same rules the reads use — no symlink, no mount
    /// crossing — and must be the very directory [`vet_dir`] saw (Unix
    /// `st_dev` / `st_ino`). So a scan root, own directory or `.ephemeral`
    /// that is itself a (same-filesystem) bind mount, or that is swapped for
    /// a symlink, a bind mount or another directory between vetting and
    /// opening, is refused.
    pub fn open(vetted: &VettedDir) -> std::io::Result<Self> {
        Ok(Self {
            root: vetted.path.clone(),
            inner: contained_imp::open_root(&vetted.anchor, &vetted.rel, vetted.identity)?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read `rel` (relative to the root, only normal components) as UTF-8,
    /// at most `max_bytes`.
    pub fn read_to_string(&self, rel: &Path, max_bytes: u64) -> std::io::Result<String> {
        let mut parts: Vec<&str> = Vec::new();
        for component in rel.components() {
            match component {
                std::path::Component::Normal(part) => parts
                    .push(part.to_str().ok_or_else(|| contained_refusal("non-UTF8 path"))?),
                std::path::Component::CurDir => {}
                _ => return Err(contained_refusal("only relative, normal path components")),
            }
        }
        if parts.is_empty() {
            return Err(contained_refusal("empty path"));
        }
        let bytes = contained_imp::read(&self.inner, &parts, max_bytes)?;
        String::from_utf8(bytes)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not UTF-8"))
    }
}

#[cfg(unix)]
mod contained_imp {
    use super::{MountId, MountKey, contained_refusal as refused, crosses_mount};
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path};

    pub(super) struct Root {
        dir: File,
        key: MountKey,
    }

    fn cname(part: &str) -> io::Result<CString> {
        if part.is_empty() || part == "." || part == ".." || part.contains('/') {
            return Err(refused("invalid path component"));
        }
        CString::new(part).map_err(|_| refused("NUL in path"))
    }

    /// Open one component below `dirfd`, never following a symlink. Below
    /// the root (`contained`), Linux first tries `openat2` with
    /// `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`.
    fn open_component(dirfd: RawFd, name: &CString, flags: libc::c_int, contained: bool) -> io::Result<File> {
        #[cfg(target_os = "linux")]
        {
            if contained && let Some(result) = linux::openat2_contained(dirfd, name, flags) {
                return result;
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = contained;
        // SAFETY: a valid directory fd and a NUL-terminated single component.
        let fd = unsafe {
            libc::openat(
                dirfd,
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new, uniquely owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn mount_key(file: &File) -> io::Result<MountKey> {
        let dev = file.metadata()?.dev();
        #[cfg(target_os = "linux")]
        let mnt = linux::mount_id(file.as_raw_fd());
        #[cfg(not(target_os = "linux"))]
        let mnt = MountId::NotTracked;
        Ok(MountKey { dev, mnt })
    }

    pub(super) fn open_root(anchor: &Path, rel: &[String], identity: Option<(u64, u64)>) -> io::Result<Root> {
        if !anchor.is_absolute() {
            return Err(refused("absolute root required"));
        }
        let mut dir = File::open("/")?;
        for component in anchor.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => {
                    let name = cname(part.to_str().ok_or_else(|| refused("non-UTF8 path"))?)?;
                    // The path to the trusted allowed root may legitimately
                    // cross mounts.
                    dir = open_component(dir.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY, false)?;
                }
                _ => return Err(refused("non-normal root component")),
            }
        }
        if !dir.metadata()?.is_dir() {
            return Err(refused("root is not a directory"));
        }
        let key = mount_key(&dir)?;
        if matches!(key.mnt, MountId::Unknown) {
            return Err(refused("the kernel reports no mount id; refusing to scan"));
        }
        // From the allowed root to the scan root: contained, no-follow, and on
        // the allowed root's own mount (a bind-mounted scan root is refused).
        for part in rel {
            let child = open_component(dir.as_raw_fd(), &cname(part)?, libc::O_RDONLY | libc::O_DIRECTORY, true)?;
            if !child.metadata()?.is_dir() {
                return Err(refused("not a directory"));
            }
            if crosses_mount(key, mount_key(&child)?) {
                return Err(refused("scan root is on another mount than its allowed root"));
            }
            dir = child;
        }
        if let Some((dev, ino)) = identity {
            let meta = dir.metadata()?;
            if meta.dev() != dev || meta.ino() != ino {
                return Err(refused("scan root was replaced after it was vetted"));
            }
        }
        Ok(Root { dir, key })
    }

    pub(super) fn read(root: &Root, parts: &[&str], max_bytes: u64) -> io::Result<Vec<u8>> {
        let (file_name, dirs) = parts.split_last().ok_or_else(|| refused("empty path"))?;
        let mut held: Option<File> = None;
        for part in dirs {
            let parent = held.as_ref().map_or(root.dir.as_raw_fd(), |f| f.as_raw_fd());
            let child = open_component(parent, &cname(part)?, libc::O_RDONLY | libc::O_DIRECTORY, true)?;
            if !child.metadata()?.is_dir() {
                return Err(refused("not a directory"));
            }
            if crosses_mount(root.key, mount_key(&child)?) {
                return Err(refused("path crosses a mount point"));
            }
            held = Some(child);
        }
        let parent = held.as_ref().map_or(root.dir.as_raw_fd(), |f| f.as_raw_fd());
        let file = open_component(parent, &cname(file_name)?, libc::O_RDONLY, true)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(refused("regular single-link file required"));
        }
        if crosses_mount(root.key, mount_key(&file)?) {
            return Err(refused("file is on another mount"));
        }
        if meta.len() > max_bytes {
            return Err(refused("file is over the size cap"));
        }
        let mut buf = Vec::new();
        file.take(max_bytes.saturating_add(1)).read_to_end(&mut buf)?;
        if buf.len() as u64 > max_bytes {
            return Err(refused("file grew past the size cap"));
        }
        Ok(buf)
    }

    #[cfg(target_os = "linux")]
    pub(super) mod linux {
        use super::{MountId, refused};
        use std::ffi::CString;
        use std::fs::File;
        use std::io;
        use std::os::fd::{FromRawFd, RawFd};
        use std::sync::atomic::{AtomicU8, Ordering};

        /// `openat2(2)`: 437 on every architecture (unified syscall table,
        /// Linux 5.6+).
        const SYS_OPENAT2: libc::c_long = 437;
        const RESOLVE_NO_XDEV: u64 = 0x01;
        const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
        const RESOLVE_NO_SYMLINKS: u64 = 0x04;
        const RESOLVE_BENEATH: u64 = 0x08;
        /// `statx(2)` mask bit for `stx_mnt_id` (Linux 5.8+).
        const STATX_MNT_ID: u32 = 0x1000;

        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }

        /// 0 = not probed yet, 1 = works, 2 = unavailable (ENOSYS / seccomp).
        static OPENAT2: AtomicU8 = AtomicU8::new(0);

        /// `None` when `openat2` is unavailable — the caller falls back to
        /// `openat` + the mount-id comparison. `EXDEV` (a mount crossing,
        /// bind mounts included) is a refusal, not a fallback.
        pub(in super::super) fn openat2_contained(
            dirfd: RawFd,
            name: &CString,
            flags: libc::c_int,
        ) -> Option<io::Result<File>> {
            if OPENAT2.load(Ordering::Relaxed) == 2 {
                return None;
            }
            let how = OpenHow {
                flags: (flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK) as u32 as u64,
                mode: 0,
                resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV | RESOLVE_NO_MAGICLINKS,
            };
            // SAFETY: valid dirfd, NUL-terminated name, a live `open_how`
            // whose size is passed alongside it.
            let fd = unsafe {
                libc::syscall(
                    SYS_OPENAT2,
                    dirfd as libc::c_long,
                    name.as_ptr(),
                    &how as *const OpenHow,
                    std::mem::size_of::<OpenHow>(),
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                return match error.raw_os_error() {
                    Some(libc::ENOSYS) | Some(libc::EPERM) | Some(libc::E2BIG)
                        if OPENAT2.load(Ordering::Relaxed) != 1 =>
                    {
                        OPENAT2.store(2, Ordering::Relaxed);
                        None
                    }
                    Some(libc::EXDEV) => Some(Err(refused("path crosses a mount point"))),
                    Some(libc::ELOOP) => Some(Err(refused("path component is a symlink"))),
                    _ => Some(Err(error)),
                };
            }
            OPENAT2.store(1, Ordering::Relaxed);
            // SAFETY: openat2 returned a new, uniquely owned descriptor.
            Some(Ok(unsafe { File::from_raw_fd(fd as RawFd) }))
        }

        /// The kernel mount id of `fd`: `statx(fd, "", AT_EMPTY_PATH,
        /// STATX_MNT_ID)`, else `mnt_id:` from `/proc/self/fdinfo/<fd>`
        /// (Linux 3.15+), else [`MountId::Unknown`].
        pub(in super::super) fn mount_id(fd: RawFd) -> MountId {
            // `struct statx` is 256 bytes; u64 storage keeps it aligned.
            let mut buf = [0u64; 32];
            let empty = c"";
            // SAFETY: valid fd, an empty NUL-terminated path with
            // AT_EMPTY_PATH, and a 256-byte writable buffer.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_statx,
                    fd as libc::c_long,
                    empty.as_ptr(),
                    (libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW) as libc::c_long,
                    STATX_MNT_ID as libc::c_long,
                    buf.as_mut_ptr(),
                )
            };
            if rc == 0 {
                // SAFETY: reinterpreting the initialised u64 buffer as bytes.
                let bytes: &[u8] =
                    unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), 256) };
                let mask = u32::from_ne_bytes(bytes[0..4].try_into().unwrap_or([0; 4]));
                if mask & STATX_MNT_ID != 0 {
                    // `stx_mnt_id` is at byte offset 144.
                    return MountId::Known(u64::from_ne_bytes(
                        bytes[144..152].try_into().unwrap_or([0; 8]),
                    ));
                }
            }
            std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))
                .ok()
                .and_then(|text| parse_fdinfo_mnt_id(&text))
                .map_or(MountId::Unknown, MountId::Known)
        }

        pub(in super::super) fn parse_fdinfo_mnt_id(text: &str) -> Option<u64> {
            text.lines()
                .find_map(|line| line.strip_prefix("mnt_id:"))
                .and_then(|v| v.trim().parse().ok())
        }
    }
}

#[cfg(windows)]
mod contained_imp {
    //! Windows: `duduclaw_agent`'s `SafeDir` opens every component relative
    //! to its parent handle (`NtCreateFile`, `OBJ_DONT_REPARSE`,
    //! `FILE_OPEN_REPARSE_POINT`) and refuses reparse points — which includes
    //! symlinks, junctions and volume mount points — and multiply-linked
    //! files.
    use duduclaw_agent::skill_loader::safe_dir::SafeDir;
    use std::io::{self, Read};
    use std::path::Path;

    use super::contained_refusal as refused;

    pub(super) struct Root {
        dir: SafeDir,
    }

    /// The allowed root is opened by path; the scan root below it one
    /// reparse-refusing handle-relative component at a time.
    pub(super) fn open_root(anchor: &Path, rel: &[String], _identity: Option<(u64, u64)>) -> io::Result<Root> {
        let mut dir = SafeDir::open(anchor)?;
        for part in rel {
            dir = dir.child(part)?;
        }
        Ok(Root { dir })
    }

    pub(super) fn read(root: &Root, parts: &[&str], max_bytes: u64) -> io::Result<Vec<u8>> {
        let (file_name, dirs) = parts.split_last().ok_or_else(|| refused("empty path"))?;
        let mut held: Option<SafeDir> = None;
        for part in dirs {
            let next = held.as_ref().unwrap_or(&root.dir).child(part)?;
            held = Some(next);
        }
        let file = held.as_ref().unwrap_or(&root.dir).read_file(file_name)?;
        if file.metadata()?.len() > max_bytes {
            return Err(refused("file is over the size cap"));
        }
        let mut buf = Vec::new();
        file.take(max_bytes.saturating_add(1)).read_to_end(&mut buf)?;
        if buf.len() as u64 > max_bytes {
            return Err(refused("file grew past the size cap"));
        }
        Ok(buf)
    }
}

#[cfg(not(any(unix, windows)))]
mod contained_imp {
    use std::io;
    use std::path::Path;

    pub(super) struct Root;

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "contained tree reads are unavailable on this platform"))
    }

    pub(super) fn open_root(_: &Path, _: &[String], _: Option<(u64, u64)>) -> io::Result<Root> {
        unsupported()
    }

    pub(super) fn read(_: &Root, _: &[&str], _: u64) -> io::Result<Vec<u8>> {
        unsupported()
    }
}

/// `table` for a result: the file's basename **with** its extension, because
/// that is the name a `db_field` rule writes (`customers.csv.name`).
fn table_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn too_large(raw: &str, len: u64, cap: u64) -> String {
    format!("檔案「{raw}」為 {len} bytes，超過此工具的上限 {cap} bytes，拒絕讀取。")
}

// ── Argument parsing ────────────────────────────────────────────────────────

fn arg_str<'a>(arguments: &'a Value, key: &str) -> &'a str {
    arguments.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// `limit` / `offset` accept a JSON number or a numeric string (some runtimes
/// stringify every tool argument). Out-of-range values clamp rather than
/// error — a model asking for 10_000 rows wants "as many as you'll give me".
fn arg_usize(arguments: &Value, key: &str) -> Option<usize> {
    match arguments.get(key) {
        Some(Value::Number(n)) => n.as_u64().map(|v| v as usize),
        Some(Value::String(s)) => s.trim().parse::<usize>().ok(),
        _ => None,
    }
}

fn arg_bool(arguments: &Value, key: &str) -> Option<bool> {
    match arguments.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_limit(arguments: &Value) -> usize {
    arg_usize(arguments, "limit")
        .unwrap_or(ROW_LIMIT_DEFAULT)
        .clamp(1, ROW_LIMIT_MAX)
}

fn resolve_offset(arguments: &Value) -> usize {
    arg_usize(arguments, "offset").unwrap_or(0)
}

/// `delimiter` is one byte. `\t` / `tab` are spelled out because a literal tab
/// character does not survive most JSON-authoring paths intact.
fn resolve_delimiter(arguments: &Value) -> Result<u8, String> {
    let raw = arg_str(arguments, "delimiter");
    if raw.is_empty() {
        return Ok(b',');
    }
    match raw {
        "\\t" | "\t" | "tab" | "TAB" => return Ok(b'\t'),
        _ => {}
    }
    let bytes = raw.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii() {
        return Err(format!(
            "delimiter 必須是單一 ASCII 字元（或 \\t 代表 tab），收到「{raw}」。"
        ));
    }
    Ok(bytes[0])
}

/// Turn a raw header row into unique, non-empty JSON object keys.
///
/// A spreadsheet's header row is customer-authored: blanks and repeats are
/// normal. Both must be repaired here, because the rows are emitted as JSON
/// objects and a duplicate key would silently drop a whole column — the kind
/// of data loss that looks like a redaction bug later.
fn normalize_columns(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for (idx, name) in raw.iter().enumerate() {
        let trimmed = name.trim();
        let base = if trimmed.is_empty() {
            format!("c{}", idx + 1)
        } else {
            trimmed.to_string()
        };
        let mut candidate = base.clone();
        let mut suffix = idx + 1;
        while out.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        out.push(candidate);
    }
    out
}

fn synthetic_columns(width: usize) -> Vec<String> {
    (1..=width).map(|i| format!("c{i}")).collect()
}

/// Build one row object. Missing trailing cells become `""` (CSV) / `null`
/// (xlsx, whose `Data::Empty` already means "no value") so every row carries
/// the same key set — a stable shape is what makes a JsonPath field rule
/// (`$.rows[*].name`) match reliably.
fn row_object(columns: &[String], cells: Vec<Value>, missing: fn() -> Value) -> Value {
    let mut obj = serde_json::Map::with_capacity(columns.len().max(cells.len()));
    let mut cells = cells.into_iter();
    for column in columns {
        obj.insert(column.clone(), cells.next().unwrap_or_else(missing));
    }
    // Cells past the header width still have to reach the model (and the
    // redaction rules) — name them positionally rather than dropping them.
    for (extra_idx, cell) in cells.enumerate() {
        let key = format!("c{}", columns.len() + extra_idx + 1);
        obj.insert(key, cell);
    }
    Value::Object(obj)
}

fn empty_string() -> Value {
    Value::String(String::new())
}

fn json_null() -> Value {
    Value::Null
}

// ── file_read ───────────────────────────────────────────────────────────────

/// Read a plain-text file (txt / md / json / log / yaml …), capped at
/// [`FILE_READ_MAX_BYTES`].
pub fn handle_file_read(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    if let Some(call) = crate::mcp_isolation::current() {
        // Synchronous entry (tests and any sync caller). `mcp::dispatch` uses
        // `handle_isolated_file_read`, which keeps root's broker round trip
        // off the async worker. Both share every check below.
        return isolated_file_read(arguments, home_dir, agent_id, &call);
    }
    let raw_path = arg_str(arguments, "path");
    let path = match vet_path(raw_path, home_dir, agent_id) {
        Ok(p) => p,
        Err(msg) => {
            audit_refusal(home_dir, agent_id, "file_read", raw_path);
            return files_error(&msg);
        }
    };
    if let Some(ext) = structured_extension(&path) {
        audit_refusal(home_dir, agent_id, "file_read", raw_path);
        return files_error(&format!(
            "「{raw_path}」是 .{ext} 結構化資料檔，file_read 只讀純文字。請改用 {}——只有結構化讀取才能讓「資料表.欄位」規則生效。",
            if ext == "csv" || ext == "tsv" {
                "csv_read"
            } else {
                "xlsx_read"
            }
        ));
    }
    let max_bytes = arg_usize(arguments, "max_bytes")
        .map(|v| (v as u64).min(FILE_READ_MAX_BYTES))
        .unwrap_or(FILE_READ_MAX_BYTES)
        .max(1);

    let total = file_len(&path);
    let buf = match guarded_fs(
        raw_path,
        &path,
        |e| format!("讀取「{raw_path}」失敗：{e}"),
        move |p| read_prefix(&p, max_bytes),
    ) {
        Ok(b) => b,
        Err(msg) => return files_error(&msg),
    };
    let truncated = total > buf.len() as u64;
    // Decode only the valid UTF-8 prefix: a byte-capped read lands mid-char on
    // any CJK file, and `from_utf8_lossy` would emit a U+FFFD for it.
    let text = match std::str::from_utf8(&buf) {
        Ok(s) => s.to_string(),
        Err(e) => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
    };
    let table = table_name(&path);
    let payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "text": text,
        "truncated": truncated,
    });
    audit(home_dir, agent_id, "file_read", &path, &table, None, true);
    files_json(&payload)
}

// ── Protected mode (native isolation) ───────────────────────────────────────

/// Generic protected-mode refusals. Fixed text: never a path, a file name or a
/// host location. The brokered root's specific reasons are
/// `WorkBrokerError::user_message`, which is fixed text as well.
const ISOLATED_FILE_REFUSED: &str = "無法讀取這次執行授權範圍內的文字檔。";
const ISOLATED_TABLE_REFUSED: &str = "無法解析這次執行授權範圍內的資料檔。";

type IsolatedInput = (duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall,
    duduclaw_mcp_boundary::resources::AuthorizedFile);

/// H11: a refused protected open → (user text, audit reason code). Root's
/// typed refusal keeps its specific message; a boundary refusal (bad path,
/// missing grant, run not live, a descriptor that is not a sealed snapshot…)
/// keeps the tool's generic text so it cannot be used as an oracle.
fn isolated_refusal(refusal: FileRefusal, generic: &'static str) -> (&'static str, &'static str) {
    match refusal.broker() {
        Some(error) => (error.user_message(), error.code()),
        None => (generic, "isolation_resource"),
    }
}

fn refuse_isolated(home_dir: &Path, agent_id: &str, tool: &str, refusal: FileRefusal,
    generic: &'static str) -> Value {
    let (message, reason) = isolated_refusal(refusal, generic);
    audit_isolated_refusal(home_dir, agent_id, tool, reason);
    files_error(message)
}

/// Resolve the authorized input on the blocking pool: a brokered
/// `employee-work` open is a root IPC round trip (up to H5's 12 s bound) and
/// must not stall the async MCP worker. The run, employee and root come only
/// from the verified capability inside `authorize_file_with_refusal`.
async fn authorize_isolated_input(call: &crate::mcp_isolation::IsolatedCall, tool: &'static str,
    arguments: &Value) -> Result<IsolatedInput, FileRefusal> {
    let (roots, context, arguments) = (call.roots.clone(), call.context.clone(), arguments.clone());
    tokio::task::spawn_blocking(move || roots.authorize_file_with_refusal(&context, tool, &arguments))
        .await
        .unwrap_or(Err(FileRefusal::Verification(duduclaw_mcp_boundary::VerificationError::Unavailable)))
}

/// Argument checks shared by both protected `file_read` entry points. Runs
/// before any root/dirfd open; `Err` is the finished tool result.
fn isolated_file_read_arguments(arguments: &Value, agent_id: &str,
    call: &crate::mcp_isolation::IsolatedCall) -> Result<(), Value> {
    let fail = || files_error(ISOLATED_FILE_REFUSED);
    if call.agent != agent_id || duduclaw_mcp_boundary::check_arguments(arguments).is_err() {
        return Err(fail());
    }
    let Some(object) = arguments.as_object() else { return Err(fail()); };
    if object.keys().any(|key| !matches!(key.as_str(), "workspace" | "path" | "max_bytes")) {
        return Err(fail());
    }
    let path = duduclaw_mcp_boundary::resources::relative_path(arg_str(arguments, "path"))
        .map_err(|_| fail())?;
    if structured_extension(path).is_some() {
        return Err(files_error("此工具只讀純文字；請以 csv_read 或 xlsx_read 讀取結構化資料檔。"));
    }
    Ok(())
}

fn isolated_file_read(arguments: &Value, home_dir: &Path, agent_id: &str,
    call: &crate::mcp_isolation::IsolatedCall) -> Value {
    if let Err(refused) = isolated_file_read_arguments(arguments, agent_id, call) { return refused; }
    let input = call.roots.authorize_file_with_refusal(&call.context, "file_read", arguments);
    finish_isolated_file_read(input, arguments, home_dir, agent_id)
}

/// Protected `file_read` as dispatched by `mcp::dispatch`: the open (root's
/// broker for `employee-work`, openat2 for a dirfd root) and the bounded read
/// run on the blocking pool. Without an isolated call it refuses; it never
/// serves the legacy host-path reader.
pub async fn handle_isolated_file_read(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let Some(call) = crate::mcp_isolation::current() else { return files_error(ISOLATED_FILE_REFUSED); };
    if let Err(refused) = isolated_file_read_arguments(arguments, agent_id, &call) { return refused; }
    let (roots, context) = (call.roots.clone(), call.context.clone());
    drop(call);
    let (arguments, home, agent) = (arguments.clone(), home_dir.to_owned(), agent_id.to_owned());
    tokio::task::spawn_blocking(move || {
        let input = roots.authorize_file_with_refusal(&context, "file_read", &arguments);
        finish_isolated_file_read(input, &arguments, &home, &agent)
    }).await.unwrap_or_else(|_| files_error(ISOLATED_FILE_REFUSED))
}

fn finish_isolated_file_read(input: Result<IsolatedInput, FileRefusal>, arguments: &Value,
    home_dir: &Path, agent_id: &str) -> Value {
    use std::io::Read;
    let fail = || files_error(ISOLATED_FILE_REFUSED);
    let file = match input {
        Ok((_, file)) => file,
        Err(refusal) => return refuse_isolated(home_dir, agent_id, "file_read", refusal, ISOLATED_FILE_REFUSED),
    };
    let raw_path = arg_str(arguments, "path");
    let Ok(path) = duduclaw_mcp_boundary::resources::relative_path(raw_path) else { return fail(); };
    // The descriptor is either an openat2-opened inode of a dirfd root or, for
    // `employee-work`, root's sealed read-only memfd snapshot (nlink 0, fixed
    // size). Only its length is used; nothing assumes a linked file and
    // nothing is reopened by path. Every read rechecks live authority.
    let total = match file.metadata() { Ok(metadata) => metadata.len(), Err(_) => return fail() };
    let max_bytes = arg_usize(arguments, "max_bytes").map(|n| n as u64)
        .unwrap_or(FILE_READ_MAX_BYTES).clamp(1, FILE_READ_MAX_BYTES);
    let mut bytes = Vec::new();
    if file.take(max_bytes).read_to_end(&mut bytes).is_err() { return fail(); }
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_owned(),
        Err(error) => String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned(),
    };
    let table = table_name(path);
    audit(home_dir, agent_id, "file_read", path, &table, None, true);
    files_json(&json!({"path": raw_path, "table": table, "text": text,
        "truncated": total > bytes.len() as u64}))
}

/// The file's extension when it is one `file_read` must not swallow.
fn structured_extension(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    STRUCTURED_EXTENSIONS.contains(&ext.as_str()).then_some(ext)
}

/// Read at most `max_bytes` from `path` without materializing the whole file.
fn read_prefix(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(max_bytes).read_to_end(&mut buf)?;
    Ok(buf)
}

// ── csv_read ────────────────────────────────────────────────────────────────

/// Isolated table readers keep the original dispatch gates, but send the exact
/// authorized fd to E instead of invoking a parser with the gateway's identity.
/// For `employee-work` that fd is root's sealed snapshot (H11); the installed
/// parser runs E's ReadTable op on it (gateway `isolation_execution_worker`).
pub async fn handle_isolated_table_read(tool: &str, arguments: &Value,
    home_dir: &Path, agent_id: &str) -> Value {
    use duduclaw_mcp_boundary::parsing::{TableKind, TableOptions, authorized_table_request, decode_table_response};
    let fail = || files_error(ISOLATED_TABLE_REFUSED);
    let Some(call) = crate::mcp_isolation::current() else { return fail(); };
    let Some(parser) = call.parser.as_ref() else { return fail(); };
    if call.agent != agent_id || duduclaw_mcp_boundary::check_arguments(arguments).is_err() {
        return fail();
    }
    let Some(object) = arguments.as_object() else { return fail(); };
    let (tool, kind): (&'static str, _) = match tool {
        "csv_read" => ("csv_read", TableKind::Csv),
        "xlsx_read" => ("xlsx_read", TableKind::Spreadsheet),
        _ => return fail(),
    };
    if object.keys().any(|key| match key.as_str() {
        "workspace" | "path" | "offset" | "limit" => false,
        "delimiter" | "has_header" => kind != TableKind::Csv,
        "sheet" => kind != TableKind::Spreadsheet,
        _ => true,
    }) { return fail(); }
    let delimiter = match resolve_delimiter(arguments) { Ok(value) => value, Err(_) => return fail() };
    let options = TableOptions { kind, delimiter, has_header: arg_bool(arguments, "has_header").unwrap_or(true),
        offset: resolve_offset(arguments), limit: resolve_limit(arguments),
        sheet: arguments.get("sheet").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned) };
    if options.validate().is_err() { return fail(); }
    let raw_path = arg_str(arguments, "path");
    let path = match duduclaw_mcp_boundary::resources::relative_path(raw_path) {
        Ok(path) => path, Err(_) => return fail(),
    };
    let (authorized, input) = match authorize_isolated_input(&call, tool, arguments).await {
        Ok(input) => input,
        Err(refusal) => return refuse_isolated(home_dir, agent_id, tool, refusal, ISOLATED_TABLE_REFUSED),
    };
    let request = match authorized_table_request(&input, &options) { Ok(request) => request, Err(_) => return fail() };
    let response = match tokio::time::timeout(std::time::Duration::from_secs(30),
        parser.parse(authorized, request.clone(), input, options.clone())).await {
        Ok(Ok(response)) => response, _ => return fail(),
    };
    let mut payload = match decode_table_response(&request, &options, response) {
        Ok(payload) => payload, Err(_) => return fail(),
    };
    let table = table_name(path);
    payload["path"] = json!(raw_path);
    payload["table"] = json!(table);
    let rows = payload["row_count"].as_u64().map(|n| n as usize);
    audit(home_dir, agent_id, tool, path, &table, rows, true);
    files_json(&payload)
}

/// Read a delimited text file into `{columns, rows, row_count, truncated}`.
///
/// Cells are always strings: a CSV has no types, and guessing them (is
/// `0912345678` a number or a phone?) would both mangle data and hand the
/// redaction rules a shape that changes per file.
pub fn handle_csv_read(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    if crate::mcp_isolation::current().is_some() {
        return files_error("隔離執行必須透過授權的資料解析服務。");
    }
    if duduclaw_parse_worker::client::required() {
        return read_isolated_table(arguments, home_dir, agent_id, false);
    }
    let raw_path = arg_str(arguments, "path");
    let path = match vet_path(raw_path, home_dir, agent_id) {
        Ok(p) => p,
        Err(msg) => {
            audit_refusal(home_dir, agent_id, "csv_read", raw_path);
            return files_error(&msg);
        }
    };
    let len = file_len(&path);
    if len > CSV_MAX_FILE_BYTES {
        audit_refusal(home_dir, agent_id, "csv_read", raw_path);
        return files_error(&too_large(raw_path, len, CSV_MAX_FILE_BYTES));
    }
    let delimiter = match resolve_delimiter(arguments) {
        Ok(d) => d,
        Err(msg) => return files_error(&msg),
    };
    let has_header = arg_bool(arguments, "has_header").unwrap_or(true);
    let limit = resolve_limit(arguments);
    let offset = resolve_offset(arguments);

    let file = match guarded_fs(
        raw_path,
        &path,
        |e| format!("讀取「{raw_path}」失敗：{e}"),
        |p| std::fs::File::open(p),
    ) {
        Ok(f) => f,
        Err(msg) => return files_error(&msg),
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        // Headers are handled here, not by the csv crate, so the no-header
        // case can synthesize `c1..cN` from the first data record.
        .has_headers(false)
        // Ragged rows are normal in exported spreadsheets; refusing the whole
        // file over one short line would be the wrong trade.
        .flexible(true)
        .from_reader(file);

    let mut records = reader.records();
    let mut columns: Vec<String> = Vec::new();
    if has_header {
        match records.next() {
            Some(Ok(rec)) => {
                columns = normalize_columns(&rec.iter().map(str::to_string).collect::<Vec<_>>());
            }
            Some(Err(e)) => {
                return files_error(&format!("解析「{raw_path}」的表頭失敗：{e}"));
            }
            None => {
                // Empty file: an honest empty result, not an error.
                return empty_table_result(home_dir, agent_id, "csv_read", &path, None);
            }
        }
    }

    let mut rows: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut skipped = 0usize;
    for record in records {
        let record = match record {
            Ok(r) => r,
            Err(e) => {
                return files_error(&format!(
                    "解析「{raw_path}」第 {} 列失敗：{e}",
                    skipped + rows.len() + 1
                ));
            }
        };
        if skipped < offset {
            skipped += 1;
            continue;
        }
        if rows.len() >= limit {
            truncated = true;
            break;
        }
        if columns.is_empty() {
            columns = synthetic_columns(record.len());
        }
        let cells: Vec<Value> = record
            .iter()
            .map(|c| Value::String(c.to_string()))
            .collect();
        rows.push(row_object(&columns, cells, empty_string));
    }

    let table = table_name(&path);
    let row_count = rows.len();
    let payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "columns": columns,
        "rows": rows,
        "row_count": row_count,
        "truncated": truncated,
    });
    audit(
        home_dir,
        agent_id,
        "csv_read",
        &path,
        &table,
        Some(row_count),
        true,
    );
    files_json(&payload)
}

fn empty_table_result(
    home_dir: &Path,
    agent_id: &str,
    tool: &str,
    path: &Path,
    sheet: Option<(&str, Vec<String>)>,
) -> Value {
    let table = table_name(path);
    let mut payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "columns": [],
        "rows": [],
        "row_count": 0,
        "truncated": false,
    });
    if let Some((name, sheets)) = sheet
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("sheet".into(), json!(name));
        obj.insert("sheets".into(), json!(sheets));
    }
    audit(home_dir, agent_id, tool, path, &table, Some(0), true);
    files_json(&payload)
}

// ── xlsx_read ───────────────────────────────────────────────────────────────

/// Read one worksheet of a workbook (xlsx / xlsm / xls / ods) into the same
/// shape `csv_read` returns, plus `sheet` and `sheets`.
pub fn handle_xlsx_read(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    if crate::mcp_isolation::current().is_some() {
        return files_error("隔離執行必須透過授權的資料解析服務。");
    }
    use calamine::Reader;

    if duduclaw_parse_worker::client::required() {
        return read_isolated_table(arguments, home_dir, agent_id, true);
    }

    let raw_path = arg_str(arguments, "path");
    let path = match vet_path(raw_path, home_dir, agent_id) {
        Ok(p) => p,
        Err(msg) => {
            audit_refusal(home_dir, agent_id, "xlsx_read", raw_path);
            return files_error(&msg);
        }
    };
    let len = file_len(&path);
    if len > XLSX_MAX_FILE_BYTES {
        audit_refusal(home_dir, agent_id, "xlsx_read", raw_path);
        return files_error(&too_large(raw_path, len, XLSX_MAX_FILE_BYTES));
    }
    let limit = resolve_limit(arguments);
    let offset = resolve_offset(arguments);

    let mut workbook = match calamine::open_workbook_auto(&path) {
        Ok(w) => w,
        Err(e) => {
            return files_error(&format!(
                "無法開啟「{raw_path}」：{e}。支援 xlsx／xlsm／xls／ods。"
            ));
        }
    };
    let sheets = workbook.sheet_names().to_vec();
    if sheets.is_empty() {
        return files_error(&format!("「{raw_path}」沒有任何工作表。"));
    }
    let requested = arg_str(arguments, "sheet").trim().to_string();
    let sheet = if requested.is_empty() {
        sheets[0].clone()
    } else {
        match sheets.iter().find(|s| s.as_str() == requested) {
            Some(s) => s.clone(),
            None => {
                return files_error(&format!(
                    "「{raw_path}」沒有名為「{requested}」的工作表。可用的工作表：{}。",
                    sheets.join("、")
                ));
            }
        }
    };
    let range = match workbook.worksheet_range(&sheet) {
        Ok(r) => r,
        Err(e) => return files_error(&format!("讀取工作表「{sheet}」失敗：{e}")),
    };

    let mut iter = range.rows();
    let header = match iter.next() {
        Some(h) => h,
        None => {
            return empty_table_result(
                home_dir,
                agent_id,
                "xlsx_read",
                &path,
                Some((&sheet, sheets)),
            );
        }
    };
    let columns = normalize_columns(&header.iter().map(cell_to_header_string).collect::<Vec<_>>());

    let mut rows: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut skipped = 0usize;
    for record in iter {
        if skipped < offset {
            skipped += 1;
            continue;
        }
        if rows.len() >= limit {
            truncated = true;
            break;
        }
        let cells: Vec<Value> = record.iter().map(cell_to_json).collect();
        rows.push(row_object(&columns, cells, json_null));
    }

    let table = table_name(&path);
    let row_count = rows.len();
    let payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "sheet": sheet,
        "sheets": sheets,
        "columns": columns,
        "rows": rows,
        "row_count": row_count,
        "truncated": truncated,
    });
    audit(
        home_dir,
        agent_id,
        "xlsx_read",
        &path,
        &table,
        Some(row_count),
        true,
    );
    files_json(&payload)
}

/// Package D authorizes the path before this adapter snapshots the file. The
/// worker returns bounded data; normal table shaping, auditing and redaction
/// remain in the caller just as for the non-protected reader.
fn read_isolated_table(arguments: &Value, home_dir: &Path, agent_id: &str, spreadsheet: bool) -> Value {
    use duduclaw_parse_worker::worker::{TableFormat, TableOptions};
    let tool = if spreadsheet { "xlsx_read" } else { "csv_read" };
    let raw_path = arg_str(arguments, "path");
    let path = match vet_path(raw_path, home_dir, agent_id) {
        Ok(path) => path,
        Err(error) => {
            audit_refusal(home_dir, agent_id, tool, raw_path);
            return files_error(&error);
        }
    };
    let max_bytes = if spreadsheet { XLSX_MAX_FILE_BYTES } else { CSV_MAX_FILE_BYTES };
    let len = file_len(&path);
    if len > max_bytes {
        audit_refusal(home_dir, agent_id, tool, raw_path);
        return files_error(&too_large(raw_path, len, max_bytes));
    }
    let delimiter = if spreadsheet { b',' } else {
        match resolve_delimiter(arguments) {
            Ok(delimiter) => delimiter,
            Err(error) => return files_error(&error),
        }
    };
    let Ok(offset) = u32::try_from(resolve_offset(arguments)) else {
        return files_error("起始列超過隔離解析上限。");
    };
    let requested = arg_str(arguments, "sheet").trim();
    let sheet = if spreadsheet && !requested.is_empty() {
        match duduclaw_parse_worker::bounded::Text::new(requested.to_string()) {
            Ok(sheet) => Some(sheet),
            Err(_) => return files_error("工作表名稱超過隔離解析上限。"),
        }
    } else { None };
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match open.open(&path) {
        Ok(file) if file.metadata().is_ok_and(|meta| meta.is_file() && meta.len() <= max_bytes) => file,
        _ => return files_error("無法安全讀取這份檔案，已停止解析。"),
    };
    let result = duduclaw_parse_worker::adapters::read_table_file(&mut file, TableOptions {
        format: if spreadsheet { TableFormat::Spreadsheet } else { TableFormat::Csv },
        delimiter,
        has_header: spreadsheet || arg_bool(arguments, "has_header").unwrap_or(true),
        offset,
        limit: resolve_limit(arguments) as u16,
        sheet,
    });
    let output = match result {
        Ok(output) => output,
        Err(_) => {
            audit_refusal(home_dir, agent_id, tool, raw_path);
            return files_error("隔離解析服務無法讀取這份表格，請確認格式及大小後重試。");
        }
    };
    let (columns, rows) = isolated_table_rows(&output, spreadsheet);
    let table = table_name(&path);
    let row_count = rows.len();
    let mut payload = json!({ "path": path.to_string_lossy(), "table": table,
        "columns": columns, "rows": rows, "row_count": row_count, "truncated": output.truncated });
    if spreadsheet {
        payload["sheet"] = json!(output.sheet.as_ref().map(|s| s.as_str()));
        payload["sheets"] = json!(output.sheets.as_slice());
    }
    audit(home_dir, agent_id, tool, &path, &table, Some(row_count), true);
    files_json(&payload)
}

fn isolated_table_rows(output: &duduclaw_parse_worker::worker::TableOutput, spreadsheet: bool) -> (Vec<String>, Vec<Value>) {
    use duduclaw_parse_worker::worker::TableCell;
    let columns: Vec<String> = output.columns.as_slice().iter().map(|s| s.as_str().to_owned()).collect();
    let missing: fn() -> Value = if spreadsheet { json_null } else { empty_string };
    let rows: Vec<Value> = output.rows.as_slice().iter().map(|row| {
        let cells = row.as_slice().iter().map(|cell| match cell {
            TableCell::Null => Value::Null,
            TableCell::Bool(value) => Value::Bool(*value),
            TableCell::Number(value) => Value::Number(value.clone()),
            TableCell::String(value) => Value::String(value.as_str().to_owned()),
        }).collect();
        row_object(&columns, cells, missing)
    }).collect();
    (columns, rows)
}

/// §14.2 cell mapping: number → number, bool → bool, empty → null,
/// date/time → ISO 8601 string, everything else → string.
pub fn cell_to_json(cell: &calamine::Data) -> Value {
    use calamine::Data;
    match cell {
        Data::Int(i) => json!(i),
        Data::Float(f) => {
            // A worksheet stores every number as f64; an integral one reads
            // far better as `3` than `3.0` on the wire, and a JsonPath rule
            // comparing an id column should see an integer.
            if f.fract() == 0.0 && f.abs() < 9.0e15 {
                json!(*f as i64)
            } else {
                json!(f)
            }
        }
        Data::Bool(b) => json!(b),
        Data::Empty => Value::Null,
        Data::String(s) => json!(s),
        Data::DateTimeIso(s) => json!(s),
        Data::DurationIso(s) => json!(s),
        Data::DateTime(dt) => match dt.as_datetime() {
            // Seconds precision: a serial-date round-trip has sub-second
            // float noise that is never real data.
            Some(ndt) => json!(ndt.format("%Y-%m-%dT%H:%M:%S").to_string()),
            None => json!(dt.as_f64()),
        },
        Data::Error(e) => json!(format!("#ERROR:{e:?}")),
    }
}

/// Header cells are always rendered as text — a numeric header is still a
/// column name, and `normalize_columns` needs a `String`.
fn cell_to_header_string(cell: &calamine::Data) -> String {
    match cell_to_json(cell) {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ── Audit ───────────────────────────────────────────────────────────────────

/// A refused read still gets a row: an agent reaching for a path outside the
/// fence, or for a file over the cap, is exactly the thing an operator wants
/// to be able to see afterwards. The REQUESTED path is recorded (there is no
/// canonical one when the fence rejected it) and nothing else — a refusal
/// never read any content to leak.
fn audit_refusal(home_dir: &Path, agent_id: &str, tool: &str, requested: &str) {
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        agent_id,
        tool,
        "refused",
        false,
        &[("path", json!(requested)), ("refused", json!(true))],
    );
}

/// Protected-mode refusal row (H11): the stable reason code only
/// (`work_file_not_found`, `workspace_broker_unavailable`, …, or
/// `isolation_resource` for a boundary refusal). Neither the employee-chosen
/// path nor any host location is recorded for a refused protected open.
fn audit_isolated_refusal(home_dir: &Path, agent_id: &str, tool: &str, reason: &str) {
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        agent_id,
        tool,
        "refused",
        false,
        &[("refused", json!(true)), ("reason", json!(reason))],
    );
}

/// One audit row per call. Records the read's SHAPE only — path, table name,
/// and how many rows came back. Cell content never goes to `tool_calls.jsonl`.
fn audit(
    home_dir: &Path,
    agent_id: &str,
    tool: &str,
    path: &Path,
    table: &str,
    row_count: Option<usize>,
    success: bool,
) {
    let mut extras: Vec<(&str, Value)> = vec![
        ("path", json!(path.to_string_lossy())),
        ("table", json!(table)),
    ];
    if let Some(n) = row_count {
        extras.push(("row_count", json!(n)));
    }
    let summary = match row_count {
        Some(n) => format!("table={table} rows={n}"),
        None => format!("table={table}"),
    };
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir, agent_id, tool, &summary, success, &extras,
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_isolation_table_preserves_types_and_ragged_rows() {
        let output = serde_json::from_value(serde_json::json!({
            "columns": ["name", "value"],
            "rows": [[{"kind":"string","value":"王"}, {"kind":"number","value":42}, {"kind":"bool","value":true}],
                [{"kind":"string","value":"missing"}]],
            "truncated": true, "sheet": null, "sheets": []
        })).unwrap();
        let (columns, csv) = super::isolated_table_rows(&output, false);
        assert_eq!(columns, ["name", "value"]);
        assert_eq!(csv[0], serde_json::json!({"name":"王", "value":42, "c3":true}));
        assert_eq!(csv[1]["value"], "");
        let (_, sheet) = super::isolated_table_rows(&output, true);
        assert!(sheet[1]["value"].is_null());
    }

    use super::*;

    fn tmp_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// An identified employee: `agents/<id>/agent.toml` exists (what
    /// [`employee_dir`] requires for a non-ephemeral id).
    fn agent_dir(home: &Path, id: &str) -> PathBuf {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), format!("[agent]\nname = \"{id}\"\n")).unwrap();
        dir
    }

    fn payload(v: &Value) -> Value {
        let text = v
            .pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("{}");
        serde_json::from_str(text).unwrap_or(Value::Null)
    }

    fn is_error(v: &Value) -> bool {
        v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false)
    }

    fn err_text(v: &Value) -> String {
        v.pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    }

    // ── Path fence ──────────────────────────────────────────────────────────

    #[test]
    fn vet_path_accepts_a_file_inside_the_agent_dir() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("customers.csv");
        std::fs::write(&file, "id,name\n1,Amy\n").unwrap();
        let ok = vet_path(file.to_str().unwrap(), home.path(), "sales").unwrap();
        assert!(ok.ends_with("customers.csv"));
    }

    #[test]
    fn vet_path_accepts_the_home_attachments_dir() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let att = home.path().join("attachments");
        std::fs::create_dir_all(&att).unwrap();
        let file = att.join("report.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        assert!(vet_path(file.to_str().unwrap(), home.path(), "sales").is_ok());
    }

    #[test]
    fn vet_path_refuses_a_path_outside_the_roots_and_names_them() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("secret.csv");
        std::fs::write(&file, "x\n").unwrap();
        let err = vet_path(file.to_str().unwrap(), home.path(), "sales").unwrap_err();
        assert!(err.contains("不在允許的目錄範圍內"), "{err}");
        assert!(err.contains("agents"), "root list must be reported: {err}");
    }

    #[test]
    fn vet_path_refuses_parent_dir_components() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let traversal = format!("{}/../../../etc/hosts", dir.display());
        let err = vet_path(&traversal, home.path(), "sales").unwrap_err();
        assert!(err.contains(".."), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn vet_path_refuses_a_symlink_escaping_the_roots() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("payroll.csv");
        std::fs::write(&target, "x\n").unwrap();
        let link = dir.join("innocent.csv");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = vet_path(link.to_str().unwrap(), home.path(), "sales").unwrap_err();
        assert!(err.contains("不在允許的目錄範圍內"), "{err}");
    }

    #[test]
    fn vet_path_refuses_a_directory() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let err = vet_path(dir.to_str().unwrap(), home.path(), "sales").unwrap_err();
        assert!(err.contains("不是一般檔案"), "{err}");
    }

    #[test]
    fn configured_roots_widen_the_fence() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let extra = tempfile::tempdir().unwrap();
        let canonical = extra.path().canonicalize().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[files]\nallowed_roots = [\"{}\"]\n",
                canonical.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let file = canonical.join("exports.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        assert!(vet_path(file.to_str().unwrap(), home.path(), "sales").is_ok());
    }

    #[test]
    fn relative_configured_roots_are_ignored() {
        let home = tmp_home();
        std::fs::write(
            home.path().join("config.toml"),
            "[files]\nallowed_roots = [\"relative/dir\", \"\"]\n",
        )
        .unwrap();
        assert!(configured_roots(home.path()).is_empty());
    }

    #[test]
    fn an_invalid_agent_id_gets_no_agent_roots() {
        let home = tmp_home();
        let roots = allowed_roots(home.path(), "../evil");
        assert_eq!(roots.len(), 1, "only <home>/attachments: {roots:?}");
        assert!(roots[0].ends_with("attachments"));
    }

    // ── Guarded filesystem calls (RM-155) ───────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn a_protected_eperm_names_the_macos_permission_and_steps() {
        let err = guarded_fs_with::<(), _, _>(
            "~/Documents/客戶.csv",
            Path::new("/Users/amy/Documents/客戶.csv"),
            true,
            std::time::Duration::from_secs(2),
            |e| format!("讀取失敗：{e}"),
            |_| Err(std::io::Error::from_raw_os_error(1)),
        )
        .unwrap_err();
        assert!(err.contains("macOS"), "{err}");
        assert!(err.contains("系統設定 › 隱私權與安全性 › 檔案和資料夾"), "{err}");
        assert!(err.contains("System Settings › Privacy & Security › Files and Folders"), "{err}");
        assert!(err.contains("~/Documents/客戶.csv"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unprotected_or_posix_failure_keeps_the_generic_text() {
        let eperm_elsewhere = guarded_fs_with::<(), _, _>(
            "/srv/a.csv",
            Path::new("/srv/a.csv"),
            false,
            std::time::Duration::from_secs(2),
            |e| format!("讀取失敗：{e}"),
            |_| Err(std::io::Error::from_raw_os_error(1)),
        )
        .unwrap_err();
        assert!(eperm_elsewhere.starts_with("讀取失敗："), "{eperm_elsewhere}");
        let eacces = guarded_fs_with::<(), _, _>(
            "~/Documents/a.csv",
            Path::new("/Users/amy/Documents/a.csv"),
            true,
            std::time::Duration::from_secs(2),
            |e| format!("讀取失敗：{e}"),
            |_| Err(std::io::Error::from_raw_os_error(13)),
        )
        .unwrap_err();
        assert!(eacces.starts_with("讀取失敗：") && !eacces.contains("檔案和資料夾"), "{eacces}");
    }

    #[test]
    fn a_blocked_protected_read_says_macos_is_waiting_for_allow() {
        let err = guarded_fs_with::<(), _, _>(
            "~/Documents/a.csv",
            // Own path: the shared limiter de-duplicates per path, and this
            // op stays in flight after the test returns.
            Path::new("/Users/amy/Documents/blocked-protected.csv"),
            true,
            std::time::Duration::from_millis(50),
            |e| e.to_string(),
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(600));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("macOS is waiting for someone on this Mac to click Allow"), "{err}");
        assert!(err.contains("按「允許」"), "{err}");
    }

    #[test]
    fn a_blocked_unprotected_read_is_a_plain_timeout() {
        let err = guarded_fs_with::<(), _, _>(
            "/srv/a.csv",
            Path::new("/srv/blocked-unprotected.csv"),
            false,
            std::time::Duration::from_millis(50),
            |e| e.to_string(),
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(600));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("逾時") && !err.contains("macOS"), "{err}");
    }

    #[test]
    fn a_retry_into_a_stuck_read_is_pending_without_a_new_thread() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let path = Path::new("/Users/amy/Documents/stuck-retry.csv");
        let stuck = |calls: &std::sync::Arc<std::sync::atomic::AtomicUsize>| {
            let calls = std::sync::Arc::clone(calls);
            move |_: PathBuf| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(800));
                Ok::<(), std::io::Error>(())
            }
        };
        let short = std::time::Duration::from_millis(50);
        let first = guarded_fs_with("~/Documents/stuck-retry.csv", path, true, short,
            |e| e.to_string(), stuck(&calls)).unwrap_err();
        assert!(first.contains("click Allow"), "{first}");
        let retry = guarded_fs_with("~/Documents/stuck-retry.csv", path, true, short,
            |e| e.to_string(), stuck(&calls)).unwrap_err();
        assert!(retry.contains("still waiting for macOS permission"), "{retry}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "no second thread");
        let plain = duduclaw_gateway::macos_folder_access::file_pending_text("/srv/a.csv", false);
        assert!(!plain.contains("macOS") && plain.contains("not read"), "{plain}");
    }

    #[test]
    fn guarded_calls_pass_results_through() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("ok.txt");
        std::fs::write(&file, "hello").unwrap();
        let got = guarded_fs("ok.txt", &file, |e| e.to_string(), |p| read_prefix(&p, 3)).unwrap();
        assert_eq!(got, b"hel");
        let missing = guarded_fs("nope", &dir.join("nope"), |_| "generic".to_string(), |p| {
            std::fs::metadata(p)
        })
        .unwrap_err();
        assert_eq!(missing, "generic");
    }

    // ── Directory fence + contained tree (v1.81, S1) ────────────────────────

    #[test]
    fn vet_dir_defaults_to_the_own_directory_and_resolves_relative_under_it() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let own = dir.canonicalize().unwrap();
        let vetted = vet_dir("", home.path(), "coder").unwrap();
        assert_eq!(vetted.path(), own.as_path());
        // The implicit own root is walked from the trusted `agents/`.
        assert_eq!(vetted.anchor, own.parent().unwrap());
        assert_eq!(vetted.rel, vec!["coder".to_string()]);
        assert_eq!(vet_dir("  ", home.path(), "coder").unwrap().path(), own.as_path());
        let src = vet_dir("src", home.path(), "coder").unwrap();
        assert_eq!(src.path(), own.join("src").as_path());
        assert_eq!(src.anchor, own.parent().unwrap(), "a sub-root is walked from `agents/` too");
        assert_eq!(src.rel, vec!["coder".to_string(), "src".to_string()]);
        assert_eq!(
            vet_dir(own.join("src").to_str().unwrap(), home.path(), "coder").unwrap().path(),
            own.join("src").as_path()
        );
        // A valid id with no `agent.toml` is not an employee.
        std::fs::create_dir_all(home.path().join("agents/ghost")).unwrap();
        assert!(vet_dir("", home.path(), "ghost").is_err());
    }

    /// A role member's own directory is its `.ephemeral/<id>` scaffold, not
    /// `agents/<id>`; other employees' directories stay outside the fence.
    #[test]
    fn vet_dir_resolves_a_role_member_to_its_ephemeral_directory() {
        let home = tmp_home();
        let parent = agent_dir(home.path(), "boss");
        let id = "eph-boss-r1-exec-abc123";
        let member = home.path().join("agents/.ephemeral").join(id);
        std::fs::create_dir_all(member.join("src")).unwrap();
        std::fs::write(member.join("agent.toml"), "[agent]\nname = \"m\"\n").unwrap();
        let member_real = member.canonicalize().unwrap();
        assert_eq!(employee_dir(home.path(), id).unwrap(), member_real);
        let vetted = vet_dir("", home.path(), id).unwrap();
        assert_eq!(vetted.path(), member_real.as_path());
        // Walked from `agents/`, through `.ephemeral` — never anchored at it.
        let agents_real = home.path().join("agents").canonicalize().unwrap();
        assert_eq!(vetted.anchor, agents_real);
        assert_eq!(vetted.rel, vec![".ephemeral".to_string(), id.to_string()]);
        let src = vet_dir("src", home.path(), id).unwrap();
        assert_eq!(src.path(), member_real.join("src").as_path());
        assert_eq!(src.anchor, agents_real);
        assert_eq!(src.rel, vec![".ephemeral".to_string(), id.to_string(), "src".to_string()]);
        let tree = ContainedTree::open(&vetted).expect("an ordinary role member root opens");
        assert_eq!(tree.read_to_string(Path::new("agent.toml"), 1024).unwrap(), "[agent]\nname = \"m\"\n");
        let err = vet_dir(parent.to_str().unwrap(), home.path(), id).unwrap_err();
        assert!(err.contains("不在允許的目錄範圍內"), "{err}");
        // An `eph-` id with no scaffold resolves to nothing — even when an
        // `agents/<id>/agent.toml` exists.
        let fake = "eph-boss-r1-exec-ffffff";
        agent_dir(home.path(), fake);
        assert!(employee_dir(home.path(), fake).is_none());
        assert!(vet_dir("", home.path(), fake).is_err());
    }

    #[test]
    fn vet_dir_refuses_everything_outside_the_fence() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        let other = agent_dir(home.path(), "other");
        std::fs::write(dir.join("file.rs"), "fn x() {}\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        for raw in [
            other.to_str().unwrap(),
            "/",
            outside.path().to_str().unwrap(),
            home.path().to_str().unwrap(),
        ] {
            let err = vet_dir(raw, home.path(), "coder").unwrap_err();
            assert!(err.contains("不在允許的目錄範圍內"), "{raw}: {err}");
        }
        assert!(vet_dir("../other", home.path(), "coder").unwrap_err().contains(".."));
        assert!(vet_dir("file.rs", home.path(), "coder").unwrap_err().contains("不是目錄"));
        // No identity ⇒ no own directory.
        for bad in ["", "../coder", "a/b"] {
            assert!(vet_dir("", home.path(), bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn vet_dir_refuses_a_symlinked_directory_escaping_the_fence() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("escape")).unwrap();
        let err = vet_dir("escape", home.path(), "coder").unwrap_err();
        assert!(err.contains("不在允許的目錄範圍內"), "{err}");
    }

    #[test]
    fn contained_tree_reads_regular_files_and_rejects_bad_paths() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/c.rs"), "fn c() {}\n").unwrap();
        std::fs::write(dir.join("big.rs"), vec![b'x'; 64]).unwrap();
        let root = vet_dir("", home.path(), "coder").unwrap();
        let tree = ContainedTree::open(&root).unwrap();
        assert_eq!(tree.root(), root.path());
        assert_eq!(tree.read_to_string(Path::new("a/b/c.rs"), 1024).unwrap(), "fn c() {}\n");
        assert!(tree.read_to_string(Path::new("big.rs"), 16).is_err(), "over the cap");
        assert!(tree.read_to_string(Path::new("a"), 1024).is_err(), "a directory");
        assert!(tree.read_to_string(Path::new("a/../agent.toml"), 1024).is_err());
        assert!(tree.read_to_string(Path::new("missing.rs"), 1024).is_err());
        assert!(tree.read_to_string(Path::new(""), 1024).is_err());
        assert!(tree.read_to_string(&root.path().join("a/b/c.rs"), 1024).is_err(), "absolute");
    }

    #[cfg(unix)]
    #[test]
    fn contained_tree_refuses_symlinks_hard_links_and_fifos() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("pkg")).unwrap();
        std::fs::write(outside.path().join("secret.rs"), "secret\n").unwrap();
        std::fs::write(outside.path().join("pkg/mod.rs"), "secret\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.rs"), dir.join("file_link.rs")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("pkg"), dir.join("dir_link")).unwrap();
        let root = vet_dir("", home.path(), "coder").unwrap();
        let tree = ContainedTree::open(&root).unwrap();
        assert!(tree.read_to_string(Path::new("file_link.rs"), 1024).is_err());
        assert!(tree.read_to_string(Path::new("dir_link/mod.rs"), 1024).is_err());
        if std::fs::hard_link(outside.path().join("secret.rs"), dir.join("hard.rs")).is_ok() {
            assert!(tree.read_to_string(Path::new("hard.rs"), 1024).is_err(), "nlink == 2");
        }
        let fifo = std::ffi::CString::new(dir.join("pipe.rs").to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path; mkfifo creates nothing else.
        if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } == 0 {
            assert!(tree.read_to_string(Path::new("pipe.rs"), 1024).is_err(), "must not hang");
        }
    }

    /// A directory swapped for a symlink after `ContainedTree::open` cannot
    /// redirect a later read: the walk re-opens every component no-follow.
    #[cfg(unix)]
    #[test]
    fn contained_tree_is_not_redirected_by_a_later_swap() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn inside() {}\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("a.rs"), "fn outside() {}\n").unwrap();
        let tree = ContainedTree::open(&vet_dir("", home.path(), "coder").unwrap()).unwrap();
        std::fs::rename(dir.join("src"), dir.join("old")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("src")).unwrap();
        assert!(tree.read_to_string(Path::new("src/a.rs"), 1024).is_err());
        assert_eq!(tree.read_to_string(Path::new("old/a.rs"), 1024).unwrap(), "fn inside() {}\n");
    }

    #[cfg(unix)]
    #[test]
    fn contained_tree_root_must_be_symlink_free() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        let real = dir.canonicalize().unwrap();
        std::os::unix::fs::symlink(&real, home.path().join("alias")).unwrap();
        let alias = home.path().canonicalize().unwrap().join("alias");
        let raw = |anchor: PathBuf, rel: &[&str]| VettedDir {
            path: anchor.clone(),
            anchor,
            rel: rel.iter().map(|s| s.to_string()).collect(),
            identity: None,
        };
        // A symlink on the way to the allowed root, or below it.
        assert!(ContainedTree::open(&raw(alias.clone(), &[])).is_err());
        assert!(ContainedTree::open(&raw(home.path().canonicalize().unwrap(), &["alias"])).is_err());
        assert!(ContainedTree::open(&raw(PathBuf::from("relative"), &[])).is_err());
        assert!(ContainedTree::open(&raw(real.clone(), &[".."])).is_err());
    }

    /// The Daybreak Low: a scan root replaced between [`vet_dir`] and
    /// [`ContainedTree::open`] — by a symlink to outside, or by another
    /// directory — is refused (the root is reached from the allowed root
    /// no-follow and must be the vetted inode).
    #[cfg(unix)]
    #[test]
    fn contained_tree_refuses_a_root_swapped_between_vet_and_open() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn inside() {}\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("a.rs"), "fn outside() {}\n").unwrap();

        // Specified sub-root swapped for a symlink to outside.
        let vetted = vet_dir("src", home.path(), "coder").unwrap();
        std::fs::rename(dir.join("src"), dir.join("src.old")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("src")).unwrap();
        assert!(ContainedTree::open(&vetted).is_err(), "symlink swap");

        // Specified sub-root swapped for another (fresh) directory.
        std::fs::remove_file(dir.join("src")).unwrap();
        std::fs::create_dir(dir.join("src")).unwrap();
        let err = ContainedTree::open(&vetted).err().expect("directory swap must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");

        // The implicit own root swapped for another directory.
        let vetted = vet_dir("", home.path(), "coder").unwrap();
        let agents = home.path().join("agents");
        std::fs::rename(agents.join("coder"), agents.join("coder.old")).unwrap();
        std::fs::create_dir(agents.join("coder")).unwrap();
        assert!(ContainedTree::open(&vetted).is_err(), "own-root swap");

        // Unswapped, the same vetting opens fine.
        std::fs::remove_dir(agents.join("coder")).unwrap();
        std::fs::rename(agents.join("coder.old"), agents.join("coder")).unwrap();
        let tree = ContainedTree::open(&vet_dir("src.old", home.path(), "coder").unwrap()).unwrap();
        assert_eq!(tree.read_to_string(Path::new("a.rs"), 1024).unwrap(), "fn inside() {}\n");
    }

    /// The comparison the walk applies to every component (all platforms).
    #[test]
    fn mount_crossing_comparison_fails_closed() {
        let key = |dev, mnt| MountKey { dev, mnt };
        // Same device, platform without mount ids: same mount.
        assert!(!crosses_mount(key(1, MountId::NotTracked), key(1, MountId::NotTracked)));
        // Same device, same mount id: same mount.
        assert!(!crosses_mount(key(1, MountId::Known(7)), key(1, MountId::Known(7))));
        // A same-filesystem bind mount: same st_dev, different mount id.
        assert!(crosses_mount(key(1, MountId::Known(7)), key(1, MountId::Known(8))));
        // A different device is always a crossing.
        assert!(crosses_mount(key(1, MountId::NotTracked), key(2, MountId::NotTracked)));
        assert!(crosses_mount(key(1, MountId::Known(7)), key(2, MountId::Known(7))));
        // Unknown or mixed mount ids fail closed.
        assert!(crosses_mount(key(1, MountId::Known(7)), key(1, MountId::Unknown)));
        assert!(crosses_mount(key(1, MountId::Unknown), key(1, MountId::Unknown)));
        assert!(crosses_mount(key(1, MountId::NotTracked), key(1, MountId::Known(7))));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fdinfo_mount_id_parses_and_the_kernel_reports_one() {
        use super::contained_imp::linux::{mount_id, parse_fdinfo_mnt_id};
        use std::os::fd::AsRawFd;
        assert_eq!(parse_fdinfo_mnt_id("pos:\t0\nflags:\t02100000\nmnt_id:\t29\nino:\t5\n"), Some(29));
        assert_eq!(parse_fdinfo_mnt_id("pos:\t0\n"), None);
        assert_eq!(parse_fdinfo_mnt_id("mnt_id:\tx\n"), None);
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::open(dir.path()).unwrap();
        assert!(matches!(mount_id(file.as_raw_fd()), MountId::Known(_)));
    }

    /// The S1 review Low: a same-filesystem bind mount inside the tree has the
    /// root's `st_dev`, and must still be refused. Needs root to mount; skips
    /// (passes) otherwise.
    #[cfg(target_os = "linux")]
    #[test]
    fn contained_tree_refuses_a_same_filesystem_bind_mount() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: creating a bind mount needs root");
            return;
        }
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        std::fs::create_dir_all(dir.join("mnt")).unwrap();
        std::fs::write(dir.join("ok.rs"), "fn ok() {}\n").unwrap();
        let outside = tempfile::tempdir_in(home.path()).unwrap();
        std::fs::write(outside.path().join("secret.rs"), "fn secret() {}\n").unwrap();
        let src = std::ffi::CString::new(outside.path().to_str().unwrap()).unwrap();
        let dst = std::ffi::CString::new(dir.join("mnt").to_str().unwrap()).unwrap();
        // SAFETY: NUL-terminated paths; a bind mount with no data argument.
        let rc = unsafe {
            libc::mount(src.as_ptr(), dst.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null())
        };
        if rc != 0 {
            eprintln!("skipped: bind mount failed: {}", std::io::Error::last_os_error());
            return;
        }
        struct Unmount(std::ffi::CString);
        impl Drop for Unmount {
            fn drop(&mut self) {
                // SAFETY: NUL-terminated path of the mount made above.
                unsafe { libc::umount2(self.0.as_ptr(), libc::MNT_DETACH) };
            }
        }
        let _guard = Unmount(dst.clone());
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(dir.join("mnt")).unwrap().dev(),
            std::fs::metadata(&dir).unwrap().dev(),
            "fixture: the bind mount shares the tree's st_dev"
        );
        let tree = ContainedTree::open(&vet_dir("", home.path(), "coder").unwrap()).unwrap();
        assert_eq!(tree.read_to_string(Path::new("ok.rs"), 1024).unwrap(), "fn ok() {}\n");
        let err = tree.read_to_string(Path::new("mnt/secret.rs"), 1024).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    }

    /// The Daybreak Low: the scan root ITSELF is a same-filesystem bind mount
    /// — a specified sub-root, and the implicit own directory. `vet_dir`
    /// accepts both paths (a bind mount is not a symlink); opening refuses
    /// them because the root's mount differs from its allowed root's. Needs
    /// root to mount; skips (passes) otherwise.
    #[cfg(target_os = "linux")]
    #[test]
    fn contained_tree_refuses_a_scan_root_that_is_a_bind_mount() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: creating a bind mount needs root");
            return;
        }
        struct Unmount(std::ffi::CString);
        impl Drop for Unmount {
            fn drop(&mut self) {
                // SAFETY: NUL-terminated path of a mount made below.
                unsafe { libc::umount2(self.0.as_ptr(), libc::MNT_DETACH) };
            }
        }
        fn bind(src: &Path, dst: &Path) -> Option<Unmount> {
            let s = std::ffi::CString::new(src.to_str().unwrap()).unwrap();
            let d = std::ffi::CString::new(dst.to_str().unwrap()).unwrap();
            // SAFETY: NUL-terminated paths; a bind mount with no data argument.
            let rc = unsafe {
                libc::mount(s.as_ptr(), d.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null())
            };
            if rc != 0 {
                eprintln!("skipped: bind mount failed: {}", std::io::Error::last_os_error());
                return None;
            }
            Some(Unmount(d))
        }

        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        std::fs::create_dir_all(dir.join("mnt")).unwrap();
        let outside = tempfile::tempdir_in(home.path()).unwrap();
        std::fs::write(outside.path().join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        std::fs::write(outside.path().join("secret.rs"), "fn secret() {}\n").unwrap();

        // 1. A specified root that is itself a bind mount.
        let Some(_m1) = bind(outside.path(), &dir.join("mnt")) else { return };
        let vetted = vet_dir("mnt", home.path(), "coder").expect("a bind mount is not a symlink");
        let err = ContainedTree::open(&vetted).err().expect("bind-mounted root must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");

        // 2. The implicit own directory is itself a bind mount: compared with
        // its parent `agents/`.
        let own = home.path().join("agents").join("mounted");
        std::fs::create_dir_all(&own).unwrap();
        let Some(_m2) = bind(outside.path(), &own) else { return };
        let vetted = vet_dir("", home.path(), "mounted").expect("agent.toml is visible through the mount");
        let err = ContainedTree::open(&vetted).err().expect("bind-mounted own root must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    }

    /// Bind-mount helpers for the round-2 tests below (same mount call,
    /// unmount guard and skip message as the tests above).
    #[cfg(target_os = "linux")]
    struct Unmount(std::ffi::CString);

    #[cfg(target_os = "linux")]
    impl Drop for Unmount {
        fn drop(&mut self) {
            // SAFETY: NUL-terminated path of a mount made by `bind_mount`.
            unsafe { libc::umount2(self.0.as_ptr(), libc::MNT_DETACH) };
        }
    }

    #[cfg(target_os = "linux")]
    fn bind_mount(src: &Path, dst: &Path) -> Option<Unmount> {
        let s = std::ffi::CString::new(src.to_str().unwrap()).unwrap();
        let d = std::ffi::CString::new(dst.to_str().unwrap()).unwrap();
        // SAFETY: NUL-terminated paths; a bind mount with no data argument.
        let rc = unsafe {
            libc::mount(s.as_ptr(), d.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null())
        };
        if rc != 0 {
            eprintln!("skipped: bind mount failed: {}", std::io::Error::last_os_error());
            return None;
        }
        Some(Unmount(d))
    }

    /// Daybreak round 2 Low (a): the own directory `agents/<id>` is itself a
    /// same-filesystem bind mount and the caller names a sub-root
    /// (`root = "src"`). The walk starts at `agents/`, so `<id>` is compared
    /// with the `agents/` mount and refused — the bind mount is never the
    /// baseline. Needs root to mount; skips (passes) otherwise.
    #[cfg(target_os = "linux")]
    #[test]
    fn contained_tree_refuses_a_sub_root_of_a_bind_mounted_own_root() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: creating a bind mount needs root");
            return;
        }
        let home = tmp_home();
        let outside = tempfile::tempdir_in(home.path()).unwrap();
        std::fs::write(outside.path().join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(outside.path().join("src")).unwrap();
        std::fs::write(outside.path().join("src/secret.rs"), "fn secret() {}\n").unwrap();
        let own = home.path().join("agents").join("mounted");
        std::fs::create_dir_all(&own).unwrap();
        let Some(_m) = bind_mount(outside.path(), &own) else { return };
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&own).unwrap().dev(),
            std::fs::metadata(home.path().join("agents")).unwrap().dev(),
            "fixture: the bind mount shares the agents/ st_dev"
        );

        let vetted = vet_dir("src", home.path(), "mounted").expect("a bind mount is not a symlink");
        assert_eq!(vetted.rel, vec!["mounted".to_string(), "src".to_string()]);
        let err = ContainedTree::open(&vetted).err().expect("sub-root of a bind-mounted own root must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        // The absolute spelling of the same directory is refused too.
        let abs = own.join("src");
        let vetted = vet_dir(abs.to_str().unwrap(), home.path(), "mounted").unwrap();
        let err = ContainedTree::open(&vetted).err().expect("absolute sub-root must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    }

    /// Daybreak round 2 Low (b): `agents/.ephemeral` is itself a
    /// same-filesystem bind mount. A role member's default root is walked
    /// from `agents/` through `.ephemeral`, which is compared with the
    /// `agents/` mount and refused. Needs root to mount; skips (passes)
    /// otherwise.
    #[cfg(target_os = "linux")]
    #[test]
    fn contained_tree_refuses_a_role_member_under_a_bind_mounted_ephemeral_dir() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: creating a bind mount needs root");
            return;
        }
        let home = tmp_home();
        agent_dir(home.path(), "boss");
        let id = "eph-boss-r1-exec-abc123";
        let outside = tempfile::tempdir_in(home.path()).unwrap();
        std::fs::create_dir_all(outside.path().join(id)).unwrap();
        std::fs::write(outside.path().join(id).join("agent.toml"), "[agent]\nname = \"m\"\n").unwrap();
        std::fs::write(outside.path().join(id).join("secret.rs"), "fn secret() {}\n").unwrap();
        let ephemeral = home.path().join("agents/.ephemeral");
        std::fs::create_dir_all(&ephemeral).unwrap();
        let Some(_m) = bind_mount(outside.path(), &ephemeral) else { return };

        let vetted = vet_dir("", home.path(), id).expect("the scaffold is visible through the mount");
        assert_eq!(vetted.rel, vec![".ephemeral".to_string(), id.to_string()]);
        let err = ContainedTree::open(&vetted).err().expect("role member under a bind-mounted .ephemeral must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    }

    /// Daybreak round 2 Low (c): the own directory is swapped for a bind
    /// mount between [`vet_dir`] and [`ContainedTree::open`] — of an outside
    /// tree, and of the original directory itself (so the vetted inode
    /// identity still matches and only the mount comparison at `<id>` can
    /// refuse it). Needs root to mount; skips (passes) otherwise.
    #[cfg(target_os = "linux")]
    #[test]
    fn contained_tree_refuses_an_own_root_swapped_for_a_bind_mount_between_vet_and_open() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: creating a bind mount needs root");
            return;
        }
        let home = tmp_home();
        let dir = agent_dir(home.path(), "coder");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn inside() {}\n").unwrap();
        let outside = tempfile::tempdir_in(home.path()).unwrap();
        std::fs::create_dir_all(outside.path().join("src")).unwrap();
        std::fs::write(outside.path().join("src/a.rs"), "fn outside() {}\n").unwrap();
        let agents = home.path().join("agents");

        let vetted = vet_dir("src", home.path(), "coder").unwrap();
        std::fs::rename(agents.join("coder"), agents.join("coder.old")).unwrap();
        std::fs::create_dir(agents.join("coder")).unwrap();

        // 1. Swapped for a bind mount of an outside tree.
        {
            let Some(_m) = bind_mount(outside.path(), &agents.join("coder")) else { return };
            let err = ContainedTree::open(&vetted).err().expect("outside bind-mount swap must be refused");
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        }

        // 2. Swapped for a bind mount of the original directory: `src` keeps
        // its vetted (st_dev, st_ino), so the refusal is the mount check.
        {
            let Some(_m) = bind_mount(&agents.join("coder.old"), &agents.join("coder")) else { return };
            use std::os::unix::fs::MetadataExt;
            let through = std::fs::metadata(agents.join("coder/src")).unwrap();
            assert_eq!(vetted.identity, Some((through.dev(), through.ino())), "fixture: identity still matches");
            let err = ContainedTree::open(&vetted).err().expect("self bind-mount swap must be refused");
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        }

        // Unswapped, the same vetting opens and reads the inside file.
        std::fs::remove_dir(agents.join("coder")).unwrap();
        std::fs::rename(agents.join("coder.old"), agents.join("coder")).unwrap();
        let tree = ContainedTree::open(&vetted).expect("the vetted directory is back");
        assert_eq!(tree.read_to_string(Path::new("a.rs"), 1024).unwrap(), "fn inside() {}\n");
    }

    // ── csv_read ────────────────────────────────────────────────────────────

    #[test]
    fn csv_read_returns_header_keyed_rows() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("customers.csv");
        std::fs::write(&file, "id,name,email\n1,王小明,a@b.c\n2,李小華,d@e.f\n").unwrap();

        let out = handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(!is_error(&out));
        let p = payload(&out);
        assert_eq!(p["table"], "customers.csv");
        assert_eq!(p["columns"], json!(["id", "name", "email"]));
        assert_eq!(p["row_count"], 2);
        assert_eq!(p["truncated"], false);
        assert_eq!(p["rows"][0]["name"], "王小明");
        assert_eq!(p["rows"][1]["email"], "d@e.f");
        // Cells are always strings — never coerced.
        assert!(p["rows"][0]["id"].is_string());
    }

    #[test]
    fn csv_read_synthesizes_columns_without_a_header() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("raw.csv");
        std::fs::write(&file, "1,Amy\n2,Bob\n").unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap(), "has_header": false }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["columns"], json!(["c1", "c2"]));
        assert_eq!(p["rows"][0]["c2"], "Amy");
        assert_eq!(p["row_count"], 2);
    }

    #[test]
    fn csv_read_honours_limit_offset_and_reports_truncation() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("many.csv");
        let mut body = String::from("n\n");
        for i in 0..10 {
            body.push_str(&format!("{i}\n"));
        }
        std::fs::write(&file, body).unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap(), "limit": 3, "offset": 2 }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["row_count"], 3);
        assert_eq!(p["rows"][0]["n"], "2");
        assert_eq!(p["truncated"], true);
    }

    #[test]
    fn csv_read_limit_is_clamped_to_the_maximum() {
        let args = json!({ "limit": 99_999 });
        assert_eq!(resolve_limit(&args), ROW_LIMIT_MAX);
        assert_eq!(resolve_limit(&json!({})), ROW_LIMIT_DEFAULT);
        assert_eq!(resolve_limit(&json!({ "limit": "5" })), 5);
    }

    #[test]
    fn csv_read_accepts_a_tab_delimiter() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("data.tsv");
        std::fs::write(&file, "a\tb\n1\t2\n").unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap(), "delimiter": "\\t" }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["columns"], json!(["a", "b"]));
        assert_eq!(p["rows"][0]["b"], "2");
    }

    #[test]
    fn csv_read_rejects_a_multi_char_delimiter() {
        assert!(resolve_delimiter(&json!({ "delimiter": "||" })).is_err());
        assert_eq!(resolve_delimiter(&json!({})).unwrap(), b',');
    }

    #[test]
    fn csv_read_refuses_a_path_outside_the_roots() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("payroll.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        let out = handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(is_error(&out));
        assert!(err_text(&out).contains("不在允許的目錄範圍內"));
    }

    #[test]
    fn csv_read_on_an_empty_file_is_an_empty_result_not_an_error() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("empty.csv");
        std::fs::write(&file, "").unwrap();
        let out = handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(!is_error(&out));
        assert_eq!(payload(&out)["row_count"], 0);
    }

    #[test]
    fn duplicate_and_blank_headers_never_collide() {
        let cols = normalize_columns(&["name".into(), "".into(), "name".into(), "  ".into()]);
        assert_eq!(cols.len(), 4);
        let unique: std::collections::HashSet<_> = cols.iter().collect();
        assert_eq!(unique.len(), 4, "columns must be unique: {cols:?}");
        assert_eq!(cols[0], "name");
        assert_eq!(cols[1], "c2");
    }

    #[test]
    fn ragged_rows_keep_the_full_column_key_set() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("ragged.csv");
        std::fs::write(&file, "a,b,c\n1\n1,2,3,4\n").unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["rows"][0]["b"], "");
        assert_eq!(p["rows"][1]["c4"], "4");
    }

    // ── file_read ───────────────────────────────────────────────────────────

    #[test]
    fn file_read_returns_text_and_truncation_state() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("notes.md");
        std::fs::write(&file, "# 標題\n內容\n").unwrap();
        let p = payload(&handle_file_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["table"], "notes.md");
        assert_eq!(p["text"], "# 標題\n內容\n");
        assert_eq!(p["truncated"], false);
    }

    #[test]
    fn file_read_truncates_on_a_char_boundary() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("cjk.txt");
        // Every char is 3 bytes; a 4-byte cap lands mid-char.
        std::fs::write(&file, "王小明測試").unwrap();
        let p = payload(&handle_file_read(
            &json!({ "path": file.to_str().unwrap(), "max_bytes": 4 }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["text"], "王", "must cut back to a char boundary");
        assert_eq!(p["truncated"], true);
    }

    #[test]
    fn file_read_refuses_a_structured_data_file_and_names_the_right_tool() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        for (name, expected_tool) in [
            ("customers.csv", "csv_read"),
            ("a.TSV", "csv_read"),
            ("客戶清單.xlsx", "xlsx_read"),
            ("legacy.xls", "xlsx_read"),
            ("open.ods", "xlsx_read"),
        ] {
            let file = dir.join(name);
            std::fs::write(&file, "a,b\n1,2\n").unwrap();
            let out = handle_file_read(
                &json!({ "path": file.to_str().unwrap() }),
                home.path(),
                "sales",
            );
            assert!(is_error(&out), "{name} must be refused");
            assert!(
                err_text(&out).contains(expected_tool),
                "{name} should point at {expected_tool}: {}",
                err_text(&out)
            );
        }
        // Plain text is unaffected.
        let ok = dir.join("readme.txt");
        std::fs::write(&ok, "hello").unwrap();
        assert!(!is_error(&handle_file_read(
            &json!({ "path": ok.to_str().unwrap() }),
            home.path(),
            "sales"
        )));
    }

    #[test]
    fn file_read_max_bytes_cannot_exceed_the_cap() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("big.txt");
        std::fs::write(&file, vec![b'x'; (FILE_READ_MAX_BYTES + 1024) as usize]).unwrap();
        let p = payload(&handle_file_read(
            &json!({ "path": file.to_str().unwrap(), "max_bytes": 99_999_999u64 }),
            home.path(),
            "sales",
        ));
        assert_eq!(
            p["text"].as_str().unwrap().len(),
            FILE_READ_MAX_BYTES as usize
        );
        assert_eq!(p["truncated"], true);
    }

    // ── xlsx_read ───────────────────────────────────────────────────────────

    #[test]
    fn xlsx_read_refuses_a_path_outside_the_roots() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("payroll.xlsx");
        std::fs::write(&file, b"PK\x03\x04").unwrap();
        let out = handle_xlsx_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(is_error(&out));
    }

    #[test]
    fn xlsx_read_reports_a_non_workbook_honestly() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("not-a-workbook.xlsx");
        std::fs::write(&file, b"this is not a zip").unwrap();
        let out = handle_xlsx_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(is_error(&out));
        assert!(err_text(&out).contains("無法開啟"), "{}", err_text(&out));
    }

    // ── Audit ───────────────────────────────────────────────────────────────

    fn audit_rows(home: &Path) -> Vec<Value> {
        let raw = std::fs::read_to_string(home.join("tool_calls.jsonl")).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect()
    }

    #[test]
    fn a_successful_read_audits_its_shape_and_never_a_cell() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("customers.csv");
        std::fs::write(&file, "id,name\n1,王小明\n").unwrap();

        handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );

        let rows = audit_rows(home.path());
        assert_eq!(rows.len(), 1, "one row per call: {rows:?}");
        let row = &rows[0];
        assert_eq!(row["tool_name"], "csv_read");
        assert_eq!(row["agent_id"], "sales");
        assert_eq!(row["table"], "customers.csv");
        assert_eq!(row["row_count"], 1);
        assert_eq!(row["success"], true);
        let whole = serde_json::to_string(row).unwrap();
        assert!(
            !whole.contains("王小明"),
            "cell content must never reach tool_calls.jsonl: {whole}"
        );
    }

    #[test]
    fn a_refused_read_is_audited_as_a_failure() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("payroll.csv");
        std::fs::write(&file, "salary\n999\n").unwrap();

        handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );

        let rows = audit_rows(home.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["success"], false);
        assert_eq!(rows[0]["refused"], true);
        assert_eq!(rows[0]["tool_name"], "csv_read");
    }

    #[test]
    fn cell_mapping_follows_the_contract() {
        use calamine::{Data, ExcelDateTime, ExcelDateTimeType};
        assert_eq!(cell_to_json(&Data::Int(7)), json!(7));
        assert_eq!(cell_to_json(&Data::Float(1.5)), json!(1.5));
        // Integral floats read back as integers (a worksheet has no int type).
        assert_eq!(cell_to_json(&Data::Float(3.0)), json!(3));
        assert_eq!(cell_to_json(&Data::Bool(true)), json!(true));
        assert_eq!(cell_to_json(&Data::Empty), Value::Null);
        assert_eq!(cell_to_json(&Data::String("台北".into())), json!("台北"));
        assert_eq!(
            cell_to_json(&Data::DateTimeIso("2026-09-22T10:00:00".into())),
            json!("2026-09-22T10:00:00")
        );
        // Serial 45000 is 2023-03-15 in the 1900 date system.
        let dt = Data::DateTime(ExcelDateTime::new(
            45000.0,
            ExcelDateTimeType::DateTime,
            false,
        ));
        let rendered = cell_to_json(&dt);
        let s = rendered.as_str().expect("ISO string");
        assert!(s.starts_with("2023-03-15T"), "got {s}");
    }
}
