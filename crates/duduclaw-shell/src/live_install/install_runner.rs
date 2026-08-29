// Y20-P3 (2026-08-29) — kicks off the real, destructive install write and
// the post-install reboot. Both are one-shot side effects triggered from
// `render.rs`'s `button_row` (the Confirm step's own "開始安裝" action, the
// Progress step's own "重新開機" action once the write settles) rather than
// from a step's own in-card button — see `render.rs`'s own header comment
// for why P3 folds these into the shared bottom-nav slot instead of growing
// a second forward-action button per step.
//
// ── Why a STREAMING bridge, not `oobe::steps::network`'s one-shot poll ────
// `kick_off_scan`/`kick_off_connect` (`oobe/steps/network.rs`) send exactly
// ONE result down their `mpsc::channel`, and the `cx.spawn` poll loop
// `break`s the instant it arrives — appropriate for a scan/connect call that
// settles once. Writing a multi-GB image takes anywhere from seconds to
// minutes, and this step needs to paint a LIVE, moving bar the whole time —
// so the background thread here sends an event PER progress sample (and per
// log line), and the poll loop keeps looping, draining every event already
// queued each tick, until an explicit `InstallEvent::Finished` sentinel
// arrives. New shape, not a copy of that one-shot pattern (task brief:
// "network.rs 範本只一次性回，這裡要 tx 迴圈 send...收到完成 sentinel 才
// break——新寫").
//
// ── The `DUDUCLAW_PROGRESS:<pct>` wire format ─────────────────────────────
// Defined and versioned together with the producer side in THIS round — see
// `duduclaw-os-install.sh`'s own header comment on its `dd`/`pv` step for
// the full contract. Any stdout line of the exact shape
// `DUDUCLAW_PROGRESS:<digits>` is a percentage sample; every other stdout OR
// stderr line is opaque log text, shown verbatim as the step's status line
// but never parsed for meaning beyond that. `parse_progress_line` below is
// the ONE place either side of that contract is decoded — a future format
// change only has to update this fn and the shell script's own emitter.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use gpui::Context;

use super::{InstallState, LiveInstallStep};
use crate::ShellView;

/// The installer's own executable name — installed at `${sbindir}/
/// duduclaw-os-install` by `duduclaw-os-installer.bb`'s `do_install`, which
/// puts it on `$PATH` for every root shell (and thus for `Command::new`'s
/// bare-name lookup) in the live image.
const INSTALL_BIN: &str = "duduclaw-os-install";

enum InstallEvent {
    Progress(u8),
    Status(String),
    Finished(Result<(), String>),
}

/// The Confirm step's own bottom-nav action (`render.rs`'s `button_row`,
/// invoked only while that button is enabled — which `LiveInstallFlow::
/// can_advance` gates on `confirm_checked`, so by the time this runs the
/// destructive-write acknowledgement has already happened). Guards against
/// re-entry (a second click landing while a write is already in-flight),
/// then: records `Running`, advances the step to `Progress` so the very
/// next render shows the live bar, and spawns the real write on a
/// background thread bridged back via `mpsc` + a `cx.spawn` poll loop — same
/// background-thread -> channel -> foreground-executor shape every other
/// real I/O call in this crate uses (see `oobe/steps/network.rs`'s own
/// header comment), generalized here to a many-message stream (this file's
/// own header comment explains why).
pub(super) fn start_install(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(flow) = view.live_install.as_mut() else { return };
    if flow.current() != LiveInstallStep::Confirm {
        // Not reachable via the UI (this fn is only ever wired to the
        // Confirm step's own bottom-nav button), a defensive no-op either
        // way.
        return;
    }
    if matches!(flow.install(), InstallState::Running { .. }) {
        return;
    }
    let Some(disk) = flow.selected_disk().cloned() else {
        // Not reachable in practice — `DiskSelect::can_advance` already
        // requires a selection before `Confirm` is even reachable — but
        // this is a destructive action, so a missing precondition fails
        // CLOSED (refuse and report) rather than falling through to an
        // unset env var the script would then have to guess about.
        flow.set_install_failed("未選擇目標磁碟，無法安裝".to_string());
        cx.notify();
        return;
    };

    flow.set_install_running(None, "準備安裝…".to_string());
    flow.next();
    cx.notify();

    let (tx, rx) = mpsc::channel::<InstallEvent>();
    let disk_name = disk.name.clone();
    std::thread::spawn(move || run_install(&disk_name, &tx));

    cx.spawn(async move |weak, cx| loop {
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(InstallEvent::Progress(pct)) => {
                    let _ = weak.update(cx, |view, cx| {
                        if let Some(flow) = view.live_install.as_mut() {
                            flow.set_install_percent(pct);
                        }
                        cx.notify();
                    });
                }
                Ok(InstallEvent::Status(line)) => {
                    let _ = weak.update(cx, |view, cx| {
                        if let Some(flow) = view.live_install.as_mut() {
                            flow.set_install_status(line);
                        }
                        cx.notify();
                    });
                }
                Ok(InstallEvent::Finished(result)) => {
                    let _ = weak.update(cx, |view, cx| {
                        if let Some(flow) = view.live_install.as_mut() {
                            match result {
                                Ok(()) => flow.set_install_done(),
                                Err(message) => flow.set_install_failed(message),
                            }
                        }
                        cx.notify();
                    });
                    finished = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            break;
        }
        cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
    })
    .detach();
}

