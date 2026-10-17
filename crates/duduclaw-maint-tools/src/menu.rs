//! The maintenance menu: what it offers, how a command is recognised, and
//! exactly which argv each action runs.
//!
//! Two ways in, one grammar:
//! - **Command mode** — dropbear runs the menu as the forced command and puts
//!   what the client asked for in `SSH_ORIGINAL_COMMAND`. That string, split
//!   on ASCII whitespace, must equal one of [`Action::command`] word for word;
//!   anything else (`/bin/sh`, `sh -c id`, `scp -t .`, the sftp server path,
//!   `status; id`, `logs gateway --all`) exits with code 2 and the usage text.
//! - **Interactive mode** — missing or empty `SSH_ORIGINAL_COMMAND`: a numbered,
//!   bilingual menu read line by line from stdin. Works without a terminal
//!   and never looks at `TERM`.
//!
//! The menu's own `argv` (dropbear passes `-c <forced command>`) is ignored.
//! No byte of client input is ever passed to a child: every argument below is
//! a constant, and the only thing input does is pick one of nine actions.
//! The two restarts are not run here at all: the menu asks the root
//! `duduclaw-maint-helper` for `restart-gateway` / `restart-model` (owner
//! decision 2026-10-16: no polkit).

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

use crate::journal::{PRIORITY_INFO, PRIORITY_WARNING};
use crate::protocol::{
    GATEWAY_UNIT, LogRequest, LogSource, MODEL_UNIT, REPLY_OK_PREFIX, RestartTarget,
};
use crate::tools::Tool;

/// Longest `SSH_ORIGINAL_COMMAND` considered at all. The longest valid
/// command is 23 bytes.
pub const MAX_COMMAND_BYTES: usize = 256;

/// Exit code for a refused command (design §5.1).
pub const EXIT_REFUSED: i32 = 2;
/// Exit code when an action ran but a step failed (restart refused, log
/// service unreachable, a program missing).
pub const EXIT_FAILED: i32 = 1;

/// One menu action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Status,
    Logs(LogRequest),
    Resources,
    Network,
    RestartModel,
    RestartGateway,
}

impl Action {
    /// Every action, in menu order (menu number = index + 1).
    pub const ALL: [Action; 9] = [
        Action::Status,
        Action::Logs(LogRequest::ALL[0]),
        Action::Logs(LogRequest::ALL[1]),
        Action::Logs(LogRequest::ALL[2]),
        Action::Logs(LogRequest::ALL[3]),
        Action::Resources,
        Action::Network,
        Action::RestartModel,
        Action::RestartGateway,
    ];

    /// The exact command-mode spelling (design §5.1 table).
    pub fn command(self) -> &'static str {
        match self {
            Action::Status => "status",
            Action::Logs(LogRequest {
                source: LogSource::Gateway,
                follow: false,
            }) => "logs gateway",
            Action::Logs(LogRequest {
                source: LogSource::Gateway,
                follow: true,
            }) => "logs gateway follow",
            Action::Logs(LogRequest {
                source: LogSource::Model,
                follow: false,
            }) => "logs model",
            Action::Logs(LogRequest {
                source: LogSource::Model,
                follow: true,
            }) => "logs model follow",
            Action::Resources => "resources",
            Action::Network => "network",
            Action::RestartModel => "restart model",
            Action::RestartGateway => "restart gateway confirm",
        }
    }

    /// Menu label, zh-TW / English.
    pub fn label(self) -> &'static str {
        match self {
            Action::Status => "系統狀態 / Status",
            Action::Logs(LogRequest {
                source: LogSource::Gateway,
                follow: false,
            }) => "gateway 紀錄 / Gateway logs",
            Action::Logs(LogRequest {
                source: LogSource::Gateway,
                follow: true,
            }) => "持續顯示 gateway 紀錄（Ctrl-C 返回）/ Follow gateway logs (Ctrl-C to return)",
            Action::Logs(LogRequest {
                source: LogSource::Model,
                follow: false,
            }) => "本地模型紀錄 / Local model logs",
            Action::Logs(LogRequest {
                source: LogSource::Model,
                follow: true,
            }) => "持續顯示本地模型紀錄（Ctrl-C 返回）/ Follow local model logs (Ctrl-C to return)",
            Action::Resources => "資源使用 / Resources",
            Action::Network => "網路 / Network",
            Action::RestartModel => "重新啟動本地模型 / Restart the local model",
            Action::RestartGateway => {
                "重新啟動 gateway（會結束維修視窗）/ Restart the gateway (ends the maintenance window)"
            }
        }
    }
}

