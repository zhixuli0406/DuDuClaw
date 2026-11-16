//! `duduclaw-bootstrap` argv (plan R1). Pure and platform-neutral so it is tested
//! everywhere. Arguments are compared as `OsStr` against fixed literals: non-UTF-8
//! input is a usage error, never a panic, and nothing is ever converted lossily.
use std::ffi::OsString;

/// The whole command line after argv[0]. Exactly one fixed word is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `--print-root-pin`: read-only, prints the compiled pin kind and digest.
    PrintRootPin,
    /// `--version`: read-only, prints [`version_line`].
    Version,
    Install,
    Upgrade,
    Rollback,
    /// Anything else, including extra, empty or non-UTF-8 arguments.
    Usage,
}

impl Command {
    /// Install/upgrade/rollback change root state; the caller hardens before any
    /// other work once one of them is selected.
    pub fn changes_state(self) -> bool { matches!(self, Self::Install | Self::Upgrade | Self::Rollback) }
}

/// Parse `std::env::args_os().skip(1)`. At most two items are pulled from the
/// iterator, so an oversized argv is not collected.
pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Command {
    let mut args = args.into_iter();
    let (Some(word), None) = (args.next(), args.next()) else { return Command::Usage };
    if word == *"--print-root-pin" {
        Command::PrintRootPin
    } else if word == *"--version" {
        Command::Version
    } else if word == *"install" {
        Command::Install
    } else if word == *"upgrade" {
        Command::Upgrade
    } else if word == *"rollback" {
        Command::Rollback
    } else {
        Command::Usage
    }
}

/// `--version` output: the crate (= workspace release) version. The packaging
/// script requires it to equal the version being packaged.
pub fn version_line() -> String { format!("duduclaw-bootstrap {}", env!("CARGO_PKG_VERSION")) }

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> { list.iter().map(OsString::from).collect() }

    #[test]
    fn argv_is_fixed_words_only() {
        assert_eq!(parse(args(&["--print-root-pin"])), Command::PrintRootPin);
        assert_eq!(parse(args(&["--version"])), Command::Version);
        assert_eq!(parse(args(&["install"])), Command::Install);
        assert_eq!(parse(args(&["upgrade"])), Command::Upgrade);
        assert_eq!(parse(args(&["rollback"])), Command::Rollback);
        let usage: Vec<Vec<&str>> = vec![vec![], vec![""], vec!["INSTALL"], vec!["install "], vec![" install"],
            vec!["install\0"], vec!["--install"], vec!["install", "upgrade"], vec!["install", ""],
            vec!["--print-root-pin", "x"], vec!["--version", "--version"], vec!["-V"], vec!["--print-root-pin=1"],
            vec!["ｉnstall"], vec!["help"]];
        for bad in &usage {
            assert_eq!(parse(args(bad)), Command::Usage, "{bad:?}");
        }
        assert!(Command::Install.changes_state() && Command::Upgrade.changes_state() && Command::Rollback.changes_state());
        for read_only in [Command::PrintRootPin, Command::Version, Command::Usage] { assert!(!read_only.changes_state()); }
        assert_eq!(version_line(), format!("duduclaw-bootstrap {}", env!("CARGO_PKG_VERSION")));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_argv_is_a_usage_error_not_a_panic() {
        use std::os::unix::ffi::OsStringExt;
        for bytes in [&b"\xff"[..], b"install\xff", b"\xc3\x28", b"--version\x80"] {
            assert_eq!(parse(vec![OsString::from_vec(bytes.to_vec())]), Command::Usage);
            assert_eq!(parse(vec![OsString::from("install"), OsString::from_vec(bytes.to_vec())]), Command::Usage);
            assert_eq!(parse(vec![OsString::from_vec(bytes.to_vec()), OsString::from("install")]), Command::Usage);
        }
        // Only two items are ever pulled, so an unbounded iterator terminates.
        assert_eq!(parse(std::iter::repeat(OsString::from("install"))), Command::Usage);
    }
}
