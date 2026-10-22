//! Gemini embeddings: `POST {base}/v1beta/models/{model}:batchEmbedContents`
//! with `x-goog-api-key`; each request carries `taskType`
//! (`RETRIEVAL_DOCUMENT` / `RETRIEVAL_QUERY`) and `outputDimensionality`.
//! Response `embeddings[].values`, in request order. `gemini-embedding-001`
//! vectors below 3072 dimensions must be normalised by the caller (docs,
//! checked 2026-10-22); [`super::cloud::CloudEmbedder`] normalises every
//! vector.

use serde_json::{Value, json};

/// `(url, headers, body)` for one batch.
pub fn request(
    base: &str,
    model: &str,
    key: &str,
    texts: &[String],
    query: bool,
) -> (String, Vec<(&'static str, String)>, Value) {
    let task = if query {
        "RETRIEVAL_QUERY"
    } else {
        "RETRIEVAL_DOCUMENT"
    };
    let requests: Vec<Value> = texts
        .iter()
        .map(|t| {
            json!({
                "model": format!("models/{model}"),
                "content": {"parts": [{"text": t}]},
                "taskType": task,
                "outputDimensionality": super::cloud::CloudProvider::Gemini.dim(),
            })
        })
        .collect();
    (
        format!("{base}/v1beta/models/{model}:batchEmbedContents"),
        vec![("x-goog-api-key", key.to_string())],
        json!({ "requests": requests }),
    )
}

/// Vectors in request order.
pub fn parse(resp: &Value, n: usize) -> Result<Vec<Vec<f32>>, String> {
    let list = resp
        .get("embeddings")
        .and_then(Value::as_array)
        .ok_or("缺少 embeddings")?;
    if list.len() != n {
        return Err("向量數量不符".to_string());
    }
    list.iter()
        .map(|e| {
            super::cloud_openai::floats(e.get("values"))
                .ok_or_else(|| "values 不是數字陣列".to_string())
        })
        .collect()
}
