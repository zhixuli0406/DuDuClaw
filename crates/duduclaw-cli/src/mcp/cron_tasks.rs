use super::*;

/// The cron rail behind `tasks_create` with a cron `schedule`: write a
/// recurring task. Writes directly to the shared
/// SQLite cron store (`<home>/cron_tasks.db`). The gateway's running
/// `CronScheduler` picks up the new task on its next baseline tick
/// (≤ 30 seconds) — no inter-process signal is required because both
/// processes use WAL-mode SQLite.
pub(crate) async fn handle_schedule_task(params: &Value, home_dir: &Path, caller: &str) -> Value {
    handle_schedule_task_for_actor(params, home_dir, RecordActor::Agent(caller)).await
}

pub(crate) async fn handle_schedule_task_for_actor(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    let caller = actor.id();
    let principal = host_on_behalf_of(home_dir, caller);
    use duduclaw_gateway::cron_store::{CronStore, CronTaskRow};

    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let cron = params.get("cron").and_then(|v| v.as_str()).unwrap_or("");
    let task = params
        .get("task")
        .or_else(|| params.get("prompt"))
        .or_else(|| params.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed");

    if cron.is_empty() || task.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: cron and task are required"}],
            "isError": true
        });
    }
    if let Err(message) = duduclaw_gateway::scheduled_webchat::check_name(name) {
        return tool_error(&message);
    }

    // Validate cron expression before persisting — through the shared
    // normaliser so validation reads day-of-week exactly like the scheduler.
    if cron.len() > duduclaw_gateway::cron_store::MAX_CRON_BYTES {
        return tool_error("Automation schedule exceeds 256 bytes");
    }
    let normalised_cron = duduclaw_core::cron_tz::normalise_cron(cron);
    if normalised_cron.parse::<cron::Schedule>().is_err() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: invalid cron expression: {cron}")}],
            "isError": true
        });
    }

    // ── WP21 C3 (cron rail) ──────────────────────────────────────────────
    // A scheduled task fires under the `cron` system-sender identity, so once
    // the row exists the delegation predicate waves it through by design.
    // That makes *creating* a row aimed at somebody else the delegation, and
    // it is judged here — otherwise a scheduled task launders any
    // cross-department assignment through the scheduler.
    let scheduled_target = if agent_id == "default" {
        resolve_main_agent_name(home_dir).await
    } else {
        agent_id.to_string()
    };
    if let Err(reason) = duduclaw_gateway::on_behalf_of::access::check_main_target(
        home_dir, principal.as_deref(), &scheduled_target,
    ).await {
        return tool_error(&reason);
    }
    if scheduled_target != caller {
        if let Err(reason) =
            check_delegation_allowed(home_dir, caller, &scheduled_target, "tasks_create_schedule")
                .await
        {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {reason}")}],
                "isError": true
            });
        }
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: open cron store: {e}")}],
                "isError": true
            });
        }
    };

    // RM-124: the model's notify target, or — when it names none — the
    // WebChat conversation this turn answers (gateway-attested, bound to the
    // acting employee). A model-supplied `webchat` target is refused.
    let origin_env = std::env::var(duduclaw_core::ENV_ORIGIN_SESSION).ok();
    let target = match resolve_notify_target(
        params, home_dir, actor.agent(), &scheduled_target, origin_env.as_deref(),
    ) {
        Ok(target) => target,
        Err(message) => return tool_error(&message),
    };
    let delivery = target.delivery();
    let NotifyTarget { channel: notify_channel, chat: notify_chat_id, thread: notify_thread_id, .. } = target;
    // v1.8.25: auto-detect the host's IANA timezone when the caller doesn't
    // specify one explicitly. Historically the cron rail fell through to UTC
    // if `cron_timezone` was absent — which surprised every Taipei-based
    // user whose "0 8 * * *" fired at 16:00 local time. Auto-detecting
    // matches what a human would expect "8am every day" to mean when
    // running the scheduler on their own laptop / server.
    //
    // Explicit opt-out: pass `cron_timezone = "UTC"` to force UTC
    // evaluation. Explicit any-other-IANA-name still wins (below).
    let cron_timezone = params
        .get("cron_timezone")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            // Human-origin rows share the store's UTC cadence contract.
            if principal.is_some() { return Some("UTC".into()); }
            let detected = detect_local_timezone();
            if let Some(ref tz) = detected {
                tracing::info!(detected_tz = %tz, "cron schedule: auto-detected local timezone (no cron_timezone param supplied)");
            } else {
                tracing::warn!("cron schedule: could not detect local timezone — falling back to UTC");
            }
            detected
        });

    // Validate cron_timezone against the IANA database at call time so a
    // typo is reported to the scheduler caller instead of silently falling
    // back to UTC at firing time. Auto-detected values always parse (they
    // come from the host's TZ database), but explicit user input might
    // have a typo.
    if let Some(ref tz_name) = cron_timezone {
        if duduclaw_core::parse_timezone(tz_name).is_none() {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: unknown cron_timezone '{tz_name}'. Use an IANA name like 'Asia/Taipei' or 'America/New_York'."
                )}],
                "isError": true
            });
        }
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let mut row = CronTaskRow::new(
        task_id.clone(),
        name.to_string(),
        agent_id.to_string(),
        cron.to_string(),
        task.to_string(),
    );
    row.on_behalf_of = principal;
    row.notify_channel = notify_channel;
    row.notify_chat_id = notify_chat_id;
    row.notify_thread_id = notify_thread_id;
    row.cron_timezone = cron_timezone;

    let _admission = match admit_background_caller(home_dir, actor, "tasks_create").await {
        Ok(guard) => guard,
        Err(reason) => return tool_error(&reason),
    };
    match store.insert(&row).await {
        Ok(()) => {
            // WP6.4 — channel receipt. The old result text was an English
            // developer string ("Task 'x' scheduled (id: …)"), which the agent
            // paraphrased however it liked, so the user often got no
            // confirmation at all and never learned the routine is visible and
            // editable on the dashboard. The result is now the exact zh-TW
            // sentence to relay, plus a machine-readable summary line for the
            // model. The dashboard side of the same action is the
            // `cron.changed` event raised by the MCP dispatch tail.
            let receipt = format!(
                "{}{}",
                cron_created_receipt(name, cron, row.cron_timezone.as_deref()),
                delivery_receipt_suffix(delivery),
            );
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "{receipt}\n\n(請把上面這句話原樣轉達給使用者。id: {task_id})"
                )}]
            })
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: failed to persist task: {e}")}],
            "isError": true
        }),
    }
}