/// Recognise a command-mode string. Exact word-for-word match after
/// splitting on ASCII whitespace; anything else is `None`.
pub fn parse_command(original: &str) -> Option<Action> {
    if original.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let words: Vec<&str> = original.split_ascii_whitespace().collect();
    Action::ALL
        .into_iter()
        .find(|a| a.command().split(' ').eq(words.iter().copied()))
}

/// What a line typed at the interactive prompt means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    Run(Action),
    Exit,
    Invalid,
}

/// `1`–`9` pick an action, `0` / `q` / `exit` / `quit` leave.
pub fn parse_menu_choice(input: &str) -> MenuChoice {
    match input.trim() {
        "0" | "q" | "exit" | "quit" => MenuChoice::Exit,
        s => match s.parse::<usize>() {
            Ok(n) if (1..=Action::ALL.len()).contains(&n) && s.len() == 1 => {
                MenuChoice::Run(Action::ALL[n - 1])
            }
            _ => MenuChoice::Invalid,
        },
    }
}

/// `systemctl` options every call carries: no pager, and never ask for a
/// password (nothing the menu runs needs privileges; a prompt would only
/// hang the session).
pub const SYSTEMCTL_BASE: [&str; 2] = ["--no-pager", "--no-ask-password"];

/// Units `status` reports on, each only if it exists on this edition.
pub const STATUS_UNITS: [&str; 6] = [
    GATEWAY_UNIT,
    "duduclaw-sysd.service",
    MODEL_UNIT,
    "duduclaw-kiosk.service",
    "nftables.service",
    "dropbear.socket",
];

/// Mount points `resources` reports with `df -h`.
pub const DF_MOUNTS: [&str; 2] = ["/", "/data"];

