//! Common Odoo model helpers and base types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Extract display name from Odoo many2one field `[id, "name"]` or `false`.
pub fn extract_many2one_name(val: &Value) -> String {
    match val {
        Value::Array(arr) if arr.len() >= 2 => {
            arr[1].as_str().unwrap_or("").to_string()
        }
        _ => String::new(),
    }
}

/// Extract ID from Odoo many2one field `[id, "name"]`.
pub fn extract_many2one_id(val: &Value) -> i64 {
    match val {
        Value::Array(arr) if !arr.is_empty() => arr[0].as_i64().unwrap_or(0),
        Value::Number(n) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Odoo partner (res.partner) — shared across all modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Partner {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub phone: String,
    pub is_company: bool,
}

pub fn map_partner(v: &Value) -> Partner {
    Partner {
        id: v["id"].as_i64().unwrap_or(0),
        name: v["name"].as_str().unwrap_or("").to_string(),
        email: v["email"].as_str().unwrap_or("").to_string(),
        phone: v["phone"].as_str().unwrap_or("").to_string(),
        is_company: v["is_company"].as_bool().unwrap_or(false),
    }
}

/// Fields commonly requested for partners.
pub const PARTNER_FIELDS: &[&str] = &["id", "name", "email", "phone", "is_company"];