/// The background-thread body — runs entirely off gpui's executors (real
/// process I/O, potentially minutes long). Spawns the real installer with
/// the automation env vars the task brief specifies, reads its stdout AND
/// stderr each on their own joined thread (both feeding the SAME `tx`, so
/// ordering between the two streams is whatever the OS scheduler gives —
/// acceptable here because every consumer of `InstallEvent::Status` only
/// ever displays the latest one line, never reconstructs a transcript), and
/// finishes with exactly one `Finished` sentinel — see this file's own
/// header comment for why this shape (not `kick_off_scan`'s one-shot) is
/// needed.
fn run_install(disk_name: &str, tx: &mpsc::Sender<InstallEvent>) {
    let mut command = Command::new(INSTALL_BIN);
    command
        .env("DUDUCLAW_INSTALL_TARGET", disk_name)
        .env("DUDUCLAW_INSTALL_YES", "1")
        .env("DUDUCLAW_INSTALL_PROGRESS", "1")
        .env("DUDUCLAW_INSTALL_POWEROFF", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = tx.send(InstallEvent::Finished(Err(format!("無法啟動 {INSTALL_BIN}：{e}"))));
            return;
        }
    };

    let stdout_handle = child.stdout.take().map(|out| {
        let tx = tx.clone();
        std::thread::spawn(move || pump_lines(out, &tx))
    });
    let stderr_handle = child.stderr.take().map(|err| {
        let tx = tx.clone();
        std::thread::spawn(move || pump_lines(err, &tx))
    });

    if let Some(handle) = stdout_handle {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_handle {
        let _ = handle.join();
    }

    let result = match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{INSTALL_BIN} 結束碼異常：{status:?}")),
        Err(e) => Err(format!("等待 {INSTALL_BIN} 結束時失敗：{e}")),
    };
    let _ = tx.send(InstallEvent::Finished(result));
}

/// Reads one process stream to EOF, classifying every line via
/// `parse_progress_line` and forwarding it as the matching `InstallEvent`.
/// An unreadable line (non-UTF-8 byte sequence) ends this stream's pump
/// early rather than panicking — `lines()` already yields `Result`, and
/// `map_while(Result::ok)` is the same "one malformed line costs that line,
/// not the whole read" discipline `audio/wpctl.rs`'s parsers document for
/// reading another process's output.
///
/// Y20-P4 (2026-08-29): every line is passed through `strip_ansi_sgr` first.
/// `duduclaw-os-install.sh`'s own `log()`/`warn()`/`err()`/`ok()` helpers
/// (its own header comment, `c_info`/`c_warn`/`c_err`/`c_ok`/`c_off`) wrap
/// every status line in an SGR color escape for a real terminal — this
/// crate's `steps::progress` status line is a plain gpui text run, not a
/// terminal, so the raw escape bytes were rendering as literal `▌[1;36m…`
/// noise instead of color. Stripping here (not in `steps::progress`) keeps
/// the fix at the single ingestion point for BOTH consumers of a pumped
/// line — `InstallEvent::Status` (displayed verbatim) and
/// `parse_progress_line` (matched exact-prefix) — even though the script's
/// own `DUDUCLAW_PROGRESS:` emitter never wraps its own line in color today;
/// a future change to that emitter can't silently regress this.
fn pump_lines(stream: impl std::io::Read, tx: &mpsc::Sender<InstallEvent>) {
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let line = strip_ansi_sgr(&line);
        match parse_progress_line(&line) {
            Some(pct) => {
                let _ = tx.send(InstallEvent::Progress(pct));
            }
            None => {
                let _ = tx.send(InstallEvent::Status(line));
            }
        }
    }
}

