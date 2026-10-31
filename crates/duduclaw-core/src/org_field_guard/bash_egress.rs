//! Lexical network-command telemetry, not a shell containment boundary.

use super::bash_cmd::{base_name, command_and_args, INTERPRETERS};
use super::bash_parse::command_words;
use super::operator_cmd::shell_payload;

const NETWORK_COMMANDS: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "socat", "ssh", "scp", "sftp", "rsync",
    "ftp", "telnet", "http", "httpie", "aria2c", "lftp",
];
const GIT_NETWORK_COMMANDS: &[&str] = &["push", "fetch", "clone", "pull", "ls-remote"];
const MAX_REPARSE_DEPTH: usize = 2;
const MAX_REPARSE_BYTES: usize = 64 * 1024;

fn network_verb(cmd: &str, args: &[String]) -> bool {
    if matches!(cmd, "busybox" | "toybox") {
        return args.first().is_some_and(|applet| NETWORK_COMMANDS.contains(&applet.as_str()));
    }
    if NETWORK_COMMANDS.contains(&cmd) { return true; }
    if cmd != "git" { return false; }
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if matches!(arg.as_str(), "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env") {
            args.next();
        } else if arg == "--" || !arg.starts_with('-') {
            let verb = if arg == "--" { args.next().map(String::as_str) } else { Some(arg.as_str()) };
            return verb.is_some_and(|verb| GIT_NETWORK_COMMANDS.contains(&verb)
                || (verb == "remote" && args.find(|arg| !arg.starts_with('-'))
                    .is_some_and(|arg| matches!(arg.as_str(), "update" | "prune"))));
        }
    }
    false
}

/// Detect literal network verbs, including the existing bounded shell and
/// echo/printf reparsing. Interpreters, aliases, scripts, renamed executables
/// and `/dev/tcp` remain outside this speed bump.
pub fn bash_egress_command(command: &str) -> bool {
    network_at_depth(command, 0)
}

fn network_at_depth(command: &str, depth: usize) -> bool {
    for words in command_words(command) {
        let Some(parsed) = command_and_args(&words) else { continue; };
        if network_verb(&parsed.cmd, parsed.args) { return true; }
        // Only command-launching positions get this conservative scan.
        // Ordinary file names and search patterns are not executable words.
        if matches!(parsed.cmd.as_str(), "xargs" | "find" | "fd")
            && words.iter().enumerate().any(|(i, word)| network_verb(&base_name(word), &words[i + 1..]))
        { return true; }
        if depth >= MAX_REPARSE_DEPTH { continue; }
        if matches!(parsed.cmd.as_str(), "echo" | "printf") && parsed.args.iter()
            .filter(|arg| arg.len() <= MAX_REPARSE_BYTES)
            .any(|arg| network_at_depth(arg, depth + 1))
        { return true; }
        if shell_payload(&parsed.cmd, parsed.args)
            .filter(|payload| payload.len() <= MAX_REPARSE_BYTES)
            .is_some_and(|payload| network_at_depth(&payload, depth + 1))
        { return true; }
    }
    false
}

fn simple_literal(command: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for c in command.chars() {
        // Reject expansion even inside quotes: approvals cover literal text
        // only. `@` can name mutable input in several network tools.
        if matches!(c, '$' | '`' | '@' | '\n' | '\r' | '\0') { return false; }
        if escaped { escaped = false; continue; }
        if c == '\\' && quote != Some('\'') { escaped = true; continue; }
        if let Some(q) = quote {
            if c == q { quote = None; }
        } else if matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}' | '*' | '?' | '[' | ']' | '~' | '#') {
            return false;
        }
    }
    quote.is_none() && !escaped
}

/// Only a direct, single, literal network command can receive an ask ticket.
/// Wrappers and assignments are intentionally excluded from this small set.
pub fn approvable_egress_command(command: &str) -> bool {
    if !simple_literal(command) { return false; }
    let segments = command_words(command);
    let [words] = segments.as_slice() else { return false; };
    let Some(first) = words.first() else { return false; };
    let cmd = base_name(first);
    if !network_verb(&cmd, &words[1..]) { return false; }
    let verbs = words.iter().enumerate()
        .filter(|(i, word)| network_verb(&base_name(word), &words[i + 1..])).count();
    if verbs != 1 { return false; }
    !words.iter().any(|word| {
        command_words(word).iter().flatten().any(|part| {
            let base = base_name(part);
            INTERPRETERS.contains(&base.as_str()) || base.strip_prefix("python").is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_network_verbs_and_git_subcommands() {
        for verb in NETWORK_COMMANDS {
            assert!(bash_egress_command(&format!("{verb} example.test")), "{verb}");
            assert!(bash_egress_command(&format!("env LANG=C /usr/bin/{verb} example.test")), "{verb}");
        }
        for verb in GIT_NETWORK_COMMANDS {
            assert!(bash_egress_command(&format!("git -C repo {verb} origin")), "{verb}");
        }
        for command in ["git status", "git log", "git remote -v", "git show HEAD:curl", "printf hello", "rg curl file", "grep curl file", "cat curl", "ls wget", "wc ssh"] {
            assert!(!bash_egress_command(command), "{command}");
        }
    }

    #[test]
    fn egress_multicall_applets_and_remote_maintenance() {
        for command in ["busybox wget https://example.test", "toybox wget https://example.test", "git remote update", "git remote prune origin"] {
            assert!(bash_egress_command(command), "{command}");
        }
        for command in ["git remote -v", "git remote rename origin update", "git remote add prune https://example.test"] {
            assert!(!bash_egress_command(command), "{command}");
        }
        assert!(!bash_egress_command("busybox echo wget"));
        assert!(!bash_egress_command("toybox ls wget"));
    }

    #[test]
    fn egress_shell_reparse_is_bounded_and_literal() {
        for command in ["echo 'curl https://example.test' | sh", "printf '%s' 'wget https://example.test' | bash", "sh -lc 'curl https://example.test'", "bash -xc -o pipefail -- 'git fetch origin'", "eval 'git fetch origin'", "xargs curl", "find . -exec curl https://example.test \\;"] {
            assert!(bash_egress_command(command), "{command}");
            assert!(!approvable_egress_command(command), "{command}");
        }
        // Embedded interpreter programs are deliberately not parsed.
        assert!(!bash_egress_command("python -c 'import urllib.request; urllib.request.urlopen(\"https://example.test\")'"));
        assert!(!bash_egress_command("echo hello > /dev/tcp/example.test/80"));
    }

    #[test]
    fn egress_approval_requires_one_simple_literal_command() {
        for command in ["curl https://example.test", "curl 'https://example.test/?x=1&y=2'", "/usr/bin/wget https://example.test", "git fetch origin", "git -C repo push origin main", "c\\url https://example.test", "cu''rl https://example.test"] {
            assert!(approvable_egress_command(command), "{command}");
        }
        for command in ["curl $URL", "curl '${URL}'", "curl $(cat url)", "curl `cat url`", "curl -d @secret https://example.test", "curl -F 'f=@secret' https://example.test", "curl a | cat", "curl a; curl b", "curl a && echo done", "curl a > out", "curl <(cat secret)", "X=y curl a", "env curl a", "curl 'unclosed", "curl a\n", "curl *", "ssh host 'sh -c curl'", "ssh host python3", "curl a wget", "echo hello"] {
            assert!(!approvable_egress_command(command), "{command}");
        }
    }
}
