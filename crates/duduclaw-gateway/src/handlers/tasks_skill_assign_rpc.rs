//! `tasks.skill_assign` — the 交辦任務 page's 「一般」 tab: hand a skill to the
//! AI employee who last ran it, as an autonomous goal.
//!
//! Params: `skill_name` (required), `note` (optional, one line, at most
//! [`MAX_NOTE_CHARS`]), `attachments` (optional, see `task_attachments`).
//!
//! The server, never the browser, decides who runs it:
//!
//! 1. the skill is read with the same loader as `skills.list` (company-wide,
//!    department and own layers, per-employee switches, usage index), over the
//!    employees the caller may see there (Viewer or above);
//! 2. holders that have it switched off, or whose copy failed the install
//!    scan, are out;
//! 3. the runner is the remaining holder with the latest `last_used_at`; if
//!    none of them ever ran it, the first one by employee id (the order
//!    `skills.list` returns).
//!
//! The caller then needs the same right `tasks.goal_create` asks for on that
//! runner (`can_assign_to`: Admin, Operator binding, or a team grant that
//! derived Operator). A runner the caller may not assign to is refused, not
//! swapped for another employee, so the page's 「由 X 執行」 line is always
//! the employee that would run it.
//!
//! The goal goes through `goal_create_core` like every other goal. Its
//! description is a short readable header (skill, note, attachment paths)
//! followed by the skill's full text, because a goal round's prompt is the
//! description: the runner's system prompt only carries skills on some
//! runtimes and prompt modes, and then under a byte budget.

#[allow(unused_imports)]
use super::*;

use super::skills_list_rpc::SkillHolding;
use super::task_attachments::{
    attachments_block, check_block_fits, decode_task_attachments, named, remove_saved,
    save_task_attachments, SavedAttachment, DESCRIPTION_MAX_CHARS,
};

/// Longest `note` accepted (characters).
pub(crate) const MAX_NOTE_CHARS: usize = 500;

/// `trigger_signal` of the `skill_activate` row a 一般-tab hand-over writes.
pub(crate) const SKILL_ASSIGN_TRIGGER: &str = "skill_assign";

/// Pick the runner among `holdings` (already in registry order): the enabled,
/// scan-clean holder that ran the skill most recently, else the first enabled,
/// scan-clean holder. Ties on the timestamp keep registry order.
pub(crate) fn pick_skill_runner(holdings: &[SkillHolding]) -> Option<&SkillHolding> {
    let eligible = || holdings.iter().filter(|h| h.enabled && !h.security_failed);
    let mut best: Option<&SkillHolding> = None;
    for h in eligible() {
        let Some(t) = h.last_used_at.as_deref() else {
            continue;
        };
        if best
            .and_then(|b| b.last_used_at.as_deref())
            .is_none_or(|bt| t > bt)
        {
            best = Some(h);
        }
    }
    best.or_else(|| eligible().next())
}

/// Display name, falling back to the id.
fn label<'a>(display: &'a str, id: &'a str) -> &'a str {
    if display.trim().is_empty() {
        id
    } else {
        display.trim()
    }
}

