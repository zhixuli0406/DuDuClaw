//! Odoo → decision-twin pilot export (X1 方案 4).
//!
//! `duduclaw-odoo::support_export` owns the mapping; this module owns the two
//! things that mapping cannot reach from inside the odoo crate:
//!
//! 1. **Per-agent credentials.** The connector is built from the agent's own
//!    `agent.toml [odoo]` override merged over the global `config.toml [odoo]`,
//!    with the same precedence the `OdooConnectorPool` uses (override wins
//!    field by field, api-key preferred over password). No new global
//!    credential path is introduced — the decryption goes through the existing
//!    [`crate::config_crypto::decrypt_config_field_async`], so a `secret://`
//!    reference resolves exactly as it does everywhere else and is never sent
//!    to Odoo as a literal.
//! 2. **The pilot contract type.** `SupportPilotExport` lives in this crate,
//!    so the final assembly — including the canonical `source_version_hashes`
//!    digest the importer recomputes — happens here.
//!
//! The result is a *file*, not an import: the operator reviews it and feeds it
//! to `/api/decision/import-pilot` (or `duduclaw decision-import-pilot`)
//! deliberately. An adapter that both reads a customer's ERP and silently
//! commits a pilot would be exactly the "passed a screen ⇒ promoted" move all
//! three specs forbid.

use std::path::Path;

use duduclaw_odoo::support_export::{
    SupportModel, SupportQueueExtract, build_extract, fetch_rows, folded_stage_ids, model_available,
};
use duduclaw_odoo::{AgentOdooConfig, OdooConfig, OdooConnector};
use serde::{Deserialize, Serialize};

use crate::decision_ingest::{DailyStaffing, SupportPilotExport, TicketEvent};
use crate::decision_task_board_export::canonical_source_sha256;

/// Lineage prefix stamped on every Odoo-derived pilot, so the dashboard can
/// tell it from a synthetic demo the same way it tells a task-board pilot.
pub const LINEAGE_PREFIX: &str = "odoo-export@";

#[derive(Debug, Clone, Deserialize)]
pub struct OdooExportRequest {
    /// `helpdesk.ticket` or `project.task`.
    pub model: String,
    /// Helpdesk team id, or project id.
    pub queue: i64,
    pub since_utc: String,
    pub until_utc: String,
    pub horizon_days: usize,
}

/// The export plus the provenance an import needs.
#[derive(Debug, Clone, Serialize)]
pub struct OdooPilotExport {
    pub export: SupportPilotExport,
    pub queue_id: String,
    pub source_lineage: String,
    pub contributing_user_ids: Vec<i64>,
    pub skipped_rows: usize,
    /// Always present — the staffing series is a proxy, and the reader has to
    /// be told before they read a number off it.
    pub limitations: Vec<String>,
}

fn limitations() -> Vec<String> {
    vec![
        "staffing[].agents counts distinct Odoo assignees active that day — a proxy for staffed capacity, not a roster".into(),
        "fixed_extra_capacity is always 0: Odoo carries no such lane".into(),
        "project.task closure uses date_end, falling back to date_last_stage_update only inside a folded (fold = true) stage".into(),
        "Reopened or transferred rows need an upstream normalization rule: this adapter assumes one creation and at most one closure per id".into(),
    ]
}

