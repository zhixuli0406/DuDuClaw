//! G1 round 2 (2026-10): resolving symbolic links for the Write/Edit lane.
//! Split out of `mod.rs` to keep it under the project's 800-line ceiling.

use std::path::{Component, Path, PathBuf};

use crate::agent_guard::{lexical_normalize, GuardDecision};

use super::matcher::first_line;

// ── Symbolic links (G1 round 2) ──────────────────────────────────────────────

/// The real location `path` names: the deepest ancestor that exists,
/// canonicalised by the OS (so every link and every `..` after a link is
/// followed the way the kernel would), with the not-yet-existing tail
/// appended and lexically normalised.
///
/// Errors when an existing prefix cannot be canonicalised for a reason other
/// than "not found" (permission, loop, …), and when a prefix is a dangling
/// symbolic link — writing through one creates its target, which the
/// caller cannot see from here.
pub fn resolve_real_path(path: &Path) -> std::io::Result<PathBuf> {
    let comps: Vec<Component<'_>> = path.components().collect();
    for n in (1..=comps.len()).rev() {
        let prefix: PathBuf = comps[..n].iter().collect();
        match std::fs::canonicalize(&prefix) {
            Ok(real) => {
                let mut out = real;
                for c in &comps[n..] {
                    out.push(c.as_os_str());
                }
                return Ok(lexical_normalize(&out));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&prefix).is_ok_and(|m| m.file_type().is_symlink()) {
                    return Err(std::io::Error::other(format!(
                        "dangling symbolic link: {}",
                        prefix.display()
                    )));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(lexical_normalize(path))
}

/// Apply `check` to the literal (path, home) and, when that allows, to the
/// real pair; the first block wins. A resolution failure is a block.
pub(super) fn with_real_path(
    file_path: &Path,
    home: &Path,
    check: impl Fn(&Path, &Path) -> GuardDecision,
) -> GuardDecision {
    let literal = check(file_path, home);
    if !literal.is_allowed() {
        return literal;
    }
    let real = resolve_real_path(file_path).and_then(|p| Ok((p, resolve_real_path(home)?)));
    match real {
        Ok((p, h)) => {
            let d = check(&p, &h);
            if d.is_allowed() { literal } else { d }
        }
        Err(e) => GuardDecision::BlockedUnresolvablePath {
            attempted_path: lexical_normalize(file_path),
            reason: format!(
                "符號連結無法解析（懸空連結、循環連結，或上層資料夾無法讀取）：{}",
                first_line(&e.to_string())
            ),
        },
    }
}
