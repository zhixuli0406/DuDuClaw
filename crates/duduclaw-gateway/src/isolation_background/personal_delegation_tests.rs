//! SR2 finding 2 (owner decision 2026-11-08): Personal → Shared delegation
//! needs a single-use owner approval bound to (source employee, target
//! employee, full task text, stable delivery fields). Real approvals.db and
//! receipt store; the live run association (`delegation_source`) is
//! exercised on Linux by `record_delegation`. The owner list RPC and the
//! generic approval visibility are covered in the handler tests
//! (`personal_delegation_shown_text_equals_bound_text`,
//! `personal_delegation_card_hidden_from_admin_list`).
use super::*;
use duduclaw_isolation::{bounded::Text, execution::RunHandle, ids::RunId};

const OWNER: &str = "alice-user-id";
const SOURCE: &str = "alice-personal";
const TARGET: &str = "team";
const SOURCE_ID: EmployeeId = EmployeeId([1; 16]);
const TARGET_ID: EmployeeId = EmployeeId([2; 16]);

fn personal() -> EmployeeBinding { EmployeeBinding::Personal(Text::new(OWNER.into()).unwrap()) }
fn shared() -> EmployeeBinding { EmployeeBinding::Shared(Text::new("tenant-1".into()).unwrap()) }

fn material(task: &str) -> Value {
    super::super::material::delegation(SOURCE, TARGET, task, None, None, None, 1, None, None,
        Some(SOURCE), false, 0, false, Some("user:alice-user-id"))
}

/// One delegation material with every field the resend may or may not change.
struct Resend<'a> { task: &'a str, turn: &'a str, session: &'a str, reply: &'a str, lane: Option<&'a str>,
    owner: &'a str, spawned: bool }
impl Resend<'_> {
    fn material(&self) -> Value {
        super::super::material::delegation(SOURCE, TARGET, self.task, Some(self.turn), Some(self.session), self.lane,
            1, Some(SOURCE), Some(self.reply), Some(SOURCE), false, 0, self.spawned, Some(self.owner))
    }
    fn gate(&self) -> Gate {
        gate(&personal(), OWNER, SOURCE, SOURCE_ID, TARGET, TARGET_ID, &self.material()).unwrap().unwrap()
    }
}
const FIRST: Resend<'static> = Resend { task: "整理客戶回覆", turn: "turn-1", session: "session-1", reply: "line:room",
    lane: None, owner: "user:alice-user-id", spawned: false };

fn gate_for(task: &str, target: EmployeeId) -> Gate {
    gate(&personal(), OWNER, SOURCE, SOURCE_ID, TARGET, target, &material(task)).unwrap().expect("personal source")
}

/// The exact delegation receipt `record_delegation` builds for the target.
fn receipt(home: &Path, id: &str, task: &str, target: EmployeeId) -> RecordAuthority {
    let mut value = super::super::tests::receipt(home);
    value.kind = "delegation".into();
    value.id = id.into();
    value.agent = TARGET.into();
    value.employee = target;
    value.approval = None;
    value.caller = Some(SOURCE_ID);
    value.parent_run = Some(RunHandle(RunId([3; 16])));
    value.parent_agent = Some(SOURCE.into());
    value.parent_policy = Some("source-policy".into());
    value.material = store::material_digest(&material(task)).unwrap();
    value
}

/// A receipt from another source employee.
fn receipt_from(home: &Path, id: &str, source: EmployeeId) -> RecordAuthority {
    let mut value = receipt(home, id, "x", TARGET_ID);
    value.caller = Some(source);
    value
}

async fn cards(home: &Path) -> Vec<ApprovalRecord> {
    ApprovalBroker::open(home).unwrap().list_by_kind(KIND).await.unwrap()
}

async fn pending_card(home: &Path, gate: &Gate) -> crate::approval::ApprovalId {
    cards(home).await.into_iter().find(|card| binds(card, gate) && card.status == ApprovalStatus::Pending)
        .expect("a pending owner card").id
}

async fn approve(home: &Path, gate: &Gate) {
    let id = pending_card(home, gate).await;
    ApprovalBroker::open(home).unwrap().decide(&id, true, &format!("dashboard:{OWNER}")).await.unwrap();
}

fn queued(home: &Path, id: &str) -> bool { store::load(home, "delegation", id).unwrap().is_some() }

