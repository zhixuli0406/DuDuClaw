use duduclaw_import::{import, Error, ImportRequest, SourceKind};
use duduclaw_isolation::{ids::RequestId, migration::ImportRecord};
fn request() -> ImportRequest {
    ImportRequest { version: 1, migration_id: RequestId([7;16]), employee: None,
        source_id: "memory".into(), kind: SourceKind::MemoryText, title: String::new(),
        legacy_agent: None, has_wal: false, blob_handle: None }
}
#[test]
fn content_is_host_bound_and_has_no_authority_variant() {
    let req = request();
    let mut output = Vec::new();
    assert_eq!(import(&req, "舊記憶".as_bytes(), None, &mut output), Ok(1));
    let record: ImportRecord = serde_json::from_slice(&output).unwrap();
    assert_eq!(record.migration_id, req.migration_id);
    for category in ["users", "admin", "mcp", "approval", "schedule", "session_token"] {
        let mut json = serde_json::to_value(&req).unwrap();
        json["kind"] = category.into();
        assert!(ImportRequest::decode(&serde_json::to_vec(&json).unwrap()).is_err());
    }
}
#[test]
fn rejects_unknown_fields_wal_mismatch_and_bad_utf8() {
    let mut value = serde_json::to_value(request()).unwrap();
    value["path"] = "/etc/shadow".into();
    assert!(ImportRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    assert_eq!(import(&request(), &[255][..], None, Vec::new()), Err(Error::Content));
    let mut wal: &[u8] = b"";
    assert_eq!(import(&request(), &b"hello"[..], Some(&mut wal), Vec::new()), Err(Error::InvalidRequest));
}
#[test]
fn refuses_oversize_without_emitting_partial_text() {
    let mut output = Vec::new();
    assert_eq!(import(&request(), &vec![b'x'; 65537][..], None, &mut output), Err(Error::Limit));
    assert!(output.is_empty());
    assert!(ImportRequest::decode(&vec![b' '; 8193]).is_err());
}
#[test]
fn blob_records_hash_bytes_and_keep_labels_inert() {
    use sha2::{Digest,Sha256};
    let mut req=request();req.kind=SourceKind::Attachment;req.blob_handle=Some(RequestId([4;16]));req.title="../../secret".into();
    let mut output=Vec::new();assert_eq!(import(&req,&b"blob content"[..],None,&mut output),Ok(1));
    let record:ImportRecord=serde_json::from_slice(&output).unwrap();
    let duduclaw_isolation::migration::ImportedContent::Attachment {blob}=record.content else {panic!("attachment expected")};
    assert_eq!(blob.size_bytes,12);assert_eq!(blob.sha256.0,<[u8;32]>::from(Sha256::digest(b"blob content")));
    assert_eq!(blob.display_name,"../../secret");req.blob_handle=None;assert!(req.validate().is_err());
}
