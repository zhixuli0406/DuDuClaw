//! Exact-turn notifications. The bounded socket sink is transport only;
//! attendance and viewer authorization remain independent host checks.
use std::{collections::HashMap, future::Future, path::{Path, PathBuf}, sync::{Mutex, OnceLock}};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub(crate) struct WatchNotice {
    pub agent_id: String,
    pub href: String,
}
pub(crate) const WEBCHAT_NOTICE_CAPACITY: usize = 8;

/// A completed model future may win select while a notice is already ready.
/// Drain this turn's bounded queue before its final assistant frame.
pub(crate) fn take_pending(receiver: &mut mpsc::Receiver<WatchNotice>) -> Vec<WatchNotice> {
    std::iter::from_fn(|| receiver.try_recv().ok()).take(WEBCHAT_NOTICE_CAPACITY).collect()
}

#[derive(Clone)]
struct SocketSink {
    home: PathBuf,
    agent: String,
    session: String,
    sender: mpsc::Sender<WatchNotice>,
}
tokio::task_local! { static SOCKET: Option<SocketSink>; }
tokio::task_local! { static HOST_TASK: Option<String>; }
tokio::task_local! { static HOST_RUN: Option<String>; }

pub(crate) fn task_scope<F: Future>(task: Option<String>, run: Option<String>, future: F) -> impl Future<Output=F::Output> {
    HOST_TASK.scope(task, HOST_RUN.scope(run, Box::pin(future)))
}
type Key = (PathBuf, String, String);
type Registration = (String, mpsc::Sender<WatchNotice>);
fn sockets() -> &'static Mutex<HashMap<Key, Vec<Registration>>> {
    static SOCKETS: OnceLock<Mutex<HashMap<Key, Vec<Registration>>>> = OnceLock::new();
    SOCKETS.get_or_init(Default::default)
}

#[derive(Debug)]
pub(crate) struct SocketGuard { key: Key, id: String }
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let mut sockets = sockets().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entries) = sockets.get_mut(&self.key) {
            entries.retain(|(id, _)| id != &self.id);
            if entries.is_empty() { sockets.remove(&self.key); }
        }
    }
}

pub(crate) fn webchat_scope<F: Future>(
    home: &Path, agent: &str, session: &str, internal: bool,
    sender: mpsc::Sender<WatchNotice>, future: F,
) -> impl Future<Output=F::Output> {
    let sink = internal.then(|| home.canonicalize().ok().map(|home| SocketSink {
        home, agent: agent.into(), session: session.into(), sender,
    })).flatten();
    SOCKET.scope(sink, Box::pin(future))
}

/// Called by the runtime's TurnGuard, not by a socket frame or model argument.
pub(crate) fn register_socket(agent: &str, turn: &str) -> Option<SocketGuard> {
    let sink = SOCKET.try_with(Clone::clone).ok().flatten()?;
    let session = duduclaw_memory::feedback::CURRENT_SESSION_ID
        .try_with(Clone::clone).ok().flatten()?;
    if sink.agent != agent || sink.session != session { return None; }
    let key = (sink.home, agent.into(), turn.into());
    let id = uuid::Uuid::new_v4().to_string();
    sockets().lock().unwrap_or_else(|p| p.into_inner())
        .entry(key.clone()).or_default().push((id.clone(), sink.sender));
    Some(SocketGuard { key, id })
}

fn socket_for(home: &Path, agent: &str, turn: &str) -> Option<mpsc::Sender<WatchNotice>> {
    sockets().lock().unwrap_or_else(|p| p.into_inner())
        .get(&(home.canonicalize().ok()?, agent.into(), turn.into()))?
        .last().map(|(_, sender)| sender.clone())
}

fn mapped_user(agent: &str, turn: &str) -> Option<String> {
    use duduclaw_knowledge::requester::RequesterIdentity;
    match crate::knowledge::turn_requesters::resolve(agent, Some(turn)) {
        RequesterIdentity::DashboardUser { user_id }
        | RequesterIdentity::ChannelUser { user_id: Some(user_id), .. } => Some(user_id),
        _ => None,
    }.filter(|user| !user.is_empty() && user != "system")
}

fn still_attended(agent: &str, turn: &str) -> bool {
    crate::computer_use_sessions::attended_call::current_turn(agent).as_deref() == Some(turn)
        && crate::computer_use_sessions::turns::attended_live_for(agent, turn)
}