/// The goal text: header first (it becomes the task title), the skill's full
/// text last. When the whole thing does not fit the description cap the
/// inline copy is shortened and `full_copy` (the complete text saved as a
/// file) is named instead.
pub(crate) fn skill_goal_description(
    skill_name: &str,
    skill: &SkillHolding,
    note: &str,
    attachment_paths: &[SavedAttachment],
    full_copy: Option<&std::path::Path>,
) -> String {
    let shown = header_safe(&label(&skill.display_name, skill_name));
    let id = header_safe(skill_name);
    let mut head = if note.is_empty() {
        format!("執行技能「{shown}」")
    } else {
        format!("技能「{shown}」：{note}")
    };
    head.push_str(&format!("\n技能 ID：{id}"));
    if !attachment_paths.is_empty() {
        head.push('\n');
        head.push_str(&attachments_block(attachment_paths));
    }
    let intro = match full_copy {
        None => format!("【技能指示】這項任務要照技能「{shown}」做，以下是技能全文："),
        Some(p) => format!(
            "【技能指示】這項任務要照技能「{shown}」做。技能全文太長，下面只放開頭，\
             請先完整讀取這個檔案再開始：{}",
            p.display()
        ),
    };
    let open = format!("<skill name=\"{id}\">\n");
    let close = "\n</skill>";
    let fixed = head.chars().count()
        + 2
        + intro.chars().count()
        + 1
        + open.chars().count()
        + close.chars().count();
    let room = DESCRIPTION_MAX_CHARS.saturating_sub(fixed);
    let body = duduclaw_core::truncate_chars(skill.content.trim(), room);
    format!("{head}\n\n{intro}\n{open}{body}{close}")
}

