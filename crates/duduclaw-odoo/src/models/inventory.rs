//! Inventory model mappers — product.product, stock.quant.
//!
//! [O-3a] Maps Odoo inventory data to DuDuClaw types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::extract_many2one_name;

/// Product with stock info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: i64,
    pub name: String,
    pub default_code: String,
    pub list_price: f64,
    pub qty_available: f64,
    pub virtual_available: f64,
    pub product_type: String,
}

pub fn map_product(v: &Value) -> Product {
    Product {
        id: v["id"].as_i64().unwrap_or(0),
        name: v["name"].as_str().unwrap_or("").to_string(),
        default_code: v["default_code"].as_str().unwrap_or("").to_string(),
        list_price: v["list_price"].as_f64().unwrap_or(0.0),
        qty_available: v["qty_available"].as_f64().unwrap_or(0.0),
        virtual_available: v["virtual_available"].as_f64().unwrap_or(0.0),
        // Odoo 18+ exposes the product type through `type`; older
        // servers retain `detailed_type`. The connector drops absent fields.
        product_type: v["detailed_type"]
            .as_str()
            .or_else(|| v["type"].as_str())
            .unwrap_or("consu")
            .to_string(),
    }
}

pub const PRODUCT_FIELDS: &[&str] = &[
    "id",
    "name",
    "default_code",
    "list_price",
    "qty_available",
    "virtual_available",
    "detailed_type",
    "type",
];

/// Stock quantity for a specific product at a location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockQuant {
    pub product: String,
    pub location: String,
    pub quantity: f64,
    pub reserved: f64,
}

pub fn map_stock_quant(v: &Value) -> StockQuant {
    StockQuant {
        product: extract_many2one_name(&v["product_id"]),
        location: extract_many2one_name(&v["location_id"]),
        quantity: v["quantity"].as_f64().unwrap_or(0.0),
        reserved: v["reserved_quantity"].as_f64().unwrap_or(0.0),
    }
}

pub const STOCK_QUANT_FIELDS: &[&str] =
    &["product_id", "location_id", "quantity", "reserved_quantity"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn inventory_domain_uses_cached_field_shape_without_hiding_services() {
        for metadata in [json!({"id":{}, "name":{}, "is_storable":{}, "type":{}}),
            json!({"id":{}, "name":{}, "detailed_type":{}, "type":{}})] {
            let expected = if metadata.get("is_storable").is_some() {
                json!(["is_storable", "=", true])
            } else { json!(["detailed_type", "=", "product"]) };
            let conn = crate::connector::mocked_connector(vec![Ok(metadata), Ok(json!([]))]);
            assert_eq!(conn.inventory_product_domain("", false).await.unwrap(), Vec::<Value>::new());
            assert_eq!(conn.inventory_product_domain("課程", false).await.unwrap(),
                vec![json!(["name", "ilike", "課程"])]);
            assert_eq!(conn.inventory_product_domain("", true).await.unwrap(), vec![expected]);
            conn.search_read("product.product", vec![], PRODUCT_FIELDS, 20).await.unwrap();
            assert_eq!(conn.mocked_requests().len(), 2);
            assert_eq!(conn.mocked_requests()[0].3["attributes"], json!(["type"]));
        }
        let conn = crate::connector::mocked_connector(vec![Ok(json!({"type":{}}))]);
        assert!(conn.inventory_product_domain("", true).await.unwrap_err().contains("儲存"));
        assert!(conn.inventory_product_domain("課程", false).await.is_err());
        assert_eq!(conn.mocked_requests().len(), 1);
        let conn = crate::connector::mocked_connector(vec![Err("metadata denied".into())]);
        assert!(conn.inventory_product_domain("", false).await.is_err());
    }

    #[tokio::test]
    async fn inventory_check_returns_product_totals_and_safe_quant_error() {
        for quant_result in [Ok(json!([])), Err("AccessError: /private/server password=secret".into())] {
            let conn = crate::connector::mocked_connector(vec![
                Ok(json!({"id":{}, "name":{}, "qty_available":{}, "virtual_available":{}})),
                Ok(json!([{"id":7,"name":"課程","qty_available":0.0,"virtual_available":0.0}])),
                Ok(json!({"product_id":{}, "quantity":{}})), quant_result.clone(),
            ]);
            let result = conn.inventory_check(7).await;
            if quant_result.is_ok() {
                let result = result.unwrap();
                assert_eq!(result["name"], "課程");
                assert_eq!(result["qty_available"], 0.0);
                assert_eq!(result["virtual_available"], 0.0);
                assert_eq!(result["quants"], json!([]));
            } else {
                let error = result.unwrap_err();
                assert!(error.contains("權限"));
                assert!(!error.contains("secret"));
            }
        }
        let conn = crate::connector::mocked_connector(vec![Ok(json!({"id":{}, "name":{}})), Ok(json!([]))]);
        assert!(conn.inventory_check(7).await.unwrap_err().contains("找不到"));
        assert_eq!(conn.mocked_requests().len(), 2);
    }

    #[tokio::test]
    async fn inventory_totals_are_not_computed_from_location_sample() {
        let conn = crate::connector::mocked_connector(vec![
            Ok(json!({"id":{}, "name":{}, "qty_available":{}, "virtual_available":{}})),
            Ok(json!([{"id":7,"name":"商品","qty_available":123.0,"virtual_available":140.0}])),
            Ok(json!({"product_id":{}, "quantity":{}})),
            Ok(json!([{"product_id":[7,"商品"],"quantity":2.0}])),
        ]);
        let result = conn.inventory_check(7).await.unwrap();
        assert_eq!(result["qty_available"], 123.0);
        assert_eq!(result["virtual_available"], 140.0);
        assert_eq!(result["quants"][0]["quantity"], 2.0);
        assert_eq!(conn.mocked_requests()[3].3["limit"], 10);
        let conn = crate::connector::mocked_connector(vec![
            Ok(json!({"id":{}, "name":{}})), Ok(json!([{"id":7,"name":"課程"}])),
            Err("stock.quant missing".into()),
        ]);
        assert!(conn.inventory_check(7).await.unwrap_err().contains("權限"));
    }

    #[test]
    fn product_type_accepts_new_type_and_preserves_existing_detailed_type() {
        for product_type in ["service", "combo", "consu"] {
            assert_eq!(
                map_product(&json!({"type": product_type})).product_type,
                product_type
            );
        }
        assert_eq!(
            map_product(&json!({"detailed_type":"product","type":"consu"})).product_type,
            "product"
        );
    }
    #[tokio::test]
    async fn product_projection_selects_only_present_old_or_new_type_fields() {
        for (field, product_type) in [("type", "service"), ("detailed_type", "product")] {
            let conn = crate::connector::mocked_connector(vec![
                Ok(json!({"id":{"type":"integer"},(field):{"type":"selection"}})),
                Ok(json!([{"id":1,(field):product_type}])),
            ]);
            let result = conn
                .search_read("product.product", Vec::new(), PRODUCT_FIELDS, 10)
                .await
                .unwrap();
            assert_eq!(map_product(&result[0]).product_type, product_type);
            assert_eq!(conn.mocked_requests()[1].3["fields"], json!(["id", field]));
        }
    }
}
