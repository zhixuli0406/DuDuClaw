//! G1 (2026-10): a small, lexical reading of a shell command for the Bash
//! lane's `<home>` rule. Still a speed bump, not a parser of shell.
//!
//! | Position | Rule |
//! |---|---|
//! | backslash-newline, `\x`, quotes | undone first, as bash does (round 3) |
//! | fd redirect (`2>/dev/null`, `2>&1`, `>&N`, `&>/dev/null`, `>/dev/null`) | removed before judging, only when the digits / `-` end the word (`>&1/x` writes the file `1/x`) |
//! | output redirect target (`>`, `>>`, `>\|`, `&>`, `N>`) | refused when the target is a `<home>` target |
//! | copy destination ([`COPY_COMMANDS`], `time -o`) | refused when the destination is a `<home>` target; sources are not judged |
//! | any argument of [`MUTATING_COMMANDS`], `sed -i`, `find -delete/-exec…`, `xargs`/`parallel` running a writer | refused when any argument is a `<home>` target |
//! | any argument of [`INTERPRETERS`] (including `-c '…'` program text) | refused when any argument is a `<home>` target |
//! | [`READ_ONLY_COMMANDS`] | arguments not judged (redirects still are) |
//! | any other command (unrecognised, or behind a prefix option the scan cannot follow) | refused when any argument is a `<home>` target (round 4: no write fragment needed) |
//! | a `<home>` database file (`*.db`, `*.db-wal`, `*.db-shm`, `*.sqlite*`, `*-journal`) anywhere | refused, read or write |
//!
//! "`<home>` target": `<home>` state, another agent's directory, the
//! removed-employee area; for an untrusted caller also its own directory.
//! Paths are judged on their literal and their symlink-resolved location.
//! Relative paths resolve against the envelope `cwd` (fallback
//! `<home>/agents/<caller>`); `cd`/`pushd` move it, `( … )` restores it, and a
//! `cd` the scan cannot follow makes later relative paths unknown — refused
//! when the command names `<home>` anywhere, else not judged.

use std::path::{Path, PathBuf};

use crate::agent_guard::lexical_normalize;

use super::bash_cmd::{
    command_and_args, copy_destinations, is_mutating, positional, runs_writer, COPY_COMMANDS,
    READ_ONLY_COMMANDS,
};
use super::matcher::{classify_home_rest, components_after_ci, HomeTarget};
use super::rules::HOME_EVIDENCE_BASENAMES;

/// Database file suffixes refused wherever they appear under `<home>`.
const DB_SUFFIXES: &[&str] = &[
    ".db", ".db-wal", ".db-shm", ".db-journal", ".sqlite", ".sqlite3", ".sqlite-wal",
    ".sqlite-shm", ".sqlite-journal",
];

