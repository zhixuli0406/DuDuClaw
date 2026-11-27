use super::{args::{self, ArgError}, program::{self, AttachmentContent, OutcomeError}, scope::{Plan, ScopeVerdict}, transport::{ClosedMime, WriteCall}};
use axum::body::Bytes;
use serde_json::{json, Value};

const FILE: &str = "File_0123456789";
const PARENT: &str = "Parent_0123456789";
const OLD: &str = "OldParent_0123456789";

fn verdict(args: &args::DriveArgs) -> ScopeVerdict {
    let target = match args::plan(args) {
        Plan::Create { parent } => parent,
        Plan::Copy { source, .. } => Some(source),
        Plan::Modify { target, .. } | Plan::Move { target, .. } | Plan::Trash { target } => Some(target),
    };
    ScopeVerdict::InScope {
        matched: vec![], target_id: target.map(|id| id.as_str().to_owned()),
        parents: Some(vec![OLD.into()]),
    }
}

fn call(tool: &str, value: Value) -> WriteCall {
    let args = args::parse(tool, &value).unwrap();
    let mut calls = program::build(&args, &verdict(&args), None, Some("text/plain")).unwrap();
    assert_eq!(calls.len(), 1);
    calls.remove(0)
}

fn route(call: &WriteCall, method: &str, path: &str) {
    let (actual_method, url) = call.method_and_url();
    assert_eq!(actual_method.as_str(), method);
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("www.googleapis.com"));
    assert_eq!(url.path(), path);
    assert!(url.query_pairs().any(|(k, v)| k == "supportsAllDrives" && v == "true"));
    assert!(url.query_pairs().any(|(k, v)| k == "fields" && v == "id,name,mimeType,parents,driveId,trashed"));
}

fn body(call: &WriteCall) -> Value {
    serde_json::from_slice(&call.test_body().unwrap().unwrap().1).unwrap()
}

#[test]
fn create_folder_shape_and_root() {
    let c = call("drive_create_file", json!({"name":"  資料夾  ","folder":true,"parent_folder_id":PARENT}));
    assert!(matches!(&c, WriteCall::DriveCreateFolder { .. }));
    route(&c, "POST", "/drive/v3/files");
    assert_eq!(body(&c), json!({"name":"資料夾","mimeType":"application/vnd.google-apps.folder","parents":[PARENT]}));
    let c = call("drive_create_file", json!({"name":"root","folder":true}));
    assert_eq!(body(&c), json!({"name":"root","mimeType":"application/vnd.google-apps.folder"}));
}

#[test]
fn create_text_shape_and_conversion() {
    let c = call("drive_create_file", json!({"name":"表","text":"甲,乙","parent_folder_id":PARENT,"mime_type":"text/csv","convert_to":"spreadsheet"}));
    route(&c, "POST", "/upload/drive/v3/files");
    assert!(c.method_and_url().1.query_pairs().any(|(k,v)| k == "uploadType" && v == "multipart"));
    assert!(matches!(&c, WriteCall::DriveCreateMultipart { content_type: ClosedMime::Csv, content, .. } if content.as_ref() == "甲,乙".as_bytes()));
    let wire = String::from_utf8(c.test_body().unwrap().unwrap().1).unwrap();
    let metadata: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap().split("\r\n").next().unwrap()).unwrap();
    assert_eq!(metadata, json!({"name":"表","parents":[PARENT],"mimeType":"application/vnd.google-apps.spreadsheet"}));
}

