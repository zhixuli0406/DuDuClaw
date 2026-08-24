// D4a-6 (2026-08-24): the ControlCenter Wi-Fi quick tile's real backend.
//
// D4b built the whole settings 網路 page against real RPCs
// (`network.status`/`network.wifi_scan`/...) and deliberately left THIS
// tile alone — `commercial/docs/TODO-agent-first-os-2026-08.md`'s own D4a-6
// row: "D4b 刻意沒動 quick_tiles_row（那是並行「殼 chrome」包正在改的檔
// 案）". That parallel package has since landed and `controlcenter.rs` is
// no longer in flight, so this round closes the gap. It reuses
// `settings::network_page`'s own status RPC and parser wholesale —
// `network.status` + `parse_wifi` — rather than inventing a second reader
// against the same backend, matching the task's own "複用設定頁既有的後端
// 查詢路徑" instruction. It intentionally skips the SCAN half
// (`network.wifi_scan`): this tile shows the current link, not a network
// list, and firing a rescan every time ControlCenter opens would cost a
// real iwd round trip for information this row never displays. Passing an
// empty `{}` in place of a real scan payload into `parse_wifi` is not a
// shortcut around that function's contract — `parse_networks` (which
// `parse_wifi` calls internally) already treats a missing `networks` key as
// "zero networks", the same empty list `network_page::wifi_card` itself
// renders while a scan is still in flight.
//
// Bluetooth/勿擾 stay exactly as they were — `fake_data::QUICK_TILES`
// literals with no backend behind them yet; only the Wi-Fi entry is wired
// here, matched by id (`WIFI_TILE_ID`), the same string
// `fake_data::QUICK_TILES`'s own Wi-Fi row carries.
//
// State shape/lifecycle mirrors `codrive_row::CodriveUiState`: lives on
// `OverlayUiState` (this row renders inside `controlcenter::render`, which
// already receives `&OverlayUiState` — see that struct's own doc comment on
// why a state that only one card needs lives there rather than as a sibling
// `ShellView` field), loaded once via the same render-time-`cx.spawn`-then-
// `weak.update` idiom `codrive_row::render`/`audio::ensure_volume_probed`
// establish, and reset on every overlay-close path so a stale "已連線"
// cannot outlive an actual disconnect that happened while the panel was
// shut.

use gpui::Context;

use crate::settings::{self, network_page, Load};
use crate::ShellView;

/// The `fake_data::QuickTile::id` this module owns. This file's own test
/// (`the_tile_id_matches_a_real_entry_in_fake_data`) checks it still
/// matches an entry in `fake_data::QUICK_TILES` — if that literal ever gets
/// renamed, the tile would otherwise silently fall back to permanently
/// rendering the static Bluetooth/勿擾 treatment instead of failing loudly.
pub(crate) const WIFI_TILE_ID: &str = "tile-wifi";

const TILE_LOADING: &str = "讀取中…";
const TILE_CONNECTING: &str = "連線中…";
const TILE_DISCONNECTED: &str = "未連線";
const TILE_UNAVAILABLE: &str = "沒有 Wi-Fi 硬體";
/// Same wording `settings::network_page`'s own Wi-Fi/wired cards use for
/// this exact failure (`SettingsRpcError::is_not_appliance`) — a dev-Mac
/// running this shell outside the appliance image, not a real fault.
const TILE_NOT_APPLIANCE: &str = "非值班機";
/// Any other RPC failure (gateway unreachable, malformed reply, ...):
/// honestly says the read failed rather than keeping the OLD `fake_data`
/// "DuDu-Office" text on screen, which would be a claim nobody verified —
/// the task's own "錯誤誠實降級" instruction.
const TILE_UNREADABLE: &str = "無法讀取";
/// A `connected` link whose status payload carried no SSID at all — should
/// not happen in practice, but a blank subtitle would look like a bug
/// rather than a state.
const TILE_CONNECTED_FALLBACK: &str = "已連線";

/// Ephemeral state for the Wi-Fi quick tile. `Default` is `Load::NotLoaded`
/// via `Load<T>`'s own hand-written `Default` impl.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WifiTileState {
    load: Load<network_page::WifiSnapshot>,
}

impl WifiTileState {
    /// Pure: which colour the tile renders as (`active`) and what its
    /// second line says, given only what has been read so far. No gpui, no
    /// palette — the same "decision table testable without a window" shape
    /// `codrive_row::CodriveUiState::status` uses.
    pub(crate) fn tile_status(&self) -> (bool, String) {
        match &self.load {
            Load::NotLoaded | Load::Loading => (false, TILE_LOADING.to_string()),
            Load::Failed(e) if e.is_not_appliance() => (false, TILE_NOT_APPLIANCE.to_string()),
            Load::Failed(_) => (false, TILE_UNREADABLE.to_string()),
            Load::Loaded(snapshot) => match snapshot.link_state.as_str() {
                "connected" => (true, snapshot.link_ssid.clone().unwrap_or_else(|| TILE_CONNECTED_FALLBACK.to_string())),
                "connecting" => (false, TILE_CONNECTING.to_string()),
                "unavailable" => (false, TILE_UNAVAILABLE.to_string()),
                _ => (false, TILE_DISCONNECTED.to_string()),
            },
        }
    }

