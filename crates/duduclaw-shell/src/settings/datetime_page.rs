// D4b — 日期與時間.
//
// Reads `device.timedate` and writes `device.timedate_set`; the gateway
// routes both through `duduclaw-sysd` (`set_timezone` / `set_ntp`), because
// changing either needs root. The clock itself is NOT settable here on
// purpose: this appliance runs `systemd-timesyncd` with NTP servers handed
// out by DHCP (see the appliance's own `.network` files), so the honest
// control is "is time syncing on", not "type today's date".
//
// ── Why a shortlist plus a free-text field, and not a tz picker ────────
// The IANA database has ~600 zones. A kiosk with no scrolling (see
// `settings/mod.rs`) cannot present them, and a searchable picker is a
// bigger component than this page earns. The nine shortcuts below cover the
// deployments this product actually ships into; anything else is typed, and
// validated three times over — here (shape), in the gateway (shape again,
// defence in depth), and in sysd (shape PLUS a real `/usr/share/zoneinfo`
// containment check, which is the only check that can actually prove a zone
// exists).

use gpui::{div, prelude::*, px, Context, Div};

use serde_json::Value;

use super::widgets::{self, Tone};
use super::{client, spawn_rpc, Load};
use crate::palette::ShellPalette;
use crate::ShellView;

pub(crate) const READ_METHOD: &str = "device.timedate";
pub(crate) const WRITE_METHOD: &str = "device.timedate_set";

/// Longest timezone name accepted — the same bound sysd enforces, checked
/// here so an obviously-wrong value never leaves the machine.
const MAX_TIMEZONE_LEN: usize = 64;

/// The shortcuts. `(IANA name, what an operator calls it)`.
pub(crate) const COMMON_ZONES: [(&str, &str); 9] = [
    ("Asia/Taipei", "台北"),
    ("Asia/Tokyo", "東京"),
    ("Asia/Shanghai", "上海"),
    ("Asia/Hong_Kong", "香港"),
    ("Asia/Singapore", "新加坡"),
    ("Europe/London", "倫敦"),
    ("America/New_York", "紐約"),
    ("America/Los_Angeles", "洛杉磯"),
    ("UTC", "世界標準時間"),
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TimeDateInfo {
    pub(crate) timezone: Option<String>,
    pub(crate) local_time: Option<String>,
    pub(crate) ntp_enabled: Option<bool>,
    pub(crate) ntp_synchronized: Option<bool>,
    /// The gateway's own flag for "I could not run `timedatectl` here".
    /// Absent ⇒ available, since only a gateway that KNOWS it failed sends
    /// it.
    pub(crate) available: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DateTimePageState {
    pub(crate) info: Load<TimeDateInfo>,
    /// A write is in flight. The authoritative double-submit guard.
    in_flight: bool,
    /// The last write's outcome, as an already-rendered line + tone. Kept as
    /// text rather than the error value so this struct stays `Eq` and the
    /// message is decided once, at settle time, next to the evidence.
    last_result: Option<(String, bool)>,
    /// A client-side validation complaint about the typed zone. Separate
    /// from `last_result`: nothing was sent, so calling it a failed write
    /// would misdescribe what happened.
    typed_error: Option<&'static str>,
}

impl DateTimePageState {
    fn begin(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        self.typed_error = None;
        true
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.in_flight
    }
}

/// Pure: `device.timedate`'s payload -> [`TimeDateInfo`].
pub(crate) fn parse_timedate(payload: &Value) -> TimeDateInfo {
    let s = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string).filter(|v| !v.trim().is_empty());
    TimeDateInfo {
        timezone: s("timezone"),
        local_time: s("local_time"),
        ntp_enabled: payload.get("ntp_enabled").and_then(Value::as_bool),
        ntp_synchronized: payload.get("ntp_synchronized").and_then(Value::as_bool),
        available: payload.get("available").and_then(Value::as_bool).unwrap_or(true),
    }
}

/// Pure: is this a plausibly-shaped IANA zone name?
///
/// Shape only. It cannot tell whether the zone EXISTS — only sysd's
/// `/usr/share/zoneinfo` containment check can, and it does. What this buys
/// is that an obviously-wrong value (a path traversal, a sentence, a
/// megabyte) never reaches the wire, and that the operator gets a specific
/// complaint instead of a generic backend refusal.
pub(crate) fn validate_timezone(raw: &str) -> Result<&str, &'static str> {
    let tz = raw.trim();
    if tz.is_empty() {
        return Err("請輸入時區名稱。");
    }
    if tz.len() > MAX_TIMEZONE_LEN {
        return Err("時區名稱太長。");
    }
    if !tz.is_ascii() {
        return Err("時區名稱只能使用英文、數字與 / 符號，例如 Asia/Taipei。");
    }
    if tz.starts_with('/') || tz.ends_with('/') || tz.contains("..") || tz.contains("//") {
        return Err("時區名稱格式不正確，例如 Asia/Taipei。");
    }
    if !tz.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.' | '/')) {
        return Err("時區名稱只能使用英文、數字與 / 符號，例如 Asia/Taipei。");
    }
    if tz.split('/').count() > 3 {
        return Err("時區名稱格式不正確，例如 Asia/Taipei。");
    }
    Ok(tz)
}

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.datetime.info.needs_load() {
        return;
    }
    view.settings_ui.datetime.info = Load::Loading;
    spawn_rpc(
        cx,
        || client::call(READ_METHOD, serde_json::json!({})),
        |view, result, cx| {
            view.settings_ui.datetime.info = match result {
                Ok(payload) => Load::Loaded(parse_timedate(&payload)),
                Err(e) => {
                    eprintln!("[settings/datetime] {READ_METHOD} failed: {e:?}");
                    Load::Failed(e)
                }
            };
            cx.notify();
        },
    );
}

