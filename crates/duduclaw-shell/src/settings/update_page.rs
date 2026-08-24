// D4b — 更新 (system update).
//
// Drives the two `device.*` update RPCs that already exist (H3a wired the
// `systemd-sysupdate` binary into the image; the gateway routes both through
// `duduclaw-sysd`, so nothing new is needed on the privileged side for this
// page):
//
//   device.update_status  -> sysd `systemd-sysupdate list --json=short`
//   device.update_apply   -> sysd `systemd-sysupdate update`
//
// Both answer the shared device-op envelope `{success, stdout, stderr}` —
// note `success` is the underlying COMMAND's exit status, so `ok:true` at
// the RPC level with `success:false` inside is a real and common shape (the
// command ran and failed). This page renders those as two different things.
//
// ── What this page deliberately does NOT do ────────────────────────────
// It has no reboot button. A completed A/B update takes effect at the next
// boot, and adding a second destructive control here would duplicate the
// power menu the lock screen already owns (`device.power_local`) with a
// different confirmation story. The page says so in words instead.
//
// It also does not offer rollback: `device.update_rollback` exists but
// `SystemDeviceOps::update_rollback` always answers `Unsupported` (H3's boot-
// counting work package is what will make it real). Rendering a button that
// is guaranteed to fail is precisely the outcome this app's honesty contract
// forbids.

use gpui::{div, prelude::*, px, Context, Div};

use duduclaw_native_gui::theme;
use serde_json::Value;

use super::widgets::{self, Tone};
use super::{client, spawn_rpc, Load};
use crate::palette::ShellPalette;
use crate::ShellView;

pub(crate) const STATUS_METHOD: &str = "device.update_status";
pub(crate) const APPLY_METHOD: &str = "device.update_apply";

/// How much of an unparseable `stdout` to show, in CHARACTERS (not bytes —
/// this crate has no CJK-safe byte-truncation helper of its own, and slicing
/// by byte index would panic mid-codepoint the moment a message contains
/// anything non-ASCII; see this repo's coding convention 1).
const RAW_OUTPUT_MAX_CHARS: usize = 400;

/// One row of `systemd-sysupdate list --json=short`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VersionRow {
    pub(crate) version: String,
    pub(crate) installed: bool,
    pub(crate) available: bool,
    pub(crate) obsolete: bool,
}

/// A settled `device.update_status`. Keeps BOTH the structured rows and the
/// raw text: sysupdate's JSON shape is not something this shell controls, so
/// when the parse yields nothing the honest fallback is to show what the
/// command actually printed rather than an empty "no updates" screen that
/// would be indistinguishable from a real "you are up to date".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateStatus {
    pub(crate) command_ok: bool,
    pub(crate) rows: Vec<VersionRow>,
    pub(crate) raw_stdout: String,
    pub(crate) stderr: String,
}

impl UpdateStatus {
    /// The version currently running, when sysupdate reported one.
    pub(crate) fn installed_version(&self) -> Option<&str> {
        self.rows.iter().find(|r| r.installed).map(|r| r.version.as_str())
    }

    /// A newer version that is fetchable and not already installed. `None`
    /// means "nothing offered", which is NOT the same as "up to date" when
    /// `rows` is empty — see `has_structured_answer`.
    pub(crate) fn candidate_version(&self) -> Option<&str> {
        self.rows.iter().find(|r| r.available && !r.installed && !r.obsolete).map(|r| r.version.as_str())
    }

    /// Whether the command produced something this page could actually
    /// understand. Guards every "你已是最新版本" claim: saying that on an
    /// unparseable answer would be inventing a fact.
    pub(crate) fn has_structured_answer(&self) -> bool {
        !self.rows.is_empty()
    }
}

/// A settled `device.update_apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyOutcome {
    pub(crate) command_ok: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UpdatePageState {
    pub(crate) status: Load<UpdateStatus>,
    /// `true` from the moment 立即更新 is pressed until the RPC settles. The
    /// authoritative guard — the button checks THIS, not a visual state, so
    /// a double click can never start two `systemd-sysupdate update` runs.
    pub(crate) applying: bool,
    /// The most recent apply result, kept until the next one starts.
    pub(crate) last_apply: Option<Result<ApplyOutcome, client::SettingsRpcError>>,
}

