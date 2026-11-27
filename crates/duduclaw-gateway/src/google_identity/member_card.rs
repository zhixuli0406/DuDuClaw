//! Member confirmations never inherit managerial or chat-admin authority.
use std::path::Path;
use serde_json::Value;
use crate::approval::ApprovalRecord;
use super::{chat_send, drive_write::approval as drive};

pub(crate) fn is_member_card(rec: &ApprovalRecord) -> bool {
    // Recognize even malformed cards so they cannot fall through to Admin gates.
    rec.payload["chat_member_confirmation"] == true
        || rec.payload["drive_write_member_confirmation"] == true
}
fn drive_card(rec: &ApprovalRecord) -> bool {
    rec.payload["drive_write_member_confirmation"] == true
        && rec.payload["chat_member_confirmation"] != true
}
fn chat_card(rec: &ApprovalRecord) -> bool {
    chat_send::is_member_card(rec) && rec.payload["drive_write_member_confirmation"] != true
}
pub(crate) fn eligible(home: &Path, rec: &ApprovalRecord) -> bool {
    if drive_card(rec) { drive::eligible(home, rec) }
    else { chat_card(rec) && chat_send::eligible(home, rec) }
}
pub(crate) fn may_decide(home: &Path, rec: &ApprovalRecord, uid: &str) -> bool {
    if drive_card(rec) { drive::may_decide(home, rec, uid) }
    else { chat_card(rec) && chat_send::may_decide(home, rec, uid) }
}
pub(crate) fn decided_by_member(home: &Path, rec: &ApprovalRecord) -> bool {
    if drive_card(rec) { drive::decided_by_member(home, rec) }
    else { chat_card(rec) && chat_send::decided_by_member(home, rec) }
}
pub(crate) fn confirmation_display(rec: &ApprovalRecord) -> Value {
    if drive_card(rec) { drive::confirmation_display(rec) }
    else if chat_card(rec) { chat_send::confirmation_display(rec) }
    else { Value::Null }
}
pub(crate) fn card_body(rec: &ApprovalRecord) -> String {
    if drive_card(rec) { drive::card_body(rec) }
    else if chat_card(rec) { chat_send::card_body(rec) }
    else { String::new() }
}
/// Only use after eligibility has re-derived the member from gateway identity.
pub(crate) fn member_id(rec: &ApprovalRecord) -> Option<&str> {
    let snapshot = if drive_card(rec) { "drive_snapshot" } else if chat_card(rec) { "chat_snapshot" } else { return None; };
    rec.payload[snapshot]["member"].as_str()
}