/// A skill name or display name as it may appear in the header and the
/// `<skill name="…">` wrapper: one line, without the quote and angle brackets
/// that could close the wrapper early. Names on disk may contain them.
fn header_safe(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '"' | '<' | '>' => '_',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

/// Whether the full skill text fits inline next to the header.
fn fits_inline(
    skill_name: &str,
    skill: &SkillHolding,
    note: &str,
    paths: &[SavedAttachment],
) -> bool {
    let d = skill_goal_description(skill_name, skill, note, paths, None);
    d.contains(skill.content.trim()) && d.chars().count() <= DESCRIPTION_MAX_CHARS
}

/// Acceptance criteria: the skill's own promise plus the note.
pub(crate) fn skill_goal_criteria(skill_name: &str, skill: &SkillHolding, note: &str) -> String {
    let shown = label(&skill.display_name, skill_name);
    let desc = skill.description.trim();
    let mut c = if desc.is_empty() {
        format!("依照技能「{shown}」的指示完成這項任務。")
    } else {
        format!("依照技能「{shown}」的指示完成這項任務：{desc}")
    };
    if !note.is_empty() {
        c.push_str(&format!("\n補充要求：{note}"));
    }
    c
}

impl MethodHandler {
    pub(crate) async fn handle_tasks_skill_assign(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let skill_name = params
            .get("skill_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if duduclaw_agent::skill_state::validate_skill_name(skill_name).is_err() {
            return WsFrame::error_response("", "valid skill_name is required");
        }
        let note = match params.get("note") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.split_whitespace().collect::<Vec<_>>().join(" "),
            Some(_) => return WsFrame::error_response("", "note must be text"),
        };
        if note.chars().count() > MAX_NOTE_CHARS {
            return WsFrame::error_response(
                "",
                &format!("note_too_long: at most {MAX_NOTE_CHARS} characters"),
            );
        }
        let fresh =
            match crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx) {
                Ok(f) => f,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
        let (holdings, usage_error) = self.skill_holdings(&fresh, skill_name).await;
        if let Some(e) = usage_error {
            warn!(skill = skill_name, error = %e, "skill_assign: usage unavailable, runner falls back to the first holder");
        }
        if holdings.is_empty() {
            return WsFrame::error_response(
                "",
                "skill_not_found: no AI employee you can see has this skill",
            );
        }
        let Some(runner) = pick_skill_runner(&holdings).cloned() else {
            return WsFrame::error_response(
                "",
                "skill_not_enabled: no AI employee has this skill switched on",
            );
        };
        // The same right `tasks.goal_create` asks for, on the runner.
        if !ctx.can_assign_to(&runner.agent_id) {
            return WsFrame::error_response("", "permission denied");
        }
        // Files are decoded only once the caller may assign to the runner,
        // and all of them are checked before anything is written.
        let files = match decode_task_attachments(&params) {
            Ok(f) => f,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let mut saved = match save_task_attachments(&self.home_dir, &runner.agent_id, &files).await
        {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let attachment_paths = named(&files, &saved);
        if let Err(e) = check_block_fits(&attachment_paths) {
            remove_saved(&saved).await;
            return WsFrame::error_response("", &e);
        }
        let full_copy = if fits_inline(skill_name, &runner, &note, &attachment_paths) {
            None
        } else {
            let base = crate::media::agent_attachment_base(&self.home_dir, &runner.agent_id);
            match crate::media::save_attachment_in_base_untracked(
                &base,
                runner.content.as_bytes(),
                &format!("skill-{skill_name}.md"),
            )
            .await
            {
                Ok(p) => {
                    saved.push(p.clone());
                    Some(p)
                }
                Err(e) => {
                    remove_saved(&saved).await;
                    return WsFrame::error_response("", &format!("attachments_save_failed: {e}"));
                }
            }
        };
        let req = crate::goal_create_core::GoalCreateRequest {
            agent_id: runner.agent_id.clone(),
            created_by: "goal:dashboard".to_string(),
            description: skill_goal_description(
                skill_name,
                &runner,
                &note,
                &attachment_paths,
                full_copy.as_deref(),
            ),
            acceptance_criteria: Some(skill_goal_criteria(skill_name, &runner, &note)),
            priority: String::new(),
            outcome: None,
            duration_hours: None,
            cost_cap_cents: None,
            timeout_secs: None,
            risk_boundary: None,
            require_beliefs: false,
            plan_first: false,
            parent_task_id: None,
            source_label: "儀表板".to_string(),
            on_behalf_of: crate::trigger_route::assign::principal_for_user(&ctx.user_id),
            created_via: Some("dashboard".to_string()),
            source: None,
            task_id: None,
            grant_via: crate::trigger_route::assign::dashboard_grant_via(ctx, &runner.agent_id),
        };
        let created =
            match crate::goal_create_core::create_goal_task(&self.home_dir, &store, req).await {
                Ok(c) => c,
                Err(e) => {
                    remove_saved(&saved).await;
                    return WsFrame::error_response("", &e);
                }
            };
        // A skill handed over from the 一般 tab counts as a use: the tab
        // orders skills by `last_used_at`, which only `skill_activate` rows
        // feed. A distinct trigger keeps these apart from the evolution
        // loop's own activations.
        // The global emitter writes where `skills.list` reads usage from
        // (`resolve_default_dir`, from the environment). Only write when that
        // is this handler's own home, so a handler built over another
        // directory (tests) never appends to the operator's real audit log.
        if crate::evolution_events::logger::resolve_default_dir().starts_with(&self.home_dir) {
            crate::evolution_events::emitter::EvolutionEventEmitter::global().emit_skill_activate(
                &runner.agent_id,
                skill_name,
                SKILL_ASSIGN_TRIGGER,
            );
        }
        let cap = crate::goal_loop::GoalLoopConfig::from_home(&self.home_dir).iteration_cap;
        let dispatch_enabled = crate::dispatch_engine::dispatch_engine_enabled(&self.home_dir);
        WsFrame::ok_response(
            "",
            json!({
                "task": task_row_to_json(&created.task),
                "iteration_cap": cap,
                "dispatch_enabled": dispatch_enabled,
                "plan_first": created.plan_first,
                "attachments_saved": attachment_paths.len(),
                "runner": {
                    "agent_id": runner.agent_id,
                    "display_name": runner.agent_display_name,
                },
                "skill": {
                    "name": skill_name,
                    "display_name": label(&runner.display_name, skill_name),
                },
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_names_cannot_close_the_skill_wrapper() {
        assert_eq!(header_safe("x\">\n</skill>"), "x__ _/skill_");
        assert_eq!(header_safe("報價整理"), "報價整理");
    }

    fn holding(agent: &str, enabled: bool, last: Option<&str>) -> SkillHolding {
        SkillHolding {
            agent_id: agent.to_string(),
            agent_display_name: agent.to_uppercase(),
            enabled,
            last_used_at: last.map(str::to_string),
            display_name: "報價單".to_string(),
            description: "產出一份報價單".to_string(),
            content: "---\nname: quote\n---\n照公司格式做報價單。".to_string(),
            security_failed: false,
        }
    }

    #[test]
    fn the_most_recent_enabled_runner_wins() {
        let h = vec![
            holding("a", true, Some("2026-10-01T00:00:00Z")),
            holding("b", true, Some("2026-10-03T00:00:00Z")),
            holding("c", false, Some("2026-10-05T00:00:00Z")),
            holding("d", true, None),
        ];
        assert_eq!(
            pick_skill_runner(&h).unwrap().agent_id,
            "b",
            "c is switched off"
        );
    }

    #[test]
    fn never_run_falls_back_to_the_first_enabled_holder() {
        let h = vec![
            holding("a", false, None),
            holding("b", true, None),
            holding("c", true, None),
        ];
        assert_eq!(pick_skill_runner(&h).unwrap().agent_id, "b");
    }

    #[test]
    fn nobody_enabled_means_no_runner() {
        assert!(pick_skill_runner(&[holding("a", false, Some("2026-10-01T00:00:00Z"))]).is_none());
        assert!(pick_skill_runner(&[]).is_none());
    }

    #[test]
    fn a_scan_failed_copy_is_never_picked() {
        let mut bad = holding("a", true, Some("2026-10-09T00:00:00Z"));
        bad.security_failed = true;
        let h = vec![bad, holding("b", true, None)];
        assert_eq!(pick_skill_runner(&h).unwrap().agent_id, "b");
    }

    #[test]
    fn ties_keep_registry_order() {
        let t = Some("2026-10-01T00:00:00Z");
        let h = vec![holding("a", true, t), holding("b", true, t)];
        assert_eq!(pick_skill_runner(&h).unwrap().agent_id, "a");
    }

    #[test]
    fn description_has_a_readable_header_and_the_full_skill() {
        let s = holding("a", true, None);
        let paths = vec![(
            "需求.pdf".to_string(),
            std::path::PathBuf::from("/h/agents/a/attachments/1_x.pdf"),
        )];
        let d = skill_goal_description("quote", &s, "給王先生", &paths, None);
        assert!(
            d.starts_with("技能「報價單」：給王先生\n技能 ID：quote"),
            "{d}"
        );
        assert!(d.contains("/h/agents/a/attachments/1_x.pdf"));
        assert!(d.contains(
            "<skill name=\"quote\">\n---\nname: quote\n---\n照公司格式做報價單。\n</skill>"
        ));
        assert!(fits_inline("quote", &s, "給王先生", &paths));
        let bare = skill_goal_description("quote", &s, "", &[], None);
        assert!(bare.starts_with("執行技能「報價單」\n"), "{bare}");
    }

    #[test]
    fn a_long_skill_is_shortened_and_points_at_the_full_copy() {
        let mut s = holding("a", true, None);
        s.content = "步驟".repeat(3000);
        assert!(!fits_inline("quote", &s, "", &[]));
        let copy = std::path::PathBuf::from("/h/agents/a/attachments/1_skill-quote.md");
        let d = skill_goal_description("quote", &s, "", &[], Some(&copy));
        assert!(d.chars().count() <= DESCRIPTION_MAX_CHARS);
        assert!(d.contains("/h/agents/a/attachments/1_skill-quote.md"));
        assert!(d.ends_with("</skill>"));
    }

    #[test]
    fn criteria_carry_the_skill_promise_and_the_note() {
        let s = holding("a", true, None);
        assert_eq!(
            skill_goal_criteria("quote", &s, "三天內"),
            "依照技能「報價單」的指示完成這項任務：產出一份報價單\n補充要求：三天內"
        );
        let mut plain = s.clone();
        plain.description.clear();
        plain.display_name.clear();
        assert_eq!(
            skill_goal_criteria("quote", &plain, ""),
            "依照技能「quote」的指示完成這項任務。"
        );
    }
}
