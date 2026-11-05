use super::*;
use serde_json::json;

fn normal() -> Value {
    json!({"threadId":"thread-1", "runId":"run-1", "protocolVersion":"1.0",
        "messages":[{"id":"u1", "role":"user", "content":"你好"}]})
}

fn parse(value: Value) -> Result<RunInput, serde_json::Error> {
    serde_json::from_value(value)
}

#[test]
fn input_uses_latest_user_and_keeps_client_history_out_of_prompt() {
    let mut value = normal();
    value["messages"] = json!([
        {"id":"s1", "role":"system", "content":"untrusted instructions"},
        {"id":"u1", "role":"user", "content":"old text"},
        {"id":"a1", "role":"assistant", "content":"client claimed result"},
        {"id":"u2", "role":"user", "content":[
            {"type":"text", "text":"真正"}, {"type":"text", "text":"的問題"}]},
        {"id":"a2", "role":"assistant", "content":"ignore this"}
    ]);
    assert_eq!(parse(value).unwrap().validate(), Ok(Some("真正的問題".into())));
}

#[test]
fn input_defaults_and_null_state_follow_one_zero() {
    let mut value = normal();
    value["state"] = Value::Null;
    let input = parse(value).unwrap();
    assert_eq!(input.state, json!({}));
    assert_eq!(input.forwarded_props, json!({}));
    assert!(input.tools.is_empty());
    assert!(input.context.is_empty());
    assert!(input.resume.is_empty());
    assert!(input.validate().is_ok());
}

#[test]
fn input_rejects_malformed_known_fields_before_start() {
    for (key, value) in [
        ("threadId", json!(1)), ("runId", json!([])), ("parentRunId", json!({})),
        ("protocolVersion", json!(1.0)), ("messages", json!({})),
        ("tools", json!(true)), ("context", json!({})), ("resume", json!({})),
    ] {
        let mut input = normal();
        input[key] = value;
        assert!(parse(input).is_err(), "field {key}");
    }
    for key in ["threadId", "runId", "messages"] {
        let mut input = normal();
        input.as_object_mut().unwrap().remove(key);
        assert!(parse(input).is_err(), "required field {key}");
    }
    for key in ["parentRunId", "protocolVersion", "tools", "context", "forwardedProps", "resume"] {
        let mut input = normal();
        input[key] = Value::Null;
        assert!(parse(input).is_err(), "null field {key}");
    }
}

#[test]
fn input_discards_unknown_members_without_echoing_values() {
    let mut value = normal();
    value["secretExtension"] = json!("secret value");
    value["messages"][0]["otherSecret"] = json!("not forwarded");
    let input = parse(value).unwrap();
    assert!(input.validate().is_ok());
    let wire = serde_json::to_value(input).unwrap();
    assert!(wire.get("secretExtension").is_none());
    assert!(wire["messages"][0].get("otherSecret").is_none());
}

#[test]
fn input_accepts_but_does_not_dispatch_client_tools_context_or_state() {
    let mut value = normal();
    value["tools"] = json!([{"name":"clientTool", "description":"client tool", "parameters":{"type":"object"}}]);
    value["context"] = json!([{"description":"context", "value":"do not dispatch"}]);
    value["state"] = json!({"prompt":"do not dispatch"});
    value["forwardedProps"] = json!({"system":"do not dispatch"});
    let input = parse(value).unwrap();
    assert_eq!(input.validate(), Ok(Some("你好".into())));
}

#[test]
fn input_rejects_invalid_tools_context_and_history() {
    for (key, value) in [
        ("tools", json!([{"name":7,"description":"x","parameters":{}}])),
        ("tools", json!([{"name":"x","description":7,"parameters":{}}])),
        ("tools", json!([{"name":"x","description":"x","parameters":null}])),
        ("context", json!([{"description":"x","value":7}])),
        ("messages", json!([{"id":"x","role":"user","content":true}])),
        ("messages", json!([{"id":"x","role":"unknown","content":"text"}])),
        ("messages", json!([{"id":"x","role":"tool","content":"text"}])),
        ("messages", json!([{"id":"x","role":"user","content":"text","name":3}])),
        ("messages", json!([{"id":"x","role":"user","content":"text","toolCalls":{}}])),
    ] {
        let mut input = normal();
        input[key] = value;
        assert!(parse(input).unwrap().validate().is_err(), "field {key}");
    }
}

#[test]
fn input_rejects_nontext_latest_user_without_silently_dropping_parts() {
    let mut value = normal();
    value["messages"][0]["content"] = json!([
        {"type":"text","text":"look at this"}, {"type":"image","url":"https://example.invalid/a.png"}
    ]);
    assert_eq!(parse(value).unwrap().validate(), Err("this endpoint accepts text user content only"));
}