/// Strips ANSI CSI "Select Graphic Rendition" escape sequences
/// (`\x1b[<params>m`, e.g. `\x1b[1;36m` / `\x1b[0m`) from a single line of
/// text. Hand-rolled rather than pulling in the `regex` crate for one fixed
/// three-byte-prefix pattern — this crate has no other regex need (see
/// `Cargo.toml`'s own dependency list), and the shape is narrow enough
/// (`ESC` `[` then ASCII digits/`;` then a literal `m`) to scan by hand
/// without the risk a hand-rolled *parser* usually carries: an unrecognized
/// or truncated escape (missing final `m`, non-numeric params) is left
/// byte-for-byte in the output rather than guessed at or silently eaten —
/// same "don't invent structure that isn't there" discipline
/// `parse_progress_line`'s own doc comment holds itself to.
fn strip_ansi_sgr(line: &str) -> String {
    const ESC: char = '\u{1b}';
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ESC || chars.peek() != Some(&'[') {
            out.push(c);
            continue;
        }
        // Tentatively consume `ESC '['` plus every following digit/`;`,
        // stopping at the first character that is neither. Only a
        // sequence that ends in a literal `m` is an SGR code we recognize
        // and drop; anything else (an unterminated escape at EOF, or a CSI
        // sequence ending in some other final byte, e.g. cursor-movement
        // `H`/`K`) is restored verbatim into `out` rather than swallowed.
        let mut rest = chars.clone();
        rest.next(); // consume the '[' already peeked
        let terminated_m = loop {
            match rest.next() {
                Some(next) if next.is_ascii_digit() || next == ';' => continue,
                Some('m') => break true,
                _ => break false,
            }
        };
        if terminated_m {
            chars = rest;
        } else {
            // Not a recognized SGR sequence — keep the ESC and let the
            // outer loop re-examine `[` and everything after it normally.
            out.push(ESC);
        }
    }
    out
}

/// Decodes ONE line of `duduclaw-os-install.sh`'s `DUDUCLAW_PROGRESS:<pct>`
/// wire format (see this file's own header comment). Whole-line anchored
/// (`strip_prefix`, not a substring search — this project's own coding
/// convention 2 forbids unanchored `contains` for anything a decision is
/// based on) and clamped to `0..=100` since `pv -n`'s own numeric stream can
/// exceed 100 when the `-s` total the script estimated undershoots the real
/// decompressed size (see the script's own comment on why that total is
/// sometimes an estimate, not an exact figure).
fn parse_progress_line(line: &str) -> Option<u8> {
    let digits = line.strip_prefix("DUDUCLAW_PROGRESS:")?;
    let value: u32 = digits.trim().parse().ok()?;
    Some(value.min(100) as u8)
}

