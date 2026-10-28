//! Explicitly ignored, synthetic native backend acceptance. Never compiled
//! into shipping gateways. The paired app/presence and page server are fixtures;
//! Chrome, its sandbox, profile, CDP pipe, masking, guards and backend are real.
//! Execution requires an independently reviewed fresh-home launcher.
use super::{
    backend::LocalBrowserBackend,
    cdp_pipe::CdpPipe,
    companion::{self, Companion},
};
use crate::computer_use_orchestrator::{ComputerUseConfig, PinnedHost};
use crate::computer_use_sessions::{SessionBackend, attended, attended_call, turns};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct Fixture {
    fulfilled: AtomicUsize,
    downloads_canceled: AtomicUsize,
    downloads_completed: AtomicUsize,
    file_choosers: AtomicUsize,
    extra_closed: AtomicUsize,
    extra_ids: std::sync::Mutex<std::collections::HashSet<String>>,
    control_download: std::sync::atomic::AtomicBool,
    mask_delay: std::sync::atomic::AtomicBool,
    mask_armed: std::sync::Mutex<Option<(std::time::Instant, std::time::Instant)>>,
    home: std::path::PathBuf,
    children: std::sync::Mutex<Vec<super::processes::OwnedGroup>>,
}
tokio::task_local! {static FIXTURE:Arc<Fixture>;}
pub(super) fn current() -> Option<Arc<Fixture>> {
    FIXTURE.try_with(Arc::clone).ok()
}
pub(super) fn machine_lease_path() -> Option<std::path::PathBuf> {
    current().map(|fixture| {
        let canonical = fixture
            .home
            .canonicalize()
            .expect("owned native fixture home must exist");
        assert_eq!(canonical, fixture.home);
        assert!(
            canonical
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ddc-b9b-native-")
        );
        assert_eq!(
            std::fs::read_to_string(canonical.join("b9b-native-fixture-owned-root")).unwrap(),
            "synthetic-only\n"
        );
        canonical.join("owned-machine-lease.lock")
    })
}

#[tokio::test]
async fn native_fixture_machine_barrier_is_owned_and_never_uses_the_real_uid_lease() {
    let root = tempfile::Builder::new()
        .prefix("ddc-b9b-native-")
        .tempdir()
        .unwrap();
    let home = root.path().canonicalize().unwrap();
    std::fs::write(
        home.join("b9b-native-fixture-owned-root"),
        "synthetic-only\n",
    )
    .unwrap();
    let fixture = Arc::new(Fixture {
        home: home.clone(),
        ..Default::default()
    });
    FIXTURE
        .scope(fixture, async {
            let path = super::machine_lease::path();
            assert_eq!(path, home.join("owned-machine-lease.lock"));
            let lease = super::machine_lease::MachineLease::acquire(&path).unwrap();
            lease.begin_launch().unwrap();
            drop(lease);
            assert_eq!(super::machine_lease::status(&path), "cleanup_unverified");
        })
        .await;
}
pub(super) fn active() -> bool {
    current().is_some()
}
// These seams only wait. They never use privileged CDP to move page content
// after the detector (which would manufacture an unreachable page-script RED).
pub(super) async fn before_mask_capture(rectangles: &[[u32; 4]]) {
    if let Some(fixture) = current().filter(|fixture| fixture.mask_delay.load(Ordering::Acquire)) {
        let earliest = fixture.mask_armed.lock().unwrap().unwrap().0;
        assert!(
            earliest.elapsed() < Duration::from_secs(4),
            "INVALID: detector missed first animation A window"
        );
        assert!(
            rectangles
                .iter()
                .any(|r| r[0] <= 40 && r[0] + r[2] >= 200 && r[1] <= 100 && r[1] + r[3] >= 148),
            "mask-race fixture did not start at A"
        );
        tokio::time::sleep(Duration::from_secs(7)).await;
        assert_phase_b(&fixture);
    }
}
pub(super) async fn after_mask_capture() {
    if let Some(fixture) = current().filter(|fixture| fixture.mask_delay.load(Ordering::Acquire)) {
        assert_phase_b(&fixture);
        tokio::time::sleep(Duration::from_secs(8)).await;
    }
}
fn assert_phase_b(fixture: &Fixture) {
    let (earliest, latest) = fixture.mask_armed.lock().unwrap().unwrap();
    // The page's animation start lies between request send and reply receive.
    // Both bounds must place the complete capture in its B plateau [5.1,10.8).
    assert!(
        latest.elapsed() >= Duration::from_secs(6) && earliest.elapsed() < Duration::from_secs(10),
        "INVALID: capture outside proven animation B window"
    );
}

