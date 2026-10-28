//! Native-only approval and heartbeat lane. No dashboard JWT authorization.
use super::{
    policy::LocalBrowserConfig,
    presence::{PresenceReport, PresenceState},
};
use crate::computer_use_orchestrator::OrchestratorControl;
use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::Engine;
use duduclaw_core::local_browser_channel as wire;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

#[derive(Clone, Serialize)]
pub(crate) struct ApprovalCard {
    pub id: String,
    pub fingerprint: String,
    pub employee: String,
    pub host: String,
    pub action: String,
    pub details: String,
    pub read_consent_seconds: u64,
    pub thumbnail_png: String,
}
struct Pending {
    card: ApprovalCard,
    expires: Instant,
    reply: oneshot::Sender<bool>,
}
struct Active {
    id: String,
    employee: String,
    control: Weak<OrchestratorControl>,
    pid: u32,
}
struct Inner {
    generation: u64,
    controller_heartbeat: Option<Instant>,
    key: Option<Zeroizing<Vec<u8>>>,
    replay: VecDeque<(String, Instant)>,
    presence: PresenceState,
    active: Option<Active>,
    pending: Option<Pending>,
    #[cfg(target_os = "macos")]
    parent: Option<NativeParent>,
}
pub(crate) struct Companion {
    home: PathBuf,
    inner: Mutex<Inner>,
    manager: OnceLock<Weak<super::super::ComputerUseSessions>>,
    start_job: Mutex<Option<StartJob>>,
}
struct StartJob {
    id: String,
    employee: String,
    result: Option<bool>,
}
fn key_fingerprint(key: &[u8], generation: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(key);
    digest.update(generation.to_be_bytes());
    format!("{:x}", digest.finalize())
}

#[cfg(target_os = "macos")]
struct NativeParent {
    pid: i32,
    birth: (u64, u64),
    path: PathBuf,
    inode: u64,
}
#[cfg(target_os = "macos")]
fn process_birth(pid: i32) -> Option<(u64, u64)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    } != size as i32
    {
        return None;
    }
    let info = unsafe { info.assume_init() };
    (info.pbi_uid == unsafe { libc::geteuid() })
        .then_some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}
fn native_parent_live(inner: &Inner) -> bool {
    // Unit-test keys are injected only into a private in-memory fixture.
    #[cfg(all(test, target_os = "macos"))]
    if inner.key.is_some() && inner.parent.is_none() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        inner.parent.as_ref().is_some_and(|parent|
            unsafe{libc::getppid()}==parent.pid && process_birth(parent.pid)==Some(parent.birth)
            && process_path(parent.pid).as_ref()==Some(&parent.path)
            && std::fs::metadata(&parent.path).is_ok_and(|meta|meta.ino()==parent.inode))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = inner;
        false
    }
}

#[cfg(target_os = "macos")]
fn process_path(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut raw = [0u8; 4096];
    if unsafe { libc::proc_pidpath(pid, raw.as_mut_ptr().cast(), raw.len() as u32) } <= 0 {
        return None;
    }
    let end = raw.iter().position(|byte| *byte == 0)?;
    std::fs::canonicalize(PathBuf::from(std::ffi::OsStr::from_bytes(&raw[..end]))).ok()
}