fn write(view: &mut ShellView, params: Value, success_line: &'static str, cx: &mut Context<ShellView>) {
    if !view.settings_ui.datetime.begin() {
        return;
    }
    view.settings_ui.datetime.last_result = None;
    cx.notify();
    spawn_rpc(
        cx,
        move || client::call(WRITE_METHOD, params),
        move |view, result, cx| {
            view.settings_ui.datetime.in_flight = false;
            view.settings_ui.datetime.last_result = Some(match result {
                Ok(_) => (success_line.to_string(), true),
                Err(e) => {
                    eprintln!("[settings/datetime] {WRITE_METHOD} failed: {e:?}");
                    (e.user_message(), false)
                }
            });
            // Whatever happened, the displayed clock/zone is now suspect.
            view.settings_ui.datetime.info = Load::NotLoaded;
            ensure_loaded(view, cx);
            cx.notify();
        },
    );
}

fn set_timezone(view: &mut ShellView, timezone: String, cx: &mut Context<ShellView>) {
    write(view, serde_json::json!({ "timezone": timezone }), "時區已更新。", cx);
}

fn set_ntp(view: &mut ShellView, enabled: bool, cx: &mut Context<ShellView>) {
    let line = if enabled { "已開啟自動校時。" } else { "已關閉自動校時。" };
    write(view, serde_json::json!({ "ntp": enabled }), line, cx);
}

fn submit_typed_timezone(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.settings_ui.datetime.is_busy() {
        return;
    }
    let typed = view.settings_fields.timezone.read(cx).content(cx);
    match validate_timezone(&typed) {
        Ok(tz) => {
            let tz = tz.to_string();
            set_timezone(view, tz, cx);
        }
        Err(complaint) => {
            view.settings_ui.datetime.typed_error = Some(complaint);
            cx.notify();
        }
    }
}

// ── Render ───────────────────────────────────────────────────────────────

pub(crate) fn render(
    body: Div,
    state: &DateTimePageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    let mut body = body.child(now_card(state, palette, cx));
    if let Some(info) = state.info.value() {
        if info.available {
            body = body.child(timezone_card(info, state, fields, palette, cx));
        }
    }
    if let Some((message, ok)) = &state.last_result {
        body = body.child(widgets::notice(message.clone(), if *ok { Tone::Success } else { Tone::Danger }, palette));
    }
    body
}

