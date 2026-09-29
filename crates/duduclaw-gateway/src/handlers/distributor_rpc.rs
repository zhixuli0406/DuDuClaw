//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Distributor management (owner instance) ──────────────

    pub(crate) fn distributor_store(&self) -> DistributorStore {
        DistributorStore::new(&self.home_dir.join("distributor.db"))
    }

    /// Read `[distributor] issuer_key_path` from config.toml. `None` when
    /// unset/empty — issuance is then explicitly refused rather than guessing a
    /// path (design §3.3: "無預設值：未配置 = 明確錯誤").
    pub(crate) async fn distributor_issuer_key_path(&self) -> Option<String> {
        let config_path = self.home_dir.join("config.toml");
        let content = tokio::fs::read_to_string(&config_path).await.ok()?;
        let table = content.parse::<toml::Table>().ok()?;
        table
            .get("distributor")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("issuer_key_path"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    }

    /// Read `[distributor] public_url` from config.toml — the owner gateway's
    /// externally-reachable base URL, baked into issued keys as
    /// `license.control_url` (§10.5) so distributor instances phone home without
    /// `DUDUCLAW_CONTROL_URL`. Returns `None` when unset/empty or when the value
    /// is not an http(s) URL (fail-safe: a malformed URL is treated as unset
    /// rather than embedding a broken endpoint).
    pub(crate) async fn distributor_public_url(&self) -> Option<String> {
        let config_path = self.home_dir.join("config.toml");
        let content = tokio::fs::read_to_string(&config_path).await.ok()?;
        let table = content.parse::<toml::Table>().ok()?;
        let raw = table
            .get("distributor")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("public_url"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let lower = raw.to_ascii_lowercase();
        if (lower.starts_with("http://") || lower.starts_with("https://"))
            && !raw.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            Some(raw.to_string())
        } else {
            warn!("[distributor] public_url is not a valid http(s) URL — ignoring");
            None
        }
    }

    /// Append a distributor audit event (issue / revoke) to
    /// `security_audit.jsonl` via the shared, flock-protected writer. The
    /// private signing key is NEVER part of `details`.
    pub(crate) fn audit_distributor_event(&self, event_type: &str, details: Value) {
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                event_type,
                "system",
                duduclaw_security::audit::Severity::Info,
                details,
            ),
        );
    }

    pub(crate) async fn handle_distributor_status(&self) -> WsFrame {
        let issuer_configured = self.distributor_issuer_key_path().await.is_some();
        let stats = self.distributor_store().compute_stats();
        let stats_v = serde_json::to_value(stats).unwrap_or_else(|_| json!({}));
        WsFrame::ok_response(
            "",
            json!({
                "issuer_configured": issuer_configured,
                "issuer_key_id": "v2",
                // P2: the refresh + CRL control-plane endpoints self-gate on the
                // same issuer key, so they are live iff an issuer key is set.
                "refresh_endpoint_active": issuer_configured,
                "stats": stats_v,
            }),
        )
    }

    pub(crate) async fn handle_distributor_list(&self) -> WsFrame {
        let store = self.distributor_store();
        let mut out: Vec<Value> = Vec::new();
        for d in store.list_distributors() {
            let licenses = store.list_licenses(Some(&d.id));
            let mut dv = serde_json::to_value(&d).unwrap_or_else(|_| json!({}));
            if let Some(obj) = dv.as_object_mut() {
                obj.insert(
                    "licenses".into(),
                    serde_json::to_value(&licenses).unwrap_or_else(|_| json!([])),
                );
            }
            out.push(dv);
        }
        WsFrame::ok_response("", json!({ "distributors": out }))
    }

    pub(crate) async fn handle_distributor_add(&self, params: Value) -> WsFrame {
        let input: DistributorInput = match serde_json::from_value(params) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &format!("經銷商欄位無效：{e}")),
        };
        let store = self.distributor_store();
        match store.add_distributor(&input) {
            Ok(id) => {
                let profile = store
                    .get_distributor(&id)
                    .and_then(|p| serde_json::to_value(&p).ok())
                    .unwrap_or_else(|| json!({ "id": id }));
                WsFrame::ok_response("", json!({ "ok": true, "distributor": profile }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_distributor_update(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "缺少 'id' 參數"),
        };
        let patch_value = params.get("patch").cloned().unwrap_or_else(|| json!({}));
        let patch: DistributorPatch = match serde_json::from_value(patch_value) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &format!("patch 欄位無效：{e}")),
        };
        match self.distributor_store().update_distributor(&id, &patch) {
            Ok(()) => WsFrame::ok_response("", json!({ "ok": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_distributor_remove(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "缺少 'id' 參數"),
        };
        // The store refuses removal while active licenses remain (revoke first).
        match self.distributor_store().delete_distributor(&id) {
            Ok(()) => WsFrame::ok_response("", json!({ "ok": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Sign a fresh OEM (`tier=Oem`) white-label license for a distributor.
    ///
    /// Reuses the existing License v2 format so `duduclaw license activate`
    /// consumes it unchanged. The issuer private key path comes ONLY from
    /// `[distributor] issuer_key_path`; the signed blob is self-verified against
    /// the binary's baked v2 public key before it is booked, so a mismatched
    /// key pair fails loudly instead of shipping an unverifiable license.
    pub(crate) async fn handle_distributor_issue(&self, params: Value) -> WsFrame {
        let distributor_id = match params.get("distributor_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "缺少 'distributor_id' 參數"),
        };
        let machine_fingerprint = match params.get("machine_fingerprint").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return WsFrame::error_response(
                    "",
                    "缺少 'machine_fingerprint'（請經銷商執行 duduclaw license fingerprint 提供）",
                );
            }
        };
        // Default 365 days; clamp to a sane 1..=36500 window.
        let expires_days = params
            .get("expires_days")
            .and_then(|v| v.as_u64())
            .unwrap_or(365)
            .clamp(1, 36_500) as i64;

        // WP8: optional field-level white-label edit claim. Absent ⇒ `None` ⇒ the
        // consumer resolves to the full vendor set (a reseller token). Present ⇒
        // a narrowed range (a customer token). Fail-closed: every listed name
        // must be a real vendor-editable field — a claim can never grant a
        // system-only / unknown field, so a bad list rejects the whole issue.
        let branding_editable: Option<Vec<String>> = match params.get("branding_editable") {
            None | Some(Value::Null) => None,
            Some(Value::Array(arr)) => {
                let list: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                let invalid: Vec<&str> = list
                    .iter()
                    .map(String::as_str)
                    .filter(|f| {
                        crate::branding::field_level(f)
                            != crate::branding::BrandingFieldLevel::Vendor
                    })
                    .collect();
                if !invalid.is_empty() {
                    return WsFrame::error_response(
                        "",
                        &format!(
                            "branding_editable 含不可授權的欄位（僅允許品牌面欄位）：{}",
                            invalid.join("、")
                        ),
                    );
                }
                Some(list)
            }
            Some(_) => {
                return WsFrame::error_response("", "branding_editable 必須是欄位名稱的字串陣列");
            }
        };

        // P-License: optional signed per-license agent-count quota. Absent ⇒
        // `None` ⇒ no override (the Oem tier default applies). Present ⇒ sell
        // exactly N agents (`0` = unlimited enterprise bundle). Clamped to a sane
        // ceiling; it is part of the signed payload so the count cannot be raised
        // locally without invalidating the signature.
        let max_agents: Option<u32> = match params.get("max_agents") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(n) => Some(n.min(100_000) as u32),
                None => {
                    return WsFrame::error_response("", "max_agents 必須是非負整數（0 = 不限量）");
                }
            },
        };

        let store = self.distributor_store();
        let dist = match store.get_distributor(&distributor_id) {
            Some(d) => d,
            None => return WsFrame::error_response("", "找不到該經銷商"),
        };

        // Issuer key path — explicit config, no path guessing.
        let key_path = match self.distributor_issuer_key_path().await {
            Some(p) => p,
            None => {
                return WsFrame::error_response(
                    "",
                    "尚未配置簽發金鑰：請在 config.toml 設定 [distributor] issuer_key_path 指向 license-signing-v2.key",
                );
            }
        };
        let seed = match crate::distributor_store::load_issuer_signing_seed(Path::new(&key_path)) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Identifiers embed the distributor id for traceability.
        let license_id = uuid::Uuid::new_v4().to_string();
        let short: String = license_id.chars().take(8).collect();
        let subscription_id = format!("dist-{distributor_id}-{short}");
        let customer_id = format!("dist-{distributor_id}");

        // §10.5: if the owner has declared its externally-reachable URL, bake it
        // into the key so the distributor instance auto-refreshes without env.
        let public_url = self.distributor_public_url().await;

        let registry = crate::license_runtime::production_registry();
        let (license, blob) = match crate::distributor_store::issue_signed_oem_license(
            &seed,
            &registry,
            "v2",
            &subscription_id,
            &customer_id,
            &machine_fingerprint,
            expires_days,
            public_url.as_deref(),
            branding_editable,
            max_agents,
        ) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let record = IssuedLicense {
            id: license_id.clone(),
            distributor_id: distributor_id.clone(),
            subscription_id: subscription_id.clone(),
            customer_id,
            tier: "oem".to_string(),
            machine_fingerprint: machine_fingerprint.clone(),
            issued_at: license.issued_at.to_rfc3339(),
            expires_at: license.expires_at.to_rfc3339(),
            status: "active".to_string(),
            revoked_at: None,
            license_blob: blob.clone(),
            last_refresh_at: None,
        };
        if let Err(e) = store.add_license(&record) {
            return WsFrame::error_response("", &format!("寫入授權紀錄失敗：{e}"));
        }

        // Audit — the private key never appears; fingerprint is truncated
        // (CJK-safe) for a compact forensic line.
        self.audit_distributor_event(
            "distributor_license_issued",
            json!({
                "license_id": license_id,
                "distributor_id": distributor_id,
                "distributor_name": dist.name,
                "subscription_id": subscription_id,
                "tier": "oem",
                "machine_fingerprint": duduclaw_core::truncate_chars(&machine_fingerprint, 16),
                "expires_at": record.expires_at,
            }),
        );

        let record_v = serde_json::to_value(&record).unwrap_or_else(|_| json!({}));
        // §10.5: when public_url is configured, the key carries its own
        // control-plane address (no env needed). Otherwise fall back to the §9.3
        // guidance to set DUDUCLAW_CONTROL_URL.
        let warnings = match &public_url {
            Some(url) => vec![format!(
                "金鑰已內建續期端點（{url}）：客戶 instance 無需設定 DUDUCLAW_CONTROL_URL，即會自動 phone-home 續期並接收撤銷（CRL）"
            )],
            None => vec![
                "請經銷商在其 DuDuClaw instance 設定環境變數 DUDUCLAW_CONTROL_URL 指向本 owner gateway（例如 https://your-gateway.example.com），授權即會自動 phone-home 續期並接收撤銷（CRL）；未設定時仍會在 60 天無 phone-home 後降級為 OpenSource。或在 config.toml 設定 [distributor] public_url 讓後續簽發的金鑰自帶續期端點".to_string(),
            ],
        };
        WsFrame::ok_response(
            "",
            json!({
                "ok": true,
                "license_blob": blob,
                "record": record_v,
                "warnings": warnings,
            }),
        )
    }

    pub(crate) async fn handle_distributor_revoke(&self, params: Value) -> WsFrame {
        let license_id = match params.get("license_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "缺少 'license_id' 參數"),
        };
        let store = self.distributor_store();
        let rec = store.get_license(&license_id);
        match store.revoke_license(&license_id) {
            Ok(()) => {
                self.audit_distributor_event(
                    "distributor_license_revoked",
                    json!({
                        "license_id": license_id,
                        "distributor_id": rec.as_ref().map(|r| r.distributor_id.clone()),
                        "subscription_id": rec.as_ref().map(|r| r.subscription_id.clone()),
                    }),
                );
                WsFrame::ok_response(
                    "",
                    json!({
                        "ok": true,
                        "crl_note": "本地已標記撤銷；遠端撤銷需發布 CRL（見 commercial/LICENSE-OPERATIONS.md）",
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Local (offline-backup) white-label upgrade: re-sign an already-issued OEM
    /// license with a new `max_agents` (P-License add-on, plan §12.1).
    ///
    /// The issuer key re-signs the SAME key — preserving subscription / customer
    /// / fingerprint / issued_at / expires_at / control_url and only swapping the
    /// signed agent-count quota — then `issued_licenses.license_blob` is replaced.
    /// The `refresh` endpoint re-signs off the previous blob, so continued
    /// phone-home renewals naturally carry the new count; the returned blob can
    /// also be delivered to the customer directly for immediate effect. A revoked
    /// license is refused (fail-closed).
    pub(crate) async fn handle_distributor_upgrade(&self, params: Value) -> WsFrame {
        let license_id = match params.get("license_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "缺少 'license_id' 參數"),
        };
        // Required signed agent-count quota. `0` = unlimited. Clamped to a sane
        // ceiling; part of the signed payload so it cannot be raised locally.
        let max_agents: u32 = match params.get("max_agents") {
            Some(v) => match v.as_u64() {
                Some(n) => n.min(100_000) as u32,
                None => {
                    return WsFrame::error_response("", "max_agents 必須是非負整數（0 = 不限量）");
                }
            },
            None => return WsFrame::error_response("", "缺少 'max_agents' 參數"),
        };

        let store = self.distributor_store();
        let rec = match store.get_license(&license_id) {
            Some(r) => r,
            None => return WsFrame::error_response("", "找不到該授權"),
        };
        if rec.status == "revoked" {
            return WsFrame::error_response("", "已撤銷的授權無法升級（請重新簽發）");
        }

        // Prior signed quota (for the audit old→new line), decoded from the blob.
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        let old_max_agents = B64
            .decode(rec.license_blob.trim())
            .ok()
            .and_then(|b| serde_json::from_slice::<duduclaw_license::License>(&b).ok())
            .and_then(|l| l.max_agents);

        // Issuer key — explicit config, no path guessing.
        let key_path = match self.distributor_issuer_key_path().await {
            Some(p) => p,
            None => {
                return WsFrame::error_response(
                    "",
                    "尚未配置簽發金鑰：請在 config.toml 設定 [distributor] issuer_key_path 指向 license-signing-v2.key",
                );
            }
        };
        let seed = match crate::distributor_store::load_issuer_signing_seed(Path::new(&key_path)) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let registry = crate::license_runtime::production_registry();
        let (_license, blob) = match crate::distributor_store::resign_license_with_max_agents(
            &seed,
            &registry,
            &rec,
            Some(max_agents),
        ) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };

        if let Err(e) = store.update_license_blob(&license_id, &blob) {
            return WsFrame::error_response("", &format!("更新授權失敗：{e}"));
        }

        self.audit_distributor_event(
            "distributor_license_upgraded",
            json!({
                "license_id": license_id,
                "distributor_id": rec.distributor_id,
                "subscription_id": rec.subscription_id,
                "old_max_agents": old_max_agents,
                "new_max_agents": max_agents,
            }),
        );

        WsFrame::ok_response(
            "",
            json!({
                "ok": true,
                "license_blob": blob,
                "old_max_agents": old_max_agents,
                "new_max_agents": max_agents,
                "note": "已更新本地授權；可將新 license_blob 交付客戶立即生效，或由客戶端下次 phone-home 續期自動帶入",
            }),
        )
    }

    /// Owner-side offline co-signing of a branding bundle (§10.3): the operator
    /// pastes a distributor's branding JSON and the owner signs it directly with
    /// the local issuer key — covering the case where the distributor instance
    /// cannot reach `/v1/branding/sign`. Admin-gated; the owner is the
    /// authoritative sanitizer (branding is run through `validate_input`).
    pub(crate) async fn handle_distributor_bundle_sign(&self, params: Value) -> WsFrame {
        let distributor_id = match params.get("distributor_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "缺少 'distributor_id' 參數"),
        };
        let store = self.distributor_store();
        if store.get_distributor(&distributor_id).is_none() {
            return WsFrame::error_response("", "找不到該經銷商");
        }

        // Parse + authoritatively sanitize the submitted branding.
        let submitted: crate::branding::BrandingConfig = match params.get("branding") {
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(c) => c,
                Err(e) => return WsFrame::error_response("", &format!("branding 欄位無效：{e}")),
            },
            None => crate::branding::BrandingConfig::default(),
        };
        let sanitized = match crate::branding::validate_input(submitted.into()) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Traceability subscription id: explicit param, else derived from the id.
        let subscription_id = params
            .get("subscription_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("dist-{distributor_id}"));

        let key_path = match self.distributor_issuer_key_path().await {
            Some(p) => p,
            None => {
                return WsFrame::error_response(
                    "",
                    "尚未配置簽發金鑰：請在 config.toml 設定 [distributor] issuer_key_path 指向 license-signing-v2.key",
                );
            }
        };
        let seed = match crate::distributor_store::load_issuer_signing_seed(Path::new(&key_path)) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let issued_at = chrono::Utc::now().to_rfc3339();
        let bundle = match crate::branding::sign_bundle(
            &seed,
            &distributor_id,
            &subscription_id,
            &sanitized,
            &issued_at,
            crate::branding::BUNDLE_KEY_ID,
        ) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &e),
        };

        self.audit_distributor_event(
            "branding_bundle_signed",
            json!({
                "distributor_id": distributor_id,
                "subscription_id": subscription_id,
                "mode": "owner_offline",
            }),
        );

        match serde_json::to_value(&bundle) {
            Ok(v) => WsFrame::ok_response("", json!({ "ok": true, "bundle": v })),
            Err(e) => WsFrame::error_response("", &format!("serialize bundle: {e}")),
        }
    }
}