async fn native_fixture_ingress<F: std::future::Future>(future: F) -> F::Output {
    attended::verified_webchat_message("webchat:native-synthetic", Some("synthetic-human"), future)
        .await
}

#[tokio::test]
async fn native_fixture_producer_uses_authenticated_webchat_scope_and_revokes_on_exit() {
    let proof = native_fixture_ingress(async {
        assert!(attended::bind_final_route(
            "webchat:native-synthetic",
            "webchat:native-synthetic",
            "alice",
            "native-synthetic-turn"
        ));
        duduclaw_memory::feedback::CURRENT_SESSION_ID
            .scope(
                Some("webchat:native-synthetic".into()),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                    Some("native-synthetic-turn".into()),
                    async {
                        let _turn = turns::register_current_turn("alice").unwrap();
                        let proof = attended_call::capture("alice").unwrap();
                        assert!(proof.valid_for("alice"));
                        assert!(!proof.valid_for("other-agent"));
                        proof
                    },
                ),
            )
            .await
    })
    .await;
    assert!(!proof.valid_for("alice"));
    attended::verified_channel_message(
        "webchat:native-synthetic",
        "synthetic-human",
        false,
        async {
            assert!(!attended::bind_final_route(
                "webchat:native-synthetic",
                "webchat:native-synthetic",
                "alice",
                "native-synthetic-turn"
            ));
        },
    )
    .await;
}
pub(super) fn allow_download_control(allow: bool) {
    current()
        .expect("native fixture scope")
        .control_download
        .store(allow, Ordering::Release);
}
pub(super) fn refresh(
    fixture: Option<&Fixture>,
    group: &super::processes::OwnedGroup,
) -> Result<(), String> {
    let Some(fixture) = fixture else {
        return Ok(());
    };
    crate::fs_safe::SafeDir::open_root(&fixture.home)
        .map_err(|_| "native evidence directory unavailable")?
        .write_file_atomic(
            "owned-processes.json",
            &serde_json::to_string(&group.manifest())
                .map_err(|_| "native child evidence invalid")?,
        )
        .map_err(|_| "native child evidence unavailable".into())
}
pub(super) fn track(group: &super::processes::OwnedGroup) -> Result<(), String> {
    use std::io::Write;
    let Some(fixture) = current() else {
        return Ok(());
    };
    // Establish owned PID/birth/exe evidence before any fallible CDP await.
    let path = fixture.home.join("owned-processes.json");
    if path.exists() {
        if !fixture
            .children
            .lock()
            .unwrap()
            .iter()
            .all(super::processes::OwnedGroup::inactive)
        {
            return Err("previous native browser cleanup incomplete".into());
        }
        refresh(Some(&fixture), group)?;
        fixture.children.lock().unwrap().push(group.clone());
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "native child evidence unavailable")?;
    file.write_all(
        serde_json::to_string(&group.manifest())
            .map_err(|_| "native child evidence invalid")?
            .as_bytes(),
    )
    .map_err(|_| "native child evidence unavailable")?;
    file.sync_all()
        .map_err(|_| "native child evidence unavailable")?;
    std::fs::File::open(&fixture.home)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "native child evidence unavailable")?;
    fixture.children.lock().unwrap().push(group.clone());
    Ok(())
}

const PAGE: &str = r#"<!doctype html><html><head><title>Synthetic B9b fixture</title></head>
<body style="background:white;color:black"><h1>Synthetic backend fixture</h1>
<input id="password" type="password" value="SYNTHETIC_PASSWORD_NEVER_CAPTURE">
<input id="plain" value=""><button id="action" onclick="document.body.dataset.clicked='yes'">Synthetic action</button>
<input id="upload" type="file"><a id="download" href="/download">Synthetic download</a>
</body></html>"#;

