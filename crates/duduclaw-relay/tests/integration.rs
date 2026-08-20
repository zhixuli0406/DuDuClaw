//! Integration tests: registration, WS challenge-response auth, hook
//! forwarding through a real mock WSS client, offline queueing, rate
//! limiting, and the `/v1/find` device listing.
//!
//! These bind the real axum router to an ephemeral localhost port so the
//! WebSocket upgrade handshake exercises the actual network stack — there
//! is no way to drive a WS upgrade through `tower::Service::call` alone.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use duduclaw_relay::db::RelayDb;
use duduclaw_relay::state::AppState;
use futures_util::{SinkExt, StreamExt};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as WsMessage;

async fn spawn_server() -> (SocketAddr, Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("relay.db");
    let db = RelayDb::open(&db_path).expect("open db");
    let state = AppState::new(db);
    let app = duduclaw_relay::build_router(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    // Give the accept loop a moment to be ready.
    tokio::time::sleep(Duration::from_millis(30)).await;
    (addr, state, dir)
}

fn gen_keypair() -> (Ed25519KeyPair, String) {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let pubkey_b64 = BASE64.encode(key.public_key().as_ref());
    (key, pubkey_b64)
}

async fn register_device(
    addr: SocketAddr,
    client: &reqwest::Client,
    device_id: &str,
    pubkey_b64: &str,
) -> reqwest::Response {
    client
        .post(format!("http://{addr}/v1/device/register"))
        .json(&json!({ "device_id": device_id, "pubkey_b64": pubkey_b64, "name": "測試裝置" }))
        .send()
        .await
        .unwrap()
}

/// Perform the challenge-response handshake against a live WS connection,
/// returning the connected+authenticated stream positioned right after the
/// `ready` frame.
async fn connect_authenticated(
    addr: SocketAddr,
    device_id: &str,
    key: &Ed25519KeyPair,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}/v1/device/ws?device_id={device_id}");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url).await.expect("ws connect");

    let challenge_msg = ws.next().await.expect("challenge frame").unwrap();
    let challenge_json: Value = serde_json::from_str(challenge_msg.to_text().unwrap()).unwrap();
    assert_eq!(challenge_json["type"], json!("challenge"));
    let nonce_b64 = challenge_json["nonce_b64"].as_str().unwrap();
    let nonce = BASE64.decode(nonce_b64).unwrap();

    let sig = key.sign(&nonce);
    let sig_b64 = BASE64.encode(sig.as_ref());
    ws.send(WsMessage::Text(
        json!({ "type": "auth", "signature_b64": sig_b64 }).to_string().into(),
    ))
    .await
    .unwrap();

    let ready_msg = ws.next().await.expect("ready frame").unwrap();
    let ready_json: Value = serde_json::from_str(ready_msg.to_text().unwrap()).unwrap();
    assert_eq!(ready_json["type"], json!("ready"));

    ws
}

#[tokio::test]
async fn register_then_reregister_is_idempotent() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let (_key, pubkey_b64) = gen_keypair();

    let resp = register_device(addr, &client, "box-alpha-01", &pubkey_b64).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["already_registered"], json!(false));

    let resp2 = register_device(addr, &client, "box-alpha-01", &pubkey_b64).await;
    assert_eq!(resp2.status(), 200);
    let body2: Value = resp2.json().await.unwrap();
    assert_eq!(body2["already_registered"], json!(true));
}

#[tokio::test]
async fn register_conflicting_key_is_rejected() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let (_key1, pubkey1) = gen_keypair();
    let (_key2, pubkey2) = gen_keypair();

    let resp = register_device(addr, &client, "box-beta-01", &pubkey1).await;
    assert_eq!(resp.status(), 200);

    let resp2 = register_device(addr, &client, "box-beta-01", &pubkey2).await;
    assert_eq!(resp2.status(), 409);
}

