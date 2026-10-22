use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::embed::redactor::RefuseRedactor;

/// Replaces every `@`-address with `[EMAIL]` — stands in for the gateway's
/// rule engine.
struct EmailRedactor;

impl TextRedactor for EmailRedactor {
    fn redact(&self, text: &str) -> Result<String, String> {
        Ok(text
            .split(' ')
            .map(|w| if w.contains('@') { "[EMAIL]" } else { w })
            .collect::<Vec<_>>()
            .join(" "))
    }
}

/// A one-thread HTTP mock: records each request body and answers with
/// `respond(body)`.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

fn mock_server(respond: fn(&Value) -> Value) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0usize;
            let mut auth = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("authorization:") || lower.starts_with("x-goog-api-key:") {
                    auth = line.trim().to_string();
                }
                head.push_str(&line);
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            log.lock()
                .unwrap()
                .push((format!("{path} {auth}"), body.clone()));
            let resp = respond(&body).to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
        }
    });
    (format!("http://127.0.0.1:{}", addr.port()), seen)
}

fn openai_answer(body: &Value) -> Value {
    let n = body["input"].as_array().map(|a| a.len()).unwrap_or(0);
    // Reverse order on purpose: `index` must decide.
    let data: Vec<Value> = (0..n)
        .rev()
        .map(|i| {
            let mut v = vec![0.0f32; 1536];
            v[i] = 2.0;
            json!({"object": "embedding", "index": i, "embedding": v})
        })
        .collect();
    json!({"object": "list", "data": data})
}

fn gemini_answer(body: &Value) -> Value {
    let n = body["requests"].as_array().map(|a| a.len()).unwrap_or(0);
    let list: Vec<Value> = (0..n)
        .map(|i| {
            let mut v = vec![0.0f32; 768];
            v[i] = 3.0;
            json!({"values": v})
        })
        .collect();
    json!({"embeddings": list})
}

fn embedder(provider: CloudProvider, base: &str, redactor: Arc<dyn TextRedactor>) -> CloudEmbedder {
    // The local mock is on 127.0.0.1: an explicitly allowed private endpoint.
    let (base, host) = validate_base_url(base, true).unwrap();
    CloudEmbedder::new(
        provider,
        provider.default_model(),
        &base,
        &host,
        "sk-test".into(),
        redactor,
        Arc::new(HttpTransport::new(true).unwrap()),
    )
}

#[test]
fn openai_body_is_redacted_and_vectors_follow_index() {
    let (base, seen) = mock_server(openai_answer);
    let e = embedder(CloudProvider::OpenAi, &base, Arc::new(EmailRedactor));
    let v = e
        .embed_passages(&["寄給 amy@example.com 謝謝", "第二段"])
        .unwrap();
    assert_eq!(v.len(), 2);
    assert!(
        (v[0][0] - 1.0).abs() < 1e-6,
        "normalised and in input order"
    );
    assert!((v[1][1] - 1.0).abs() < 1e-6);
    let log = seen.lock().unwrap();
    let (line, body) = &log[0];
    assert!(line.starts_with("/v1/embeddings "), "{line}");
    assert!(line.contains("Bearer sk-test"), "{line}");
    let sent = body.to_string();
    assert!(
        !sent.contains("amy@example.com"),
        "raw address must not leave: {sent}"
    );
    assert!(sent.contains("[EMAIL]"));
    assert_eq!(body["model"], "text-embedding-3-small");
    assert_eq!(body["dimensions"], 1536);
    assert_eq!(e.model_id(), "openai/text-embedding-3-small@1536");
    assert_eq!(
        e.egress(),
        Egress::Cloud {
            provider: "openai".into(),
            host: "127.0.0.1".into()
        }
    );
}

#[test]
fn gemini_query_and_document_task_types() {
    let (base, seen) = mock_server(gemini_answer);
    let e = embedder(CloudProvider::Gemini, &base, Arc::new(EmailRedactor));
    e.embed_passages(&["文件 x@y.tw"]).unwrap();
    let q = e.embed_query("問題 z@y.tw").unwrap();
    assert_eq!(q.len(), 768);
    let log = seen.lock().unwrap();
    assert!(
        log[0]
            .0
            .starts_with("/v1beta/models/gemini-embedding-001:batchEmbedContents ")
    );
    assert!(log[0].0.contains("x-goog-api-key: sk-test"), "{}", log[0].0);
    assert_eq!(log[0].1["requests"][0]["taskType"], "RETRIEVAL_DOCUMENT");
    assert_eq!(log[1].1["requests"][0]["taskType"], "RETRIEVAL_QUERY");
    assert_eq!(log[1].1["requests"][0]["outputDimensionality"], 768);
    for (_, body) in log.iter() {
        assert!(!body.to_string().contains("@y.tw"), "{body}");
    }
}