/// One step of an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A section heading printed before the next step.
    Heading(&'static str),
    /// Run a program with fixed arguments.
    Run {
        tool: Tool,
        args: Vec<&'static str>,
        /// Whether a non-zero exit makes the action fail (display commands
        /// such as `systemctl status` exit 3 for an inactive unit, which is
        /// information, not failure).
        check_status: bool,
        /// Skip quietly when the program is not installed (design: "若存在").
        optional: bool,
    },
    /// `systemctl status -n 0 <unit>` if the unit is loaded on this device.
    UnitStatus(&'static str),
    /// Relay one log request from the helper.
    Logs(LogRequest),
    /// Ask the helper to restart one fixed unit.
    Restart(RestartTarget),
}

fn systemctl(rest: &[&'static str], check_status: bool) -> Step {
    let mut args = SYSTEMCTL_BASE.to_vec();
    args.extend_from_slice(rest);
    Step::Run {
        tool: Tool::Systemctl,
        args,
        check_status,
        optional: false,
    }
}

fn show(tool: Tool, args: &[&'static str], optional: bool) -> Step {
    Step::Run {
        tool,
        args: args.to_vec(),
        check_status: false,
        optional,
    }
}

/// The fixed steps of `action`.
pub fn plan(action: Action) -> Vec<Step> {
    match action {
        Action::Status => {
            let mut steps = vec![
                Step::Heading("== 失敗的服務 / Failed units =="),
                systemctl(&["--failed"], false),
                Step::Heading("== 服務狀態 / Service status =="),
            ];
            steps.extend(STATUS_UNITS.iter().map(|u| Step::UnitStatus(u)));
            steps
        }
        Action::Logs(req) => vec![Step::Logs(req)],
        Action::Resources => {
            let mut df = vec!["-h"];
            df.extend_from_slice(&DF_MOUNTS);
            vec![
                Step::Heading("== 開機時間與負載 / Uptime and load =="),
                show(Tool::Uptime, &[], false),
                Step::Heading("== 記憶體（MB）/ Memory (MB) =="),
                show(Tool::Free, &["-m"], false),
                Step::Heading("== 磁碟 / Disk =="),
                show(Tool::Df, &df, false),
            ]
        }
        Action::Network => vec![
            Step::Heading("== 位址 / Addresses =="),
            show(Tool::Ip, &["-brief", "address"], false),
            Step::Heading("== 路由 / Routes =="),
            show(Tool::Ip, &["route"], false),
            Step::Heading("== DNS =="),
            show(Tool::Resolvectl, &["--no-pager", "status"], true),
        ],
        Action::RestartModel => vec![Step::Restart(RestartTarget::Model)],
        Action::RestartGateway => vec![Step::Restart(RestartTarget::Gateway)],
    }
}

/// Everything the dispatcher needs from the outside world. The real
/// implementation spawns processes ([`crate::system`]); tests record calls.
pub trait Executor {
    /// Absolute path of `tool`, if installed.
    fn resolve(&self, tool: Tool) -> Option<PathBuf>;
    /// Run to completion with the terminal as stdout/stderr. `Ok(None)` when
    /// the child was killed by a signal.
    fn run(&mut self, program: &Path, args: &[String]) -> io::Result<Option<i32>>;
    /// Run and return (bounded) stdout.
    fn capture(&mut self, program: &Path, args: &[String]) -> io::Result<String>;
    /// Relay one log request to stdout.
    fn stream_logs(&mut self, req: LogRequest) -> io::Result<()>;
    /// Ask the helper for a restart; returns its status line.
    fn request_restart(&mut self, target: RestartTarget) -> io::Result<String>;
    /// Write one journal entry (identifier `duduclaw-maint`).
    fn record(&mut self, priority: u8, message: &str, fields: &[(&str, &str)]);
    /// Text for the technician (stdout).
    fn print(&mut self, text: &str);
    /// Warnings and errors (stderr).
    fn warn(&mut self, text: &str);
}

/// Interactive or command mode, as recorded in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Interactive,
    Command,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Interactive => "interactive",
            Mode::Command => "command",
        }
    }
}

/// Warning shown before the gateway restart.
pub const GATEWAY_RESTART_WARNING: &str = "重新啟動 gateway 後維修視窗會關閉，這條連線也會中斷。\nRestarting the gateway closes the maintenance window and ends this connection.\n";

/// Usage text for refused commands.
pub fn usage() -> String {
    let mut s = String::from(
        "這個帳號只能執行下列維修指令 / This account only runs these maintenance commands:\n",
    );
    for a in Action::ALL {
        s.push_str("  ");
        s.push_str(a.command());
        s.push('\n');
    }
    s.push_str(
        "例如 / For example: ssh duduclaw-maint@duduclaw.local logs gateway > gateway.log\n",
    );
    s
}

/// Run one action. Returns whether every checked step succeeded.
pub fn execute_action<E: Executor + ?Sized>(action: Action, mode: Mode, exec: &mut E) -> bool {
    // Recorded before running, so a gateway restart that ends this session
    // is still on record.
    exec.record(
        PRIORITY_INFO,
        &format!("maint-shell action: {}", action.command()),
        &[
            ("MAINT_ACTION", action.command()),
            ("MAINT_MODE", mode.as_str()),
        ],
    );
    let mut ok = true;
    for step in plan(action) {
        ok &= run_step(&step, exec);
    }
    if !ok && matches!(action, Action::RestartModel | Action::RestartGateway) {
        exec.warn("重新啟動沒有成功 / The restart did not succeed.\n");
    }
    ok
}

