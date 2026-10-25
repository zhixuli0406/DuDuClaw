use super::*;
use std::collections::VecDeque;

#[derive(Default)]
struct RecordingIo {
    json: VecDeque<Value>,
    text: VecDeque<String>,
    wire: Vec<(String, String, Vec<(String, String)>, Option<Value>)>,
}
#[async_trait::async_trait]
impl LocalIo for RecordingIo {
    async fn json(&mut self, step: NativeStep<'_>) -> Result<Value, GoogleApiError> {
        self.wire.push((
            step.method().to_string(),
            step.url().into(),
            step.query().to_vec(),
            step.body().cloned(),
        ));
        Ok(self.json.pop_front().unwrap_or(json!({})))
    }
    async fn text(&mut self, step: NativeStep<'_>) -> Result<String, GoogleApiError> {
        self.wire.push((
            step.method().to_string(),
            step.url().into(),
            step.query().to_vec(),
            None,
        ));
        Ok(self.text.pop_front().unwrap_or_default())
    }
    fn checkpoint(&mut self) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn reject_derived(&mut self, _: &str) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn sanitize(&self, s: &str) -> String {
        s.into()
    }
    fn closed(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn eighteen_pre_refactor_method_and_target_goldens() {
    let cases = vec![
        ("gmail_search", json!({"query":"q"}), "GET", "/messages"),
        (
            "gmail_read",
            json!({"message_id":"abc"}),
            "GET",
            "/messages/abc",
        ),
        (
            "gmail_create_draft",
            json!({"to":"a@b.test","subject":"subject","body":"body"}),
            "POST",
            "/drafts",
        ),
        (
            "calendar_list_events",
            json!({}),
            "GET",
            "/calendars/primary/events",
        ),
        (
            "calendar_create_event",
            json!({"summary":"s","start":"2026-08-01T00:00:00Z","end":"2026-08-01T01:00:00Z"}),
            "POST",
            "/calendars/primary/events",
        ),
        (
            "sheets_read",
            json!({"spreadsheet_id":"s","range":"A1:C2"}),
            "GET",
            "/s/values/A1%3AC2",
        ),
        (
            "sheets_append",
            json!({"spreadsheet_id":"s","range":"A1","values":["x"]}),
            "POST",
            "/s/values/A1:append",
        ),
        ("forms_get", json!({"form_id":"f"}), "GET", "/forms/f"),
        (
            "forms_list_responses",
            json!({"form_id":"f"}),
            "GET",
            "/forms/f/responses",
        ),
        ("gtasks_lists", json!({}), "GET", "/users/@me/lists"),
        ("gtasks_list", json!({}), "GET", "/lists/%40default/tasks"),
        (
            "gtasks_create",
            json!({"title":"t"}),
            "POST",
            "/lists/%40default/tasks",
        ),
        (
            "gtasks_complete",
            json!({"task_id":"t"}),
            "PATCH",
            "/lists/%40default/tasks/t",
        ),
        (
            "drive_search",
            json!({"query":"q"}),
            "GET",
            "/drive/v3/files",
        ),
        (
            "drive_read",
            json!({"file_id":"d"}),
            "GET",
            "/drive/v3/files/d",
        ),
        (
            "docs_read",
            json!({"document_id":"d"}),
            "GET",
            "/documents/d",
        ),
        (
            "docs_append",
            json!({"document_id":"d","text":"x"}),
            "POST",
            "/documents/d:batchUpdate",
        ),
        (
            "slides_read",
            json!({"presentation_id":"p"}),
            "GET",
            "/presentations/p",
        ),
    ];
    for (tool, args, method, suffix) in cases {
        let template = NativeCallTemplate::prepare(tool, &args).unwrap();
        let mut io = RecordingIo::default();
        let result = run(&template, &mut io).await;
        assert_ne!(result.get("isError"), Some(&json!(true)), "{tool}");
        assert_eq!(io.wire.len(), 1, "{tool}");
        assert_eq!(io.wire[0].0, method, "{tool}");
        assert!(io.wire[0].1.ends_with(suffix), "{tool}: {}", io.wire[0].1);
        if tool == "docs_append" {
            assert_eq!(
                io.wire[0].3,
                Some(json!({"requests":[{"insertText":{"text":"x","endOfSegmentLocation":{}}}]}))
            );
        }
        if tool == "sheets_append" {
            assert_eq!(io.wire[0].3, Some(json!({"values":[["x"]]})));
        }
        if tool == "gtasks_complete" {
            assert_eq!(io.wire[0].3, Some(json!({"status":"completed"})));
        }
    }
}

#[tokio::test]
async fn gmail_fanout_preserves_repeated_query_fields_and_curated_output() {
    let template =
        NativeCallTemplate::prepare("gmail_search", &json!({"query":"from:a","max_results":100}))
            .unwrap();
    let mut io = RecordingIo {
        json: VecDeque::from([
            json!({"messages":[{"id":"abc","threadId":"thread"}]}),
            json!({"payload":{"headers":[{"name":"From","value":"Alice"},{"name":"Subject","value":"Subject"},{"name":"Date","value":"Today"}]},"snippet":"snippet","ignored":"not returned"}),
        ]),
        ..Default::default()
    };
    let result = run(&template, &mut io).await;
    assert_eq!(io.wire.len(), 2);
    assert_eq!(
        io.wire[0].2,
        vec![
            ("q".into(), "from:a".into()),
            ("maxResults".into(), "25".into())
        ]
    );
    assert_eq!(
        io.wire[1].2,
        vec![
            ("format".into(), "metadata".into()),
            ("metadataHeaders".into(), "From".into()),
            ("metadataHeaders".into(), "Subject".into()),
            ("metadataHeaders".into(), "Date".into())
        ]
    );
    let decoded: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        decoded,
        json!({"count":1,"messages":[{"id":"abc","thread_id":"thread","from":"Alice","subject":"Subject","date":"Today","snippet":"snippet"}]})
    );
}

#[tokio::test]
async fn drive_uses_fixed_export_mapping_and_ignores_provider_export_link() {
    let template = NativeCallTemplate::prepare("drive_read", &json!({"file_id":"doc"})).unwrap();
    let mut io = RecordingIo {
        json: VecDeque::from([
            json!({"id":"doc","name":"Title","mimeType":"application/vnd.google-apps.document","exportLinks":{"text/plain":"https://unexpected.invalid/steal"}}),
        ]),
        text: VecDeque::from(["document text".into()]),
        ..Default::default()
    };
    let result = run(&template, &mut io).await;
    assert_eq!(io.wire.len(), 2);
    assert_eq!(
        io.wire[1].1,
        "https://www.googleapis.com/drive/v3/files/doc/export"
    );
    assert_eq!(io.wire[1].2, vec![("mimeType".into(), "text/plain".into())]);
    assert!(!result.to_string().contains("unexpected.invalid"));
    assert!(result.to_string().contains("document text"));
}
impl LocalRender for RecordingIo {
    fn inspect_curated(&mut self, _: &Value) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn refuse(&mut self) -> GoogleApiError {
        GoogleApiError::Api {
            status: 502,
            message: "fixture refusal".into(),
        }
    }
}

#[test]
fn scrub_reflected_strings_cannot_reintroduce_short_ascii_bearers() {
    for token in [
        "redacted",
        "r",
        "a",
        "-",
        ".",
        "_",
        "=",
        "/",
        "+",
        "0",
        "fixture-original-never-output",
    ] {
        let reflected = format!("before:{token}{token}:after");
        let mut value = Value::String(reflected.clone());
        assert!(scrub_final(&mut value, token));
        let text = value.as_str().unwrap();
        assert_eq!(text, reflected.replace(token, "•••"));
        assert!(
            !text.contains(token),
            "the reflected string retained token {token:?}"
        );
        assert!("•••".chars().all(|c| !c.is_ascii()));
    }
}