impl Companion {
    /// Isolated unit/native-fixture key only. Never compiled into a gateway
    /// binary: shipping pairing always requires the verified app parent.
    #[cfg(all(test, target_os = "macos"))]
    pub(in crate::computer_use_sessions) fn fixture_key(&self, key: [u8; 32]) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.key = Some(Zeroizing::new(key.to_vec()));
    }
    pub(crate) fn new(home: &Path) -> Arc<Self> {
        Arc::new(Self {
            home: home.to_path_buf(),
            inner: Mutex::new(Inner {
                generation: 0,
                controller_heartbeat: None,
                key: None,
                replay: VecDeque::new(),
                presence: PresenceState::new(),
                active: None,
                pending: None,
                #[cfg(target_os = "macos")]
                parent: None,
            }),
            manager: OnceLock::new(),
            start_job: Mutex::new(None),
        })
    }
    pub(crate) fn attach_manager(&self, manager: &Arc<super::super::ComputerUseSessions>) {
        let _ = self.manager.set(Arc::downgrade(manager));
    }
    pub(crate) fn heartbeat_received(&self) -> Option<Instant> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .controller_heartbeat
    }
    pub(super) fn canonical_home(&self) -> Option<PathBuf> {
        self.home.canonicalize().ok()
    }
    pub(super) fn admission_key_fingerprint(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !native_parent_live(&inner) || inner.presence.problem(Instant::now()).is_some() {
            return None;
        }
        Some(key_fingerprint(inner.key.as_ref()?, inner.generation))
    }
    fn start_native(
        self: &Arc<Self>,
        employee: String,
        nonce: &str,
        verified_key: &str,
    ) -> Result<Value, String> {
        if !LocalBrowserConfig::load(&self.home)?.enabled || !super::policy::platform_supported() {
            return Err("local_browser_disabled".into());
        }
        let manager = self
            .manager
            .get()
            .and_then(Weak::upgrade)
            .ok_or("native_start_manager_unavailable")?;
        if !Arc::ptr_eq(&manager.companion, self) {
            return Err("native_start_manager_mismatch".into());
        }
        let permit = super::native_start::issue(self, &employee, nonce, verified_key)?;
        let mut job = self.start_job.lock().unwrap_or_else(|p| p.into_inner());
        if job.as_ref().is_some_and(|job| job.result.is_none()) {
            return Err("native_start_busy".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        *job = Some(StartJob {
            id: id.clone(),
            employee: employee.clone(),
            result: None,
        });
        let companion = self.clone();
        let job_id = id.clone();
        // Do not abort a browser launch future on an HTTP timeout: its owned
        // cleanup must finish. The authority itself expires after 60 seconds
        // and every startup gate revalidates it before publication.
        tokio::spawn(async move {
            use futures_util::FutureExt;
            let result = std::panic::AssertUnwindSafe(manager.start_native(&employee, permit))
                .catch_unwind()
                .await;
            let ok = matches!(result, Ok(Ok(_)));
            super::audit::record(
                &companion.home,
                &employee,
                "local_native_start_result",
                json!({"actor":super::native_start::ACTOR,"job_id":job_id,"ok":ok}),
            );
            let mut job = companion
                .start_job
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(job) = job.as_mut().filter(|job| job.id == job_id) {
                job.result = Some(ok);
            }
        });
        Ok(json!({"ok":true,"accepted":true,"id":id}))
    }
    pub(crate) fn ready(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.key.is_some()
            && native_parent_live(&inner)
            && inner.presence.problem(Instant::now()).is_none()
    }
    pub(crate) fn bind(
        &self,
        id: &str,
        employee: &str,
        control: &Arc<OrchestratorControl>,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner
            .active
            .as_ref()
            .is_some_and(|a| a.control.upgrade().is_some())
        {
            return Err("local_busy".into());
        }
        if inner.key.is_none() {
            return Err("local_companion_offline".into());
        }
        inner.active = Some(Active {
            id: id.into(),
            employee: employee.into(),
            control: Arc::downgrade(control),
            pid: 0,
        });
        Ok(())
    }
    pub(crate) fn set_pid(&self, id: &str, pid: u32) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(active) = &mut inner.active {
            if active.id == id {
                active.pid = pid;
            }
        }
    }
    pub(crate) fn unbind(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.active.as_ref().is_some_and(|a| a.id == id) {
            inner.active = None;
            inner.pending = None;
        }
    }
    pub(crate) fn gate(&self, id: &str) -> Result<(), String> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !native_parent_live(&inner) {
            return Err("local_companion_offline".into());
        }
        if !inner.active.as_ref().is_some_and(|a| a.id == id) {
            return Err("local_companion_offline".into());
        }
        if let Some(problem) = inner.presence.problem(Instant::now()) {
            return Err(format!("local_presence_{problem:?}"));
        }
        Ok(())
    }
    pub(crate) fn heartbeat_lost(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        !native_parent_live(&inner) || inner.presence.must_stop(Instant::now())
    }
    pub(crate) async fn approve(&self, card: ApprovalCard) -> Result<(), String> {
        let id = card.id.clone();
        let employee = card.employee.clone();
        let fingerprint = card.fingerprint.clone();
        let (tx, rx) = oneshot::channel();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if inner.pending.is_some() {
                return Err("local_approval_busy".into());
            }
            inner.pending = Some(Pending {
                card,
                expires: Instant::now() + Duration::from_secs(60),
                reply: tx,
            });
        }
        let accepted = tokio::time::timeout(Duration::from_secs(60), rx).await;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.pending.as_ref().is_some_and(|p| p.card.id == id) {
            inner.pending = None;
        }
        super::audit::record(
            &self.home,
            &employee,
            "local_approval",
            json!({"decision":matches!(accepted,Ok(Ok(true))),"grant_fingerprint":fingerprint}),
        );
        if matches!(accepted, Ok(Ok(true))) {
            Ok(())
        } else {
            Err("local_approval_denied".into())
        }
    }

    fn handle(&self, operation: Operation) -> Result<Value, &'static str> {
        let config = LocalBrowserConfig::load(&self.home).map_err(|_| "local_browser_disabled")?;
        if !config.enabled || !super::policy::platform_supported() {
            return Err("local_browser_disabled");
        }
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !native_parent_live(&inner) {
            return Err("local_companion_offline");
        }
        let previous = inner.presence.problem(now);
        let employee = inner.active.as_ref().map(|active| active.employee.clone());
        let stopping = matches!(&operation, Operation::Stop);
        let polled = matches!(&operation, Operation::Poll { .. });
        match operation {
            Operation::Poll { presence } => {
                inner.presence.update(presence, now, config.idle_seconds);
                inner.controller_heartbeat = Some(now);
            }
            Operation::Continue => {
                inner
                    .presence
                    .resume(now, config.idle_seconds)
                    .map_err(|_| "local_presence_refused")?;
            }
            Operation::Stop => {
                inner.generation = inner.generation.wrapping_add(1);
                if let Some(control) = inner.active.as_ref().and_then(|a| a.control.upgrade()) {
                    // This remains an emergency stop, even if the owned
                    // browser exits cleanly in the same scheduling window.
                    control
                        .discard_login
                        .store(true, std::sync::atomic::Ordering::Release);
                    control
                        .stopped
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                inner.pending = None;
            }
            Operation::Start { .. } => return Err("native_start_requires_authenticated_handler"),
            Operation::Decision {
                id,
                fingerprint,
                approve,
            } => {
                let valid = inner.pending.as_ref().is_some_and(|p| {
                    p.card.id == id && p.card.fingerprint == fingerprint && now < p.expires
                });
                if !valid {
                    return Err("local_approval_stale");
                }
                // Remove first: even a repeated signed request cannot consume twice.
                let pending = inner.pending.take().ok_or("local_approval_stale")?;
                let accepted = approve && inner.presence.problem(now).is_none();
                let _ = pending.reply.send(accepted);
            }
        }
        if let Some(employee) = employee {
            let current = inner.presence.problem(now);
            if previous != current {
                super::audit::record(
                    &self.home,
                    &employee,
                    if current.is_some() {
                        "local_freeze"
                    } else {
                        "local_thaw"
                    },
                    json!({"reason":current.map(|problem|format!("{problem:?}"))}),
                );
            }
            if stopping {
                super::audit::record(
                    &self.home,
                    &employee,
                    "local_stop",
                    json!({"by":"indicator"}),
                );
            }
        }
        if inner
            .pending
            .as_ref()
            .is_some_and(|p| now >= p.expires || p.reply.is_closed())
        {
            inner.pending = None;
        }
        let start_result = self
            .start_job
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|job| {
                job.result
                    .map(|ok| json!({"id":job.id,"employee":job.employee,"ok":ok}))
            });
        let answer = json!({"ok":true,"active":inner.active.as_ref().map(|a| json!({"session_id":a.id,"employee":a.employee,"browser_pid":a.pid})),"hold":inner.presence.problem(now).map(|p|format!("{p:?}")),"approval":inner.pending.as_ref().map(|p|&p.card),"start_result":start_result});
        drop(inner);
        // Only this authenticated native handler reaches Poll in production.
        // Do not hold the companion mutex while entering the session registry.
        if polled && let Some(manager) = self.manager.get().and_then(Weak::upgrade) {
            manager.note_native_controller_heartbeat(now);
        }
        Ok(answer)
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Start {
        employee: String,
    },
    Poll {
        presence: PresenceReport,
    },
    Continue,
    Stop,
    Decision {
        id: String,
        fingerprint: String,
        approve: bool,
    },
}

