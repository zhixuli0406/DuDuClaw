//! G1 round 3: what a Bash command segment runs — prefix commands and their
//! option syntax, the read-only / copy / mutating / interpreter tables, and
//! where a copy-shaped command writes. Split out of `bash_parse.rs`.

/// Copy-shaped commands: only the destination is judged.
pub(super) const COPY_COMMANDS: &[&str] = &[
    "cp", "install", "ditto", "scp", "gcp", "dd", "curl", "wget", "tar", "bsdtar", "gtar", "unzip",
];

/// Commands that change whatever any of their arguments names.
pub(super) const MUTATING_COMMANDS: &[&str] = &[
    "mv", "rm", "rmdir", "unlink", "shred", "srm", "ln", "link", "chmod", "chown", "chgrp",
    "chflags", "chattr", "xattr", "setfacl", "touch", "truncate", "rsync", "tee", "mkdir",
    "mkfifo", "mknod", "sqlite3", "duckdb", "sqlite-utils", "patch", "split", "csplit",
];

/// Interpreters and shells: any `<home>` target among their arguments
/// (program text included) is refused.
pub(super) const INTERPRETERS: &[&str] = &[
    "python", "ruby", "node", "nodejs", "deno", "bun", "perl", "php", "lua", "rscript",
    "osascript", "awk", "gawk", "mawk", "nawk", "bash", "sh", "zsh", "dash", "ksh", "fish",
    "pwsh", "powershell", "swift", "eval", "source", ".",
];

/// Commands known to only read their arguments. A `<home>` path among their
/// arguments is fine; their redirects are still judged. Everything not in
/// any list is refused when any argument is a `<home>` target (round 4).
///
/// Round 4 (S2) removed every entry with an option that writes a file or runs
/// another program, or that is interactive: `ll`/`la` (aliases, unknown
/// meaning), `bat` (`--pager`), `less`/`more` (log file, editor, shell
/// escape), `rg` (`--pre`), `ag`/`ack` (`--pager`), `file` (`-C` compiles a
/// magic file), `tree` (`-o`), `md5` (platform-dependent options), `sort`
/// (`-o`, `--compress-program`). `find`/`fd` stay read-only only without an
/// action option (`is_mutating`).
pub(super) const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "egrep", "fgrep", "wc", "stat", "du", "df", "diff",
    "cmp", "comm", "cut", "tr", "jq", "echo", "printf", "pwd", "realpath", "readlink",
    "basename", "dirname", "md5sum", "sha1sum", "sha256sum", "sha512sum", "shasum", "cksum",
    "strings", "od", "hexdump", "column", "nl", "fold", "rev", "tac", "paste", "join", "seq",
    "date", "whoami", "id", "uname", "which", "type", "test", "[", "true", "false", "sleep",
    "zcat", "gzcat", "bzcat", "xzcat", "look", "lsof", "ps", "export", "unset", "set", "local",
    "declare", "find", "fd",
];

/// Prefix words that run the next word as the command.
const PREFIX_COMMANDS: &[&str] = &[
    "sudo", "doas", "env", "command", "builtin", "exec", "nohup", "time", "nice", "ionice",
    "stdbuf", "timeout", "caffeinate", "!", "{", "then", "do", "else", "elif", "if", "while",
    "until",
];

/// One prefix command's option syntax: options taking a value, options that
/// don't, options whose value is a written file, options this scan cannot
/// follow, and how many positional words precede the command (`timeout 5`).
struct PrefixSpec {
    value: &'static [&'static str],
    plain: &'static [&'static str],
    dest: &'static [&'static str],
    strict: &'static [&'static str],
    positionals: usize,
}

