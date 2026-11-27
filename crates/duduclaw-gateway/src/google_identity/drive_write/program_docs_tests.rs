use super::*;
use super::super::args::{parse_any, ToolArgs};
use super::super::args_docs::{parse, parse_a1};

const ID: &str = "document_12345";
fn verdict() -> ScopeVerdict {
    ScopeVerdict::InScope { target_id: Some(ID.into()), matched: vec![], parents: None }
}
fn paragraph(start: u32, text: &str) -> Value {
    let end = start + text.encode_utf16().count() as u32;
    json!({"startIndex":start,"endIndex":end,"paragraph":{"elements":[
        {"startIndex":start,"endIndex":end,"textRun":{"content":text}}
    ]}})
}
fn doc(text: &str) -> Value { json!({"documentId":ID,"body":{"content":[paragraph(1, text)]}}) }
fn sheet_state() -> Value {
    // The transport's fields=sheets.properties intentionally omits spreadsheetId.
    json!({"sheets":[{"properties":{"sheetId":7,"title":"Sheet1"}}, {"properties":{"sheetId":12,"title":"My Sheet"}}]})
}
fn build_tool(tool: &str, value: Value, state: &Value) -> Result<BatchPlan, ArgError> {
    build(&parse(tool, &value)?, &verdict(), state)
}
fn insert(after: &str, state: &Value) -> Result<BatchPlan, ArgError> {
    build_tool("docs_insert_text", json!({"document_id":ID,"text":"X","after_text":after}), state)
}
fn format(operations: Value, state: &Value) -> BatchPlan {
    build_tool("docs_format", json!({"document_id":ID,"operations":operations}), state).unwrap()
}
fn reply(replies: Value, document: bool) -> Vec<u8> {
    let mut value = json!({"replies":replies});
    value[if document { "documentId" } else { "spreadsheetId" }] = json!(ID);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn unique_anchor_and_utf16_across_runs() {
    let state = json!({"documentId":ID,"body":{"content":[{
        "startIndex":1,"endIndex":9,"paragraph":{"elements":[
            {"startIndex":1,"endIndex":4,"textRun":{"content":"甲😀"}},
            {"startIndex":4,"endIndex":9,"textRun":{"content":"乙abc\n"}}
        ]}
    }]}});
    let p = insert("😀乙", &state).unwrap();
    assert_eq!(p.requests, vec![json!({"insertText":{"location":{"index":5},"text":"X"}})]);
    let p = insert("甲", &state).unwrap();
    assert_eq!(p.requests[0]["insertText"]["location"]["index"], 2);
}

#[test]
fn missing_duplicate_and_overlapping_anchors_refused() {
    for (text, anchor) in [("one\n", "absent"), ("one one\n", "one"), ("aaa\n", "aa")] {
        assert!(matches!(insert(anchor, &doc(text)), Err(ArgError::AnchorNotUnique)));
    }
    assert_eq!(ArgError::AnchorNotUnique.user_text(), "找不到唯一的位置，請提供更精確的文字。");
}

#[test]
fn anchor_cannot_cross_nontext_gap() {
    let state = json!({"documentId":ID,"body":{"content":[{
        "startIndex":1,"endIndex":5,"paragraph":{"elements":[
            {"startIndex":1,"endIndex":2,"textRun":{"content":"a"}},
            {"startIndex":2,"endIndex":3,"inlineObjectElement":{}},
            {"startIndex":3,"endIndex":5,"textRun":{"content":"b\n"}}
        ]}
    }]}});
    assert!(matches!(insert("ab", &state), Err(ArgError::AnchorNotUnique)));
}

#[test]
fn start_end_and_tab_targeting() {
    for (at, expected) in [("start", 1), ("end", 5)] {
        let p = build_tool("docs_insert_text", json!({"document_id":ID,"text":"X","at":at}), &doc("甲😀乙\n")).unwrap();
        assert_eq!(p.requests[0]["insertText"]["location"]["index"], expected);
    }
    let state = json!({"documentId":ID,"tabs":[{"tabProperties":{"tabId":"parent"},"childTabs":[
        {"tabProperties":{"tabId":"t.2"},"documentTab":{"body":{"content":[paragraph(10,"甲😀乙\n")]}}}
    ]}]});
    for (at, expected) in [("start", 10), ("end", 14)] {
        let args = parse("docs_insert_text", &json!({"document_id":ID,"text":"X","at":at,"tab_id":"t.2"})).unwrap();
        assert!(matches!(read_call(&args, &verdict()), Ok(WriteCall::DocsGet { include_tabs: true, .. })));
        let p = build(&args, &verdict(), &state).unwrap();
        assert_eq!(p.requests[0]["insertText"]["location"], json!({"index":expected,"tabId":"t.2"}));
    }
    assert!(build_tool("docs_insert_text", json!({"document_id":ID,"text":"X","at":"end","tab_id":"missing"}), &state).is_err());
}

#[test]
fn paragraph_and_text_styles_have_exact_masks() {
    let p = format(json!([
        {"type":"paragraph_style","text":"😀","named_style":"HEADING_2","alignment":"CENTER"},
        {"type":"text_style","text":"😀","bold":false,"italic":true,"underline":true,"font_size_pt":12},
        {"type":"paragraph_style","text":"甲","named_style":"NORMAL_TEXT"},
        {"type":"text_style","text":"甲","bold":true}
    ]), &doc("甲😀乙\n"));
    assert_eq!(p.requests[0], json!({"updateParagraphStyle":{"range":{"startIndex":1,"endIndex":6},"paragraphStyle":{"namedStyleType":"HEADING_2","alignment":"CENTER"},"fields":"namedStyleType,alignment"}}));
    assert_eq!(p.requests[1], json!({"updateTextStyle":{"range":{"startIndex":2,"endIndex":4},"textStyle":{"bold":false,"italic":true,"underline":true,"fontSize":{"magnitude":12.0,"unit":"PT"}},"fields":"bold,italic,underline,fontSize"}}));
    assert_eq!(p.requests[2]["updateParagraphStyle"]["fields"], "namedStyleType");
    assert_eq!(p.requests[3]["updateTextStyle"]["fields"], "bold");
}

#[test]
fn table_cell_style_uses_table_location_and_rgb() {
    let state = json!({"documentId":ID,"body":{"content":[paragraph(1,"a\n"),
        {"startIndex":3,"endIndex":9,"table":{"tableRows":[{"tableCells":[{"content":[paragraph(5,"b\n")]}]}]}},
        paragraph(9,"\n")
    ]}});
    let p = format(json!([{ "type":"table_cell_style","table_index":0,"row":0,"column":0,"background_hex":"#FF8000" }]), &state);
    assert_eq!(p.requests[0], json!({"updateTableCellStyle":{
        "tableRange":{"tableCellLocation":{"tableStartLocation":{"index":3},"rowIndex":0,"columnIndex":0},"rowSpan":1,"columnSpan":1},
        "tableCellStyle":{"backgroundColor":{"color":{"rgbColor":{"red":1.0,"green":128.0/255.0,"blue":0.0}}}},"fields":"backgroundColor"
    }}));
}

#[test]
fn header_two_phase_and_add_tab_response() {
    let p = format(json!([{ "type":"add_tab","title":"第二頁" }, {"type":"header","text":"公司"}]), &doc("a\n"));
    assert_eq!(p.requests, vec![json!({"addDocumentTab":{"tabProperties":{"title":"第二頁"}}}), json!({"createHeader":{"type":"DEFAULT"}})]);
    let response = reply(json!([{"addDocumentTab":{"tabProperties":{"tabId":"t.new"}}},{"createHeader":{"headerId":"h.123"}}]), true);
    assert_eq!(p.phase2_requests(200, &response).unwrap(), vec![json!({"insertText":{"location":{"segmentId":"h.123","index":0},"text":"公司"}})]);
    assert!(matches!(p.phase2(200, &response), Ok(Some(WriteCall::DocsBatchUpdate { .. }))));
    let outcome = p.interpret(200, &response).unwrap();
    assert_eq!(outcome.tab_ids, vec!["t.new"]);
    assert_eq!(outcome.header_ids, vec!["h.123"]);
    assert!(outcome.needs_phase2);
    assert_eq!(outcome.link, format!("https://docs.google.com/document/d/{ID}/edit"));
    assert!(p.interpret_phase2(200, &reply(json!([{}]), true)).is_ok());
    assert!(p.phase2(200, &reply(json!([{},{}]), true)).is_err());
    assert!(p.phase2(500, &response).is_err());
}

#[test]
fn format_closed_fields_and_limits() {
    let bad = [
        json!({"type":"unknown"}), json!({"type":"header","text":"a","bold":true}),
        json!({"type":"text_style","text":"a","font_size":12}),
        json!({"type":"text_style","text":"a","bold":null}),
        json!({"type":"text_style","text":"a","bold":"true"}),
        json!({"type":"text_style","text":"a"}),
        json!({"type":"text_style","text":"a","font_size_pt":97}),
        json!({"type":"text_style","text":"a","font_size_pt":5}),
        json!({"type":"paragraph_style","text":"a","named_style":"HEADING_7"}),
        json!({"type":"paragraph_style","text":"a","named_style":"TITLE","alignment":"LEFT"}),
        json!({"type":"table_cell_style","table_index":100,"row":0,"column":0,"background_hex":"#ffffff"}),
        json!({"type":"table_cell_style","table_index":0,"row":500,"column":0,"background_hex":"#ffffff"}),
        json!({"type":"table_cell_style","table_index":0,"row":0,"column":50,"background_hex":"#ffffff"}),
        json!({"type":"table_cell_style","table_index":0,"row":0,"column":0,"background_hex":"#gggggg"}),
    ];
    for op in bad { assert!(parse("docs_format", &json!({"document_id":ID,"operations":[op]})).is_err()); }
    for size in [0, 51] {
        assert!(parse("docs_format", &json!({"document_id":ID,"operations":vec![json!({"type":"add_tab","title":"x"});size]})).is_err());
    }
    assert!(parse("docs_format", &json!({"document_id":ID,"operations":vec![json!({"type":"add_tab","title":"x"});50]})).is_ok());
}

#[test]
fn a1_examples_and_grid_ranges() {
    for (a1, sheet, sr, er, sc, ec) in [
        ("A1", None, 0, 1, 0, 1), ("B2:D10", None, 1, 10, 1, 4),
        ("'My Sheet'!A1:B2", Some("My Sheet"), 0, 2, 0, 2), ("Sheet1!Z100", Some("Sheet1"), 99, 100, 25, 26),
        ("'O''Brien'!AA1", Some("O'Brien"), 0, 1, 26, 27),
    ] {
        let r = parse_a1(a1).unwrap();
        assert_eq!(r.sheet.as_deref(), sheet);
        assert_eq!((r.start_row,r.end_row,r.start_column,r.end_column), (sr,er,sc,ec));
    }
    let p = build_tool("sheets_add_note", json!({"spreadsheet_id":ID,"range":"'My Sheet'!B2:D10","note":""}), &sheet_state()).unwrap();
    assert_eq!(p.requests, vec![json!({"repeatCell":{"range":{"sheetId":12,"startRowIndex":1,"endRowIndex":10,"startColumnIndex":1,"endColumnIndex":4},"cell":{"note":""},"fields":"note"}})]);
    let p = build_tool("sheets_add_note", json!({"spreadsheet_id":ID,"range":"A1","note":"n"}), &sheet_state()).unwrap();
    assert_eq!(p.requests[0]["repeatCell"]["range"]["sheetId"], 7);
}

#[test]
fn a1_malformed_overflow_and_large_ranges_refused() {
    for value in ["", "a1", "A0", "A01", "A-1", "$A$1", "A1:", "A1:B2:C3", "A:A", "1:2", "B2:A1", "A1 B2", "A1!A1!A1", "'bad!A1", "My Sheet!A1", "'x'junk!A1", "A999999999999999999999", "ZZZZZZZZZZZZZZZZ1", "'x'!A1\n"] {
        assert!(parse_a1(value).is_err(), "{value}");
    }
    assert!(parse_a1("A1:J100").is_ok());
    assert_eq!(parse_a1("A1:J101"), Err(ArgError::ContentTooLarge));
}

#[test]
fn add_sheet_duplicate_and_copy_resolution() {
    let make = |title, source: Option<&str>, state: &Value| {
        let mut v = json!({"spreadsheet_id":ID,"title":title});
        if let Some(source) = source { v["copy_from_sheet"] = json!(source); }
        build_tool("sheets_add_sheet", v, state)
    };
    assert!(matches!(make("Sheet1", None, &sheet_state()), Err(ArgError::SheetConflict)));
    assert!(matches!(make("New", Some("missing"), &sheet_state()), Err(ArgError::SheetNotUnique)));
    let ambiguous = json!({"sheets":[{"properties":{"sheetId":1,"title":"S"}},{"properties":{"sheetId":2,"title":"S"}}]});
    assert!(matches!(make("New", Some("S"), &ambiguous), Err(ArgError::SheetNotUnique)));
    let p = make("New", Some("Sheet1"), &sheet_state()).unwrap();
    assert_eq!(p.requests, vec![json!({"duplicateSheet":{"sourceSheetId":7,"newSheetName":"New"}})]);
    let outcome = p.interpret(200, &reply(json!([{"duplicateSheet":{"properties":{"sheetId":42}}}]), false)).unwrap();
    assert_eq!(outcome.sheet_ids, vec![42]);
    let p = make("New", None, &sheet_state()).unwrap();
    assert_eq!(p.requests, vec![json!({"addSheet":{"properties":{"title":"New"}}})]);
    assert_eq!(p.interpret(200, &reply(json!([{"addSheet":{"properties":{"sheetId":43}}}]), false)).unwrap().sheet_ids, vec![43]);
}

#[test]
fn ids_links_injection_and_unified_dispatch() {
    for (tool, key, base) in [
        ("docs_insert_text", "document_id", json!({"text":"x","at":"end"})),
        ("sheets_add_note", "spreadsheet_id", json!({"range":"A1","note":""})),
    ] {
        for id in ["x/permissions", "document_12345/permissions", "https://docs.google.com.evil/document/d/document_12345", "https://docs.google.com/document/d/x%2Fpermissions", "https://docs.google.com/document/d/../permissions"] {
            let mut value = base.clone(); value[key] = json!(id);
            assert!(matches!(parse_any(tool, &value), Err(ArgError::InvalidId)));
        }
        let mut value = base; value[key] = json!(format!("https://docs.google.com/document/d/{ID}/edit"));
        assert!(matches!(parse_any(tool, &value), Ok(ToolArgs::DocsSheets(_))));
    }
    assert!(matches!(parse_any("drive_trash_file", &json!({"file_id":ID})), Ok(ToolArgs::Drive(_))));
    assert!(matches!(parse_any("other", &json!({})), Err(ArgError::UnknownTool)));
}

#[test]
fn input_limits_choices_and_unknown_keys() {
    for value in [
        json!({"document_id":ID,"text":"","at":"start"}),
        json!({"document_id":ID,"text":"x","at":"start","after_text":"a"}),
        json!({"document_id":ID,"text":"x"}),
        json!({"document_id":ID,"text":"x","at":"middle"}),
        json!({"document_id":ID,"text":"x","after_text":""}),
        json!({"document_id":ID,"text":"x","at":"end","tab_id":"../bad/"}),
        json!({"document_id":ID,"text":"x","at":"end","extra":true}),
        json!({"document_id":ID,"text":"甲".repeat(20001),"at":"end"}),
        json!({"document_id":ID,"text":"x","after_text":"甲".repeat(1001)}),
    ] { assert!(parse("docs_insert_text", &value).is_err()); }
    assert!(parse("docs_insert_text", &json!({"document_id":ID,"text":"😀".repeat(20000),"at":"end"})).is_ok());
    for title in ["", "a[b", "a]b", "a*b", "a?b", "a/b", "a\\b", "a:b"] {
        assert!(parse("sheets_add_sheet", &json!({"spreadsheet_id":ID,"title":title})).is_err());
    }
    assert!(parse("sheets_add_note", &json!({"spreadsheet_id":ID,"range":"A1","note":"x".repeat(5001)})).is_err());
}

#[test]
fn scope_state_and_response_fail_closed() {
    let args = parse("docs_insert_text", &json!({"document_id":ID,"text":"x","at":"end"})).unwrap();
    assert!(matches!(plan(&args), Plan::Modify { rename: false, .. }));
    assert!(matches!(build(&args, &ScopeVerdict::Failed, &doc("a\n")), Err(ArgError::ScopeUnconfirmed)));
    assert!(read_call(&args, &ScopeVerdict::Failed).is_err());
    assert!(build(&args, &verdict(), &json!({"documentId":"another_doc_123","body":{}})).is_err());
    let p = build(&args, &verdict(), &doc("a\n")).unwrap();
    assert!(p.interpret(200, &reply(json!([{}]), true)).is_ok());
    assert!(p.phase2(200, &reply(json!([{}]), true)).unwrap().is_none());
    assert_eq!(p.interpret(403, br#"{"error":{"errors":[{"reason":"insufficientPermissions"}]}}"#), Err(OutcomeError::InsufficientPermissions));
    assert_eq!(p.interpret(500, b"{}"), Err(OutcomeError::Google(500)));
    for body in [b"{}".as_slice(), b"garbage", b"{\"documentId\":\"document_12345\",\"replies\":[]}"] {
        assert_eq!(p.interpret(200, body), Err(OutcomeError::Unconfirmed));
    }
}
