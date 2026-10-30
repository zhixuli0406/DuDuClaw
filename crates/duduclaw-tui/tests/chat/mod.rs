//! Bind-based acceptance tests. NOT RUN when the execution environment refuses sockets.
use super::*;
pub(super) async fn upgrade(
    ws: WebSocketUpgrade,
    State(f): State<Arc<Fixture>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| serve_chat(socket, f))
}
struct Active(Arc<Fixture>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.chat_active.fetch_sub(1, Ordering::SeqCst);
    }
}
async fn serve_chat(mut socket: WebSocket, f: Arc<Fixture>) {
    let active = f.chat_active.fetch_add(1, Ordering::SeqCst) + 1;
    f.chat_max_active.fetch_max(active, Ordering::SeqCst);
    let _active = Active(f.clone());
    let mut controls = f.events.subscribe();
    let Some(Ok(Message::Text(wire))) = socket.recv().await else {
        return;
    };
    let value: Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(value["type"], "auth");
    let payload = value["token"].as_str().unwrap().split('.').nth(1).unwrap();
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    if claims["refreshed"] == true {
        f.chat_refreshed.fetch_add(1, Ordering::SeqCst);
    }
    let generation = f.chat_connections.fetch_add(1, Ordering::SeqCst) + 1;
    if socket.send(Message::Text(json!({"type":"session_info","session_id":format!("webchat:webchat:owner:{generation}"),
        "agent_name":"adu","agent_icon":"","supports_vision":false,"model":"fixture"}).to_string().into())).await.is_err() { return; }
    loop {
        tokio::select! {
            control = controls.recv() => if let Ok(Control::Close) = control {
                let _ = socket.send(Message::Close(None)).await; return;
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Text(wire))) => {
                    let value: Value = serde_json::from_str(&wire).unwrap();
                    f.chat_sent.lock().unwrap().push(value.clone());
                    let mut frames = Vec::new();
                    if value["type"] == "user_message" {
                        frames.push(json!({"type":"step","phase":"start","tool":"Read","summary":"file","depth":0,"ts":1,
                            "conv":value["conv"],"msg_id":value["msg_id"],"session_id":"webchat:webchat:owner:effective"}));
                        if value["content"] != "hold" {
                            frames.push(json!({"type":"assistant_done","content":"reply\u{1b}]52;c;bad\u{7}","tokens_used":1,
                                "conv":value["conv"],"msg_id":value["msg_id"]}));
                        }
                    } else if value["type"] == "cancel" {
                        frames.push(json!({"type":"cancel_accepted","msg_id":value["msg_id"]}));
                        frames.push(json!({"type":"cancelled","msg_id":value["msg_id"]}));
                    } else { panic!("unexpected chat frame"); }
                    for frame in frames {
                        if socket.send(Message::Text(frame.to_string().into())).await.is_err() { return; }
                    }
                }
                Some(Ok(Message::Ping(bytes))) => { if socket.send(Message::Pong(bytes)).await.is_err() { return; } }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {},
            }
        }
    }
}
async fn chat_until(
    state: &mut watch::Receiver<duduclaw_tui::chat::ChatState>,
    predicate: impl Fn(&duduclaw_tui::chat::ChatState) -> bool,
) {
    timeout(Duration::from_secs(6), async {
        loop {
            if predicate(&state.borrow_and_update()) {
                return;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("chat state deadline");
}
#[tokio::test]
async fn one_chat_socket_reconnect_refresh_cancel_and_no_replay() {
    let Some(gateway) = Gateway::start(false).await else {
        return;
    };
    let client = gateway.client().await;
    connected(&client, 1).await;
    let chat = client.chat();
    let other = client.clone().chat();
    let mut state = chat.state();
    chat_until(&mut state, |s| s.connected).await;
    assert!(other.state().borrow().connected);
    chat.send(duduclaw_tui::chat::submission("hold", Some("adu")).unwrap())
        .unwrap();
    chat_until(&mut state, |s| s.can_cancel()).await;
    chat.cancel().unwrap();
    chat_until(&mut state, |s| {
        !s.running && s.status.is(duduclaw_tui::strings::CHAT_CANCELLED)
    })
    .await;
    gateway.fixture.events.send(Control::Close).unwrap();
    connected(&client, 2).await;
    timeout(Duration::from_secs(6), async {
        while gateway.fixture.chat_connections.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    chat_until(&mut state, |s| s.connected).await;
    assert_eq!(
        gateway.fixture.chat_sent.lock().unwrap().len(),
        2,
        "never replay sends/cancels"
    );
    let _ = client
        .call("expire", json!({}), Duration::from_secs(2))
        .await;
    timeout(Duration::from_secs(6), async {
        while gateway.fixture.chat_refreshed.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    chat_until(&mut state, |s| s.connected).await;
    chat.send(duduclaw_tui::chat::submission("你好", Some("adu")).unwrap())
        .unwrap();
    chat_until(&mut state, |s| {
        !s.running && s.lines.iter().any(|line| line.plain().contains("reply"))
    })
    .await;
    assert_eq!(gateway.fixture.chat_max_active.load(Ordering::SeqCst), 1);
    assert!(
        state
            .borrow()
            .lines
            .iter()
            .all(|line| !line.plain().contains('\x1b'))
    );
}
#[tokio::test]
async fn watch_falls_back_to_real_poll_shapes_without_screen_control() {
    let Some(gateway) = Gateway::start(false).await else {
        return;
    };
    let client = gateway.client().await;
    connected(&client, 1).await;
    let mut output = Vec::new();
    let result = timeout(
        Duration::from_millis(250),
        duduclaw_tui::watch::run_watch_to(
            duduclaw_tui::WatchOptions {
                client,
                agent: "adu".into(),
            },
            &mut output,
        ),
    )
    .await;
    assert!(result.is_err());
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains(duduclaw_tui::strings::WATCH_POLLING));
    assert!(text.contains("done"));
    assert!(!text.contains('\x1b'));
}
