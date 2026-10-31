//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Growth / gamification (V10-T10.0) ───────────────────

    /// Gather the real, already-persisted facts the growth engine scores. Every
    /// value comes from an existing internal surface (tasks store / wiki /
    /// skills registry / cron / custom-skill registry); nothing is fabricated.
    /// Missing/uninjected surfaces contribute 0 rather than an error so the
    /// snapshot degrades gracefully.
    pub(crate) async fn gather_growth_facts(&self) -> crate::growth::GrowthFacts {
        // Agents.
        let agents_count = self.registry.read().await.list().len() as u64;

        // Completed tasks (status == "done", NOT "completed").
        let tasks_completed = match self.task_store.read().await.as_ref().cloned() {
            Some(store) => store
                .list_tasks(Some("done"), None, None)
                .await
                .map(|v| v.len() as u64)
                .unwrap_or(0),
            None => 0,
        };

        // Knowledge pages: every agent wiki + the shared wiki.
        let mut knowledge_pages: u64 = 0;
        if let Ok(rd) = std::fs::read_dir(self.home_dir.join("agents")) {
            for entry in rd.flatten() {
                let wiki_dir = entry.path().join("wiki");
                if wiki_dir.exists() {
                    let store = duduclaw_memory::WikiStore::new(wiki_dir);
                    if let Ok(pages) = store.list_pages() {
                        knowledge_pages += pages.len() as u64;
                    }
                }
            }
        }
        if self.home_dir.join("shared").join("wiki").exists() {
            let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
            if let Ok(pages) = store.list_pages() {
                knowledge_pages += pages.len() as u64;
            }
        }

        // Installed skills: global + each agent's SKILLS.
        let mut skills_acquired: u64 =
            duduclaw_agent::registry::AgentRegistry::load_skills(&self.home_dir.join("skills"))
                .await
                .len() as u64;
        if let Ok(rd) = std::fs::read_dir(self.home_dir.join("agents")) {
            for entry in rd.flatten() {
                if entry.path().is_dir() {
                    let dir = entry.path().join("SKILLS");
                    skills_acquired += duduclaw_agent::registry::AgentRegistry::load_skills(&dir)
                        .await
                        .len() as u64;
                }
            }
        }

        // Successful routine runs = total runs − failures (real cron counters).
        let routines_completed = match self.cron_store.read().await.as_ref().cloned() {
            Some(store) => store
                .list_all()
                .await
                .map(|rows| {
                    rows.iter()
                        .map(|r| (r.run_count - r.failure_count).max(0) as u64)
                        .sum()
                })
                .unwrap_or(0),
            None => 0,
        };

        // Approved custom skills — count + cumulative real saved-hours (floored).
        // Both derive from one registry read so the numbers can't disagree.
        let (custom_skills_approved, custom_skill_saved_hours) =
            match crate::custom_skills::CustomSkillStore::open(&self.home_dir) {
                Ok(store) => {
                    let approved = store.list_approved().await.unwrap_or_default();
                    let now = chrono::Utc::now();
                    let saved: f64 = approved
                        .iter()
                        .map(|r| crate::custom_skills::estimate_saved_hours(r, now))
                        .sum();
                    (approved.len() as u64, saved.floor().max(0.0) as u64)
                }
                Err(_) => (0, 0),
            };

        crate::growth::GrowthFacts {
            agents_count,
            tasks_completed,
            knowledge_pages,
            skills_acquired,
            routines_completed,
            custom_skills_approved,
            // Filled by `handle_growth_snapshot` (needs the growth store to record
            // today's snapshot first); 0 here for callers that don't (daily_report).
            inbox_zero_streak_days: 0,
            custom_skill_saved_hours,
        }
    }

    /// Count the actionable inbox for the current moment — the server's own view
    /// of "work waiting on a human": pending approvals + blocked tasks + open
    /// budget incidents. Drives the `inbox_zero_streak_7` achievement. Any source
    /// that fails to open contributes 0 (a store we can't read is not fabricated
    /// as work).
    pub(crate) async fn count_actionable_inbox(&self) -> u64 {
        let pending = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b
                .list_pending(None)
                .await
                .map(|v| v.len() as u64)
                .unwrap_or(0),
            Err(_) => 0,
        };
        let blocked = match self.task_store.read().await.as_ref().cloned() {
            Some(store) => store
                .list_tasks(Some("blocked"), None, None)
                .await
                .map(|v| v.len() as u64)
                .unwrap_or(0),
            None => 0,
        };
        let incidents = std::fs::read_to_string(self.home_dir.join("budget_events.jsonl"))
            .map(|raw| raw.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0);
        pending.saturating_add(blocked).saturating_add(incidents)
    }

    /// Reduced reads use the last Manager-persisted XP, never gather company
    /// facts or settle inbox / achievement snapshots. A fresh install has zero
    /// recorded XP; corrupt/unreadable stores refuse.
    fn reduced_growth_snapshot(&self) -> Result<Value, String> {
        use rusqlite::OptionalExtension;
        let path = self.home_dir.join("growth.db");
        let xp = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => return Err("growth snapshot unavailable".into()),
            Ok(_) => {
                let db = crate::review_evidence::audience::readonly_db(&path)?;
                db.query_row("SELECT xp FROM xp_snapshots ORDER BY id DESC LIMIT 1", [], |row| row.get::<_, u64>(0))
                    .optional().map_err(|_| "growth snapshot unavailable")?.unwrap_or(0)
            }
        };
        let level = crate::growth::level_from_xp(xp);
        let floor = crate::growth::xp_threshold_for_level(level);
        Ok(json!({
            "xp": xp,
            "level": level,
            "xp_into_level": xp.saturating_sub(floor),
            "xp_for_next_level": crate::growth::xp_threshold_for_level(level + 1).saturating_sub(floor),
        }))
    }

    /// Managers receive company facts and persist growth; other authenticated
    /// humans receive only the saved level / XP summary.
    pub(crate) async fn handle_growth_snapshot(&self, ctx: &UserContext) -> WsFrame {
        let live = match self.bound_history_live(ctx) {
            Ok(live) => live,
            Err(_) => return WsFrame::error_response("", "permission denied"),
        };
        if !live.role.at_least(UserRole::Manager) {
            let summary = match self.reduced_growth_snapshot() {
                Ok(summary) => summary,
                Err(_) => return WsFrame::error_response("", "growth snapshot unavailable"),
            };
            if self.bound_history_live(&live).is_err() {
                return WsFrame::error_response("", "permission denied");
            }
            return WsFrame::ok_response("", summary);
        }
        let may_manage = || self.bound_history_live(&live)
            .is_ok_and(|current| current.role.at_least(UserRole::Manager));
        let store = match crate::growth::GrowthStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open growth store: {e}")),
        };
        // L5 §14: lazily record today's actionable-inbox count (server self-check),
        // then read the current zero-streak. `record_inbox_snapshot` takes the
        // daily MIN, so an inbox cleared at any point today counts as zero.
        let now = chrono::Utc::now();
        let today = now.format("%Y-%m-%d").to_string();
        let actionable = self.count_actionable_inbox().await;
        if !may_manage() {
            return WsFrame::error_response("", "permission denied");
        }
        let _ = store
            .record_inbox_snapshot(&today, actionable as i64, &now.to_rfc3339())
            .await;
        let inbox_streak = store.inbox_zero_streak_days().await.unwrap_or(0);

        let mut facts = self.gather_growth_facts().await;
        facts.inbox_zero_streak_days = inbox_streak;
        if !may_manage() {
            return WsFrame::error_response("", "permission denied");
        }
        let (snap, unlock_times) = match crate::growth::snapshot_with_store(&store, &facts).await {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &format!("growth snapshot: {e}")),
        };
        let achievements: Vec<Value> = snap
            .achievements
            .iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "unlocked": a.unlocked,
                    "progress_current": a.progress_current,
                    "progress_denominator": a.progress_denominator,
                    "xp_reward": a.xp_reward,
                    "available": a.available,
                    "unavailable_reason": a.unavailable_reason,
                    "unlocked_at": unlock_times.get(&a.id),
                })
            })
            .collect();
        if !may_manage() {
            return WsFrame::error_response("", "permission denied");
        }
        WsFrame::ok_response(
            "",
            json!({
                "xp": snap.xp,
                "level": snap.level,
                "xp_into_level": snap.xp_into_level,
                "xp_for_next_level": snap.xp_for_next_level,
                "facts": facts,
                "achievements": achievements,
            }),
        )
    }

    /// `growth.daily_report` — yesterday's settlement card: completed tasks,
    /// cost, most-active agent, new knowledge pages, and the XP gained. Cached
    /// per date in `growth.db`. `date` param (YYYY-MM-DD) overrides "yesterday".
    pub(crate) async fn handle_growth_daily_report(&self, params: Value) -> WsFrame {
        let store = match crate::growth::GrowthStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("open growth store: {e}")),
        };
        let report_date = params
            .get("date")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                (chrono::Utc::now() - chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string()
            });

        // Serve from cache when present (a settled past day never changes).
        if let Ok(Some(cached)) = store.get_daily_report(&report_date).await {
            if let Ok(v) = serde_json::from_str::<Value>(&cached) {
                return WsFrame::ok_response("", v);
            }
        }

        // Completed tasks whose completed_at falls on report_date.
        let (tasks_done_yesterday, most_active_agent) =
            match self.task_store.read().await.as_ref().cloned() {
                Some(ts) => {
                    let done = ts
                        .list_tasks(Some("done"), None, None)
                        .await
                        .unwrap_or_default();
                    let count = done
                        .iter()
                        .filter(|t| {
                            t.completed_at
                                .as_deref()
                                .map(|c| c.starts_with(&report_date))
                                .unwrap_or(false)
                        })
                        .count() as u64;
                    // Most active agent = most task completions on report_date.
                    let mut by_agent: std::collections::HashMap<String, u64> =
                        std::collections::HashMap::new();
                    for t in &done {
                        if t.completed_at
                            .as_deref()
                            .map(|c| c.starts_with(&report_date))
                            .unwrap_or(false)
                            && !t.assigned_to.is_empty()
                        {
                            *by_agent.entry(t.assigned_to.clone()).or_insert(0) += 1;
                        }
                    }
                    let top = by_agent.into_iter().max_by_key(|(_, c)| *c).map(|(a, _)| a);
                    (count, top)
                }
                None => (0, None),
            };

        // New knowledge pages authored on report_date (agent + shared wikis).
        let mut new_knowledge_yesterday: u64 = 0;
        let count_pages_on = |dir: duduclaw_memory::WikiStore| -> u64 {
            dir.list_pages()
                .map(|pages| {
                    pages
                        .iter()
                        .filter(|p| p.updated.format("%Y-%m-%d").to_string() == report_date)
                        .count() as u64
                })
                .unwrap_or(0)
        };
        if let Ok(rd) = std::fs::read_dir(self.home_dir.join("agents")) {
            for entry in rd.flatten() {
                let wiki_dir = entry.path().join("wiki");
                if wiki_dir.exists() {
                    new_knowledge_yesterday +=
                        count_pages_on(duduclaw_memory::WikiStore::new(wiki_dir));
                }
            }
        }
        if self.home_dir.join("shared").join("wiki").exists() {
            new_knowledge_yesterday +=
                count_pages_on(duduclaw_memory::WikiStore::new_shared(&self.home_dir));
        }

        // Cost over report_date (rolling 24h telemetry approximation, in cents).
        let cost_cents = if let Some(t) = crate::cost_telemetry::get_telemetry() {
            t.summary_global(24)
                .await
                .map(|s| s.total_cost_millicents)
                .unwrap_or(0)
        } else {
            0
        };

        // XP earned yesterday from the datable events we can attribute (task
        // completions + new knowledge pages). Skills/routines lack a per-day
        // timestamp here, so they are intentionally excluded — documented,
        // not fabricated.
        let xp_gained = tasks_done_yesterday * crate::growth::XP_PER_TASK
            + new_knowledge_yesterday * crate::growth::XP_PER_KNOWLEDGE_PAGE;

        let payload = json!({
            "date": report_date,
            "tasks_completed": tasks_done_yesterday,
            "cost_cents": cost_cents,
            "most_active_agent": most_active_agent,
            "new_knowledge_pages": new_knowledge_yesterday,
            "xp_gained": xp_gained,
            "xp_basis": "task completions ×12 + new knowledge pages ×8 (skills/routines excluded — no per-day timestamp)",
        });

        // Cache only past days (today may still change).
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        if report_date != today {
            let now = chrono::Utc::now().to_rfc3339();
            let _ = store
                .put_daily_report(&report_date, &payload.to_string(), &now)
                .await;
        }

        WsFrame::ok_response("", payload)
    }
}