/// Render a cron expression as a zh-TW phrase for the channel receipt.
///
/// Deliberately small: it covers the shapes users actually dictate in chat
/// ("每天早上九點", "每週一", "每小時") and falls back to the raw expression
/// rather than guessing. A wrong-but-confident schedule description is worse
/// than showing the expression — the user cannot spot the error.
///
/// Accepts 5-field (`m h dom mon dow`) and 6-field (`s m h dom mon dow`) forms.
///
/// ## Day-of-week convention
///
/// User-facing expressions follow the classic Unix crontab convention —
/// `0` and `7` = Sunday, `1` = Monday … `6` = Saturday. The `cron` crate
/// itself is Quartz-flavoured (`1` = Sunday), but every parse site routes
/// through `duduclaw_core::cron_tz::normalise_cron`, which translates the
/// day-of-week field at parse time, so this function must read the *raw*
/// expression exactly as a crontab man page would. Anything above `7`
/// returns `None` so the receipt falls back to the raw expression instead
/// of guessing. `humanize_cron_matches_the_cron_crate` pins this against
/// the real scheduler (crate + normaliser together).
pub(crate) fn humanize_cron_zh(cron: &str) -> Option<String> {
    let f: Vec<&str> = cron.split_whitespace().collect();
    let f: &[&str] = match f.len() {
        5 => &f[..],
        6 => &f[1..],
        _ => return None,
    };
    let (min, hour, dom, mon, dow) = (f[0], f[1], f[2], f[3], f[4]);
    if mon != "*" || dom != "*" {
        return None;
    }
    let m: u32 = min.parse().ok()?;
    if m > 59 {
        return None;
    }

    // 每小時 — "M * * * *". Only when the day-of-week is unrestricted:
    // "0 * * * 2" fires hourly *on Tuesdays only*, so calling it "每小時"
    // would promise the user 24×7 coverage they are not getting.
    if hour == "*" {
        if dow != "*" {
            return None;
        }
        return Some(if m == 0 {
            "每小時整點".to_string()
        } else {
            format!("每小時第 {m} 分")
        });
    }

    let h: u32 = hour.parse().ok()?;
    if h > 23 {
        return None;
    }
    let time = format!("{h:02}:{m:02}");
    match dow {
        "*" => Some(format!("每天 {time}")),
        d => {
            // Unix crontab: 0 and 7 = Sunday, 1 = Monday … 6 = Saturday
            // (normalise_cron translates to the crate's Quartz ordinals at
            // parse time, so the raw expression reads as crontab).
            let names = ["日", "一", "二", "三", "四", "五", "六"];
            let idx: usize = d.parse().ok()?;
            if idx > 7 {
                return None;
            }
            let name = names[idx % 7];
            Some(format!("每週{name} {time}"))
        }
    }
}

