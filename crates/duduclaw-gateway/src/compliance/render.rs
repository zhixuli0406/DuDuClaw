use super::*;
use duduclaw_redaction::{RuleEngine, Source};
use sha2::{Digest, Sha256};

struct Labels {
    title: &'static str,
    sections: [&'static str; 4],
    headers: [&'static str; 10],
    summary: &'static str,
    note: &'static str,
    coverage: &'static str,
    masked: &'static str,
    full: &'static str,
}
fn labels(language: Language) -> Labels {
    match language {
        Language::ZhTw => Labels {
            title: "個人資料處理合規報表", sections: ["私人資料外送決策", "審批紀錄", "已記錄的資料來源處理", "動作審查"],
            headers: ["時間（UTC）", "員工", "目的地通路／主機", "資料類別／來源標籤", "決策", "核准者", "原因", "種類", "申請者", "紀錄編號"],
            summary: "摘要筆數", masked: "已遮罩", full: "包含個人資料",
            note: "保存說明：本報表只涵蓋目前保留的審批與稽核紀錄，輪替、刪除或未啟用記錄的資料無法還原；來源處理紀錄表示已記錄的資料來源工具呼叫與偵測／遮罩事件，不代表完整存取清單。閘門決策與核准不代表實際送達，同一動作可能有多筆事件。空白表示未記錄。期間含起點、不含終點。報表不構成個資法合規認證；請依組織目的、法定義務與保存政策管理下載副本。",
            coverage: "來源涵蓋與判讀說明：",
        },
        Language::En => Labels {
            title: "Personal data processing compliance report", sections: ["Private data outbound decisions", "Approvals", "Recorded data-source processing", "Action reviews"],
            headers: ["Time (UTC)", "Employee", "Destination channel / host", "Data categories / source labels", "Decision", "Approver", "Reason", "Kind", "Requester", "Record ID"],
            summary: "Summary counts", masked: "Masked", full: "Includes personal data",
            note: "Retention: this report covers only currently retained approval and audit records. Rotated, deleted or unrecorded evidence cannot be recovered. Source processing rows represent recorded data-source tool calls and detection/redaction events, not a complete access inventory. Gate decisions and approvals are not delivery receipts; one action may have several events. Blank means unrecorded. The range includes its start and excludes its end. This report is not legal compliance certification. Manage downloaded copies under your organization's purpose, legal obligations and retention policy.",
            coverage: "Source coverage and interpretation:",
        },
        Language::Ja => Labels {
            title: "個人データ処理コンプライアンスレポート", sections: ["非公開データの外部送信判断", "承認履歴", "記録されたデータソース処理", "アクション審査"],
            headers: ["時刻（UTC）", "従業員", "送信先チャネル／ホスト", "データ分類／ソースラベル", "判断", "承認者", "理由", "種類", "申請者", "記録ID"],
            summary: "件数の概要", masked: "マスク済み", full: "個人データを含む",
            note: "保存について：現在保持されている承認・監査記録のみが対象です。ローテーション、削除、未記録の情報は復元できません。ソース処理行は記録されたデータソースのツール呼び出しと検出・マスクのイベントであり、完全なアクセス一覧ではありません。判断・承認は到達証明ではなく、同じ操作に複数の記録が存在する場合があります。空欄は未記録を表します。期間は開始を含み終了を含みません。法令遵守を認証するものではありません。ダウンロードした副本は組織の目的、法的義務、保存方針に従って管理してください。",
            coverage: "ソースの対象範囲と解釈：",
        },
    }
}

fn coverage_note(note: &str, language: Language) -> &'static str {
    match (note, language) {
        ("approvals_missing", Language::ZhTw) => "沒有可讀取的審批歷史來源。",
        ("approvals_missing", Language::En) => "No retained approval history source exists.",
        ("approvals_missing", Language::Ja) => "保存された承認履歴ソースがありません。",
        ("tool_audit_missing", Language::ZhTw) => "沒有保留的工具稽核紀錄。",
        ("tool_audit_missing", Language::En) => "No retained tool audit records exist.",
        ("tool_audit_missing", Language::Ja) => "保存されたツール監査記録がありません。",
        ("redaction_audit_missing", Language::ZhTw) => "沒有保留的資料遮罩稽核紀錄。",
        ("redaction_audit_missing", Language::En) => "No retained redaction audit records exist.",
        ("redaction_audit_missing", Language::Ja) => "保存されたマスク監査記録がありません。",
        ("approval_status_snapshot", Language::ZhTw) => "審批狀態為產生報表時的快照；決策在期間外或時間未記錄時會明確標記，不推測過去狀態。",
        ("approval_status_snapshot", Language::En) => "Approval states are current snapshots. Decisions outside the range or without a recorded time are marked explicitly; historical states are not inferred.",
        ("approval_status_snapshot", Language::Ja) => "承認状態は生成時点のスナップショットです。期間外または時刻未記録の判断には明示的な印を付け、過去の状態を推測しません。",
        (_, Language::ZhTw) => "部分歷史來源無法涵蓋；請配合原始紀錄查核。",
        (_, Language::En) => "Some historical evidence is not covered; consult original records.",
        (_, Language::Ja) => "一部の履歴は対象外です。元の記録と併せて確認してください。",
    }
}

struct Masker { engine: RuleEngine, salt: String, full: bool }
impl Masker {
    fn new(full: bool) -> Result<Self, String> {
        let mut specs = Vec::new();
        for name in ["general", "taiwan_strict"] {
            let profile = duduclaw_redaction::profiles::load_builtin(name)
                .map_err(|_| "COMPLIANCE_MASK_UNAVAILABLE".to_string())?
                .ok_or("COMPLIANCE_MASK_UNAVAILABLE")?;
            specs.extend(profile.into_specs());
        }
        Ok(Self { engine: RuleEngine::from_specs(specs).map_err(|_| "COMPLIANCE_MASK_UNAVAILABLE".to_string())?,
            salt: uuid::Uuid::new_v4().to_string(), full })
    }
    fn text(&self, value: &str) -> Result<String, String> {
        // Refuse oversized cells rather than cutting a credential or a PII span.
        if value.len() > 8_000 { return Err("COMPLIANCE_TOO_LARGE".into()); }
        let mut text = duduclaw_security::credential_detect::mask_document(value)
            .map_err(|_| "COMPLIANCE_MASK_UNAVAILABLE".to_string())?;
        if !self.full {
            let hits = self.engine.apply(&text, &Source::ToolResult { tool_name: "compliance_report".into() });
            for hit in hits.iter().rev() { text.replace_range(hit.span.start..hit.span.end, "[MASKED]"); }
        }
        Ok(text)
    }
    fn person(&self, value: &str) -> Result<String, String> {
        if self.full || value.is_empty() { return self.text(value); }
        let mut hash = Sha256::new();
        hash.update(self.salt.as_bytes());
        hash.update(value.as_bytes());
        Ok(format!("person-{}", duduclaw_core::truncate_chars(&format!("{:x}", hash.finalize()), 12)))
    }
    fn row(&self, row: &Row) -> Result<Vec<String>, String> {
        Ok(vec![self.text(&row.time)?, self.person(&row.employee)?,
            if self.full || row.destination.is_empty() { self.text(&row.destination)? } else { "[MASKED]".into() },
            self.text(&row.categories)?, self.text(&row.decision)?, self.person(&row.approver)?,
            if self.full || row.reason.is_empty() { self.text(&row.reason)? } else { "[MASKED]".into() },
            self.text(&row.kind)?, self.person(&row.requester)?, self.text(&row.id)?])
    }
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}
fn csv_cell(value: &str) -> String {
    let trimmed = value.trim_start_matches(|c: char| c.is_whitespace() || c.is_control());
    if trimmed.starts_with(['=', '+', '-', '@']) || value.starts_with(['\t', '\r', '\n']) {
        format!("'{value}")
    } else { value.into() }
}

pub(super) fn encode(report: &Report, request: &Request) -> Result<Vec<u8>, String> {
    let l = labels(request.language);
    let mask = Masker::new(request.include_personal_data)?;
    let sections = [&report.outbound, &report.approvals, &report.accesses, &report.reviews];
    let count: usize = sections.iter().map(|rows| rows.len()).sum();
    if count > MAX_ROWS { return Err("COMPLIANCE_TOO_LARGE".into()); }
    let mut table = vec![vec![l.title.into()], vec![request.from.to_rfc3339(), request.until.to_rfc3339()],
        vec![if request.include_personal_data { l.full.into() } else { l.masked.into() }], vec![l.summary.into()]];
    for (name, rows) in l.sections.iter().zip(sections) { table.push(vec![(*name).into(), rows.len().to_string()]); }
    table.push(vec![l.note.into()]);
    if !report.notes.is_empty() {
        table.push(vec![l.coverage.into()]);
        for note in &report.notes { table.push(vec![coverage_note(note, request.language).into()]); }
    }
    for (name, rows) in l.sections.iter().zip(sections) {
        table.push(vec![(*name).into()]);
        table.push(l.headers.iter().map(|s| (*s).into()).collect());
        for row in rows { table.push(mask.row(row)?); }
    }
    // Bound uncompressed input before asking the XLSX encoder to allocate.
    let size: usize = table.iter().flatten().map(String::len).sum();
    if size > MAX_OUTPUT / 2 { return Err("COMPLIANCE_TOO_LARGE".into()); }
    let bytes = match request.format {
        Format::Csv => {
            let mut writer = csv::WriterBuilder::new().flexible(true).from_writer(vec![0xef, 0xbb, 0xbf]);
            for row in table { writer.write_record(row.iter().map(|s| csv_cell(s))).map_err(|_| "COMPLIANCE_ENCODE".to_string())?; }
            writer.into_inner().map_err(|_| "COMPLIANCE_ENCODE".to_string())?
        },
        Format::Xlsx => {
            let mut workbook = rust_xlsxwriter::Workbook::new();
            let sheet = workbook.add_worksheet();
            sheet.set_column_width(0, 28).map_err(|_| "COMPLIANCE_ENCODE".to_string())?;
            for (index, row) in table.iter().enumerate() {
                for (column, value) in row.iter().enumerate() {
                    sheet.write_string(index as u32, column as u16, value).map_err(|_| "COMPLIANCE_ENCODE".to_string())?;
                }
            }
            workbook.save_to_buffer().map_err(|_| "COMPLIANCE_ENCODE".to_string())?
        },
        Format::Html => {
            let lang = match request.language { Language::ZhTw => "zh-Hant", Language::En => "en", Language::Ja => "ja" };
            let mut html = format!("<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><title>{}</title><style>body{{font:14px system-ui;margin:24px;color:#102b48}}table{{border-collapse:collapse;width:100%;table-layout:fixed}}td{{border:1px solid #bbb;padding:6px;overflow-wrap:anywhere;white-space:pre-wrap}}@media print{{@page{{size:A4 landscape;margin:12mm}}body{{margin:0}}tr{{break-inside:avoid}}}}</style></head><body><h1>{}</h1><table>", escape(l.title), escape(l.title));
            for row in table {
                html.push_str("<tr>");
                let width = if row.len() == 1 { " colspan=\"10\"" } else { "" };
                for value in row { html.push_str(&format!("<td{width}>{}</td>", escape(&value))); }
                html.push_str("</tr>");
            }
            html.push_str("</table></body></html>");
            html.into_bytes()
        },
    };
    if bytes.len() > MAX_OUTPUT { return Err("COMPLIANCE_TOO_LARGE".into()); }
    Ok(bytes)
}
