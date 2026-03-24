//! CRM model mappers — crm.lead, crm.stage.
//!
//! [O-2a] Maps Odoo CRM data to DuDuClaw types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{extract_many2one_id, extract_many2one_name};

/// CRM Lead / Opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrmLead {
    pub id: i64,
    pub name: String,
    pub contact_name: String,
    pub email: String,
    pub phone: String,
    pub stage: String,
    pub stage_id: i64,
    pub expected_revenue: f64,
    pub probability: f64,
    pub salesperson: String,
    pub team: String,
    pub lead_type: String,
}

pub fn map_crm_lead(v: &Value) -> CrmLead {
    CrmLead {
        id: v["id"].as_i64().unwrap_or(0),
        name: v["name"].as_str().unwrap_or("").to_string(),
        contact_name: v["contact_name"].as_str().unwrap_or("").to_string(),
        email: v["email_from"].as_str().unwrap_or("").to_string(),
        phone: v["phone"].as_str().unwrap_or("").to_string(),
        stage: extract_many2one_name(&v["stage_id"]),
        stage_id: extract_many2one_id(&v["stage_id"]),
        expected_revenue: v["expected_revenue"].as_f64().unwrap_or(0.0),
        probability: v["probability"].as_f64().unwrap_or(0.0),
        salesperson: extract_many2one_name(&v["user_id"]),
        team: extract_many2one_name(&v["team_id"]),
        lead_type: v["type"].as_str().unwrap_or("lead").to_string(),
    }
}

pub const CRM_LEAD_FIELDS: &[&str] = &[
    "id", "name", "contact_name", "email_from", "phone",
    "stage_id", "expected_revenue", "probability",
    "user_id", "team_id", "type",
];