fn prefix_spec(name: &str) -> PrefixSpec {
    let none: &'static [&'static str] = &[];
    match name {
        "sudo" => PrefixSpec {
            value: &["-u", "-g", "-C", "-h", "-p", "-r", "-t", "-T", "-U", "--user", "--group",
                "--close-from", "--host", "--prompt", "--role", "--type", "--command-timeout",
                "--other-user"],
            plain: &["-A", "-b", "-E", "-H", "-i", "-K", "-k", "-n", "-P", "-S", "-s", "-B",
                "--askpass", "--background", "--preserve-env", "--set-home", "--login",
                "--remove-timestamp", "--reset-timestamp", "--non-interactive",
                "--preserve-groups", "--stdin", "--shell", "--bell"],
            dest: none,
            strict: &["-D", "-e", "-l", "--chdir", "--edit", "--list"],
            positionals: 0,
        },
        "doas" => PrefixSpec { value: &["-u", "-C"], plain: &["-n", "-s", "-L"], dest: none, strict: none, positionals: 0 },
        "env" => PrefixSpec {
            value: &["-u", "-P", "--unset"],
            plain: &["-i", "-0", "-v", "-", "--ignore-environment", "--null", "--debug"],
            dest: none,
            strict: &["-C", "-S", "--chdir", "--split-string"],
            positionals: 0,
        },
        "nice" => PrefixSpec { value: &["-n", "--adjustment"], plain: none, dest: none, strict: none, positionals: 0 },
        "ionice" => PrefixSpec { value: &["-c", "-n", "-p", "-P", "-u"], plain: &["-t"], dest: none, strict: none, positionals: 0 },
        "timeout" => PrefixSpec {
            value: &["-s", "-k", "--signal", "--kill-after"],
            plain: &["-v", "--preserve-status", "--foreground", "--verbose"],
            dest: none,
            strict: none,
            positionals: 1,
        },
        "stdbuf" => PrefixSpec { value: &["-i", "-o", "-e", "--input", "--output", "--error"], plain: none, dest: none, strict: none, positionals: 0 },
        "time" => PrefixSpec {
            value: &["-f", "--format"],
            plain: &["-a", "-p", "-v", "-q", "--append", "--portability", "--verbose", "--quiet"],
            dest: &["-o", "--output"],
            strict: none,
            positionals: 0,
        },
        "command" => PrefixSpec { value: none, plain: &["-p"], dest: none, strict: none, positionals: 0 },
        "exec" => PrefixSpec { value: &["-a"], plain: &["-c", "-l"], dest: none, strict: none, positionals: 0 },
        "caffeinate" => PrefixSpec { value: &["-t", "-w"], plain: &["-d", "-i", "-m", "-s", "-u"], dest: none, strict: none, positionals: 0 },
        _ => PrefixSpec { value: none, plain: none, dest: none, strict: none, positionals: 0 },
    }
}

/// The command a segment runs, after prefixes and assignments.
pub(super) struct Parsed<'a> {
    pub cmd: String,
    pub args: &'a [String],
    /// A prefix option this scan cannot follow: judge the segment strictly.
    pub strict: bool,
    /// Files a prefix writes (`time -o`).
    pub dests: Vec<&'a str>,
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((k, _)) => {
            !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !k.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

pub(super) fn base_name(word: &str) -> String {
    word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase()
}

/// Consume one prefix's options from `words[i..]`; returns the new index.
fn skip_prefix_options<'a>(name: &str, words: &'a [String], mut i: usize, p: &mut Parsed<'a>) -> usize {
    let spec = prefix_spec(name);
    while i < words.len() {
        let f = words[i].as_str();
        if f == "--" {
            i += 1;
            break;
        }
        let numeric_nice = name == "nice" && f.len() > 1 && f[1..].trim_start_matches('+').parse::<i64>().is_ok();
        if !f.starts_with('-') || f == "-" && !spec.plain.contains(&"-") {
            break;
        }
        if numeric_nice || spec.plain.contains(&f) {
            i += 1;
        } else if let Some((long, v)) = f.split_once('=').filter(|_| f.starts_with("--")) {
            if spec.dest.contains(&long) {
                p.dests.push(v);
            } else if spec.strict.contains(&long) || !(spec.value.contains(&long) || spec.plain.contains(&long)) {
                p.strict = true;
            }
            i += 1;
        } else if spec.strict.contains(&f) {
            p.strict = true;
            i += 2;
        } else if spec.value.contains(&f) || spec.dest.contains(&f) {
            if spec.dest.contains(&f) {
                if let Some(v) = words.get(i + 1) {
                    p.dests.push(v.as_str());
                }
            }
            i += 2;
        } else if f.len() > 2 && !f.starts_with("--") && (spec.value.contains(&&f[..2]) || spec.dest.contains(&&f[..2])) {
            if spec.dest.contains(&&f[..2]) {
                p.dests.push(&f[2..]);
            }
            i += 1;
        } else if f.len() > 2
            && !f.starts_with("--")
            && f[1..].chars().all(|c| spec.plain.iter().any(|pf| pf.len() == 2 && pf.ends_with(c)))
        {
            i += 1;
        } else {
            // An option this table does not know: from here on the scan
            // cannot tell which word is the command.
            p.strict = true;
            i += 1;
        }
    }
    let mut n = spec.positionals;
    while n > 0 && i < words.len() {
        i += 1;
        n -= 1;
    }
    i
}

pub(super) fn command_and_args(words: &[String]) -> Option<Parsed<'_>> {
    let mut p = Parsed { cmd: String::new(), args: &[], strict: false, dests: Vec::new() };
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if is_assignment(w) {
            i += 1;
            continue;
        }
        let name = base_name(w);
        if PREFIX_COMMANDS.contains(&name.as_str()) {
            if name == "command" && matches!(words.get(i + 1).map(String::as_str), Some("-v" | "-V")) {
                p.cmd = "type".into();
                p.args = &words[i + 1..];
                return Some(p);
            }
            i = skip_prefix_options(&name, words, i + 1, &mut p);
            continue;
        }
        p.cmd = name;
        p.args = &words[i + 1..];
        return Some(p);
    }
    // Only assignments / prefixes: nothing runs, unless a prefix confused us.
    p.strict.then_some(p)
}

