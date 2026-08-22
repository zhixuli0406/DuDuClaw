// Fake data for the OOBE screens — Shell-S1.
//
// OOBE has no `.dc.html` design board this round (task brief: "OOBE 無畫布
// 畫板——版面照研究檔的原生慣例自行設計，記為『視覺拍板待使用者親測』"), so
// this copy is originated for this task rather than lifted from a board —
// still centralized in one file, same presentation/data split `home.rs`'s
// own `fake_data.rs` keeps for the Home surface.
//
// Honest stub (Shell-S1 round 3, OOBE i18n): every string in THIS file stays
// hardcoded zh-TW even though every OOBE step screen that reads it now
// re-renders through `crate::i18n` on a language change. Deliberate, not an
// oversight — this is DATA standing in for real-world entity names (a Wi-Fi
// network's own SSID, an example account name), not screen chrome, the same
// category the task brief's own item 4 carves out for Home/overlay. Only
// `FAKE_TEMPLATES.{title,desc}` is a genuinely arguable case (an invented
// industry template's display name IS UI copy, not a proper noun) — left
// untranslated this round to avoid turning `title`/`desc` into effectively
// dead fields (an id-keyed i18n lookup would stop reading them at all), not
// because the case for translating them is weak. Flagged here for whoever
// wires Templates i18n for real in a later round.

/// Shell-S3 (2026-08-21) update: this table is no longer read directly by
/// `steps::network` — it now backs `network::FakeNetworkBackend::scan()`
/// (`oobe/network/fake.rs`), reached through the `NetworkBackend` trait
/// like the real NetworkManager backend is. The table itself is unchanged
/// (still hand-authored example SSIDs, still not routed through
/// `crate::i18n` — see this file's own header comment above), but "選取→假
/// 連線成功" (Shell-S1's original, no-failure-path behavior) no longer
/// applies verbatim: `FakeNetworkBackend::connect` now honors the same
/// PSK-length precondition a secured network needs, so a click on one of
/// the `secured: true` rows below CAN fail in the dev/demo build too — see
/// that module's own header comment.
pub(super) struct FakeWifiNetwork {
    pub(super) ssid: &'static str,
    /// 1..=4, purely decorative (rendered as filled bars, see
    /// `steps::network::signal_bars` — plain `div()`s, no glyph and no icon
    /// asset: the approved OOBE boards contain no SVG at all, so there is
    /// nothing here to wire up the way ICON-1 wired the Home/overlay
    /// boards' own icons).
    pub(super) signal_bars: u8,
    pub(super) secured: bool,
}

pub(super) const FAKE_WIFI_NETWORKS: &[FakeWifiNetwork] = &[
    FakeWifiNetwork { ssid: "DuDu-Office", signal_bars: 4, secured: true },
    FakeWifiNetwork { ssid: "DuDu-Guest", signal_bars: 3, secured: false },
    FakeWifiNetwork { ssid: "5F-Meeting-Room", signal_bars: 2, secured: true },
    FakeWifiNetwork { ssid: "iPhone 的個人熱點", signal_bars: 3, secured: true },
];

/// `AccountCreate` step's field placeholder hints — round 1 used these as
/// prefilled fake VALUES (no real typing yet); round 2 replaces the fields
/// themselves with real `widgets::OobeTextField` entities (see that
/// struct's own header comment) and repurposes these same two constants as
/// PLACEHOLDER hint text instead (shown only while a field is empty), kept
/// rather than invented fresh so the "example shape" these were always
/// showing stays the same.
pub(super) const FAKE_ACCOUNT_NAME: &str = "Louis";
pub(super) const FAKE_ACCOUNT_PASSWORD_MASK: &str = "••••••••";

/// `Templates` step's "自訂" list — task brief: "自訂清單（3-4 個假板模卡
/// 可單選）". `id` is the STABLE key persisted into `TemplateChoice::Custom`
/// (see that variant's own doc comment in `oobe/mod.rs`) — `title`/`desc`
/// are display-only and free to reword without touching persisted state.
pub(super) struct FakeTemplate {
    pub(super) id: &'static str,
    pub(super) title: &'static str,
    pub(super) desc: &'static str,
}

pub(super) const FAKE_TEMPLATES: &[FakeTemplate] = &[
    FakeTemplate { id: "retail", title: "零售門市", desc: "客服應答、庫存提醒、促銷排程" },
    FakeTemplate { id: "clinic", title: "診所助理", desc: "預約提醒、初診問卷、回診追蹤" },
    FakeTemplate { id: "logistics", title: "物流倉儲", desc: "到貨通知、異常回報、司機調度" },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_wifi_networks_has_at_least_three_entries() {
        assert!(FAKE_WIFI_NETWORKS.len() >= 3);
    }

    #[test]
    fn no_field_is_empty_and_signal_bars_in_range() {
        for net in FAKE_WIFI_NETWORKS {
            assert!(!net.ssid.is_empty());
            assert!((1..=4).contains(&net.signal_bars), "{} out of range", net.ssid);
        }
        assert!(!FAKE_ACCOUNT_NAME.is_empty());
        assert!(!FAKE_ACCOUNT_PASSWORD_MASK.is_empty());
    }

    #[test]
    fn wifi_ssids_are_unique() {
        let mut ssids: Vec<&str> = FAKE_WIFI_NETWORKS.iter().map(|n| n.ssid).collect();
        ssids.sort_unstable();
        ssids.dedup();
        assert_eq!(ssids.len(), FAKE_WIFI_NETWORKS.len());
    }

    #[test]
    fn fake_templates_has_three_to_four_entries() {
        assert!((3..=4).contains(&FAKE_TEMPLATES.len()), "task brief: \"3-4 個假板模卡\"");
    }

    #[test]
    fn fake_template_fields_are_not_empty() {
        for tpl in FAKE_TEMPLATES {
            assert!(!tpl.id.is_empty());
            assert!(!tpl.title.is_empty());
            assert!(!tpl.desc.is_empty());
        }
    }

    #[test]
    fn fake_template_ids_are_unique() {
        let mut ids: Vec<&str> = FAKE_TEMPLATES.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), FAKE_TEMPLATES.len());
    }
}
