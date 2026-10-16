//! The external programs the two binaries may run, how they are located, and
//! the environment every child gets.
//!
//! Programs are looked up by a fixed name in a fixed list of root-owned
//! directories and executed by absolute path. `PATH` from the SSH session is
//! never consulted.

use std::path::{Path, PathBuf};

/// Directories searched, in order, for a [`Tool`]. Covers both a merged-`/usr`
/// layout (the OS image's own units call `/usr/bin/systemctl`) and the
/// classic split one, so a layout change does not silently break the menu.
pub const TOOL_DIRS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// The whole environment of every child process (design §5.1). Applied after
/// `env_clear()`, so nothing from the SSH session (`TERM`, `SSH_*`, `LANG`,
/// `SYSTEMD_*`, `LD_*`) leaks into a child.
pub const CHILD_ENV: [(&str, &str); 6] = [
    ("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"),
    ("LANG", "C.UTF-8"),
    ("SYSTEMD_PAGER", "cat"),
    ("SYSTEMD_PAGERSECURE", "1"),
    ("SYSTEMD_COLORS", "0"),
    ("SYSTEMD_LESS", "FRXMK"),
];

/// Every program either binary runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Systemctl,
    Journalctl,
    Uptime,
    Free,
    Df,
    Ip,
    Resolvectl,
}

impl Tool {
    pub fn binary_name(self) -> &'static str {
        match self {
            Tool::Systemctl => "systemctl",
            Tool::Journalctl => "journalctl",
            Tool::Uptime => "uptime",
            Tool::Free => "free",
            Tool::Df => "df",
            Tool::Ip => "ip",
            Tool::Resolvectl => "resolvectl",
        }
    }
}

/// First `TOOL_DIRS/<name>` for which `is_file` holds.
pub fn resolve_tool(tool: Tool, is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    TOOL_DIRS
        .iter()
        .map(|d| Path::new(d).join(tool.binary_name()))
        .find(|p| is_file(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_in_fixed_order_to_absolute_paths() {
        let p = resolve_tool(Tool::Systemctl, |p| p == Path::new("/bin/systemctl")).unwrap();
        assert_eq!(p, Path::new("/bin/systemctl"));
        let p = resolve_tool(Tool::Ip, |p| {
            p.starts_with("/usr/sbin") || p.starts_with("/sbin")
        })
        .unwrap();
        assert_eq!(p, Path::new("/usr/sbin/ip"));
        assert!(resolve_tool(Tool::Resolvectl, |_| false).is_none());
    }

    #[test]
    fn child_env_has_no_session_variables() {
        for (k, _) in CHILD_ENV {
            assert!(!matches!(
                k,
                "TERM" | "HOME" | "SSH_ORIGINAL_COMMAND" | "LD_PRELOAD"
            ));
        }
        assert!(CHILD_ENV.contains(&("SYSTEMD_PAGERSECURE", "1")));
        assert!(CHILD_ENV.contains(&("SYSTEMD_PAGER", "cat")));
    }
}