#[tokio::test]
async fn personal_to_shared_delegation_needs_owner_approval() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    let task = "請整理本週客戶回覆，並草擬給業務團隊的摘要。".repeat(20);
    let gate = gate_for(&task, TARGET_ID);
    assert_eq!(gate.task_digest, task_digest(&task));
    let first = receipt(home, "msg-1", &task, TARGET_ID);

    // 1. Nothing is queued; one owner card names source and target and binds
    //    (source id, target id, the full task text and its digest).
    assert_eq!(admit(home, &gate, &first).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-1"));
    let filed = cards(home).await;
    assert_eq!(filed.len(), 1);
    let card = &filed[0];
    assert_eq!((card.agent_id.as_str(), card.status), (SOURCE, ApprovalStatus::Pending));
    assert!(card.summary.contains(SOURCE) && card.summary.contains(TARGET), "{}", card.summary);
    assert!(!card.summary.contains("請整理"), "the generic summary carries no task text: {}", card.summary);
    assert_eq!(card.payload["task"], task);
    assert_eq!(shown_task(card), Some(task.as_str()));
    assert_eq!(card.payload["source_employee"], hex::encode(SOURCE_ID.0));
    assert_eq!(card.payload["target_employee"], hex::encode(TARGET_ID.0));
    assert_eq!(card.payload["task_digest"], task_digest(&task));
    assert_eq!(card.payload["owner_user_id"], OWNER);
    // Retrying while it is pending files no second card.
    assert_eq!(admit(home, &gate, &first).await, Err(DelegationError::OwnerApprovalPending));
    assert_eq!(cards(home).await.len(), 1);
    // The generic approval centre cannot decide it, not even an Admin.
    let handler = crate::handlers::MethodHandler::new(home.to_path_buf()).await;
    let frame = handler.handle_approvals_decide(json!({"id": card.id.as_str(), "approve": true}),
        &duduclaw_auth::UserContext::admin_fallback()).await;
    assert!(matches!(&frame, crate::protocol::WsFrame::Response { ok: false, error: Some(error), .. }
        if error.as_str() == Some(DECIDE_ELSEWHERE)), "{frame:?}");
    assert_eq!(pending_card(home, &gate).await, card.id);

    // 2. Approve (owner, dashboard) → queued exactly once.
    let broker = ApprovalBroker::open(home).unwrap();
    broker.decide(&card.id, true, &format!("dashboard:{OWNER}")).await.unwrap();
    assert_eq!(admit(home, &gate, &first).await, Ok(()));
    assert!(queued(home, "msg-1"));
    // Replay of the same delegation id: refused, nothing new.
    assert_eq!(admit(home, &gate, &first).await, Err(DelegationError::Failed(VerificationError::Revoked)));
    // Replay of the approval for a new delegation id: single use, a new card.
    let second = receipt(home, "msg-2", &task, TARGET_ID);
    assert_eq!(admit(home, &gate, &second).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-2"));
    assert_eq!(cards(home).await.len(), 2);

    // 3. Deny (owner) → not queued, and no new card while the denial stands.
    let denied = pending_card(home, &gate).await;
    broker.decide(&denied, false, &format!("dashboard:{OWNER}")).await.unwrap();
    assert_eq!(admit(home, &gate, &second).await, Err(DelegationError::OwnerDenied));
    assert!(!queued(home, "msg-2"));
    assert_eq!(cards(home).await.len(), 2);

    // 4. Expired: an owner approval past the card's TTL never sends.
    let late_task = "another task";
    let late = gate_for(late_task, TARGET_ID);
    let third = receipt(home, "msg-3", late_task, TARGET_ID);
    assert_eq!(admit(home, &late, &third).await, Err(DelegationError::OwnerApprovalPending));
    let id = pending_card(home, &late).await;
    broker.decide(&id, true, &format!("dashboard:{OWNER}")).await.unwrap();
    let old = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
    let older = (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339();
    rusqlite::Connection::open(home.join("approvals.db")).unwrap()
        .execute("UPDATE approvals SET created_at=?1, decided_at=?2 WHERE id=?3",
            rusqlite::params![older, old, id.as_str()]).unwrap();
    assert_eq!(admit(home, &late, &third).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-3"));

    // 5. Approved by anyone but the owner (an Admin, a channel): never sends.
    let other_task = "third task";
    let other = gate_for(other_task, TARGET_ID);
    let fourth = receipt(home, "msg-4", other_task, TARGET_ID);
    assert_eq!(admit(home, &other, &fourth).await, Err(DelegationError::OwnerApprovalPending));
    broker.decide(&pending_card(home, &other).await, true, "dashboard:admin-user-id").await.unwrap();
    assert_eq!(admit(home, &other, &fourth).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-4"));

    // 6. The approval is bound to the target employee: another target id
    //    (e.g. the label recreated) cannot use it.
    let moved_task = "fourth task";
    let original = gate_for(moved_task, TARGET_ID);
    let fifth = receipt(home, "msg-5", moved_task, TARGET_ID);
    assert_eq!(admit(home, &original, &fifth).await, Err(DelegationError::OwnerApprovalPending));
    broker.decide(&pending_card(home, &original).await, true, &format!("dashboard:{OWNER}")).await.unwrap();
    let recreated = gate_for(moved_task, EmployeeId([9; 16]));
    let sixth = receipt(home, "msg-6", moved_task, EmployeeId([9; 16]));
    assert_eq!(admit(home, &recreated, &sixth).await, Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "msg-6"));
    // A receipt that does not match the gate is refused outright.
    assert_eq!(admit(home, &original, &sixth).await, Err(DelegationError::Failed(VerificationError::BindingMismatch)));
    // The owner's own approval still works once for the original target.
    assert_eq!(admit(home, &original, &fifth).await, Ok(()));
    assert!(queued(home, "msg-5"));

    assert_eq!(DelegationError::OwnerApprovalPending.user_message(), Some(PENDING_TEXT));
    assert_eq!(DelegationError::OwnerDenied.user_message(), Some(DENIED_TEXT));
}

/// F6b A4: the card binds the source incarnation; an owner approval of a
/// card filed for a prior incarnation never queues a delegation of the
/// restored employee (same employee id, incarnation + 1), and a receipt whose
/// caller incarnation differs from the gate is refused outright.
#[tokio::test]
async fn delegation_card_from_prior_incarnation_never_binds() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    let task = "整理上週的工單";
    let before = gate_for(task, TARGET_ID);
    assert_eq!(before.source_incarnation, 0);
    let first = receipt(home, "inc-1", task, TARGET_ID);
    assert_eq!(admit(home, &before, &first).await, Err(DelegationError::OwnerApprovalPending));
    let card = cards(home).await.into_iter().next().unwrap();
    assert_eq!(card.payload["source_incarnation"], json!(0));
    approve(home, &before).await;
    // Restored: same source id, next incarnation.
    let after = gate_for(task, TARGET_ID).with_source_incarnation(1);
    assert_ne!(after.payload(), before.payload());
    let mut restored = receipt(home, "inc-2", task, TARGET_ID);
    restored.caller_incarnation = 1;
    assert_eq!(admit(home, &after, &restored).await, Err(DelegationError::OwnerApprovalPending),
        "the old approval does not bind; a new card is filed");
    assert!(!queued(home, "inc-2"));
    assert_eq!(cards(home).await.len(), 2);
    // Gate and receipt must agree on the source incarnation.
    assert_eq!(admit(home, &before, &restored).await, Err(DelegationError::Failed(VerificationError::BindingMismatch)));
    assert_eq!(admit(home, &after, &first).await, Err(DelegationError::Failed(VerificationError::BindingMismatch)));
    assert!(!queued(home, "inc-1"));
}

#[tokio::test]
async fn shared_delegation_unchanged() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    // Shared → anything: no gate, whatever the principal.
    for principal in [OWNER, "someone-else", ""] {
        assert_eq!(gate(&shared(), principal, SOURCE, SOURCE_ID, TARGET, TARGET_ID, &material("x")), Ok(None));
    }
    // The Shared path stores the receipt directly; no owner card is filed.
    let value = receipt(home, "shared-1", "x", TARGET_ID);
    store::save(&value).unwrap();
    assert!(queued(home, "shared-1"));
    assert!(cards(home).await.is_empty());
    // A Personal source whose run principal is not the owner is refused, and
    // a delegation without task text cannot be bound.
    assert_eq!(gate(&personal(), "someone-else", SOURCE, SOURCE_ID, TARGET, TARGET_ID, &material("x")),
        Err(VerificationError::Revoked));
    assert_eq!(gate(&personal(), OWNER, SOURCE, SOURCE_ID, TARGET, TARGET_ID, &json!({})),
        Err(VerificationError::Invalid));
}

/// N1: the owner is shown exactly the bound text, so it must fit on the card
/// (≤ MAX_TASK_CHARS) and survive credential masking and rendering unchanged.
/// Otherwise no card is filed, nothing is sent, and the employee is told why.
#[tokio::test]
async fn personal_delegation_oversized_task_refused() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    // One character over the cap: refused before any card; nothing queued.
    let oversized = "字".repeat(MAX_TASK_CHARS + 1);
    let refused = receipt(home, "big-1", &oversized, TARGET_ID);
    assert_eq!(admit(home, &gate_for(&oversized, TARGET_ID), &refused).await, Err(DelegationError::TaskTooLong));
    assert!(cards(home).await.is_empty());
    assert!(!queued(home, "big-1"));
    assert_eq!(DelegationError::TaskTooLong.user_message(), Some(TOO_LONG_TEXT));
    assert!(TOO_LONG_TEXT.contains("4000"));
    // Exactly at the cap: a card carrying the whole text.
    let full = "字".repeat(MAX_TASK_CHARS);
    let fits = gate_for(&full, TARGET_ID);
    assert_eq!(admit(home, &fits, &receipt(home, "big-2", &full, TARGET_ID)).await,
        Err(DelegationError::OwnerApprovalPending));
    let filed = cards(home).await;
    assert_eq!(filed.len(), 1);
    assert_eq!(shown_task(&filed[0]), Some(full.as_str()));

    // Text the owner would not see as bound: a credential (masked on every
    // approval surface) or invisible/bidirectional formatting.
    let aws = format!("請用 AKIA{} 上傳報表", "A".repeat(16));
    assert_ne!(duduclaw_security::credential_detect::mask(&aws), aws, "fixture must be masked");
    for (n, task) in [aws.as_str(), "核准\u{202e}txt.exe", "看不見\u{200b}的字", "響鈴\u{7}"].into_iter().enumerate() {
        let id = format!("hidden-{n}");
        assert_eq!(admit(home, &gate_for(task, TARGET_ID), &receipt(home, &id, task, TARGET_ID)).await,
            Err(DelegationError::TaskNotShowable), "{task:?}");
        assert!(!queued(home, &id));
    }
    assert_eq!(cards(home).await.len(), 1, "no card for any refused task");
    assert_eq!(DelegationError::TaskNotShowable.user_message(), Some(NOT_SHOWABLE_TEXT));
    // Line breaks, tabs and emoji joiners are shown as they are (a carriage
    // return is not: SR2 round 4 allows only `\n` and `\t`).
    let plain = "第一步：整理\n第二步：\t寄出 👩\u{200d}💻";
    assert_eq!(admit(home, &gate_for(plain, TARGET_ID), &receipt(home, "plain", plain, TARGET_ID)).await,
        Err(DelegationError::OwnerApprovalPending));

    // A stored card whose text was altered or is not showable is never shown.
    let mut card = cards(home).await.into_iter().find(|card| card.payload["task"] == full).unwrap();
    card.payload["task"] = json!(format!("{full}!"));
    assert_eq!(shown_task(&card), None, "text no longer matches the digest");
    card.payload["task_digest"] = json!(task_digest(&oversized));
    card.payload["task"] = json!(oversized);
    assert_eq!(shown_task(&card), None, "over the cap");
}

