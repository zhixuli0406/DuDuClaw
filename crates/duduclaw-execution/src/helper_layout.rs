//! HX5 (D8 revision §2.4, [F9]): where a helper binary finds its signed files
//! and its private scratch. Two layouts, decided from the process the root
//! side set up and never from request bytes:
//! - **in-run** (the trampoline's helper launcher, HX3b): exec'd as
//!   `/helpers/<entry>`, cwd = `HOME` = `TMPDIR` = `/helper-tmp/<call>` (32
//!   lowercase hex, 0700, created by the launcher, unreachable for the runtime);
//!   signed files are under `/helpers`, all scratch under that call directory;
//! - **legacy child run**: exec'd from `/runtime`, cwd `/work`, scratch `/tmp`.
//!
//! Any mix (e.g. `/helpers` entry with cwd `/work`, a `TMPDIR` outside
//! `/helper-tmp`, `HOME` ≠ `TMPDIR`) is refused, so a helper never falls back
//! to runtime-reachable `/tmp` scratch inside a run.
use std::{ffi::OsStr, path::{Component, Path, PathBuf}};

pub const IN_RUN_ROOT: &str = "/helpers";
pub const LEGACY_ROOT: &str = "/runtime";
pub const SCRATCH_ROOT: &str = "/helper-tmp";
pub const WORKSPACE: &str = "/work";
/// HX6 security fix round (finding 5): the recording's only workspace grant
/// (`HelperFs::WorkRecordings`, created by the launcher before its Landlock).
/// The recording opens it directly (`O_NOFOLLOW|O_DIRECTORY`), never `/work`.
pub const RECORDINGS: &str = "/work/recordings";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperLayout {
    /// Directory of the helper's signed files (interpreters, scripts).
    pub root: &'static str,
    /// `HOME` given to interpreters the helper starts.
    pub home: PathBuf,
    /// `TMPDIR` and every helper-owned staging directory live below this.
    pub scratch: PathBuf,
    /// The cwd the helper was started in (checked by `helper_input_args`).
    pub cwd: PathBuf,
}

impl HelperLayout {
    pub fn legacy() -> Self {
        Self { root: LEGACY_ROOT, home: PathBuf::from("/home/employee"), scratch: PathBuf::from("/tmp"),
            cwd: PathBuf::from(WORKSPACE) }
    }
    pub fn in_run(&self) -> bool { self.root == IN_RUN_ROOT }
    /// `<root>/<name>` for a flat signed file name (no separators, no dots).
    pub fn file(&self, name: &str) -> Result<String, ()> {
        if name.is_empty() || name.len() > 255 || name.starts_with('.') || name.contains('/') || name.contains('\0') {
            return Err(());
        }
        Ok(format!("{}/{name}", self.root))
    }
    /// A named staging directory inside the scratch area (e.g. `office`).
    pub fn staging(&self, name: &str) -> PathBuf { self.scratch.join(name) }
}

fn is_call_dir(path: &Path) -> bool {
    let mut components = path.components();
    let (Some(Component::RootDir), Some(Component::Normal(root)), Some(Component::Normal(call)), None) =
        (components.next(), components.next(), components.next(), components.next()) else { return false };
    Path::new("/").join(root) == Path::new(SCRATCH_ROOT)
        && call.to_str().is_some_and(|call| call.len() == 32 && call.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// Pure decision from argv[0], cwd, `HOME` and `TMPDIR`. Fails closed.
pub fn layout(argv0: &OsStr, cwd: &Path, home: Option<&OsStr>, tmpdir: Option<&OsStr>) -> Result<HelperLayout, ()> {
    let entry = Path::new(argv0);
    if entry.parent() == Some(Path::new(IN_RUN_ROOT)) {
        let scratch = PathBuf::from(tmpdir.ok_or(())?);
        if home != Some(scratch.as_os_str()) || !is_call_dir(&scratch) || cwd != scratch { return Err(()); }
        return Ok(HelperLayout { root: IN_RUN_ROOT, home: scratch.clone(), cwd: scratch.clone(), scratch });
    }
    // Legacy argv[0] is whatever the child-run plan used; it only must not
    // claim the in-run root, and the old cwd/scratch rules hold.
    if !entry.starts_with(IN_RUN_ROOT) && cwd == Path::new(WORKSPACE)
        && tmpdir.is_none_or(|dir| !Path::new(dir).starts_with(SCRATCH_ROOT)) {
        return Ok(HelperLayout::legacy());
    }
    Err(())
}

/// The running helper's layout.
pub fn current() -> Result<HelperLayout, ()> {
    let argv0 = std::env::args_os().next().ok_or(())?;
    let cwd = std::env::current_dir().map_err(|_| ())?;
    layout(&argv0, &cwd, std::env::var_os("HOME").as_deref(), std::env::var_os("TMPDIR").as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    const CALL: &str = "/helper-tmp/0123456789abcdef0123456789abcdef";

    /// [F9] In-run helpers use `/helpers` files and only `/helper-tmp/<call>`
    /// scratch; every mixed or runtime-reachable layout is refused.
    #[test]
    fn helper_layout_in_run_uses_helpers_root_and_private_scratch() {
        let s = |v: &str| Some(OsStr::new(v).to_owned());
        let layout_of = |argv0: &str, cwd: &str, home: Option<std::ffi::OsString>, tmp: Option<std::ffi::OsString>|
            layout(OsStr::new(argv0), Path::new(cwd), home.as_deref(), tmp.as_deref());
        let in_run = layout_of("/helpers/duduclaw-program-helper", CALL, s(CALL), s(CALL)).unwrap();
        assert!(in_run.in_run());
        assert_eq!(in_run.file("python3").unwrap(), "/helpers/python3");
        assert_eq!(in_run.staging("office"), Path::new(CALL).join("office"));
        assert_eq!((in_run.home.as_path(), in_run.cwd.as_path()), (Path::new(CALL), Path::new(CALL)));
        for (argv0, cwd, home, tmp) in [
            ("/helpers/h", "/work", s(CALL), s(CALL)),
            ("/helpers/h", CALL, s("/home/employee"), s(CALL)),
            ("/helpers/h", "/tmp", s("/tmp"), s("/tmp")),
            ("/helpers/h", CALL, s(CALL), None),
            ("/helpers/h", "/helper-tmp/../tmp", s("/helper-tmp/../tmp"), s("/helper-tmp/../tmp")),
            ("/helpers/h", "/helper-tmp/ABC", s("/helper-tmp/ABC"), s("/helper-tmp/ABC")),
            ("/helpers/nested/h", CALL, s(CALL), s(CALL)),
            ("/runtime/h", CALL, s(CALL), s(CALL)),
            ("/runtime/h", "/work", None, s(CALL)),
            ("/usr/bin/h", "/tmp", None, None),
        ] {
            assert!(layout_of(argv0, cwd, home, tmp).is_err(), "{argv0} {cwd}");
        }
        let legacy = layout_of("/runtime/duduclaw-program-helper", "/work", None, None).unwrap();
        assert_eq!(legacy, HelperLayout::legacy());
        assert_eq!(legacy.file("python3").unwrap(), "/runtime/python3");
        for name in ["", "../python3", "a/b", ".hidden"] { assert!(legacy.file(name).is_err()); }
    }
}