/// Merge the global `[odoo]` block with an agent's `agent.toml [odoo]`
/// override and resolve the credential — the gateway-side twin of
/// `OdooConnectorPool::merge_credentials`.
async fn connect_for_agent(
    home_dir: &Path,
    agent_id: &str,
    profile: Option<&str>,
) -> Result<OdooConnector, String> {
    if agent_id.trim().is_empty() {
        return Err("agent id is required".into());
    }
    let global_raw = std::fs::read_to_string(home_dir.join("config.toml"))
        .map_err(|_| "config.toml is unreadable".to_string())?;
    let global_table = global_raw
        .parse::<toml::Table>()
        .map_err(|_| "config.toml is malformed".to_string())?;
    let mut config = OdooConfig::from_toml(&global_table);

    let agent_toml_path =
        crate::outcome_spec::agent_work_dir(home_dir, agent_id).join("agent.toml");
    let agent_raw = std::fs::read_to_string(&agent_toml_path).unwrap_or_default();
    let agent_override = AgentOdooConfig::from_agent_toml(&agent_raw);
    if let (Some(requested), Some(cfg)) = (profile, agent_override.as_ref()) {
        // An explicit `--profile` must name the agent's actual profile; it is
        // an assertion by the caller, not a selector that silently falls back
        // to somebody else's credentials.
        if cfg.profile_or_default() != requested {
            return Err("requested profile does not match this agent's [odoo] profile".into());
        }
    }
    if let Some(cfg) = agent_override.as_ref() {
        if let Some(username) = &cfg.username {
            config.username = username.clone();
        }
        if let Some(key) = &cfg.api_key_enc {
            config.api_key_enc = key.clone();
        }
        if let Some(password) = &cfg.password_enc {
            config.password_enc = password.clone();
        }
    }

    // Resolve the credential from whichever file actually supplied it, so an
    // agent override with its own `api_key_enc` is decrypted against the
    // agent's own table (and its `secret://` reference resolved) rather than
    // the global one.
    let agent_table = agent_raw.parse::<toml::Table>().unwrap_or_default();
    let override_has_key = agent_override
        .as_ref()
        .is_some_and(|c| c.api_key_enc.is_some() || c.password_enc.is_some());
    let (table, field) = if override_has_key {
        let field = if agent_override
            .as_ref()
            .and_then(|c| c.api_key_enc.as_ref())
            .is_some()
        {
            "api_key"
        } else {
            "password"
        };
        (&agent_table, field)
    } else if !config.api_key_enc.is_empty() {
        (&global_table, "api_key")
    } else if !config.password_enc.is_empty() {
        (&global_table, "password")
    } else {
        return Err("no Odoo credential configured for this agent".into());
    };
    let credential =
        crate::config_crypto::decrypt_config_field_async(table, "odoo", field, home_dir)
            .await
            .ok_or_else(|| "Odoo credential could not be resolved".to_string())?;
    let mut connector = OdooConnector::connect(&config, credential.expose()).await?;
    if let Some(cfg) = agent_override.as_ref() {
        if !cfg.company_ids.is_empty() {
            connector =
                connector.with_company_ids(cfg.company_ids.iter().map(|&id| id as i64).collect());
        }
    }
    Ok(connector)
}

/// Wrap a queue extract in the pilot contract type, computing the canonical
/// source digest the importer will recompute.
pub fn assemble_export(
    extract: &SupportQueueExtract,
    seed: u64,
) -> Result<SupportPilotExport, String> {
    let tickets: Vec<TicketEvent> = extract
        .tickets
        .iter()
        .map(|t| TicketEvent {
            queue_id: t.queue_id.clone(),
            ticket_id: t.ticket_id.clone(),
            created_at_utc: t.created_at_utc.clone(),
            resolved_at_utc: t.resolved_at_utc.clone(),
        })
        .collect();
    let staffing: Vec<DailyStaffing> = extract
        .staffing
        .iter()
        .map(|s| DailyStaffing {
            queue_id: s.queue_id.clone(),
            day_utc: s.day_utc.clone(),
            agents: s.agents,
            fixed_extra_capacity: s.fixed_extra_capacity,
        })
        .collect();
    let digest = canonical_source_sha256(&tickets, &staffing).map_err(|e| e.to_string())?;
    let window_tag = extract
        .window_start_utc
        .split('T')
        .next()
        .unwrap_or("window")
        .replace('-', "");
    // `:` is legal in the 128-char selector the importer validates, and the
    // queue id already carries the model — so the snapshot id stays unique
    // per (queue, window) without a second lookup table.
    Ok(SupportPilotExport {
        snapshot_id: format!("{}-{window_tag}", extract.queue_id),
        baseline_scenario_id: format!("{}-baseline-{window_tag}", extract.queue_id),
        window_start_utc: extract.window_start_utc.clone(),
        data_cutoff_utc: extract.data_cutoff_utc.clone(),
        source_version_hashes: vec![digest],
        seed,
        horizon_days: extract.horizon_days,
        tickets,
        staffing,
    })
}

