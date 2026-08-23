// D4b — 關於 (about this machine).
//
// Reads `device.about` (admin + appliance) and renders it as a spec sheet.
// Every field is `Option`: the gateway reads `/etc/os-release`,
// `/proc/sys/kernel/osrelease` and `/etc/hostname`, all of which can be
// absent (off-Linux, a partial image), and it answers `null` rather than
// guessing — this page renders an em-dash for those, never a plausible
// placeholder.
//
// `device_id` is deliberately a DERIVED value, not `/etc/machine-id`:
// systemd's own documentation treats the machine ID as confidential
// (`machine-id(5)`: "this ID... should not be used in untrusted
// environments... derive an application-specific ID instead"). The gateway
// hashes it; this page only displays what it is given.

use gpui::{div, prelude::*, px, Context, Div};

use duduclaw_native_gui::theme;
use serde_json::Value;

use super::widgets::{self, Tone};
use super::{client, spawn_rpc, Load};
use crate::palette::ShellPalette;
use crate::ShellView;

pub(crate) const METHOD: &str = "device.about";

/// The gateway's `device.about` answer. Every OS-level field is `Option`
/// because every one of them has a real "cannot be read here" case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AboutInfo {
    pub(crate) os_pretty_name: Option<String>,
    pub(crate) os_version_id: Option<String>,
    pub(crate) kernel: Option<String>,
    pub(crate) hostname: Option<String>,
    /// The gateway binary's own version — always present (it is a compile-
    /// time constant on the far end), unlike everything above it.
    pub(crate) gateway_version: Option<String>,
    pub(crate) device_id: Option<String>,
    pub(crate) is_appliance: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AboutPageState {
    pub(crate) info: Load<AboutInfo>,
}

/// Pure: `device.about`'s payload -> [`AboutInfo`]. Tolerant by design — a
/// gateway that predates a field simply omits it, and this page shows an
/// em-dash rather than failing the whole read. A wrong TYPE (a number where
/// a string belongs) is treated the same way as absent, for the same reason:
/// one odd field must not blank the other seven.
pub(crate) fn parse_about(payload: &Value) -> AboutInfo {
    let s = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string).filter(|v| !v.trim().is_empty());
    AboutInfo {
        os_pretty_name: s("os_pretty_name"),
        os_version_id: s("os_version_id"),
        kernel: s("kernel"),
        hostname: s("hostname"),
        gateway_version: s("gateway_version"),
        device_id: s("device_id"),
        is_appliance: payload.get("is_appliance").and_then(Value::as_bool).unwrap_or(false),
    }
}

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.about.info.needs_load() {
        return;
    }
    view.settings_ui.about.info = Load::Loading;
    spawn_rpc(
        cx,
        || client::call(METHOD, serde_json::json!({})),
        |view, result, cx| {
            view.settings_ui.about.info = match result {
                Ok(payload) => Load::Loaded(parse_about(&payload)),
                Err(e) => {
                    eprintln!("[settings/about] {METHOD} failed: {e:?}");
                    Load::Failed(e)
                }
            };
            cx.notify();
        },
    );
}

pub(crate) fn render(body: Div, state: &AboutPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // Same "the page opening IS the click" exception `pointer_settings::
    // render` documents: the first read is armed from the render body
    // because there is no other moment to hang it on, and `ensure_loaded`
    // is idempotent (it claims `Loading` before spawning).
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    body.child(machine_card(state, palette, cx)).child(licence_card(palette))
}