fn now_card(state: &DateTimePageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let refresh = widgets::button(
        "settings-datetime-refresh",
        "重新整理".to_string(),
        widgets::ButtonWeight::Secondary,
        !state.is_busy() && !matches!(state.info, Load::Loading),
        palette,
        cx.listener(|view, _ev, _window, cx| {
            view.settings_ui.datetime.info = Load::NotLoaded;
            ensure_loaded(view, cx);
            cx.notify();
        }),
    );
    let card = widgets::card(palette).child(widgets::card_header("目前時間", Some(refresh.into_any_element()), palette));

    match &state.info {
        Load::NotLoaded | Load::Loading => card.child(widgets::notice_static("讀取中…", Tone::Muted, palette)),
        Load::Failed(e) if e.is_not_appliance() => {
            card.child(widgets::notice_static("這台電腦不是 DuDuClaw 值班機，時間設定由作業系統自己管理。", Tone::Muted, palette))
        }
        Load::Failed(e) => card.child(widgets::notice(e.user_message(), Tone::Danger, palette)),
        Load::Loaded(info) if !info.available => card.child(widgets::notice_static(
            "這台機器沒有可用的時間服務，因此無法顯示或調整時間。",
            Tone::Warning,
            palette,
        )),
        Load::Loaded(info) => {
            let card = card
                .child(widgets::value_row("本機時間", info.local_time.clone().unwrap_or_else(|| "—".to_string()), palette))
                .child(widgets::value_row("時區", info.timezone.clone().unwrap_or_else(|| "—".to_string()), palette));
            let ntp_on = info.ntp_enabled.unwrap_or(false);
            let toggle = widgets::toggle_pill(
                "settings-datetime-ntp",
                ntp_on,
                info.ntp_enabled.is_some() && !state.is_busy(),
                palette,
                cx.listener(move |view, _ev, _window, cx| set_ntp(view, !ntp_on, cx)),
            );
            let card = card.child(widgets::control_row(
                "自動校時",
                ntp_subtitle(info),
                toggle.into_any_element(),
                palette,
            ));
            if info.ntp_enabled.is_none() {
                card.child(widgets::notice_static("時間服務沒有回報自動校時狀態，因此無法切換。", Tone::Warning, palette))
            } else {
                card
            }
        }
    }
}

/// The one line that tells the operator whether the clock can be TRUSTED —
/// "on" and "actually synchronised" are different facts and are reported as
/// such, because a box that has been offline all day has NTP on and a wrong
/// clock.
pub(crate) fn ntp_subtitle(info: &TimeDateInfo) -> String {
    match (info.ntp_enabled, info.ntp_synchronized) {
        (None, _) => "狀態未知".to_string(),
        (Some(false), _) => "關閉時，時間需要由支援人員手動校正".to_string(),
        (Some(true), Some(true)) => "已與網路時間伺服器同步".to_string(),
        (Some(true), Some(false)) => "已開啟，但尚未同步成功（可能是網路不通）".to_string(),
        (Some(true), None) => "已開啟".to_string(),
    }
}