/// SR2 round 4 (N1): `true` when the task is refused as not showable.
fn hidden(task: &str) -> bool { showable(task) == Err(DelegationError::TaskNotShowable) }

fn assert_hidden(tasks: &[String]) {
    for task in tasks { assert!(hidden(task), "must be refused: {task:?} {:x?}", task.chars().map(u32::from).collect::<Vec<_>>()); }
}

fn assert_shown(tasks: &[&str]) {
    for task in tasks { assert_eq!(showable(task), Ok(()), "must be shown: {task:?}"); }
}

#[test]
fn tag_characters_refused() {
    let tags: Vec<String> = ['\u{e0000}', '\u{e0001}', '\u{e0020}', '\u{e0041}', '\u{e0067}', '\u{e007f}', '\u{e0080}']
        .iter().map(|c| format!("核准{c}這次交辦")).collect();
    assert_hidden(&tags);
    // Hidden ASCII spelled in tags after a visible word.
    let smuggled: String = "approve".chars().map(|c| char::from_u32(0xe0000 + u32::from(c)).unwrap()).collect();
    // An emoji tag sequence (subdivision flag) carries tags too: refused.
    assert_hidden(&[format!("整理{smuggled}"), "🏴\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}".into()]);
    assert_shown(&["整理報表 🏴"]);
}

