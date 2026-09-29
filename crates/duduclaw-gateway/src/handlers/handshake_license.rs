//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── OpenClaw handshake ───────────────────────────────────

    pub(crate) fn handle_connect_challenge(&self, _params: Value) -> WsFrame {
        let challenge = uuid::Uuid::new_v4().to_string();
        WsFrame::ok_response("", json!({ "challenge": challenge }))
    }

    pub(crate) fn handle_connect(&self, params: Value) -> WsFrame {
        let version = params
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        WsFrame::ok_response(
            "",
            json!({ "version": crate::updater::current_version(), "client_version": version, "status": "connected" }),
        )
    }

    pub(crate) fn handle_hello_ok(&self, _params: Value) -> WsFrame {
        WsFrame::ok_response("", json!({ "ack": true }))
    }

    /// `license.status` — read-only snapshot of the current LicenseRuntime.
    ///
    /// Returns OpenSource defaults when no runtime is registered (e.g. a
    /// gateway started without `start_gateway`) or when no license file is
    /// installed. Never errors — license queries must not break the
    /// dashboard. Dashboard surface fields are stable across schema bumps
    /// because we project through [`crate::license_runtime::LicenseSnapshot`]
    /// rather than serializing the raw [`duduclaw_license::License`] (which
    /// contains the Ed25519 signature).
    /// `license.fingerprint` — this machine's fingerprint, shown in the
    /// dashboard upgrade card so the operator can have a license issued.
    ///
    /// `fingerprint` is deliberately the **strong** value — identical to what
    /// `duduclaw license fingerprint` prints — because this RPC exists to get a
    /// *new* license issued, and issuing against a legacy placeholder-MAC value
    /// would perpetuate the weak binding this card is meant to fix. `effective`
    /// reports what the currently-installed license is actually bound to, and
    /// `legacy_binding` flags the "still accepted, please re-issue" state.
    pub(crate) async fn handle_license_fingerprint(&self) -> WsFrame {
        let strong = crate::license_runtime::strong_fingerprint();
        let effective = crate::license_runtime::cached_fingerprint();
        WsFrame::ok_response(
            "",
            json!({
                "fingerprint": strong,
                "effective": effective,
                "legacy_binding": effective != strong,
            }),
        )
    }

    /// `license.activate {key}` — install a purchased license from the
    /// dashboard (base64 blob or raw JSON; filesystem paths deliberately not
    /// accepted from a browser form). Signature/fingerprint/expiry verified
    /// fail-closed, then the license runtime hot-reloads — no restart needed.
    pub(crate) async fn handle_license_activate(&self, params: Value) -> WsFrame {
        let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
        if key.chars().count() > 64_000 {
            return WsFrame::error_response("", "授權金鑰內容過長");
        }
        let Some(runtime) = crate::license_runtime::global() else {
            return WsFrame::error_response("", "授權服務尚未就緒，請稍後再試");
        };
        let license = match crate::license_runtime::parse_license_key(key) {
            Ok(l) => l,
            Err(e) => return WsFrame::error_response("", &e),
        };
        match runtime.install_and_reload(license).await {
            Ok(snapshot) => {
                info!(tier = %snapshot.tier, "license activated via dashboard");
                // Edition may have flipped (e.g. → enterprise): push a fresh
                // system.status so open dashboards update without a refresh.
                self.broadcast_system_status().await;
                WsFrame::ok_response("", json!({ "success": true, "status": snapshot }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `license.redeem {code, email?}` — partner (NFR) code redemption from
    /// the dashboard; control-plane issues a free license bound to this
    /// machine, installed with the same fail-closed verification.
    pub(crate) async fn handle_license_redeem(&self, params: Value) -> WsFrame {
        let code = params.get("code").and_then(|v| v.as_str()).unwrap_or("");
        let email = params
            .get("email")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(runtime) = crate::license_runtime::global() else {
            return WsFrame::error_response("", "授權服務尚未就緒，請稍後再試");
        };
        match crate::license_runtime::redeem_partner_code(runtime, code, email).await {
            Ok(snapshot) => {
                info!(tier = %snapshot.tier, "partner code redeemed via dashboard");
                self.broadcast_system_status().await;
                WsFrame::ok_response("", json!({ "success": true, "status": snapshot }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_license_status(&self) -> WsFrame {
        let snapshot = match crate::license_runtime::global() {
            Some(runtime) => runtime.snapshot().await,
            None => crate::license_runtime::LicenseSnapshot {
                tier: duduclaw_license::LicenseTier::OpenSource,
                mode: "opensource",
                installed: false,
                customer_id: None,
                subscription_id: None,
                expires_at: None,
                days_until_expiry: None,
                last_phone_home: None,
                days_since_phone_home: None,
                fingerprint_match: None,
                branding_editable: None,
                max_agents: None,
                nfr: false,
            },
        };

        let payload = match serde_json::to_value(&snapshot) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response("", &format!("serialize license snapshot: {e}"));
            }
        };

        WsFrame::ok_response("", payload)
    }
}
