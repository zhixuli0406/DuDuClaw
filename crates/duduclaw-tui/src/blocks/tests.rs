use super::fetch::{Gateway, pane};
use super::*;
use crate::{
    client::RpcReply,
    controls::{Control, CronAction},
    decide::Outcome,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Mutex;

#[test]
fn layout_focus_close_and_resize_preserve_targets() {
    let mut b = Blocks::default();
    b.open(Block::Run, Some("adu".into()));
    for split in [Split::Vertical, Split::Horizontal, Split::Vertical] {
        b.add(split);
    }
    b.add(Split::Vertical);
    assert_eq!(b.panes.len(), 4);
    b.select(99);
    assert_eq!(b.focused, 3);
    b.select(0);
    for area in [Rect::new(0, 0, 140, 40), Rect::new(0, 0, 90, 32)] {
        let (regions, hidden) = b.areas(area);
        assert_eq!(regions.len(), 4);
        assert_eq!(hidden, 0);
        let sum: u32 = regions
            .iter()
            .map(|(_, r)| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert_eq!(sum, u32::from(area.width) * u32::from(area.height));
        for (i, a) in &regions {
            for (j, other) in &regions {
                if i != j {
                    assert!(a.intersection(*other).is_empty());
                }
            }
        }
    }
    for area in [Rect::new(0, 0, 30, 40), Rect::new(0, 0, 100, 10)] {
        let (regions, hidden) = b.areas(area);
        assert_eq!(regions.len(), 1);
        assert_eq!(hidden, 3);
        assert_eq!(b.panes[0].agent.as_deref(), Some("adu"));
        assert_eq!(b.panes.len(), 4);
    }
    b.close();
    assert_eq!(b.panes.len(), 3);
    b.close();
    b.close();
    b.close();
    assert_eq!(b.panes.len(), 1);
    assert_eq!(b.active, Block::Detail);
}
struct Fake {
    calls: Mutex<Vec<(String, Value)>>,
    subscribe: bool,
}
#[async_trait]
impl Gateway for Fake {
    async fn request(&self, method: &str, params: Value) -> Result<RpcReply> {
        self.calls
            .lock()
            .unwrap()
            .push((method.into(), params.clone()));
        // Copied wire fields: runs_rpc, task_json, tasks_detail_rpc,
        // responsibilities_rpc, task_changes::to_wire_json and logging.rs.
        let v = match method {
            "runs.list" => {
                json!({"runs":[{"id":"dispatch:12","agent_id":"adu","status":"completed","preview":"private-input"}]})
            }
            "runs.get" => {
                json!({"run":{"id":"dispatch:12","agent_id":"adu","status":"completed","started_at":"2026-10-31T00:00:00Z"},
                "events":[{"kind":"text","role":"user","ts":"09:00","preview":"private-input"},{"kind":"tool_step","label":"private-label","ts":"09:01","seq":1,"preview":"private-tool"}]})
            }
            "runs.subscribe" if !self.subscribe => {
                return Ok(RpcReply::Rejected {
                    message: TerminalText::new("Unknown method: runs.subscribe".into()),
                    digest_mismatch: false,
                });
            }
            "runs.subscribe" | "logs.subscribe" => json!({"subscribed":true}),
            "agents.list" => json!({"agents":[{"name":"adu","runtime":{"provider":"codex"}}]}),
            "tasks.list_page" => {
                json!({"tasks":[{"id":"t1","title":"任務","assigned_to":"adu"}],"total":1,"limit":50,"offset":0})
            }
            "tasks.timeline" => {
                json!({"task":{"id":"t1","kind":"goal","title":"任務","assigned_to":"adu","status":"needs_human","goal_mode":true,"updated_at":"rev1"},"activity":[{"type":"task.updated","timestamp":"09:00","summary":"private-tool"}],"runs":[],"criteria_ledger":null})
            }
            "tasks.iterations" => {
                json!({"iterations":[{"round":1,"verdict":"revise","dispatched_at":"09:00","judge_feedback":"private-feedback"}]})
            }
            "tasks.changes" => {
                json!({"changes":[{"path":"src/中文.rs","op":"write","tool_name":"Write","timestamp":"09:00","success":true,"snippet":"private-source"}],"distinct_paths":1,"truncated":false})
            }
            "tasks.stop_status" => json!({"stop":null,"authority_revision":7}),
            "responsibilities.list" => {
                json!({"responsibilities":[{"responsibility_id":"r1","owner_agent_id":"adu","objective":"檢查狀態","state":"active","control_epoch":2}]})
            }
            "responsibilities.get" => {
                json!({"summary":{"responsibility":{"responsibility_id":"r1","owner_agent_id":"adu","objective":"檢查狀態","state":"active","control_epoch":2},"next_due_at":"10:00","open_occurrence":{"task_id":"t1"}}})
            }
            "responsibilities.fires" => json!({"fires":[]}),
            "tasks.stop" => {
                assert_eq!(params["task_id"], "t1");
                assert_eq!(params["expected_authority_revision"], 7);
                json!({"stop":{"state":"cancel_pending"}})
            }
            "tasks.steer" => {
                assert_eq!(params["body"], "先補測試");
                assert!(!params["client_request_id"].as_str().unwrap().is_empty());
                json!({"steering":{"state":"pending"},"duplicate":false})
            }
            "responsibilities.pause" => {
                assert_eq!(params["expected_control_epoch"], 2);
                json!({"conflict":true,"current":{"control_epoch":3}})
            }
            "agents.resume" => {
                assert_eq!(params["agent_id"], "adu");
                json!({"success":true})
            }
            "cron.pause" | "cron.resume" => {
                assert_eq!(params["id"], "cron1");
                json!({"success":true,"enabled":method == "cron.resume"})
            }
            "heartbeat.trigger" => {
                assert_eq!(params["agent_id"], "adu");
                json!({"success":true})
            }
            "responsibilities.disable" => {
                assert_eq!(params["responsibility_id"], "r1");
                assert_eq!(params["expected_control_epoch"], 2);
                json!({"responsibility":{"state":"disabled","control_epoch":3}})
            }
            "agents.pause" => {
                return Ok(RpcReply::Rejected {
                    message: TerminalText::new("permission denied\u{1b}".into()),
                    digest_mismatch: false,
                });
            }
            "cron.run_now" => {
                assert!(!params["request_id"].as_str().unwrap().is_empty());
                json!({"success":true,"run_status":"pending"})
            }
            _ => return Err(Error::Protocol),
        };
        Ok(RpcReply::Accepted(v))
    }
}
#[tokio::test]
async fn real_shapes_parse_history_tasks_responsibilities_and_subscription_fallback() {
    for subscribe in [true, false] {
        let g = Fake {
            calls: Mutex::new(vec![]),
            subscribe,
        };
        let mut p = Pane::new(Block::Run);
        p.agent = Some("adu".into());
        let data = pane(&g, &p, true).await.unwrap();
        assert_eq!(data.subscribed, subscribe);
        assert_eq!(data.section("events").as_array().unwrap().len(), 2);
        assert!(data.runtime.unwrap().is("codex"));
        p.kind = Block::Task;
        let data = pane(&g, &p, true).await.unwrap();
        assert_eq!(data.section("stop")["authority_revision"], 7);
        assert_eq!(data.task().unwrap().data()["updated_at"], "rev1");
        p.responsibilities = true;
        let data = pane(&g, &p, true).await.unwrap();
        assert_eq!(data.selected["control_epoch"], 2);
        assert_eq!(data.section("summary")["summary"]["next_due_at"], "10:00");
        assert!(
            g.calls
                .lock()
                .unwrap()
                .iter()
                .any(|(m, _)| m == "tasks.changes")
        );
    }
}
#[tokio::test]
async fn log_subscription_is_role_gated() {
    let g = Fake {
        calls: Mutex::new(vec![]),
        subscribe: true,
    };
    let p = Pane::new(Block::Log);
    assert!(!pane(&g, &p, false).await.unwrap().subscribed);
    assert!(g.calls.lock().unwrap().is_empty());
    assert!(pane(&g, &p, true).await.unwrap().subscribed);
}
#[tokio::test]
async fn fake_gateway_actions_echo_revisions_conflict_and_rejection_refetch_only() {
    let g = Fake {
        calls: Mutex::new(vec![]),
        subscribe: false,
    };
    let stop = Control::Stop {
        task: "t1".into(),
        revision: 7,
    };
    assert!(matches!(
        crate::controls::send(&g, &stop).await,
        Outcome::Refetch
    ));
    let steer = Control::Steer {
        task: "t1".into(),
        body: "先補測試".into(),
        request_id: crate::controls::request_id(),
    };
    assert!(matches!(
        crate::controls::send(&g, &steer).await,
        Outcome::Refetch
    ));
    let pause = Control::Responsibility {
        id: "r1".into(),
        epoch: 2,
        disable: false,
    };
    assert!(matches!(
        crate::controls::send(&g, &pause).await,
        Outcome::Changed
    ));
    let employee = Control::Employee {
        agent: "adu".into(),
        resume: false,
    };
    let outcome = crate::controls::send(&g, &employee).await;
    assert!(matches!(outcome, Outcome::Error(_)));
    assert!(!outcome.notice().contains('\u{1b}'));
    let cron = Control::Cron {
        id: "cron1".into(),
        action: CronAction::Run,
        request_id: crate::controls::request_id(),
    };
    assert!(matches!(
        crate::controls::send(&g, &cron).await,
        Outcome::Refetch
    ));
    assert_eq!(g.calls.lock().unwrap().len(), 5, "mutations never retry");
}
#[test]
fn buffers_and_hostile_strings_are_bounded_and_safe() {
    let mut tail = LogTail::default();
    for _ in 0..500 {
        tail.push(&json!({"message":"a".repeat(5000)+"\u{1b}]9;injection\u{7}\u{202e}"}));
    }
    assert_eq!(tail.lines.len(), 256);
    assert!(tail.lost);
    assert!(
        tail.lines
            .iter()
            .all(|line| line.plain().chars().count() <= 2048)
    );
    let v = json!({"path":"\u{1b}]8;;bad\u{7}中文\u{202e}"});
    assert!(!field(&v, "path").chars().any(crate::sanitize::forbidden));
    assert!(array(&json!({"tasks":vec![json!({});2001]}), "tasks").is_err());
}
#[test]
fn live_steps_require_subscription_and_exact_run_then_bound_and_deduplicate() {
    let mut b = Blocks::default();
    b.open(Block::Run, Some("adu".into()));
    let d = &mut b.panes[0].data;
    d.stale = false;
    d.selected = json!({"id":"r1"});
    d.sections.push(("events", json!([])));
    let event =
        json!({"run_id":"r1","agent_id":"adu","seq":1,"kind":"tool_step","preview":"secret"});
    b.run_step(&event);
    assert_eq!(b.panes[0].data.section("events"), &json!([]));
    b.panes[0].data.subscribed = true;
    b.run_step(&json!({"run_id":"wrong","agent_id":"adu","kind":"text","preview":"bad"}));
    b.run_step(&event);
    b.run_step(&event);
    assert_eq!(
        b.panes[0].data.section("events").as_array().unwrap().len(),
        1
    );
    for seq in 2..400 {
        b.run_step(&json!({"run_id":"r1","agent_id":"adu","seq":seq,"kind":"text","preview":"x".repeat(5000)}));
    }
    let events = b.panes[0].data.section("events").as_array().unwrap();
    assert_eq!(events.len(), 256);
    assert!(events.last().unwrap()["preview"].as_str().unwrap().len() <= 2048);
    b.invalidate();
    b.run_step(&event);
    assert_eq!(
        b.panes[0].data.section("events").as_array().unwrap().len(),
        256
    );
}
#[test]
fn block_event_cursor_detects_loss_regression_and_boot_change_not_duplicates() {
    let mut c = EventCursor::default();
    let e = |seq, boot: &str| crate::client::ServerEvent {
        name: "run.step".into(),
        payload: json!({}),
        seq: Some(seq),
        boot_id: Some(boot.into()),
    };
    assert!(!c.observe(&e(1, "a")));
    assert!(!c.observe(&e(1, "a")));
    assert!(c.observe(&e(3, "a")));
    assert!(c.observe(&e(2, "a")));
    assert!(c.observe(&e(1, "b")));
    assert!(!c.observe(&e(2, "b")));
}

#[tokio::test]
async fn remaining_controls_use_real_rpc_shapes_without_retries() {
    let g = Fake {
        calls: Mutex::new(vec![]),
        subscribe: false,
    };
    for control in [
        Control::Employee {
            agent: "adu".into(),
            resume: true,
        },
        Control::Cron {
            id: "cron1".into(),
            action: CronAction::Pause,
            request_id: crate::controls::request_id(),
        },
        Control::Cron {
            id: "cron1".into(),
            action: CronAction::Resume,
            request_id: crate::controls::request_id(),
        },
        Control::Heartbeat {
            agent: "adu".into(),
        },
        Control::Responsibility {
            id: "r1".into(),
            epoch: 2,
            disable: true,
        },
    ] {
        assert!(matches!(
            crate::controls::send(&g, &control).await,
            Outcome::Refetch
        ));
    }
    assert_eq!(g.calls.lock().unwrap().len(), 5);
}
