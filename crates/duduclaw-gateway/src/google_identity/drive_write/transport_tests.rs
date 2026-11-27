use super::*;
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ID: &str = "file_1234567890";
const PARENT: &str = "parent_1234567890";
const OLD_PARENT: &str = "old_parent_1234567890";
const TOKEN: &str = "secret_bearer_1234567890";
const EXPECTED_SCOPE: &str = "id,name,mimeType,parents,driveId,trashed,shortcutDetails,capabilities(canEdit,canRename,canAddChildren,canTrash,canMoveItemWithinDrive)";
const EXPECTED_WRITE: &str = "id,name,mimeType,parents,driveId,trashed";

fn id() -> GoogleId { GoogleId::new(ID).unwrap() }
fn metadata() -> JsonBody { JsonBody::new(json!({"name": "report"})).unwrap() }
fn trash() -> WriteCall { WriteCall::DriveTrash { file_id: id() } }

struct Case {
    call: WriteCall,
    method: Method,
    host: &'static str,
    path: String,
    query: Vec<(&'static str, &'static str)>,
}

fn cases() -> Vec<Case> {
    let write_query = vec![("supportsAllDrives", "true"), ("fields", EXPECTED_WRITE)];
    let upload_query = vec![("uploadType", "multipart"), ("supportsAllDrives", "true"), ("fields", EXPECTED_WRITE)];
    vec![
        Case {
            call: WriteCall::DriveProbe,
            method: Method::GET, host: "www.googleapis.com", path: "/drive/v3/files".into(),
            query: vec![("pageSize", "1"), ("fields", "files(id)"),
                ("supportsAllDrives", "true"), ("includeItemsFromAllDrives", "true")],
        },
        Case {
            call: WriteCall::DriveGetMetadata { file_id: id(), fields: MetadataFields::Scope },
            method: Method::GET, host: "www.googleapis.com", path: format!("/drive/v3/files/{ID}"),
            query: vec![("fields", EXPECTED_SCOPE), ("supportsAllDrives", "true")],
        },
        Case {
            call: WriteCall::DriveCreateFolder { metadata_json: metadata() },
            method: Method::POST, host: "www.googleapis.com", path: "/drive/v3/files".into(),
            query: write_query.clone(),
        },
        Case {
            call: WriteCall::DriveCreateMultipart { metadata_json: metadata(), content: Bytes::from_static(b"hello"), content_type: ClosedMime::PlainText },
            method: Method::POST, host: "www.googleapis.com", path: "/upload/drive/v3/files".into(),
            query: upload_query.clone(),
        },
        Case {
            call: WriteCall::DriveCopy { file_id: id(), metadata_json: metadata() },
            method: Method::POST, host: "www.googleapis.com", path: format!("/drive/v3/files/{ID}/copy"),
            query: write_query.clone(),
        },
        Case {
            call: WriteCall::DriveUpdateMetadata { file_id: id(), metadata_json: metadata() },
            method: Method::PATCH, host: "www.googleapis.com", path: format!("/drive/v3/files/{ID}"),
            query: write_query.clone(),
        },
        Case {
            call: WriteCall::DriveUpdateContent { file_id: id(), metadata_json: metadata(), content: Bytes::from_static(b"hello"), content_type: ClosedMime::Markdown },
            method: Method::PATCH, host: "www.googleapis.com", path: format!("/upload/drive/v3/files/{ID}"),
            query: upload_query,
        },
        Case {
            call: WriteCall::DriveMove { file_id: id(), add_parent: GoogleId::new(PARENT).unwrap(), remove_parents: vec![GoogleId::new(OLD_PARENT).unwrap()] },
            method: Method::PATCH, host: "www.googleapis.com", path: format!("/drive/v3/files/{ID}"),
            query: vec![("addParents", PARENT), ("removeParents", OLD_PARENT), ("supportsAllDrives", "true"), ("fields", EXPECTED_WRITE)],
        },
        Case {
            call: trash(), method: Method::PATCH, host: "www.googleapis.com", path: format!("/drive/v3/files/{ID}"),
            query: write_query,
        },
        Case {
            call: WriteCall::DocsGet { document_id: id(), include_tabs: true },
            method: Method::GET, host: "docs.googleapis.com", path: format!("/v1/documents/{ID}"),
            query: vec![("includeTabsContent", "true")],
        },
        Case {
            call: WriteCall::DocsGet { document_id: id(), include_tabs: false },
            method: Method::GET, host: "docs.googleapis.com", path: format!("/v1/documents/{ID}"), query: vec![],
        },
        Case {
            call: WriteCall::DocsBatchUpdate { document_id: id(), requests_json: JsonBody::new(json!({"requests": []})).unwrap() },
            method: Method::POST, host: "docs.googleapis.com", path: format!("/v1/documents/{ID}:batchUpdate"), query: vec![],
        },
        Case {
            call: WriteCall::SheetsGetProperties { spreadsheet_id: id() },
            method: Method::GET, host: "sheets.googleapis.com", path: format!("/v4/spreadsheets/{ID}"), query: vec![("fields", "sheets.properties")],
        },
        Case {
            call: WriteCall::SheetsBatchUpdate { spreadsheet_id: id(), requests_json: JsonBody::new(json!({"requests": []})).unwrap() },
            method: Method::POST, host: "sheets.googleapis.com", path: format!("/v4/spreadsheets/{ID}:batchUpdate"), query: vec![],
        },
    ]
}

