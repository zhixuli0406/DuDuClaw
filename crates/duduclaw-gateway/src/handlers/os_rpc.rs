//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── OS-native page RPCs (P4-3) ──────────────────────────

    /// `os.status` — whole-fleet OS-native snapshot for the dashboard OS page.
    /// Per agent: os_native flag, watch paths + live stats, frontmost poll
    /// interval + running flag, footprint flag, proactive config, and induced
    /// rule count; plus fleet-level quota (limit/used) and edition.
    pub(crate) async fn handle_os_status(&self) -> WsFrame {
        let edition = self.resolve_edition_profile().await;
        let quota = crate::license_runtime::os_native_agent_quota(edition);

        // Live in-process watch stats + frontmost running set (dir-name keyed).
        let watch_snap = self.os_watchers.snapshot().await;

        // Induced autopilot rules (PBD, P4-1) grouped by the agent id their
        // conditions reference — best-effort deterministic count for the page.
        let induced_rules: Vec<AutopilotRuleRow> = match self.autopilot_store.read().await.as_ref()
        {
            Some(store) => store
                .list_rules()
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|r| rule_is_induced(r))
                .collect(),
            None => Vec::new(),
        };

        let mut agents: Vec<Value> = Vec::new();
        let mut used = 0usize;
        {
            let reg = self.registry.read().await;
            let mut list: Vec<&duduclaw_agent::registry::LoadedAgent> = reg.list();
            // Stable order so the page and the startup quota gate agree on
            // which agent holds the single Personal seat.
            list.sort_by(|a, b| a.config.agent.name.cmp(&b.config.agent.name));
            for a in list {
                let name = a.config.agent.name.clone();
                let dir_id = a
                    .dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let os_native = a.config.capabilities.os_native;
                if os_native {
                    used += 1;
                }

                // Watch paths + stats (only present while a watcher runs).
                let (watch_paths, watch_events, watch_dropped) = match watch_snap.get(&dir_id) {
                    Some(s) => (s.watched_paths.clone(), s.emitted, s.dropped),
                    None => (Vec::new(), 0, 0),
                };

                let frontmost_running = self.os_frontmost.status(&dir_id).await;
                let frontmost_poll_secs =
                    crate::os_frontmost::read_frontmost_poll_secs(&a.dir).unwrap_or(0);
                let footprint = crate::footprint_distill::read_footprint_enabled(&a.dir);
                let pcfg = crate::proactive_gate::read_proactive_config(&a.dir);
                let induced_count = induced_rules
                    .iter()
                    .filter(|r| rule_targets_agent(r, &dir_id) || rule_targets_agent(r, &name))
                    .count();

                agents.push(json!({
                    "agent_id": name,
                    "os_native": os_native,
                    "watch": {
                        "paths": watch_paths,
                        "events": watch_events,
                        "dropped": watch_dropped,
                    },
                    "frontmost": {
                        "poll_secs": frontmost_poll_secs,
                        "running": frontmost_running.is_some(),
                    },
                    "footprint": footprint,
                    "proactive": {
                        "enabled": pcfg.enabled,
                        "base_threshold": pcfg.base_threshold,
                        "max_per_hour": pcfg.max_per_hour,
                    },
                    "induced_rules_count": induced_count,
                }));
            }
        }

        WsFrame::ok_response(
            "",
            json!({
                "edition": edition.as_str(),
                "quota": {
                    // null limit ⇒ unlimited (Enterprise).
                    "limit": quota,
                    "used": used,
                },
                "agents": agents,
            }),
        )
    }

    /// `os.settings.update` — per-agent OS-native settings write. Remaps the
    /// flat OS-page params onto the canonical `agents.update` shape and
    /// delegates, so the quota gate, `[os_watch]` / `[proactive]` validators,
    /// and the three-subsystem hot reload are reused verbatim (one write path,
    /// no divergence).
    pub(crate) async fn handle_os_settings_update(
        &self,
        params: Value,
        caller: Option<&UserContext>,
    ) -> WsFrame {
        let Some(agent_id) = params.get("agent_id").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        };

        let mut mapped = serde_json::Map::new();
        mapped.insert("agent_id".into(), json!(agent_id));

        if let Some(v) = params.get("os_native").and_then(|v| v.as_bool()) {
            mapped.insert("capabilities".into(), json!({ "os_native": v }));
        }
        if let Some(p) = params.get("proactive") {
            mapped.insert("proactive".into(), p.clone());
        }
        // footprint / frontmost_poll_secs live under [os_watch].
        let mut ow = serde_json::Map::new();
        if let Some(v) = params.get("footprint") {
            ow.insert("footprint".into(), v.clone());
        }
        if let Some(v) = params.get("frontmost_poll_secs") {
            ow.insert("frontmost_poll_secs".into(), v.clone());
        }
        if let Some(os_watch) = params.get("os_watch").and_then(|v| v.as_object()) {
            // Allow a nested os_watch object too (paths/ignore/debounce/etc.).
            for (k, v) in os_watch {
                ow.insert(k.clone(), v.clone());
            }
        }
        if !ow.is_empty() {
            mapped.insert("os_watch".into(), Value::Object(ow));
        }

        self.handle_agents_update_as(Value::Object(mapped), caller).await
    }

    /// `os.gate.recent` — tail of `proactive_gate.jsonl` (default 50, max 200)
    /// plus the four-quadrant outcome aggregation for the OS page's proactivity
    /// panel.
    pub(crate) async fn handle_os_gate_recent(&self, params: Value) -> WsFrame {
        let n = params
            .get("n")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .clamp(1, 200) as usize;
        let agent = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let path = self.home_dir.join("proactive_gate.jsonl");
        let recent = read_jsonl_tail(&path, n);
        // Optional per-agent filter on the parsed rows.
        let rows: Vec<Value> = match &agent {
            Some(a) => recent
                .into_iter()
                .filter(|r| r.get("agent").and_then(|v| v.as_str()) == Some(a.as_str()))
                .collect(),
            None => recent,
        };

        // Quadrant aggregation (pure read of the same file). `quadrant_stats`
        // filters to one agent; with no filter we sum it across the fleet so
        // the OS page can show a whole-team confusion matrix.
        let lookback = std::time::Duration::from_secs(7 * 24 * 60 * 60);
        let agent_ids: Vec<String> = match &agent {
            Some(a) => vec![a.clone()],
            None => {
                let reg = self.registry.read().await;
                reg.list()
                    .iter()
                    .map(|a| a.config.agent.name.clone())
                    .collect()
            }
        };
        let mut agg = crate::proactive_feedback::QuadrantStats::default();
        for aid in &agent_ids {
            if let Ok(s) = crate::proactive_feedback::quadrant_stats(&self.home_dir, aid, lookback)
            {
                agg.cd += s.cd;
                agg.fa += s.fa;
                agg.mn += s.mn;
                agg.nr += s.nr;
                agg.cs += s.cs;
                agg.unknown += s.unknown;
            }
        }
        let quadrants = json!({
            "correct_detection": agg.cd,
            "false_alarm": agg.fa,
            "missed_need": agg.mn,
            "non_response": agg.nr,
            "correct_silence": agg.cs,
            "unknown": agg.unknown,
        });

        WsFrame::ok_response(
            "",
            json!({
                "recent": rows,
                "quadrants": quadrants,
            }),
        )
    }

    /// `os.events.recent` — most recent os_* perception events from `events.db`
    /// (default 50, max 200), newest first.
    pub(crate) async fn handle_os_events_recent(&self, params: Value) -> WsFrame {
        let n = params
            .get("n")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .clamp(1, 200) as i64;
        let store = match crate::events_store::EventBusStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open events.db: {e}")),
        };
        match store.fetch_recent_by_prefix("os_", n).await {
            Ok(rows) => {
                let events: Vec<Value> = rows
                    .into_iter()
                    .map(|r| {
                        let payload: Value =
                            serde_json::from_str(&r.payload).unwrap_or(Value::Null);
                        json!({
                            "id": r.id,
                            "event": r.event,
                            "ts": r.ts,
                            "source": r.source,
                            "payload": payload,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "events": events }))
            }
            Err(e) => WsFrame::error_response("", &format!("os events: {e}")),
        }
    }

    /// `os.events.subscribe` (P4-3+) — opt this WebSocket connection into a
    /// live tail of `os_file`/`os_frontmost` events, pushed as `os.events.entry`
    /// frames shaped like one `os.events.recent` row (minus `id`, which only
    /// exists once the async persistence bridge — `os_events::
    /// spawn_os_event_persistence` — has written the row; live pushes race
    /// that write, so they never carry one). This handler only authorizes
    /// (`require_admin!` at the dispatch site) and acks; the actual
    /// subscribe/forward loop lives in `server.rs`'s WS handler, mirroring
    /// `logs.subscribe`'s per-connection-flag pattern. A per-connection
    /// forwarding cap (`os_events::OS_EVENTS_PUSH_CAP_PER_SEC`) guards against
    /// a runaway watcher flooding the socket.
    pub(crate) fn handle_os_events_subscribe(&self, _params: Value) -> WsFrame {
        info!("os.events.subscribe activated — live OS event push enabled for this connection");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "subscribed": true,
                "message": "Live OS event push active — os_file/os_frontmost events will stream as os.events.entry on this WebSocket connection",
            }),
        )
    }

    /// `os.events.unsubscribe` (P4-3+) — counterpart to
    /// [`Self::handle_os_events_subscribe`].
    pub(crate) fn handle_os_events_unsubscribe(&self, _params: Value) -> WsFrame {
        info!("os.events.unsubscribe — live OS event push disabled for this connection");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "subscribed": false,
            }),
        )
    }

    /// `os.doctor.run` — on-demand OS-native environment probes (the only
    /// expensive OS RPC). Reuses the `duduclaw-os` probe functions directly (no
    /// shell-out to the CLI binary): a live native notification, a live
    /// frontmost/System-Events probe, a live calendar probe, and an `mdfind`
    /// existence check. Each returns a structured `{status, detail}` — never a
    /// bypass or auto-grant on a TCC denial (report-only, fail-closed).
    pub(crate) async fn handle_os_doctor_run(&self) -> WsFrame {
        let mut checks: Vec<Value> = Vec::new();

        // 1) Native notification helper + a live test dispatch.
        let helper = if cfg!(target_os = "macos") {
            "osascript"
        } else if cfg!(target_os = "linux") {
            "notify-send"
        } else {
            ""
        };
        if helper.is_empty() {
            checks.push(json!({
                "id": "notification",
                "status": "warn",
                "detail": "此平台不支援原生桌面通知。",
            }));
        } else {
            match duduclaw_os::send_notification("DuDuClaw", "OS doctor test notification").await {
                Ok(()) => checks.push(json!({
                    "id": "notification",
                    "status": "ok",
                    "detail": "已送出測試通知。注意：送出成功不代表一定顯示——在背景服務下，通知的權限歸屬可能被系統靜音，請手動確認是否跳出。",
                })),
                Err(e) => checks.push(json!({
                    "id": "notification",
                    "status": "fail",
                    "detail": format!("測試通知失敗：{e}"),
                })),
            }
        }

        // 2) Frontmost (System Events automation) — live probe.
        match duduclaw_os::frontmost_info().await {
            Ok(info) => checks.push(json!({
                "id": "frontmost",
                "status": "ok",
                "detail": format!(
                    "前景偵測正常（目前：{} — 「{}」）。",
                    if info.app.is_empty() { "(未知)" } else { &info.app },
                    info.window_title
                ),
            })),
            Err(duduclaw_os::FrontmostError::Unsupported) => checks.push(json!({
                "id": "frontmost",
                "status": "skip",
                "detail": "此平台不支援前景視窗偵測。",
            })),
            Err(duduclaw_os::FrontmostError::PermissionDenied(msg)) => checks.push(json!({
                "id": "frontmost",
                "status": "fail",
                "detail": format!(
                    "尚未授權自動化權限（{msg}）。請到「系統設定 → 隱私權與安全性 → 自動化」允許終端機控制「System Events」。"
                ),
            })),
            Err(e) => checks.push(json!({
                "id": "frontmost",
                "status": "fail",
                "detail": format!("前景偵測失敗：{e}"),
            })),
        }

        // 3) Calendar (automation) — live probe.
        match duduclaw_os::today_events().await {
            Ok(events) => checks.push(json!({
                "id": "calendar",
                "status": "ok",
                "detail": format!("行事曆讀取正常（今日 {} 筆事件）。", events.len()),
            })),
            Err(duduclaw_os::CalendarError::Unsupported) => checks.push(json!({
                "id": "calendar",
                "status": "skip",
                "detail": "此平台不支援行事曆讀取。",
            })),
            Err(duduclaw_os::CalendarError::PermissionDenied(msg)) => checks.push(json!({
                "id": "calendar",
                "status": "fail",
                "detail": format!(
                    "尚未授權行事曆權限（{msg}）。請到「系統設定 → 隱私權與安全性 → 行事曆」允許存取。"
                ),
            })),
            Err(e) => checks.push(json!({
                "id": "calendar",
                "status": "fail",
                "detail": format!("行事曆讀取失敗：{e}"),
            })),
        }

        // 4) Spotlight (mdfind) existence check — macOS only, no TCC prompt.
        if cfg!(target_os = "macos") {
            if std::path::Path::new("/usr/bin/mdfind").exists() {
                checks.push(json!({
                    "id": "spotlight",
                    "status": "ok",
                    "detail": "已找到 /usr/bin/mdfind。",
                }));
            } else {
                checks.push(json!({
                    "id": "spotlight",
                    "status": "fail",
                    "detail": "找不到 /usr/bin/mdfind — Spotlight 搜尋將無法使用。",
                }));
            }
        } else {
            checks.push(json!({
                "id": "spotlight",
                "status": "skip",
                "detail": "Spotlight 搜尋僅支援 macOS。",
            }));
        }

        WsFrame::ok_response("", json!({ "checks": checks }))
    }
}
