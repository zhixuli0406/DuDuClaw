//! Unit tests for the tool-driven computer-use sessions (design §5), with a
//! fake container backend, plus the real-Docker `#[ignore]` tests at the end.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::http::HeaderMap;
use serde_json::{Value, json};

use super::actions::{self, ActionRequest, Gate};
use super::auth::{self, AuthFailure, RateLimiter, ReplayGuard};
use super::backend::{BackendFactory, SessionBackend};
use super::sweep;
use super::*;
use crate::channel_sender::{ChannelSendError, ChannelSender};
use crate::computer_use::{ComputerAction, ComputerUseError};
use crate::computer_use_orchestrator::{
    ComputerUseConfig, FullMaskReason, MaskedScreenshot, OrchestratorControl, ThreatLevel,
};
use crate::risk_detector::RiskLevel;

const INTERNAL_KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";
const EXTERNAL_KEY: &str = "ddc_prod_fedcba9876543210fedcba9876543210";

// ── fixtures ─────────────────────────────────────────────────────────────

/// A temp home with both MCP keys, an identity key and two employees
/// (`alice`, `bob`) that have computer use on.
fn home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), "");
    duduclaw_core::ensure_identity_key(tmp.path()).unwrap();
    for agent in ["alice", "bob"] {
        write_agent(tmp.path(), agent, "[capabilities]\ncomputer_use = true\n");
    }
    tmp
}

fn write_config(home: &std::path::Path, extra: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        home.join("config.toml"),
        format!(
            "{extra}\n[mcp_keys.\"{INTERNAL_KEY}\"]\nclient_id = \"gateway-internal\"\nis_external = false\n\
             created_at = \"{now}\"\nscopes = [\"admin\"]\n\n\
             [mcp_keys.\"{EXTERNAL_KEY}\"]\nclient_id = \"claude-desktop\"\nis_external = true\n\
             created_at = \"{now}\"\nscopes = [\"admin\"]\n"
        ),
    )
    .unwrap();
}

fn write_agent(home: &std::path::Path, agent: &str, body: &str) {
    let dir = home.join("agents").join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), body).unwrap();
}

fn token(home: &std::path::Path, agent: &str) -> String {
    let key = duduclaw_core::load_identity_key(home).unwrap();
    duduclaw_core::mint_identity_token(&key, agent)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn fresh_nonce() -> String {
    uuid::Uuid::new_v4().as_simple().to_string()
}

/// Signed headers as the MCP client builds them. `key` signs; `tok`
/// overrides the agent token; `ts` overrides the timestamp.
fn signed(
    home: &std::path::Path,
    key: &str,
    agent: &str,
    tok: Option<String>,
    ts: Option<u64>,
    nonce: &str,
    body: &[u8],
) -> HeaderMap {
    let tok = tok.unwrap_or_else(|| token(home, agent));
    let ts = ts.unwrap_or_else(unix_now).to_string();
    let sig =
        duduclaw_core::internal_request_signature(key.as_bytes(), agent, &tok, &ts, nonce, body);
    let mut h = HeaderMap::new();
    h.insert(auth::AGENT_ID_HEADER, agent.parse().unwrap());
    h.insert(auth::TIMESTAMP_HEADER, ts.parse().unwrap());
    h.insert(auth::NONCE_HEADER, nonce.parse().unwrap());
    h.insert(auth::SIGNATURE_HEADER, sig.parse().unwrap());
    h
}

/// Correctly signed headers for `agent` over `body`.
fn headers(home: &std::path::Path, agent: &str, body: &[u8]) -> HeaderMap {
    signed(home, INTERNAL_KEY, agent, None, None, &fresh_nonce(), body)
}

fn loopback() -> SocketAddr {
    "127.0.0.1:50000".parse().unwrap()
}

#[derive(Default)]
struct FakeState {
    executed: Mutex<Vec<ComputerAction>>,
    stops: AtomicU32,
    starts: AtomicU32,
    title: Mutex<Option<Result<String, String>>>,
    fail_start: AtomicBool,
    /// How long `stop` takes, in milliseconds.
    stop_delay_ms: std::sync::atomic::AtomicU64,
    /// URLs handed to `navigate`.
    navigated: Mutex<Vec<String>>,
    /// Make `navigate` report a browser error.
    nav_fail: AtomicBool,
    /// The config the factory last built a backend with.
    last_config: Mutex<Option<ComputerUseConfig>>,
    /// What `screenshot` reports as the full-mask reason.
    full_mask: Mutex<Option<FullMaskReason>>,
    frame: Mutex<Option<String>>,
}

struct FakeBackend {
    state: Arc<FakeState>,
    control: Arc<OrchestratorControl>,
}

#[async_trait]
impl SessionBackend for FakeBackend {
    async fn start(&mut self) -> Result<(), ComputerUseError> {
        if self.state.fail_start.load(Ordering::SeqCst) {
            return Err(ComputerUseError::ApiError(
                "docker said: secret /path".into(),
            ));
        }
        self.state.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn register(&mut self, _session_id: &str) -> Result<(), ComputerUseError> {
        Ok(())
    }
    async fn stop(&mut self) {
        let delay = self.state.stop_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        self.state.stops.fetch_add(1, Ordering::SeqCst);
    }
    async fn screenshot(&self) -> Result<MaskedScreenshot, ComputerUseError> {
        Ok(MaskedScreenshot {
            png_base64: self.state.frame.lock().unwrap().clone().unwrap_or_else(tiny_png_b64),
            full_mask: *self.state.full_mask.lock().unwrap(),
        })
    }
    async fn window_title(&self) -> Result<String, String> {
        self.state
            .title
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok("about:blank - Chromium".to_string()))
    }
    async fn execute(&self, action: &ComputerAction) -> Result<(), ComputerUseError> {
        self.state.executed.lock().unwrap().push(action.clone());
        Ok(())
    }
    async fn navigate(
        &self,
        url: &str,
    ) -> Result<crate::computer_use_orchestrator::NavigateOutcome, ComputerUseError> {
        self.state.navigated.lock().unwrap().push(url.to_string());
        let failed = self.state.nav_fail.load(Ordering::SeqCst);
        Ok(crate::computer_use_orchestrator::NavigateOutcome {
            ok: !failed,
            host: (!failed).then(|| "example.com".to_string()),
            error: failed.then(|| "net::ERR_CONNECTION_REFUSED".to_string()),
        })
    }
    fn control(&self) -> Arc<OrchestratorControl> {
        Arc::clone(&self.control)
    }
}

/// Resolves `example.com` and `docs.example.com`; everything else fails.
struct FakeResolver;

#[async_trait]
impl navigation::HostResolver for FakeResolver {
    async fn resolve(&self, host: &str) -> Option<std::net::Ipv4Addr> {
        match host {
            "example.com" => Some(std::net::Ipv4Addr::new(93, 184, 215, 14)),
            "docs.example.com" => Some(std::net::Ipv4Addr::new(93, 184, 215, 15)),
            _ => None,
        }
    }
}

fn tiny_png_b64() -> String {
    use base64::Engine;
    use image::ImageEncoder;
    let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([255, 255, 255, 255]));
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(img.as_raw(), 4, 3, image::ExtendedColorType::Rgba8)
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode(&buf)
}

fn fake_factory(state: Arc<FakeState>) -> BackendFactory {
    Arc::new(
        move |_agent: &str, _home: &std::path::Path, cfg: ComputerUseConfig| {
            *state.last_config.lock().unwrap() = Some(cfg);
            Box::new(FakeBackend {
                state: Arc::clone(&state),
                control: Arc::new(OrchestratorControl::new()),
            }) as Box<dyn SessionBackend>
        },
    )
}

/// Confirmation targets for tests: turn `yes` answers yes, `no` answers no,
/// `slow` never answers within the test; anything else has no live turn.
struct FakeConfirmers(Arc<AtomicU32>, PathBuf);

#[async_trait]
impl ConfirmerResolver for FakeConfirmers {
    async fn context(
        &self,
        _agent_id: &str,
        turn_id: &str,
    ) -> Option<crate::approval::DecisionContext> {
        matches!(turn_id, "yes" | "no" | "slow").then(|| crate::approval::DecisionContext {
            channel: "line".into(),
            account_id: "test-bot".into(),
            conversation_id: "human".into(),
            principal_id: "human".into(),
        })
    }
    async fn resolve(&self, _agent_id: &str, turn_id: &str) -> Option<Box<dyn ChannelSender>> {
        let answer = match turn_id {
            "yes" => Some(true),
            "no" => Some(false),
            "slow" => None,
            _ => return None,
        };
        Some(Box::new(FakeConfirmer(
            answer,
            Arc::clone(&self.0),
            self.1.clone(),
        )))
    }
}

fn manager(home: &std::path::Path, state: &Arc<FakeState>, idle: Duration) -> ComputerUseSessions {
    manager_asking(home, state, idle, Arc::new(AtomicU32::new(0)))
}

fn manager_asking(
    home: &std::path::Path,
    state: &Arc<FakeState>,
    idle: Duration,
    asked: Arc<AtomicU32>,
) -> ComputerUseSessions {
    let mut mgr =
        ComputerUseSessions::with_parts(home.to_path_buf(), fake_factory(Arc::clone(state)), idle)
            .with_confirmers(Arc::new(FakeConfirmers(asked, home.to_path_buf())))
            .with_resolver(Arc::new(FakeResolver));
    mgr.approval_ttl_secs = 5;
    mgr.approval_poll = Duration::from_millis(20);
    mgr
}

