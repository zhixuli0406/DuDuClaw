//! `duduclaw weekly-report` — per-agent weekly usage report.
//!
//! Aggregates three local SQLite stores under `<home>/`:
//! - `cost_telemetry.db` → API call count, token usage, estimated cost
//! - `tasks.db`          → activity log grouped by `event_type`
//! - `audit_index.db`    → Evolution-Events reliability metrics
//!
//! The report is grouped per agent (matched against `AgentRunner::list_agents()`)
//! and rendered as Markdown (default) or JSON.
//!
//! Week-over-week deltas for cost / call count are derived from two non-overlapping
//! windows: `[now - days*24h, now]` (current) and `[now - days*48h, now - days*24h]`
//! (previous). The previous-window totals are computed by subtracting the current
//! window from the `2*days` cumulative window — i.e. `prev = total(2N) - total(N)`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use duduclaw_agent::AgentRunner;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::cost_telemetry::CostTelemetry;
use duduclaw_gateway::evolution_events::query::AuditEventIndex;
use duduclaw_gateway::evolution_events::reliability::ReliabilitySummary;
use duduclaw_gateway::task_store::TaskStore;
use serde::Serialize;

/// Hard cap on activity rows scanned per agent. The activity table has no time
/// index, so we fetch DESC-ordered rows up to this cap and filter by timestamp
/// client-side. If the cap is hit, the renderer notes that older rows in the
/// window may be undercounted.
const ACTIVITY_FETCH_CAP: i64 = 10_000;

/// Top-N rows shown in the leaderboard table.
const LEADERBOARD_TOP_N: usize = 5;

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

