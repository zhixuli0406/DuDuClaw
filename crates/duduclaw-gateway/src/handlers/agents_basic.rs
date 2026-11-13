//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Agents ───────────────────────────────────────────────

    pub(crate) async fn handle_agents_status(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(a) => {
                let cfg = &a.config;
                WsFrame::ok_response(
                    "",
                    json!({
                        "name": cfg.agent.name,
                        "display_name": cfg.agent.display_name,
                        "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                        "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                    }),
                )
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    /// WP-6F P1 — `presets.list`: every preset under `~/.duduclaw/presets/`,
    /// read-only (design §9 P1 scope excludes any switching UI). A preset
    /// that fails to parse is still listed (with an `error` field) rather
    /// than silently dropped, matching `duduclaw preset list`'s CLI twin.
    pub(crate) async fn handle_presets_list(&self) -> WsFrame {
        let ids = duduclaw_core::preset::list_presets(&self.home_dir);
        let items: Vec<Value> = ids
            .into_iter()
            .map(
                |id| match duduclaw_core::preset::load_preset(&self.home_dir, &id) {
                    Ok(p) => json!({
                        "id": id,
                        "version": p.meta.version,
                        "label": p.meta.label,
                        "description": p.meta.description,
                    }),
                    Err(e) => json!({ "id": id, "error": e.to_string() }),
                },
            )
            .collect();
        WsFrame::ok_response("", json!({ "presets": items }))
    }

    /// WP-6F P1 — `presets.status`: one agent's current preset binding +
    /// live resolution outcome + which fields its own `agent.toml` overrides
    /// (design §1 R1.4 "已覆寫"). Read-only — binding writes are CLI-only in
    /// P1 (`duduclaw preset bind`), see `preset_cmd` module docs.
    pub(crate) async fn handle_presets_status(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let reg = self.registry.read().await;
        let Some(agent) = reg.get(agent_id) else {
            return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
        };
        let resolution = match &agent.preset_resolution {
            duduclaw_core::preset::PresetResolution::Unbound => json!({ "state": "unbound" }),
            duduclaw_core::preset::PresetResolution::Applied {
                preset_id,
                version,
                label,
                changed_fields,
                ..
            } => json!({
                "state": "applied",
                "preset_id": preset_id,
                "version": version,
                "label": label,
                "changed_fields": changed_fields,
            }),
            duduclaw_core::preset::PresetResolution::Unresolved {
                preset_id,
                version,
                reason,
            } => json!({
                "state": "unresolved",
                "preset_id": preset_id,
                "version": version,
                "reason": reason,
            }),
        };
        WsFrame::ok_response(
            "",
            json!({ "agent_id": agent_id, "resolution": resolution }),
        )
    }

    /// Cloud-tier resource cap. Returns `Some(message)` when the active tier
    /// caps this resource, the deployment is Cloud (`DUDUCLAW_DEPLOYMENT=cloud`,
    /// injected into managed tenant containers), and the cap is already
    /// reached. Self-hosted deployments (Apache 2.0) are NEVER capped; a
    /// `max` of 0 in features.toml means unlimited.
    ///
    /// The agent cap does not go through here: every create / restore path
    /// calls [`Self::admit_agent_seats`] (RM-149), which counts the seat
    /// holders on disk, under the seat admission lock, instead of taking a
    /// caller's own count.
    pub(crate) async fn tier_limit_message(&self, kind: &str, current: usize) -> Option<String> {
        if kind == "agent" {
            let cap = self.effective_agent_cap().await;
            if !crate::license_runtime::cap_exceeded(cap, current) {
                return None;
            }
            return Some(self.agent_cap_text(cap).await);
        }

        let rt = crate::license_runtime::global()?;
        // Apache 2.0 promise: never limit self-host. Channels keep the pure
        // self-host exemption (only the agent cap has a signed override).
        if crate::license_runtime::is_self_host_deployment() {
            return None;
        }
        let tier = rt.current_tier().await;
        let max = match kind {
            "channel" => rt.feature_gate().max_channels(tier),
            _ => 0,
        };
        if !crate::license_runtime::cap_exceeded(max, current) {
            return None;
        }
        Some(format!(
            "您的方案（{tier}）最多可建立 {max} 個通道。\
             如需調整授權範圍，請聯絡總經銷：https://futurecorp.tw"
        ))
    }

    /// Test seam: pin the agent cap (`None` = resolve normally).
    #[cfg(test)]
    pub(crate) async fn set_agent_cap_fixture(&self, cap: Option<usize>) {
        *self.agent_cap_fixture.write().await = cap;
    }

    /// The zh-TW agent-cap refusal for this gateway's edition / tier.
    pub(crate) async fn agent_cap_text(&self, cap: usize) -> String {
        if self.resolve_edition_profile().await.is_personal() {
            return crate::license_runtime::agent_cap_text(cap, None);
        }
        let tier = match crate::license_runtime::global() {
            Some(rt) => rt.current_tier().await.to_string(),
            None => "目前方案".to_string(),
        };
        crate::license_runtime::agent_cap_text(cap, Some(&tier))
    }

    /// RM-149 — the one licence-seat admission of every dashboard create /
    /// restore path (`agents.create`, `templates.create_agent`,
    /// `templates.apply`, `agents.unarchive`): take the cross-process seat
    /// admission lock ([`duduclaw_core::agent_seats::SeatAdmission`]), then —
    /// inside it — resolve the cap, re-scan the seats and decide
    /// ([`Self::seat_refusal_under`]). `Ok` hands the held admission back:
    /// the caller keeps it until the new employee (or the whole batch) is
    /// published, so two creates can no longer both pass and end over the
    /// cap. `Err` is the structured refusal, with the lock already released.
    ///
    /// Lock order: this is taken **before** the global org lock, any
    /// `agent.toml` lock and the lifecycle barrier (see
    /// [`duduclaw_core::agent_seats`]). Not re-entrant: a batch takes it once
    /// and passes it down. A lock that cannot be taken within
    /// [`duduclaw_core::agent_seats::SEAT_ADMISSION_WAIT`] refuses (fail
    /// closed).
    pub(crate) async fn admit_agent_seats(
        &self,
        adding: usize,
        already_counted: Option<&str>,
    ) -> Result<duduclaw_core::agent_seats::SeatAdmission, Value> {
        let admission = self.acquire_seat_admission().await?;
        match self.seat_refusal_under(&admission, adding, already_counted).await {
            None => Ok(admission),
            Some(refusal) => Err(refusal),
        }
    }

    /// Take the seat admission lock (bounded wait, on a blocking thread).
    /// Fails closed with the `agent_seat_count_unavailable` refusal.
    pub(crate) async fn acquire_seat_admission(&self) -> Result<duduclaw_core::agent_seats::SeatAdmission, Value> {
        let home = self.home_dir.clone();
        match tokio::task::spawn_blocking(move || duduclaw_core::agent_seats::SeatAdmission::acquire(&home)).await {
            Ok(Ok(admission)) => Ok(admission),
            Ok(Err(e)) => {
                warn!(error = %e, "agent seat admission lock not acquired; refusing (fail closed)");
                Err(seat_count_unavailable_error())
            }
            Err(e) => {
                warn!(error = %e, "agent seat admission task failed; refusing (fail closed)");
                Err(seat_count_unavailable_error())
            }
        }
    }

    /// The seat decision taken **under** a held admission: the cap is
    /// resolved and the seats re-scanned now. Returns the structured refusal
    /// (`{code, message, max, current, adding, seat_holders}`) when adding
    /// `adding` employees would go over the cap, `None` when allowed.
    ///
    /// Seat holders come from [`duduclaw_core::agent_seats::seat_holders`]:
    /// every employee whose status is not `deleted`, archived and unreadable
    /// ones included. `already_counted` is the **directory name** of an
    /// employee that already holds a seat and is being brought back
    /// (unarchive), so it is not counted twice. A seat count that cannot be
    /// read refuses (fail closed) whenever a cap is in force. The holder list
    /// goes only to these Admin-only dashboard RPCs (sanitised, see
    /// [`duduclaw_core::agent_seats::seat_holders_json`]); MCP never gets it.
    pub(crate) async fn seat_refusal_under(
        &self,
        admission: &duduclaw_core::agent_seats::SeatAdmission,
        adding: usize,
        already_counted: Option<&str>,
    ) -> Option<Value> {
        use duduclaw_core::agent_seats::{decide, SeatDecision};
        let cap = self.effective_agent_cap().await;
        if cap == 0 || adding == 0 {
            return None;
        }
        let home = admission.home().to_path_buf();
        let scan = match tokio::task::spawn_blocking(move || duduclaw_core::agent_seats::seat_holders(&home)).await {
            Ok(scan) => scan,
            Err(e) => {
                warn!(error = %e, "agent seat count task failed; refusing (fail closed)");
                return Some(seat_count_unavailable_error());
            }
        };
        match decide(cap, adding, already_counted, scan) {
            SeatDecision::Admitted => None,
            SeatDecision::Unverifiable(e) => {
                warn!(error = %e, "agent seat count unreadable; refusing (fail closed)");
                Some(seat_count_unavailable_error())
            }
            SeatDecision::Full { max, current, holders } => {
                let message = format!(
                    "{}{}",
                    self.agent_cap_text(max).await,
                    crate::license_runtime::AGENT_SEAT_NOTE
                );
                Some(json!({
                    "code": duduclaw_core::agent_seats::SEAT_LIMIT_ERROR_CODE,
                    "message": message,
                    "max": max,
                    "current": current,
                    "adding": adding,
                    "seat_holders": duduclaw_core::agent_seats::seat_holders_json(&holders),
                }))
            }
        }
    }

    /// Test helper: the admission decision without keeping the lock.
    #[cfg(test)]
    pub(crate) async fn agent_seat_refusal(&self, adding: usize, already_counted: Option<&str>) -> Option<Value> {
        self.admit_agent_seats(adding, already_counted).await.err()
    }

    /// The agent-count cap in force for this gateway (`0` = unlimited) —
    /// the one decision `agents.create`, `templates.create` and the pack
    /// importer (`experts.install` passes it as `--agent-cap`) all use.
    ///
    /// Personal edition: `personal_max_agents()` (unlimited by default per
    /// the B+C decision of 2026-07-16; `DUDUCLAW_PERSONAL_MAX_AGENTS` opts a
    /// hosted deployment into a hard cap), without consulting license tiers.
    /// Otherwise the license: a signed per-license `max_agents` override is
    /// the issuer's explicit intent to cap the seat count and applies even
    /// self-hosted (P-License §7(b)); without one, self-host is never capped.
    pub(crate) async fn effective_agent_cap(&self) -> usize {
        #[cfg(test)]
        if let Some(cap) = *self.agent_cap_fixture.read().await {
            return cap;
        }
        if self.resolve_edition_profile().await.is_personal() {
            return duduclaw_core::EditionProfile::personal_max_agents();
        }
        let Some(rt) = crate::license_runtime::global() else {
            return 0;
        };
        let agent_override = rt.snapshot().await.max_agents.is_some();
        let is_self_host = crate::license_runtime::is_self_host_deployment();
        if !crate::license_runtime::agent_cap_enforced(is_self_host, agent_override) {
            return 0;
        }
        // Effective limit (override > tier) so a per-license quota bites.
        rt.effective_max_agents().await
    }

    /// Count configured channels across global config.toml + every agent's
    /// `[channels]` section. Mirrors `handle_channels_status`'s enumeration so
    /// the cap counts exactly what the dashboard shows.
    pub(crate) async fn count_configured_channels(&self) -> usize {
        let mut n = 0usize;
        let config_path = self.home_dir.join("config.toml");
        if let Ok(content) = tokio::fs::read_to_string(&config_path).await
            && let Ok(config) = content.parse::<toml::Table>()
            && let Some(ch) = config.get("channels").and_then(|v| v.as_table())
        {
            for key in [
                "line_channel_token",
                "telegram_bot_token",
                "discord_bot_token",
            ] {
                // Presence = plaintext OR `_enc` (2026-07 MED: channels.add is
                // now enc-only for these too; a plaintext-only check would
                // undercount freshly-saved channels).
                let plain = ch
                    .get(key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let enc = ch
                    .get(&format!("{key}_enc"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                if plain || enc {
                    n += 1;
                }
            }
        }
        let reg = self.registry.read().await;
        for agent in reg.list() {
            if let Some(ch) = &agent.config.channels {
                if ch.discord.as_ref().is_some_and(|d| {
                    !d.bot_token.is_empty()
                        || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                }) {
                    n += 1;
                }
                if ch.telegram.as_ref().is_some_and(|t| {
                    !t.bot_token.is_empty()
                        || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                }) {
                    n += 1;
                }
                if ch.slack.as_ref().is_some_and(|s| {
                    !s.bot_token.is_empty()
                        || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                }) {
                    n += 1;
                }
            }
        }
        n
    }
}

/// `error.code` of a create / restore refused because the seat count could
/// not be read while a cap is in force (fail closed).
pub(crate) const SEAT_COUNT_UNAVAILABLE_ERROR_CODE: &str = "agent_seat_count_unavailable";

pub(crate) fn seat_count_unavailable_error() -> Value {
    json!({
        "code": SEAT_COUNT_UNAVAILABLE_ERROR_CODE,
        "message": "目前無法確認已使用的 AI 員工名額，為了安全先不建立或恢復。請稍後再試，或請管理員檢查 AI 員工資料夾的讀取權限。",
    })
}

/// The error frame carrying a structured seat refusal.
pub(crate) fn seat_refusal_frame(error: Value) -> WsFrame {
    WsFrame::Response { id: String::new(), ok: false, payload: None, error: Some(error) }
}