/// Insert a ready session for `agent` without going through `start`.
fn insert_session(
    mgr: &ComputerUseSessions,
    state: &Arc<FakeState>,
    agent: &str,
    config: ComputerUseConfig,
) -> String {
    let now = Instant::now();
    let id = format!("cu-{}", uuid::Uuid::new_v4().as_simple());
    mgr.insert(ManagedSession {
        session_id: id.clone(),
        agent_id: agent.to_string(),
        backend: Box::new(FakeBackend {
            state: Arc::clone(state),
            control: Arc::new(OrchestratorControl::new()),
        }),
        config,
        started: now,
        deadline: now + Duration::from_secs(600),
        actions_used: 0,
        last_activity: now,
        shared: Arc::new(SessionShared::default()),
        ended: false,
        nav_hosts: Vec::new(),
        nav_configured: false,
    });
    id
}

/// A confirmer answering `Some(answer)` at once, or never (`None`).
struct FakeConfirmer(Option<bool>, Arc<AtomicU32>, PathBuf);

#[async_trait]
impl ChannelSender for FakeConfirmer {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        if let Some(answer) = self.0 {
            let id = text
                .lines()
                .find_map(|l| l.strip_prefix("同意：確認 "))
                .unwrap();
            let ctx = crate::approval::DecisionContext {
                channel: "line".into(),
                account_id: "test-bot".into(),
                conversation_id: "human".into(),
                principal_id: "human".into(),
            };
            let command = format!("{} {id}", if answer { "確認" } else { "取消" });
            crate::decision_notify::route_bound_text(&self.2, &ctx, &command)
                .await
                .unwrap()
                .unwrap();
        }
        Ok(())
    }
    async fn send_photo(&self, _png: &[u8], _caption: &str) -> Result<(), ChannelSendError> {
        Ok(())
    }
    async fn request_confirmation(
        &self,
        _prompt: &str,
        _screenshot: Option<&[u8]>,
        _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        match self.0 {
            Some(answer) => Ok(answer),
            None => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(true)
            }
        }
    }
    fn channel_type(&self) -> &'static str {
        "fake"
    }
}

fn click(x: i64, y: i64) -> ActionRequest {
    ActionRequest::Click {
        x,
        y,
        button: None,
        double: None,
    }
}

fn typed(text: &str) -> ActionRequest {
    ActionRequest::Type {
        text: text.to_string(),
    }
}

// ── authentication ───────────────────────────────────────────────────────

const BODY: &[u8] = br#"{"op":"status"}"#;

#[test]
fn authentication_accepts_a_correct_signature_from_loopback() {
    let tmp = home();
    let h = tmp.path();
    let now = unix_now();
    let ok = auth::authenticate(h, loopback(), &headers(h, "alice", BODY), BODY, now).unwrap();
    assert_eq!(ok.agent_id, "alice");
    assert_eq!(ok.nonce.len(), 32);
    // IPv4-mapped IPv6 loopback is loopback too.
    let mapped: SocketAddr = "[::ffff:127.0.0.1]:5".parse().unwrap();
    assert!(auth::authenticate(h, mapped, &headers(h, "alice", BODY), BODY, now).is_ok());
    // A clock difference inside the window is accepted.
    let skewed = signed(
        h,
        INTERNAL_KEY,
        "alice",
        None,
        Some(now - 59),
        &fresh_nonce(),
        BODY,
    );
    assert!(auth::authenticate(h, loopback(), &skewed, BODY, now).is_ok());
}

#[test]
fn authentication_refuses_each_check_on_its_own() {
    let tmp = home();
    let h = tmp.path();
    let now = unix_now();
    let lan: SocketAddr = "192.168.1.20:5".parse().unwrap();
    let nonce = fresh_nonce();
    let mut missing = headers(h, "alice", BODY);
    missing.remove(auth::SIGNATURE_HEADER);
    let mut bad_nonce = headers(h, "alice", BODY);
    bad_nonce.insert(auth::NONCE_HEADER, "NOT-HEX".parse().unwrap());
    let cases: Vec<(SocketAddr, HeaderMap, &[u8], AuthFailure)> = vec![
        // A non-loopback peer with a perfect signature.
        (
            lan,
            headers(h, "alice", BODY),
            BODY,
            AuthFailure::NotLoopback,
        ),
        (loopback(), missing, BODY, AuthFailure::MissingHeaders),
        (
            loopback(),
            HeaderMap::new(),
            BODY,
            AuthFailure::MissingHeaders,
        ),
        (loopback(), bad_nonce, BODY, AuthFailure::MalformedHeaders),
        // Signed with another client's valid key / a made-up key.
        (
            loopback(),
            signed(h, EXTERNAL_KEY, "alice", None, None, &nonce, BODY),
            BODY,
            AuthFailure::BadSignature,
        ),
        (
            loopback(),
            signed(
                h,
                "ddc_prod_00000000000000000000000000000000",
                "alice",
                None,
                None,
                &nonce,
                BODY,
            ),
            BODY,
            AuthFailure::BadSignature,
        ),
        // Someone else's identity token, or a guessed one.
        (
            loopback(),
            signed(
                h,
                INTERNAL_KEY,
                "alice",
                Some(token(h, "bob")),
                None,
                &nonce,
                BODY,
            ),
            BODY,
            AuthFailure::BadSignature,
        ),
        (
            loopback(),
            signed(
                h,
                INTERNAL_KEY,
                "alice",
                Some("00".into()),
                None,
                &nonce,
                BODY,
            ),
            BODY,
            AuthFailure::BadSignature,
        ),
        // The body is covered: a different body fails.
        (
            loopback(),
            headers(h, "alice", BODY),
            br#"{"op":"stop"}"#,
            AuthFailure::BadSignature,
        ),
        // Too old / too far ahead.
        (
            loopback(),
            signed(h, INTERNAL_KEY, "alice", None, Some(now - 61), &nonce, BODY),
            BODY,
            AuthFailure::StaleTimestamp,
        ),
        (
            loopback(),
            signed(h, INTERNAL_KEY, "alice", None, Some(now + 61), &nonce, BODY),
            BODY,
            AuthFailure::StaleTimestamp,
        ),
    ];
    for (i, (peer, hdrs, body, want)) in cases.into_iter().enumerate() {
        assert_eq!(
            auth::authenticate(h, peer, &hdrs, body, now),
            Err(want),
            "case {i}"
        );
    }
    // A bad agent id is refused before any use.
    let mut bad_id = headers(h, "alice", BODY);
    bad_id.insert(auth::AGENT_ID_HEADER, "../x".parse().unwrap());
    assert_eq!(
        auth::authenticate(h, loopback(), &bad_id, BODY, now),
        Err(AuthFailure::InvalidAgentId)
    );
    // A forwarded header never turns a LAN peer into a local one.
    let mut fwd = headers(h, "alice", BODY);
    fwd.insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
    assert_eq!(
        auth::authenticate(h, lan, &fwd, BODY, now),
        Err(AuthFailure::NotLoopback)
    );
}

#[test]
fn authentication_fails_closed_without_an_identity_key_or_an_internal_key() {
    let tmp = home();
    let h = tmp.path();
    let good = headers(h, "alice", BODY);
    std::fs::remove_file(duduclaw_core::identity_key_path(h)).unwrap();
    assert_eq!(
        auth::authenticate(h, loopback(), &good, BODY, unix_now()),
        Err(AuthFailure::NoIdentityKey)
    );
    let tmp = home();
    let h = tmp.path();
    let good = headers(h, "alice", BODY);
    std::fs::write(h.join("config.toml"), "").unwrap();
    assert_eq!(
        auth::authenticate(h, loopback(), &good, BODY, unix_now()),
        Err(AuthFailure::BadSignature)
    );
}

#[test]
fn a_nonce_is_accepted_once_within_the_window() {
    let guard = ReplayGuard::default();
    let t0 = Instant::now();
    assert!(guard.record("aa", t0));
    assert!(
        !guard.record("aa", t0 + Duration::from_secs(1)),
        "replay refused"
    );
    assert!(guard.record("bb", t0));
    assert!(
        guard.record("aa", t0 + auth::NONCE_WINDOW),
        "forgotten after the window"
    );
}

#[test]
fn rate_limit_is_per_employee_and_slides() {
    let limiter = RateLimiter::default();
    let t0 = Instant::now();
    for _ in 0..auth::RATE_LIMIT_PER_MINUTE {
        assert!(limiter.allow("alice", t0));
    }
    assert!(!limiter.allow("alice", t0));
    assert!(
        limiter.allow("bob", t0),
        "another employee has their own window"
    );
    assert!(limiter.allow("alice", t0 + Duration::from_secs(61)));
}

// ── lifecycle decisions ─────────────────────────────────────────────────

#[test]
fn end_reason_orders_stop_threat_deadline_idle() {
    let t = Instant::now();
    let later = t + Duration::from_secs(1000);
    let idle = Duration::from_secs(120);
    assert_eq!(
        end_reason(t, later, t, idle, false, ThreatLevel::Green),
        None
    );
    assert_eq!(
        end_reason(t, later, t, idle, true, ThreatLevel::Red),
        Some(EndReason::Stopped)
    );
    assert_eq!(
        end_reason(t, later, t, idle, false, ThreatLevel::Red),
        Some(EndReason::ThreatRed)
    );
    assert_eq!(
        end_reason(later, later, later, idle, false, ThreatLevel::Yellow),
        Some(EndReason::Deadline)
    );
    assert_eq!(
        end_reason(
            t + Duration::from_secs(121),
            later,
            t,
            idle,
            false,
            ThreatLevel::Green
        ),
        Some(EndReason::Idle)
    );
    assert_eq!(
        end_reason(
            t + Duration::from_secs(119),
            later,
            t,
            idle,
            false,
            ThreatLevel::Green
        ),
        None
    );
}

// ── parameter validation ────────────────────────────────────────────────