#[test]
fn variation_selectors_refused_except_after_emoji() {
    let refused: Vec<String> = ["a\u{fe0f}", "字\u{fe0e}", "\u{fe0f}開頭", "核\u{e0100}", "核\u{e01ef}", "a\u{fe00}",
        "a\u{fe0d}", "ᠠ\u{180b}", "ᠠ\u{180f}", "😀\u{fe0f}\u{fe0f}", "😀\u{fe0e}\u{fe0f}", "1\u{fe0f}", "1\u{fe0f}2",
        "a\u{fe0f}\u{20e3}", "😀\u{fe0f}\u{e0100}"].iter().map(|s| (*s).to_owned()).collect();
    assert_hidden(&refused);
    assert_shown(&["❤\u{fe0f}", "☺\u{fe0e}", "😀\u{fe0f}", "1\u{fe0f}\u{20e3} 第一項", "#\u{fe0f}\u{20e3}", "©\u{fe0f} 2026"]);
}

#[test]
fn arabic_letter_mark_refused() {
    let mut refused: Vec<String> = ["مرحبا\u{061c}", "\u{061c}abc", "a\u{061c}b"].iter().map(|s| (*s).to_owned()).collect();
    // Every bidi control, between letters of either direction.
    for c in ['\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}'] {
        refused.push(format!("invoice{c}exe"));
        refused.push(format!("مر{c}حبا"));
    }
    // Joiners only between letters of a script that needs them, one at a time.
    refused.extend(["ab\u{200c}cd", "می\u{200c}", "\u{200c}می", "می\u{200c}\u{200c}خواهم", "می\u{200c}a", "字\u{200d}字",
        "ab\u{200d}cd"].iter().map(|s| (*s).to_owned()));
    assert_hidden(&refused);
    assert_shown(&["می\u{200c}خواهم", "क्\u{200d}ष", "ক্\u{200c}ষ", "مرحبا بالعالم", "नमस्ते"]);
}