/// Pure: the shared device-op envelope -> [`UpdateStatus`].
pub(crate) fn parse_status(payload: &Value) -> UpdateStatus {
    let command_ok = payload.get("success").and_then(Value::as_bool).unwrap_or(false);
    let raw_stdout = payload.get("stdout").and_then(Value::as_str).unwrap_or_default().to_string();
    let stderr = payload.get("stderr").and_then(Value::as_str).unwrap_or_default().to_string();
    UpdateStatus { command_ok, rows: parse_version_rows(&raw_stdout), raw_stdout, stderr }
}

/// Pure: `systemd-sysupdate list --json=short`'s stdout -> rows.
///
/// Deliberately tolerant in one direction only. It accepts a top-level array
/// (the documented shape), an object with a `"versions"`/`"transfers"` array
/// (shapes seen across systemd releases), or the REAL shape this appliance's
/// own `systemd-sysupdate` binary actually emits — `{"current":"0.1.0",
/// "all":["0.1.0"],"appstreamUrls":[]}` — found live during the M1 VM sweep
/// (2026-08-24): every prior round's build only ever produced the honest
/// "無法判斷" fallback in practice, because NONE of the shapes this function
/// recognized before today matched the command it actually parses. It skips
/// any entry with no usable `version` string. It never SYNTHESISES a row:
/// text it cannot parse yields an empty vec, and the caller then shows the
/// raw output.
pub(crate) fn parse_version_rows(stdout: &str) -> Vec<VersionRow> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Vec::new();
    };
    if let Some(rows) = parse_current_all_shape(&value) {
        return rows;
    }
    let list = match &value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => map
            .get("versions")
            .or_else(|| map.get("transfers"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut rows = Vec::new();
    for item in list {
        let Some(version) = item.get("version").and_then(Value::as_str) else {
            continue;
        };
        if version.trim().is_empty() {
            continue;
        }
        let flag = |key: &str| item.get(key).and_then(Value::as_bool).unwrap_or(false);
        rows.push(VersionRow {
            version: version.to_string(),
            installed: flag("installed"),
            // An entry with no explicit `available` flag is still a version
            // sysupdate listed, so treat listed-and-not-installed as
            // available — that is what "list" means. `false` here would hide
            // every candidate on a systemd build that omits the key.
            available: item.get("available").and_then(Value::as_bool).unwrap_or(true),
            obsolete: flag("obsolete"),
        });
    }
    rows
}

/// The real `systemd-sysupdate list --json=short` shape: a top-level OBJECT
/// with `"current"` (the installed version string) and `"all"` (every
/// version string sysupdate knows about, INCLUDING current — a flat array of
/// STRINGS, not objects). Unlike the `"versions"`/`"transfers"` shape above,
/// these entries carry no per-item flags at all, so `installed`/`available`
/// have to be DERIVED from `current` and list membership rather than read
/// off each entry.
///
/// Returns `None` (not an empty `Vec`) when the shape does not match at all,
/// so the caller can fall through to the other shapes rather than treating a
/// present-but-different object as "this shape, zero rows".
fn parse_current_all_shape(value: &Value) -> Option<Vec<VersionRow>> {
    let map = value.as_object()?;
    let current = map.get("current").and_then(Value::as_str)?;
    let all = map.get("all").and_then(Value::as_array)?;
    let mut rows = Vec::new();
    for entry in all {
        let Some(version) = entry.as_str() else { continue };
        if version.trim().is_empty() {
            continue;
        }
        rows.push(VersionRow {
            version: version.to_string(),
            installed: version == current,
            // Listed at all ⇒ sysupdate can fetch/has fetched it — the same
            // "listed means available" reading the other shape uses for a
            // missing `available` key.
            available: true,
            // This shape carries no obsolete/superseded concept; never
            // invented, same default the other shape uses for a missing key.
            obsolete: false,
        });
    }
    Some(rows)
}

fn parse_apply(payload: &Value) -> ApplyOutcome {
    ApplyOutcome {
        command_ok: payload.get("success").and_then(Value::as_bool).unwrap_or(false),
        stdout: payload.get("stdout").and_then(Value::as_str).unwrap_or_default().to_string(),
        stderr: payload.get("stderr").and_then(Value::as_str).unwrap_or_default().to_string(),
    }
}

/// CJK-safe truncation by CHARACTER count. See `RAW_OUTPUT_MAX_CHARS`.
pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().nth(max_chars).is_some() {
        out.push('…');
    }
    out
}

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.update.status.needs_load() {
        return;
    }
    view.settings_ui.update.status = Load::Loading;
    spawn_rpc(
        cx,
        || client::call(STATUS_METHOD, serde_json::json!({})),
        |view, result, cx| {
            view.settings_ui.update.status = match result {
                Ok(payload) => Load::Loaded(parse_status(&payload)),
                Err(e) => {
                    eprintln!("[settings/update] {STATUS_METHOD} failed: {e:?}");
                    Load::Failed(e)
                }
            };
            cx.notify();
        },
    );
}