#[test]
fn action_parameters_are_validated() {
    let ok = |r: &ActionRequest| actions::to_action(r, 1280, 800);
    assert_eq!(
        ok(&click(0, 0)).unwrap(),
        ComputerAction::LeftClick { coordinate: [0, 0] }
    );
    assert_eq!(
        ok(&ActionRequest::Click {
            x: 5,
            y: 6,
            button: Some("left".into()),
            double: Some(true)
        })
        .unwrap(),
        ComputerAction::DoubleClick { coordinate: [5, 6] }
    );
    assert_eq!(
        ok(&ActionRequest::Click {
            x: 5,
            y: 6,
            button: Some("right".into()),
            double: None
        })
        .unwrap(),
        ComputerAction::RightClick { coordinate: [5, 6] }
    );
    for bad in [
        click(1280, 0),
        click(0, 800),
        click(-1, 5),
        ActionRequest::Click {
            x: 1,
            y: 1,
            button: Some("middle".into()),
            double: None,
        },
        typed(""),
        typed(&"字".repeat(actions::MAX_TYPE_CHARS + 1)),
        ActionRequest::Key {
            key: "--window 0 ctrl+c".into(),
        },
        ActionRequest::Key { key: "".into() },
        ActionRequest::Scroll {
            x: 1,
            y: 1,
            direction: Some("left".into()),
            amount: None,
        },
        ActionRequest::Scroll {
            x: 1,
            y: 1,
            direction: None,
            amount: Some(0),
        },
        ActionRequest::Scroll {
            x: 1,
            y: 1,
            direction: None,
            amount: Some(21),
        },
    ] {
        let err = ok(&bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidAction, "{bad:?}");
    }
    // 2,000 CJK characters are fine (characters, not bytes).
    assert!(ok(&typed(&"字".repeat(actions::MAX_TYPE_CHARS))).is_ok());
    assert_eq!(
        ok(&ActionRequest::Scroll {
            x: 1,
            y: 1,
            direction: None,
            amount: None
        })
        .unwrap(),
        ComputerAction::Scroll {
            coordinate: [1, 1],
            direction: "down".into(),
            amount: 3
        }
    );
    // The invalid key is refused, never replaced by Escape.
    assert!(
        ok(&ActionRequest::Key {
            key: "ctrl+s".into()
        })
        .is_ok()
    );
}

#[test]
fn risk_gate_covers_execute_confirm_and_refuse() {
    assert_eq!(
        actions::risk_gate(RiskLevel::Low, false, false, false),
        Gate::Execute
    );
    assert_eq!(
        actions::risk_gate(RiskLevel::Medium, false, false, false),
        Gate::Execute
    );
    assert_eq!(
        actions::risk_gate(RiskLevel::High, false, true, false),
        Gate::Execute
    );
    assert_eq!(
        actions::risk_gate(RiskLevel::High, false, false, true),
        Gate::Confirm
    );
    let refused = |g: Gate| match g {
        Gate::Refuse(e) => e.code,
        other => panic!("expected refusal, got {other:?}"),
    };
    assert_eq!(
        refused(actions::risk_gate(RiskLevel::High, false, false, false)),
        ErrorCode::ConfirmationRequired
    );
    assert_eq!(
        refused(actions::risk_gate(RiskLevel::Blocked, false, true, true)),
        ErrorCode::Blocked
    );
    assert_eq!(
        refused(actions::risk_gate(RiskLevel::Low, true, true, true)),
        ErrorCode::Blocked
    );
}

// ── sweep decisions ─────────────────────────────────────────────────────

#[test]
fn sweep_listing_and_removal_decisions() {
    let id = "a".repeat(64);
    let name = format!("duduclaw-cu-{}", "b".repeat(32));
    let text = format!("{id}|{name}|running|1000\n{id}|{name}|exited|\n");
    let listed = sweep::parse_listing(&text).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].deadline, Some(1000));
    assert_eq!(listed[1].deadline, None);
    // Running and within deadline + grace: kept. Past it: removed.
    assert!(!sweep::removable(&listed[0], 1000 + sweep::GRACE.as_secs()));
    assert!(sweep::removable(&listed[0], 1001 + sweep::GRACE.as_secs()));
    // Exited: removed regardless of deadline.
    assert!(sweep::removable(&listed[1], 0));
    // Running without a readable deadline: never guessed.
    let mut no_deadline = listed[0].clone();
    no_deadline.deadline = None;
    assert!(!sweep::removable(&no_deadline, u64::MAX));
    // Not a computer-use container name: never touched.
    let mut other = listed[1].clone();
    other.name = "postgres".into();
    assert!(!sweep::removable(&other, u64::MAX));
    // Malformed listings remove nothing.
    assert!(sweep::parse_listing("short|x|running|1").is_none());
    assert!(sweep::parse_listing(&format!("{id}|{name}|Running|1")).is_none());
    assert_eq!(sweep::parse_listing("").unwrap(), vec![]);
}

// ── sessions (fake backend) ─────────────────────────────────────────────

#[tokio::test]
async fn start_refuses_disabled_and_native_employees_and_a_second_session() {
    let tmp = home();
    write_agent(
        tmp.path(),
        "carol",
        "[capabilities]\ncomputer_use = false\n",
    );
    write_agent(
        tmp.path(),
        "dave",
        "[capabilities]\ncomputer_use = true\ncomputer_use_mode = \"native\"\n",
    );
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let code = |r: Result<Value, OpError>| r.unwrap_err().code;
    assert_eq!(
        code(mgr.start("carol", StartRequest::default()).await),
        ErrorCode::CapabilityDisabled
    );
    assert_eq!(
        code(mgr.start("nobody", StartRequest::default()).await),
        ErrorCode::CapabilityDisabled
    );
    let native = mgr
        .start("dave", StartRequest::default())
        .await
        .unwrap_err();
    assert_eq!(native.code, ErrorCode::NativeUnsupported);
    // The removed mode is named, with the fix; no container was started for it.
    assert!(
        native.message.contains("已移除") && native.message.contains("\"container\""),
        "{}",
        native.message
    );
    assert_eq!(
        state.starts.load(Ordering::SeqCst),
        0,
        "no container for a refused employee"
    );

    let started = mgr.start("alice", StartRequest::default()).await.unwrap();
    assert_eq!(started["ok"], true);
    assert_eq!(started["width"], 1280);
    assert_eq!(started["max_actions"], 50);
    assert_eq!(started["confirmation_channel"], false);
    let first = started["session_id"].as_str().unwrap().to_string();
    let again = mgr
        .start("alice", StartRequest::default())
        .await
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::SessionExists);
    assert!(again.message.contains(&first), "{}", again.message);
    // Display size bounds.
    assert_eq!(
        code(
            mgr.start(
                "bob",
                StartRequest {
                    width: Some(100),
                    ..Default::default()
                }
            )
            .await
        ),
        ErrorCode::BadRequest
    );
}

#[tokio::test]
async fn a_failed_start_is_operator_safe_and_leaves_nothing() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    state.fail_start.store(true, Ordering::SeqCst);
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let err = mgr
        .start("alice", StartRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::StartFailed);
    assert!(
        !err.message.contains("secret") && !err.message.contains("/path"),
        "{}",
        err.message
    );
    assert!(mgr.is_empty());
    assert_eq!(
        state.stops.load(Ordering::SeqCst),
        1,
        "a half-started container is cleaned up"
    );
}

#[tokio::test]
async fn only_the_owner_can_see_or_drive_a_session() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());

    assert_eq!(
        mgr.screenshot("bob", None).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        mgr.screenshot("bob", Some(&id)).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        mgr.action("bob", Some(&id), None, &click(1, 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        mgr.stop("bob", Some(&id)).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(mgr.status("bob").await["active"], false);
    // A wrong session id is "not found" for the owner too.
    assert_eq!(
        mgr.screenshot("alice", Some("cu-other"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );

    let shot = mgr.screenshot("alice", Some(&id)).await.unwrap();
    assert_eq!(shot["png_base64"], tiny_png_b64());
    assert_eq!(shot["fully_masked"], false);
    assert_eq!(shot["mask_reason"], Value::Null);
    assert_eq!(mgr.status("alice").await["active"], true);
    let stopped = mgr.stop("alice", None).await.unwrap();
    assert_eq!(stopped["session_id"], id.as_str());
    assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    assert!(mgr.is_empty());
    assert_eq!(
        mgr.screenshot("alice", None).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

/// A fully masked screenshot says so, with its closed reason code, in the
/// response and in the browser audit row; a normal one says it was not.
#[tokio::test]
async fn a_fully_masked_screenshot_carries_its_reason_to_the_response_and_audit() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    for (reason, code) in [
        (FullMaskReason::HelperFailed, "helper_failed"),
        (FullMaskReason::SeveralPages, "several_pages"),
        (FullMaskReason::TitleSensitive, "title_sensitive"),
        (FullMaskReason::TitleUnreadable, "title_unreadable"),
    ] {
        *state.full_mask.lock().unwrap() = Some(reason);
        let shot = mgr.screenshot("alice", None).await.unwrap();
        assert_eq!(shot["fully_masked"], true, "{code}");
        assert_eq!(shot["mask_reason"], code);
    }
    *state.full_mask.lock().unwrap() = None;
    let shot = mgr.screenshot("alice", None).await.unwrap();
    assert_eq!(
        (shot["fully_masked"].clone(), shot["mask_reason"].clone()),
        (json!(false), Value::Null)
    );

    let rows: Vec<Value> = BrowserAuditLog::new(tmp.path(), AUDIT_RETENTION_DAYS)
        .entries_for_agent("alice", 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "screenshot")
        .map(|e| e.details)
        .collect();
    let reasons: Vec<Value> = rows.iter().map(|d| d["mask_reason"].clone()).collect();
    assert_eq!(
        reasons,
        vec![
            json!("helper_failed"),
            json!("several_pages"),
            json!("title_sensitive"),
            json!("title_unreadable"),
            Value::Null
        ]
    );
    assert_eq!(rows.iter().filter(|d| d["fully_masked"] == true).count(), 4);
}

#[tokio::test]
async fn the_action_budget_is_enforced_and_the_session_survives_it() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let cfg = ComputerUseConfig {
        max_actions: 2,
        ..Default::default()
    };
    insert_session(&mgr, &state, "alice", cfg);
    let first = mgr.action("alice", None, None, &click(1, 1)).await.unwrap();
    assert_eq!(first["actions_used"], 1);
    assert_eq!(first["actions_remaining"], 1);
    mgr.action("alice", None, None, &typed("hello"))
        .await
        .unwrap();
    let err = mgr
        .action("alice", None, None, &click(1, 1))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ActionLimit);
    assert_eq!(state.executed.lock().unwrap().len(), 2);
    // Screenshots and stop still work.
    assert!(mgr.screenshot("alice", None).await.is_ok());
    assert!(mgr.stop("alice", None).await.is_ok());
    // Invalid parameters never reach the backend nor count.
    insert_session(&mgr, &state, "bob", ComputerUseConfig::default());
    let bad = mgr
        .action("bob", None, None, &click(5000, 1))
        .await
        .unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidAction);
    assert_eq!(mgr.status("bob").await["actions_used"], 0);
}

#[tokio::test]
async fn deadline_and_idle_end_the_session_on_the_next_op_and_in_the_reaper() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, Duration::from_millis(50));
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    tokio::time::sleep(Duration::from_millis(80)).await;
    let err = mgr
        .action("alice", None, None, &click(1, 1))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::SessionEnded);
    assert!(mgr.is_empty());
    assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    assert!(state.executed.lock().unwrap().is_empty());

    // The reaper ends an idle session nobody touches again.
    insert_session(&mgr, &state, "bob", ComputerUseConfig::default());
    assert_eq!(mgr.reap_once().await, 0, "fresh session is kept");
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(mgr.reap_once().await, 1);
    assert!(mgr.is_empty());

    // A past deadline ends it even when active.
    let long = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let entry_id = insert_session(&long, &state, "alice", ComputerUseConfig::default());
    {
        let entry = long.lookup("alice").unwrap();
        let mut s = entry.lock().await;
        s.deadline = Instant::now() - Duration::from_secs(1);
    }
    let err = long.screenshot("alice", Some(&entry_id)).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::SessionEnded);
}

