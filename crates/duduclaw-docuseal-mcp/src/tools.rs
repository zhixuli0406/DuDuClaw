//! Tool definitions + the pure mapping from tool arguments to an HTTP plan.
//!
//! Every tool resolves to an [`HttpPlan`] (method, path, query, body) without
//! touching the network, so the whole surface is unit-testable offline.
//! Endpoint shapes follow the DocuSeal OpenAPI 3.1 spec
//! (`https://console.docuseal.com/openapi.yml`, fetched 2026-07-30).

use serde_json::{json, Map, Value};

/// HTTP method of a plan (only the verbs the DocuSeal API needs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

/// One fully-described REST call, ready for [`crate::DocusealApi::execute`].
#[derive(Debug, Clone, PartialEq)]
pub struct HttpPlan {
    pub method: Method,
    /// Path relative to the base URL, starting with `/`.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
}

/// Mapping failure: bad/missing arguments (reported as a tool error, never a
/// protocol error — the model can correct itself and retry).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ArgError(pub String);

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn req_u64(args: &Value, key: &str) -> Result<u64, ArgError> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| ArgError(format!("missing required integer argument '{key}'")))
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ArgError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ArgError(format!("missing required string argument '{key}'")))
}

/// Copy optional scalar args (as strings) into a query list.
fn push_query(args: &Value, keys: &[&str], query: &mut Vec<(String, String)>) {
    for k in keys {
        match args.get(*k) {
            Some(Value::String(s)) if !s.is_empty() => query.push((k.to_string(), s.clone())),
            Some(Value::Number(n)) => query.push((k.to_string(), n.to_string())),
            Some(Value::Bool(b)) => query.push((k.to_string(), b.to_string())),
            _ => {}
        }
    }
}