/// Only the successful host start path calls this. It emits no page content,
/// never reconstructs a bot account, and never grants permission to the URL.
pub(crate) async fn notify_started(home: &Path, agent: &str, turn: Option<&str>, session: &str, remaining: u64) {
    let Some(turn) = turn.filter(|turn| still_attended(agent, turn)) else {
        record_task_start(home, agent, session, remaining).await;
        return;
    };
    let target = crate::computer_use_sessions::turns::target_for(agent, turn);
    let socket = socket_for(home, agent, turn);
    let channel = if socket.is_some() { "webchat".to_owned() } else {
        let Some(target) = &target else { return; };
        target.context.channel.clone()
    };
    let user = mapped_user(agent, turn).filter(|user|
        crate::review_evidence::audience::trusted_dashboard_principal(home, user).is_ok());
    let mapped = user.is_some();
    let base = crate::deep_link::dashboard_base_url(home);
    let Some(route) = super::notice_route(&channel, mapped, base.as_deref()) else { return; };
    let href = match route {
        super::NoticeRoute::TextOnly => None,
        super::NoticeRoute::WebChat | super::NoticeRoute::ExternalBase(_) => {
            let Ok(token) = super::issue_at(home, agent, session, chrono::Utc::now().timestamp(), remaining) else { return; };
            Some(format!("/computer/{agent}?l={token}"))
        }
    };
    if !still_attended(agent, turn) { return; }
    if href.is_some() {
        let Ok(digest) = crate::team_history::audit::session_digest(home, session) else { return; };
        if crate::team_history::audit::write(home, "computer_link_issued", user.as_deref().unwrap_or("unknown"),
            agent, serde_json::json!({"source":"attended_turn","agent":agent,"session":digest}), None).is_err() { return; }
    }
    if !still_attended(agent, turn) { return; }
    match route {
        super::NoticeRoute::WebChat => {
            if let (Some(sender), Some(href)) = (socket_for(home, agent, turn), href) {
                let _ = sender.try_send(WatchNotice { agent_id: agent.into(), href });
            }
        }
        super::NoticeRoute::ExternalBase(base) => {
            if let (Some(target), Some(href)) = (target, href) {
                let url = format!("{base}{href}");
                let link = if channel == "discord" { format!("<{url}>") } else { url };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5),
                    target.send_watch_notice(&format!("已開啟瀏覽器。觀看狀態：{link}"))).await;
            }
        }
        super::NoticeRoute::TextOnly => {
            if let Some(target) = target {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5),
                    target.send_watch_notice("已開啟瀏覽器；請登入控制台查看目前狀態。")).await;
            }
        }
    }
}

