//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! WP4 dashboard RPCs: `ticks.sources` / `ticks.recent`. Mirrors the
//! `MethodHandler::new(root).await` + `handler.handle_xxx(...).await`
//! harness used throughout this file (see `evolution_v3_dashboard_tests`).
use super::*;

fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        WsFrame::Response {
            ok: false, error, ..
        } => {
            panic!("RPC returned an error frame: {error:?}")
        }
        other => panic!("unexpected frame shape: {other:?}"),
    }
}

// ── ticks.sources ─────────────────────────────────────────

#[tokio::test]
async fn sources_with_no_config_and_no_hub_is_an_empty_not_missing_list() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = payload(&handler.handle_ticks_sources().await);
    assert_eq!(p["enabled"], false);
    assert!(p["sources"].as_array().unwrap().is_empty());
    assert_eq!(p["screen"]["pass"], 0);
    assert_eq!(p["screen"]["drop"], 0);
    assert_eq!(p["screen"]["unavailable"], 0);
}

#[tokio::test]
async fn sources_lists_configured_sources_with_live_counters() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "twse-2330"
            kind = "http_poll"
            enabled = true
            interval_secs = 10
            url = "https://example.com/quote"
            "#,
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let hub = Arc::new(crate::tick_source::TickHub::new());
    hub.record_emit("twse-2330").await;
    hub.record_emit("twse-2330").await;
    hub.record_drop("twse-2330", crate::tick_source::DropReason::RateCap)
        .await;
    hub.record_screen_outcome("pass");
    hub.record_screen_outcome("drop");
    handler.set_tick_hub(hub).await;

    let p = payload(&handler.handle_ticks_sources().await);
    assert_eq!(p["enabled"], true);
    let sources = p["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1);
    let s0 = &sources[0];
    assert_eq!(s0["id"], "twse-2330");
    assert_eq!(s0["kind"], "http_poll");
    assert_eq!(s0["enabled"], true);
    assert_eq!(s0["events_emitted_total"], 2);
    assert_eq!(s0["dropped"]["rate_cap"], 1);
    assert_eq!(s0["dropped"]["unchanged"], 0);
    // D5-W — the websocket-only reason is still reported (as 0) for a
    // polling source, so the dashboard can sum a fixed set of keys.
    assert_eq!(s0["dropped"]["non_text"], 0);
    // F3 — same contract for the field-less-frame reason: always present,
    // so `droppedTotal` on the dashboard never has to guess a key.
    assert_eq!(s0["dropped"]["no_fields"], 0);
    assert_eq!(
        s0["dropped"].as_object().unwrap().len(),
        6,
        "the dropped breakdown is a fixed six-key shape: {:?}",
        s0["dropped"]
    );
    assert!(s0["last_tick_ts"].is_string());
    assert_eq!(p["screen"]["pass"], 1);
    assert_eq!(p["screen"]["drop"], 1);
    assert_eq!(p["screen"]["unavailable"], 0);
}

#[tokio::test]
async fn sources_shows_a_disabled_source_with_zero_counts() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
            [tick]
            enabled = false
            [[tick.sources]]
            id = "quiet"
            kind = "http_poll"
            enabled = false
            url = "https://example.com/quote"
            "#,
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = payload(&handler.handle_ticks_sources().await);
    assert_eq!(p["enabled"], false, "master switch reflected");
    let sources = p["sources"].as_array().unwrap();
    assert_eq!(
        sources.len(),
        1,
        "a configured-but-inactive source is still listed, not hidden"
    );
    assert_eq!(sources[0]["enabled"], false);
    assert_eq!(sources[0]["events_emitted_total"], 0);
    assert!(sources[0]["last_tick_ts"].is_null());
}

/// D5-W2 — a source's custom headers are credentials. The RPC reports how
/// many there are and nothing else: no name, and above all no value.
#[tokio::test]
async fn sources_reports_a_header_count_and_never_a_header_value() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "secured"
            kind = "http_poll"
            url = "https://example.com/quote"
            headers = { "X-Api-Key" = "sk-live-DO-NOT-LEAK", "Accept" = "application/json" }
            [[tick.sources]]
            id = "plain"
            kind = "http_poll"
            url = "https://example.com/other"
            "#,
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = payload(&handler.handle_ticks_sources().await);

    let sources = p["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["headers_count"], 2, "{sources:?}");
    assert_eq!(
        sources[1]["headers_count"], 0,
        "the key is always present so the UI never has to branch"
    );

    // The whole response, serialized — the value must not appear anywhere
    // in it, under any key.
    let body = p.to_string();
    assert!(
        !body.contains("sk-live"),
        "a header value reached the dashboard RPC: {body}"
    );
    assert!(
        !body.contains("X-Api-Key"),
        "even the header name stays in the config file: {body}"
    );
}

// ── ticks.recent ──────────────────────────────────────────

#[tokio::test]
async fn recent_requires_a_source_param() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler.handle_ticks_recent(json!({})).await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "{frame:?}"
    );
}

#[tokio::test]
async fn recent_returns_empty_records_when_the_hub_was_never_wired() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = payload(
        &handler
            .handle_ticks_recent(json!({ "source": "twse-2330" }))
            .await,
    );
    assert_eq!(p["source"], "twse-2330");
    assert!(p["records"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn recent_returns_buffered_records_oldest_first() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let hub = Arc::new(crate::tick_source::TickHub::new());
    for i in 0..5 {
        hub.push(
            "twse-2330",
            crate::tick_source::TickRecord {
                ts: format!("t{i}"),
                fields: json!({ "price": 100 + i }).as_object().cloned().unwrap(),
                raw: None,
            },
        )
        .await;
    }
    handler.set_tick_hub(hub).await;

    let p = payload(
        &handler
            .handle_ticks_recent(json!({ "source": "twse-2330", "limit": 3 }))
            .await,
    );
    let records = p["records"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["ts"], "t2", "oldest of the last 3");
    assert_eq!(records[2]["ts"], "t4", "newest last");
    assert_eq!(records[2]["fields"]["price"], 104);
}

#[tokio::test]
async fn recent_limit_is_capped_even_if_the_caller_asks_for_more() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let hub = Arc::new(crate::tick_source::TickHub::new());
    for i in 0..80 {
        hub.push(
            "s1",
            crate::tick_source::TickRecord {
                ts: format!("t{i}"),
                fields: serde_json::Map::new(),
                raw: None,
            },
        )
        .await;
    }
    handler.set_tick_hub(hub).await;
    let p = payload(
        &handler
            .handle_ticks_recent(json!({ "source": "s1", "limit": 500 }))
            .await,
    );
    assert_eq!(
        p["records"].as_array().unwrap().len(),
        crate::tick_source::TICK_RECENT_RPC_LIMIT
    );
}
