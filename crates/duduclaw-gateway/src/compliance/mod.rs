//! Enterprise evidence reports. A gate decision is never a delivery receipt.
mod source;
mod render;
#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};
use duduclaw_auth::UserContext;
use duduclaw_core::EditionProfile;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(super) const MAX_ROWS: usize = 10_000;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "zh-TW")]
    ZhTw,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja-JP")]
    Ja,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format { Csv, Xlsx, Html }

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub from: DateTime<Utc>,
    pub until: DateTime<Utc>,
    #[serde(default)]
    pub language: Language,
    pub format: Format,
    #[serde(default)]
    pub include_personal_data: bool,
}
impl Request {
    fn validate(&self) -> Result<(), String> {
        if self.from >= self.until || self.until - self.from > chrono::Duration::days(366) {
            return Err("COMPLIANCE_INVALID_REQUEST".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Row {
    pub time: String,
    pub employee: String,
    pub destination: String,
    pub categories: String,
    pub decision: String,
    pub approver: String,
    pub reason: String,
    pub kind: String,
    pub requester: String,
    pub id: String,
}
#[derive(Default)]
pub(super) struct Report {
    pub outbound: Vec<Row>,
    pub approvals: Vec<Row>,
    pub accesses: Vec<Row>,
    pub reviews: Vec<Row>,
    pub notes: Vec<String>,
}
#[derive(Serialize)]
pub struct Output {
    pub filename: String,
    pub content_type: &'static str,
    pub content_base64: String,
    pub outbound_count: usize,
    pub approval_count: usize,
    pub access_count: usize,
}

pub(crate) fn authorize(home: &Path, ctx: &UserContext, edition: EditionProfile) -> Result<UserContext, String> {
    let live = crate::handlers::task_privacy::live_reader_context(home, ctx)
        .map_err(|_| "COMPLIANCE_PERMISSION_DENIED".to_string())?;
    if !live.is_admin() || live.requires_password_change() {
        return Err("COMPLIANCE_PERMISSION_DENIED".into());
    }
    if edition != EditionProfile::Enterprise {
        return Err("enterprise_edition_required".into());
    }
    Ok(live)
}

/// Called only after the handler resolves the active license-derived edition.
pub(crate) fn generate(home: &Path, ctx: &UserContext, edition: EditionProfile, request: &Request) -> Result<Output, String> {
    use base64::Engine;
    let live = authorize(home, ctx, edition)?;
    request.validate()?;
    let report = source::collect(home, request)?;
    let bytes = render::encode(&report, request)?;
    if bytes.len() > MAX_OUTPUT { return Err("COMPLIANCE_TOO_LARGE".into()); }
    authorize(home, &live, edition)?;
    let event = duduclaw_security::audit::AuditEvent::new(
        if request.include_personal_data { "compliance_personal_data_export" } else { "compliance_report_export" },
        "dashboard",
        duduclaw_security::audit::Severity::Info,
        serde_json::json!({"actor": live.user_id, "from":request.from, "until":request.until,
            "include_personal_data":request.include_personal_data,
            "outbound_count":report.outbound.len(), "approval_count":report.approvals.len(),
            "access_count":report.accesses.len(), "review_count":report.reviews.len()}),
    );
    // Content is never released when the strict, locked audit append fails.
    duduclaw_security::audit::try_append_audit_event(home, &event)
        .map_err(|_| "COMPLIANCE_AUDIT_UNAVAILABLE".to_string())?;
    let (extension, content_type) = match request.format {
        Format::Csv => ("csv", "text/csv; charset=utf-8"),
        Format::Xlsx => ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        Format::Html => ("html", "text/html; charset=utf-8"),
    };
    Ok(Output {
        filename: format!("duduclaw-compliance-{}.{}", request.from.format("%Y%m%d"), extension),
        content_type,
        content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        outbound_count: report.outbound.len(), approval_count: report.approvals.len(), access_count: report.accesses.len(),
    })
}