/// End to end: connect as `agent_id`, page through the queue, and assemble a
/// contract-legal pilot export.
pub async fn export_for_agent(
    home_dir: &Path,
    agent_id: &str,
    profile: Option<&str>,
    request: &OdooExportRequest,
) -> Result<OdooPilotExport, String> {
    let model = SupportModel::parse(&request.model)
        .ok_or_else(|| "model must be helpdesk.ticket or project.task".to_string())?;
    if !(1..=366).contains(&request.horizon_days) {
        return Err("horizon_days must be 1..=366".into());
    }
    let connector = connect_for_agent(home_dir, agent_id, profile).await?;
    if !model_available(&connector, model) {
        return Err(format!(
            "{} is not installed on this Odoo instance",
            model.model_name()
        ));
    }
    let rows = fetch_rows(
        &connector,
        model,
        request.queue,
        &request.since_utc,
        &request.until_utc,
    )
    .await
    .map_err(|e| e.to_string())?;
    let folded = folded_stage_ids(&connector, model).await;
    let extract = build_extract(
        &rows,
        model,
        request.queue,
        &request.since_utc,
        request.horizon_days,
        &folded,
    )
    .map_err(|e| e.to_string())?;
    let export = assemble_export(&extract, 0)?;
    Ok(OdooPilotExport {
        queue_id: extract.queue_id.clone(),
        source_lineage: format!("{LINEAGE_PREFIX}{}", export.source_version_hashes[0]),
        contributing_user_ids: extract.contributing_user_ids.clone(),
        skipped_rows: extract.skipped_rows,
        export,
        limitations: limitations(),
    })
}