fn apply_update(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.settings_ui.update.applying {
        return;
    }
    view.settings_ui.update.applying = true;
    view.settings_ui.update.last_apply = None;
    cx.notify();
    spawn_rpc(
        cx,
        || client::call(APPLY_METHOD, serde_json::json!({})),
        |view, result, cx| {
            view.settings_ui.update.applying = false;
            view.settings_ui.update.last_apply = Some(result.map(|payload| parse_apply(&payload)));
            // Whatever happened, the previous status listing is now stale.
            view.settings_ui.update.status = Load::NotLoaded;
            ensure_loaded(view, cx);
            cx.notify();
        },
    );
}

pub(crate) fn render(body: Div, state: &UpdatePageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    body.child(status_card(state, palette, cx)).child(notes_card(palette))
}

fn status_card(state: &UpdatePageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let busy = state.applying;
    let refresh = widgets::button(
        "settings-update-refresh",
        "重新檢查".to_string(),
        widgets::ButtonWeight::Secondary,
        !busy && !matches!(state.status, Load::Loading),
        palette,
        cx.listener(|view, _ev, _window, cx| {
            view.settings_ui.update.status = Load::NotLoaded;
            ensure_loaded(view, cx);
            cx.notify();
        }),
    );
    let mut card = widgets::card(palette).child(widgets::card_header("系統更新", Some(refresh.into_any_element()), palette));

    card = match &state.status {
        Load::NotLoaded | Load::Loading => card.child(widgets::notice_static("檢查中…", Tone::Muted, palette)),
        Load::Failed(e) if e.is_not_appliance() => {
            card.child(widgets::notice_static("這台電腦不是 DuDuClaw 值班機，沒有系統更新可以套用。", Tone::Muted, palette))
        }
        Load::Failed(e) => card.child(widgets::notice(e.user_message(), Tone::Danger, palette)),
        Load::Loaded(status) => {
            let mut section = card
                .child(widgets::value_row("目前版本", status.installed_version().unwrap_or("—").to_string(), palette))
                .child(widgets::value_row("可更新版本", status.candidate_version().unwrap_or("—").to_string(), palette));

            if !status.command_ok {
                section = section.child(widgets::notice_static("更新檢查指令執行失敗，下方是它的原始輸出。", Tone::Danger, palette));
            } else if !status.has_structured_answer() {
                // The one claim this page must never make on a blank answer.
                section = section.child(widgets::notice_static(
                    "更新服務沒有回報任何版本清單，因此無法判斷是否已是最新版本。",
                    Tone::Warning,
                    palette,
                ));
            } else if status.candidate_version().is_none() {
                section = section.child(widgets::notice_static("目前已是最新版本。", Tone::Success, palette));
            }

            if !status.has_structured_answer() && !(status.raw_stdout.trim().is_empty() && status.stderr.trim().is_empty()) {
                section = section.child(raw_output_block(&status.raw_stdout, &status.stderr, palette));
            }

            let can_apply = !busy && status.command_ok && status.candidate_version().is_some();
            section = section.child(
                div().flex().items_center().gap(px(10.)).child(widgets::button(
                    "settings-update-apply",
                    if busy { "更新中…".to_string() } else { "立即更新".to_string() },
                    widgets::ButtonWeight::Primary,
                    can_apply,
                    palette,
                    cx.listener(|view, _ev, _window, cx| apply_update(view, cx)),
                )),
            );
            if busy {
                // The one instruction that matters while an A/B write is in
                // flight, and the reason this page has an explicit busy
                // state at all rather than just a spinner.
                section = section.child(widgets::notice_static("更新中，請勿關機或拔除電源。", Tone::Warning, palette));
            }
            section
        }
    };

    if let Some(outcome) = &state.last_apply {
        card = card.child(apply_result_line(outcome, palette));
    }
    card
}