#[test]
fn refusing_redactor_sends_nothing() {
    let (base, seen) = mock_server(openai_answer);
    let e = embedder(
        CloudProvider::OpenAi,
        &base,
        Arc::new(RefuseRedactor("去識別化未啟用".into())),
    );
    let err = e.embed_query("機密 a@b.tw").unwrap_err();
    assert!(
        matches!(err, EmbedError::Unavailable(ref m) if m.contains("未送出")),
        "{err:?}"
    );
    assert!(e.embed_passages(&["x"]).is_err());
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(seen.lock().unwrap().is_empty(), "no request may be made");
}

#[test]
fn wrong_dimension_or_shape_is_an_error() {
    fn short(_: &Value) -> Value {
        json!({"data": [{"index": 0, "embedding": [1.0, 2.0]}]})
    }
    let (base, _) = mock_server(short);
    let e = embedder(CloudProvider::OpenAi, &base, Arc::new(EmailRedactor));
    assert!(matches!(e.embed_query("x"), Err(EmbedError::Failed(_))));
    assert!(super::super::cloud_openai::parse(&json!({}), 1).is_err());
    assert!(super::super::cloud_gemini::parse(&json!({"embeddings": []}), 1).is_err());
}

#[test]
fn base_url_rules() {
    assert_eq!(
        validate_base_url("https://api.openai.com/", false).unwrap(),
        (
            "https://api.openai.com".to_string(),
            "api.openai.com".to_string()
        )
    );
    assert!(validate_base_url("http://127.0.0.1:9999", true).is_ok());
    assert!(validate_base_url("http://127.0.0.1:9999", false).is_err());
    assert!(
        validate_base_url("http://api.openai.com", true).is_err(),
        "plain http off loopback"
    );
    assert!(validate_base_url("http://localhost.evil.com", true).is_err());
    assert!(validate_base_url("https://u:p@api.openai.com", false).is_err());
    assert!(validate_base_url("https://api.openai.com/?x=1", false).is_err());
    assert!(validate_base_url("ftp://x", false).is_err());
}

#[test]
fn private_endpoints_need_the_explicit_switch() {
    for private in [
        "http://127.0.0.1:9999",
        "https://127.0.0.1",
        "https://localhost",
        "https://api.localhost",
        "https://10.0.0.5",
        "https://192.168.1.10",
        "https://169.254.169.254",
        "https://[::1]",
        "https://[fe80::1]",
        "https://[::ffff:127.0.0.1]",
        "https://100.64.0.1",
    ] {
        let err = validate_base_url(private, false).unwrap_err();
        assert!(err.contains("allow_private_endpoint"), "{private}: {err}");
    }
    assert!(validate_base_url("https://10.0.0.5", true).is_ok());
    assert!(
        validate_base_url("https://8.8.8.8", false).is_ok(),
        "public literal"
    );
}

#[test]
fn resolved_private_addresses_are_refused_at_connect_time() {
    let url = reqwest::Url::parse("https://127.0.0.1:1/v1").unwrap();
    assert!(checked_addrs(&url, false).is_err());
    assert!(checked_addrs(&url, true).is_ok());
    // A refused endpoint is never contacted: the error comes before any send.
    let t = HttpTransport::new(false).unwrap();
    let err = t
        .post_json("https://127.0.0.1:1/v1/embeddings", &[], &json!({}))
        .unwrap_err();
    assert!(err.contains("內部網路"), "{err}");
}

#[test]
fn debug_never_prints_the_key() {
    let e = embedder(
        CloudProvider::OpenAi,
        "https://api.openai.com",
        Arc::new(EmailRedactor),
    );
    assert!(!format!("{e:?}").contains("sk-test"));
}

/// Live test 2026-10-22 caught this: a blocking client built and dropped on
/// an async worker panics ("Cannot drop a runtime …") and took the
/// knowledge worker down. Building embedders in async code must be safe.
#[tokio::test(flavor = "multi_thread")]
async fn transport_is_safe_to_build_and_drop_in_async_code() {
    for _ in 0..3 {
        let e = embedder(
            CloudProvider::OpenAi,
            "https://api.openai.com",
            Arc::new(EmailRedactor),
        );
        drop(e);
        let transport = HttpTransport::new(false).unwrap();
        let _ = &transport;
    }
}