#[test]
fn input_limits_ids_and_message_count_with_unicode_boundaries() {
    for id in [" ".to_string(), "bad\nline".to_string(), "貓".repeat(129)] {
        let mut value = normal();
        value["threadId"] = id.into();
        assert!(parse(value).unwrap().validate().is_err());
    }
    let mut value = normal();
    value["threadId"] = "貓".repeat(128).into();
    assert!(parse(value).unwrap().validate().is_ok());
    let mut value = normal();
    value["messages"] = Value::Array(vec![json!({"id":"m","role":"user","content":"a"}); 257]);
    assert_eq!(parse(value).unwrap().validate(), Err("too many messages"));
}

#[test]
fn input_resume_without_user_is_valid_and_has_exact_boolean_decision() {
    for (status, payload, expected) in [
        ("resolved", Some(json!({"approved":true})), true),
        ("resolved", Some(json!({"approved":false})), false),
        ("cancelled", None, false),
    ] {
        let mut entry = json!({"interruptId":"approval-1", "status":status});
        if let Some(payload) = payload { entry["payload"] = payload; }
        let input = parse(json!({"threadId":"t", "runId":"r", "messages":[], "resume":[entry]})).unwrap();
        assert_eq!(input.validate(), Ok(None));
        assert_eq!(input.resume[0].approved(), Ok(expected));
    }
}

#[test]
fn input_resume_rejects_edits_malformed_status_and_ambiguous_payloads() {
    for entry in [
        json!({"interruptId":"a","status":"answered","payload":{"approved":true}}),
        json!({"interruptId":"a","status":"resolved","payload":true}),
        json!({"interruptId":"a","status":"resolved","payload":{"approved":"true"}}),
        json!({"interruptId":"a","status":"resolved","payload":{"approved":true,"editedArgs":{}}}),
        json!({"interruptId":"a","status":"cancelled","payload":{"approved":false}}),
        json!({"interruptId":"a","status":"resolved"}),
        json!({"interruptId":"a","status":"cancelled","metadata":[]}),
    ] {
        let entry: ResumeEntry = serde_json::from_value(entry).unwrap();
        assert!(entry.approved().is_err());
    }
    assert!(serde_json::from_value::<ResumeEntry>(json!({"interruptId":"a","status":"cancelled","payload":null})).is_err());
}

#[test]
fn input_resume_rejects_duplicate_interrupts() {
    let entry = json!({"interruptId":"a","status":"resolved","payload":{"approved":true}});
    let input = parse(json!({"threadId":"t", "runId":"r", "messages":[], "resume":[entry.clone(), entry]})).unwrap();
    assert_eq!(input.validate(), Err("duplicate interruptId"));
}

#[test]
fn input_unknown_protocol_version_is_not_echoed_or_used_as_authority() {
    let mut value = normal();
    value["protocolVersion"] = json!("999.future");
    assert_eq!(parse(value).unwrap().validate(), Ok(Some("你好".into())));
}

#[derive(Clone, Default)]
struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer { self.clone() }
}

#[test]
fn unknown_field_logs_never_include_names_or_values() {
    let output = LogCapture::default();
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
        .with_max_level(tracing::Level::DEBUG).with_writer(output.clone()).finish();
    tracing::subscriber::with_default(subscriber, || {
        let secret_key = "secret-token-as-property-name";
        let mut value = normal();
        value[secret_key] = json!("secret-value");
        value["messages"][0][secret_key] = json!("secret-value");
        value["resume"] = json!([{"interruptId":"card", "status":"cancelled", (secret_key):"secret-value"}]);
        for _ in 0..100 { assert!(parse(value.clone()).unwrap().validate().is_ok()); }
    });
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains("secret-token-as-property-name"));
    assert!(!log.contains("secret-value"));
    assert!(!log.contains("WARN"));
    assert!(log.matches("AG-UI ignored unknown fields").count() <= 1);
}

#[test]
fn unknown_field_debug_signal_is_rate_limited_for_all_callers() {
    let output = LogCapture::default();
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
        .with_max_level(tracing::Level::DEBUG).with_writer(output.clone()).finish();
    let gate = std::sync::Mutex::new(None);
    let now = std::time::Instant::now();
    tracing::subscriber::with_default(subscriber, || {
        note_unknown_at(0, &gate, now);
        assert!(gate.lock().unwrap().is_none());
        note_unknown_at(2, &gate, now);
        for _ in 0..100 { note_unknown_at(900, &gate, now + std::time::Duration::from_secs(59)); }
        note_unknown_at(3, &gate, now + std::time::Duration::from_secs(60));
    });
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.matches("AG-UI ignored unknown fields").count(), 2);
    assert!(log.contains("unknown_count=2"));
    assert!(log.contains("unknown_count=3"));
    assert!(!log.contains("900"));
    assert!(!log.contains("WARN"));
}