#[tokio::test]
async fn register_rejects_bad_device_id_and_bad_key() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/v1/device/register"))
        .json(&json!({ "device_id": "UPPER CASE!", "pubkey_b64": "AAAA" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let resp2 = client
        .post(format!("http://{addr}/v1/device/register"))
        .json(&json!({ "device_id": "valid-id-1", "pubkey_b64": "not-32-bytes" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 400);
}

#[tokio::test]
async fn ws_auth_fails_for_unregistered_device() {
    let (addr, _state, _dir) = spawn_server().await;
    let url = format!("ws://{addr}/v1/device/ws?device_id=never-registered");
    // Not found before upgrade — connect_async surfaces this as a non-101
    // handshake error.
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
}

#[tokio::test]
async fn ws_auth_fails_with_wrong_signature() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let (_key, pubkey_b64) = gen_keypair();
    let (wrong_key, _wrong_pub) = gen_keypair();
    let device_id = "box-wrongsig-01";
    register_device(addr, &client, device_id, &pubkey_b64).await;

    let url = format!("ws://{addr}/v1/device/ws?device_id={device_id}");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url).await.expect("ws connect");
    let challenge_msg = ws.next().await.unwrap().unwrap();
    let challenge_json: Value = serde_json::from_str(challenge_msg.to_text().unwrap()).unwrap();
    let nonce_b64 = challenge_json["nonce_b64"].as_str().unwrap();
    let nonce = BASE64.decode(nonce_b64).unwrap();

    // Sign with the WRONG key.
    let sig = wrong_key.sign(&nonce);
    let sig_b64 = BASE64.encode(sig.as_ref());
    ws.send(WsMessage::Text(
        json!({ "type": "auth", "signature_b64": sig_b64 }).to_string().into(),
    ))
    .await
    .unwrap();

    // Server must respond with an error frame (and then close), never "ready".
    let next_msg = ws.next().await.unwrap().unwrap();
    let next_json: Value = serde_json::from_str(next_msg.to_text().unwrap()).unwrap();
    assert_eq!(next_json["type"], json!("error"));
}

#[tokio::test]
async fn hook_forwards_live_and_delivers_raw_body() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let (key, pubkey_b64) = gen_keypair();
    let device_id = "box-gamma-01";
    register_device(addr, &client, device_id, &pubkey_b64).await;

    let mut ws = connect_authenticated(addr, device_id, &key).await;

    let body = br#"{"events":[{"type":"message"}]}"#.to_vec();
    let resp = client
        .post(format!("http://{addr}/v1/hook/line/{device_id}"))
        .header("x-line-signature", "sig-from-line")
        .header("content-type", "application/json")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let frame_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("hook frame within timeout")
        .expect("stream not closed")
        .unwrap();
    let frame: Value = serde_json::from_str(frame_msg.to_text().unwrap()).unwrap();
    assert_eq!(frame["type"], json!("hook"));
    assert_eq!(frame["channel"], json!("line"));
    assert_eq!(frame["headers"]["x-line-signature"], json!("sig-from-line"));
    assert_eq!(frame["headers"]["content-type"], json!("application/json"));
    let decoded = BASE64.decode(frame["body_b64"].as_str().unwrap()).unwrap();
    assert_eq!(decoded, body);
}

#[tokio::test]
async fn hook_rejects_unsupported_channel() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/hook/whatsapp/some-device-1"))
        .body(b"x".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn hook_returns_404_for_unknown_device() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/hook/line/never-registered-1"))
        .body(b"x".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn hook_still_returns_200_when_device_offline_and_queues_then_flushes() {
    let (addr, state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let (key, pubkey_b64) = gen_keypair();
    let device_id = "box-delta-01";
    register_device(addr, &client, device_id, &pubkey_b64).await;

    let body = b"queued-payload".to_vec();
    let resp = client
        .post(format!("http://{addr}/v1/hook/line/{device_id}"))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(state.offline_queue.len(device_id).await, 1);

    // Connecting now must flush the queued frame right after `ready`.
    let mut ws = connect_authenticated(addr, device_id, &key).await;
    let frame_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(frame_msg.to_text().unwrap()).unwrap();
    assert_eq!(frame["type"], json!("hook"));
    let decoded = BASE64.decode(frame["body_b64"].as_str().unwrap()).unwrap();
    assert_eq!(decoded, body);
    assert_eq!(state.offline_queue.len(device_id).await, 0);
}

#[tokio::test]
async fn second_connection_evicts_the_first() {
    let (addr, state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let (key, pubkey_b64) = gen_keypair();
    let device_id = "box-evict-01";
    register_device(addr, &client, device_id, &pubkey_b64).await;

    let mut ws1 = connect_authenticated(addr, device_id, &key).await;
    let mut ws2 = connect_authenticated(addr, device_id, &key).await;

    // The first connection must be terminated by the server (evicted).
    // Loop past incidental Ping/Pong control frames. Both a clean Close
    // frame *and* a protocol-level read error settle the assertion: the
    // relay's contract is "the old connection stops working", not a
    // specific close-handshake shape — a server-initiated close racing the
    // peer's close-reply commonly surfaces to the peer as a reset rather
    // than a clean close across WebSocket implementations, which is not a
    // relay-behavior bug.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws1.next().await {
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                other => panic!("unexpected frame on evicted connection: {other:?}"),
            }
        }
    })
    .await
    .expect("evicted connection did not terminate within timeout");

    // The second (current) connection stays registered and reachable.
    assert!(state.registry.is_registered(device_id).await);
    let body = b"still-alive".to_vec();
    client
        .post(format!("http://{addr}/v1/hook/line/{device_id}"))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    let frame_msg = tokio::time::timeout(Duration::from_secs(2), ws2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(frame_msg.to_text().unwrap()).unwrap();
    let decoded = BASE64.decode(frame["body_b64"].as_str().unwrap()).unwrap();
    assert_eq!(decoded, body);
}

#[tokio::test]
async fn hook_rate_limit_returns_429_after_burst() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let (_key, pubkey_b64) = gen_keypair();
    let device_id = "box-epsilon-01";
    register_device(addr, &client, device_id, &pubkey_b64).await;

    let mut saw_429 = false;
    for _ in 0..65 {
        let resp = client
            .post(format!("http://{addr}/v1/hook/line/{device_id}"))
            .body(b"x".to_vec())
            .send()
            .await
            .unwrap();
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "expected a 429 within 65 rapid requests against a 60/min limit");
}

#[tokio::test]
async fn register_rate_limit_returns_429_after_burst() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();

    let mut saw_429 = false;
    for i in 0..15 {
        let (_key, pubkey_b64) = gen_keypair();
        let resp = register_device(addr, &client, &format!("box-burst-{i:02}"), &pubkey_b64).await;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "expected a 429 within 15 rapid registrations against a 10/min limit");
}