fn run_step<E: Executor + ?Sized>(step: &Step, exec: &mut E) -> bool {
    match step {
        Step::Heading(h) => {
            exec.print(&format!("\n{h}\n"));
            true
        }
        Step::Run {
            tool,
            args,
            check_status,
            optional,
        } => {
            let Some(path) = exec.resolve(*tool) else {
                if *optional {
                    return true;
                }
                exec.warn(&format!(
                    "找不到 {0}，無法執行 / {0} is not installed\n",
                    tool.binary_name()
                ));
                return false;
            };
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            match exec.run(&path, &args) {
                Ok(Some(0)) => true,
                Ok(_) => !*check_status,
                Err(e) => {
                    exec.warn(&format!(
                        "無法執行 {} / could not run it: {e}\n",
                        tool.binary_name()
                    ));
                    false
                }
            }
        }
        Step::UnitStatus(unit) => run_unit_status(unit, exec),
        Step::Restart(target) => match exec.request_restart(*target) {
            Ok(line) => {
                exec.print(&format!("{line}\n"));
                line.starts_with(REPLY_OK_PREFIX)
            }
            Err(e) => {
                exec.warn(&format!(
                    "無法連到維修輔助服務 / The maintenance helper is not reachable: {e}\n"
                ));
                false
            }
        },
        Step::Logs(req) => match exec.stream_logs(*req) {
            Ok(()) => true,
            Err(e) => {
                exec.warn(&format!(
                    "無法取得紀錄（維修輔助服務沒有回應）/ Logs are unavailable (maintenance helper not reachable): {e}\n"
                ));
                false
            }
        },
    }
}

/// `systemctl show -p LoadState --value <unit>`; `status -n 0` only when
/// loaded, so a unit that does not exist on this edition (the kiosk on the
/// base edition) shows one plain line instead of an error.
fn run_unit_status<E: Executor + ?Sized>(unit: &str, exec: &mut E) -> bool {
    let Some(systemctl) = exec.resolve(Tool::Systemctl) else {
        exec.warn("找不到 systemctl / systemctl is not installed\n");
        return false;
    };
    let mut show: Vec<String> = SYSTEMCTL_BASE.iter().map(|s| s.to_string()).collect();
    show.extend(["show", "-p", "LoadState", "--value", unit].map(String::from));
    match exec.capture(&systemctl, &show) {
        Ok(state) if state.trim() == "loaded" => {
            let mut status: Vec<String> = SYSTEMCTL_BASE.iter().map(|s| s.to_string()).collect();
            status.extend(["status", "-n", "0", unit].map(String::from));
            match exec.run(&systemctl, &status) {
                Ok(_) => true,
                Err(e) => {
                    exec.warn(&format!(
                        "無法執行 systemctl / could not run systemctl: {e}\n"
                    ));
                    false
                }
            }
        }
        Ok(_) => {
            exec.print(&format!(
                "（{unit}：此裝置沒有安裝 / not installed on this device）\n"
            ));
            true
        }
        Err(e) => {
            exec.warn(&format!(
                "無法執行 systemctl / could not run systemctl: {e}\n"
            ));
            false
        }
    }
}

/// Command mode. Returns the process exit code.
pub fn run_command_mode<E: Executor + ?Sized>(original: &str, exec: &mut E) -> i32 {
    let Some(action) = parse_command(original) else {
        let len = original.len().to_string();
        exec.record(
            PRIORITY_WARNING,
            "maint-shell rejected command",
            &[
                ("MAINT_REJECTED_LEN", &len),
                ("MAINT_MODE", Mode::Command.as_str()),
            ],
        );
        exec.warn(&usage());
        return EXIT_REFUSED;
    };
    if action == Action::RestartGateway {
        exec.warn(GATEWAY_RESTART_WARNING);
    }
    if execute_action(action, Mode::Command, exec) {
        0
    } else {
        EXIT_FAILED
    }
}

/// The numbered menu text.
pub fn menu_text() -> String {
    let mut s = String::from("\nDuDuClaw 維修選單 / Maintenance menu\n");
    for (i, a) in Action::ALL.iter().enumerate() {
        s.push_str(&format!("  {}) {}\n", i + 1, a.label()));
    }
    s.push_str("  0) 離開 / Exit\n");
    s
}

/// Banner shown once when an interactive session starts.
pub const BANNER: &str = "DuDuClaw 維修連線。這個連線的每個操作都會留下紀錄。\nDuDuClaw maintenance session. Every action in this session is recorded.\n";

