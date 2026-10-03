//! Repository-relative paths reported by an LLM.
//!
//! An LLM-supplied `file` must never be handed to `Path::join` raw: an
//! absolute path, a drive letter or a `..` component replaces or escapes the
//! base directory. [`SafeRepoPath::parse`] mirrors Cloudflare
//! security-audit-skill (MIT) `isSafeRelativeSourcePath`, so a value that
//! parses can only name something under the root it is joined to.

use std::fmt;
use std::path::{Path, PathBuf};

use super::visible_text::is_forbidden_identifier_char;

/// Maximum path length in bytes.
pub const MAX_LEN: usize = 4096;

/// A validated repository-relative path with POSIX `/` separators.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafeRepoPath(String);

/// Why a path was rejected. Closed set; the first rule hit is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathViolation {
    Empty,
    TooLong {
        max: usize,
    },
    SurroundingWhitespace,
    /// Starts with `/` or `~`.
    Absolute,
    /// Starts with a drive letter (`C:`) or a UNC prefix (`\\`).
    DriveOrUnc,
    Backslash,
    /// `a//b`, a leading or a trailing `/`.
    EmptyComponent,
    /// A `.` component.
    DotComponent,
    /// A `..` component.
    ParentComponent,
    /// Control, format, line/paragraph separator, default-ignorable code
    /// point, NUL, or `:` anywhere.
    ForbiddenCharacter,
    /// A Windows device name component (`con`, `com1.txt`, `LPT³`…) or a
    /// component ending in `.` or space, which Windows cannot represent.
    WindowsReservedName,
}

impl fmt::Display for PathViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "path is empty"),
            Self::TooLong { max } => write!(f, "path is longer than {max} bytes"),
            Self::SurroundingWhitespace => write!(f, "path has leading or trailing whitespace"),
            Self::Absolute => write!(
                f,
                "path is absolute; a repository-relative path is required"
            ),
            Self::DriveOrUnc => write!(f, "path starts with a drive letter or UNC prefix"),
            Self::Backslash => write!(f, "path contains a backslash; use '/' separators"),
            Self::EmptyComponent => write!(f, "path has an empty component"),
            Self::DotComponent => write!(f, "path has a '.' component"),
            Self::ParentComponent => write!(f, "path has a '..' component"),
            Self::ForbiddenCharacter => {
                write!(
                    f,
                    "path contains a control, format, invisible character or ':'"
                )
            }
            Self::WindowsReservedName => {
                write!(
                    f,
                    "path has a Windows reserved or unrepresentable component name"
                )
            }
        }
    }
}

impl std::error::Error for PathViolation {}