pub(crate) fn router(companion: Arc<Companion>) -> Router {
    Router::new()
        .route(wire::ROUTE, post(handle))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(companion)
}

async fn handle(
    State(companion): State<Arc<Companion>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let refused = || {
        (
            StatusCode::FORBIDDEN,
            "local companion authentication required",
        )
            .into_response()
    };
    if !super::policy::platform_supported() || !peer.ip().to_canonical().is_loopback() {
        return refused();
    }
    let Some(timestamp) = headers
        .get("x-local-time")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return refused();
    };
    let Some(nonce) = headers.get("x-local-nonce").and_then(|v| v.to_str().ok()) else {
        return refused();
    };
    let Some(signature) = headers
        .get("x-local-signature")
        .and_then(|v| v.to_str().ok())
    else {
        return refused();
    };
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    let (key, verified_key) =
        {
            let mut inner = companion.inner.lock().unwrap_or_else(|p| p.into_inner());
            let Some(key) = &inner.key else {
                return refused();
            };
            if !wire::verify(key, false, timestamp, nonce, &body, signature, now) {
                return refused();
            }
            let verified_key = key_fingerprint(key, inner.generation);
            let key = key.clone();
            let at = Instant::now();
            while inner.replay.front().is_some_and(|(_, time)| {
                at.saturating_duration_since(*time) >= Duration::from_secs(65)
            }) {
                inner.replay.pop_front();
            }
            if inner.replay.len() >= 4096 || inner.replay.iter().any(|(n, _)| n == nonce) {
                return refused();
            }
            inner.replay.push_back((nonce.into(), at));
            (key, verified_key)
        };
    let answer = match serde_json::from_slice::<Operation>(&body) {
        Ok(Operation::Start { employee }) => companion
            .start_native(employee, nonce, &verified_key)
            .unwrap_or_else(|code| json!({"ok":false,"code":code})),
        Ok(operation) => companion
            .handle(operation)
            .unwrap_or_else(|code| json!({"ok":false,"code":code})),
        Err(_) => json!({"ok":false,"code":"invalid_local_request"}),
    };
    let encoded = serde_json::to_vec(&answer).unwrap_or_default();
    let Ok(proof) = wire::sign(&key, true, timestamp, nonce, &encoded) else {
        return refused();
    };
    (
        [
            ("content-type", "application/json"),
            ("x-local-signature", proof.as_str()),
        ],
        encoded,
    )
        .into_response()
}