fn apply_result_line(outcome: &Result<ApplyOutcome, client::SettingsRpcError>, palette: ShellPalette) -> Div {
    match outcome {
        Ok(ok) if ok.command_ok => widgets::notice_static("更新已下載完成，重新開機後生效。", Tone::Success, palette),
        Ok(failed) => {
            // The command ran and failed — show what it said, truncated, and
            // never dressed up as a success.
            let detail = if failed.stderr.trim().is_empty() { failed.stdout.clone() } else { failed.stderr.clone() };
            widgets::notice(format!("更新失敗：{}", truncate_chars(detail.trim(), 160)), Tone::Danger, palette)
        }
        Err(e) => widgets::notice(e.user_message(), Tone::Danger, palette),
    }
}

fn raw_output_block(stdout: &str, stderr: &str, palette: ShellPalette) -> Div {
    let mut combined = stdout.trim().to_string();
    if !stderr.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr.trim());
    }
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(widgets::field_label("原始輸出", palette))
        .child(
            div()
                .p(px(10.))
                .rounded(px(8.))
                .bg(theme::alpha(palette.app_shell, 1.0))
                .text_size(px(11.))
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .child(truncate_chars(&combined, RAW_OUTPUT_MAX_CHARS)),
        )
}

fn notes_card(palette: ShellPalette) -> Div {
    widgets::card(palette)
        .child(widgets::card_header("關於更新方式", None, palette))
        .child(widgets::notice_static(
            "本機採用 A/B 雙分割更新：新版本會寫入另一個分割區，重新開機後才切換過去，因此更新過程不會影響目前正在執行的服務。",
            Tone::Muted,
            palette,
        ))
        .child(widgets::notice_static("更新完成後請由鎖定畫面的電源選單重新開機。", Tone::Muted, palette))
        .child(widgets::notice_static("自動退回舊版本的功能仍在開發中，本版尚未提供手動退版按鈕。", Tone::Warning, palette))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(stdout: &str, ok: bool) -> UpdateStatus {
        parse_status(&json!({ "success": ok, "stdout": stdout, "stderr": "" }))
    }

    #[test]
    fn a_documented_array_listing_parses_into_rows() {
        let s = status(r#"[{"version":"0.2.0","installed":false,"available":true,"obsolete":false},
                           {"version":"0.1.0","installed":true,"available":true,"obsolete":false}]"#, true);
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.installed_version(), Some("0.1.0"));
        assert_eq!(s.candidate_version(), Some("0.2.0"));
        assert!(s.has_structured_answer());
    }

    #[test]
    fn an_object_wrapped_listing_also_parses() {
        let s = status(r#"{"versions":[{"version":"0.3.0","installed":true}]}"#, true);
        assert_eq!(s.installed_version(), Some("0.3.0"));
    }

    /// The REAL `systemd-sysupdate list --json=short` shape — found live
    /// during the M1 VM sweep (2026-08-24): this appliance's actual binary
    /// answers `{"current":"0.1.0","all":["0.1.0"],"appstreamUrls":[]}`, not
    /// either shape above. Before this test/fix, this exact real-world
    /// output always fell through to "無法判斷" on every real machine —
    /// caught by clicking through the live Settings 更新 page, not by
    /// reading the code.
    #[test]
    fn the_real_sysupdate_current_all_shape_parses() {
        let s = status(r#"{"current":"0.1.0","all":["0.1.0"],"appstreamUrls":[]}"#, true);
        assert!(s.has_structured_answer(), "the real shape must not fall back to the unparseable-output message");
        assert_eq!(s.installed_version(), Some("0.1.0"));
        assert_eq!(s.candidate_version(), None, "the only listed version IS the installed one — nothing to offer");
    }

    /// Same real shape, but `all` actually offers something newer.
    #[test]
    fn the_real_sysupdate_shape_offers_a_newer_version_when_one_is_listed() {
        let s = status(r#"{"current":"0.1.0","all":["0.1.0","0.2.0"],"appstreamUrls":[]}"#, true);
        assert_eq!(s.installed_version(), Some("0.1.0"));
        assert_eq!(s.candidate_version(), Some("0.2.0"));
    }

    /// An object that merely HAPPENS to have neither `"current"`/`"all"` nor
    /// `"versions"`/`"transfers"` must still fall through to the honest
    /// empty answer, not panic or silently match the wrong branch.
    #[test]
    fn an_object_with_none_of_the_known_shapes_yields_no_rows() {
        let s = status(r#"{"unexpected":"shape"}"#, true);
        assert!(s.rows.is_empty());
        assert!(!s.has_structured_answer());
    }

    /// An obsolete or already-installed entry is never offered as an update.
    #[test]
    fn obsolete_and_installed_entries_are_not_candidates() {
        let s = status(r#"[{"version":"0.0.9","installed":false,"available":true,"obsolete":true},
                           {"version":"0.1.0","installed":true,"available":true}]"#, true);
        assert_eq!(s.candidate_version(), None);
    }

    /// A systemd build that omits `available` must not hide every candidate.
    #[test]
    fn a_listed_entry_without_an_available_flag_still_counts_as_offered() {
        let s = status(r#"[{"version":"0.2.0","installed":false}]"#, true);
        assert_eq!(s.candidate_version(), Some("0.2.0"));
    }

    /// The load-bearing honesty test: unparseable output must NOT read as
    /// "you are up to date".
    #[test]
    fn unparseable_output_yields_no_rows_and_no_up_to_date_claim() {
        let s = status("sysupdate: no configuration found", true);
        assert!(s.rows.is_empty());
        assert!(!s.has_structured_answer(), "an unstructured answer must never license an up-to-date claim");
        assert_eq!(s.installed_version(), None);
        assert_eq!(s.candidate_version(), None);
        assert_eq!(s.raw_stdout, "sysupdate: no configuration found", "the raw text has to survive for the fallback block");
    }

    #[test]
    fn entries_without_a_usable_version_are_skipped_not_faked() {
        let rows = parse_version_rows(r#"[{"installed":true},{"version":"   "},{"version":"0.1.0"}]"#);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].version, "0.1.0");
    }

    #[test]
    fn empty_and_blank_stdout_parse_to_nothing_without_panicking() {
        assert!(parse_version_rows("").is_empty());
        assert!(parse_version_rows("   \n ").is_empty());
        assert!(parse_version_rows("null").is_empty());
    }

    /// `ok:true` at the RPC level with a non-zero command exit is a real
    /// shape and must survive as "the command failed".
    #[test]
    fn a_failed_command_inside_a_successful_rpc_is_recorded_as_failed() {
        let s = parse_status(&json!({ "success": false, "stdout": "", "stderr": "sysupdate: not found" }));
        assert!(!s.command_ok);
        assert_eq!(s.stderr, "sysupdate: not found");
    }

    /// Coding convention 1: never slice a string by raw byte index.
    #[test]
    fn truncation_is_codepoint_safe_for_cjk_output() {
        let text = "更新失敗：找不到設定檔";
        let cut = truncate_chars(text, 4);
        assert_eq!(cut, "更新失敗…");
        assert_eq!(truncate_chars(text, 999), text, "no ellipsis when nothing was cut");
        assert_eq!(truncate_chars("", 4), "");
    }

    #[test]
    fn a_fresh_page_has_asked_nothing_and_is_not_applying() {
        let state = UpdatePageState::default();
        assert!(state.status.needs_load());
        assert!(!state.applying);
        assert!(state.last_apply.is_none());
    }
}
