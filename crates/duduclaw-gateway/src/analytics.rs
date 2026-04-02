//! ROI analytics module — computes conversation stats, auto-reply rates,
//! response latency, and estimated cost savings.
//!
//! Currently returns realistic mock data keyed by period ("day" / "week" / "month").
//! When the session and cost SQLite tables are fully populated, the mock branches
//! can be replaced with real queries.

use chrono::Datelike;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// High-level analytics summary for a given time period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsSummary {
    /// Total number of conversations in the period.
    pub total_conversations: u64,
    /// Total number of individual messages.
    pub total_messages: u64,
    /// Fraction of messages answered without human intervention (0.0 - 1.0).
    pub auto_reply_rate: f64,
    /// Average end-to-end response latency in milliseconds.
    pub avg_response_ms: u64,
    /// 95th-percentile response latency in milliseconds.
    pub p95_response_ms: u64,
    /// Fraction of conversations handled at zero API cost by the evolution engine.
    pub zero_cost_ratio: f64,
    /// Estimated dollar savings in cents: (conversations * avg_human_cost) - actual_api_cost.
    pub estimated_savings_cents: u64,
    /// The requested period: `"day"`, `"week"`, or `"month"`.
    pub period: String,
}

/// A single day's conversation count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyConversation {
    pub date: String,
    pub count: u64,
    pub auto_count: u64,
}

/// Monthly cost comparison row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonthlyCostRow {
    pub month: String,
    pub human_cost: u64,
    pub agent_cost: u64,
    pub savings: u64,
}

// ---------------------------------------------------------------------------
// Compute functions
// ---------------------------------------------------------------------------

/// Compute an analytics summary for the given period.
///
/// `_sessions_db_path` and `_cost_db_path` are accepted for future use when
/// real DB queries are wired up.  For now we return deterministic mock data
/// that varies by period so the dashboard looks alive.
pub fn compute_summary(
    _sessions_db_path: &Path,
    _cost_db_path: &Path,
    period: &str,
) -> Result<AnalyticsSummary, String> {
    let summary = match period {
        "day" => AnalyticsSummary {
            total_conversations: 47,
            total_messages: 312,
            auto_reply_rate: 0.91,
            avg_response_ms: 820,
            p95_response_ms: 2_100,
            zero_cost_ratio: 0.87,
            estimated_savings_cents: 1_410,
            period: "day".into(),
        },
        "week" => AnalyticsSummary {
            total_conversations: 328,
            total_messages: 2_156,
            auto_reply_rate: 0.88,
            avg_response_ms: 950,
            p95_response_ms: 2_400,
            zero_cost_ratio: 0.84,
            estimated_savings_cents: 9_840,
            period: "week".into(),
        },
        "month" | _ => AnalyticsSummary {
            total_conversations: 1_423,
            total_messages: 9_487,
            auto_reply_rate: 0.86,
            avg_response_ms: 1_020,
            p95_response_ms: 2_800,
            zero_cost_ratio: 0.82,
            estimated_savings_cents: 42_690,
            period: "month".into(),
        },
    };
    Ok(summary)
}

/// Return the last 30 days of daily conversation counts (mock data).
pub fn compute_conversations() -> Vec<DailyConversation> {
    let base = chrono::Utc::now().date_naive();
    (0..30)
        .rev()
        .map(|days_ago| {
            let date = base - chrono::Duration::days(days_ago);
            // Deterministic pseudo-random based on day-of-year
            let seed = date.ordinal() as u64;
            let count = 30 + (seed * 7 + 13) % 40;
            let auto_count = (count as f64 * (0.78 + (seed % 15) as f64 * 0.01)) as u64;
            DailyConversation {
                date: date.format("%Y-%m-%d").to_string(),
                count,
                auto_count: auto_count.min(count),
            }
        })
        .collect()
}

/// Return the last 6 months of cost comparison data (mock data).
pub fn compute_cost_savings() -> Vec<MonthlyCostRow> {
    let now = chrono::Utc::now().date_naive();
    (0..6)
        .rev()
        .map(|months_ago| {
            let month_date = now - chrono::Duration::days(months_ago * 30);
            let label = month_date.format("%Y-%m").to_string();
            let seed = month_date.month() as u64;
            let human_cost = 8_000 + (seed * 311) % 4_000;
            let agent_cost = 1_200 + (seed * 137) % 1_800;
            MonthlyCostRow {
                month: label,
                human_cost,
                agent_cost,
                savings: human_cost.saturating_sub(agent_cost),
            }
        })
        .collect()
}
