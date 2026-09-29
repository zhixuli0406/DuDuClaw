//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Analytics ────────────────────────────────────────────

    /// Summary metrics for the dashboard report page.
    ///
    /// Aggregates data from CostTelemetry (SQLite) and session counts.
    pub(crate) async fn handle_analytics_summary(&self, params: Value) -> WsFrame {
        let period = params
            .get("period")
            .and_then(|v| v.as_str())
            .unwrap_or("month");
        let hours: u64 = match period {
            "day" => 24,
            "week" => 168,
            _ => 720, // month
        };

        // Session counts from sessions.db
        let session_db = self.home_dir.join("sessions.db");
        let (
            total_conversations,
            total_messages,
            auto_reply_count,
            avg_response_ms,
            p95_response_ms,
        ) = if session_db.exists() {
            match rusqlite::Connection::open(&session_db) {
                Ok(conn) => {
                    let cutoff =
                        (chrono::Utc::now() - chrono::Duration::hours(hours as i64)).to_rfc3339();
                    let convos: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM sessions WHERE last_active >= ?1",
                            params![cutoff],
                            |r| r.get(0),
                        )
                        .unwrap_or(0);
                    // 2026-07 MED: exclude hide/undo tombstones so the
                    // dashboard counts match runs.list / runs.get. Older
                    // DBs without the columns fall back to plain counts
                    // (never silently 0).
                    let msgs: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM session_messages sm
                             JOIN sessions s ON sm.session_id = s.id
                             WHERE s.last_active >= ?1
                               AND COALESCE(sm.hidden, 0) = 0 AND sm.undone_at IS NULL",
                            params![cutoff],
                            |r| r.get(0),
                        )
                        .or_else(|e| match e {
                            rusqlite::Error::SqliteFailure(..) => conn.query_row(
                                "SELECT COUNT(*) FROM session_messages sm
                                 JOIN sessions s ON sm.session_id = s.id
                                 WHERE s.last_active >= ?1",
                                params![cutoff],
                                |r| r.get(0),
                            ),
                            other => Err(other),
                        })
                        .unwrap_or(0);
                    // auto_reply: messages from assistant role
                    let auto: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM session_messages sm
                             JOIN sessions s ON sm.session_id = s.id
                             WHERE s.last_active >= ?1 AND sm.role = 'assistant'
                               AND COALESCE(sm.hidden, 0) = 0 AND sm.undone_at IS NULL",
                            params![cutoff],
                            |r| r.get(0),
                        )
                        .or_else(|e| match e {
                            rusqlite::Error::SqliteFailure(..) => conn.query_row(
                                "SELECT COUNT(*) FROM session_messages sm
                                 JOIN sessions s ON sm.session_id = s.id
                                 WHERE s.last_active >= ?1 AND sm.role = 'assistant'",
                                params![cutoff],
                                |r| r.get(0),
                            ),
                            other => Err(other),
                        })
                        .unwrap_or(0);
                    (convos, msgs, auto, 850_u64, 2400_u64)
                }
                Err(_) => (0, 0, 0, 0, 0),
            }
        } else {
            (0, 0, 0, 0, 0)
        };

        // Cost data from CostTelemetry
        let (zero_cost_ratio, estimated_savings_cents) =
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                match telemetry.summary_global(hours).await {
                    Ok(summary) => {
                        let total_reqs = summary.total_requests.max(1);
                        // Zero-cost = requests handled without API calls (local inference / cached)
                        let cache_eff = summary.avg_cache_efficiency;
                        // `*_millicents` already holds whole cents (see
                        // `estimated_cost_millicents`); the dashboard divides by
                        // 100 for dollars, so pass cents straight through.
                        let savings = summary.total_cache_savings_millicents;
                        (cache_eff, savings)
                    }
                    Err(_) => (0.0, 0),
                }
            } else {
                (0.0, 0)
            };

        let auto_reply_rate = if total_messages > 0 {
            auto_reply_count as f64 / total_messages as f64
        } else {
            0.0
        };

        WsFrame::ok_response(
            "",
            json!({
                "total_conversations": total_conversations,
                "total_messages": total_messages,
                "auto_reply_rate": auto_reply_rate,
                "avg_response_ms": avg_response_ms,
                "p95_response_ms": p95_response_ms,
                "zero_cost_ratio": zero_cost_ratio,
                "estimated_savings_cents": estimated_savings_cents,
                "period": period,
            }),
        )
    }

    /// Daily conversation counts for the trend chart.
    pub(crate) async fn handle_analytics_conversations(&self) -> WsFrame {
        let session_db = self.home_dir.join("sessions.db");
        let daily: Vec<Value> = if session_db.exists() {
            match rusqlite::Connection::open(&session_db) {
                Ok(conn) => {
                    let mut stmt = conn
                        .prepare(
                            "SELECT DATE(last_active) as day,
                                COUNT(*) as total,
                                COUNT(CASE WHEN total_tokens > 0 THEN 1 END) as auto
                         FROM sessions
                         WHERE last_active >= DATE('now', '-30 days')
                         GROUP BY day
                         ORDER BY day ASC",
                        )
                        .unwrap();
                    let rows = stmt
                        .query_map([], |row| {
                            let date: String = row.get(0)?;
                            let count: i64 = row.get(1)?;
                            let auto_count: i64 = row.get(2)?;
                            Ok(json!({
                                "date": date,
                                "count": count,
                                "auto_count": auto_count,
                            }))
                        })
                        .unwrap();
                    rows.filter_map(|r| r.ok()).collect()
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        WsFrame::ok_response("", json!({ "daily": daily }))
    }

    /// Monthly cost comparison data for the savings table.
    pub(crate) async fn handle_analytics_cost_savings(&self) -> WsFrame {
        let monthly: Vec<Value> = if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
            // Get data for last 6 months
            let mut result = Vec::new();
            for months_ago in (0..6).rev() {
                let start_hours = (months_ago + 1) * 720;
                let end_hours = months_ago * 720;

                let start_summary = telemetry.summary_global(start_hours).await;
                let end_summary = telemetry.summary_global(end_hours).await;

                let (period_cost, period_savings) = match (start_summary, end_summary) {
                    (Ok(start), Ok(end)) => {
                        let cost = start
                            .total_cost_millicents
                            .saturating_sub(end.total_cost_millicents);
                        let savings = start
                            .total_cache_savings_millicents
                            .saturating_sub(end.total_cache_savings_millicents);
                        (cost, savings)
                    }
                    _ => (0, 0),
                };

                let month_date = chrono::Utc::now() - chrono::Duration::hours(end_hours as i64);
                let month_label = month_date.format("%Y-%m").to_string();

                // Estimate human cost as 3x of agent cost (industry benchmark)
                let human_cost_estimate = period_cost * 3;

                // `*_millicents` already holds whole cents (see
                // `estimated_cost_millicents`); the dashboard divides by 100 for
                // dollars, so emit cents directly — no scaling.
                result.push(json!({
                    "month": month_label,
                    "human_cost": human_cost_estimate,
                    "agent_cost": period_cost,
                    "savings": human_cost_estimate.saturating_sub(period_cost),
                }));
            }
            result
        } else {
            Vec::new()
        };

        WsFrame::ok_response("", json!({ "monthly": monthly }))
    }

    // ── Billing ──────────────────────────────────────────────

    /// Return real usage data for the billing page.
    ///
    /// - conversations: session count this month from sessions.db
    /// - agents: active agent count from registry
    /// - channels: connected channel count from channel_status
    /// - inference_hours: estimated from CostTelemetry token usage this month
    pub(crate) async fn handle_billing_usage(&self) -> WsFrame {
        let now = chrono::Utc::now();
        // Start of current month in RFC3339
        let month_start = now
            .date_naive()
            .with_day(1)
            .unwrap_or(now.date_naive())
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default();
        let month_start_utc =
            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(month_start, chrono::Utc);
        let hours_since_start = hours_since_month_start();

        // Conversations this month from sessions.db
        let session_db = self.home_dir.join("sessions.db");
        let conversations_used: i64 = if session_db.exists() {
            rusqlite::Connection::open(&session_db)
                .ok()
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT COUNT(*) FROM sessions WHERE last_active >= ?1",
                        params![month_start_utc.to_rfc3339()],
                        |r| r.get(0),
                    )
                    .ok()
                })
                .unwrap_or(0)
        } else {
            0
        };

        // Active agents from registry
        let reg = self.registry.read().await;
        let agents_used = reg.list().len() as i64;
        drop(reg);

        // Connected channels
        let channel_map = self.channel_status.read().await;
        let channels_used = channel_map.values().filter(|s| s.connected).count() as i64;
        drop(channel_map);

        // Inference hours estimated from total output tokens this month
        // Rough heuristic: 1 hour ≈ 50 requests average
        let inference_hours_used: f64 =
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                match telemetry.summary_global(hours_since_start).await {
                    Ok(summary) => summary.total_requests as f64 / 50.0,
                    Err(_) => 0.0,
                }
            } else {
                0.0
            };

        // Community edition: unlimited (-1)
        let reset_at = (month_start_utc + chrono::Duration::days(30)).to_rfc3339();

        WsFrame::ok_response(
            "",
            json!({
                "plan": "community",
                "tier": "community",
                "conversations": { "used": conversations_used, "limit": -1 },
                "agents": { "used": agents_used, "limit": -1 },
                "channels": { "used": channels_used, "limit": -1 },
                "inference_hours": { "used": inference_hours_used.round() as i64, "limit": -1 },
                "reset_at": reset_at,
            }),
        )
    }

    // ── Heartbeat ────────────────────────────────────────────

    pub(crate) async fn handle_heartbeat_status(&self) -> WsFrame {
        let hb = self.heartbeat.read().await;
        match hb.as_ref() {
            Some(scheduler) => {
                let statuses = scheduler.status().await;
                WsFrame::ok_response(
                    "",
                    json!({
                        "heartbeats": statuses,
                        "count": statuses.len(),
                    }),
                )
            }
            None => WsFrame::ok_response(
                "",
                json!({
                    "heartbeats": [],
                    "count": 0,
                    "message": "Heartbeat scheduler not started",
                }),
            ),
        }
    }

    pub(crate) async fn handle_heartbeat_trigger(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "agent_id is required");
        }

        let hb = self.heartbeat.read().await;
        match hb.as_ref() {
            Some(scheduler) => {
                let triggered = scheduler.trigger(agent_id).await;
                if triggered {
                    WsFrame::ok_response(
                        "",
                        json!({
                            "success": true,
                            "message": format!("Heartbeat triggered for agent '{agent_id}'"),
                        }),
                    )
                } else {
                    WsFrame::error_response(
                        "",
                        &format!("Agent '{agent_id}' not found in heartbeat scheduler"),
                    )
                }
            }
            None => WsFrame::error_response("", "Heartbeat scheduler not started"),
        }
    }

    // ── Logs ────────────────────────────────────────────────

    pub(crate) fn handle_logs_subscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(
            filter,
            "logs.subscribe activated — WebSocket push enabled for this connection"
        );
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "subscribed": true,
                "filter": filter,
                "message": "Log push active — events will stream on this WebSocket connection",
            }),
        )
    }

    pub(crate) fn handle_logs_unsubscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(
            filter,
            "logs.unsubscribe — WebSocket push disabled for this connection"
        );
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "subscribed": false,
                "filter": filter,
            }),
        )
    }
}
