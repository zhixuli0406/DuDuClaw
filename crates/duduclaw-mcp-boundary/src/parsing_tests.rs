use super::*;
use duduclaw_isolation::bounded::Text;
use duduclaw_isolation::ids::RequestId;

fn options(kind: TableKind) -> TableOptions {
    TableOptions { kind, delimiter: b',', has_header: true, offset: 0, limit: 2, sheet: None }
}
fn make_request(options: &TableOptions) -> ParseRequest {
    let file = tempfile::tempfile().unwrap();
    table_request(&file, options).unwrap()
}
fn response(request: &ParseRequest, value: Value) -> ParseResponse {
    ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
        result: ParseResult::Complete { items: List::new(vec![ParsedItem::Text {
            input_slot: 0, page: None, text: Text::new(value.to_string()).unwrap(),
        }]).unwrap() } }
}
fn table() -> Value {
    serde_json::json!({"columns":["name"],"rows":[{"name":"員工"}],"truncated":false})
}

#[test]
fn table_worker_contract_preserves_structured_redaction_shape() {
    let options = options(TableKind::Csv);
    let request = make_request(&options);
    assert_eq!(request.expected_fd_count(), 1);
    assert_eq!(request.inputs.as_slice()[0].format, ContentFormat::Text);
    let value = decode_table_response(&request, &options, response(&request, table())).unwrap();
    assert_eq!(value["rows"][0]["name"], "員工");
    assert_eq!(value["row_count"], 1);
    assert!(value.get("path").is_none());
    assert!(value.get("table").is_none());
}

#[test]
fn table_worker_refuses_forged_authority_nested_cells_and_wrong_columns() {
    let options = options(TableKind::Csv);
    let request = make_request(&options);
    let mut cases = vec![];
    for key in ["path", "table", "principal", "agent", "row_count"] {
        let mut value = table(); value[key] = serde_json::json!("forged"); cases.push(value);
    }
    let mut nested = table(); nested["rows"][0]["name"] = serde_json::json!({"secret":"nested"}); cases.push(nested);
    let mut numeric = table(); numeric["rows"][0]["name"] = serde_json::json!(42); cases.push(numeric);
    let mut columns = table(); columns["columns"] = serde_json::json!(["other"]); cases.push(columns);
    let mut repeated = table(); repeated["columns"] = serde_json::json!(["name", "name"]); cases.push(repeated);
    let mut empty = table(); empty["columns"] = serde_json::json!([]); cases.push(empty);
    for value in cases {
        assert!(decode_table_response(&request, &options, response(&request, value)).is_err());
    }
}

#[test]
fn table_worker_refuses_wrong_correlation_kind_limits_and_sheet() {
    let options = options(TableKind::Csv);
    let request = make_request(&options);
    let mut wrong = response(&request, table()); wrong.request_id = RequestId([0; 16]);
    assert!(decode_table_response(&request, &options, wrong).is_err());
    let mut wrong = response(&request, table()); wrong.result = ParseResult::Complete {
        items: List::new(vec![ParsedItem::Image { input_slot: 0, width: 1, height: 1 }]).unwrap() };
    assert!(decode_table_response(&request, &options, wrong).is_err());
    let mut value = table(); value["rows"] = serde_json::json!([{"name":"a"},{"name":"b"},{"name":"c"}]);
    assert!(decode_table_response(&request, &options, response(&request, value)).is_err());
    let mut options = options; options.kind = TableKind::Spreadsheet; options.sheet = Some("Sheet1".into());
    let request = make_request(&options);
    let mut value = table(); value["sheet"] = serde_json::json!("Sheet2"); value["sheets"] = serde_json::json!(["Sheet2"]);
    assert!(decode_table_response(&request, &options, response(&request, value.clone())).is_err());
    value["sheet"] = serde_json::json!("Sheet1"); value["sheets"] = serde_json::json!(["Sheet1"]);
    value["rows"][0]["name"] = serde_json::json!(42);
    assert!(decode_table_response(&request, &options, response(&request, value)).is_ok());
}

#[test]
fn table_worker_refuses_oversized_inputs_and_options() {
    let file = tempfile::tempfile().unwrap();
    file.set_len(32 * 1024 * 1024 + 1).unwrap();
    assert!(table_request(&file, &options(TableKind::Spreadsheet)).is_err());
    assert!(table_request(&file, &options(TableKind::Csv)).is_ok());
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    assert!(table_request(&file, &options(TableKind::Csv)).is_err());
    let mut options = options(TableKind::Csv); options.offset = 1_000_001;
    assert!(options.validate().is_err()); options.offset = 0; options.limit = 2001;
    assert!(options.validate().is_err()); options.limit = 2; options.delimiter = b'\n';
    assert!(options.validate().is_err());
}