pub async fn run(
    home_dir: &Path,
    days: u32,
    agent_filter: Option<&str>,
    output: Option<&Path>,
    format: &str,
) -> Result<()> {
    if !(1..=365).contains(&days) {
        return Err(DuDuClawError::Config(
            "weekly-report --days must be in [1, 365]".into(),
        ));
    }

    let report = collect(home_dir, days, agent_filter).await?;

    let rendered = match format {
        "json" => serde_json::to_string_pretty(&report)?,
        _ => render_markdown(&report),
    };

    match output {
        Some(path) => {
            std::fs::write(path, &rendered)?;
            eprintln!("Report written to {}", path.display());
        }
        None => {
            print!("{rendered}");
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Data model
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ReportData {
    pub generated_at: DateTime<Utc>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub days: u32,
    pub total_agents_registered: usize,
    pub total_agents_active: usize,
    pub agents: Vec<AgentReport>,
    /// True when at least one agent's activity scan hit ACTIVITY_FETCH_CAP.
    pub activity_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentReport {
    pub name: String,
    pub display_name: String,
    pub role: String,
    pub status: String,
    pub cost: AgentCostMetrics,
    pub activity: ActivityMetrics,
    pub reliability: Option<ReliabilitySummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentCostMetrics {
    pub requests_current: u64,
    pub requests_previous: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_millicents: u64,
    pub cost_millicents_previous: u64,
    pub cache_efficiency: f64,
    pub cache_health: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ActivityMetrics {
    pub by_event_type: BTreeMap<String, u64>,
    pub total: u64,
    /// True when the underlying scan hit ACTIVITY_FETCH_CAP.
    pub truncated: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Data collection
// ─────────────────────────────────────────────────────────────────────────────

async fn collect(
    home_dir: &Path,
    days: u32,
    agent_filter: Option<&str>,
) -> Result<ReportData> {
    // Open data sources. Missing DBs are auto-created empty by the
    // underlying constructors — that yields zero counts, which is fine.
    let cost_db: PathBuf = home_dir.join("cost_telemetry.db");
    let cost = CostTelemetry::new(&cost_db).map_err(DuDuClawError::Gateway)?;
    let tasks = TaskStore::open(home_dir).map_err(DuDuClawError::Gateway)?;

    // Audit index needs an explicit sync from the JSONL source files. If the
    // events directory doesn't exist (fresh install / no evolution events
    // emitted yet), we log and proceed with an empty index.
    let audit = AuditEventIndex::open(home_dir).map_err(DuDuClawError::Gateway)?;
    if let Err(e) = audit.sync_from_files().await {
        tracing::warn!(error = %e, "weekly-report: audit index sync failed (continuing with empty index)");
    }

    // Enumerate agents.
    let runner = AgentRunner::new(home_dir.to_path_buf())
        .await
        .map_err(|e| DuDuClawError::Agent(format!("load agents: {e}")))?;
    let agents = runner.list_agents();

    let total_registered = agents.len();
    let now = Utc::now();
    let window_end = now;
    let window_start = window_end - Duration::hours((days as i64) * 24);
    let hours_current = days as u64 * 24;
    let hours_double = hours_current.saturating_mul(2);

    let mut agent_reports = Vec::new();
    let mut active_count = 0usize;
    let mut activity_truncated = false;

    for agent in &agents {
        let info = &agent.config.agent;
        if let Some(filter) = agent_filter {
            if info.name != filter {
                continue;
            }
        }

        // Cost metrics — current + 2N windows for WoW.
        let cur = cost
            .summary_by_agent(&info.name, hours_current)
            .await
            .map_err(DuDuClawError::Gateway)?;
        let dbl = cost
            .summary_by_agent(&info.name, hours_double)
            .await
            .map_err(DuDuClawError::Gateway)?;

        let requests_previous = dbl
            .summary
            .total_requests
            .saturating_sub(cur.summary.total_requests);
        let cost_previous = dbl
            .summary
            .total_cost_millicents
            .saturating_sub(cur.summary.total_cost_millicents);

        let cost_metrics = AgentCostMetrics {
            requests_current: cur.summary.total_requests,
            requests_previous,
            input_tokens: cur.summary.total_input_tokens,
            output_tokens: cur.summary.total_output_tokens,
            cache_read_tokens: cur.summary.total_cache_read_tokens,
            cache_creation_tokens: cur.summary.total_cache_creation_tokens,
            cost_millicents: cur.summary.total_cost_millicents,
            cost_millicents_previous: cost_previous,
            cache_efficiency: cur.summary.avg_cache_efficiency,
            cache_health: cur.cache_health,
        };

        // Activity metrics — pull DESC-ordered rows and filter by timestamp.
        let activity = collect_activity(&tasks, &info.name, &window_start).await?;
        if activity.truncated {
            activity_truncated = true;
        }

        // Reliability metrics — sourced from audit_index.
        let reliability = match audit.compute_reliability_summary(&info.name, days).await {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(agent = %info.name, error = %e, "reliability summary failed");
                None
            }
        };

        let active = cost_metrics.requests_current > 0
            || activity.total > 0
            || reliability.as_ref().map(|r| r.total_events > 0).unwrap_or(false);
        if active {
            active_count += 1;
        }

        agent_reports.push(AgentReport {
            name: info.name.clone(),
            display_name: info.display_name.clone(),
            role: format!("{:?}", info.role),
            status: format!("{:?}", info.status),
            cost: cost_metrics,
            activity,
            reliability,
        });
    }

    // Sort by current-window call count descending so the per-agent section
    // and the leaderboard share a consistent ordering.
    agent_reports.sort_by(|a, b| {
        b.cost
            .requests_current
            .cmp(&a.cost.requests_current)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(ReportData {
        generated_at: now,
        window_start,
        window_end,
        days,
        total_agents_registered: total_registered,
        total_agents_active: active_count,
        agents: agent_reports,
        activity_truncated,
    })
}

async fn collect_activity(
    tasks: &TaskStore,
    agent_id: &str,
    window_start: &DateTime<Utc>,
) -> Result<ActivityMetrics> {
    let (rows, _total) = tasks
        .list_activity(Some(agent_id), None, ACTIVITY_FETCH_CAP, 0)
        .await
        .map_err(DuDuClawError::Gateway)?;

    let truncated = rows.len() as i64 >= ACTIVITY_FETCH_CAP;
    let mut by_event_type: BTreeMap<String, u64> = BTreeMap::new();
    let mut total: u64 = 0;
    for row in &rows {
        let ts = match DateTime::parse_from_rfc3339(&row.timestamp) {
            Ok(t) => t.with_timezone(&Utc),
            Err(_) => continue,
        };
        if ts < *window_start {
            // Rows are DESC-ordered — once we cross the boundary we can stop.
            break;
        }
        *by_event_type.entry(row.event_type.clone()).or_insert(0) += 1;
        total += 1;
    }

    Ok(ActivityMetrics {
        by_event_type,
        total,
        truncated,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Markdown renderer (pure — testable without DB)
// ─────────────────────────────────────────────────────────────────────────────

pub fn render_markdown(report: &ReportData) -> String {
    let mut out = String::new();

    out.push_str("# DuDuClaw Agent 週報\n\n");
    out.push_str(&format!(
        "- **統計區間**：{} ~ {}（過去 {} 天）\n",
        format_date(&report.window_start),
        format_date(&report.window_end),
        report.days
    ));
    out.push_str(&format!(
        "- **產出時間**：{}\n",
        report.generated_at.to_rfc3339()
    ));
    out.push_str(&format!(
        "- **註冊 Agent 數**：{}（本期有活動：{}）\n\n",
        report.total_agents_registered, report.total_agents_active
    ));

    // Totals
    let total_requests: u64 = report.agents.iter().map(|a| a.cost.requests_current).sum();
    let total_requests_prev: u64 = report.agents.iter().map(|a| a.cost.requests_previous).sum();
    let total_cost: u64 = report.agents.iter().map(|a| a.cost.cost_millicents).sum();
    let total_cost_prev: u64 = report.agents.iter().map(|a| a.cost.cost_millicents_previous).sum();
    let total_input: u64 = report.agents.iter().map(|a| a.cost.input_tokens).sum();
    let total_output: u64 = report.agents.iter().map(|a| a.cost.output_tokens).sum();
    let total_activity: u64 = report.agents.iter().map(|a| a.activity.total).sum();

    out.push_str("## 總覽\n\n");
    out.push_str("| 指標 | 數值 | 上週 | 變化 |\n");
    out.push_str("|---|---:|---:|---:|\n");
    out.push_str(&format!(
        "| API 呼叫總次數 | {} | {} | {} |\n",
        fmt_num(total_requests),
        fmt_num(total_requests_prev),
        fmt_delta(total_requests, total_requests_prev)
    ));
    out.push_str(&format!(
        "| 估算成本 (USD) | {} | {} | {} |\n",
        fmt_usd(total_cost),
        fmt_usd(total_cost_prev),
        fmt_delta(total_cost, total_cost_prev)
    ));
    out.push_str(&format!("| Input Token | {} | — | — |\n", fmt_num(total_input)));
    out.push_str(&format!("| Output Token | {} | — | — |\n", fmt_num(total_output)));
    out.push_str(&format!(
        "| 任務活動事件總數 | {} | — | — |\n\n",
        fmt_num(total_activity)
    ));

    // Leaderboard
    out.push_str(&format!(
        "## Top {} 高頻使用 Agent\n\n",
        LEADERBOARD_TOP_N.min(report.agents.len()).max(1)
    ));
    if report.agents.is_empty() {
        out.push_str("_（區間內無 Agent 資料）_\n\n");
    } else {
        out.push_str("| 排名 | Agent | 呼叫次數 | Token (in/out) | 成本 (USD) | WoW |\n");
        out.push_str("|---:|---|---:|---|---:|---:|\n");
        for (rank, agent) in report.agents.iter().take(LEADERBOARD_TOP_N).enumerate() {
            out.push_str(&format!(
                "| {} | {} | {} | {} / {} | {} | {} |\n",
                rank + 1,
                agent.name,
                fmt_num(agent.cost.requests_current),
                fmt_num(agent.cost.input_tokens),
                fmt_num(agent.cost.output_tokens),
                fmt_usd(agent.cost.cost_millicents),
                fmt_delta(
                    agent.cost.requests_current,
                    agent.cost.requests_previous
                )
            ));
        }
        out.push('\n');
    }

    // Per-agent detail
    out.push_str("## 各 Agent 詳細報告\n\n");
    if report.agents.is_empty() {
        out.push_str("_（無資料）_\n\n");
    }
    for agent in &report.agents {
        out.push_str(&format!("### {} ({})\n\n", agent.name, agent.display_name));
        out.push_str(&format!(
            "- **角色 / 狀態**：{} / {}\n",
            agent.role, agent.status
        ));
        out.push_str(&format!(
            "- **API 呼叫**：{} 次（上週 {}，{}）\n",
            fmt_num(agent.cost.requests_current),
            fmt_num(agent.cost.requests_previous),
            fmt_delta(agent.cost.requests_current, agent.cost.requests_previous)
        ));
        out.push_str(&format!(
            "- **Token**：input {} / output {} / cache_read {} / cache_creation {}\n",
            fmt_num(agent.cost.input_tokens),
            fmt_num(agent.cost.output_tokens),
            fmt_num(agent.cost.cache_read_tokens),
            fmt_num(agent.cost.cache_creation_tokens),
        ));
        out.push_str(&format!(
            "- **快取效率**：{:.1}%（{}）\n",
            agent.cost.cache_efficiency * 100.0,
            agent.cost.cache_health
        ));
        out.push_str(&format!(
            "- **估算成本**：{}（上週 {}，{}）\n",
            fmt_usd(agent.cost.cost_millicents),
            fmt_usd(agent.cost.cost_millicents_previous),
            fmt_delta(
                agent.cost.cost_millicents,
                agent.cost.cost_millicents_previous
            )
        ));

        if agent.activity.total == 0 {
            out.push_str("- **任務活動**：（無）\n");
        } else {
            let parts: Vec<String> = agent
                .activity
                .by_event_type
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            out.push_str(&format!(
                "- **任務活動**：總計 {} 件（{}）\n",
                fmt_num(agent.activity.total),
                parts.join(", ")
            ));
        }

        if let Some(rel) = &agent.reliability {
            out.push_str(&format!(
                "- **可靠性**：success={:.1}% / consistency={:.1}% / skill_adoption={:.1}% / fallback={:.1}% （{} 事件）\n",
                rel.task_success_rate * 100.0,
                rel.consistency_score * 100.0,
                rel.skill_adoption_rate * 100.0,
                rel.fallback_trigger_rate * 100.0,
                rel.total_events,
            ));
        } else {
            out.push_str("- **可靠性**：（無 audit 資料）\n");
        }
        out.push('\n');
    }

    // Footer
    out.push_str("---\n\n");
    out.push_str("**資料來源**：\n");
    out.push_str("- `~/.duduclaw/cost_telemetry.db`（CostTelemetry）\n");
    out.push_str("- `~/.duduclaw/tasks.db`（TaskStore activity）\n");
    out.push_str("- `~/.duduclaw/audit_index.db`（Evolution Events reliability）\n");
    if report.activity_truncated {
        out.push_str(&format!(
            "\n> ⚠️  部分 Agent 的活動掃描達到 {} 列上限，週初的事件可能未完全納入。\n",
            ACTIVITY_FETCH_CAP
        ));
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Formatting helpers
// ─────────────────────────────────────────────────────────────────────────────

fn format_date(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d").to_string()
}

/// Thousands-separated integer formatting.
fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Millicents → "$X.XX" string.
fn fmt_usd(millicents: u64) -> String {
    let dollars = millicents as f64 / 100_000.0;
    format!("${dollars:.2}")
}

/// Render a current-vs-previous percentage delta as "+12.5%" / "-3.0%" / "—".
fn fmt_delta(current: u64, previous: u64) -> String {
    if previous == 0 {
        return if current == 0 { "—".into() } else { "new".into() };
    }
    let delta = current as f64 - previous as f64;
    let pct = delta / previous as f64 * 100.0;
    let sign = if pct >= 0.0 { "+" } else { "" };
    format!("{sign}{pct:.1}%")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — pure renderer + formatting helpers
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn fmt_num_inserts_thousands_separators() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(42), "42");
        assert_eq!(fmt_num(1_234), "1,234");
        assert_eq!(fmt_num(1_234_567), "1,234,567");
        assert_eq!(fmt_num(12_345_678_901), "12,345,678,901");
    }

    #[test]
    fn fmt_usd_converts_millicents_to_dollars() {
        assert_eq!(fmt_usd(0), "$0.00");
        assert_eq!(fmt_usd(100_000), "$1.00");
        assert_eq!(fmt_usd(123_456), "$1.23");
        assert_eq!(fmt_usd(50), "$0.00");
    }

    #[test]
    fn fmt_delta_handles_zero_previous() {
        assert_eq!(fmt_delta(0, 0), "—");
        assert_eq!(fmt_delta(100, 0), "new");
    }

    #[test]
    fn fmt_delta_renders_positive_and_negative_pct() {
        assert_eq!(fmt_delta(110, 100), "+10.0%");
        assert_eq!(fmt_delta(90, 100), "-10.0%");
        assert_eq!(fmt_delta(100, 100), "+0.0%");
    }

    fn sample_report() -> ReportData {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let mut activity = BTreeMap::new();
        activity.insert("task_completed".into(), 12);
        activity.insert("task_blocked".into(), 1);

        ReportData {
            generated_at: now,
            window_start: now - Duration::days(7),
            window_end: now,
            days: 7,
            total_agents_registered: 2,
            total_agents_active: 1,
            agents: vec![
                AgentReport {
                    name: "agnes".into(),
                    display_name: "Agnes".into(),
                    role: "Strategist".into(),
                    status: "Active".into(),
                    cost: AgentCostMetrics {
                        requests_current: 1234,
                        requests_previous: 1100,
                        input_tokens: 3_100_000,
                        output_tokens: 800_000,
                        cache_read_tokens: 1_300_000,
                        cache_creation_tokens: 200_000,
                        cost_millicents: 185_000,
                        cost_millicents_previous: 165_000,
                        cache_efficiency: 0.64,
                        cache_health: "healthy".into(),
                    },
                    activity: ActivityMetrics {
                        by_event_type: activity,
                        total: 13,
                        truncated: false,
                    },
                    reliability: Some(ReliabilitySummary {
                        agent_id: "agnes".into(),
                        window_days: 7,
                        consistency_score: 0.891,
                        task_success_rate: 0.923,
                        skill_adoption_rate: 0.05,
                        fallback_trigger_rate: 0.021,
                        total_events: 156,
                        generated_at: now.to_rfc3339(),
                    }),
                },
                AgentReport {
                    name: "bobby".into(),
                    display_name: "Bobby".into(),
                    role: "Helper".into(),
                    status: "Active".into(),
                    cost: AgentCostMetrics::default(),
                    activity: ActivityMetrics::default(),
                    reliability: None,
                },
            ],
            activity_truncated: false,
        }
    }

    #[test]
    fn render_markdown_includes_overview_and_per_agent_sections() {
        let md = render_markdown(&sample_report());

        assert!(md.contains("# DuDuClaw Agent 週報"));
        assert!(md.contains("2026-04-30 ~ 2026-05-07"));
        assert!(md.contains("## 總覽"));
        assert!(md.contains("## Top"));
        assert!(md.contains("## 各 Agent 詳細報告"));
        assert!(md.contains("### agnes (Agnes)"));
        assert!(md.contains("### bobby (Bobby)"));
        assert!(md.contains("$1.85"));
        assert!(md.contains("+12.2%")); // 1234 vs 1100 = +12.18%
        assert!(md.contains("task_completed=12"));
        assert!(md.contains("success=92.3%"));
    }

    #[test]
    fn render_markdown_handles_empty_agent_list() {
        let mut report = sample_report();
        report.agents.clear();
        report.total_agents_active = 0;
        let md = render_markdown(&report);
        assert!(md.contains("（區間內無 Agent 資料）"));
        assert!(md.contains("（無資料）"));
    }

    #[test]
    fn render_markdown_warns_when_activity_truncated() {
        let mut report = sample_report();
        report.activity_truncated = true;
        let md = render_markdown(&report);
        assert!(md.contains("活動掃描達到"));
    }

    #[test]
    fn render_markdown_handles_missing_reliability() {
        let report = sample_report();
        let md = render_markdown(&report);
        assert!(md.contains("（無 audit 資料）"));
    }
}