/// Copy optional args verbatim into a JSON body object.
fn copy_fields(args: &Value, keys: &[&str], body: &mut Map<String, Value>) {
    for k in keys {
        if let Some(v) = args.get(*k) {
            if !v.is_null() {
                body.insert(k.to_string(), v.clone());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool → plan mapping
// ---------------------------------------------------------------------------

/// Map one tool call to its HTTP plan. Unknown tool ⇒ `Err` (the RPC layer
/// converts it to a tool error).
pub fn plan_for(tool: &str, args: &Value) -> Result<HttpPlan, ArgError> {
    match tool {
        "docuseal_list_templates" => {
            let mut query = Vec::new();
            push_query(args, &["q", "folder", "external_id", "archived", "limit", "after"], &mut query);
            Ok(HttpPlan { method: Method::Get, path: "/templates".into(), query, body: None })
        }
        "docuseal_get_template" => {
            let id = req_u64(args, "template_id")?;
            Ok(HttpPlan { method: Method::Get, path: format!("/templates/{id}"), query: vec![], body: None })
        }
        "docuseal_create_template_from_pdf" => {
            // `file` accepts base64 content OR a downloadable URL — the API
            // takes both in the same `documents[].file` field.
            let file = req_str(args, "file")?;
            let doc_name = args
                .get("document_name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("Document");
            let mut body = Map::new();
            copy_fields(args, &["name", "external_id", "folder_name"], &mut body);
            body.insert("documents".into(), json!([{ "name": doc_name, "file": file }]));
            Ok(HttpPlan { method: Method::Post, path: "/templates/pdf".into(), query: vec![], body: Some(Value::Object(body)) })
        }
        "docuseal_create_submission" => {
            let template_id = req_u64(args, "template_id")?;
            let submitters = args
                .get("submitters")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or_else(|| ArgError("missing required non-empty array argument 'submitters'".into()))?;
            for (i, s) in submitters.iter().enumerate() {
                if s.get("email").and_then(Value::as_str).map(str::trim).unwrap_or("").is_empty() {
                    return Err(ArgError(format!("submitters[{i}] is missing required 'email'")));
                }
            }
            let mut body = Map::new();
            body.insert("template_id".into(), json!(template_id));
            body.insert("submitters".into(), Value::Array(submitters.clone()));
            copy_fields(args, &["send_email", "order", "message", "expire_at", "reply_to", "completed_redirect_url"], &mut body);
            Ok(HttpPlan { method: Method::Post, path: "/submissions".into(), query: vec![], body: Some(Value::Object(body)) })
        }
        "docuseal_get_submission" => {
            let id = req_u64(args, "submission_id")?;
            Ok(HttpPlan { method: Method::Get, path: format!("/submissions/{id}"), query: vec![], body: None })
        }
        "docuseal_list_submissions" => {
            let mut query = Vec::new();
            push_query(args, &["template_id", "status", "q", "template_folder", "archived", "limit", "after"], &mut query);
            Ok(HttpPlan { method: Method::Get, path: "/submissions".into(), query, body: None })
        }
        "docuseal_archive_submission" => {
            let id = req_u64(args, "submission_id")?;
            Ok(HttpPlan { method: Method::Delete, path: format!("/submissions/{id}"), query: vec![], body: None })
        }
        "docuseal_get_submission_documents" => {
            let id = req_u64(args, "submission_id")?;
            Ok(HttpPlan { method: Method::Get, path: format!("/submissions/{id}/documents"), query: vec![], body: None })
        }
        "docuseal_resend_submitter_email" => {
            let id = req_u64(args, "submitter_id")?;
            let mut body = Map::new();
            body.insert("send_email".into(), json!(true));
            copy_fields(args, &["message"], &mut body);
            Ok(HttpPlan { method: Method::Put, path: format!("/submitters/{id}"), query: vec![], body: Some(Value::Object(body)) })
        }
        "docuseal_update_submitter" => {
            let id = req_u64(args, "submitter_id")?;
            let mut body = Map::new();
            copy_fields(args, &["name", "email", "phone", "values", "external_id", "metadata"], &mut body);
            if body.is_empty() {
                return Err(ArgError("update_submitter needs at least one field to change".into()));
            }
            Ok(HttpPlan { method: Method::Put, path: format!("/submitters/{id}"), query: vec![], body: Some(Value::Object(body)) })
        }
        other => Err(ArgError(format!("unknown tool '{other}'"))),
    }
}

// ---------------------------------------------------------------------------
// tools/list definitions
// ---------------------------------------------------------------------------

fn obj_schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

/// The `tools/list` payload: name / description / inputSchema per tool.
pub fn tool_definitions() -> Vec<Value> {
    let submitter_schema = json!({
        "type": "array",
        "description": "Signing parties. Each: {email (required), name, role (must match a template role when the template defines several), phone (E.164), values (object: field name → prefill value)}.",
        "items": { "type": "object", "properties": {
            "email": { "type": "string" },
            "name": { "type": "string" },
            "role": { "type": "string" },
            "phone": { "type": "string" },
            "values": { "type": "object" }
        }, "required": ["email"] }
    });
    vec![
        json!({
            "name": "docuseal_list_templates",
            "description": "List document templates. Optional filters: q (name contains), folder, external_id, archived, limit (max 100), after (pagination cursor from a previous response).",
            "inputSchema": obj_schema(json!({
                "q": {"type": "string"}, "folder": {"type": "string"},
                "external_id": {"type": "string"}, "archived": {"type": "boolean"},
                "limit": {"type": "integer"}, "after": {"type": "integer"}
            }), &[]),
        }),
        json!({
            "name": "docuseal_get_template",
            "description": "Get one template (fields, roles/submitters, documents, shared_link) by id.",
            "inputSchema": obj_schema(json!({ "template_id": {"type": "integer"} }), &["template_id"]),
        }),
        json!({
            "name": "docuseal_create_template_from_pdf",
            "description": "Create (or update, when external_id matches an existing one) a template from a PDF. 'file' is base64-encoded PDF content OR a downloadable URL. Embed {{Field;role=Signer1;type=signature}} text tags in the PDF to auto-place fields.",
            "inputSchema": obj_schema(json!({
                "file": {"type": "string", "description": "Base64 PDF content or a downloadable URL"},
                "name": {"type": "string"}, "document_name": {"type": "string"},
                "external_id": {"type": "string"}, "folder_name": {"type": "string"}
            }), &["file"]),
        }),
        json!({
            "name": "docuseal_create_submission",
            "description": "Send a template out for signature. Returns one entry per submitter incl. the signing link (embed_src). send_email defaults to true; message = {subject, body} customizes the invite.",
            "inputSchema": obj_schema(json!({
                "template_id": {"type": "integer"},
                "submitters": submitter_schema,
                "send_email": {"type": "boolean"},
                "order": {"type": "string", "enum": ["preserved", "random"]},
                "message": {"type": "object", "properties": {"subject": {"type": "string"}, "body": {"type": "string"}}},
                "expire_at": {"type": "string", "description": "e.g. 2026-09-01 12:00:00 UTC"},
                "reply_to": {"type": "string"},
                "completed_redirect_url": {"type": "string"}
            }), &["template_id", "submitters"]),
        }),
        json!({
            "name": "docuseal_get_submission",
            "description": "Get one submission: per-submitter status (sent/opened/completed/declined), events, signed documents, audit_log_url.",
            "inputSchema": obj_schema(json!({ "submission_id": {"type": "integer"} }), &["submission_id"]),
        }),
        json!({
            "name": "docuseal_list_submissions",
            "description": "List submissions. Filters: template_id, status (pending|completed|declined|expired), q, template_folder, archived, limit, after.",
            "inputSchema": obj_schema(json!({
                "template_id": {"type": "integer"}, "status": {"type": "string"},
                "q": {"type": "string"}, "template_folder": {"type": "string"},
                "archived": {"type": "boolean"}, "limit": {"type": "integer"}, "after": {"type": "integer"}
            }), &[]),
        }),
        json!({
            "name": "docuseal_archive_submission",
            "description": "Archive (void) a submission — pending signature requests stop being actionable.",
            "inputSchema": obj_schema(json!({ "submission_id": {"type": "integer"} }), &["submission_id"]),
        }),
        json!({
            "name": "docuseal_get_submission_documents",
            "description": "Get download URLs of a submission's documents (the signed files once completed).",
            "inputSchema": obj_schema(json!({ "submission_id": {"type": "integer"} }), &["submission_id"]),
        }),
        json!({
            "name": "docuseal_resend_submitter_email",
            "description": "Re-send the signature request email to one submitter. Optional message = {subject, body}.",
            "inputSchema": obj_schema(json!({
                "submitter_id": {"type": "integer"},
                "message": {"type": "object", "properties": {"subject": {"type": "string"}, "body": {"type": "string"}}}
            }), &["submitter_id"]),
        }),
        json!({
            "name": "docuseal_update_submitter",
            "description": "Update a pending submitter: contact info (name/email/phone), prefilled values, external_id, metadata.",
            "inputSchema": obj_schema(json!({
                "submitter_id": {"type": "integer"},
                "name": {"type": "string"}, "email": {"type": "string"}, "phone": {"type": "string"},
                "values": {"type": "object"}, "external_id": {"type": "string"}, "metadata": {"type": "object"}
            }), &["submitter_id"]),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_definition_maps() {
        // Each advertised tool must be routable by plan_for (with minimal
        // args) or fail with a *missing-argument* error — never unknown-tool.
        for def in tool_definitions() {
            let name = def["name"].as_str().unwrap();
            let err = plan_for(name, &json!({})).err();
            if let Some(ArgError(msg)) = err {
                assert!(!msg.contains("unknown tool"), "{name} not routed: {msg}");
            }
        }
    }

    #[test]
    fn list_templates_query_mapping() {
        let plan = plan_for(
            "docuseal_list_templates",
            &json!({"q": "contract", "archived": false, "limit": 5}),
        )
        .unwrap();
        assert_eq!(plan.method, Method::Get);
        assert_eq!(plan.path, "/templates");
        assert!(plan.query.contains(&("q".into(), "contract".into())));
        assert!(plan.query.contains(&("archived".into(), "false".into())));
        assert!(plan.query.contains(&("limit".into(), "5".into())));
        assert!(plan.body.is_none());
    }

    #[test]
    fn create_submission_requires_submitter_email() {
        let err = plan_for(
            "docuseal_create_submission",
            &json!({"template_id": 7, "submitters": [{"name": "no-email"}]}),
        )
        .err()
        .unwrap();
        assert!(err.0.contains("email"));

        let plan = plan_for(
            "docuseal_create_submission",
            &json!({
                "template_id": 7,
                "submitters": [{"email": "a@b.c", "role": "Signer1", "values": {"價格": "3萬"}}],
                "send_email": false,
                "message": {"subject": "請簽署", "body": "合約如附件"}
            }),
        )
        .unwrap();
        assert_eq!(plan.method, Method::Post);
        assert_eq!(plan.path, "/submissions");
        let body = plan.body.unwrap();
        assert_eq!(body["template_id"], json!(7));
        assert_eq!(body["send_email"], json!(false));
        assert_eq!(body["submitters"][0]["values"]["價格"], json!("3萬"));
        assert_eq!(body["message"]["subject"], json!("請簽署"));
    }

    #[test]
    fn template_from_pdf_wraps_documents() {
        let plan = plan_for(
            "docuseal_create_template_from_pdf",
            &json!({"file": "https://example.com/contract.pdf", "name": "NDA", "document_name": "NDA.pdf"}),
        )
        .unwrap();
        let body = plan.body.unwrap();
        assert_eq!(body["name"], json!("NDA"));
        assert_eq!(body["documents"][0]["file"], json!("https://example.com/contract.pdf"));
        assert_eq!(body["documents"][0]["name"], json!("NDA.pdf"));
    }

    #[test]
    fn resend_forces_send_email_true() {
        let plan = plan_for("docuseal_resend_submitter_email", &json!({"submitter_id": 42})).unwrap();
        assert_eq!(plan.method, Method::Put);
        assert_eq!(plan.path, "/submitters/42");
        assert_eq!(plan.body.unwrap()["send_email"], json!(true));
    }

    #[test]
    fn update_submitter_rejects_empty_change() {
        let err = plan_for("docuseal_update_submitter", &json!({"submitter_id": 1})).err().unwrap();
        assert!(err.0.contains("at least one field"));
    }

    #[test]
    fn archive_and_documents_paths() {
        assert_eq!(
            plan_for("docuseal_archive_submission", &json!({"submission_id": 9})).unwrap().path,
            "/submissions/9"
        );
        assert_eq!(
            plan_for("docuseal_get_submission_documents", &json!({"submission_id": 9})).unwrap().path,
            "/submissions/9/documents"
        );
    }

    #[test]
    fn unknown_tool_is_an_arg_error() {
        assert!(plan_for("docuseal_nope", &json!({})).is_err());
    }
}