/// The Progress step's own bottom-nav action once the write has settled
/// (`LiveInstallFlow::can_advance` gates the button on `InstallState::Done`,
/// so — same reasoning `start_install` documents above — this only ever
/// runs once a completed write has already been confirmed). Fire-and-forget,
/// same shape `oobe::portal_browser::open_portal` uses for a spawn with
/// nothing to wait for: a failed spawn (e.g. a dev build running outside the
/// live image, where `reboot` may not exist on `$PATH` at all) is logged and
/// otherwise inert — there is nothing else on this screen left to do once
/// the install has already succeeded.
pub(super) fn start_reboot(view: &mut ShellView, _cx: &mut Context<ShellView>) {
    let Some(flow) = view.live_install.as_ref() else { return };
    if !matches!(flow.install(), InstallState::Done) {
        return;
    }
    if let Err(e) = Command::new("reboot").spawn() {
        eprintln!("[live_install] failed to spawn reboot: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_progress_line() {
        assert_eq!(parse_progress_line("DUDUCLAW_PROGRESS:42"), Some(42));
    }

    #[test]
    fn parses_zero_and_one_hundred() {
        assert_eq!(parse_progress_line("DUDUCLAW_PROGRESS:0"), Some(0));
        assert_eq!(parse_progress_line("DUDUCLAW_PROGRESS:100"), Some(100));
    }

    #[test]
    fn clamps_a_percentage_over_one_hundred() {
        // `pv -n`'s own stream can exceed 100 when the estimated `-s` total
        // undershoots the real decompressed size — see this file's own
        // header comment.
        assert_eq!(parse_progress_line("DUDUCLAW_PROGRESS:137"), Some(100));
    }

    #[test]
    fn rejects_ordinary_log_lines() {
        assert_eq!(parse_progress_line("[installer] 正在寫入映像..."), None);
    }

    #[test]
    fn rejects_a_non_numeric_suffix() {
        assert_eq!(parse_progress_line("DUDUCLAW_PROGRESS:abc"), None);
    }

    #[test]
    fn the_prefix_match_is_anchored_not_a_substring_search() {
        // Coding convention 2: no unanchored `contains`/`starts_with` for a
        // decision this matters for. A line that merely MENTIONS the tag
        // (e.g. inside an unrelated log sentence) must not be misread as a
        // sample.
        assert_eq!(parse_progress_line("see DUDUCLAW_PROGRESS:50 in the log"), None);
    }

    #[test]
    fn rejects_empty_digits() {
        assert_eq!(parse_progress_line("DUDUCLAW_PROGRESS:"), None);
    }

    #[test]
    fn rejects_a_negative_looking_value() {
        assert_eq!(parse_progress_line("DUDUCLAW_PROGRESS:-5"), None);
    }

    // ── strip_ansi_sgr (Y20-P4) ──────────────────────────────────────────

    #[test]
    fn strips_the_exact_info_color_the_installer_script_emits() {
        // `duduclaw-os-install.sh`'s own `log()`: `${c_info}[installer]${c_off} %s`
        // == "\x1b[1;36m[installer]\x1b[0m 正在寫入映像到 /dev/vda...".
        let raw = "\u{1b}[1;36m[installer]\u{1b}[0m 正在寫入映像到 /dev/vda...";
        assert_eq!(strip_ansi_sgr(raw), "[installer] 正在寫入映像到 /dev/vda...");
    }

    #[test]
    fn strips_warn_err_and_ok_colors_too() {
        assert_eq!(strip_ansi_sgr("\u{1b}[1;33m[installer]\u{1b}[0m warn text"), "[installer] warn text");
        assert_eq!(strip_ansi_sgr("\u{1b}[1;31m[installer] 錯誤:\u{1b}[0m err text"), "[installer] 錯誤: err text");
        assert_eq!(strip_ansi_sgr("\u{1b}[1;32m[installer]\u{1b}[0m ok text"), "[installer] ok text");
    }

    #[test]
    fn a_line_with_no_escape_codes_is_unchanged() {
        assert_eq!(strip_ansi_sgr("DUDUCLAW_PROGRESS:42"), "DUDUCLAW_PROGRESS:42");
        assert_eq!(strip_ansi_sgr("plain log line, no color at all"), "plain log line, no color at all");
    }

    #[test]
    fn strips_a_bare_reset_with_no_digits() {
        assert_eq!(strip_ansi_sgr("\u{1b}[mreset"), "reset");
    }

    #[test]
    fn an_unterminated_escape_at_end_of_line_is_preserved_not_swallowed() {
        // Truncated mid-sequence (e.g. a line split across a read boundary)
        // — must not eat real content on the theory that it "looks like" an
        // escape. See `strip_ansi_sgr`'s own doc comment.
        let raw = "before\u{1b}[1;36";
        assert_eq!(strip_ansi_sgr(raw), raw);
    }

    #[test]
    fn a_csi_sequence_not_ending_in_m_is_preserved_not_swallowed() {
        // e.g. cursor-position `\x1b[2K` (erase line) — a real CSI sequence,
        // but not an SGR color code, so this fn has no business dropping it.
        let raw = "\u{1b}[2Ktext";
        assert_eq!(strip_ansi_sgr(raw), raw);
    }

    #[test]
    fn a_lone_escape_not_followed_by_bracket_is_preserved() {
        let raw = "a\u{1b}b";
        assert_eq!(strip_ansi_sgr(raw), raw);
    }

    #[test]
    fn pump_lines_strips_color_before_classifying_a_progress_sample() {
        // Defensive: even though the shell script's own DUDUCLAW_PROGRESS
        // emitter never wraps its line in color today, stripping happens
        // BEFORE `parse_progress_line` runs, so a future change to the
        // emitter that added color could not silently break parsing.
        assert_eq!(parse_progress_line(&strip_ansi_sgr("\u{1b}[1;36mDUDUCLAW_PROGRESS:42\u{1b}[0m")), Some(42));
    }
}
