//! Windows host path → WSL path for `docker -v` inside a WSL2 distro.
//!
//! Pure string conversion (no IO), compiled on every platform so it is
//! unit-tested everywhere; only the Windows-only WSL2 backend calls it.
//! Accepted: an absolute drive path (`C:\Users\x`, `c:/Users/x`) →
//! `/mnt/c/Users/x`. Refused: UNC and device paths (`\\server\share`,
//! `\\?\C:\…`), relative paths, drive-relative paths (`C:foo`), and any
//! component that is `..`, contains `:`, a comma or a control character
//! (the result goes into a `host:container:mode` mount string).

/// Convert an absolute Windows drive path to its `/mnt/<drive>/…` form.
pub fn windows_to_wsl_path(path: &str) -> Result<String, String> {
    let refused = |why: &str| Err(format!("cannot mount {path:?} into WSL2: {why}"));
    if path.starts_with("\\\\") || path.starts_with("//") {
        return refused("UNC and device paths are not supported");
    }
    let bytes = path.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || !matches!(bytes[2], b'\\' | b'/') {
        return refused("not an absolute drive path");
    }
    let drive = (bytes[0] as char).to_ascii_lowercase();
    let mut out = format!("/mnt/{drive}");
    for part in path[3..].split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." || part.chars().any(|c| c == ':' || c == ',' || c.is_control()) {
            return refused("a path component is not allowed");
        }
        out.push('/');
        out.push_str(part);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_paths_map_under_mnt() {
        assert_eq!(windows_to_wsl_path(r"C:\Users\x").unwrap(), "/mnt/c/Users/x");
        assert_eq!(windows_to_wsl_path(r"d:\Temp\duduclaw_ptc_ab\").unwrap(), "/mnt/d/Temp/duduclaw_ptc_ab");
        assert_eq!(windows_to_wsl_path("E:/a/./b").unwrap(), "/mnt/e/a/b");
        assert_eq!(windows_to_wsl_path(r"C:\").unwrap(), "/mnt/c");
    }

    #[test]
    fn unc_relative_and_odd_paths_are_refused() {
        for bad in [
            r"\\server\share\x", r"\\?\C:\Users\x", "//server/share", r"Users\x", "C:foo", "", "C:",
            r"C:\a\..\b", r"C:\a:b", r"C:\a,b", "C:\\a\nb", "/mnt/c/x", "1:\\x",
        ] {
            assert!(windows_to_wsl_path(bad).is_err(), "{bad:?}");
        }
    }
}