/// The exact zh-TW sentence a channel user should see after a routine is
/// created — names the routine, when it runs, and where to find it.
pub(crate) fn cron_created_receipt(name: &str, cron: &str, timezone: Option<&str>) -> String {
    let when = match humanize_cron_zh(cron) {
        Some(h) => match timezone {
            Some(tz) => format!("{h}（{tz}）"),
            None => h,
        },
        None => format!("排程 {cron}"),
    };
    format!("已建立例行工作：「{name}」，{when} 執行。可在 dashboard 的「例行工作」頁查看或修改。")
}

/// Where a scheduled result will go (RM-124) — decides the receipt wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// A channel target the model named (`notify_channel` + `notify_chat_id`).
    Channel,
    /// The WebChat conversation this turn answers (gateway-attested origin).
    OriginWebchat,
    /// Nowhere — results stay in the run history only.
    Nowhere,
}

/// A resolved notify target for a new schedule / one-shot wake-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotifyTarget {
    pub channel: Option<String>,
    pub chat: Option<String>,
    pub thread: Option<String>,
}

impl NotifyTarget {
    pub(crate) fn delivery(&self) -> Delivery {
        match self.channel.as_deref() {
            Some(duduclaw_gateway::scheduled_webchat::CHANNEL) => Delivery::OriginWebchat,
            Some(_) => Delivery::Channel,
            None => Delivery::Nowhere,
        }
    }
}

/// RM-124: resolve the notify target of a new schedule.
///
/// * A target the model supplies is used as-is — except `webchat`, which is
///   refused: a WebChat conversation is only ever taken from the
///   gateway-attested `origin_env` ([`duduclaw_core::ENV_ORIGIN_SESSION`]).
/// * With no target, the origin WebChat conversation becomes the default,
///   but only when the schedule runs as the verified acting employee
///   (`acting_agent == scheduled_target`) and the session id binds to that
///   employee (`scheduled_webchat::bind_origin`: `#agent:` segment + stored
///   session row + owner tag). Anything else ⇒ no target (the receipt says
///   so).
pub(crate) fn resolve_notify_target(
    params: &Value,
    home_dir: &Path,
    acting_agent: Option<&str>,
    scheduled_target: &str,
    origin_env: Option<&str>,
) -> std::result::Result<NotifyTarget, String> {
    let field = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (channel, chat, thread) = (field("notify_channel"), field("notify_chat_id"), field("notify_thread_id"));
    duduclaw_gateway::scheduled_webchat::refuse_supplied_target(channel.as_deref(), chat.as_deref(), None)
        .map_err(str::to_string)?;
    // If one of notify_channel / notify_chat_id is set, the other must also
    // be set — a partial target would silently fail at delivery time.
    if channel.is_some() != chat.is_some() {
        return Err("notify_channel and notify_chat_id must be set together".into());
    }
    if channel.is_some() {
        return Ok(NotifyTarget { channel, chat, thread });
    }
    let origin = acting_agent
        .filter(|agent| *agent == scheduled_target)
        .and_then(|agent| duduclaw_gateway::scheduled_webchat::bind_origin(home_dir, origin_env, agent));
    Ok(match origin {
        Some(session) => NotifyTarget {
            channel: Some(duduclaw_gateway::scheduled_webchat::CHANNEL.to_string()),
            chat: Some(session),
            thread: None,
        },
        None => NotifyTarget { channel: None, chat: None, thread },
    })
}