/// Whether a stored pilot's `source_lineage` came from this adapter.
/// Anchored, per CLAUDE.md coding convention 2.
pub fn is_odoo_lineage(lineage: &str) -> bool {
    lineage.starts_with(LINEAGE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn helpdesk_rows() -> Vec<serde_json::Value> {
        (1..=20_i64)
            .map(|i| {
                json!({
                    "id": i,
                    "create_date": format!("2026-09-{:02} 08:00:00", (i % 14) + 1),
                    "close_date": if i % 3 == 0 {
                        json!(format!("2026-09-{:02} 18:00:00", (i % 14) + 1))
                    } else {
                        json!(false)
                    },
                    "team_id": [7, "Support"],
                    "user_id": [5 + (i % 3), "Agent"],
                })
            })
            .collect()
    }

    #[test]
    fn assembled_export_satisfies_the_pilot_contract_and_round_trips() {
        let extract = build_extract(
            &helpdesk_rows(),
            SupportModel::HelpdeskTicket,
            7,
            "2026-09-01T00:00:00Z",
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        let export = assemble_export(&extract, 0).unwrap();
        // `deny_unknown_fields` round trip.
        let json = serde_json::to_string(&export).unwrap();
        let back: SupportPilotExport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.horizon_days, 14);
        // And the importer's own validator accepts it.
        let pilot = crate::decision_ingest::build_support_pilot(&export)
            .expect("odoo export must satisfy the pilot contract");
        assert_eq!(
            pilot.snapshot.queue_id.as_deref(),
            Some("odoo:helpdesk.ticket:7")
        );
    }

    #[test]
    fn source_version_hash_matches_the_importers_canonical_digest() {
        let extract = build_extract(
            &helpdesk_rows(),
            SupportModel::HelpdeskTicket,
            7,
            "2026-09-01T00:00:00Z",
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        let export = assemble_export(&extract, 0).unwrap();
        let expected = canonical_source_sha256(&export.tickets, &export.staffing).unwrap();
        assert_eq!(export.source_version_hashes, vec![expected]);
    }

    #[test]
    fn project_task_export_also_satisfies_the_contract() {
        let rows: Vec<_> = (1..=10_i64)
            .map(|i| {
                json!({
                    "id": i,
                    "create_date": format!("2026-09-{:02} 09:00:00", i),
                    "date_end": false,
                    "date_last_stage_update": format!("2026-09-{:02} 17:00:00", i),
                    "stage_id": [if i % 2 == 0 { 9 } else { 4 }, "Stage"],
                    "project_id": [3, "Support"],
                    "user_ids": [11],
                })
            })
            .collect();
        let extract = build_extract(
            &rows,
            SupportModel::ProjectTask,
            3,
            "2026-09-01T00:00:00Z",
            14,
            &BTreeSet::from([9_i64]),
        )
        .unwrap();
        let export = assemble_export(&extract, 0).unwrap();
        let pilot = crate::decision_ingest::build_support_pilot(&export).unwrap();
        assert_eq!(
            pilot.snapshot.queue_id.as_deref(),
            Some("odoo:project.task:3")
        );
        // Only the folded-stage rows closed.
        assert_eq!(
            export
                .tickets
                .iter()
                .filter(|t| t.resolved_at_utc.is_some())
                .count(),
            5
        );
    }

    #[test]
    fn empty_queue_assembles_a_zero_arrival_pilot() {
        let extract = build_extract(
            &[],
            SupportModel::HelpdeskTicket,
            7,
            "2026-09-01T00:00:00Z",
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        let export = assemble_export(&extract, 0).unwrap();
        crate::decision_ingest::build_support_pilot(&export).unwrap();
        assert!(export.tickets.is_empty());
    }

    #[test]
    fn snapshot_id_fits_the_importers_selector_limit() {
        let extract = build_extract(
            &[],
            SupportModel::ProjectTask,
            i64::MAX,
            "2026-09-01T00:00:00Z",
            14,
            &BTreeSet::new(),
        )
        .unwrap();
        let export = assemble_export(&extract, 0).unwrap();
        assert!(export.snapshot_id.len() <= 128);
        assert!(export.baseline_scenario_id.len() <= 128);
        assert!(!export.snapshot_id.starts_with("synthetic-support-"));
    }

    #[test]
    fn lineage_prefix_match_is_anchored() {
        assert!(is_odoo_lineage("odoo-export@abc"));
        assert!(!is_odoo_lineage("not-odoo-export@abc"));
    }

    #[tokio::test]
    async fn export_refuses_an_unknown_model_before_touching_the_network() {
        let home = tempfile::tempdir().unwrap();
        let err = export_for_agent(
            home.path(),
            "agent",
            None,
            &OdooExportRequest {
                model: "res.partner".into(),
                queue: 1,
                since_utc: "2026-09-01T00:00:00Z".into(),
                until_utc: "2026-09-15T00:00:00Z".into(),
                horizon_days: 14,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("helpdesk.ticket or project.task"), "{err}");
    }

    #[tokio::test]
    async fn export_refuses_an_out_of_range_horizon() {
        let home = tempfile::tempdir().unwrap();
        let err = export_for_agent(
            home.path(),
            "agent",
            None,
            &OdooExportRequest {
                model: "project.task".into(),
                queue: 1,
                since_utc: "2026-09-01T00:00:00Z".into(),
                until_utc: "2026-09-15T00:00:00Z".into(),
                horizon_days: 0,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("horizon_days"), "{err}");
    }

    #[tokio::test]
    async fn missing_credentials_fail_closed_without_a_connection_attempt() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[odoo]\nurl = 'https://example.invalid'\ndb = 'x'\nusername = 'u'\n",
        )
        .unwrap();
        let err = connect_for_agent(home.path(), "agent", None)
            .await
            .err()
            .expect("missing credentials must fail");
        assert!(err.contains("no Odoo credential configured"), "{err}");
    }

    #[tokio::test]
    async fn a_mismatched_profile_is_refused_rather_than_silently_ignored() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[odoo]\nurl = 'https://example.invalid'\ndb = 'x'\n",
        )
        .unwrap();
        let dir = home.path().join("agents").join("agent");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "[odoo]\nprofile = 'sales'\nusername = 'u'\napi_key_enc = 'ZmFrZQ=='\n",
        )
        .unwrap();
        let err = connect_for_agent(home.path(), "agent", Some("support"))
            .await
            .err()
            .expect("a mismatched profile must fail");
        assert!(err.contains("requested profile"), "{err}");
    }

    #[test]
    fn limitations_are_never_empty() {
        assert!(limitations().len() >= 4);
    }
}
