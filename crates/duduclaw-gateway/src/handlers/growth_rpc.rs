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

    /// `growth.snapshot` — company XP/level + the achievement wall (progress,
    /// availability, unlock timestamps). Login-readable; the judging engine is
    /// pure and every scored value is a real fact.
    pub(crate) async fn handle_growth_snapshot(&self) -> WsFrame {
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
        let _ = store
            .record_inbox_snapshot(&today, actionable as i64, &now.to_rfc3339())
            .await;
        let inbox_streak = store.inbox_zero_streak_days().await.unwrap_or(0);

        let mut facts = self.gather_growth_facts().await;
        facts.inbox_zero_streak_days = inbox_streak;
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