#[test]
fn copy_update_move_trash_shapes() {
    let c = call("drive_copy_file", json!({"file_id":FILE,"parent_folder_id":PARENT,"name":"副本"}));
    assert!(matches!(&c, WriteCall::DriveCopy { .. }));
    route(&c, "POST", &format!("/drive/v3/files/{FILE}/copy"));
    assert_eq!(body(&c), json!({"parents":[PARENT],"name":"副本"}));
    let c = call("drive_update_file", json!({"file_id":FILE,"name":"新名稱","description":"說明"}));
    assert!(matches!(&c, WriteCall::DriveUpdateMetadata { .. }));
    route(&c, "PATCH", &format!("/drive/v3/files/{FILE}"));
    assert_eq!(body(&c), json!({"name":"新名稱","description":"說明"}));
    let c = call("drive_update_file", json!({"file_id":FILE,"text":"新內容"}));
    route(&c, "PATCH", &format!("/upload/drive/v3/files/{FILE}"));
    assert!(matches!(c, WriteCall::DriveUpdateContent { content_type: ClosedMime::PlainText, content, .. } if content.as_ref() == "新內容".as_bytes()));
    let a = args::parse("drive_move_file", &json!({"file_id":FILE,"destination_folder_id":PARENT})).unwrap();
    let v = verdict(&a);
    let c = program::build(&a, &v, None, None).unwrap().remove(0);
    route(&c, "PATCH", &format!("/drive/v3/files/{FILE}"));
    assert!(matches!(&c, WriteCall::DriveMove { add_parent, remove_parents, .. } if add_parent.as_str() == PARENT && remove_parents.len() == 1 && remove_parents[0].as_str() == OLD));
    assert!(c.method_and_url().1.query_pairs().any(|(k,v)| k == "removeParents" && v == OLD));
    assert_eq!(program::expected_parents(&a, &v), vec![PARENT.to_owned()]);
    let c = call("drive_trash_file", json!({"file_id":FILE}));
    assert!(matches!(&c, WriteCall::DriveTrash { .. }));
    route(&c, "PATCH", &format!("/drive/v3/files/{FILE}"));
    assert_eq!(c.test_body().unwrap().unwrap().1.as_slice(), br#"{"trashed":true}"#);
}

#[test]
fn ids_reject_injection_and_accept_links() {
    for id in ["x/permissions".into(), "../x".into(), "%2F".into(), "short".into(), "x".repeat(129),
        format!("http://drive.google.com/file/d/{FILE}/view"),
        format!("https://drive.google.com.evil.com/file/d/{FILE}/view"),
        format!("https://drive.google.com/file/d/{FILE}%2Fpermissions/view"),
        format!("https://drive.google.com/../file/d/{FILE}/view"),
        format!("https://drive.google.com/%2e%2e/file/d/{FILE}/view"),
        format!("https://drive.google.com/drive/folders/{FILE}?id={PARENT}"),
        format!("https://drive.google.com/open?id={FILE}&id={PARENT}"),
    ] {
        assert_eq!(args::parse("drive_trash_file", &json!({"file_id":id})).unwrap_err(), ArgError::InvalidId);
    }
    for link in [FILE.into(), format!("https://drive.google.com/drive/folders/{FILE}"),
        format!("https://drive.google.com/drive/u/0/folders/{FILE}"),
        format!("https://drive.google.com/file/d/{FILE}/view"),
        format!("https://docs.google.com/document/d/{FILE}/edit"),
        format!("https://docs.google.com/spreadsheets/d/{FILE}/edit"),
        format!("https://drive.google.com/open?id={FILE}"),
        format!("https://docs.google.com/?id={FILE}"),
    ] {
        for (tool, value) in [
            ("drive_trash_file", json!({"file_id":link})),
            ("drive_create_file", json!({"name":"x","folder":true,"parent_folder_id":link})),
            ("drive_move_file", json!({"file_id":FILE,"destination_folder_id":link})),
        ] { assert!(args::parse(tool, &value).is_ok(), "{value}"); }
    }
}

#[test]
fn closed_arguments_and_content_combinations() {
    assert_eq!(args::parse("docs_insert_text", &json!({})).unwrap_err(), ArgError::UnknownTool);
    for value in [json!({"name":"x"}), json!({"name":"x","folder":false}),
        json!({"name":"x","text":"","folder":true}), json!({"name":"x","text":"","attachment":"a.txt"}),
        json!({"name":"x","folder":true,"convert_to":"document"}), json!({"name":"x","folder":true,"mime_type":"text/plain"}),
        json!({"name":"x","attachment":"a.txt","mime_type":"text/plain"}), json!({"name":"x","text":"","convert_to":"pdf"}),
        json!({"name":"x","text":"","mime_type":"application/json"}), json!({"name":"x","text":null}),
        json!({"name":"x","folder":"true"}), json!({"name":"x","text":"","extra":true}),
    ] { assert!(args::parse("drive_create_file", &value).is_err(), "{value}"); }
    assert!(args::parse("drive_create_file", &json!({"name":"a/b","text":"","folder":false})).is_ok());
    for key in ["removeParents", "remove_parents"] {
        let mut value = json!({"file_id":FILE,"destination_folder_id":PARENT});
        value[key] = json!(OLD);
        assert!(args::parse("drive_move_file", &value).is_err());
    }
    assert!(args::parse("drive_update_file", &json!({"file_id":FILE})).is_err());
    assert!(args::parse("drive_trash_file", &json!({"file_id":42})).is_err());
    assert!(args::parse("drive_copy_file", &json!({"file_id":FILE})).is_err());
}

#[test]
fn unicode_and_attachment_limits() {
    for size in [100_000, 100_001] {
        for (tool, value) in [
            ("drive_create_file", json!({"name":"x","text":"字".repeat(size)})),
            ("drive_update_file", json!({"file_id":FILE,"text":"字".repeat(size)})),
        ] { assert_eq!(args::parse(tool, &value).is_ok(), size == 100_000); }
    }
    for name in ["a/b", "a\\b", "a..b", "\0", ""] {
        assert!(args::parse("drive_create_file", &json!({"name":"x","attachment":name})).is_err());
    }
    for name in [" ", "a\n", "\0", &"字".repeat(256)] {
        assert!(args::parse("drive_create_file", &json!({"name":name,"folder":true})).is_err());
    }
    for size in [4_000, 4_001] {
        assert_eq!(args::parse("drive_update_file", &json!({"file_id":FILE,"description":"字".repeat(size)})).is_ok(), size == 4_000);
    }
    for (file_name, mime) in [("a.txt", "plain"), ("a.csv", "csv"), ("a.md", "markdown"), ("a.bin", "unknown")] {
        let a = args::parse("drive_create_file", &json!({"name":"x","attachment":file_name})).unwrap();
        for size in [5 * 1024 * 1024, 5 * 1024 * 1024 + 1] {
            let result = program::build(&a, &verdict(&a), Some(AttachmentContent { file_name: file_name.into(), bytes: Bytes::from(vec![0; size]), sha256: "executor-verified".into() }), None);
            if size > 5 * 1024 * 1024 { assert_eq!(result.unwrap_err(), ArgError::ContentTooLarge); continue; }
            let calls = result.unwrap();
            let WriteCall::DriveCreateMultipart { content_type, .. } = &calls[0] else { panic!("multipart expected") };
            assert!(matches!((mime, content_type), ("plain", ClosedMime::PlainText) | ("csv", ClosedMime::Csv) | ("markdown", ClosedMime::Markdown) | ("unknown", ClosedMime::UnknownAttachment)));
        }
    }
}

#[test]
fn build_fails_closed_on_scope_mime_and_missing_attachment() {
    let a = args::parse("drive_update_file", &json!({"file_id":FILE,"text":"x"})).unwrap();
    for mime in ["application/vnd.google-apps.document", "application/vnd.google-apps.spreadsheet", "application/vnd.google-apps.future", "application/vnd.google-apps.future.subtype"] {
        assert_eq!(program::build(&a, &verdict(&a), None, Some(mime)).unwrap_err(), ArgError::GoogleNativeContent);
    }
    assert!(program::build(&a, &verdict(&a), None, None).is_err());
    assert!(program::build(&a, &ScopeVerdict::Failed, None, Some("text/plain")).is_err());
    let mut v = verdict(&a);
    if let ScopeVerdict::InScope { target_id, .. } = &mut v { *target_id = Some(PARENT.into()); }
    assert!(program::build(&a, &v, None, Some("text/plain")).is_err());
    let a = args::parse("drive_create_file", &json!({"name":"x","attachment":"a.txt"})).unwrap();
    assert!(program::build(&a, &verdict(&a), None, None).is_err());
}

#[test]
fn interpret_success_errors_and_malformed() {
    let value = json!({"id":FILE,"name":format!("{}\n", "字".repeat(100)),"mimeType":"text/plain","parents":[PARENT],"trashed":false});
    let out = program::interpret("drive_create_file", 201, &serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(out.file_id, FILE);
    assert_eq!(out.file_name.chars().count(), 80);
    assert_eq!(out.full_file_name, "字".repeat(100));
    assert_eq!(out.parents_after, Some(vec![PARENT.into()]));
    assert_eq!(out.link, format!("https://drive.google.com/open?id={FILE}"));
    for error in [json!({"error":{"errors":[{"reason":"insufficientPermissions"}]}}), json!({"error":{"status":"insufficientPermissions"}})] {
        assert_eq!(program::interpret("drive_trash_file", 403, &serde_json::to_vec(&error).unwrap()).unwrap_err(), OutcomeError::InsufficientPermissions);
    }
    assert_eq!(program::interpret("drive_trash_file", 500, b"failure").unwrap_err(), OutcomeError::Google(500));
    assert_eq!(program::interpret("drive_trash_file", 403, b"{}").unwrap_err(), OutcomeError::Google(403));
    for body in [b"bad".as_slice(), b"{}", b"null"] {
        assert_eq!(program::interpret("drive_create_file", 200, body).unwrap_err(), OutcomeError::Unconfirmed);
    }
    for (key, bad) in [("id", json!("x/permissions")), ("parents", json!(["bad"])), ("parents", Value::Null), ("driveId", json!(4)), ("trashed", json!("false")), ("mimeType", json!(""))] {
        let mut bad_value = value.clone(); bad_value[key] = bad;
        assert_eq!(program::interpret("drive_create_file", 200, &serde_json::to_vec(&bad_value).unwrap()).unwrap_err(), OutcomeError::Unconfirmed);
    }
}

#[test]
fn plans_and_expected_parents() {
    let create = args::parse("drive_create_file", &json!({"name":"x","text":"","parent_folder_id":PARENT})).unwrap();
    assert!(matches!(args::plan(&create), Plan::Create { parent: Some(id) } if id.as_str() == PARENT));
    assert_eq!(program::expected_parents(&create, &verdict(&create)), vec![PARENT.to_owned()]);
    let copy = args::parse("drive_copy_file", &json!({"file_id":FILE,"parent_folder_id":PARENT})).unwrap();
    assert!(matches!(args::plan(&copy), Plan::Copy { source, parent } if source.as_str() == FILE && parent.as_str() == PARENT));
    assert_eq!(program::expected_parents(&copy, &verdict(&copy)), vec![PARENT.to_owned()]);
    for (value, rename) in [(json!({"file_id":FILE,"description":""}), false), (json!({"file_id":FILE,"name":"x"}), true)] {
        let a = args::parse("drive_update_file", &value).unwrap();
        assert!(matches!(args::plan(&a), Plan::Modify { target, rename: actual } if target.as_str() == FILE && actual == rename));
        assert_eq!(program::expected_parents(&a, &verdict(&a)), vec![OLD.to_owned()]);
    }
    let trash = args::parse("drive_trash_file", &json!({"file_id":FILE})).unwrap();
    assert!(matches!(args::plan(&trash), Plan::Trash { target } if target.as_str() == FILE));
}

#[test]
fn attachment_binding_conversion_and_text_defaults() {
    let a = args::parse("drive_create_file", &json!({"name":"x","attachment":"a.txt","convert_to":"document"})).unwrap();
    let content = |file_name: &str| AttachmentContent { file_name: file_name.into(), bytes: Bytes::from_static(b"hello"), sha256: "executor-verified".into() };
    assert!(program::build(&a, &verdict(&a), Some(content("other.txt")), None).is_err());
    let c = program::build(&a, &verdict(&a), Some(content("a.txt")), None).unwrap().remove(0);
    let wire = String::from_utf8(c.test_body().unwrap().unwrap().1).unwrap();
    assert!(wire.contains("application/vnd.google-apps.document"));
    let c = call("drive_create_file", json!({"name":"x","text":""}));
    assert!(matches!(c, WriteCall::DriveCreateMultipart { content_type: ClosedMime::PlainText, .. }));
    assert!(args::parse("drive_create_file", &json!({"name":"字".repeat(255),"attachment":"a".repeat(255)})).is_ok());
    assert!(args::parse("drive_create_file", &json!({"name":"x","attachment":"a".repeat(256)})).is_err());
}

#[test]
fn move_requires_valid_google_parents_and_root_requires_empty_scope() {
    let a = args::parse("drive_move_file", &json!({"file_id":FILE,"destination_folder_id":PARENT})).unwrap();
    for parents in [None, Some(vec![]), Some(vec!["bad".into()]), Some(vec![OLD.into(), OLD.into()])] {
        let v = ScopeVerdict::InScope { matched: vec![], target_id: Some(FILE.into()), parents };
        assert!(program::build(&a, &v, None, None).is_err());
    }
    let a = args::parse("drive_create_file", &json!({"name":"x","folder":true})).unwrap();
    let v = ScopeVerdict::InScope { matched: vec![PARENT.into()], target_id: None, parents: None };
    assert!(program::build(&a, &v, None, None).is_err());
}

#[test]
fn interpret_trash_and_missing_parents() {
    let value = json!({"id":FILE,"name":"x","mimeType":"text/plain","trashed":true,"driveId":PARENT});
    let out = program::interpret("drive_trash_file", 200, &serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(out.trashed);
    assert_eq!(out.drive_id.as_deref(), Some(PARENT));
    assert_eq!(out.parents_after, None);
    assert_eq!(program::interpret("drive_create_file", 200, &serde_json::to_vec(&value).unwrap()).unwrap_err(), OutcomeError::Unconfirmed);
    assert_eq!(OutcomeError::Unconfirmed.code(), "result_unconfirmed");
    assert_eq!(ArgError::InvalidId.code(), "invalid_id");
}
