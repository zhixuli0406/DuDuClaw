//! Accounting model mappers — account.move, account.payment.
//!
//! [O-3c] Maps Odoo accounting data to DuDuClaw types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::extract_many2one_name;

/// Invoice / Bill / Journal Entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub id: i64,
    pub number: String,
    pub partner: String,
    pub move_type: String,
    pub status: String,
    pub total: f64,
    pub balance_due: f64,
    pub payment_status: String,
    pub date: String,
}

pub fn map_invoice(v: &Value) -> Invoice {
    Invoice {
        id: v["id"].as_i64().unwrap_or(0),
        number: v["name"].as_str().unwrap_or("").to_string(),
        partner: extract_many2one_name(&v["partner_id"]),
        move_type: v["move_type"].as_str().unwrap_or("").to_string(),
        status: v["state"].as_str().unwrap_or("").to_string(),
        total: v["amount_total"].as_f64().unwrap_or(0.0),
        balance_due: v["amount_residual"].as_f64().unwrap_or(0.0),
        payment_status: v["payment_state"].as_str().unwrap_or("").to_string(),
        date: v["invoice_date"].as_str().unwrap_or("").to_string(),
    }
}

pub const INVOICE_FIELDS: &[&str] = &[
    "id", "name", "partner_id", "move_type", "state",
    "amount_total", "amount_residual", "payment_state", "invoice_date",
];