impl SafeRepoPath {
    /// Validate `s`. Rules (first hit wins): empty; longer than [`MAX_LEN`]
    /// bytes; leading/trailing whitespace; starts with `/` or `~`; starts with
    /// `X:` or `\\`; contains `\`; contains a `\p{Cc}\p{Cf}\p{Zl}\p{Zp}` or
    /// Default_Ignorable code point or `:`; any component that is empty,
    /// `.`, `..`, ends with `.` or space, or is a Windows reserved device
    /// name (`con prn aux nul clock$ conin$ conout$ com1-9 lpt1-9`, digits
    /// including superscript `¹²³`, with or without an extension, Unicode
    /// case-insensitive).
    pub fn parse(s: &str) -> Result<Self, PathViolation> {
        if s.is_empty() {
            return Err(PathViolation::Empty);
        }
        if s.len() > MAX_LEN {
            return Err(PathViolation::TooLong { max: MAX_LEN });
        }
        if s.trim() != s {
            return Err(PathViolation::SurroundingWhitespace);
        }
        if s.starts_with('/') || s.starts_with('~') {
            return Err(PathViolation::Absolute);
        }
        if s.starts_with("\\\\") || has_drive_prefix(s) {
            return Err(PathViolation::DriveOrUnc);
        }
        if s.contains('\\') {
            return Err(PathViolation::Backslash);
        }
        if s.chars()
            .any(|c| c == ':' || is_forbidden_identifier_char(c))
        {
            return Err(PathViolation::ForbiddenCharacter);
        }
        for component in s.split('/') {
            match component {
                "" => return Err(PathViolation::EmptyComponent),
                "." => return Err(PathViolation::DotComponent),
                ".." => return Err(PathViolation::ParentComponent),
                _ => {}
            }
            if component.ends_with('.')
                || component.ends_with(' ')
                || is_windows_reserved_component(component)
            {
                return Err(PathViolation::WindowsReservedName);
            }
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `root.join(self)`. Validation guarantees the result stays under `root`
    /// lexically (symlinks inside the tree are the caller's concern).
    pub fn join_under(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

impl fmt::Display for SafeRepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SafeRepoPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

fn has_drive_prefix(s: &str) -> bool {
    let mut chars = s.chars();
    matches!((chars.next(), chars.next()), (Some(d), Some(':')) if d.is_ascii_alphabetic())
}

/// Simple Unicode case folding restricted to what can fold onto the ASCII
/// letters used by the reserved names: ASCII itself, KELVIN SIGN → `k`,
/// LONG S → `s`. Mirrors the reference regex's `/iu` flags.
fn fold_char(c: char) -> char {
    match c {
        '\u{212A}' => 'k',
        '\u{017F}' => 's',
        _ => c.to_ascii_lowercase(),
    }
}

/// `^(?:con|prn|aux|nul|clock\$|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)`
/// with Unicode case-insensitivity, applied to one component.
pub(crate) fn is_windows_reserved_component(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or("");
    let folded: String = stem.chars().map(fold_char).collect();
    match folded.as_str() {
        "con" | "prn" | "aux" | "nul" | "clock$" | "conin$" | "conout$" => true,
        _ => {
            let mut chars = folded.chars();
            let prefix: String = chars.by_ref().take(3).collect();
            let rest: Vec<char> = chars.collect();
            (prefix == "com" || prefix == "lpt")
                && rest.len() == 1
                && matches!(rest[0], '1'..='9' | '\u{B9}' | '\u{B2}' | '\u{B3}')
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> PathViolation {
        SafeRepoPath::parse(s).expect_err(s)
    }

    #[test]
    fn accepts_ordinary_paths() {
        for p in [
            "src/main.rs",
            "src/客戶.rs",
            "a",
            "a/b/c.d.e",
            ".github/workflows/ci.yml",
            "con_tool/x.rs",
            "console.log",
            "com10.txt",
            "lpt0",
            "my file.rs",
            "a/.hidden",
        ] {
            let sp = SafeRepoPath::parse(p).unwrap_or_else(|e| panic!("{p}: {e}"));
            assert_eq!(sp.as_str(), p);
        }
    }

    #[test]
    fn rejects_design_corpus() {
        assert_eq!(err("../x"), PathViolation::ParentComponent);
        assert_eq!(err("src/../file.c"), PathViolation::ParentComponent);
        assert_eq!(err("/etc/passwd"), PathViolation::Absolute);
        assert_eq!(err("~home/file.c"), PathViolation::Absolute);
        assert_eq!(err("C:\\x"), PathViolation::DriveOrUnc);
        assert_eq!(err("C:/file.c"), PathViolation::DriveOrUnc);
        assert_eq!(err("\\\\srv\\x"), PathViolation::DriveOrUnc);
        assert_eq!(err("a\\b"), PathViolation::Backslash);
        assert_eq!(err("a//b"), PathViolation::EmptyComponent);
        assert_eq!(err("a/"), PathViolation::EmptyComponent);
        assert_eq!(err("./a"), PathViolation::DotComponent);
        assert_eq!(err("a/./b"), PathViolation::DotComponent);
        assert_eq!(err("CON"), PathViolation::WindowsReservedName);
        assert_eq!(err("com1.txt"), PathViolation::WindowsReservedName);
        assert_eq!(err("nul.rs"), PathViolation::WindowsReservedName);
        assert_eq!(err("a\u{0}b"), PathViolation::ForbiddenCharacter);
        assert_eq!(
            err("src/file\u{200b}.js"),
            PathViolation::ForbiddenCharacter
        );
        assert_eq!(err(" src/a.rs"), PathViolation::SurroundingWhitespace);
        assert_eq!(err("src/a.rs "), PathViolation::SurroundingWhitespace);
        assert_eq!(err(""), PathViolation::Empty);
    }

    #[test]
    fn rejects_reference_corpus() {
        // From validate-findings.test.cjs path corpus.
        for p in [
            "src/cloc\u{212a}$.txt",
            "src/CLOCK$.txt",
            "src/con.txt",
            "src/COM\u{b9}.log",
            "src/lpt\u{b3}",
            "src/Lpt\u{b2}.x",
            "src/conin$",
            "src/CONOUT$.a.b",
            "aux",
            "PRN.md",
        ] {
            assert_eq!(err(p), PathViolation::WindowsReservedName, "{p}");
        }
        assert_eq!(err("src/file:name.c"), PathViolation::ForbiddenCharacter);
        assert_eq!(err("src/file.c."), PathViolation::WindowsReservedName);
        assert_eq!(err("src /file.c"), PathViolation::WindowsReservedName);
        for p in [
            "src/file\u{202e}name.c",
            "src/file\u{34f}.js",
            "src/file\u{fe0f}.js",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{7f}b",
            "a\nb",
            "a\u{feff}b",
            "a\u{e0041}b",
        ] {
            assert_eq!(err(p), PathViolation::ForbiddenCharacter, "{p:?}");
        }
    }

    #[test]
    fn rejects_too_long() {
        let long = "a/".repeat(MAX_LEN / 2) + "b";
        assert_eq!(err(&long), PathViolation::TooLong { max: MAX_LEN });
        let ok = "a".repeat(MAX_LEN);
        assert!(SafeRepoPath::parse(&ok).is_ok());
    }

    #[test]
    fn join_under_stays_in_root() {
        let root = Path::new("/repo");
        let p = SafeRepoPath::parse("src/客戶.rs").unwrap();
        let joined = p.join_under(root);
        assert!(joined.starts_with(root));
        assert_eq!(joined, PathBuf::from("/repo/src/客戶.rs"));
    }

    #[test]
    fn display_messages() {
        let all = [
            PathViolation::Empty,
            PathViolation::TooLong { max: 4096 },
            PathViolation::SurroundingWhitespace,
            PathViolation::Absolute,
            PathViolation::DriveOrUnc,
            PathViolation::Backslash,
            PathViolation::EmptyComponent,
            PathViolation::DotComponent,
            PathViolation::ParentComponent,
            PathViolation::ForbiddenCharacter,
            PathViolation::WindowsReservedName,
        ];
        for v in &all {
            assert!(v.to_string().starts_with("path "), "{v:?}");
        }
        assert_eq!(all[1].to_string(), "path is longer than 4096 bytes");
        assert_eq!(all[8].to_string(), "path has a '..' component");
    }
}