/// Interactive mode: menu loop over `input` until `0` or end of input.
/// Returns the process exit code (0).
pub fn run_interactive<R: BufRead + ?Sized, E: Executor + ?Sized>(
    input: &mut R,
    exec: &mut E,
) -> i32 {
    exec.print(BANNER);
    let mut buf = Vec::new();
    loop {
        exec.print(&menu_text());
        exec.print("請選擇 / Choose [0-9]: ");
        let line = match read_input_line(input, &mut buf) {
            InputLine::Eof => {
                exec.print("\n");
                return 0;
            }
            InputLine::TooLong => String::new(),
            InputLine::Line(l) => l,
        };
        match parse_menu_choice(&line) {
            MenuChoice::Exit => return 0,
            MenuChoice::Invalid => exec.print("請輸入 0 到 9 / Enter a number from 0 to 9\n"),
            MenuChoice::Run(Action::RestartGateway) => {
                exec.print(GATEWAY_RESTART_WARNING);
                exec.print("確定要繼續請輸入 yes / Type yes to continue: ");
                match read_input_line(input, &mut buf) {
                    InputLine::Line(l) if l.trim() == "yes" => {
                        execute_action(Action::RestartGateway, Mode::Interactive, exec);
                    }
                    InputLine::Eof => return 0,
                    _ => exec.print("已取消 / Cancelled\n"),
                }
            }
            MenuChoice::Run(action) => {
                execute_action(action, Mode::Interactive, exec);
            }
        }
    }
}

/// Longest line read at the interactive prompt. Longer input is consumed
/// and discarded without being held in memory.
pub const MAX_INPUT_LINE_BYTES: usize = 256;

enum InputLine {
    Line(String),
    TooLong,
    Eof,
}

