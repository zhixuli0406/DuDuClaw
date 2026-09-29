//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── White-label branding + About ─────────────────────────
    //
    // The upstream vendor attribution ("嘟嘟數位科技有限公司") is const-assembled
    // by [`crate::branding::VendorBlock::upstream`] into every response — it is
    // never read from config and never writable, so a reseller can rebrand the
    // product surface while the "software by" credit stays intact.

    /// white_label feature gate — **fail-closed**. `true` only when a license
    /// runtime is registered AND its active tier unlocks `white_label` (tier =
    /// Oem). No runtime / OpenSource / any error ⇒ `false` ⇒ writes denied.
    pub(crate) async fn white_label_active(&self) -> bool {
        match crate::license_runtime::global() {
            Some(rt) => rt.check_feature("white_label").await,
            None => false,
        }
    }

    /// WP8: whether THIS gateway is the upstream/owner ("system") instance — the
    /// one that holds a **genuine, trusted** distributor issuer signing key. Such
    /// an instance has full [`BrandingEditScope::System`] rights (every field,
    /// including system-only ones).
    ///
    /// Non-forgeable: the configured `[distributor] issuer_key_path` must load a
    /// seed that pairs with a baked/trusted issuer public key. A customer-admin
    /// pointing this at a random 32-byte file does NOT get System scope (the
    /// derived pubkey is untrusted) — it falls through to their license claim.
    pub(crate) async fn is_system_branding_instance(&self) -> bool {
        let key_path = match self.distributor_issuer_key_path().await {
            Some(p) => p,
            None => return false,
        };
        let seed = match crate::distributor_store::load_issuer_signing_seed(Path::new(&key_path)) {
            Ok(s) => s,
            Err(_) => return false,
        };
        crate::distributor_store::issuer_seed_is_trusted(
            &seed,
            &crate::license_runtime::production_registry(),
        )
    }

    /// WP8: resolve the branding edit scope for THIS instance from the
    /// system-instance signal, the white_label feature gate, and the active
    /// license's signed `branding_editable` claim. `None` ⇒ editing not
    /// permitted at all (fail-closed).
    pub(crate) async fn resolve_branding_edit_scope(&self) -> Option<crate::branding::BrandingEditScope> {
        let is_system = self.is_system_branding_instance().await;
        let white_label = self.white_label_active().await;
        let claim = match crate::license_runtime::global() {
            Some(rt) => rt.snapshot().await.branding_editable,
            None => None,
        };
        crate::branding::resolve_edit_scope(is_system, white_label, claim.as_deref())
    }

    pub(crate) async fn handle_branding_get(&self) -> WsFrame {
        let (branding, source) = crate::branding::load_with_source(&self.home_dir);
        let vendor = crate::branding::VendorBlock::upstream();
        let white_label_active = self.white_label_active().await;
        // WP8: the field names the current instance may edit, so the dashboard
        // can mask the branding form. Empty when editing is not permitted.
        let editable_fields = self
            .resolve_branding_edit_scope()
            .await
            .map(|s| s.editable_fields())
            .unwrap_or_default();
        let branding_v = serde_json::to_value(&branding).unwrap_or_else(|_| json!({}));
        let vendor_v = serde_json::to_value(vendor).unwrap_or_else(|_| json!({}));
        WsFrame::ok_response(
            "",
            json!({
                "branding": branding_v,
                "vendor": vendor_v,
                "source": source,
                "defaults": {
                    "product_name": crate::branding::DEFAULT_PRODUCT_NAME,
                    "subtitle_key": "app.subtitle",
                },
                "white_label_active": white_label_active,
                "editable_fields": editable_fields,
            }),
        )
    }

    pub(crate) async fn handle_about_get(&self) -> WsFrame {
        let (branding, source) = crate::branding::load_with_source(&self.home_dir);
        let vendor = crate::branding::VendorBlock::upstream();
        let tier = match crate::license_runtime::global() {
            Some(rt) => rt.snapshot().await.tier.to_string(),
            None => duduclaw_license::LicenseTier::OpenSource.to_string(),
        };
        let white_label_active = self.white_label_active().await;
        let branding_v = serde_json::to_value(&branding).unwrap_or_else(|_| json!({}));
        let vendor_v = serde_json::to_value(vendor).unwrap_or_else(|_| json!({}));
        WsFrame::ok_response(
            "",
            json!({
                "vendor": vendor_v,
                "branding": branding_v,
                "source": source,
                "version": env!("CARGO_PKG_VERSION"),
                "tier": tier,
                "white_label_active": white_label_active,
            }),
        )
    }

    pub(crate) async fn handle_branding_set(&self, params: Value) -> WsFrame {
        // WP8: resolve the edit scope (system instance / white_label + license
        // claim). `None` ⇒ editing not permitted — fail-closed DENY before disk.
        let scope = match self.resolve_branding_edit_scope().await {
            Some(s) => s,
            None => {
                return WsFrame::error_response(
                    "",
                    "白牌功能需經銷商授權（未取得 white_label 授權）",
                );
            }
        };
        let input: crate::branding::BrandingInput = match serde_json::from_value(params) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &format!("品牌設定欄位無效：{e}")),
        };
        // WP8: reject (do NOT silently drop) any field the scope cannot edit.
        let violations = crate::branding::disallowed_fields(&scope, &input);
        if !violations.is_empty() {
            return WsFrame::error_response(
                "",
                &format!("此授權層級不可編輯下列品牌欄位：{}", violations.join("、")),
            );
        }
        let cfg = match crate::branding::validate_input(input) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        // WP8: a partial-scope writer (customer granted a subset) must not wipe
        // the reseller's other fields by omitting them — restore them from disk.
        let cfg = crate::branding::preserve_unscoped(&self.home_dir, &scope, cfg);
        if let Err(e) = crate::branding::save(&self.home_dir, &cfg) {
            return WsFrame::error_response("", &e);
        }
        match serde_json::to_value(&cfg) {
            Ok(v) => WsFrame::ok_response("", json!({ "ok": true, "branding": v })),
            Err(e) => WsFrame::error_response("", &format!("serialize branding: {e}")),
        }
    }

    pub(crate) async fn handle_branding_reset(&self) -> WsFrame {
        // WP8: scope-aware reset — a full-scope caller reverts the whole brand;
        // a partial-scope (customer) caller clears only its granted fields.
        let scope = match self.resolve_branding_edit_scope().await {
            Some(s) => s,
            None => {
                return WsFrame::error_response(
                    "",
                    "白牌功能需經銷商授權（未取得 white_label 授權）",
                );
            }
        };
        match crate::branding::reset_scoped(&self.home_dir, &scope) {
            Ok(()) => WsFrame::ok_response("", json!({ "ok": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Sanitize a raw about_html block and echo the exact sanitized string the
    /// server would persist (§10.2 — "preview is what you get"). white_label
    /// gated + fail-closed, same as `branding.set`.
    pub(crate) async fn handle_branding_preview(&self, params: Value) -> WsFrame {
        if !self.white_label_active().await {
            return WsFrame::error_response("", "白牌功能需經銷商授權（未取得 white_label 授權）");
        }
        let raw = params
            .get("about_html")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match crate::branding::sanitize_about_html(raw) {
            Ok(html) => WsFrame::ok_response(
                "",
                json!({ "ok": true, "sanitized_html": html.unwrap_or_default() }),
            ),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Produce a signed branding bundle for this distributor instance (§10.3):
    /// take the local subscription_id + machine fingerprint from the license
    /// runtime snapshot, POST the current branding to the owner gateway's
    /// `/v1/branding/sign`, and hand the signed bundle back for download.
    pub(crate) async fn handle_branding_bundle_create(&self) -> WsFrame {
        if !self.white_label_active().await {
            return WsFrame::error_response("", "白牌功能需經銷商授權（未取得 white_label 授權）");
        }
        let runtime = match crate::license_runtime::global() {
            Some(rt) => rt,
            None => {
                return WsFrame::error_response(
                    "",
                    "尚未載入授權資訊，無法產生散發包（請確認已啟用有效授權）",
                );
            }
        };
        let snapshot = runtime.snapshot().await;
        let subscription_id = match snapshot.subscription_id {
            Some(s) if !s.is_empty() => s,
            _ => {
                return WsFrame::error_response(
                    "",
                    "找不到訂閱識別碼（subscription_id）；散發包需要有效的經銷商授權",
                );
            }
        };
        // The *effective* fingerprint — the one the installed license is bound
        // to — so a still-accepted legacy binding keeps matching the
        // control-plane row (v1.66.1).
        let machine_fingerprint = crate::license_runtime::cached_fingerprint();
        let (branding, _source) = crate::branding::load_with_source(&self.home_dir);
        let branding_v = serde_json::to_value(&branding).unwrap_or_else(|_| json!({}));

        // Resolve the owner endpoint exactly like phone-home (env > license >
        // default), then call server-to-server.
        let base = runtime.control_url().trim_end_matches('/').to_string();
        let endpoint = format!("{base}/v1/branding/sign");
        let request_body = json!({
            "subscription_id": subscription_id,
            "machine_fingerprint": machine_fingerprint,
            "branding": branding_v,
        });

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("建立 HTTP 用戶端失敗：{e}")),
        };
        let response = match client.post(&endpoint).json(&request_body).send().await {
            Ok(r) => r,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "無法連線簽發端點（{endpoint}）：{e}。請確認 DUDUCLAW_CONTROL_URL 或金鑰內建的續期端點可連線，或改用 owner 端「代簽散發包」"
                    ),
                );
            }
        };
        let status = response.status();
        let body: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &format!("簽發端點回應解析失敗：{e}")),
        };
        if !status.is_success() {
            let reason = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return WsFrame::error_response(
                "",
                &format!("簽發端點拒絕（HTTP {status}）：{reason}"),
            );
        }
        WsFrame::ok_response("", json!({ "ok": true, "bundle": body }))
    }
}