#[test]
fn soft_hyphen_and_fillers_refused() {
    let refused: Vec<String> = ['\u{00ad}', '\u{034f}', '\u{115f}', '\u{1160}', '\u{3164}', '\u{ffa0}', '\u{180e}',
        '\u{fff9}', '\u{fffa}', '\u{fffb}', '\u{17b4}', '\u{17b5}', '\u{2028}', '\u{2029}', '\u{200b}', '\u{2060}',
        '\u{2061}', '\u{2064}', '\u{feff}', '\u{1bca0}', '\u{1d173}', '\r', '\u{0}', '\u{1b}', '\u{85}', '\u{9f}']
        .iter().map(|c| format!("整理{c}報表")).collect();
    assert_hidden(&refused);
    // A Hangul filler alone (a "blank" name or task) is refused too.
    assert_hidden(&["\u{3164}".into(), "\u{115f}\u{1160}".into()]);
    assert_shown(&["第一行\n\t第二行", "한글 정리", "整理\u{3000}報表"]);
}

#[test]
fn zwj_emoji_sequence_allowed() {
    assert_shown(&["👩\u{200d}💻 寫程式", "👨\u{200d}👩\u{200d}👧", "🧑🏽\u{200d}💻", "🏳\u{fe0f}\u{200d}🌈",
        "❤\u{fe0f}\u{200d}🔥", "🕵\u{fe0f}\u{200d}♀\u{fe0f}"]);
    let refused: Vec<String> = ["👩\u{200d}\u{200d}💻", "👩\u{200d}", "\u{200d}💻", "👩\u{200d}a", "a\u{200d}💻",
        "👩\u{fe0f}\u{fe0f}\u{200d}💻", "👩\u{200d}\u{fe0f}💻", "👩\u{fe0e}\u{200d}💻", "👩\u{200c}💻",
        "a\u{fe0f}\u{200d}💻"].iter().map(|s| (*s).to_owned()).collect();
    assert_hidden(&refused);
}