/// One bounded line from the technician (read errors end the session like
/// end of input does).
fn read_input_line<R: BufRead + ?Sized>(input: &mut R, buf: &mut Vec<u8>) -> InputLine {
    match crate::logs_filter::read_bounded_line(input, MAX_INPUT_LINE_BYTES, buf) {
        Ok(crate::logs_filter::BoundedLine::Line) => {
            InputLine::Line(String::from_utf8_lossy(buf).into_owned())
        }
        Ok(crate::logs_filter::BoundedLine::TooLong) => InputLine::TooLong,
        Ok(crate::logs_filter::BoundedLine::Eof) | Err(_) => InputLine::Eof,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// (priority, message, fields) of one journal entry.
    type RecordedEntry = (u8, String, Vec<(String, String)>);

    /// Records every interaction; every tool "exists" under /usr/bin unless
    /// listed in `missing`; `run` returns `exit` for argv matching `fail_on`.
    #[derive(Default)]
    struct Recorder {
        runs: Vec<Vec<String>>,
        captures: Vec<Vec<String>>,
        logs: Vec<LogRequest>,
        records: Vec<RecordedEntry>,
        printed: String,
        warned: String,
        missing: Vec<Tool>,
        not_loaded: Vec<&'static str>,
        fail_on: Option<&'static str>,
        logs_fail: bool,
        restarts: Vec<RestartTarget>,
        /// Reply the fake helper gives to a restart (default "ok: …").
        restart_reply: Option<&'static str>,
        restart_unreachable: bool,
    }

    impl Executor for Recorder {
        fn resolve(&self, tool: Tool) -> Option<PathBuf> {
            (!self.missing.contains(&tool)).then(|| Path::new("/usr/bin").join(tool.binary_name()))
        }
        fn run(&mut self, program: &Path, args: &[String]) -> io::Result<Option<i32>> {
            let mut argv = vec![program.display().to_string()];
            argv.extend(args.iter().cloned());
            let fail = self.fail_on.is_some_and(|f| argv.iter().any(|a| a == f));
            self.runs.push(argv);
            Ok(Some(if fail { 4 } else { 0 }))
        }
        fn capture(&mut self, program: &Path, args: &[String]) -> io::Result<String> {
            let mut argv = vec![program.display().to_string()];
            argv.extend(args.iter().cloned());
            let unit = args.last().cloned().unwrap_or_default();
            self.captures.push(argv);
            Ok(if self.not_loaded.iter().any(|u| *u == unit) {
                "not-found\n"
            } else {
                "loaded\n"
            }
            .into())
        }
        fn stream_logs(&mut self, req: LogRequest) -> io::Result<()> {
            self.logs.push(req);
            if self.logs_fail {
                Err(io::Error::new(io::ErrorKind::NotFound, "no socket"))
            } else {
                Ok(())
            }
        }
        fn request_restart(&mut self, target: RestartTarget) -> io::Result<String> {
            self.restarts.push(target);
            if self.restart_unreachable {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no socket"));
            }
            Ok(self
                .restart_reply
                .map(String::from)
                .unwrap_or_else(|| format!("ok: restarted {}", target.unit())))
        }
        fn record(&mut self, priority: u8, message: &str, fields: &[(&str, &str)]) {
            self.records.push((
                priority,
                message.to_string(),
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ));
        }
        fn print(&mut self, text: &str) {
            self.printed.push_str(text);
        }
        fn warn(&mut self, text: &str) {
            self.warned.push_str(text);
        }
    }

    #[test]
    fn every_documented_command_is_accepted() {
        for a in Action::ALL {
            assert_eq!(parse_command(a.command()), Some(a), "{}", a.command());
        }
        // Surrounding and repeated ASCII whitespace is only a separator.
        assert_eq!(
            parse_command("  logs\tgateway   follow \n"),
            Some(Action::ALL[2])
        );
    }

    #[test]
    fn injection_and_near_misses_are_refused() {
        let cases = [
            "",
            " ",
            "/bin/sh",
            "sh",
            "sh -c id",
            "bash -i",
            "scp -t .",
            "scp -f /etc/shadow",
            "/usr/libexec/sftp-server",
            "internal-sftp",
            "rsync --server -vlogDtpre.iLsfxC . /tmp",
            "status; id",
            "status && id",
            "status | sh",
            "status `id`",
            "status $(id)",
            "status\0id",
            "status id",
            "STATUS",
            "Status",
            "logs",
            "logs gateway --all",
            "logs gateway -f",
            "logs gateway follow follow",
            "logs sysd",
            "logs dropbear",
            "logs kernel",
            "logs gateway-follow",
            "logs ../gateway",
            "logs _SYSTEMD_UNIT=dropbear.service",
            "restart",
            "restart gateway",
            "restart gateway yes",
            "restart gateway confirm now",
            "restart duduclaw-sysd.service",
            "restart sysd",
            "restart shell",
            "restart network",
            "reboot",
            "poweroff",
            "systemctl reboot",
            "network; reboot",
            "resources --help",
            "help",
            // Unicode whitespace is not a separator: NBSP glues words.
            "logs\u{00A0}gateway",
            "status\u{2028}",
        ];
        for c in cases {
            assert_eq!(parse_command(c), None, "must refuse {c:?}");
        }
        let long = format!("status{}", " ".repeat(MAX_COMMAND_BYTES));
        assert_eq!(parse_command(&long), None);
    }

    #[test]
    fn refused_command_runs_nothing_and_exits_2() {
        for c in [
            "sh -c id",
            "/usr/libexec/sftp-server",
            "status; reboot",
            "restart gateway",
        ] {
            let mut r = Recorder::default();
            assert_eq!(run_command_mode(c, &mut r), EXIT_REFUSED);
            assert!(
                r.runs.is_empty() && r.captures.is_empty() && r.logs.is_empty(),
                "{c}"
            );
            assert!(r.warned.contains("logs gateway"), "usage shown for {c}");
            // Recorded by length only: the client's text never reaches the journal.
            assert_eq!(r.records.len(), 1);
            let (prio, msg, fields) = &r.records[0];
            assert_eq!(*prio, PRIORITY_WARNING);
            assert!(!msg.contains(c));
            assert!(fields.iter().all(|(_, v)| !v.contains(c)));
            assert!(fields.contains(&("MAINT_REJECTED_LEN".into(), c.len().to_string())));
        }
    }

    #[test]
    fn every_systemctl_call_has_no_pager_and_no_ask_password() {
        for a in Action::ALL {
            let mut r = Recorder::default();
            run_command_mode(a.command(), &mut r);
            for argv in r.runs.iter().chain(r.captures.iter()) {
                assert!(argv[0].starts_with('/'), "absolute path: {argv:?}");
                if argv[0].ends_with("/systemctl") {
                    assert_eq!(&argv[1..3], ["--no-pager", "--no-ask-password"], "{argv:?}");
                }
            }
        }
    }

    #[test]
    fn restarts_go_to_the_helper_and_run_nothing_locally() {
        let mut r = Recorder::default();
        assert_eq!(run_command_mode("restart model", &mut r), 0);
        assert_eq!(r.restarts, [RestartTarget::Model]);
        assert!(
            r.runs.is_empty(),
            "no systemctl from the shell: {:?}",
            r.runs
        );
        assert!(
            r.printed
                .contains("ok: restarted duduclaw-llama-server.service")
        );

        let mut r = Recorder::default();
        assert_eq!(run_command_mode("restart gateway confirm", &mut r), 0);
        assert_eq!(r.restarts, [RestartTarget::Gateway]);
        assert!(r.runs.is_empty());
        assert!(r.warned.contains("closes the maintenance window"));

        // No action runs a restart, reboot or unit change locally.
        for a in Action::ALL {
            for step in plan(a) {
                if let Step::Run { args, .. } = step {
                    for bad in [
                        "restart",
                        "reboot",
                        "poweroff",
                        "halt",
                        "kexec",
                        "stop",
                        "start",
                        "edit",
                        "set-property",
                    ] {
                        assert!(!args.contains(&bad), "{a:?} runs {bad}");
                    }
                }
            }
        }
    }

    #[test]
    fn helper_refusals_are_reported_as_failure() {
        for reply in [
            "error: too soon, try again in 42 s",
            "error: restart failed (exit 1)",
            "error: not permitted",
            "",
        ] {
            let mut r = Recorder {
                restart_reply: Some(reply),
                ..Default::default()
            };
            assert_eq!(
                run_command_mode("restart model", &mut r),
                EXIT_FAILED,
                "{reply:?}"
            );
            assert!(r.printed.contains(reply));
            assert!(r.warned.contains("did not succeed"));
        }
        let mut r = Recorder {
            restart_unreachable: true,
            ..Default::default()
        };
        assert_eq!(
            run_command_mode("restart gateway confirm", &mut r),
            EXIT_FAILED
        );
        assert!(r.warned.contains("helper is not reachable"));
    }

    #[test]
    fn status_skips_units_missing_on_this_edition() {
        let mut r = Recorder {
            not_loaded: vec!["duduclaw-kiosk.service"],
            ..Default::default()
        };
        assert_eq!(run_command_mode("status", &mut r), 0);
        assert_eq!(r.captures.len(), STATUS_UNITS.len());
        let status_runs: Vec<_> = r
            .runs
            .iter()
            .filter(|a| a.contains(&"status".to_string()))
            .collect();
        assert_eq!(status_runs.len(), STATUS_UNITS.len() - 1);
        assert!(status_runs.iter().all(|a| a[3..6] == ["status", "-n", "0"]));
        assert!(r.printed.contains("duduclaw-kiosk.service"));
        assert!(r.runs[0].ends_with(&["--failed".to_string()]));
    }

    #[test]
    fn resources_and_network_use_fixed_argv() {
        // The mock resolves paths with Path::join, whose separator is native
        // to the test host; the fixed command arguments stay byte-for-byte.
        let program = |name: &str| Path::new("/usr/bin").join(name).display().to_string();
        let mut r = Recorder::default();
        run_command_mode("resources", &mut r);
        assert_eq!(
            r.runs,
            vec![
                vec![program("uptime")],
                vec![program("free"), "-m".into()],
                vec![program("df"), "-h".into(), "/".into(), "/data".into()],
            ]
        );
        let mut r = Recorder {
            missing: vec![Tool::Resolvectl],
            ..Default::default()
        };
        assert_eq!(
            run_command_mode("network", &mut r),
            0,
            "resolvectl is optional"
        );
        assert_eq!(
            r.runs,
            vec![
                vec![program("ip"), "-brief".into(), "address".into()],
                vec![program("ip"), "route".into()],
            ]
        );
    }

    #[test]
    fn missing_required_tool_fails_loudly() {
        let mut r = Recorder {
            missing: vec![Tool::Free],
            ..Default::default()
        };
        assert_eq!(run_command_mode("resources", &mut r), EXIT_FAILED);
        assert!(r.warned.contains("free"));
    }

    #[test]
    fn logs_go_through_the_helper_only() {
        let mut r = Recorder::default();
        assert_eq!(run_command_mode("logs gateway", &mut r), 0);
        assert_eq!(
            r.logs,
            [LogRequest {
                source: LogSource::Gateway,
                follow: false
            }]
        );
        assert!(r.runs.is_empty(), "no journalctl from the shell");
        let mut r = Recorder {
            logs_fail: true,
            ..Default::default()
        };
        assert_eq!(run_command_mode("logs model follow", &mut r), EXIT_FAILED);
        assert!(r.warned.contains("maintenance helper not reachable"));
    }

    #[test]
    fn each_action_records_one_journal_entry() {
        for a in Action::ALL {
            let mut r = Recorder::default();
            run_command_mode(a.command(), &mut r);
            assert_eq!(r.records.len(), 1, "{a:?}");
            let (prio, msg, fields) = &r.records[0];
            assert_eq!(*prio, PRIORITY_INFO);
            assert_eq!(msg, &format!("maint-shell action: {}", a.command()));
            assert!(fields.contains(&("MAINT_ACTION".into(), a.command().into())));
            assert!(fields.contains(&("MAINT_MODE".into(), "command".into())));
        }
    }

    #[test]
    fn menu_choices() {
        for (i, a) in Action::ALL.iter().enumerate() {
            assert_eq!(
                parse_menu_choice(&format!("{}\n", i + 1)),
                MenuChoice::Run(*a)
            );
        }
        for s in ["0", "q", "exit", " quit \n"] {
            assert_eq!(parse_menu_choice(s), MenuChoice::Exit);
        }
        for s in ["", "10", "01", "+1", "-1", "1;id", "status", "sh", "١"] {
            assert_eq!(parse_menu_choice(s), MenuChoice::Invalid, "{s:?}");
        }
    }

    #[test]
    fn interactive_runs_choices_until_exit() {
        let mut r = Recorder::default();
        let mut input = Cursor::new("1\n7\nbogus\n0\n8\n");
        assert_eq!(run_interactive(&mut input, &mut r), 0);
        assert_eq!(r.records.len(), 2, "status and network, nothing after 0");
        assert!(
            r.records
                .iter()
                .all(|(_, _, f)| f.contains(&("MAINT_MODE".into(), "interactive".into())))
        );
        assert!(r.printed.contains("Enter a number from 0 to 9"));
        assert!(r.restarts.is_empty());
    }

    #[test]
    fn interactive_gateway_restart_needs_yes() {
        for (answer, restarted) in [
            ("yes\n", true),
            ("y\n", false),
            ("YES\n", false),
            ("\n", false),
            ("yes please\n", false),
        ] {
            let mut r = Recorder::default();
            let mut input = Cursor::new(format!("9\n{answer}0\n"));
            run_interactive(&mut input, &mut r);
            let did = r.restarts.contains(&RestartTarget::Gateway);
            assert_eq!(did, restarted, "answer {answer:?}");
            assert!(r.printed.contains("ends this connection"));
        }
    }

    #[test]
    fn interactive_discards_overlong_lines() {
        let mut r = Recorder::default();
        let long = "7".repeat(MAX_INPUT_LINE_BYTES * 100);
        let mut input = Cursor::new(format!("{long}\n9\n{long}yes\n0\n"));
        assert_eq!(run_interactive(&mut input, &mut r), 0);
        assert!(r.records.is_empty(), "nothing ran");
        assert!(r.printed.contains("Enter a number from 0 to 9"));
        assert!(r.printed.contains("Cancelled"));
    }

    #[test]
    fn interactive_ends_on_eof() {
        let mut r = Recorder::default();
        assert_eq!(run_interactive(&mut Cursor::new(""), &mut r), 0);
        let mut r = Recorder::default();
        assert_eq!(run_interactive(&mut Cursor::new("9\n"), &mut r), 0);
        assert!(r.runs.is_empty());
    }
}