/// Values of `flags` (`-o x`, `-ox`, `--output x`, `--output=x`).
fn flag_values<'a>(args: &'a [String], flags: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        for f in flags {
            if a == f {
                if let Some(v) = it.peek() {
                    out.push(v.as_str());
                }
            } else if f.starts_with("--") {
                if let Some(v) = a.strip_prefix(&format!("{f}=")) {
                    out.push(v);
                }
            } else if f.len() == 2 && a.starts_with(f) && a.len() > 2 && !a.starts_with("--") {
                out.push(&a[2..]);
            }
        }
    }
    out
}

pub(super) fn positional(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect()
}

/// Destination words of a copy-shaped command.
pub(super) fn copy_destinations<'a>(cmd: &str, args: &'a [String]) -> Vec<&'a str> {
    let mut out = flag_values(args, &["-t", "--target-directory"]);
    match cmd {
        "dd" => out.extend(args.iter().filter_map(|a| a.strip_prefix("of="))),
        "curl" => out.extend(flag_values(args, &["-o", "--output", "--output-dir"])),
        "wget" => out.extend(flag_values(args, &["-O", "-o", "-P", "--output-document", "--directory-prefix", "--output-file"])),
        "tar" | "bsdtar" | "gtar" => {
            out.extend(flag_values(args, &["-C", "--directory", "--file"]));
            // A combined short-flag word carrying `f` names the archive next.
            let mut it = args.iter().peekable();
            while let Some(a) = it.next() {
                if a.starts_with('-') && !a.starts_with("--") && a.contains('f') {
                    if let Some(v) = it.peek() {
                        out.push(v.as_str());
                    }
                }
            }
        }
        "unzip" => out.extend(flag_values(args, &["-d"])),
        _ => {
            if out.is_empty() {
                if let Some(last) = positional(args).last() {
                    out.push(last);
                }
            }
        }
    }
    out
}

pub(super) fn is_interpreter(name: &str) -> bool {
    INTERPRETERS.iter().any(|i| {
        name == *i
            || (matches!(*i, "python" | "ruby" | "perl" | "php" | "node" | "lua")
                && name.strip_prefix(i).is_some_and(|r| r.chars().all(|c| c.is_ascii_digit() || c == '.')))
    })
}

pub(super) fn is_mutating(cmd: &str, args: &[String]) -> bool {
    if MUTATING_COMMANDS.contains(&cmd) {
        return true;
    }
    match cmd {
        "sed" | "gsed" => args.iter().any(|a| {
            a == "--in-place" || a.starts_with("--in-place=") || (a.starts_with('-') && !a.starts_with("--") && a.contains('i'))
        }),
        "find" | "fd" => args.iter().any(|a| {
            // Round 5: `--exec=cmd` / `--exec-batch=cmd` are the same action
            // as the separated spelling.
            let a = a.split_once('=').map_or(a.as_str(), |(k, _)| k);
            matches!(a, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir" | "-fprint" | "-fprint0" | "-fprintf" | "-fls" | "-x" | "-X" | "--exec" | "--exec-batch")
        }),
        _ => false,
    }
}

/// `xargs` / `parallel`: does the command they run write? Unknown options
/// count as yes.
pub(super) fn runs_writer(cmd: &str, args: &[String]) -> bool {
    if cmd == "parallel" {
        return true;
    }
    const VALUE: &[&str] = &["-I", "-n", "-P", "-L", "-l", "-s", "-d", "-E", "-e", "-a"];
    const PLAIN: &[&str] = &["-0", "-r", "-t", "-p", "-x", "-o", "--null", "--no-run-if-empty", "--verbose", "--interactive", "--open-tty"];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if !a.starts_with('-') {
            break;
        }
        if PLAIN.contains(&a) || a.starts_with("--") && a.contains('=') {
            i += 1;
        } else if VALUE.contains(&a) {
            i += 2;
        } else if a.len() > 2 && VALUE.contains(&&a[..2]) {
            i += 1;
        } else {
            return true;
        }
    }
    let rest: Vec<String> = args[i.min(args.len())..].to_vec();
    match command_and_args(&rest) {
        Some(p) => {
            p.strict
                || is_mutating(&p.cmd, p.args)
                || is_interpreter(&p.cmd)
                || COPY_COMMANDS.contains(&p.cmd.as_str())
                || !READ_ONLY_COMMANDS.contains(&p.cmd.as_str())
        }
        None => false,
    }
}