#[tokio::test]
async fn emergency_stop_flag_and_threat_levels_are_honoured() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    // The chat emergency stop sets `stopped` on the control handle.
    mgr.lookup("alice")
        .unwrap()
        .lock()
        .await
        .backend
        .control()
        .stopped
        .store(true, Ordering::SeqCst);
    assert_eq!(mgr.reap_once().await, 1);

    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    std::fs::write(tmp.path().join("threat_level"), "YELLOW\n").unwrap();
    assert_eq!(
        mgr.action("alice", None, None, &click(1, 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Paused
    );
    assert!(
        mgr.screenshot("alice", None).await.is_ok(),
        "YELLOW still allows screenshots"
    );
    std::fs::write(tmp.path().join("threat_level"), "RED\n").unwrap();
    assert_eq!(
        mgr.screenshot("alice", None).await.unwrap_err().code,
        ErrorCode::SessionEnded
    );
    assert!(mgr.is_empty());
}

#[tokio::test]
async fn revoking_the_capability_ends_the_session() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    write_agent(
        tmp.path(),
        "alice",
        "[capabilities]\ncomputer_use = false\n",
    );
    assert_eq!(
        mgr.screenshot("alice", None).await.unwrap_err().code,
        ErrorCode::SessionEnded
    );
    assert!(mgr.is_empty());
}

#[tokio::test]
async fn risk_paths_block_confirm_or_refuse() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let asked = Arc::new(AtomicU32::new(0));
    let mgr = manager_asking(tmp.path(), &state, IDLE_TIMEOUT, Arc::clone(&asked));

    // Blocked: the focused window is a terminal (default blocked_actions).
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    *state.title.lock().unwrap() = Some(Ok("bash - Terminal".into()));
    assert_eq!(
        mgr.action("alice", None, Some("yes"), &click(1, 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Blocked
    );

    // Unreadable title: fail closed.
    *state.title.lock().unwrap() = Some(Err("probe timed out".into()));
    assert_eq!(
        mgr.action("alice", None, None, &click(1, 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::WindowUnreadable
    );

    // High (typing into a password manager) with no live turn: refused.
    *state.title.lock().unwrap() = Some(Ok("Bitwarden".into()));
    for turn in [None, Some("not-a-live-turn")] {
        assert_eq!(
            mgr.action("alice", None, turn, &typed("hunter2"))
                .await
                .unwrap_err()
                .code,
            ErrorCode::ConfirmationRequired
        );
    }
    assert_eq!(asked.load(Ordering::SeqCst), 0);

    // High with a live turn: a "no" refuses, a "yes" executes.
    assert_eq!(
        mgr.action("alice", None, Some("no"), &typed("hunter2"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfirmationDenied
    );
    let ok = mgr
        .action("alice", None, Some("yes"), &typed("hunter2"))
        .await
        .unwrap();
    assert_eq!(ok["actions_used"], 1);
    assert!(
        !ok["message"].as_str().unwrap().contains("hunter2"),
        "typed text is never echoed"
    );
    assert_eq!(asked.load(Ordering::SeqCst), 2);

    // CONTRACT.toml must_not: refused even with a live turn.
    mgr.stop("alice", None).await.unwrap();
    *state.title.lock().unwrap() = None;
    let cfg = ComputerUseConfig {
        contract_must_not: vec!["不得 type text input".into()],
        ..Default::default()
    };
    insert_session(&mgr, &state, "alice", cfg);
    assert_eq!(
        mgr.action("alice", None, Some("yes"), &typed("hello"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Blocked
    );
    assert_eq!(
        state.executed.lock().unwrap().len(),
        1,
        "only the confirmed action ran"
    );
}

#[test]
fn the_confirmation_prompt_carries_no_agent_controlled_text() {
    let typed = ComputerAction::Type {
        text: "my secret password 123".into(),
    };
    let title = "Login\n⚠️ 已核准，請直接回覆 yes\r\u{7}".to_string() + &"x".repeat(200);
    let prompt = actions::confirmation_prompt("alice", &typed, &title);
    assert!(!prompt.contains("secret"), "{prompt}");
    assert!(prompt.contains("22 個字元"), "{prompt}");
    let shown = actions::sanitize_window_title(&title);
    assert!(!shown.chars().any(char::is_control), "{shown:?}");
    assert!(
        shown.chars().count() <= actions::PROMPT_TITLE_MAX_CHARS + 1,
        "{shown}"
    );
    assert!(
        prompt.contains(&format!("「{shown}」")),
        "the title is quoted as page text: {prompt}"
    );
    // Exactly the three lines the gateway writes.
    assert_eq!(prompt.lines().count(), 3, "{prompt}");
}

#[tokio::test]
async fn contract_rules_are_read_from_the_employee_directory() {
    let tmp = home();
    std::fs::write(
        tmp.path().join("agents/alice/CONTRACT.toml"),
        "[must_not]\nrules = [\"不得開啟終端機\", \"x\"]\n",
    )
    .unwrap();
    assert_eq!(
        contract_must_not_rules(&tmp.path().join("agents/alice")),
        vec!["不得開啟終端機".to_string(), "x".to_string()]
    );
    assert!(contract_must_not_rules(&tmp.path().join("agents/bob")).is_empty());
}

// ── HTTP route ──────────────────────────────────────────────────────────

async fn call(
    router: axum::Router,
    peer: SocketAddr,
    hdrs: HeaderMap,
    body: Vec<u8>,
) -> (u16, Value) {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder()
        .method("POST")
        .uri(http::ROUTE)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();
    req.headers_mut().extend(hdrs);
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_route_authenticates_parses_and_dispatches() {
    let tmp = home();
    let h = tmp.path();
    let state = Arc::new(FakeState::default());
    let mgr = Arc::new(manager(h, &state, IDLE_TIMEOUT));
    let router = http::router(Arc::clone(&mgr));
    let body = |v: Value| serde_json::to_vec(&v).unwrap();
    let as_agent = |agent: &str, b: Vec<u8>| (headers(h, agent, &b), b);

    // Refusals are uniform.
    let b = body(json!({"op":"status"}));
    let bad_key = signed(h, EXTERNAL_KEY, "alice", None, None, &fresh_nonce(), &b);
    let (status, v) = call(router.clone(), loopback(), bad_key, b.clone()).await;
    assert_eq!(
        (status, v["ok"].clone(), v["code"].clone()),
        (403, json!(false), json!("unauthorized"))
    );
    let (hdrs, b) = as_agent("alice", body(json!({"op":"status"})));
    let (status, _) = call(router.clone(), "10.0.0.2:1".parse().unwrap(), hdrs, b).await;
    assert_eq!(status, 403);
    // The old bearer scheme alone gets nothing.
    let mut bearer = HeaderMap::new();
    bearer.insert(
        "authorization",
        format!("Bearer {INTERNAL_KEY}").parse().unwrap(),
    );
    bearer.insert(auth::AGENT_ID_HEADER, "alice".parse().unwrap());
    let (status, v) = call(
        router.clone(),
        loopback(),
        bearer,
        body(json!({"op":"status"})),
    )
    .await;
    assert_eq!((status, v["code"].clone()), (403, json!("unauthorized")));
    // A replayed request (same nonce and signature) is refused.
    let (hdrs, b) = as_agent("alice", body(json!({"op":"status"})));
    let (status, _) = call(router.clone(), loopback(), hdrs.clone(), b.clone()).await;
    assert_eq!(status, 200);
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["code"].clone()), (403, json!("unauthorized")));

    // Malformed and oversized bodies.
    let (hdrs, b) = as_agent("alice", b"{not json".to_vec());
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["code"].clone()), (400, json!("bad_request")));
    let (hdrs, b) = as_agent("alice", body(json!({"op":"launch"})));
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["code"].clone()), (400, json!("bad_request")));
    let (hdrs, b) = as_agent("alice", vec![b' '; http::MAX_BODY_BYTES + 10]);
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!(
        (status, v["code"].clone()),
        (413, json!("payload_too_large"))
    );
    // A caller-named chat is not even a field any more; a bad turn id is refused.
    let (hdrs, b) = as_agent("alice", body(json!({"op":"start","turn_id":"a\nb"})));
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["code"].clone()), (400, json!("bad_request")));

    // A full round trip.
    let (hdrs, b) = as_agent("alice", body(json!({"op":"status"})));
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["active"].clone()), (200, json!(false)));
    let (hdrs, b) = as_agent(
        "alice",
        body(json!({"op":"start","task":"t","reply_channel":"telegram:666"})),
    );
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v["confirmation_channel"], false,
        "a reply_channel in the body is ignored"
    );
    let sid = v["session_id"].as_str().unwrap().to_string();
    let (hdrs, b) = as_agent(
        "alice",
        body(json!({"op":"action","action":{"type":"click","x":10,"y":20}})),
    );
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["actions_used"].clone()), (200, json!(1)), "{v}");
    let (hdrs, b) = as_agent(
        "alice",
        body(json!({"op":"action","action":{"type":"key","key":"$(rm)"}})),
    );
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["code"].clone()), (400, json!("invalid_action")));
    let (hdrs, b) = as_agent("bob", body(json!({"op":"screenshot","session_id": sid})));
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["code"].clone()), (404, json!("not_found")));
    let (hdrs, b) = as_agent(
        "alice",
        body(json!({"op":"screenshot","session_id":"../../x"})),
    );
    let (status, _) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!(status, 404);
    let (hdrs, b) = as_agent("alice", body(json!({"op":"stop"})));
    let (status, v) = call(router.clone(), loopback(), hdrs, b).await;
    assert_eq!((status, v["session_id"].clone()), (200, json!(sid)));
}