/// Remove fd duplications and redirects to `/dev/null`, which write nothing.
pub(super) fn strip_fd_redirects(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(b.len());
    let boundary = |i: usize| i == 0 || b[i - 1].is_whitespace() || matches!(b[i - 1], ';' | '|' | '(' | '&');
    let mut i = 0;
    while i < b.len() {
        if b[i] != '>' && b[i] != '<' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        // Prefix already copied: `N` digits or `&` that start a word.
        let mut p = i;
        while p > 0 && b[p - 1].is_ascii_digit() {
            p -= 1;
        }
        if p < i && !boundary(p) {
            p = i;
        }
        if p == i && i > 0 && b[i - 1] == '&' && b[i] == '>' && boundary(i - 1) {
            p = i - 1;
        }
        let mut k = i + 1;
        if k < b.len() && b[k] == '>' && b[i] == '>' {
            k += 1;
        }
        let mut end = None;
        if k < b.len() && b[k] == '&' {
            let mut m = k + 1;
            while m < b.len() && (b[m].is_ascii_digit() || b[m] == '-') {
                m += 1;
            }
            // Round 4 (M1): only a whole word of digits or `-` is a
            // duplication. `>&1/x` and `>&-x` redirect into the file `1/x` /
            // `-x`, so they must stay and be judged as writes.
            let at_word_end = b.get(m).is_none_or(|c| {
                c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')' | '(' | '<' | '>' | '`')
            });
            if m > k + 1 && at_word_end {
                end = Some(m);
            }
        } else {
            let mut m = k;
            while m < b.len() && b[m] == ' ' {
                m += 1;
            }
            let rest: String = b[m..].iter().take(9).collect();
            let after = b.get(m + 9);
            if rest == "/dev/null"
                && after.is_none_or(|c| c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')'))
            {
                end = Some(m + 9);
            }
        }
        match end {
            Some(e) => {
                out.truncate(out.len() - (i - p));
                out.push(' ');
                i = e;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    out.into_iter().collect()
}

/// Undo shell quoting escapes the way bash reads them, for the text rules:
/// a backslash-newline disappears (line continuation); outside quotes a
/// backslash keeps the next character literally; inside double quotes only
/// `$`, `` ` ``, `"`, `\` and newline are escapable; inside single quotes
/// nothing is. Quote characters themselves are kept.
pub(super) fn shell_unescape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match (quote, c) {
            (Some('\''), '\'') => {
                quote = None;
                out.push(c);
            }
            (Some('\''), _) => out.push(c),
            (_, '\\') if i + 1 < chars.len() => {
                let n = chars[i + 1];
                let escapable = quote.is_none() || matches!(n, '$' | '`' | '"' | '\\' | '\n');
                if escapable {
                    if n != '\n' {
                        out.push(n);
                    }
                    i += 2;
                    continue;
                }
                out.push(c);
            }
            (Some('"'), '"') => {
                quote = None;
                out.push(c);
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// Target of an output redirect.
    RedirOut(String),
    /// `;`, `&&`, `||`, `|`, `&`, newline, backquote.
    Sep,
    /// `(` — a subshell or `$(`; the working directory is restored at `)`.
    Open,
    Close,
}

/// Split a command into words, redirect targets and separators, undoing
/// quotes and backslash escapes as bash does.
fn tokenize(s: &str) -> Vec<Tok> {
    struct St {
        toks: Vec<Tok>,
        cur: String,
        in_word: bool,
        pending_out: bool,
        pending_in: bool,
    }
    impl St {
        fn flush(&mut self) {
            if self.in_word {
                let w = std::mem::take(&mut self.cur);
                if self.pending_out {
                    self.toks.push(Tok::RedirOut(w));
                    self.pending_out = false;
                } else if self.pending_in {
                    self.pending_in = false;
                } else {
                    self.toks.push(Tok::Word(w));
                }
                self.in_word = false;
            }
        }
        fn sep(&mut self, t: Tok) {
            self.flush();
            self.pending_out = false;
            self.pending_in = false;
            self.toks.push(t);
        }
    }
    let mut st = St { toks: Vec::new(), cur: String::new(), in_word: false, pending_out: false, pending_in: false };
    let mut quote: Option<char> = None;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if q == '"' && c == '\\' && matches!(chars.get(i + 1), Some('$' | '`' | '"' | '\\' | '\n')) {
                if chars[i + 1] != '\n' {
                    st.cur.push(chars[i + 1]);
                }
                i += 1;
            } else {
                st.cur.push(c);
            }
            i += 1;
            continue;
        }
        match c {
            '\\' => {
                // Outside quotes: the next character is literal; a newline
                // is a line continuation and disappears.
                if let Some(&n) = chars.get(i + 1) {
                    if n != '\n' {
                        st.cur.push(n);
                        st.in_word = true;
                    }
                    i += 1;
                }
            }
            '\'' | '"' => {
                quote = Some(c);
                st.in_word = true;
            }
            c if c.is_whitespace() && c != '\n' => st.flush(),
            '>' => {
                // A word made only of digits right before `>` is an fd number.
                if st.in_word && !st.cur.is_empty() && st.cur.chars().all(|d| d.is_ascii_digit()) {
                    st.cur.clear();
                    st.in_word = false;
                } else {
                    st.flush();
                }
                if matches!(chars.get(i + 1), Some('>') | Some('|')) {
                    i += 1;
                }
                // `>&word` (left in place by `strip_fd_redirects` only when it
                // is not a whole-word duplication) writes into `word`.
                if chars.get(i + 1) == Some(&'&') {
                    i += 1;
                }
                st.pending_out = true;
            }
            '<' => {
                st.flush();
                if chars.get(i + 1) == Some(&'<') {
                    i += 1;
                }
                st.pending_in = true;
            }
            '&' if chars.get(i + 1) == Some(&'>') => {
                st.flush();
                i += 1;
                if chars.get(i + 1) == Some(&'>') {
                    i += 1;
                }
                st.pending_out = true;
            }
            '(' => st.sep(Tok::Open),
            ')' => st.sep(Tok::Close),
            ';' | '|' | '&' | '\n' | '`' => st.sep(Tok::Sep),
            _ => {
                st.cur.push(c);
                st.in_word = true;
            }
        }
        i += 1;
    }
    st.flush();
    st.toks
}