#[cfg(test)]
mod snapshot_authorization_tests {
    use super::*;

    fn payload(frame: WsFrame) -> Value {
        match frame {
            WsFrame::Response { ok: true, payload: Some(value), .. } => value,
            other => panic!("expected success: {other:?}"),
        }
    }

    #[tokio::test]
    async fn employee_snapshot_is_reduced_read_only_and_uses_live_role() {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let user = db.create_user("growth@test.invalid", "Reader", "test-password", UserRole::Employee).unwrap();
        let employee = db.live_user_context(&user.id).unwrap().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
        handler.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise)).await;

        let reduced = payload(handler.handle("growth.snapshot", json!({}), &employee).await);
        assert_eq!(reduced.as_object().unwrap().len(), 4);
        assert_eq!(reduced["level"], 0);
        assert!(!home.path().join("growth.db").exists());
        assert!(!home.path().join("custom_skills.db").exists());

        // Manager path still exposes details and records the company inbox.
        db.update_user(&user.id, None, Some(UserRole::Manager), None).unwrap();
        let manager = db.live_user_context(&user.id).unwrap().unwrap();
        let full = payload(handler.handle("growth.snapshot", json!({}), &manager).await);
        assert!(full["facts"].is_object());
        assert!(full["achievements"].is_array());
        let store = crate::growth::GrowthStore::open(home.path()).unwrap();
        assert_eq!(store.inbox_snapshots().await.unwrap().len(), 1);
        store.save_snapshot(450, 2, "{}", "2026-10-31T00:00:00Z").await.unwrap();
        let counts = || {
            let conn = crate::review_evidence::audience::readonly_db(&home.path().join("growth.db")).unwrap();
            ["inbox_daily_snapshot", "xp_snapshots", "achievement_unlocks"].map(|table| {
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get::<_, i64>(0)).unwrap()
            })
        };
        let before = counts();
        // A stale Manager context must take the reduced lane immediately.
        db.update_user(&user.id, None, Some(UserRole::Employee), None).unwrap();
        for _ in 0..2 {
            let reduced = payload(handler.handle("growth.snapshot", json!({}), &manager).await);
            assert_eq!(reduced, json!({"xp":450,"level":2,"xp_into_level":50,"xp_for_next_level":500}));
        }
        assert_eq!(before, counts());
        db.set_user_status(&user.id, duduclaw_auth::UserStatus::Suspended).unwrap();
        assert!(matches!(handler.handle("growth.snapshot", json!({}), &manager).await, WsFrame::Response { ok: false, .. }));
    }

    #[tokio::test]
    async fn reduced_snapshot_refuses_corrupt_growth_store() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        std::fs::write(home.path().join("growth.db"), "not a database").unwrap();
        assert!(handler.reduced_growth_snapshot().is_err());
    }
}