fn machine_card(state: &AboutPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let refresh = widgets::button(
        "settings-about-refresh",
        "重新整理".to_string(),
        widgets::ButtonWeight::Secondary,
        !matches!(state.info, Load::Loading),
        palette,
        cx.listener(|view, _ev, _window, cx| {
            view.settings_ui.about.info = Load::NotLoaded;
            ensure_loaded(view, cx);
            cx.notify();
        }),
    );
    let card = widgets::card(palette).child(widgets::card_header("這台機器", Some(refresh.into_any_element()), palette));

    match &state.info {
        Load::NotLoaded | Load::Loading => card.child(widgets::notice_static("讀取中…", Tone::Muted, palette)),
        Load::Failed(e) if e.is_not_appliance() => card.child(widgets::notice_static(
            "這台電腦不是 DuDuClaw 值班機，沒有機器資訊可以顯示。",
            Tone::Muted,
            palette,
        )),
        Load::Failed(e) => card.child(widgets::notice(e.user_message(), Tone::Danger, palette)),
        Load::Loaded(info) => {
            let dash = || "—".to_string();
            card.child(widgets::value_row("系統版本", info.os_pretty_name.clone().unwrap_or_else(dash), palette))
                .child(widgets::value_row("版本編號", info.os_version_id.clone().unwrap_or_else(dash), palette))
                .child(widgets::value_row("核心", info.kernel.clone().unwrap_or_else(dash), palette))
                .child(widgets::value_row("機器名稱", info.hostname.clone().unwrap_or_else(dash), palette))
                .child(widgets::value_row("服務版本", info.gateway_version.clone().unwrap_or_else(dash), palette))
                .child(widgets::value_row("裝置識別碼", info.device_id.clone().unwrap_or_else(dash), palette))
                .child(widgets::notice_static(
                    "裝置識別碼是由本機序號推導出的代碼，用於支援與授權比對，不會透露機器的原始序號。",
                    Tone::Muted,
                    palette,
                ))
        }
    }
}

/// Text-only, on purpose: this round has no licence viewer and inventing a
/// button that opens nothing would be worse than a sentence saying where the
/// texts live.
fn licence_card(palette: ShellPalette) -> Div {
    widgets::card(palette)
        .child(widgets::card_header("開源授權", None, palette))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child("DuDuClaw OS 建立在 Linux、systemd、Mesa、Wayland 等開放原始碼專案之上。"),
        )
        .child(widgets::value_row("授權文件位置", "/usr/share/doc/".to_string(), palette))
        .child(widgets::notice_static(
            "本版尚未內建授權瀏覽器，請由支援人員透過終端機或遠端管理介面查閱上述目錄。",
            Tone::Muted,
            palette,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_full_payload_parses_every_field() {
        let info = parse_about(&json!({
            "os_pretty_name": "DuDuClaw OS 0.1.0",
            "os_version_id": "0.1.0",
            "kernel": "6.12.0-amd64",
            "hostname": "duty-box-01",
            "gateway_version": "1.61.0",
            "device_id": "a1b2c3d4e5f6a7b8",
            "is_appliance": true
        }));
        assert_eq!(info.os_pretty_name.as_deref(), Some("DuDuClaw OS 0.1.0"));
        assert_eq!(info.kernel.as_deref(), Some("6.12.0-amd64"));
        assert_eq!(info.device_id.as_deref(), Some("a1b2c3d4e5f6a7b8"));
        assert!(info.is_appliance);
    }

    /// The gateway answers `null` for anything it genuinely could not read.
    /// That must survive as `None` (rendered as an em-dash), never as the
    /// string "null" and never as an invented default.
    #[test]
    fn nulls_and_absences_both_become_none_not_a_placeholder_string() {
        let info = parse_about(&json!({ "gateway_version": "1.61.0", "os_pretty_name": null }));
        assert_eq!(info.os_pretty_name, None);
        assert_eq!(info.kernel, None);
        assert_eq!(info.hostname, None);
        assert_eq!(info.gateway_version.as_deref(), Some("1.61.0"));
        assert!(!info.is_appliance, "an absent flag must not read as an appliance");
    }

    /// A field present but blank is the same fact as absent — showing an
    /// empty row would look like a rendering bug.
    #[test]
    fn a_blank_string_is_treated_as_absent() {
        let info = parse_about(&json!({ "hostname": "   " }));
        assert_eq!(info.hostname, None);
    }

    /// One wrong-typed field must not blank the rest of the sheet.
    #[test]
    fn a_wrong_typed_field_degrades_alone() {
        let info = parse_about(&json!({ "kernel": 6, "hostname": "duty-box-01" }));
        assert_eq!(info.kernel, None);
        assert_eq!(info.hostname.as_deref(), Some("duty-box-01"));
    }

    #[test]
    fn an_empty_payload_parses_without_panicking() {
        assert_eq!(parse_about(&json!({})), AboutInfo::default());
        assert_eq!(parse_about(&Value::Null), AboutInfo::default());
    }
}