#[derive(Clone)]
enum FakeReply { Ok(u16, Vec<u8>), Error(SendError), Pending }

struct RecordingHttp {
    recorded: Arc<Mutex<Vec<BuiltRequest>>>,
    reply: FakeReply,
}

impl WriteHttp for RecordingHttp {
    fn send(&self, request: BuiltRequest)
        -> Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), SendError>> + Send + '_>>
    {
        self.recorded.lock().unwrap().push(request);
        Box::pin(async move {
            match &self.reply {
                FakeReply::Ok(status, body) => Ok((*status, body.clone())),
                FakeReply::Error(error) => Err(*error),
                FakeReply::Pending => std::future::pending().await,
            }
        })
    }
}

fn fake(reply: FakeReply) -> (WriteTransport, Arc<Mutex<Vec<BuiltRequest>>>) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let http = RecordingHttp { recorded: recorded.clone(), reply };
    (WriteTransport::with_http(Zeroizing::new(TOKEN.into()), Box::new(http)), recorded)
}

#[test]
fn every_variant_has_exact_method_host_path_and_query() {
    let cases = cases();
    assert_eq!(cases.len(), 14); // Thirteen variants plus both DocsGet shapes.
    for case in cases {
        let (method, url) = case.call.method_and_url();
        assert_eq!(method, case.method);
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some(case.host));
        assert_eq!(url.path(), case.path);
        assert_eq!(url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect::<Vec<_>>(),
            case.query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>());
        assert_eq!(case.call.allowed_query_keys(), case.query.iter().map(|(k, _)| *k).collect::<Vec<_>>().as_slice());
        assert_eq!(case.call.validate_target(&method, &url), Ok(()));
    }
}

#[test]
fn all_variants_exclude_delete_and_forbidden_routes() {
    for case in cases() {
        let (method, url) = case.call.method_and_url();
        assert_ne!(method, Method::DELETE);
        assert!(!url.as_str().contains("emptyTrash"));
        let segments = url.path_segments().unwrap().collect::<Vec<_>>();
        assert!(!segments.iter().any(|segment| matches!(*segment, "permissions" | "revisions" | "comments")));
        assert!(!segments.windows(2).any(|pair| pair == ["files", "trash"]));
        if segments.first() == Some(&"upload") {
            assert!(matches!(case.call, WriteCall::DriveCreateMultipart { .. } | WriteCall::DriveUpdateContent { .. }));
        }
    }
}

#[test]
fn id_constructors_reject_injection_and_invalid_lengths() {
    for bad in ["/", "..", "%2F", "x/permissions", "abcdefghij/other", "abcdefghij%2Fother", "abcdefghij..", "short", "中文測試中文測試", "permissions", "emptyTrash"] {
        assert_eq!(GoogleId::new(bad), Err(NotSentReason::InvalidId));
    }
    assert_eq!(GoogleId::new("a".repeat(9)), Err(NotSentReason::InvalidId));
    assert_eq!(GoogleId::new("a".repeat(129)), Err(NotSentReason::InvalidId));
    for good in ["a".repeat(10), "a".repeat(128), "abcde_1234-".into()] {
        assert!(GoogleId::new(good).is_ok());
    }
}

