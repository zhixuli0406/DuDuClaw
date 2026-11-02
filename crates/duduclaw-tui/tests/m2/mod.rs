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

#[tokio::test]
async fn autonomy_key_wire_status_refetch_reconnect_and_refusal() {
    use duduclaw_tui::ui::app::{Action, App, Selection};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let Some(gateway) = Gateway::start(false).await else { return; };
    let client = gateway.client().await;
    connected(&client, 1).await;
    let mut feed = StateFeed::start(client.clone(), Duration::from_secs(3600));
    model_until(&mut feed.receiver, |m| m.autonomy.count() == Some(1) && m.employees.count().is_some()).await;
    let mut app = App::default(); app.selected = Some(Selection::Employee("adu".into()));
    for (key, method, paused) in [('p',"autonomy.pause",true),('u',"autonomy.resume",false)] {
        let before = feed.receiver.borrow().clone();
        let Action::Control(control) = app.key(KeyEvent::new(KeyCode::Char(key),KeyModifiers::NONE), &before) else { panic!("immediate action"); };
        assert_eq!(control.request(), (method,json!({"agent_id":"adu"})));
        assert!(matches!(controls::send(&client, &control).await, Outcome::Notice(_)));
        feed.refetch();
        model_until(&mut feed.receiver, |m| m.autonomy.count() == Some(1) && m.autonomy.paused("adu") == paused).await;
    }
    for (name,seq) in [("autonomy.changed",1),("events_lost",2)] {
        let before = gateway.fixture.autonomy_calls.load(Ordering::SeqCst);
        gateway.push(name,seq,"boot-a");
        timeout(Duration::from_secs(3), async {
            while gateway.fixture.autonomy_calls.load(Ordering::SeqCst) <= before { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        model_until(&mut feed.receiver, |m| m.autonomy.count() == Some(1)).await;
    }
    let before = gateway.fixture.autonomy_calls.load(Ordering::SeqCst);
    gateway.fixture.events.send(Control::Close).unwrap();
    connected(&client,2).await;
    timeout(Duration::from_secs(3), async {
        while gateway.fixture.autonomy_calls.load(Ordering::SeqCst) <= before { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.unwrap();
    gateway.fixture.autonomy_refused.store(true,Ordering::SeqCst);
    let original = feed.receiver.borrow().autonomy.paused("adu");
    let outcome = controls::send(&client,&Request::Autonomy { agent:Some("adu".into()),resume:false }).await;
    assert!(matches!(&outcome,Outcome::Error(_)));
    app.decision_finished(outcome);
    assert!(app.notice.as_ref().unwrap().contains("權限不足"));
    assert_eq!(feed.receiver.borrow().autonomy.paused("adu"),original);
    assert!(matches!(controls::send(&client,&Request::Autonomy { agent:None,resume:false }).await,Outcome::Error(_)));
}