/// Receipt tail (zh-TW + English) telling the user where results will go.
pub(crate) fn delivery_receipt_suffix(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Channel => "",
        Delivery::OriginWebchat => {
            "執行結果會回到這個 WebChat 對話，不會即時推播；重新開啟此對話即可查看。\
             Results will be posted back to this WebChat conversation. There is no live push; reopen this conversation to see them."
        }
        Delivery::Nowhere => {
            "注意：這個例行工作沒有通知管道，執行結果不會傳送到任何地方，只會記錄在 dashboard「例行工作」頁的執行紀錄。\
             若要收到結果，請在 dashboard 編輯此例行工作並設定通知管道（notify_channel 與 notify_chat_id），\
             或改在已連接的聊天通道（例如 Telegram、LINE）或 WebChat 對話中建立。\
             Note: no notification channel is set, so results will not be delivered anywhere — they are only \
             kept in the run history on the dashboard's Routines page. To receive them, edit this routine on the \
             dashboard and set a notification channel (notify_channel and notify_chat_id), or create it from a \
             connected chat channel (e.g. Telegram, LINE) or a WebChat conversation."
        }
    }
}

// ── Cron task management handlers ─────────────────────────────

/// The agent a cron row runs as — the record's owner. `"default"` is the
/// scheduler's alias for the main agent, resolved the same way
/// the cron rail resolves it at creation.
pub(crate) async fn cron_row_owner(
    home_dir: &Path,
    row: &duduclaw_gateway::cron_store::CronTaskRow,
) -> String {
    if row.agent_id == "default" {
        resolve_main_agent_name(home_dir).await
    } else {
        row.agent_id.clone()
    }
}

/// Resolve the one cron row an `id` / `name` addresses (id wins, like the
/// update path always did; a name shared by several rows is refused with the
/// candidate ids) and check that `actor` may change or trigger it
/// ([`check_record_change_allowed`] against the row's owner). Callers act on
/// the returned row's id.
pub(crate) async fn resolve_owned_cron_row(
    store: &duduclaw_gateway::cron_store::CronStore,
    home_dir: &Path,
    params: &Value,
    actor: RecordActor<'_>,
    tool: &str,
) -> std::result::Result<duduclaw_gateway::cron_store::CronTaskRow, Value> {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() && name.is_empty() {
        return Err(tool_error("Either 'id' or 'name' is required"));
    }
    let row = if !id.is_empty() {
        match store.get(id).await {
            Ok(Some(row)) => row,
            Ok(None) => return Err(tool_error(&format!("Cron task not found: {id}"))),
            Err(e) => return Err(tool_error(&format!("lookup cron task: {e}"))),
        }
    } else {
        // Names are not unique in the store. Acting on "the first row with
        // this name" would let a caller aim at a row it cannot see the owner
        // of, so an ambiguous name is refused with the candidate ids.
        let mut rows: Vec<_> = match store.list_all().await {
            Ok(all) => all.into_iter().filter(|r| r.name == name).collect(),
            Err(e) => return Err(tool_error(&format!("lookup cron task: {e}"))),
        };
        match rows.len() {
            0 => return Err(tool_error(&format!("Cron task not found: {name}"))),
            1 => rows.remove(0),
            n => {
                let ids: Vec<String> = rows
                    .iter()
                    .map(|r| format!("{} (agent: {})", r.id, r.agent_id))
                    .collect();
                return Err(tool_error(&format!(
                    "{n} cron tasks are named '{name}'; nothing was changed. Call {tool} again \
                     with `id` set to one of: {}",
                    ids.join(", ")
                )));
            }
        }
    };
    let owner = cron_row_owner(home_dir, &row).await;
    if matches!(tool, "update_cron_task" | "run_cron_task") {
        let principal = host_on_behalf_of(home_dir, actor.id());
        for origin in [principal.as_deref(), row.on_behalf_of.as_deref()] {
            duduclaw_gateway::on_behalf_of::access::check_main_target(
                home_dir, origin, &owner,
            ).await.map_err(|reason| tool_error(&reason))?;
        }
    }
    check_record_change_allowed(home_dir, actor, &owner, tool, RecordKind::Cron)
        .await
        .map_err(|reason| tool_error(&reason))?;
    Ok(row)
}