fn timezone_card(
    info: &TimeDateInfo,
    state: &DateTimePageState,
    fields: &crate::oobe::SettingsFields,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let current = info.timezone.as_deref().unwrap_or_default().to_string();
    let enabled = !state.is_busy();

    let mut shortcuts = div().flex().flex_wrap().gap(px(8.));
    for (index, (zone, label)) in COMMON_ZONES.into_iter().enumerate() {
        shortcuts = shortcuts.child(widgets::segment(
            ("settings-datetime-zone", index),
            format!("{label}（{zone}）"),
            zone == current,
            enabled,
            palette,
            cx.listener(move |view, _ev, _window, cx| set_timezone(view, zone.to_string(), cx)),
        ));
    }

    let mut card = widgets::card(palette)
        .child(widgets::card_header("時區", None, palette))
        .child(shortcuts)
        .child(
            div()
                .flex()
                .items_end()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(widgets::field_label("其他時區（IANA 名稱，例如 Asia/Taipei）", palette))
                        .child(fields.timezone.clone()),
                )
                .child(widgets::button(
                    "settings-datetime-zone-apply",
                    "套用".to_string(),
                    widgets::ButtonWeight::Primary,
                    enabled,
                    palette,
                    cx.listener(|view, _ev, _window, cx| submit_typed_timezone(view, cx)),
                )),
        );
    if let Some(complaint) = state.typed_error {
        card = card.child(widgets::notice_static(complaint, Tone::Danger, palette));
    }
    card.child(widgets::notice_static("變更時區會立即生效，正在執行的排程會依新時區重新計算。", Tone::Muted, palette))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_full_payload_parses() {
        let info = parse_timedate(&json!({
            "timezone": "Asia/Taipei",
            "local_time": "2026-08-23T18:04:11+08:00",
            "ntp_enabled": true,
            "ntp_synchronized": true
        }));
        assert_eq!(info.timezone.as_deref(), Some("Asia/Taipei"));
        assert_eq!(info.ntp_enabled, Some(true));
        assert!(info.available, "an omitted availability flag means the gateway did not report a problem");
    }

    /// Tri-state on purpose: "off" and "we could not find out" must not both
    /// render as an off switch that looks settable.
    #[test]
    fn an_unreported_ntp_state_stays_unknown_rather_than_defaulting_to_off() {
        let info = parse_timedate(&json!({ "timezone": "UTC" }));
        assert_eq!(info.ntp_enabled, None);
        assert_eq!(ntp_subtitle(&info), "狀態未知");
    }

    #[test]
    fn an_unavailable_time_service_is_reported_as_such() {
        let info = parse_timedate(&json!({ "available": false }));
        assert!(!info.available);
        assert_eq!(info.timezone, None);
    }

    /// The distinction that decides whether the clock can be trusted.
    #[test]
    fn ntp_on_but_unsynchronised_says_so_instead_of_claiming_success() {
        let info = parse_timedate(&json!({ "ntp_enabled": true, "ntp_synchronized": false }));
        let line = ntp_subtitle(&info);
        assert!(line.contains("尚未同步"), "{line}");
    }

    #[test]
    fn common_zone_names_all_pass_our_own_validator() {
        for (zone, label) in COMMON_ZONES {
            assert_eq!(validate_timezone(zone), Ok(zone), "shortcut {label} would be rejected by the field it sits next to");
        }
    }

    /// The whole reason a client-side check exists: obviously-hostile input
    /// never leaves the machine.
    #[test]
    fn traversal_and_absolute_paths_are_refused_before_the_wire() {
        for bad in ["../../etc/passwd", "/etc/localtime", "Asia//Taipei", "Asia/Taipei/", "a/b/c/d"] {
            assert!(validate_timezone(bad).is_err(), "{bad} must not be accepted");
        }
    }

    #[test]
    fn empty_over_long_and_non_ascii_values_are_refused_with_specific_complaints() {
        assert!(validate_timezone("   ").is_err());
        assert!(validate_timezone(&"a".repeat(MAX_TIMEZONE_LEN + 1)).is_err());
        let non_ascii = validate_timezone("亞洲/台北");
        assert!(non_ascii.is_err());
        assert_ne!(non_ascii, validate_timezone("   "), "different problems must get different complaints");
    }

    #[test]
    fn a_valid_value_is_trimmed_not_merely_accepted() {
        assert_eq!(validate_timezone("  Asia/Taipei  "), Ok("Asia/Taipei"));
    }

    #[test]
    fn only_one_write_may_be_in_flight() {
        let mut state = DateTimePageState::default();
        assert!(state.begin());
        assert!(!state.begin(), "a second click must not double-submit");
    }

    /// Starting a write clears a stale validation complaint, so the operator
    /// never sees a red line describing input they already corrected.
    #[test]
    fn beginning_a_write_clears_the_previous_typed_complaint() {
        let mut state = DateTimePageState::default();
        state.typed_error = Some("請輸入時區名稱。");
        assert!(state.begin());
        assert_eq!(state.typed_error, None);
    }
}