// The page owns the animation before any capture/quiescence seam. It moves
// A -> B -> A without a timer, inspector mutation or a network dependency.
const MASK_PAGE: &str = r#"<!doctype html><html><head><title>Synthetic mask fixture</title>
<style>
html,body {margin:0;background:white;color:black}
#secret {position:absolute;left:40px;top:100px;width:160px;height:48px;box-sizing:border-box;border:0;padding:0;background:rgb(255,0,0);color:white}
#secret.running {animation:owned-shift 30s linear both}
@keyframes owned-shift {0%,16% {transform:translateX(0)} 17%,36% {transform:translateX(240px)} 37%,100% {transform:translateX(0)}}
#visible-control {position:absolute;left:600px;top:350px;width:80px;height:80px;background:rgb(0,0,255)}
</style></head><body><h1>Synthetic mask control</h1>
<input id="secret" value="SYNTHETIC_MASK_SENTINEL"><div id="visible-control"></div>
<button id="arm" onclick="const e=document.getElementById('secret');e.classList.remove('running');void e.offsetWidth;e.classList.add('running')">Arm page animation</button>
</body></html>"#;

pub(super) async fn fulfill(
    pipe: &CdpPipe,
    event: &Value,
    fixture: Option<&Fixture>,
) -> Result<bool, String> {
    let Some(fixture) = fixture else {
        return Ok(false);
    };
    match event["method"].as_str() {
        Some("Browser.downloadProgress") => match event["params"]["state"].as_str() {
            Some("canceled") => {
                fixture.downloads_canceled.fetch_add(1, Ordering::AcqRel);
            }
            Some("completed") => {
                fixture.downloads_completed.fetch_add(1, Ordering::AcqRel);
            }
            _ => {}
        },
        Some("Browser.downloadWillBegin")
            if event["params"]["url"] == "https://allowed.example/download-control"
                && fixture.control_download.load(Ordering::Acquire) =>
        {
            return Ok(true);
        }
        Some("Page.fileChooserOpened") => {
            fixture.file_choosers.fetch_add(1, Ordering::AcqRel);
        }
        Some("Target.targetCreated") if event["params"]["targetInfo"]["openerId"].is_string() => {
            if let Some(id) = event["params"]["targetInfo"]["targetId"].as_str() {
                fixture.extra_ids.lock().unwrap().insert(id.into());
            }
        }
        Some("Target.targetDestroyed") => {
            if event["params"]["targetId"]
                .as_str()
                .is_some_and(|id| fixture.extra_ids.lock().unwrap().remove(id))
            {
                fixture.extra_closed.fetch_add(1, Ordering::AcqRel);
            }
        }
        _ => {}
    }
    if event["method"] != "Fetch.requestPaused" {
        return Ok(false);
    }
    let params = &event["params"];
    let raw = params["request"]["url"].as_str().unwrap_or("");
    let (status, body, headers) = match raw {
        "https://allowed.example/fixture" => (
            200,
            PAGE.as_bytes(),
            json!([{"name":"Content-Type","value":"text/html; charset=utf-8"}]),
        ),
        "https://allowed.example/mask-fixture" => (
            200,
            MASK_PAGE.as_bytes(),
            json!([{"name":"Content-Type","value":"text/html; charset=utf-8"}]),
        ),
        "https://allowed.example/download" => (
            200,
            b"synthetic download".as_slice(),
            json!([{"name":"Content-Type","value":"application/octet-stream"},{"name":"Content-Disposition","value":"attachment; filename=synthetic.txt"}]),
        ),
        "https://allowed.example/download-control" => (
            200,
            b"synthetic download control".as_slice(),
            json!([{"name":"Content-Type","value":"application/octet-stream"},{"name":"Content-Disposition","value":"attachment; filename=synthetic-control.txt"}]),
        ),
        _ if super::page_guard::allowed_url(raw, &["allowed.example".into()], false) => (
            404,
            b"synthetic missing resource".as_slice(),
            json!([{"name":"Content-Type","value":"text/plain"}]),
        ),
        _ => return Ok(false),
    };
    pipe.call("Fetch.fulfillRequest",json!({"requestId":params["requestId"],"responseCode":status,"responseHeaders":headers,"body":base64::engine::general_purpose::STANDARD.encode(body)}),event["sessionId"].as_str()).await?;
    fixture.fulfilled.fetch_add(1, Ordering::AcqRel);
    Ok(true)
}