/// Context for resolving one command's paths.
pub(super) struct Ctx<'a> {
    pub home: &'a Path,
    /// `<home>` with its symbolic links resolved (R2-5).
    pub home_real: Option<PathBuf>,
    /// Lowercased caller id.
    pub caller: String,
    pub untrusted: bool,
    /// Set when the command names `<home>` somewhere: a relative path whose
    /// working directory is unknown then counts as `<home>` state (R2-3).
    pub strict_relative: bool,
}

fn eq_prefix_ci<'w>(w: &'w str, prefix: &str) -> Option<&'w str> {
    let head = w.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &w[prefix.len()..])
}

fn is_relative_word(w: &str) -> bool {
    !(w.starts_with('/') || w.starts_with('~') || w.starts_with('$') || w.is_empty())
}

impl Ctx<'_> {
    fn home_is_dot_duduclaw(&self) -> bool {
        self.home
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(".duduclaw"))
    }

    /// Absolute path a word names, joined but **not** normalised (a `..`
    /// after a link must stay for the real-path resolution), or `None` when
    /// the scan cannot tell.
    fn resolve_raw(&self, word: &str, cwd: Option<&Path>) -> Option<PathBuf> {
        let mut w = word.trim().to_string();
        // A quoted Windows path keeps its backslashes.
        let b = w.as_bytes();
        if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            w = w.replace('\\', "/");
        }
        let w = w.as_str();
        if w.is_empty() {
            return None;
        }
        let home_rel = |prefix: &str| -> Option<String> {
            let r = eq_prefix_ci(w, prefix)?;
            (r.is_empty() || r.starts_with('/')).then(|| r.trim_start_matches('/').to_string())
        };
        for prefix in ["$DUDUCLAW_HOME", "${DUDUCLAW_HOME}"] {
            if let Some(r) = home_rel(prefix) {
                return Some(self.home.join(r));
            }
        }
        if self.home_is_dot_duduclaw() {
            for prefix in ["~/.duduclaw", "$HOME/.duduclaw", "${HOME}/.duduclaw"] {
                if let Some(r) = home_rel(prefix) {
                    return Some(self.home.join(r));
                }
            }
        }
        if w.starts_with('/') || (w.len() > 2 && w.as_bytes()[1] == b':' && w.as_bytes()[2] == b'/') {
            return Some(PathBuf::from(w));
        }
        if !is_relative_word(w) {
            return None;
        }
        Some(cwd?.join(w))
    }

    fn target_of(&self, abs: &Path, home: &Path, evidence: Option<&str>) -> Option<HomeTarget> {
        let rest = components_after_ci(abs, home)?;
        let rest = rest.into_iter().map(|c| c.to_ascii_lowercase()).collect();
        match classify_home_rest(rest, &self.caller) {
            // An audit-log name inside the caller's own directory most likely
            // means the cwd assumption is off.
            Some(HomeTarget::OwnAgentDir) if evidence.is_some() => {
                Some(HomeTarget::State(vec![evidence.unwrap_or_default().to_string()]))
            }
            other => other,
        }
    }

    fn classify(&self, word: &str, cwd: Option<&Path>) -> Option<HomeTarget> {
        let base = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
        let evidence = HOME_EVIDENCE_BASENAMES
            .iter()
            .any(|n| base == *n || base.strip_prefix(n).is_some_and(|r| r.starts_with('.')))
            .then_some(base.as_str());
        let Some(raw) = self.resolve_raw(word, cwd) else {
            if self.strict_relative && is_relative_word(word.trim()) {
                return Some(HomeTarget::State(vec![word.trim().to_string()]));
            }
            return evidence.map(|e| HomeTarget::State(vec![e.to_string()]));
        };
        let lexical = self.target_of(&lexical_normalize(&raw), self.home, evidence);
        if lexical.as_ref().is_some_and(|t| self.refused(t)) {
            return lexical;
        }
        // R2-5: an existing link is followed the way the kernel would.
        match crate::org_field_guard::resolve_real_path(&raw) {
            Ok(real) => {
                if let Some(home_real) = &self.home_real {
                    let t = self.target_of(&real, home_real, evidence);
                    if t.as_ref().is_some_and(|t| self.refused(t)) {
                        return t;
                    }
                }
            }
            Err(_) => {
                // Name the path the write was aimed at (home-relative when it
                // is under `<home>`), not just its last component.
                let rest = components_after_ci(&lexical_normalize(&raw), self.home)
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| vec![base.clone()]);
                return Some(HomeTarget::State(rest));
            }
        }
        lexical
    }

    /// Whether a target is refused in a write position for this caller.
    pub(super) fn refused(&self, t: &HomeTarget) -> bool {
        match t {
            HomeTarget::OwnAgentDir => self.untrusted,
            _ => true,
        }
    }
}

