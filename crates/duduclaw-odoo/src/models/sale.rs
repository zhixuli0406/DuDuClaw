//! Sale model mappers — sale.order, sale.order.line.
//!
//! [O-2c] Maps Odoo sales data to DuDuClaw types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::extract_many2one_name;

/// Sale Order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleOrder {
    pub id: i64,
    pub name: String,
    pub customer: String,
    pub date: String,
    pub status: String,
    pub total: f64,
    pub salesperson: String,
    pub line_count: usize,
}

pub fn map_sale_order(v: &Value) -> SaleOrder {
    let lines = v["order_line"].as_array().map(|a| a.len()).unwrap_or(0);
    SaleOrder {
        id: v["id"].as_i64().unwrap_or(0),
        name: v["name"].as_str().unwrap_or("").to_string(),
        customer: extract_many2one_name(&v["partner_id"]),
        date: v["date_order"].as_str().unwrap_or("").to_string(),
        status: v["state"].as_str().unwrap_or("").to_string(),
        total: v["amount_total"].as_f64().unwrap_or(0.0),
        salesperson: extract_many2one_name(&v["user_id"]),
        line_count: lines,
    }
}

pub const SALE_ORDER_FIELDS: &[&str] = &[
    "id", "name", "partner_id", "date_order", "state",
    "amount_total", "user_id", "order_line",
];