/// List cron tasks, optionally filtered by agent_id and enabled status.
///
/// When `agent_id` is omitted, returns ALL tasks (not just the calling agent's).
/// This matches dashboard behavior and allows the main agent to see sub-agent
/// cron tasks — cron jobs are system resources, not session-scoped.
pub(crate) async fn handle_list_cron_tasks(
    params: &Value,
    home_dir: &Path,
    _default_agent: &str,
) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    // Explicit agent_id filter — empty or absent means show all tasks.
    let agent_id_filter = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let enabled_only = params
        .get("enabled_only")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            params
                .get("enabled_only")
                .and_then(|v| v.as_str())
                .map(|s| s == "true")
        })
        .unwrap_or(false);

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    let all_tasks = match if enabled_only {
        store.list_enabled().await
    } else {
        store.list_all().await
    } {
        Ok(t) => t,
        Err(e) => return tool_error(&format!("list cron tasks: {e}")),
    };

    // Filter by agent_id only if explicitly provided.
    let tasks: Vec<_> = if agent_id_filter.is_empty() {
        all_tasks
    } else {
        all_tasks
            .into_iter()
            .filter(|t| t.agent_id == agent_id_filter)
            .collect()
    };

    if tasks.is_empty() {
        let scope = if agent_id_filter.is_empty() {
            "any agent".to_string()
        } else {
            format!("agent '{agent_id_filter}'")
        };
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("No cron tasks found for {scope}.")}]
        });
    }

    let mut lines = Vec::with_capacity(tasks.len() + 1);
    lines.push(format!("Found {} cron task(s):\n", tasks.len()));
    for t in &tasks {
        let status_icon = if t.enabled { "▶" } else { "⏸" };
        let last_run = t.last_run_at.as_deref().unwrap_or("never");
        let last_status = t.last_status.as_deref().unwrap_or("-");
        lines.push(format!(
            "{status_icon} [{id}] {name}\n  cron: {cron} | agent: {agent} | runs: {runs} (fail: {fail})\n  last_run: {last_run} | last_status: {last_status}\n  task: {task}\n",
            id = &t.id[..8],
            name = t.name,
            cron = t.cron,
            agent = t.agent_id,
            runs = t.run_count,
            fail = t.failure_count,
            task = {
                let t_task = truncate_bytes(&t.task, 120);
                if t_task.len() < t.task.len() { format!("{t_task}…") } else { t.task.clone() }
            },
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("")}]
    })
}

/// Update an existing cron task by ID or name. Only provided fields are changed.
///
/// The row's owner (`agent_id`, which this tool cannot change) must be the
/// caller or someone the caller may delegate to — the same predicate
/// the cron rail applies at creation, so rewriting another employee's
/// prompt cannot launder what creating it would have refused.
pub(crate) async fn handle_update_cron_task(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    if id.is_empty() && name.is_empty() {
        return tool_error("Either 'id' or 'name' is required");
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    // Resolve the existing row and check the caller may change it.
    let existing =
        match resolve_owned_cron_row(&store, home_dir, params, actor, "update_cron_task").await {
            Ok(row) => row,
            Err(refusal) => return refusal,
        };

    let notify = match recipient_notify_update(params) {
        Ok(notify) => notify,
        Err(message) => return tool_error(message),
    };
    // RM-124: a WebChat target is never model-supplied; echoing the stored
    // one back unchanged is fine.
    if let Some((channel, chat, _)) = notify {
        if let Err(message) = duduclaw_gateway::scheduled_webchat::refuse_supplied_target(
            channel, chat,
            Some((existing.notify_channel.as_deref(), existing.notify_chat_id.as_deref())),
        ) {
            return tool_error(message);
        }
    }

    // Merge provided fields over existing values.
    let new_name = params
        .get("new_name")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.name);
    let new_cron = params
        .get("cron")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.cron);
    let new_task = params
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.task);

    // A rename must respect the name cap; an unchanged (legacy) name passes.
    if new_name != existing.name {
        if let Err(message) = duduclaw_gateway::scheduled_webchat::check_name(new_name) {
            return tool_error(&message);
        }
    }

    // Validate cron expression if changed — same shared normaliser as the
    // scheduler and creation path.
    if new_cron.len() > duduclaw_gateway::cron_store::MAX_CRON_BYTES {
        return tool_error("Automation schedule exceeds 256 bytes");
    }
    if new_cron != existing.cron {
        let normalised = duduclaw_core::cron_tz::normalise_cron(new_cron);
        if normalised.parse::<cron::Schedule>().is_err() {
            return tool_error(&format!("invalid cron expression: {new_cron}"));
        }
    }

    let _admission = match admit_background_caller(home_dir, actor, "update_cron_task").await {
        Ok(guard) => guard,
        Err(reason) => return tool_error(&reason),
    };
    super::workflow_operation::effect_start();
    let principal = host_on_behalf_of(home_dir, actor.id());
    let updated = store.update_fields_with_principal(
        &existing.id, new_name, &existing.agent_id, new_cron, new_task, existing.enabled,
        notify, principal.as_deref(),
    ).await;
    match updated {
        Ok(true) => {
            if let Ok(Some(row)) = store.get(&existing.id).await {
                if row.name == new_name
                    && row.cron == new_cron
                    && row.task == new_task
                    && row.agent_id == existing.agent_id
                    && row.enabled == existing.enabled
                    && row.on_behalf_of.as_deref() == principal.as_deref().or(existing.on_behalf_of.as_deref())
                    && notify.is_none_or(|(channel, chat, thread)| {
                        row.notify_channel.as_deref() == channel
                            && row.notify_chat_id.as_deref() == chat
                            && row.notify_thread_id.as_deref() == thread
                    })
                {
                    let observed = serde_json::to_value(&row).unwrap_or(Value::Null);
                    super::workflow_operation::effect_committed(
                        serde_json::json!({
                            "adapter": "update_cron_task",
                            "adapter_version": 1,
                            "cron_id": existing.id,
                            "row_hash": duduclaw_gateway::approval::payload_hash(&observed),
                            "row": observed
                        }),
                    );
                }
            }
            serde_json::json!({"content":[{
                "type": "text",
                "text": format!("Cron task '{}' updated (id: {}).", new_name, duduclaw_core::truncate_chars(&existing.id, 8))
            }]})
        }
        Ok(false) => tool_error("update returned no rows changed"),
        Err(e) => tool_error(&format!("update cron task: {e}")),
    }
}