#[tokio::test]
async fn find_lists_only_devices_sharing_the_callers_public_ip() {
    let (addr, state, _dir) = spawn_server().await;

    state.db.register_device("box-zeta-01", "k1", Some("辦公室主機")).unwrap();
    state.db.mark_online("box-zeta-01", Some("203.0.113.9")).unwrap();
    state.db.update_lan_ip("box-zeta-01", "192.168.1.50").unwrap();

    state.db.register_device("box-eta-01", "k2", Some("別的地方")).unwrap();
    state.db.mark_online("box-eta-01", Some("198.51.100.7")).unwrap();
    state.db.update_lan_ip("box-eta-01", "192.168.1.51").unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/v1/find"))
        .header("x-forwarded-for", "203.0.113.9")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let html = resp.text().await.unwrap();
    assert!(html.contains("辦公室主機"));
    assert!(html.contains("192.168.1.50"));
    assert!(!html.contains("別的地方"));
}

#[tokio::test]
async fn find_excludes_offline_devices() {
    let (addr, state, _dir) = spawn_server().await;
    state.db.register_device("box-theta-01", "k1", Some("離線裝置")).unwrap();
    state.db.mark_online("box-theta-01", Some("203.0.113.9")).unwrap();
    state.db.update_lan_ip("box-theta-01", "192.168.1.60").unwrap();
    state.db.mark_offline("box-theta-01").unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/v1/find"))
        .header("x-forwarded-for", "203.0.113.9")
        .send()
        .await
        .unwrap();
    let html = resp.text().await.unwrap();
    assert!(!html.contains("離線裝置"));
}

#[tokio::test]
async fn healthz_is_ok() {
    let (addr, _state, _dir) = spawn_server().await;
    let client = reqwest::Client::new();
    let resp = client.get(format!("http://{addr}/healthz")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}