#[tokio::test]
async fn private_use_refused() {
    let refused: Vec<String> = ['\u{e000}', '\u{f8ff}', '\u{f0000}', '\u{ffffd}', '\u{100000}', '\u{10fffd}',
        // Unassigned (Cn), including noncharacters.
        '\u{0378}', '\u{fdd0}', '\u{fffe}', '\u{ffff}', '\u{2fffe}', '\u{e0fff}']
        .iter().map(|c| format!("整理{c}報表")).collect();
    assert_hidden(&refused);
    // Through the real path: no card filed, nothing queued.
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    let task = "請把\u{e000}報表寄出";
    assert_eq!(admit(home, &gate_for(task, TARGET_ID), &receipt(home, "pua", task, TARGET_ID)).await,
        Err(DelegationError::TaskNotShowable));
    assert!(cards(home).await.is_empty());
    assert!(!queued(home, "pua"));
}

/// N2: the approval binds the stable delivery fields; the resend after the
/// approval may differ only in its turn/session ids.
#[tokio::test]
async fn personal_delegation_resend_must_match_bound_fields() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    let original = FIRST.gate();
    assert_eq!(original.payload()["delivery"], json!({"target": TARGET, "reply_channel": "line:room",
        "lane": null, "on_behalf_of": "user:alice-user-id", "spawned": false, "upstream_unknown": false}));
    assert_eq!(admit(home, &original, &receipt(home, "first", FIRST.task, TARGET_ID)).await,
        Err(DelegationError::OwnerApprovalPending));
    approve(home, &original).await;

    // Each changed bound field is a different delegation: the approval does
    // not cover it (a new card is filed) and is not consumed by it.
    let changed = [
        Resend { reply: "line:other-room", ..FIRST },
        Resend { lane: Some("explore"), ..FIRST },
        Resend { owner: "user:someone-else", ..FIRST },
        Resend { spawned: true, ..FIRST },
        Resend { task: "整理客戶回覆並寄給所有人", ..FIRST },
    ];
    for (n, send) in changed.iter().enumerate() {
        let gate = send.gate();
        assert_ne!(gate.payload(), original.payload(), "case {n}");
        let id = format!("changed-{n}");
        assert_eq!(admit(home, &gate, &receipt(home, &id, send.task, TARGET_ID)).await,
            Err(DelegationError::OwnerApprovalPending), "case {n}");
        assert!(!queued(home, &id), "case {n}");
    }
    // A material whose target differs from the delegation target is refused.
    let mut wrong = FIRST.material();
    wrong["target"] = json!("someone-else");
    assert_eq!(gate(&personal(), OWNER, SOURCE, SOURCE_ID, TARGET, TARGET_ID, &wrong), Err(VerificationError::BindingMismatch));
    let mut partial = FIRST.material();
    partial.as_object_mut().unwrap().remove("reply_channel");
    assert_eq!(gate(&personal(), OWNER, SOURCE, SOURCE_ID, TARGET, TARGET_ID, &partial), Err(VerificationError::Invalid));

    // The owner denies those (they also count toward the per-source cap).
    let broker = ApprovalBroker::open(home).unwrap();
    for card in cards(home).await.into_iter().filter(|card| card.status == ApprovalStatus::Pending) {
        broker.decide(&card.id, false, &format!("dashboard:{OWNER}")).await.unwrap();
    }

    // The resend in a new turn and session, everything else the same, is
    // covered and consumes the approval exactly once.
    let resend = Resend { turn: "turn-2", session: "session-2", ..FIRST };
    assert_eq!(resend.gate().payload(), original.payload());
    assert_eq!(admit(home, &resend.gate(), &receipt(home, "resend", FIRST.task, TARGET_ID)).await, Ok(()));
    assert!(queued(home, "resend"));
    let again = Resend { turn: "turn-3", session: "session-3", ..FIRST };
    assert_eq!(admit(home, &again.gate(), &receipt(home, "again", FIRST.task, TARGET_ID)).await,
        Err(DelegationError::OwnerApprovalPending));
    assert!(!queued(home, "again"));
}