async fn record_task_start(home: &Path, agent: &str, session: &str, remaining: u64) {
    let task = HOST_TASK.try_with(Clone::clone).ok().flatten();
    let run = HOST_RUN.try_with(Clone::clone).ok().flatten();
    if task.is_none() && run.is_none() { return; }
    let Ok(token) = super::issue_at(home, agent, session, chrono::Utc::now().timestamp(), remaining) else { return; };
    let href = format!("/computer/{agent}?l={token}");
    if let Some(run) = run {
        if let Some(store) = crate::run_steps::shared_store(home) {
            let _ = store.append_computer_watch(home, agent, &run, &href);
        }
    }
    let Some(task) = task else { return; };
    if !home.join("tasks.db").is_file() { return; }
    let Ok(store) = crate::task_store::TaskStore::open(home) else { return; };
    let Ok(Some(row)) = store.get_task(&task).await else { return; };
    if row.assigned_to != agent { return; }
    let metadata = serde_json::json!({"agent_id":agent,"href":href});
    let _ = store.append_activity(&crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(), event_type:"computer_session.started".into(),
        agent_id:agent.into(), task_id:Some(task), summary:"已開啟瀏覽器".into(),
        timestamp:chrono::Utc::now().to_rfc3339(), metadata:Some(metadata.to_string()),
    }).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn verified_final_route_runtime_guard_and_socket_deliver_only_the_live_turn() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".keyfile"),[42;32]).unwrap();
        duduclaw_core::platform::set_owner_only(&home.path().join(".keyfile")).unwrap();
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = users.create_user("synthetic@test.invalid","Synthetic","isolated-test-password",
            duduclaw_auth::UserRole::Employee).unwrap();
        let (sender, mut receiver)=mpsc::channel(WEBCHAT_NOTICE_CAPACITY);
        let conversation="webchat:synthetic#conv:notice";
        let turn="watch-live-turn";
        let session="cu-0123456789abcdef0123456789abcdef";
        webchat_scope(home.path(),"alice",conversation,true,sender,
            crate::computer_use_sessions::attended::verified_webchat_message(conversation,Some(&user.id),async {
                assert!(crate::computer_use_sessions::attended::bind_final_route(conversation,conversation,"alice",turn));
                duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(conversation.into()),
                    duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn.into()),async {
                        let _requester=crate::knowledge::turn_requesters::register("alice",turn,
                            duduclaw_knowledge::requester::RequesterIdentity::DashboardUser {user_id:user.id.clone()}).unwrap();
                        let context=crate::on_behalf_of::context::register(home.path(),"alice").unwrap();
                        let guard=crate::computer_use_sessions::turns::register_current_turn("alice").unwrap();
                        let call=crate::on_behalf_of::context::attended_call(home.path(),"alice",context.reference()).unwrap();
                        crate::computer_use_sessions::attended_call::request_scope(Some(call.clone()),async {
                            notify_started(home.path(),"alice",Some("wrong-turn"),session,60).await;
                            notify_started(home.path(),"bob",Some(turn),session,60).await;
                            assert!(receiver.try_recv().is_err());
                            notify_started(home.path(),"alice",Some(turn),session,60).await;
                            let notice=receiver.try_recv().unwrap();
                            assert_eq!(notice.agent_id,"alice");
                            let token=notice.href.strip_prefix("/computer/alice?l=").unwrap();
                            assert_eq!(super::super::verify_at(home.path(),token,chrono::Utc::now().timestamp()).unwrap().session_id,session);
                        }).await;
                        drop(guard);
                        crate::computer_use_sessions::attended_call::request_scope(Some(call),
                            notify_started(home.path(),"alice",Some(turn),session,60)).await;
                        assert!(receiver.try_recv().is_err());
                    })).await;
            })).await;
    }
    #[tokio::test]
    async fn notice_enqueued_by_the_finishing_work_is_drained_before_done() {
        let (sender, mut receiver) = mpsc::channel(WEBCHAT_NOTICE_CAPACITY);
        let work = async {
            sender.try_send(WatchNotice { agent_id:"alice".into(),href:"/computer/alice?l=context.tag".into() }).unwrap();
        };
        tokio::pin!(work);
        tokio::select! { biased;
            _ = &mut work => {},
            _ = receiver.recv() => panic!("the ready work branch should win this regression"),
        }
        let notices=take_pending(&mut receiver);
        assert_eq!(notices.len(),1);
        assert_eq!(notices[0].agent_id,"alice");
        assert!(take_pending(&mut receiver).is_empty());
    }
    #[tokio::test]
    async fn background_links_require_host_task_and_matching_employee_not_a_start_label() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".keyfile"),[42;32]).unwrap();
        duduclaw_core::platform::set_owner_only(&home.path().join(".keyfile")).unwrap();
        let store = crate::task_store::TaskStore::open(home.path()).unwrap();
        let task = crate::task_store::TaskRow::new("task-host".into(),"Synthetic".into(),"".into(),
            "normal".into(),"alice".into(),"operator".into());
        store.insert_task(&task).await.unwrap();
        let session="cu-0123456789abcdef0123456789abcdef";
        record_task_start(home.path(),"alice",session,60).await;
        task_scope(Some(task.id.clone()),None,record_task_start(home.path(),"bob",session,60)).await;
        assert!(store.list_activity_for_task(&task.id,10).await.unwrap().is_empty());
        task_scope(Some(task.id.clone()),None,record_task_start(home.path(),"alice",session,60)).await;
        let rows=store.list_activity_for_task(&task.id,10).await.unwrap();
        assert_eq!(rows.len(),1);
        assert_eq!(rows[0].event_type,"computer_session.started");
        let value:serde_json::Value=serde_json::from_str(rows[0].metadata.as_ref().unwrap()).unwrap();
        let token=value["href"].as_str().unwrap().strip_prefix("/computer/alice?l=").unwrap();
        assert_eq!(super::super::verify_at(home.path(),token,chrono::Utc::now().timestamp()).unwrap().session_id,session);
    }
    #[tokio::test]
    async fn socket_routes_are_exact_and_end_with_the_runtime_guard() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        webchat_scope(home.path(), "alice", "webchat:one", true, tx,
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("webchat:one".into()), async {
                let first = register_socket("alice", "turn1").unwrap();
                assert!(register_socket("bob", "turn1").is_none());
                assert!(socket_for(other.path(), "alice", "turn1").is_none());
                assert!(socket_for(home.path(), "alice", "turn2").is_none());
                let sender = socket_for(home.path(), "alice", "turn1").unwrap();
                sender.try_send(WatchNotice { agent_id: "alice".into(), href:"/computer/alice".into() }).unwrap();
                assert!(sender.try_send(WatchNotice { agent_id:"alice".into(), href:"extra".into() }).is_err());
                assert_eq!(rx.recv().await.unwrap().agent_id, "alice");
                drop(first);
                assert!(socket_for(home.path(), "alice", "turn1").is_none());
            })).await;
    }
    #[tokio::test]
    async fn public_socket_or_another_session_cannot_register() {
        let home = tempfile::tempdir().unwrap();
        for (internal, session) in [(false,"webchat:one"),(true,"webchat:other")] {
            let (tx, _rx) = mpsc::channel(1);
            webchat_scope(home.path(), "alice", "webchat:one", internal, tx,
                duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session.into()), async {
                    assert!(register_socket("alice", "turn1").is_none());
                })).await;
        }
    }
}