#[tokio::test]
async fn trash_and_move_bodies_are_transport_owned_exact_json() {
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
    assert!(matches!(transport.send(&trash()).await, WriteResult::Ok(_)));
    let move_call = WriteCall::DriveMove {
        file_id: id(), add_parent: GoogleId::new(PARENT).unwrap(),
        remove_parents: vec![GoogleId::new(OLD_PARENT).unwrap(), GoogleId::new("other_parent_1234").unwrap()],
    };
    assert!(matches!(transport.send(&move_call).await, WriteResult::Ok(_)));
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded[0].0.body().unwrap().as_bytes().unwrap(), br#"{"trashed":true}"#);
    assert_eq!(recorded[1].0.body().unwrap().as_bytes().unwrap(), b"{}");
    assert_eq!(recorded[0].0.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(recorded[1].0.url().query_pairs().find(|(k, _)| k == "removeParents").unwrap().1,
        format!("{OLD_PARENT},other_parent_1234"));
}

#[tokio::test]
async fn invalid_move_parents_are_refused_without_send() {
    let eleven = (0..11).map(|index| GoogleId::new(format!("old_parent_{index:02}")).unwrap()).collect::<Vec<_>>();
    for remove_parents in [
        vec![],
        eleven,
        vec![GoogleId::new(OLD_PARENT).unwrap(), GoogleId::new(OLD_PARENT).unwrap()],
    ] {
        let call = WriteCall::DriveMove {
            file_id: id(), add_parent: GoogleId::new(PARENT).unwrap(), remove_parents,
        };
        let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
        assert!(matches!(transport.send(&call).await, WriteResult::NotSent(NotSentReason::InvalidParents)));
        assert_eq!(transport.requests, 0);
        assert!(recorded.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn ten_distinct_move_parents_are_allowed() {
    let call = WriteCall::DriveMove {
        file_id: id(), add_parent: GoogleId::new(PARENT).unwrap(),
        remove_parents: (0..10).map(|index| GoogleId::new(format!("old_parent_{index:02}")).unwrap()).collect(),
    };
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
    assert!(matches!(transport.send(&call).await, WriteResult::Ok(_)));
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn final_url_over_8192_bytes_is_refused_without_send() {
    // Exercise the length gate independently of route mismatch. Production IDs
    // are constructor-bounded; this private test fixture bypasses that bound.
    let call = WriteCall::DriveTrash { file_id: GoogleId("x".repeat(8192)) };
    let (method, url) = call.method_and_url();
    assert!(url.as_str().len() > 8192);
    assert_eq!(call.validate_target(&method, &url), Err(NotSentReason::InvalidEndpoint));
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
    assert!(matches!(transport.send(&call).await, WriteResult::NotSent(NotSentReason::InvalidEndpoint)));
    assert_eq!(transport.requests, 0);
    assert!(recorded.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tampered_targets_are_refused_before_send() {
    let call = trash();
    let (method, logical) = call.method_and_url();
    let mut targets = Vec::new();
    for path in [
        format!("/drive/v3/files/{ID}/permissions"), format!("/drive/v3/files/{ID}/revisions"),
        format!("/drive/v3/files/{ID}/comments"), "/drive/v3/files/trash".into(),
        "/drive/v3/files/emptyTrash".into(), format!("/upload/drive/v3/files/{ID}"),
        format!("/drive/v3/files/{ID}/"), "/drive/v3/files/other_1234567890".into(),
        "/drive/v3/files/abcdefghij%2Fpermissions".into(),
    ] {
        let mut url = logical.clone();
        url.set_path(&path);
        targets.push((method.clone(), url));
    }
    targets.push((Method::DELETE, logical.clone()));
    targets.push((Method::POST, logical.clone()));
    let mut host = logical.clone();
    host.set_host(Some("www.googleapis.com.evil.test")).unwrap();
    targets.push((method.clone(), host));
    let mut scheme = logical.clone();
    scheme.set_scheme("http").unwrap();
    targets.push((method.clone(), scheme));
    let mut port = logical.clone();
    port.set_port(Some(444)).unwrap();
    targets.push((method.clone(), port));
    let mut user = logical.clone();
    user.set_username("attacker").unwrap();
    targets.push((method.clone(), user));
    let mut fragment = logical.clone();
    fragment.set_fragment(Some("ignored"));
    targets.push((method.clone(), fragment));
    for query in [
        "supportsAllDrives=true&fields=id&alt=media", "supportsAllDrives=false&fields=id",
        "supportsAllDrives=true&fields=id&fields=id", "", "fields=id",
    ] {
        let mut url = logical.clone();
        url.set_query(Some(query));
        targets.push((method.clone(), url));
    }
    for target in targets {
        let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
        // Even with an override, the logical URL must pass first.
        transport.set_test_api_base(Url::parse("http://127.0.0.1:12345/").unwrap()).unwrap();
        transport.test_target = Some(target);
        assert!(matches!(transport.send(&call).await, WriteResult::NotSent(NotSentReason::InvalidEndpoint)));
        assert!(recorded.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn extra_or_duplicate_query_keys_fail_for_every_variant() {
    for case in cases() {
        let (method, url) = case.call.method_and_url();
        for key in ["alt", case.call.allowed_query_keys().first().copied().unwrap_or("fields")] {
            let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
            let mut altered = url.clone();
            altered.query_pairs_mut().append_pair(key, "unexpected");
            transport.test_target = Some((method.clone(), altered));
            assert!(matches!(transport.send(&case.call).await, WriteResult::NotSent(NotSentReason::InvalidEndpoint)));
            assert!(recorded.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn content_over_five_mib_is_refused_without_send() {
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
    let content = Bytes::from(vec![b'x'; CONTENT_LIMIT + 1]);
    for call in [
        WriteCall::DriveCreateMultipart { metadata_json: metadata(), content: content.clone(), content_type: ClosedMime::PlainText },
        WriteCall::DriveUpdateContent { file_id: id(), metadata_json: metadata(), content, content_type: ClosedMime::PlainText },
    ] {
        assert!(matches!(transport.send(&call).await, WriteResult::NotSent(NotSentReason::ContentTooLarge)));
    }
    assert!(recorded.lock().unwrap().is_empty());
}

#[tokio::test]
async fn json_over_one_mib_is_refused_at_constructor_and_send() {
    let value = json!({"x": "x".repeat(JSON_LIMIT - 7)});
    let serialized = serde_json::to_vec(&value).unwrap();
    assert_eq!(serialized.len(), JSON_LIMIT + 1);
    assert!(matches!(JsonBody::new(value), Err(NotSentReason::JsonTooLarge)));
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
    for mut case in cases() {
        let body = match &mut case.call {
            WriteCall::DriveCreateFolder { metadata_json } | WriteCall::DriveCreateMultipart { metadata_json, .. }
            | WriteCall::DriveCopy { metadata_json, .. } | WriteCall::DriveUpdateMetadata { metadata_json, .. }
            | WriteCall::DriveUpdateContent { metadata_json, .. } => metadata_json,
            WriteCall::DocsBatchUpdate { requests_json, .. } | WriteCall::SheetsBatchUpdate { requests_json, .. } => requests_json,
            _ => continue,
        };
        // Deliberately break the private constructor invariant to exercise
        // the send-boundary check as well as the public constructor check.
        *body = JsonBody(serialized.clone());
        assert!(matches!(transport.send(&case.call).await, WriteResult::NotSent(NotSentReason::JsonTooLarge)));
    }
    assert!(recorded.lock().unwrap().is_empty());
    assert_eq!(transport.requests, 0);
}

#[test]
fn json_requires_an_object_and_measures_serialized_utf8_bytes() {
    for value in [Value::Null, json!([]), json!("{}"), json!(42)] {
        assert!(matches!(JsonBody::new(value), Err(NotSentReason::InvalidJson)));
    }
    // Fewer than 1 MiB characters can still exceed the serialized byte cap.
    assert!(matches!(JsonBody::new(json!({"x": "測".repeat(JSON_LIMIT / 3)})), Err(NotSentReason::JsonTooLarge)));
}

#[tokio::test]
async fn exact_size_limits_are_accepted() {
    let metadata_json = JsonBody::new(json!({"x": "x".repeat(JSON_LIMIT - 8)})).unwrap();
    assert_eq!(metadata_json.0.len(), JSON_LIMIT);
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![0; RESPONSE_LIMIT]));
    let call = WriteCall::DriveCreateMultipart {
        metadata_json, content: Bytes::from(vec![0; CONTENT_LIMIT]), content_type: ClosedMime::UnknownAttachment,
    };
    assert!(matches!(transport.send(&call).await, WriteResult::Ok((200, body)) if body.len() == RESPONSE_LIMIT));
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[test]
fn multipart_has_exactly_two_parts_with_closed_mime_and_safe_boundary() {
    for (mime, expected) in [
        (ClosedMime::PlainText, "text/plain"), (ClosedMime::Csv, "text/csv"),
        (ClosedMime::Markdown, "text/markdown"), (ClosedMime::UnknownAttachment, "application/octet-stream"),
    ] {
        let metadata = metadata();
        let content = Bytes::from_static(b"content\r\nwith\0bytes");
        let (header, body) = multipart(&metadata, &content, mime).unwrap();
        let boundary = header.strip_prefix("multipart/related; boundary=").unwrap();
        assert!(!boundary_occurs(boundary, &content));
        assert!(!boundary_occurs(boundary, &metadata.0));
        let mut expected_body = format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n").into_bytes();
        expected_body.extend_from_slice(&metadata.0);
        expected_body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Type: {expected}\r\n\r\n").as_bytes());
        expected_body.extend_from_slice(&content);
        expected_body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        assert_eq!(body, expected_body);
        let (second_header, _) = multipart(&metadata, &content, mime).unwrap();
        assert_ne!(header, second_header);
    }
    assert!(boundary_occurs("collision", b"prefixcollisionSuffix"));
    assert!(!boundary_occurs("collision", b"no-match"));
}

#[tokio::test]
async fn twenty_seventh_request_is_refused_and_reads_share_the_budget() {
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
    let calls = cases();
    for n in 0..26 {
        assert!(matches!(transport.send(&calls[n % calls.len()].call).await, WriteResult::Ok(_)));
    }
    assert!(matches!(transport.send(&trash()).await, WriteResult::NotSent(NotSentReason::RequestLimit)));
    assert_eq!(recorded.lock().unwrap().len(), 26);
}

#[tokio::test]
async fn oversized_response_is_maybe_sent_without_retry() {
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![0; RESPONSE_LIMIT + 1]));
    assert!(matches!(transport.send(&trash()).await, WriteResult::MaybeSent(SendError::ResponseTooLarge)));
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn network_error_is_maybe_sent_without_retry() {
    let (mut transport, recorded) = fake(FakeReply::Error(SendError::Network));
    assert!(matches!(transport.send(&trash()).await, WriteResult::MaybeSent(SendError::Network)));
    assert_eq!(recorded.lock().unwrap().len(), 1);
    assert_eq!(transport.requests, 1);
}

#[tokio::test(start_paused = true)]
async fn timeout_is_maybe_sent_without_retry() {
    let (mut transport, recorded) = fake(FakeReply::Pending);
    let started = tokio::time::Instant::now();
    assert!(matches!(transport.send(&trash()).await, WriteResult::MaybeSent(SendError::Timeout)));
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_bearer_is_refused_locally() {
    for bearer in ["", "bad\r\nAuthorization: injected", "token with space"] {
        let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
        transport.bearer = Zeroizing::new(bearer.into());
        assert!(matches!(transport.send(&trash()).await, WriteResult::NotSent(NotSentReason::InvalidBearer)));
        assert!(recorded.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn bearer_and_content_are_absent_from_debug_output() {
    let (mut transport, recorded) = fake(FakeReply::Ok(200, TOKEN.as_bytes().to_vec()));
    let secret_json = JsonBody::new(json!({"name": TOKEN})).unwrap();
    let call = WriteCall::DriveUpdateContent {
        file_id: GoogleId::new(TOKEN).unwrap(), metadata_json: secret_json.clone(),
        content: Bytes::from_static(TOKEN.as_bytes()), content_type: ClosedMime::PlainText,
    };
    let result = transport.send(&call).await;
    let debug = format!("{transport:?} {call:?} {secret_json:?} {:?} {result:?} {:?} {:?} {:?} {:?} {:?}",
        GoogleId::new(TOKEN).unwrap(), recorded.lock().unwrap()[0], MetadataFields::Scope,
        ClosedMime::UnknownAttachment, NotSentReason::InvalidBearer, SendError::Network);
    assert!(!debug.contains(TOKEN));
    assert!(!debug.contains(&format!("{:?}", TOKEN.as_bytes())));
    assert!(recorded.lock().unwrap()[0].0.headers()[header::AUTHORIZATION].is_sensitive());
}

#[tokio::test]
async fn test_base_override_is_instance_local_and_after_validation() {
    let (mut transport, recorded) = fake(FakeReply::Ok(200, vec![]));
    transport.set_test_api_base(Url::parse("http://127.0.0.1:12345/").unwrap()).unwrap();
    assert!(matches!(transport.send(&trash()).await, WriteResult::Ok(_)));
    {
        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded[0].0.url().host_str(), Some("127.0.0.1"));
        assert_eq!(recorded[0].0.url().path(), format!("/drive/v3/files/{ID}"));
        assert_eq!(recorded[0].0.url().query(), trash().method_and_url().1.query());
    }
    let (mut other, other_recorded) = fake(FakeReply::Ok(200, vec![]));
    assert!(matches!(other.send(&trash()).await, WriteResult::Ok(_)));
    assert_eq!(other_recorded.lock().unwrap()[0].0.url().host_str(), Some("www.googleapis.com"));
    for bad in ["http://example.com/", "http://127.0.0.1/prefix", "http://127.0.0.1/?x=1", "http://user@127.0.0.1/"] {
        assert_eq!(other.set_test_api_base(Url::parse(bad).unwrap()), Err(NotSentReason::InvalidEndpoint));
    }
}

// A real HTTP/1 fixture verifies the reqwest layer as well as the recording
// fake. It drains request bodies and can leave the response unterminated to
// prove that the response cap does not depend on Content-Length or EOF.
async fn local_server(reply: Vec<u8>, leave_open: bool)
    -> (Url, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                match socket.read_u8().await {
                    Ok(byte) => headers.push(byte),
                    Err(_) => return,
                }
                assert!(headers.len() <= 65536);
            }
            let length = String::from_utf8(headers).unwrap().lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
            }).unwrap_or(0);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            seen.fetch_add(1, Ordering::SeqCst);
            let _ = socket.write_all(&reply).await;
            if leave_open {
                // Hold the connection open without a terminating chunk.
                let _ = socket.read_u8().await;
            }
        }
    });
    (base, count, task)
}

#[tokio::test]
async fn real_http_streaming_cap_stops_before_eof() {
    let mut reply = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", RESPONSE_LIMIT + 1).into_bytes();
    reply.extend(vec![b'x'; RESPONSE_LIMIT + 1]);
    reply.extend_from_slice(b"\r\n"); // No zero-length terminating chunk.
    let (base, count, task) = local_server(reply, true).await;
    let mut transport = WriteTransport::new(Zeroizing::new(TOKEN.into())).unwrap();
    transport.set_test_api_base(base).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), transport.send(&trash())).await;
    task.abort();
    assert!(matches!(result.unwrap(), WriteResult::MaybeSent(SendError::ResponseTooLarge)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn real_http_content_length_cap_stops_before_body() {
    let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", RESPONSE_LIMIT + 1).into_bytes();
    let (base, count, task) = local_server(reply, true).await;
    let mut transport = WriteTransport::new(Zeroizing::new(TOKEN.into())).unwrap();
    transport.set_test_api_base(base).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), transport.send(&trash())).await;
    task.abort();
    assert!(matches!(result.unwrap(), WriteResult::MaybeSent(SendError::ResponseTooLarge)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn real_http_does_not_redirect_or_retry_error_status() {
    for status in [302, 503] {
        let reply = format!("HTTP/1.1 {status} Test\r\nLocation: /drive/v3/files/trash\r\nRetry-After: 0\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").into_bytes();
        let (base, count, task) = local_server(reply, false).await;
        let mut transport = WriteTransport::new(Zeroizing::new(TOKEN.into())).unwrap();
        transport.set_test_api_base(base).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), transport.send(&trash())).await;
        task.abort();
        assert!(matches!(result.unwrap(), WriteResult::Ok((actual, body)) if actual == status && body == b"{}"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn drive_probe_is_exact_get_without_body() {
    let call = WriteCall::DriveProbe;
    let (method, url) = call.method_and_url();
    assert_eq!(method, Method::GET);
    assert_eq!(url.as_str(), "https://www.googleapis.com/drive/v3/files?pageSize=1&fields=files(id)&supportsAllDrives=true&includeItemsFromAllDrives=true");
    assert!(call.validate_target(&method, &url).is_ok());
    let mut changed = url.clone();
    changed.query_pairs_mut().append_pair("q", "trashed=false");
    assert!(call.validate_target(&method, &changed).is_err());
    let mut transport = WriteTransport::new(Zeroizing::new(TOKEN.into())).unwrap();
    assert!(transport.prepare(&call).unwrap().0.body().is_none());
}