async fn native_message(companion: &Arc<Companion>, key: &[u8; 32], body: Value) -> Value {
    use axum::{
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::Request,
    };
    use duduclaw_core::local_browser_channel as wire;
    use tower::ServiceExt;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let body = serde_json::to_vec(&body).unwrap();
    let signature = wire::sign(key, false, time, &nonce, &body).unwrap();
    let request = Request::builder()
        .method("POST")
        .uri(wire::ROUTE)
        .header("x-local-time", time.to_string())
        .header("x-local-nonce", &nonce)
        .header("x-local-signature", signature)
        .extension(ConnectInfo(
            "127.0.0.1:42424".parse::<std::net::SocketAddr>().unwrap(),
        ))
        .body(Body::from(body))
        .unwrap();
    let response = companion::router(companion.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert!(response.status().is_success());
    let signature = response.headers()["x-local-signature"]
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    assert!(wire::verify(
        key, true, time, &nonce, &bytes, &signature, time
    ));
    serde_json::from_slice(&bytes).unwrap()
}
fn present() -> Value {
    json!({"session_available":true,"on_console":true,"login_done":true,"locked":false,"idle_seconds":0.1,"browser_frontmost":false,"input_sequence":1})
}

async fn wait_counter(counter: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while counter.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native event was not observed");
}
async fn click(
    backend: &LocalBrowserBackend,
    id: &str,
) -> Result<(), crate::computer_use::ComputerUseError> {
    assert!(matches!(id, "download" | "upload"));
    let expression = format!(
        "(()=>{{const r=document.getElementById({}).getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2]}})()",
        serde_json::to_string(id).unwrap()
    );
    let point = backend
        .fixture_eval(&expression)
        .await
        .map_err(crate::computer_use::ComputerUseError::Unavailable)?;
    backend
        .execute(&crate::computer_use::ComputerAction::LeftClick {
            coordinate: [
                point[0].as_f64().unwrap() as u32,
                point[1].as_f64().unwrap() as u32,
            ],
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires independently reviewed fresh-home native launcher; no real accounts or OS Keychain"]
async fn actual_local_backend_uses_the_signed_native_lane_and_hardened_chrome() {
    run_native_fixture(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires independently reviewed fresh-home native launcher; synthetic mask-race probe only"]
async fn actual_local_masking_reflow_does_not_expose_a_sensitive_field() {
    run_native_fixture(true).await;
}

async fn mask_race(backend: &LocalBrowserBackend) {
    assert!(
        backend
            .navigate("https://allowed.example/mask-fixture")
            .await
            .unwrap()
            .ok
    );
    let fixture = current().unwrap();
    arm_animation(backend, &fixture).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_phase_b(&fixture);
    // A raw calibration is test-only and stays in RAM. It proves the sensor
    // detects the red secret at B and a separate visible, nonsecret control.
    let calibration = backend
        .fixture_raw_capture_for_pixel_control()
        .await
        .unwrap();
    assert_phase_b(&fixture);
    let (red, blue) = pixel_counts(&calibration);
    assert!(
        red > 500 && blue > 1000,
        "synthetic pixel negative control invalid"
    );
    let calibration = decode_pixels(&calibration);
    let red_in = |left: u32| {
        calibration
            .enumerate_pixels()
            .filter(|(x, y, p)| {
                (*x >= left && *x < left + 160 && *y >= 100 && *y < 148)
                    && p[0] > 220
                    && p[1] < 30
                    && p[2] < 30
            })
            .count()
    };
    assert!(
        red_in(280) > 500 && red_in(40) == 0,
        "INVALID: calibration did not prove sensitive pixels moved from A to B"
    );
    arm_animation(backend, &fixture).await;
    fixture.mask_delay.store(true, Ordering::Release);
    let masked = backend.screenshot().await;
    fixture.mask_delay.store(false, Ordering::Release);
    let masked = masked.expect("mask-race screenshot must succeed without raw error output");
    assert!(
        !masked.fully_masked(),
        "a permanent black viewport is not visual acceptance"
    );
    let (red, blue) = pixel_counts(&masked.png_base64);
    assert!(blue > 1000, "nonsecret visual control must remain visible");
    let at_a = backend.fixture_eval("(()=>{const r=document.getElementById('secret').getBoundingClientRect();return Math.abs(r.x-40)<1})()").await.unwrap();
    assert_eq!(at_a, true, "the page must return to A after capture");
    assert_eq!(
        red, 0,
        "sensitive pixels escaped while CSS moved A to B to A"
    );
}

async fn arm_animation(backend: &LocalBrowserBackend, fixture: &Fixture) {
    let earliest = std::time::Instant::now();
    assert_eq!(
        backend
            .fixture_eval("document.getElementById('arm').click(); true")
            .await
            .unwrap(),
        true
    );
    let latest = std::time::Instant::now();
    *fixture.mask_armed.lock().unwrap() = Some((earliest, latest));
}

fn decode_pixels(png: &str) -> image::RgbImage {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png)
        .expect("synthetic PNG encoding");
    let pixels = image::load_from_memory(&bytes)
        .expect("synthetic PNG decoding")
        .to_rgb8();
    assert_eq!(pixels.dimensions(), (800, 600));
    pixels
}

fn pixel_counts(png: &str) -> (usize, usize) {
    let pixels = decode_pixels(png);
    pixels.pixels().fold((0, 0), |(red, blue), pixel| {
        (
            red + usize::from(pixel[0] > 220 && pixel[1] < 30 && pixel[2] < 30),
            blue + usize::from(pixel[2] > 220 && pixel[0] < 30 && pixel[1] < 30),
        )
    })
}

async fn run_native_fixture(mask_probe: bool) {
    let home = std::path::PathBuf::from(
        std::env::var_os("DUDUCLAW_HOME").expect("fresh fixture home required"),
    );
    assert_eq!(
        std::env::var_os("HOME"),
        Some(home.clone().into_os_string())
    );
    assert_eq!(
        std::fs::read_to_string(home.join("b9b-native-fixture-owned-root")).unwrap(),
        "synthetic-only\n"
    );
    let home = home.canonicalize().unwrap();
    assert!(
        home.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ddc-b9b-native-")
    );
    std::fs::create_dir_all(home.join("agents/alice")).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[computer_use.local_browser]\nenabled=true\n",
    )
    .unwrap();
    std::fs::write(home.join("agents/alice/agent.toml"),"[capabilities]\ncomputer_use=true\n[capabilities.computer_use_config]\nlocal_browser=true\nallowed_domains=['allowed.example']\n").unwrap();
    let companion = Companion::new(&home);
    let key = [0x91; 32];
    companion.fixture_key(key);
    native_message(&companion, &key, json!({"op":"poll","presence":present()})).await;
    assert_eq!(
        native_message(&companion, &key, json!({"op":"continue"})).await["ok"],
        true
    );
    let stop = CancellationToken::new();
    let app_stop = stop.clone();
    let app_companion = companion.clone();
    let app = tokio::spawn(async move {
        let mut approvals = 0usize;
        loop {
            if app_stop.is_cancelled() {
                break;
            }
            let report = native_message(
                &app_companion,
                &key,
                json!({"op":"poll","presence":present()}),
            )
            .await;
            if report["approval"].is_object() {
                let card = &report["approval"];
                let answer=native_message(&app_companion,&key,json!({"op":"decision","id":card["id"],"fingerprint":card["fingerprint"],"approve":true})).await;
                assert_eq!(answer["ok"], true);
                approvals += 1;
            }
            tokio::select! {_=app_stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
        approvals
    });
    let config = ComputerUseConfig {
        display_width: 800,
        display_height: 600,
        pinned_hosts: vec![PinnedHost {
            host: "allowed.example".into(),
            ip: "93.184.215.14".parse().unwrap(),
        }],
        ..Default::default()
    };
    let backend = Arc::new(tokio::sync::Mutex::new(LocalBrowserBackend::new(
        home.clone(),
        "alice".into(),
        config.clone(),
        companion.clone(),
    )));
    let udp = Arc::new(
        tokio::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap(),
    );
    let udp_port = udp.local_addr().unwrap().port();
    let packets = Arc::new(AtomicUsize::new(0));
    let observed = packets.clone();
    let receiver = udp.clone();
    let trap = tokio::spawn(async move {
        let mut bytes = [0u8; 2048];
        while receiver.recv_from(&mut bytes).await.is_ok() {
            observed.fetch_add(1, Ordering::AcqRel);
        }
    });
    let sender = tokio::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    sender
        .send_to(b"negative-seam", udp.local_addr().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while packets.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    packets.store(0, Ordering::Release);
    drop(sender);
    let run_backend = backend.clone();
    let run_home = home.clone();
    let run_companion = companion.clone();
    let sentinel = home.join("synthetic-sentinel.txt");
    std::fs::write(&sentinel, "OWNED_ONLY_SENTINEL_NEVER_READ").unwrap();
    let sentinel_url = url::Url::from_file_path(&sentinel).unwrap().to_string();
    let fixture = Arc::new(Fixture {
        fulfilled: AtomicUsize::new(0),
        downloads_canceled: AtomicUsize::new(0),
        downloads_completed: AtomicUsize::new(0),
        file_choosers: AtomicUsize::new(0),
        extra_closed: AtomicUsize::new(0),
        extra_ids: std::sync::Mutex::new(std::collections::HashSet::new()),
        control_download: std::sync::atomic::AtomicBool::new(false),
        mask_delay: std::sync::atomic::AtomicBool::new(false),
        mask_armed: std::sync::Mutex::new(None),
        home: home.clone(),
        children: std::sync::Mutex::new(Vec::new()),
    });
    let run_fixture = fixture.clone();
    let body = tokio::spawn(async move {
        FIXTURE.scope(run_fixture,native_fixture_ingress(async {
            assert!(attended::bind_final_route("webchat:native-synthetic","webchat:native-synthetic","alice","native-synthetic-turn"));
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("webchat:native-synthetic".into()),duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("native-synthetic-turn".into()),async {
                let _turn=turns::register_current_turn("alice").unwrap();
                let proof=attended_call::capture("alice").unwrap();
                attended_call::request_scope(Some(proof),async {
                    assert_eq!(super::machine_lease::path(),run_home.join("owned-machine-lease.lock"));
                    let mut backend=run_backend.lock().await;
                    backend.register("cu-native-synthetic").await.unwrap();backend.start().await.unwrap();
                    if mask_probe {
                        mask_race(&backend).await;
                        return;
                    }
                    assert!(backend.navigate("https://allowed.example/fixture").await.unwrap().ok);
                    assert!(backend.navigate(&sentinel_url).await.is_err());
                    assert!(backend.navigate("data:text/html,OWNED_ONLY_SENTINEL_NEVER_READ").await.is_err());
                    assert!(backend.navigate("chrome://settings/").await.is_err());
                    assert!(backend.navigate("https://127.0.0.1/").await.is_err());
                    assert!(backend.navigate("https://not-allowed.example/").await.is_err());
                    let text=backend.page_text().await.unwrap();assert!(text.contains("Synthetic backend fixture"));assert!(!text.contains("OWNED_ONLY_SENTINEL_NEVER_READ"));
                    let screenshot=backend.screenshot().await.unwrap();assert!(!screenshot.fully_masked());
                    assert!(!screenshot.png_base64.is_empty());
                    let rectangle=backend.fixture_eval("(()=>{const r=document.getElementById('password').getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2]})()").await.unwrap();
                    let pixels=image::load_from_memory(&base64::engine::general_purpose::STANDARD.decode(&screenshot.png_base64).unwrap()).unwrap().to_rgb8();
                    assert_eq!(pixels.dimensions(),(800,600));
                    assert_eq!(pixels.get_pixel(rectangle[0].as_f64().unwrap() as u32,rectangle[1].as_f64().unwrap() as u32).0,[0,0,0]);
                    backend.fixture_eval("document.getElementById('password').focus(); true").await.unwrap();
                    let denial=backend.execute(&crate::computer_use::ComputerAction::Type{text:"SYNTHETIC_BAD_WRITE".into()}).await.unwrap_err();
                    assert!(denial.to_string().contains("local_sensitive_input_refused"),"{denial}");
                    assert_eq!(backend.fixture_eval("document.getElementById('password').value").await.unwrap(),"SYNTHETIC_PASSWORD_NEVER_CAPTURE");
                    let permissions=backend.fixture_eval("navigator.permissions.query({name:'geolocation'}).then(p=>p.state)").await.unwrap();assert_eq!(permissions,"denied");
                    let picker=backend.fixture_eval("showOpenFilePicker().then(()=> 'unexpected-allowed',e=>e.name)").await.unwrap();
                    assert!(matches!(picker.as_str(),Some("SecurityError"|"NotAllowedError")),"read guard must reject before an OS chooser: {picker}");
                    let rtc=format!(r#"(async()=>{{if(!isSecureContext||typeof RTCPeerConnection!=='function'||typeof WebTransport!=='function')throw Error('native constructors required');const candidates=[];const peer=new RTCPeerConnection({{iceServers:[{{urls:'stun:127.0.0.1:{udp_port}'}}]}});peer.onicecandidate=e=>{{if(e.candidate)candidates.push(e.candidate.candidate)}};peer.createDataChannel('synthetic');await peer.setLocalDescription(await peer.createOffer());const transport=new WebTransport('https://127.0.0.1:{udp_port}/');await Promise.race([transport.ready.catch(()=>null),new Promise(r=>setTimeout(r,1200))]);await new Promise(r=>setTimeout(r,1200));transport.close();peer.close();return candidates}})()"#);
                    let candidates=backend.fixture_eval(&rtc).await.unwrap();assert_eq!(candidates,json!([]));
                    let fixture=current().unwrap();
                    // Negative seam: Chrome must be able to write the synthetic
                    // control file into the SAME configured owned directory.
                    backend.fixture_download_policy(true).await.unwrap();
                    backend.fixture_eval("(()=>{const a=document.createElement('a');a.href='/download-control';document.body.append(a);a.click();return true})()").await.unwrap();
                    wait_counter(&fixture.downloads_completed).await;
                    let downloads=run_home.join("local_browser/alice/downloads-disabled");
                    assert_eq!(std::fs::read(downloads.join("synthetic-control.txt")).unwrap(),b"synthetic download control");
                    backend.fixture_download_policy(false).await.unwrap();
                    click(&backend,"download").await.unwrap();
                    wait_counter(&fixture.downloads_canceled).await;
                    assert!(!downloads.join("synthetic.txt").exists());
                    assert!(!run_home.join("Downloads/synthetic.txt").exists());
                    assert!(!run_home.join("local_browser/alice/Downloads/synthetic.txt").exists());
                    backend.stop().await;
                    assert!(fixture.children.lock().unwrap().iter().all(super::processes::OwnedGroup::inactive));
                    // Separate real browser phase because a refused chooser
                    // deliberately ends the session instead of leaving UI open.
                    *backend=LocalBrowserBackend::new(run_home.clone(),"alice".into(),config.clone(),run_companion.clone());
                    backend.register("cu-native-synthetic-upload").await.unwrap();backend.start().await.unwrap();
                    assert!(backend.navigate("https://allowed.example/fixture").await.unwrap().ok);
                    backend.fixture_eval("window.open('about:blank'); true").await.unwrap();
                    wait_counter(&fixture.extra_closed).await;
                    let upload=click(&backend,"upload").await;
                    wait_counter(&fixture.file_choosers).await;
                    if let Err(error)=upload {assert!(error.to_string().contains("local_session")||error.to_string().contains("pipe"),"unexpected chooser-phase error: {error}");}
                    tokio::time::timeout(Duration::from_secs(5),async{while !backend.control().stopped.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
                }).await;
            })).await;
        })).await;
    });
    let mut body = body;
    let result = tokio::time::timeout(Duration::from_secs(120), &mut body).await;
    if result.is_err() {
        body.abort();
        let _ = body.await;
    }
    // Always clean the real owned backend before propagating test failure.
    let cleanup = tokio::time::timeout(Duration::from_secs(20), async {
        backend.lock().await.stop().await;
    })
    .await;
    let owned = fixture.children.lock().unwrap().clone();
    let product_cleanup_verified =
        !owned.is_empty() && owned.iter().all(super::processes::OwnedGroup::inactive);
    let mut children_gone = !owned.is_empty();
    for group in owned {
        children_gone &= group.finish().await;
    }
    stop.cancel();
    let approvals = app.await;
    trap.abort();
    let _ = trap.await;
    drop(udp);
    assert!(cleanup.is_ok(), "owned backend cleanup timed out");
    assert!(
        product_cleanup_verified,
        "fixture fallback must never mask failed product cleanup"
    );
    assert!(children_gone, "owned browser descendants did not all exit");
    assert!(
        result.is_ok_and(|outcome| outcome.is_ok()),
        "native body failed; cleanup completed"
    );
    assert!(approvals.unwrap() >= 2);
    assert!(fixture.fulfilled.load(Ordering::Acquire) >= 1);
    assert_eq!(
        packets.load(Ordering::Acquire),
        0,
        "real RTC/WebTransport traffic must not bypass the proxy"
    );
    assert!(!home.join("audit/browser/screenshots").exists());
    assert_eq!(
        super::machine_lease::status(&home.join("owned-machine-lease.lock")),
        "available",
        "product must clear only the owned fixture barrier after verified process exit"
    );
}