/// A routing edit supplies the whole pair so a partial patch cannot borrow
/// an unchecked destination from the stored row. An empty pair clears routing.
fn recipient_notify_update(params: &Value) -> std::result::Result<Option<(Option<&str>, Option<&str>, Option<&str>)>, &'static str> {
    if !["notify_channel", "notify_chat_id", "notify_thread_id"].iter().any(|key| params.get(key).is_some()) {
        return Ok(None);
    }
    if params.get("notify_channel").is_none() || params.get("notify_chat_id").is_none() {
        return Err("notify_channel and notify_chat_id must be supplied together");
    }
    let field = |key: &str| -> std::result::Result<Option<&str>, &'static str> {
        match params.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok((!value.is_empty()).then_some(value.as_str())),
            _ => Err("notify fields must be strings or null"),
        }
    };
    let channel = field("notify_channel")?;
    let chat = field("notify_chat_id")?;
    let thread = field("notify_thread_id")?;
    if channel.is_some() != chat.is_some() || (thread.is_some() && channel != Some("discord")) {
        return Err("notify target requires a channel/chat pair; threads require Discord");
    }
    Ok(Some((channel, chat, thread)))
}

#[cfg(test)]
#[test]
fn recipient_notify_updates_require_a_complete_target() {
    assert_eq!(recipient_notify_update(&serde_json::json!({})).unwrap(), None);
    for args in [serde_json::json!({"notify_thread_id":"999"}),
        serde_json::json!({"notify_channel":"line"}),
        serde_json::json!({"notify_channel":"line","notify_chat_id":"U123","notify_thread_id":"999"}),
        serde_json::json!({"notify_channel":42,"notify_chat_id":"U123"})] {
        assert!(recipient_notify_update(&args).is_err());
    }
    assert_eq!(recipient_notify_update(&serde_json::json!({"notify_channel":"discord","notify_chat_id":"123","notify_thread_id":"456"})).unwrap(),
        Some((Some("discord"), Some("123"), Some("456"))));
    assert_eq!(recipient_notify_update(&serde_json::json!({"notify_channel":null,"notify_chat_id":null})).unwrap(), Some((None, None, None)));
}

#[cfg(test)]
mod origin_target_tests {
    use super::*;

    /// A signed-in dashboard account's WebChat session answered by `agent`.
    async fn origin_session(home: &Path, agent: &str) -> String {
        let user = "dashboard-user-1";
        let tag = duduclaw_gateway::webchat::webchat_owner_tag(user);
        let session = format!("webchat:webchat:{tag}:c0ffee#agent:{agent}#conv:n1");
        let manager = duduclaw_gateway::session::SessionManager::new(&home.join("sessions.db")).unwrap();
        manager
            .get_or_create_with_origin(&session, agent, &duduclaw_gateway::session_origin::SessionOrigin::webchat_user(user))
            .await
            .unwrap();
        session
    }

