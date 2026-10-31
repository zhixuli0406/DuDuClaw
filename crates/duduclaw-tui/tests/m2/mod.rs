//! Socket coverage: compile with clippy --tests; NOT RUN where binds are refused.
use super::*;
use duduclaw_tui::{
    controls::{self, Control as Request},
    decide::Outcome,
};

#[tokio::test]
async fn control_wire_revision_conflict_rejection_and_authoritative_refetch() {
    let Some(gateway) = Gateway::start(false).await else {
        return;
    };
    let client = gateway.client().await;
    connected(&client, 1).await;
    let mut feed = StateFeed::start(client.clone(), Duration::from_secs(3600));
    model_until(&mut feed.receiver, |m| m.inbox.count().is_some()).await;
    let original = feed.receiver.borrow().employees.items[0].status.clone();
    for request in [
        Request::Employee {
            agent: "adu".into(),
            resume: false,
        },
        Request::Employee {
            agent: "adu".into(),
            resume: true,
        },
        Request::Heartbeat {
            agent: "adu".into(),
        },
        Request::Steer {
            task: "t1".into(),
            body: "先測試".into(),
            request_id: "tui-wire-steer-1".into(),
        },
        Request::Stop {
            task: "t1".into(),
            revision: 7,
        },
        Request::Responsibility {
            id: "r1".into(),
            epoch: 2,
            disable: false,
        },
        Request::Responsibility {
            id: "r1".into(),
            epoch: 2,
            disable: true,
        },
    ] {
        assert!(matches!(
            controls::send(&client, &request).await,
            Outcome::Refetch
        ));
        let (method, params) = request.request();
        assert_eq!(
            gateway.fixture.decisions.lock().unwrap().last().unwrap(),
            &(method.into(), params)
        );
        feed.refetch();
    }
    model_until(&mut feed.receiver, |m| m.local_refresh > 0).await;
    assert_eq!(
        feed.receiver.borrow().employees.items[0].status,
        original,
        "accepted is not an optimistic status change"
    );
    assert!(matches!(
        controls::send(
            &client,
            &Request::Stop {
                task: "t1".into(),
                revision: 6
            }
        )
        .await,
        Outcome::Error(_)
    ));
    assert!(matches!(
        controls::send(
            &client,
            &Request::Responsibility {
                id: "r1".into(),
                epoch: 1,
                disable: false
            }
        )
        .await,
        Outcome::Changed
    ));
    assert_eq!(
        gateway.fixture.decisions.lock().unwrap().len(),
        9,
        "no automatic retries"
    );
}
