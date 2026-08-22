// WP-C-M2 — gateway switching: startup target resolution + the two
// `screens::gateway_picker` actions ("切換到本機" / "連線" on the manual
// entry). Split out of `main.rs` purely to keep that file under this
// project's 800-line convention — `RootView` itself, `Screen`, and every
// other method on it still live in `main.rs`; this is a second inherent
// `impl RootView` block, which Rust allows across files within the same
// crate. No call-site changes were needed anywhere: `screens::
// gateway_picker`'s `this.switch_to_local(cx)` / `this.begin_manual_
// connect(cx)` and `main()`'s `resolve_startup_gateway()` call both keep
// resolving exactly as they did when these lived in `main.rs` directly.

use std::sync::Arc;

use gpui::Context;

use crate::ws_status::{Command as SessionCommand, WsConnState};
use crate::{api, config, sidecar, sidecar_target, RootView, Screen};

/// WP-C-M2 — resolve which gateway THIS launch should connect to, and start
/// managing a local sidecar when applicable. Priority, per the task brief:
///
/// 1. `DUDUCLAW_GATEWAY_URL` env override (dev/test escape hatch — wins
///    over everything, including a persisted remote selection).
/// 2. A persisted `GatewayMode::Remote` selection (`screens::
///    gateway_picker`'s last successful manual connect).
/// 3. Local (default): attach to an already-running gateway on the
///    configured port, else spawn `duduclaw run` as a managed sidecar.
///
/// Always returns a constructed `SidecarManager` even when the resolved
/// target is remote/env-overridden (see that field's own doc comment on
/// `RootView` for why) — it is simply never `.start()`ed in those two
/// branches.
pub fn resolve_startup_gateway() -> (String, Arc<sidecar::SidecarManager>) {
    let manager = sidecar::SidecarManager::new();

    if let Ok(env_url) = std::env::var("DUDUCLAW_GATEWAY_URL") {
        let env_url = env_url.trim();
        if !env_url.is_empty() {
            eprintln!("[main] DUDUCLAW_GATEWAY_URL override -> {env_url}");
            return (env_url.to_string(), manager);
        }
    }

    if let Some(config::GatewaySelection { mode: config::GatewayMode::Remote, remote_url: Some(url) }) =
        config::load_gateway_selection()
    {
        if !url.trim().is_empty() {
            eprintln!("[main] persisted remote gateway -> {url}");
            return (url, manager);
        }
    }

    let preferred_port = sidecar_target::configured_port();
    match manager.start(preferred_port) {
        Ok(port) => {
            eprintln!("[main] local gateway target: port {port} (status={:?})", manager.status());
            (format!("http://{}:{port}", sidecar_target::DEFAULT_HOST), manager)
        }
        Err(e) => {
            eprintln!(
                "[main] local sidecar failed to start: {e} — falling back to the default URL \
                 (will show 無法連線 until a gateway is reachable there; use the Gateway picker \
                 to point at a different one)"
            );
            (format!("http://{}:{preferred_port}", sidecar_target::DEFAULT_HOST), manager)
        }
    }
}

