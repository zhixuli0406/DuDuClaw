//! OpenAI embeddings: `POST {base}/v1/embeddings`, `Authorization: Bearer`,
//! body `{model, input: [...], dimensions, encoding_format: "float"}`,
//! response `data[].{index, embedding}` (API reference, checked 2026-10-22).

use serde_json::{Value, json};

/// `(url, headers, body)` for one batch.
pub fn request(
    base: &str,
    model: &str,
    key: &str,
    texts: &[String],
) -> (String, Vec<(&'static str, String)>, Value) {
    let body = json!({
        "model": model,
        "input": texts,
        "dimensions": super::cloud::CloudProvider::OpenAi.dim(),
        "encoding_format": "float",
    });
    (
        format!("{base}/v1/embeddings"),
        vec![("Authorization", format!("Bearer {key}"))],
        body,
    )
}

/// Vectors in input order (`index` decides, not response order).
pub fn parse(resp: &Value, n: usize) -> Result<Vec<Vec<f32>>, String> {
    let data = resp
        .get("data")
        .and_then(Value::as_array)
        .ok_or("缺少 data")?;
    let mut out: Vec<Option<Vec<f32>>> = vec![None; n];
    for item in data {
        let i = item
            .get("index")
            .and_then(Value::as_u64)
            .ok_or("缺少 index")? as usize;
        let v = floats(item.get("embedding")).ok_or("embedding 不是數字陣列")?;
        *out.get_mut(i).ok_or("index 超出範圍")? = Some(v);
    }
    out.into_iter()
        .map(|v| v.ok_or_else(|| "向量數量不符".to_string()))
        .collect()
}

pub(crate) fn floats(v: Option<&Value>) -> Option<Vec<f32>> {
    v?.as_array()?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32))
        .collect()
}