/// Split a word into path-shaped pieces, for arguments that may be program
/// text (`open('/x/y','w')`) or `key=value` (`of=/x`).
///
/// Round 5: `${DUDUCLAW_HOME}` and `${HOME}` are rewritten to their unbraced
/// spellings first, so the `{`/`}` split below cannot cut them in half.
fn pieces(word: &str) -> Vec<String> {
    let word = unbrace_home_vars(word);
    word.split(|c: char| {
        c.is_whitespace()
            || matches!(c, '\'' | '"' | '(' | ')' | ',' | ';' | '=' | '<' | '>' | '`' | '{' | '}' | '[' | ']')
    })
    .filter(|p| !p.is_empty())
    .map(str::to_string)
    .collect()
}

/// `${DUDUCLAW_HOME}` → `$DUDUCLAW_HOME`, `${HOME}` → `$HOME`
/// (ASCII case-insensitive).
fn unbrace_home_vars(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    'outer: while !rest.is_empty() {
        for (braced, plain) in [("${DUDUCLAW_HOME}", "$DUDUCLAW_HOME"), ("${HOME}", "$HOME")] {
            if let Some(after) = eq_prefix_ci(rest, braced) {
                out.push_str(plain);
                rest = after;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn is_db_name(word: &str) -> bool {
    let base = super::bash_cmd::base_name(word);
    DB_SUFFIXES.iter().any(|s| base.ends_with(s)) || base.ends_with("-journal")
}

/// Every `<home>` target a command refuses for `ctx`, in order.
///
pub(super) fn refused_targets(command: &str, ctx: &mut Ctx<'_>, start_cwd: Option<PathBuf>) -> Vec<HomeTarget> {
    let toks = tokenize(command);

    // R2-3 pre-pass: does any word name a `<home>` target outright?
    ctx.strict_relative = false;
    ctx.strict_relative = toks.iter().any(|t| match t {
        Tok::Word(w) | Tok::RedirOut(w) => pieces(w).iter().any(|p| {
            !is_relative_word(p) && ctx.classify(p, None).is_some_and(|t| ctx.refused(&t))
        }) || pieces(w).iter().any(|p| {
            p.contains("..") && ctx.classify(p, start_cwd.as_deref()).is_some_and(|t| ctx.refused(&t))
        }),
        _ => false,
    });

    let mut segments: Vec<(Vec<String>, Vec<String>, Option<Tok>)> = vec![(Vec::new(), Vec::new(), None)];
    for t in &toks {
        match t {
            Tok::Word(w) => segments.last_mut().unwrap().0.push(w.clone()),
            Tok::RedirOut(w) => segments.last_mut().unwrap().1.push(w.clone()),
            Tok::Sep | Tok::Open | Tok::Close => segments.push((Vec::new(), Vec::new(), Some(t.clone()))),
        }
    }

    let all_words: Vec<&str> = segments.iter().flat_map(|(w, _, _)| w.iter().map(String::as_str)).collect();
    let mut cwd = start_cwd;
    let mut stack: Vec<Option<PathBuf>> = Vec::new();
    let mut out = Vec::new();
    let judge = |out: &mut Vec<HomeTarget>, ctx: &Ctx<'_>, word: &str, cwd: Option<&Path>| {
        if let Some(t) = ctx.classify(word, cwd) {
            if ctx.refused(&t) {
                out.push(t);
            }
        }
    };

    for (words, redirs, opened_by) in &segments {
        match opened_by {
            Some(Tok::Open) => stack.push(cwd.clone()),
            Some(Tok::Close) => {
                if let Some(saved) = stack.pop() {
                    cwd = saved;
                }
            }
            _ => {}
        }
        // Databases: anywhere, any command, reads included.
        for w in words.iter().chain(redirs) {
            for p in &pieces(w) {
                if is_db_name(p) {
                    if let Some(t @ HomeTarget::State(_)) = ctx.classify(p, cwd.as_deref()) {
                        out.push(t);
                    }
                }
            }
        }
        for r in redirs {
            judge(&mut out, ctx, r, cwd.as_deref());
        }
        let Some(parsed) = command_and_args(words) else {
            continue;
        };
        for d in &parsed.dests {
            judge(&mut out, ctx, d, cwd.as_deref());
        }
        let (cmd, args) = (parsed.cmd.as_str(), parsed.args);
        if parsed.strict {
            // A prefix this scan cannot follow: every word is suspect.
            for w in words {
                for p in &pieces(w) {
                    judge(&mut out, ctx, p, cwd.as_deref());
                }
            }
            continue;
        }
        match cmd {
            "cd" | "pushd" => {
                cwd = match positional(args).first() {
                    Some(&"-") | None => None,
                    Some(d) => ctx.resolve_raw(d, cwd.as_deref()),
                };
                continue;
            }
            "popd" => {
                cwd = None;
                continue;
            }
            _ => {}
        }
        if COPY_COMMANDS.contains(&cmd) {
            for d in copy_destinations(cmd, args) {
                judge(&mut out, ctx, d, cwd.as_deref());
            }
            continue;
        }
        if matches!(cmd, "xargs" | "parallel") {
            if runs_writer(cmd, args) {
                // The paths usually arrive on stdin from another segment.
                for w in &all_words {
                    for p in &pieces(w) {
                        judge(&mut out, ctx, p, cwd.as_deref());
                    }
                }
            }
            continue;
        }
        let known_read_only = READ_ONLY_COMMANDS.contains(&cmd) && !is_mutating(cmd, args);
        if known_read_only {
            continue;
        }
        // Not known to be read-only and not a copy: mutating commands,
        // interpreters and every unrecognised command are refused on any
        // `<home>` target argument, write fragment or not (round 4, S1).
        {
            for a in args {
                for p in &pieces(a) {
                    if p.starts_with('-') && !p.contains('/') {
                        continue;
                    }
                    judge(&mut out, ctx, p, cwd.as_deref());
                }
            }
        }
    }
    out
}