/// Called only on a gateway spawned directly by the verified native app.
/// The nonsecret parent PID is corroborated with the actual parent process,
/// code signature and SO_PEERPID; dashboard/API authentication cannot pair.
#[cfg(target_os = "macos")]
pub(crate) async fn bootstrap(companion: Arc<Companion>) -> Result<(), String> {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    };
    if wire::AI_SESSION_ENV_VARS
        .iter()
        .any(|key| std::env::var_os(key).is_some())
    {
        return Err("AI process cannot pair a companion".into());
    }
    let claimed = std::env::var("DUDUCLAW_LOCAL_COMPANION_PARENT")
        .ok()
        .and_then(|p| p.parse::<i32>().ok())
        .filter(|p| *p > 1)
        .ok_or("native parent absent")?;
    let parent = unsafe { libc::getppid() };
    let birth = process_birth(parent).ok_or("native parent unavailable")?;
    if parent != claimed {
        return Err("native parent mismatch".into());
    }
    let mut path = [0u8; 4096];
    let len = unsafe { libc::proc_pidpath(parent, path.as_mut_ptr().cast(), path.len() as u32) };
    if len <= 0 {
        return Err("native parent unavailable".into());
    }
    let path = PathBuf::from(
        std::ffi::CStr::from_bytes_until_nul(&path)
            .map_err(|_| "native parent unavailable")?
            .to_string_lossy()
            .as_ref(),
    );
    let parent_path = path
        .canonicalize()
        .map_err(|_| "native parent unavailable")?;
    if !parent_path
        .to_string_lossy()
        .contains(".app/Contents/MacOS/")
    {
        return Err("native signed app required".into());
    }
    let verified = tokio::process::Command::new("/usr/bin/codesign").args(["--verify","--strict","-R=identifier \"ai.duduclaw.desktop\" and anchor apple generic and certificate leaf[subject.OU] = \"7469HYQ6HH\""]).arg(&parent_path).kill_on_drop(true).output();
    if !tokio::time::timeout(Duration::from_secs(5), verified)
        .await
        .map_err(|_| "native signature unavailable")?
        .map_err(|_| "native signature unavailable")?
        .status
        .success()
    {
        return Err("native signature refused".into());
    }
    let home = companion
        .home
        .canonicalize()
        .map_err(|_| "home unavailable")?;
    let directory = home.join("local_browser");
    if std::fs::symlink_metadata(&directory).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("linked companion directory refused".into());
    }
    std::fs::create_dir_all(&directory).map_err(|_| "companion directory unavailable")?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "companion permissions unavailable")?;
    let socket = directory.join(format!("companion-{}.sock", std::process::id()));
    // No unlink of a pre-existing socket: another gateway may own it.
    let listener =
        tokio::net::UnixListener::bind(&socket).map_err(|_| "companion bootstrap busy")?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "companion socket permissions unavailable")?;
    struct SocketGuard(PathBuf);
    impl Drop for SocketGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _socket_guard = SocketGuard(socket);
    let inode = std::fs::metadata(&parent_path)
        .map_err(|_| "native parent unavailable")?
        .ino();
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        let (mut peer, _) =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), listener.accept())
                .await
                .map_err(|_| "companion pairing expired")?
                .map_err(|_| "companion pairing unavailable")?;
        let credential = peer.peer_cred().map_err(|_| "companion peer unavailable")?;
        let mut pid: libc::pid_t = 0;
        let mut size = std::mem::size_of_val(&pid) as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                peer.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut size,
            )
        };
        if result != 0
            || pid != parent
            || credential.uid() != unsafe { libc::geteuid() }
            || unsafe { libc::getppid() } != parent
            || process_birth(parent) != Some(birth)
            || !std::fs::metadata(&parent_path).is_ok_and(|m| m.ino() == inode)
        {
            continue;
        }
        let mut key = Zeroizing::new(vec![0u8; 32]);
        rand::rngs::OsRng.fill_bytes(&mut key);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&*key);
        use tokio::io::AsyncWriteExt;
        tokio::time::timeout(
            Duration::from_secs(3),
            peer.write_all(format!("{encoded}\n").as_bytes()),
        )
        .await
        .map_err(|_| "companion pairing expired")?
        .map_err(|_| "companion pairing failed")?;
        let mut inner = companion.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.parent = Some(NativeParent {
            pid: parent,
            birth,
            path: parent_path,
            inode,
        });
        inner.key = Some(key);
        return Ok(());
    }
    Err("companion pairing expired".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn dashboard_jwt_and_employee_key_never_authenticate_the_native_lane() {
        let root = tempfile::tempdir().unwrap();
        let companion = Companion::new(root.path());
        for (authorization, body) in [
            "Bearer synthetic-admin-jwt",
            "Bearer synthetic-agent-key",
            "",
        ]
        .into_iter()
        .flat_map(|authorization| {
            [
                r#"{"op":"decision","id":"forged","fingerprint":"forged","approve":true}"#,
                r#"{"op":"start","employee":"alice"}"#,
            ]
            .into_iter()
            .map(move |body| (authorization, body))
        }) {
            let request = Request::builder()
                .uri(wire::ROUTE)
                .method("POST")
                .header("authorization", authorization)
                .extension(ConnectInfo(
                    "127.0.0.1:34567".parse::<std::net::SocketAddr>().unwrap(),
                ))
                .body(axum::body::Body::from(body))
                .unwrap();
            let result = router(companion.clone()).oneshot(request).await.unwrap();
            assert_eq!(result.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn signed_lane_is_single_use_and_rejects_remote_peers_and_modified_body() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("config.toml"),
            "[computer_use.local_browser]\nenabled=true\n",
        )
        .unwrap();
        let companion = Companion::new(root.path());
        let key = [7u8; 32];
        companion.inner.lock().unwrap().key = Some(Zeroizing::new(key.to_vec()));
        let nonce = "00112233445566778899aabbccddeeff";
        let body = br#"{"op":"stop"}"#;
        let now = chrono::Utc::now().timestamp() as u64;
        let signature = wire::sign(&key, false, now, nonce, body).unwrap();
        let request = |peer: &str, bytes: &'static [u8]| {
            Request::builder()
                .uri(wire::ROUTE)
                .method("POST")
                .header("x-local-time", now.to_string())
                .header("x-local-nonce", nonce)
                .header("x-local-signature", &signature)
                .extension(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()))
                .body(axum::body::Body::from(bytes))
                .unwrap()
        };
        assert_eq!(
            router(companion.clone())
                .oneshot(request("203.0.113.1:4567", body))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            router(companion.clone())
                .oneshot(request("127.0.0.1:4567", br#"{"op":"continue"}"#))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            router(companion.clone())
                .oneshot(request("127.0.0.1:4567", body))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            router(companion.clone())
                .oneshot(request("127.0.0.1:4567", body))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