    // Not run here: builds and tests are not executed in this worktree.
    #[tokio::test]
    async fn no_target_defaults_to_the_origin_webchat_conversation() {
        let home = tempfile::tempdir().unwrap();
        let session = origin_session(home.path(), "sales").await;
        let target = resolve_notify_target(&serde_json::json!({}), home.path(), Some("sales"), "sales", Some(&session))
            .unwrap();
        assert_eq!(target.channel.as_deref(), Some("webchat"));
        // The stored target is the session id stamped with its creation time.
        let chat = target.chat.as_deref().unwrap();
        assert!(chat.starts_with(session.as_str()) && chat.len() > session.len());
        assert_eq!(target.thread, None);
        assert_eq!(target.delivery(), Delivery::OriginWebchat);
        assert!(delivery_receipt_suffix(Delivery::OriginWebchat).contains("WebChat 對話"));
        // A channel the model names wins over the origin.
        let named = resolve_notify_target(
            &serde_json::json!({"notify_channel":"line","notify_chat_id":"U1"}),
            home.path(), Some("sales"), "sales", Some(&session),
        ).unwrap();
        assert_eq!((named.channel.as_deref(), named.chat.as_deref()), (Some("line"), Some("U1")));
        assert_eq!(named.delivery(), Delivery::Channel);
    }

    #[tokio::test]
    async fn schedule_names_over_the_cap_are_rejected_on_create_and_rename() {
        let home = tempfile::tempdir().unwrap();
        let long = "n".repeat(duduclaw_gateway::scheduled_webchat::MAX_NAME_CHARS + 1);
        let created = handle_schedule_task(
            &serde_json::json!({"cron":"0 9 * * *","task":"t","name":long}), home.path(), "sales",
        ).await;
        assert_eq!(created["isError"], true);
        assert!(created["content"][0]["text"].as_str().unwrap().contains("too long"));
    }

    #[tokio::test]
    async fn model_supplied_webchat_target_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let session = origin_session(home.path(), "sales").await;
        for args in [
            serde_json::json!({"notify_channel":"webchat","notify_chat_id": session}),
            serde_json::json!({"notify_channel":"WebChat","notify_chat_id":"webchat:webchat:x"}),
        ] {
            assert!(resolve_notify_target(&args, home.path(), Some("sales"), "sales", Some(&session)).is_err(), "{args}");
        }
        // Partial targets are still refused.
        assert!(resolve_notify_target(&serde_json::json!({"notify_channel":"line"}), home.path(), Some("sales"), "sales", None).is_err());
    }

    #[tokio::test]
    async fn origin_bound_to_another_employee_is_ignored() {
        let home = tempfile::tempdir().unwrap();
        let session = origin_session(home.path(), "sales").await;
        // Acting employee differs from the session's `#agent:` / stored owner.
        let other = resolve_notify_target(&serde_json::json!({}), home.path(), Some("hr"), "hr", Some(&session)).unwrap();
        assert_eq!(other.delivery(), Delivery::Nowhere);
        // Scheduling for someone else never borrows the caller's conversation.
        let delegated = resolve_notify_target(&serde_json::json!({}), home.path(), Some("sales"), "hr", Some(&session)).unwrap();
        assert_eq!(delegated.delivery(), Delivery::Nowhere);
        // Operator callers (no acting employee) and absent / unknown origins.
        assert_eq!(resolve_notify_target(&serde_json::json!({}), home.path(), None, "sales", Some(&session)).unwrap().delivery(), Delivery::Nowhere);
        assert_eq!(resolve_notify_target(&serde_json::json!({}), home.path(), Some("sales"), "sales", None).unwrap().delivery(), Delivery::Nowhere);
        let unknown = session.replace("c0ffee", "beef");
        assert_eq!(resolve_notify_target(&serde_json::json!({}), home.path(), Some("sales"), "sales", Some(&unknown)).unwrap().delivery(), Delivery::Nowhere);
        let receipt = delivery_receipt_suffix(Delivery::Nowhere);
        assert!(receipt.contains("不會傳送到任何地方") && receipt.contains("will not be delivered anywhere"));
        assert!(receipt.contains("notify_channel"));
    }
}