impl RootView {
    /// WP-C-M2 — commit a gateway switch that has ALREADY been validated
    /// (manual entry: health-checked; local: a fresh `sidecar.start()` was
    /// just issued). Shared by both `screens::gateway_picker` actions:
    /// persist the selection, retarget every future call this crate makes
    /// (`api::set_gateway_base_url`), tear down whatever session state
    /// belonged to the OLD gateway (a JWT/WS session from one gateway is
    /// meaningless on another), and re-run the exact boot-time probe
    /// (`TryLocalSession`) against the new target — the same flow `main()`
    /// uses on cold start, so "switch gateway" and "launch pointed at
    /// gateway X" are byte-identical from this point on.
    pub(crate) fn apply_gateway_switch(&mut self, mode: config::GatewayMode, url: String) {
        if mode != config::GatewayMode::Local {
            // Leaving local — release a sidecar THIS app spawned so a
            // remote pick never leaves an unwanted gateway process running
            // in the background (task brief: "非自己 spawn 的不動" implies
            // the inverse too — one we DID spawn must be released when no
            // longer wanted). `stop()` is already a safe no-op for an
            // attached (not-spawned-by-us) gateway or one never started.
            self.sidecar.stop();
        }
        api::set_gateway_base_url(url.clone());
        config::save_gateway_selection(&config::GatewaySelection {
            mode,
            remote_url: if mode == config::GatewayMode::Local { None } else { Some(url) },
        });

        self.jwt = None;
        self.refresh_token = None;
        self.user_id = None;
        self.display_name = None;
        self.ws_state = WsConnState::Disconnected;
        self.chat.disconnect();
        self.screen = Screen::Login;
        self.login_error = None;
        self.gateway_connect_error = None;
        let _ = self.session_tx.send(SessionCommand::TryLocalSession);
    }

    /// "切換到本機" — (re)start the local sidecar and switch to it
    /// immediately, WITHOUT a health-check gate: `sidecar.start()` may need
    /// up to 45s to actually answer on a cold spawn (`sidecar::
    /// READY_TIMEOUT`), and gating this button on that would make it look
    /// hung. This mirrors cold-boot startup itself (`resolve_startup_
    /// gateway` doesn't health-gate either) — the picker's "本機" card
    /// already shows live `Starting`/`Running`/`Error` via `sidecar.
    /// status()`, and the subsequent `TryLocalSession`/WS reconnect loop
    /// already tolerates a not-yet-ready gateway with its own backoff.
    pub(crate) fn switch_to_local(&mut self, cx: &mut Context<Self>) {
        let port = sidecar_target::configured_port();
        if let Err(e) = self.sidecar.start(port) {
            self.gateway_connect_error = Some(format!("無法啟動本機 gateway：{e}").into());
            cx.notify();
            return;
        }
        let url = format!("http://{}:{}", sidecar_target::DEFAULT_HOST, self.sidecar.port());
        self.apply_gateway_switch(config::GatewayMode::Local, url);
        cx.notify();
    }

    /// "連線" on the manual-entry field — validates the typed URL, then
    /// health-checks it (off gpui's own executor, via `ws_status::
    /// health_check` on the background tokio runtime — see that fn's doc
    /// comment) BEFORE touching any state, so a typo or an unreachable host
    /// leaves the currently-connected gateway completely untouched.
    pub(crate) fn begin_manual_connect(&mut self, cx: &mut Context<Self>) {
        if self.gateway_connecting {
            return;
        }
        let raw = self.gateway_manual_field.read(cx).content.clone();
        let candidate = match api::validate_gateway_url(&raw) {
            Ok(url) => url,
            Err(e) => {
                self.gateway_connect_error = Some(format!("網址格式錯誤：{e}").into());
                cx.notify();
                return;
            }
        };

        self.gateway_connecting = true;
        self.gateway_connect_error = None;
        cx.notify();

        let tx = self.session_tx.clone();
        cx.spawn(async move |weak, cx| {
            let rx = crate::ws_status::health_check(&tx, candidate.clone());
            let (ok, detail) = rx.await.unwrap_or((false, Some("背景連線執行緒已結束".to_string())));
            let _ = weak.update(cx, |view, cx| {
                view.gateway_connecting = false;
                if ok {
                    let mode = if api::is_local_gateway_url(&candidate) {
                        config::GatewayMode::Local
                    } else {
                        config::GatewayMode::Remote
                    };
                    view.apply_gateway_switch(mode, candidate);
                } else {
                    view.gateway_connect_error =
                        Some(format!("無法連線：{}", detail.unwrap_or_else(|| "未知錯誤".to_string())).into());
                }
                cx.notify();
            });
        })
        .detach();
    }
}
