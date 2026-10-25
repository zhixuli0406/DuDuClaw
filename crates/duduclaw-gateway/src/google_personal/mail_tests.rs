use super::tests::Fixture;
use crate::protocol::WsFrame;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn different_google_email_reads_only_own_receipts_and_never_grants_send() {
    let f = Fixture::new();
    f.connect().await;
    let handler = crate::handlers::MethodHandler::new(f.home.clone()).await;
    handler
        .set_user_db(
            f.db.clone(),
            Arc::new(duduclaw_auth::jwt::JwtConfig::new(
                b"fixture-key-for-mail-test-only-32",
            )),
        )
        .await;
    let ctx = f.db.live_user_context(&f.alice.id).unwrap().unwrap();
    assert_ne!(ctx.email, "alice@c.test");
    let mut own_id = String::new();
    for (external, source, account) in [
        ("own", "gmail", Some("alice@c.test")),
        ("home", "gmail", None),
        ("other", "gmail", Some("bob@c.test")),
        ("drop", "dropfolder", None),
    ] {
        let incoming = crate::mail::IncomingMail {
            source: source.into(),
            external_id: external.into(),
            from: "sender@c.test".into(),
            subject: external.into(),
            body: "private body".into(),
        };
        let stored = crate::mail::ingest_inbound_for_account(
            &f.home,
            &crate::mail::MailConfig::default(),
            &crate::mail::read_mailbox(&f.home),
            &incoming,
            "bot",
            account,
        );
        let crate::mail::IngestOutcome::Stored { mail_id, .. } = stored else {
            panic!("failed fixture receipt")
        };
        if external == "own" {
            own_id = mail_id;
        }
    }
    let frame = Box::pin(handler.handle("mail.list", json!({}), &ctx)).await;
    let WsFrame::Response {
        ok: true,
        payload: Some(list),
        ..
    } = frame
    else {
        panic!("mail list refused")
    };
    assert_eq!(list["count"], 1);
    assert_eq!(list["messages"][0]["mail_id"], own_id);
    let frame = Box::pin(handler.handle(
        "mail.decide",
        json!({"mail_id":own_id,"decision":"approve"}),
        &ctx,
    ))
    .await;
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }));
}