    /// Called from every overlay-close path (`main.rs`'s three, plus
    /// `chrome/windows.rs`'s own — the same four call sites
    /// `codrive_row::CodriveUiState::reset` documents), so the next
    /// ControlCenter open re-reads instead of showing a snapshot that may
    /// be stale — the link can drop or reconnect without anyone touching
    /// this panel.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Reads `network.status` once, if it has not been read yet. Safe to call
/// from a render body every pass — `Load::needs_load()` only arms on
/// `NotLoaded`, so a repaint mid-flight cannot stack a second RPC, and
/// `WifiTileState::reset` is what re-arms it for the next open.
pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.overlay_ui.wifi_tile.load.needs_load() {
        return;
    }
    view.overlay_ui.wifi_tile.load = Load::Loading;
    settings::spawn_rpc(
        cx,
        || {
            let status = settings::client::call(network_page::WIFI_STATUS_METHOD, serde_json::json!({}))?;
            // No scan half on purpose — see this file's header comment.
            Ok::<_, settings::client::SettingsRpcError>(network_page::parse_wifi(&status, &serde_json::json!({})))
        },
        |view, result, cx| {
            view.overlay_ui.wifi_tile.load = match result {
                Ok(snapshot) => Load::Loaded(snapshot),
                Err(e) => {
                    eprintln!("[overlay/wifi_tile] {} failed: {e:?}", network_page::WIFI_STATUS_METHOD);
                    Load::Failed(e)
                }
            };
            cx.notify();
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(link_state: &str, ssid: Option<&str>) -> WifiTileState {
        WifiTileState {
            load: Load::Loaded(network_page::WifiSnapshot {
                link_state: link_state.to_string(),
                link_ssid: ssid.map(str::to_string),
                internet: "unknown".to_string(),
                networks: Vec::new(),
            }),
        }
    }

    #[test]
    fn a_fresh_tile_says_it_is_reading_and_is_not_active() {
        let (active, subtitle) = WifiTileState::default().tile_status();
        assert!(!active);
        assert_eq!(subtitle, TILE_LOADING);
    }

    #[test]
    fn a_connected_link_shows_its_ssid_and_lights_up() {
        let (active, subtitle) = loaded("connected", Some("DuDu-Office")).tile_status();
        assert!(active);
        assert_eq!(subtitle, "DuDu-Office");
    }

    #[test]
    fn a_connected_link_with_no_ssid_falls_back_rather_than_going_blank() {
        let (active, subtitle) = loaded("connected", None).tile_status();
        assert!(active);
        assert_eq!(subtitle, TILE_CONNECTED_FALLBACK);
    }

    #[test]
    fn disconnected_and_unavailable_and_connecting_each_get_their_own_line() {
        assert_eq!(loaded("disconnected", None).tile_status(), (false, TILE_DISCONNECTED.to_string()));
        assert_eq!(loaded("unavailable", None).tile_status(), (false, TILE_UNAVAILABLE.to_string()));
        assert_eq!(loaded("connecting", Some("DuDu-Office")).tile_status(), (false, TILE_CONNECTING.to_string()));
    }

    /// A failure never keeps the old `fake_data` text on screen — both
    /// failure arms of `tile_status` return a plain `const` sentence rather
    /// than a passthrough of anything the RPC said, so there is no path
    /// back to a stale SSID.
    #[test]
    fn the_two_failure_sentences_are_distinct_and_non_empty() {
        assert_ne!(TILE_NOT_APPLIANCE, TILE_UNREADABLE);
        assert!(!TILE_NOT_APPLIANCE.is_empty());
        assert!(!TILE_UNREADABLE.is_empty());
    }

    #[test]
    fn resetting_forgets_everything_so_the_next_open_re_reads() {
        let mut state = loaded("connected", Some("DuDu-Office"));
        state.reset();
        assert_eq!(state, WifiTileState::default());
        assert!(state.load.needs_load());
    }

    #[test]
    fn the_tile_id_matches_a_real_entry_in_fake_data() {
        assert!(
            crate::fake_data::QUICK_TILES.iter().any(|t| t.id == WIFI_TILE_ID),
            "WIFI_TILE_ID must match fake_data::QUICK_TILES's own Wi-Fi entry, or the tile silently stops being wired"
        );
    }
}