/// Delete a cron task by ID or name.
///
/// Resolves exactly one row (id first, then a unique name) and deletes that
/// row by id after the owner check.
pub(crate) async fn handle_delete_cron_task(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    if id.is_empty() && name.is_empty() {
        return tool_error("Either 'id' or 'name' is required");
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    let row =
        match resolve_owned_cron_row(&store, home_dir, params, actor, "delete_cron_task").await {
            Ok(row) => row,
            Err(refusal) => return refusal,
        };
    let key = if !id.is_empty() { id } else { name };

    match store.delete(&row.id).await {
        Ok(true) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Cron task '{key}' deleted.")}]
        }),
        Ok(false) => tool_error(&format!("Cron task not found: {key}")),
        Err(e) => tool_error(&format!("delete cron task: {e}")),
    }
}

/// Pause or resume a cron task by ID or name (one resolved row, owner-checked).
pub(crate) async fn handle_pause_cron_task(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            params
                .get("enabled")
                .and_then(|v| v.as_str())
                .map(|s| s == "true")
        })
        .unwrap_or(false);

    if id.is_empty() && name.is_empty() {
        return tool_error("Either 'id' or 'name' is required");
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    let row = match resolve_owned_cron_row(&store, home_dir, params, actor, "pause_cron_task").await
    {
        Ok(row) => row,
        Err(refusal) => return refusal,
    };
    let key = if !id.is_empty() { id } else { name };

    let principal = if enabled { host_on_behalf_of(home_dir, actor.id()) } else { None };
    if enabled {
        let owner = cron_row_owner(home_dir, &row).await;
        for origin in [principal.as_deref(), row.on_behalf_of.as_deref()] {
            if let Err(reason) = duduclaw_gateway::on_behalf_of::access::check_main_target(
                home_dir, origin, &owner,
            ).await {
                return tool_error(&reason);
            }
        }
    }
    let action = if enabled { "resumed" } else { "paused" };
    let _admission = match admit_background_caller(home_dir, actor, "pause_cron_task").await {
        Ok(guard) => guard,
        Err(reason) => return tool_error(&reason),
    };
    match store.set_enabled_with_principal(&row.id, enabled, principal.as_deref()).await {
        Ok(true) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Cron task '{key}' {action}.")}]
        }),
        Ok(false) => tool_error(&format!("Cron task not found: {key}")),
        Err(e) => tool_error(&format!("{action} cron task: {e}")),
    }
}

// ── Reminder handlers ─────���──────────────────────────────────

/// Run a scheduled cron task once, immediately ("test execution"), by ID or
/// name. Delegates to the gateway's standalone runner which drives the SAME
/// dispatch path a scheduled fire uses (trigger gate + execute + run history),
/// blocking until the run completes and returning the recorded outcome.
///
/// The run fires the row as its owner, so triggering another employee's
/// routine needs the same owner check as changing it; the resolved row is run
/// by id.
pub(crate) async fn handle_run_cron_task(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    let store = match duduclaw_gateway::cron_store::CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };
    let row = match resolve_owned_cron_row(&store, home_dir, params, actor, "run_cron_task").await {
        Ok(row) => row,
        Err(refusal) => return refusal,
    };
    // Release the store before the (long, blocking) run opens its own.
    drop(store);

    // Fast refusal here; the runtime re-admits the caller and target at handoff.
    match admit_background_caller(home_dir, actor, "run_cron_task").await {
        Ok(guard) => drop(guard),
        Err(reason) => return tool_error(&reason),
    }
    let principal = host_on_behalf_of(home_dir, actor.id());
    match duduclaw_gateway::on_behalf_of::scope_first(principal,
        duduclaw_gateway::cron_scheduler::run_cron_task_now_for_caller(
            home_dir, &row.id, actor.agent(),
        ),
    ).await {
        Ok(summary) => serde_json::json!({
            "content": [{"type": "text", "text": summary}]
        }),
        Err(duduclaw_gateway::cron_scheduler::ManualCronError::Autonomy(pause)) => {
            tool_error(&background_caller_refusal("run_cron_task", &pause))
        }
        Err(duduclaw_gateway::cron_scheduler::ManualCronError::Other(error)) => {
            tool_error(&format!("run cron task: {error}"))
        }
    }
}
