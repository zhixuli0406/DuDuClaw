use super::*;
use std::{sync::atomic::{AtomicBool, Ordering}, time::Duration};
use duduclaw_isolation::ids::RunId;
use tokio::io::AsyncWriteExt;

struct SlowLive {
    status: Mutex<RunStatus>, slow: Arc<AtomicBool>, entered: Arc<AtomicBool>,
}
impl LiveAuthority for SlowLive {
    fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> Result<LiveSnapshot, VerificationError> {
        if self.slow.load(Ordering::Acquire) {
            self.entered.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(600));
        }
        Ok(LiveSnapshot { employee, run, control: ControlConnectionId([3;16]),
            status: self.status.lock().unwrap().clone(), principal: principal(), binding: binding(),
            policy_revision: 1, agent_slug: "alice".into() })
    }
}
fn principal() -> Principal { Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 } }
fn binding() -> EmployeeBinding { EmployeeBinding::Personal(Text::new("owner".into()).unwrap()) }
fn fixture() -> (Arc<Boundary>, CapabilityId, Connection, tokio::net::UnixStream, Arc<SlowLive>) {
    let now = now_seconds().unwrap();
    let res = Reservation { run: RunHandle(RunId([1;16])), employee: EmployeeId([2;16]),
        control: ControlConnectionId([3;16]), expires_at_unix_ms: (now+30)*1000 };
    let live = Arc::new(SlowLive { status: Mutex::new(RunStatus::Reserved { expires_at_unix_ms: res.expires_at_unix_ms }),
        slow: Arc::new(AtomicBool::new(false)), entered: Arc::new(AtomicBool::new(false)) });
    let boundary = Boundary::new(live.clone()).unwrap();
    let id = boundary.issue(&res, GrantPolicy { task: None, principal: principal(), binding: binding(),
        lane: Lane::Standard, tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(),
        policy_revision: 1, not_before: now, expires_at: now+30 }, now).unwrap();
    *live.status.lock().unwrap() = RunStatus::Running;
    let mut connection = boundary.activate_inner(id, &res, now).unwrap();
    let (host, client) = std::os::unix::net::UnixStream::pair().unwrap();
    connection.socket = Some(host);
    client.set_nonblocking(true).unwrap();
    (boundary, id, connection, tokio::net::UnixStream::from_std(client).unwrap(), live)
}
struct Wait { slow: Arc<AtomicBool> }
impl crate::server::Dispatcher for Wait {
    fn dispatch<'a>(&'a self, _: RequestContext, _: &'a str, _: serde_json::Value)
        -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<Option<serde_json::Value>,VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            self.slow.store(true, Ordering::Release);
            std::future::pending().await
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn slow_authority_does_not_starve_request_deadline_or_delay_revocation() {
    let (boundary, id, mut connection, mut client, live) = fixture();
    client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"wait\"}\n").await.unwrap();
    let dispatcher = Wait { slow: live.slow.clone() };
    let result = tokio::time::timeout(Duration::from_millis(400),
        crate::server::serve_with_deadline(&mut connection, &dispatcher, Duration::from_millis(250))).await;
    assert!(result.unwrap().is_err(), "request deadline must run while root readback blocks");
    assert!(live.entered.load(Ordering::Acquire));
    drop(connection);
    assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
}

#[tokio::test(flavor = "current_thread")]
async fn slow_authority_idle_eof_revokes_without_waiting_for_blocking_job() {
    let (boundary, id, connection, client, live) = fixture();
    live.slow.store(true, Ordering::Release);
    let dispatcher = Wait { slow: live.slow.clone() };
    let close = async {
        while !live.entered.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(5)).await; }
        drop(client);
    };
    let serve = async { crate::server::serve(connection, &dispatcher).await };
    let (_, result) = tokio::time::timeout(Duration::from_millis(400), async { tokio::join!(close, serve) }).await.unwrap();
    assert!(result.is_ok());
    assert!(matches!(boundary.entries.lock().unwrap()[&id.0].state, State::Revoked));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_verification_retains_connection_slot_until_work_finishes() {
    let (_, _, connection, _client, live) = fixture();
    live.slow.store(true, Ordering::Release);
    let verifier = connection.verifier();
    assert!(tokio::time::timeout(Duration::from_millis(50), verifier.context()).await.is_err());
    assert!(live.entered.load(Ordering::Acquire));
    assert_eq!(verifier.serial.available_permits(), 0);
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert_eq!(verifier.serial.available_permits(), 1);
}