#[test]
fn every_op_has_a_server_bound_below_the_client_timeout() {
    // The MCP client waits 420 / 360 / 400 / 360 / 15 s (start / screenshot /
    // action / stop / status); the server gives up a few seconds earlier.
    assert!(http::START_BUDGET < Duration::from_secs(420));
    assert!(http::SCREENSHOT_BUDGET < Duration::from_secs(360));
    assert!(http::ACTION_BUDGET < Duration::from_secs(400));
    assert!(http::STOP_BUDGET < Duration::from_secs(360));
    assert!(http::STATUS_BUDGET < Duration::from_secs(15));
    // An action can wait for an approval and a confirmation within its bound.
    let approval = Duration::from_secs(gates::APPROVAL_TTL_SECS as u64);
    let confirm = Duration::from_secs(CONFIRM_TIMEOUT_SECS);
    assert!(approval + confirm + Duration::from_secs(30) <= http::ACTION_BUDGET);
    assert!(approval + Duration::from_secs(95) <= http::START_BUDGET);
}

// ── F1c/F1d: per-employee tool gates ───────────────────────────────────

#[tokio::test]
async fn denied_allowed_and_scoped_tools_are_enforced_per_op() {
    let tmp = home();
    let h = tmp.path();
    let state = Arc::new(FakeState::default());
    let mgr = manager(h, &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());

    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\ndenied_tools = [\"mcp__duduclaw__computer_type\"]\n",
    );
    let err = mgr
        .action("alice", None, None, &typed("x"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(err.message.contains("denied_tools"), "{}", err.message);
    assert!(
        mgr.action("alice", None, None, &click(1, 1)).await.is_ok(),
        "other tools still run"
    );

    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\nallowed_tools = [\"computer_screenshot\", \"computer_session_stop\"]\n",
    );
    assert_eq!(
        mgr.action("alice", None, None, &click(1, 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    assert!(mgr.screenshot("alice", None).await.is_ok());
    // `status` is never gated.
    assert_eq!(mgr.status("alice").await["active"], true);

    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\nscoped_tools = [\"computer_key\"]\n",
    );
    let key = ActionRequest::Key { key: "Tab".into() };
    let err = mgr.action("alice", None, None, &key).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(
        err.message.contains("capability_request"),
        "{}",
        err.message
    );
    let store = crate::capability_grants::CapabilityGrantStore::open(h).unwrap();
    store
        .grant(
            "alice",
            None,
            "computer_key",
            crate::capability_grants::GRANTED_BY_REQUEST,
            600,
        )
        .await
        .unwrap();
    assert!(
        mgr.action("alice", None, None, &key).await.is_ok(),
        "an active grant lets it run"
    );

    // Refusals leave the MCP gate's denial rows.
    let audit = std::fs::read_to_string(h.join("tool_calls.jsonl")).unwrap();
    assert!(
        audit.contains("\"denied_tools\"") && audit.contains("\"allowed_tools\""),
        "{audit}"
    );
    assert!(audit.contains("capability_grant_missing"), "{audit}");
    assert!(
        !audit.contains("\"x\""),
        "typed text never reaches the audit"
    );
    // Start and stop are gated too.
    write_agent(
        h,
        "bob",
        "[capabilities]\ncomputer_use = true\ndenied_tools = [\"computer_session_start\"]\n",
    );
    assert_eq!(
        mgr.start("bob", StartRequest::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\ndenied_tools = [\"computer_session_stop\"]\n",
    );
    assert_eq!(
        mgr.stop("alice", None).await.unwrap_err().code,
        ErrorCode::Forbidden
    );
}

/// Approve or deny the first pending approval of `agent` once it appears.
fn decide_when_pending(
    home: std::path::PathBuf,
    agent: &'static str,
    approve: bool,
    after: Duration,
) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let broker = crate::approval::ApprovalBroker::open(&home).unwrap();
        loop {
            let pending = broker.list_pending(Some(agent)).await.unwrap();
            if let Some(rec) = pending.first() {
                tokio::time::sleep(after).await;
                broker
                    .decide(&rec.id, approve, "test-operator")
                    .await
                    .unwrap();
                return rec.summary.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

#[tokio::test]
async fn listed_tools_need_a_human_decision_and_the_session_survives_the_wait() {
    let tmp = home();
    let h = tmp.path();
    let state = Arc::new(FakeState::default());
    let mgr = manager(h, &state, Duration::from_millis(100));
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\napproval_required_tools = [\"computer_click\"]\n\
         irreversible_tools = [\"computer_type\"]\nmaybe_irreversible_tools = [\"computer_key\"]\n",
    );

    // Approved after longer than the idle timeout: the reaper leaves the
    // session alone during the wait and the action runs.
    let decider = decide_when_pending(h.to_path_buf(), "alice", true, Duration::from_millis(250));
    let reaper = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        mgr.reap_once().await
    };
    let (done, reaped) = tokio::join!(
        async {
            let req = click(1, 1);
            mgr.action("alice", None, None, &req).await
        },
        reaper
    );
    assert_eq!(reaped, 0, "a pending approval is not idleness");
    assert_eq!(done.unwrap()["actions_used"], 1);
    let summary = decider.await.unwrap();
    assert!(summary.contains("computer_click"), "{summary}");

    // Denied: refused, nothing runs. Typed text never reaches the summary.
    let decider = decide_when_pending(h.to_path_buf(), "alice", false, Duration::ZERO);
    let err = mgr
        .action("alice", None, None, &typed("hunter2"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ApprovalDenied);
    let summary = decider.await.unwrap();
    assert!(
        summary.contains("7 個字元") && !summary.contains("hunter2"),
        "{summary}"
    );

    // maybe_irreversible is always asked about here (no judge); expiry refuses.
    let mut quick = manager(h, &state, IDLE_TIMEOUT);
    quick.approval_ttl_secs = 1;
    insert_session(&quick, &state, "alice", ComputerUseConfig::default());
    let err = quick
        .action(
            "alice",
            None,
            None,
            &ActionRequest::Key { key: "Tab".into() },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ApprovalDenied);
    assert_eq!(
        state.executed.lock().unwrap().len(),
        1,
        "only the approved click ran"
    );
}

#[tokio::test]
async fn an_emergency_stop_during_the_approval_wait_wins() {
    let tmp = home();
    let h = tmp.path();
    let state = Arc::new(FakeState::default());
    let mgr = manager(h, &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\napproval_required_tools = [\"computer_click\"]\n",
    );
    let decider = decide_when_pending(h.to_path_buf(), "alice", true, Duration::from_millis(50));
    let stopper = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        mgr.entry("alice")
            .unwrap()
            .control
            .stopped
            .store(true, Ordering::SeqCst);
    };
    let (done, ()) = tokio::join!(
        async {
            let req = click(1, 1);
            mgr.action("alice", None, None, &req).await
        },
        stopper
    );
    decider.await.unwrap();
    assert_eq!(done.unwrap_err().code, ErrorCode::SessionEnded);
    assert!(state.executed.lock().unwrap().is_empty());
    assert!(mgr.is_empty());
}

// ── F3: re-checks after a confirmation wait ──────────────────────────────

#[tokio::test]
async fn a_stop_or_threat_change_during_a_confirmation_wins_over_the_answer() {
    let tmp = home();
    let h = tmp.path();
    let state = Arc::new(FakeState::default());
    let mgr = manager(h, &state, IDLE_TIMEOUT);
    *state.title.lock().unwrap() = Some(Ok("Bitwarden".into()));

    // The emergency stop flag interrupts a confirmation nobody answers.
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let control = mgr.entry("alice").unwrap().control;
    let stopper = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        control.stopped.store(true, Ordering::SeqCst);
    };
    let started = Instant::now();
    let (done, ()) = tokio::join!(
        async {
            let req = typed("pw");
            mgr.action("alice", None, Some("slow"), &req).await
        },
        stopper
    );
    assert_eq!(done.unwrap_err().code, ErrorCode::SessionEnded);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "gave up within a poll interval"
    );
    assert!(mgr.is_empty());

    // YELLOW during the wait: paused, nothing typed.
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let path = h.join("threat_level");
    let yellow = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(&path, "YELLOW\n").unwrap();
    };
    let (done, ()) = tokio::join!(
        async {
            let req = typed("pw");
            mgr.action("alice", None, Some("slow"), &req).await
        },
        yellow
    );
    assert_eq!(done.unwrap_err().code, ErrorCode::Paused);
    std::fs::remove_file(&path).unwrap();

    // A revoked capability during the wait ends the session.
    let revoke = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_agent(h, "alice", "[capabilities]\ncomputer_use = false\n");
    };
    let (done, ()) = tokio::join!(
        async {
            let req = typed("pw");
            mgr.action("alice", None, Some("slow"), &req).await
        },
        revoke
    );
    assert_eq!(done.unwrap_err().code, ErrorCode::SessionEnded);
    assert!(state.executed.lock().unwrap().is_empty());
}

// ── F2: a cancelled end leaves nothing behind ────────────────────────────

#[tokio::test]
async fn a_stop_dropped_mid_way_still_removes_the_session_and_the_container() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    state.stop_delay_ms.store(300, Ordering::SeqCst);
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let dropped = tokio::time::timeout(Duration::from_millis(50), mgr.stop("alice", None)).await;
    assert!(
        dropped.is_err(),
        "the request was dropped while the container was stopping"
    );
    // The entry is gone at once: a new start is not refused as "exists".
    assert!(mgr.is_empty());
    assert_eq!(
        mgr.stop("alice", None).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(mgr.status("alice").await["active"], false);
    // The container stop finished on its own.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    state.stop_delay_ms.store(0, Ordering::SeqCst);
    assert!(mgr.start("alice", StartRequest::default()).await.is_ok());
}

#[tokio::test]
async fn an_ended_entry_left_in_the_map_counts_as_absent() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let stale = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.lookup("alice").unwrap().lock().await.ended = true;
    assert_eq!(
        mgr.screenshot("alice", None).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert!(mgr.is_empty(), "the lookup cleared it");
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.lookup("alice").unwrap().lock().await.ended = true;
    let started = mgr.start("alice", StartRequest::default()).await.unwrap();
    assert_ne!(started["session_id"], json!(stale));
}

// ── F8: start ordering, threat level, ephemeral ids, emergency stop ──────

#[tokio::test]
async fn start_refuses_threat_levels_and_ephemeral_identities() {
    let tmp = home();
    let h = tmp.path();
    let state = Arc::new(FakeState::default());
    let mgr = manager(h, &state, IDLE_TIMEOUT);
    for level in ["YELLOW\n", "RED\n"] {
        std::fs::write(h.join("threat_level"), level).unwrap();
        assert_eq!(
            mgr.start("alice", StartRequest::default())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Paused
        );
    }
    std::fs::remove_file(h.join("threat_level")).unwrap();
    assert_eq!(
        mgr.start("eph-alice-r1-executor-abc", StartRequest::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    assert_eq!(
        state.starts.load(Ordering::SeqCst),
        0,
        "no container was started"
    );
}

/// Registers in a shared slot table (cap 1) before `start`; `start` fails
/// when asked to. Records the order of calls.
struct SlotBackend {
    slots: Arc<Mutex<Vec<String>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    mine: Option<String>,
    fail_start: bool,
    control: Arc<OrchestratorControl>,
}

#[async_trait]
impl SessionBackend for SlotBackend {
    async fn start(&mut self) -> Result<(), ComputerUseError> {
        self.events.lock().unwrap().push("start");
        if self.fail_start {
            Err(ComputerUseError::ApiError("boom".into()))
        } else {
            Ok(())
        }
    }
    async fn register(&mut self, session_id: &str) -> Result<(), ComputerUseError> {
        self.events.lock().unwrap().push("register");
        let mut slots = self.slots.lock().unwrap();
        if slots.len() >= 1 {
            return Err(ComputerUseError::ApiError("full".into()));
        }
        slots.push(session_id.to_string());
        self.mine = Some(session_id.to_string());
        Ok(())
    }
    async fn stop(&mut self) {
        self.events.lock().unwrap().push("stop");
        if let Some(id) = self.mine.take() {
            self.slots.lock().unwrap().retain(|s| *s != id);
        }
    }
    async fn screenshot(&self) -> Result<MaskedScreenshot, ComputerUseError> {
        Ok(MaskedScreenshot {
            png_base64: tiny_png_b64(),
            full_mask: None,
        })
    }
    async fn window_title(&self) -> Result<String, String> {
        Ok(String::new())
    }
    async fn execute(&self, _action: &ComputerAction) -> Result<(), ComputerUseError> {
        Ok(())
    }
    fn control(&self) -> Arc<OrchestratorControl> {
        Arc::clone(&self.control)
    }
}

#[tokio::test]
async fn the_global_slot_is_reserved_before_the_container_and_released_on_failure() {
    let tmp = home();
    let slots = Arc::new(Mutex::new(Vec::<String>::new()));
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let fail = Arc::new(AtomicBool::new(true));
    let factory: BackendFactory = {
        let (slots, events, fail) = (Arc::clone(&slots), Arc::clone(&events), Arc::clone(&fail));
        Arc::new(
            move |_a: &str, _h: &std::path::Path, _c: ComputerUseConfig| {
                Box::new(SlotBackend {
                    slots: Arc::clone(&slots),
                    events: Arc::clone(&events),
                    mine: None,
                    fail_start: fail.load(Ordering::SeqCst),
                    control: Arc::new(OrchestratorControl::new()),
                }) as Box<dyn SessionBackend>
            },
        )
    };
    let mgr = ComputerUseSessions::with_parts(tmp.path().to_path_buf(), factory, IDLE_TIMEOUT);
    // A failed start released its slot.
    assert_eq!(
        mgr.start("alice", StartRequest::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StartFailed
    );
    assert_eq!(*events.lock().unwrap(), vec!["register", "start", "stop"]);
    assert!(slots.lock().unwrap().is_empty());
    // With the slot taken, the next start never runs a container.
    fail.store(false, Ordering::SeqCst);
    assert!(mgr.start("alice", StartRequest::default()).await.is_ok());
    events.lock().unwrap().clear();
    assert_eq!(
        mgr.start("bob", StartRequest::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Capacity
    );
    assert!(
        !events.lock().unwrap().contains(&"start"),
        "{:?}",
        events.lock().unwrap()
    );
}

#[tokio::test]
async fn the_emergency_stop_ends_tool_sessions_at_once() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = Arc::new(manager(tmp.path(), &state, IDLE_TIMEOUT));
    let idle_id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let busy_id = insert_session(&mgr, &state, "bob", ComputerUseConfig::default());
    // bob is busy with an op (lock held).
    let busy = mgr.lookup("bob").unwrap();
    let held = busy.lock().await;
    let mut ids = mgr.stop_all_for_emergency();
    ids.sort();
    let mut want = vec![idle_id, busy_id];
    want.sort();
    assert_eq!(ids, want);
    assert!(
        mgr.lookup("alice").is_none(),
        "the idle session ended at once"
    );
    assert!(
        held.backend.control().stopped.load(Ordering::SeqCst),
        "the busy one is flagged"
    );
    drop(held);
    for _ in 0..100 {
        if mgr.is_empty() && state.stops.load(Ordering::SeqCst) == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(mgr.is_empty());
    assert_eq!(state.stops.load(Ordering::SeqCst), 2);
}

// ── navigation allowlist (design §7) ────────────────────────────────────

fn navigate(url: &str) -> ActionRequest {
    ActionRequest::Navigate {
        url: url.to_string(),
    }
}

fn browser_audit(home: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("audit/browser/audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[tokio::test]
async fn an_allowlist_pins_resolved_hosts_and_navigate_only_opens_them() {
    let tmp = home();
    let h = tmp.path();
    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\n[capabilities.computer_use_config]\n\
         allowed_domains = [\"Example.com\", \"down.example\", \"*.wild.example\", \"1.2.3.4\"]\n",
    );
    let state = Arc::new(FakeState::default());
    let mgr = manager(h, &state, IDLE_TIMEOUT);
    let started = mgr.start("alice", StartRequest::default()).await.unwrap();
    assert_eq!(started["network"], "allowlist");
    assert_eq!(started["reachable_hosts"], json!(["example.com"]));
    assert_eq!(started["unreachable_hosts"], json!(["down.example"]));
    let msg = started["network_message"].as_str().unwrap();
    assert!(
        msg.contains("example.com") && msg.contains("down.example") && msg.contains("2 個項目"),
        "{msg}"
    );
    assert!(
        !msg.contains("93.184"),
        "no addresses in the message: {msg}"
    );
    let cfg = state.last_config.lock().unwrap().clone().unwrap();
    assert_eq!(
        cfg.pinned_hosts,
        vec![crate::computer_use_orchestrator::PinnedHost {
            host: "example.com".into(),
            ip: std::net::Ipv4Addr::new(93, 184, 215, 14),
        }]
    );
    assert!(!cfg.network_access && cfg.allowed_domains.is_empty());

    let v = mgr
        .action(
            "alice",
            None,
            None,
            &navigate("https://EXAMPLE.com/docs?token=abc"),
        )
        .await
        .unwrap();
    assert_eq!(v["actions_used"], 1);
    assert_eq!(v["host"], "example.com");
    assert_eq!(
        *state.navigated.lock().unwrap(),
        vec!["https://example.com/docs?token=abc".to_string()]
    );
    let row = browser_audit(h)
        .into_iter()
        .find(|r| r["action"] == "navigate")
        .expect("audit row");
    assert_eq!(row["url"], "https://example.com/docs");
    assert_eq!(row["domain"], "example.com");
    assert!(
        !row.to_string().contains("token=abc"),
        "query string never audited"
    );

    // Not resolved at start, not https, not in the list: refused, not counted.
    for url in [
        "https://down.example/",
        "http://example.com/",
        "https://evil.example/",
        "https://example.com:8443/",
    ] {
        let e = mgr
            .action("alice", None, None, &navigate(url))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidAction, "{url}");
        assert!(e.message.contains("example.com"), "{}", e.message);
    }
    assert_eq!(state.navigated.lock().unwrap().len(), 1);

    // A browser error is reported with its token only and still counts.
    state.nav_fail.store(true, Ordering::SeqCst);
    let e = mgr
        .action("alice", None, None, &navigate("https://example.com/"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ExecutionFailed);
    assert!(
        e.message.contains("net::ERR_CONNECTION_REFUSED"),
        "{}",
        e.message
    );
    assert_eq!(mgr.status("alice").await["actions_used"], 2);
}

#[tokio::test]
async fn without_an_allowlist_there_is_no_network_and_navigate_names_the_setting() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let started = mgr.start("alice", StartRequest::default()).await.unwrap();
    assert_eq!(started["network"], "none");
    assert_eq!(started["reachable_hosts"], json!([]));
    assert!(
        started["network_message"]
            .as_str()
            .unwrap()
            .contains("allowed_domains")
    );
    assert!(
        state
            .last_config
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .pinned_hosts
            .is_empty()
    );
    let e = mgr
        .action("alice", None, None, &navigate("https://example.com/"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidAction);
    assert!(
        e.message
            .contains("[capabilities.computer_use_config] allowed_domains"),
        "{}",
        e.message
    );
    assert!(state.navigated.lock().unwrap().is_empty());
}

#[tokio::test]
async fn navigate_goes_through_contract_budget_and_tool_gates() {
    let tmp = home();
    let h = tmp.path();
    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\n[capabilities.computer_use_config]\nmax_actions = 1\n\
         allowed_domains = [\"example.com\", \"docs.example.com\"]\n",
    );
    std::fs::write(
        h.join("agents/alice/CONTRACT.toml"),
        "[must_not]\nrules = [\"不得 navigate docs\"]\n",
    )
    .unwrap();
    let state = Arc::new(FakeState::default());
    let mgr = manager(h, &state, IDLE_TIMEOUT);
    mgr.start("alice", StartRequest::default()).await.unwrap();
    let e = mgr
        .action("alice", None, None, &navigate("https://docs.example.com/"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Blocked);
    mgr.action("alice", None, None, &navigate("https://example.com/"))
        .await
        .unwrap();
    let e = mgr
        .action("alice", None, None, &navigate("https://example.com/"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ActionLimit);
    // denied_tools names the navigate tool on its own.
    write_agent(
        h,
        "bob",
        "[capabilities]\ncomputer_use = true\ndenied_tools = [\"computer_navigate\"]\n\
         [capabilities.computer_use_config]\nallowed_domains = [\"example.com\"]\n",
    );
    mgr.start("bob", StartRequest::default()).await.unwrap();
    let e = mgr
        .action("bob", None, None, &navigate("https://example.com/"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Forbidden);
    assert_eq!(state.navigated.lock().unwrap().len(), 1);
}

/// S5: a listed `computer_navigate` is checked against the pinned hosts
/// before any approval is requested, and the approval text names the
/// validated host (never the path or query the agent sent).
#[tokio::test]
async fn navigate_validates_before_approval_and_the_summary_names_the_host() {
    let tmp = home();
    let h = tmp.path();
    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use = true\napproval_required_tools = [\"computer_navigate\"]\n\
         [capabilities.computer_use_config]\nallowed_domains = [\"example.com\"]\n",
    );
    let state = Arc::new(FakeState::default());
    let mut mgr = manager(h, &state, IDLE_TIMEOUT);
    mgr.approval_ttl_secs = 2;
    mgr.start("alice", StartRequest::default()).await.unwrap();
    // A refused URL fails at once: no approval record is ever created.
    let e = mgr
        .action("alice", None, None, &navigate("https://evil.example/"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidAction);
    let broker = crate::approval::ApprovalBroker::open(h).unwrap();
    assert!(broker.list_pending(Some("alice")).await.unwrap().is_empty());
    // An allowed URL: the approver sees the host, not the query.
    let decider = decide_when_pending(h.to_path_buf(), "alice", true, Duration::ZERO);
    mgr.action(
        "alice",
        None,
        None,
        &navigate("https://EXAMPLE.com/a?secret=1"),
    )
    .await
    .unwrap();
    let summary = decider.await.unwrap();
    assert!(summary.contains("開啟網頁：example.com"), "{summary}");
    assert!(
        !summary.contains("secret") && !summary.contains("/a"),
        "{summary}"
    );
    assert_eq!(
        *state.navigated.lock().unwrap(),
        vec!["https://example.com/a?secret=1".to_string()]
    );
}

// ── real Docker (opt-in) ────────────────────────────────────────────────

/// A home for the real-Docker tests: keys, identity, the image override and
/// two employees.
fn docker_home(image: &str) -> tempfile::TempDir {
    let tmp = home();
    write_config(
        tmp.path(),
        &format!("[computer_use]\nimage = \"{image}\"\n"),
    );
    tmp
}

async fn containers_of(home: &std::path::Path) -> String {
    let filter = format!(
        "label={}={}",
        crate::computer_use_orchestrator::HOME_LABEL,
        crate::computer_use_orchestrator::computer_use_home_label(home)
    );
    let out = crate::computer_use_orchestrator::docker_output(
        &["ps", "--all", "--quiet", "--filter", filter.as_str()],
        Duration::from_secs(15),
        "ps",
    )
    .await
    .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Real Docker, opt-in:
/// `DUDU_COMPUTER_USE_IMAGE=duduclaw-computer-use:latest cargo test -p duduclaw-gateway --lib
/// --no-default-features -- --ignored computer_use_sessions`.
///
/// Through the real route on an ephemeral port: start → screenshot (PNG
/// decodes to the display size) → click → type → another employee cannot
/// see it → stop → the container is gone.
#[tokio::test]
#[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn real_docker_tool_session_over_http() {
    use base64::Engine;
    let image = std::env::var("DUDU_COMPUTER_USE_IMAGE").expect("set DUDU_COMPUTER_USE_IMAGE");
    let tmp = docker_home(&image);
    let h = tmp.path().to_path_buf();
    let mgr = ComputerUseSessions::new(h.clone());
    let app = http::router(Arc::clone(&mgr));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{addr}{}", http::ROUTE);
    let post = |agent: &'static str, body: Value| {
        let bytes = serde_json::to_vec(&body).unwrap();
        let req = client
            .post(&url)
            .headers(headers(&h, agent, &bytes))
            .header("content-type", "application/json")
            .timeout(Duration::from_secs(120))
            .body(bytes);
        async move {
            let resp = req.send().await.unwrap();
            let status = resp.status().as_u16();
            (status, resp.json::<Value>().await.unwrap())
        }
    };

    let (status, started) = post("alice", json!({"op":"start","task":"real docker test"})).await;
    let mut failures = Vec::new();
    if status == 200 {
        let (s, shot) = post("alice", json!({"op":"screenshot"})).await;
        let png = base64::engine::general_purpose::STANDARD
            .decode(shot["png_base64"].as_str().unwrap_or_default())
            .unwrap_or_default();
        match image::load_from_memory(&png) {
            Ok(img) if s == 200 && (img.width(), img.height()) == (1280, 800) => {}
            other => failures.push(format!(
                "screenshot: status {s}, {:?}",
                other.map(|i| (i.width(), i.height()))
            )),
        }
        let (s, v) = post(
            "alice",
            json!({"op":"action","action":{"type":"click","x":640,"y":400}}),
        )
        .await;
        if s != 200 {
            failures.push(format!("click: {v}"));
        }
        let (s, v) = post(
            "alice",
            json!({"op":"action","action":{"type":"type","text":"hello"}}),
        )
        .await;
        if s != 200 || v["actions_used"] != 2 {
            failures.push(format!("type: {v}"));
        }
        let (s, v) = post(
            "bob",
            json!({"op":"screenshot","session_id": started["session_id"]}),
        )
        .await;
        if s != 404 {
            failures.push(format!("bob saw alice's session: {s} {v}"));
        }
        let (s, v) = post("alice", json!({"op":"stop"})).await;
        if s != 200 {
            failures.push(format!("stop: {v}"));
        }
    }
    // Cleanup no matter what, then assert.
    let _ = post("alice", json!({"op":"stop"})).await;
    let left = containers_of(&h).await;
    server.abort();
    assert_eq!(status, 200, "start failed: {started}");
    assert!(failures.is_empty(), "{failures:?}");
    assert!(left.is_empty(), "containers left behind: {left}");
}

/// Real Docker, opt-in: an idle session is reaped and its container removed.
#[tokio::test]
#[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn real_docker_idle_session_is_reaped() {
    let image = std::env::var("DUDU_COMPUTER_USE_IMAGE").expect("set DUDU_COMPUTER_USE_IMAGE");
    let tmp = docker_home(&image);
    let mgr = ComputerUseSessions::with_parts(
        tmp.path().to_path_buf(),
        super::backend::orchestrator_factory(),
        Duration::from_secs(2),
    );
    let started = mgr.start("alice", StartRequest::default()).await;
    let running = containers_of(tmp.path()).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let reaped = mgr.reap_once().await;
    let left = containers_of(tmp.path()).await;
    if !left.is_empty() {
        let _ = mgr.stop("alice", None).await;
    }
    started.expect("start");
    assert!(!running.is_empty(), "container was running before the reap");
    assert_eq!(reaped, 1);
    assert!(left.is_empty(), "containers left behind: {left}");
    assert!(mgr.is_empty());
}

/// Share of pixels that differ from the top-left one (0.0 for a blank page).
fn non_uniform_share(png: &[u8]) -> f64 {
    let img = image::load_from_memory(png).expect("png").to_rgba8();
    let first = *img.get_pixel(0, 0);
    let differ = img.pixels().filter(|p| **p != first).count();
    differ as f64 / f64::from(img.width() * img.height())
}

/// Share of pixels that differ between two same-size screenshots.
fn changed_share(a: &[u8], b: &[u8]) -> f64 {
    let a = image::load_from_memory(a).expect("png").to_rgba8();
    let b = image::load_from_memory(b).expect("png").to_rgba8();
    assert_eq!(a.dimensions(), b.dimensions());
    let differ = a.pixels().zip(b.pixels()).filter(|(x, y)| x != y).count();
    differ as f64 / f64::from(a.width() * a.height())
}

/// Real Docker + outbound network, opt-in:
/// `DUDU_COMPUTER_USE_IMAGE=duduclaw-computer-use:latest cargo test -p duduclaw-gateway --lib
/// --no-default-features -- --ignored real_docker_navigation`.
///
/// Through the real route with `allowed_domains = ["example.com"]`: start
/// (example.com reachable) → blank screenshot → navigate → the screenshot
/// differs from the blank one → a host outside the list is refused →
/// `ctrl+n` opens a second window, so the next screenshot is fully masked
/// with `several_pages` → navigate again (closes the extra window) → the
/// screenshot is normal again → stop → the container is gone.
#[tokio::test]
#[ignore = "needs Docker, outbound network and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn real_docker_navigation_over_http() {
    use base64::Engine;
    let image = std::env::var("DUDU_COMPUTER_USE_IMAGE").expect("set DUDU_COMPUTER_USE_IMAGE");
    let tmp = docker_home(&image);
    write_agent(
        tmp.path(),
        "alice",
        "[capabilities]\ncomputer_use = true\n[capabilities.computer_use_config]\nallowed_domains = [\"example.com\"]\n",
    );
    let h = tmp.path().to_path_buf();
    let mgr = ComputerUseSessions::new(h.clone());
    let app = http::router(Arc::clone(&mgr));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{addr}{}", http::ROUTE);
    let post = |agent: &'static str, body: Value| {
        let bytes = serde_json::to_vec(&body).unwrap();
        let req = client
            .post(&url)
            .headers(headers(&h, agent, &bytes))
            .header("content-type", "application/json")
            .timeout(Duration::from_secs(120))
            .body(bytes);
        async move {
            let resp = req.send().await.unwrap();
            let status = resp.status().as_u16();
            (status, resp.json::<Value>().await.unwrap())
        }
    };
    let shot = |v: &Value| {
        base64::engine::general_purpose::STANDARD
            .decode(v["png_base64"].as_str().unwrap_or_default())
            .unwrap_or_default()
    };

    let (status, started) = post("alice", json!({"op":"start","task":"navigation test"})).await;
    let mut failures = Vec::new();
    if status == 200 {
        if started["reachable_hosts"] != json!(["example.com"]) || started["network"] != "allowlist"
        {
            failures.push(format!("start: {started}"));
        }
        // Let the kiosk page settle before the blank reference shot.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (_, blank) = post("alice", json!({"op":"screenshot"})).await;
        let blank_share = non_uniform_share(&shot(&blank));
        let (s, v) = post(
            "alice",
            json!({"op":"action","action":{"type":"navigate","url":"https://example.com/"}}),
        )
        .await;
        if s != 200 || v["host"] != "example.com" {
            failures.push(format!("navigate: {s} {v}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (_, after) = post("alice", json!({"op":"screenshot"})).await;
        let after_png = shot(&after);
        let after_share = non_uniform_share(&after_png);
        let changed = changed_share(&shot(&blank), &after_png);
        // A blank page is one colour; the loaded page has text on it and
        // differs from the blank shot almost everywhere (its background).
        if !(blank_share < 0.001 && after_share > 0.001 && changed > 0.5) {
            failures.push(format!(
                "screenshot did not change: blank {blank_share:.4}, after {after_share:.4}, changed {changed:.4}"
            ));
        }
        if after["fully_masked"] != false || after["mask_reason"] != Value::Null {
            failures.push(format!(
                "loaded page reported fully masked: {} {}",
                after["fully_masked"], after["mask_reason"]
            ));
        }
        let (s, v) = post(
            "alice",
            json!({"op":"action","action":{"type":"navigate","url":"https://www.iana.org/"}}),
        )
        .await;
        if s != 400 || v["code"] != "invalid_action" {
            failures.push(format!("outside host not refused: {s} {v}"));
        }
        // A second window: the masking helper cannot tell which page is on
        // top, so the whole screenshot is hidden and says why.
        let (s, v) = post(
            "alice",
            json!({"op":"action","action":{"type":"key","key":"ctrl+n"}}),
        )
        .await;
        if s != 200 {
            failures.push(format!("ctrl+n: {s} {v}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (s, two) = post("alice", json!({"op":"screenshot"})).await;
        if s != 200 || two["fully_masked"] != true || two["mask_reason"] != "several_pages" {
            failures.push(format!(
                "two windows: status {s}, fully_masked {}, mask_reason {}",
                two["fully_masked"], two["mask_reason"]
            ));
        }
        if non_uniform_share(&shot(&two)) != 0.0 {
            failures.push("two windows: screenshot was not fully masked".to_string());
        }
        // Navigating closes the extra window; the next screenshot is normal.
        let (s, v) = post(
            "alice",
            json!({"op":"action","action":{"type":"navigate","url":"https://example.com/"}}),
        )
        .await;
        if s != 200 {
            failures.push(format!("navigate after ctrl+n: {s} {v}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (s, again) = post("alice", json!({"op":"screenshot"})).await;
        if s != 200 || again["fully_masked"] != false || again["mask_reason"] != Value::Null {
            failures.push(format!(
                "after navigate: status {s}, fully_masked {}, mask_reason {}",
                again["fully_masked"], again["mask_reason"]
            ));
        }
        if non_uniform_share(&shot(&again)) <= 0.001 {
            failures.push("after navigate: the page is blank or masked".to_string());
        }
        let (s, v) = post("alice", json!({"op":"stop"})).await;
        if s != 200 {
            failures.push(format!("stop: {v}"));
        }
    }
    let _ = post("alice", json!({"op":"stop"})).await;
    let left = containers_of(&h).await;
    server.abort();
    assert_eq!(status, 200, "start failed: {started}");
    assert!(failures.is_empty(), "{failures:?}");
    assert!(left.is_empty(), "containers left behind: {left}");
}

#[tokio::test]
async fn durable_confirmation_deny_then_new_request_can_execute_and_never_persists_type_text() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    *state.title.lock().unwrap() = Some(Ok("Bitwarden".into()));
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let secret = "CONFIDENTIAL_TYPE_MARKER_987";
    let req = typed(secret);
    assert_eq!(
        mgr.action("alice", None, Some("no"), &req)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfirmationDenied
    );
    mgr.action("alice", None, Some("yes"), &req).await.unwrap();
    assert_eq!(state.executed.lock().unwrap().len(), 1);
    let conn = rusqlite::Connection::open(tmp.path().join("approvals.db")).unwrap();
    for table in ["approvals", "approval_operations"] {
        let col = if table == "approvals" {
            "payload"
        } else {
            "payload_json"
        };
        let mut q = conn.prepare(&format!("SELECT {col} FROM {table}")).unwrap();
        for row in q.query_map([], |r| r.get::<_, String>(0)).unwrap() {
            assert!(!row.unwrap().contains(secret));
        }
    }
    let b = crate::approval::ApprovalBroker::open(tmp.path()).unwrap();
    let ops = b.list_operations().await.unwrap();
    assert!(
        ops.iter()
            .any(|o| o.state == crate::approval::OperationState::Succeeded)
    );
}

#[tokio::test]
async fn fresh_window_change_invalidates_old_confirmation_then_allows_new_observation() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    *state.title.lock().unwrap() = Some(Ok("Bitwarden".into()));
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let b = crate::approval::ApprovalBroker::open(tmp.path()).unwrap();
    let decider = async {
        let mut found = None;
        for _ in 0..100 {
            let rows = b.list_pending(Some("alice")).await.unwrap();
            if let Some(row) = rows.into_iter().find(|r| r.binding.is_some()) {
                found = Some(row);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let row = found.expect("durable request exists before delivery wait");
        *state.title.lock().unwrap() = Some(Ok("1Password".into()));
        b.decide_bound(
            &row.id,
            &row.binding.as_ref().unwrap().decision_context,
            true,
        )
        .await
        .unwrap();
    };
    let req = typed("pw");
    let (result, ()) = tokio::join!(mgr.action("alice", None, Some("slow"), &req), decider);
    assert_eq!(result.unwrap_err().code, ErrorCode::ConfirmationDenied);
    assert!(state.executed.lock().unwrap().is_empty());
    mgr.action("alice", None, Some("yes"), &req).await.unwrap();
    assert_eq!(state.executed.lock().unwrap().len(), 1);
}

#[path = "tests/action_boundary.rs"]
mod action_boundary;

#[path = "tests/secret_persistence.rs"]
mod secret_persistence;

#[path = "tests/workspace_cases.rs"]
mod workspace_cases;

#[path = "tests/workspace_docker.rs"]
mod workspace_docker;