/// N3: open cards are capped per source employee (5) and per owner (10),
/// with an installation-wide backstop (100). A capped delegation files no
/// card and tells the employee why; an existing approval is still usable.
#[tokio::test]
async fn personal_delegation_cap_per_source() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    assert_eq!((MAX_PENDING_PER_SOURCE, MAX_PENDING_PER_OWNER, MAX_PENDING), (5, 10, 100));
    // An approved (not yet consumed) delegation from this source.
    let kept = gate_for("approved before the cap", TARGET_ID);
    assert_eq!(admit(home, &kept, &receipt(home, "kept", "approved before the cap", TARGET_ID)).await,
        Err(DelegationError::OwnerApprovalPending));
    approve(home, &kept).await;
    for n in 0..MAX_PENDING_PER_SOURCE {
        let task = format!("task {n}");
        assert_eq!(admit(home, &gate_for(&task, TARGET_ID), &receipt(home, &format!("s-{n}"), &task, TARGET_ID)).await,
            Err(DelegationError::OwnerApprovalPending), "{n}");
    }
    let total = cards(home).await.len();
    // The sixth open card for this source is refused; nothing is filed.
    assert_eq!(admit(home, &gate_for("one more", TARGET_ID), &receipt(home, "s-more", "one more", TARGET_ID)).await,
        Err(DelegationError::TooManyPending));
    assert_eq!(cards(home).await.len(), total);
    assert_eq!(DelegationError::TooManyPending.user_message(), Some(TOO_MANY_TEXT));
    // Retrying a delegation that already has its card is still "pending" ...
    assert_eq!(admit(home, &gate_for("task 0", TARGET_ID), &receipt(home, "s-0b", "task 0", TARGET_ID)).await,
        Err(DelegationError::OwnerApprovalPending));
    // ... and an approval granted before the cap still sends, once.
    assert_eq!(admit(home, &kept, &receipt(home, "kept-send", "approved before the cap", TARGET_ID)).await, Ok(()));

    // Per owner: a second employee of the same owner has its own five, then
    // a third one of that owner is refused at ten open cards.
    let source = |n: u8| (format!("alice-personal-{n}"), EmployeeId([10 + n; 16]));
    let other_source = |n: u8, task: &str| {
        let (agent, id) = source(n);
        gate(&personal(), OWNER, &agent, id, TARGET, TARGET_ID, &material(task)).unwrap().unwrap()
    };
    for n in 0..MAX_PENDING_PER_SOURCE {
        let task = format!("second employee {n}");
        assert_eq!(admit(home, &other_source(2, &task), &receipt_from(home, &format!("t-{n}"), source(2).1)).await,
            Err(DelegationError::OwnerApprovalPending), "{n}");
    }
    assert_eq!(admit(home, &other_source(3, "third employee"), &receipt_from(home, "u-0", source(3).1)).await,
        Err(DelegationError::TooManyPending));
    // Another owner's employee is not limited by alice's cards.
    let bob = EmployeeBinding::Personal(Text::new("bob-user-id".into()).unwrap());
    let bobs = gate(&bob, "bob-user-id", "bob-personal", EmployeeId([30; 16]), TARGET, TARGET_ID, &material("bob task"))
        .unwrap().unwrap();
    assert_eq!(admit(home, &bobs, &receipt_from(home, "bob-0", EmployeeId([30; 16]))).await,
        Err(DelegationError::OwnerApprovalPending));

    // Installation backstop: with MAX_PENDING open cards in total, a fresh
    // owner's first card is refused too.
    let broker = ApprovalBroker::open(home).unwrap();
    let open = cards(home).await.iter().filter(|card| card.status == ApprovalStatus::Pending).count();
    for n in open..MAX_PENDING {
        broker.request("filler", KIND, "filler", json!({"owner_user_id": format!("owner-{n}"),
            "source_employee": format!("{n:032x}")}), 3600).await.unwrap();
    }
    let carol = EmployeeBinding::Personal(Text::new("carol-user-id".into()).unwrap());
    let carols = gate(&carol, "carol-user-id", "carol-personal", EmployeeId([40; 16]), TARGET, TARGET_ID, &material("carol"))
        .unwrap().unwrap();
    assert_eq!(admit(home, &carols, &receipt_from(home, "carol-0", EmployeeId([40; 16]))).await,
        Err(DelegationError::TooManyPending));
}
