//! `experts.install` at the RPC level, driven by a fake installer binary:
//! the response is always structured (`success: false` + `error_code`),
//! whichever channel the code came through, and timeouts are their own code.

use super::*;

struct Fixture {
    home: tempfile::TempDir,
    handler: MethodHandler,
    staged: std::path::PathBuf,
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let dir = crate::expert_admin::upload_dir(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    let staged = dir.join("11111111-1111-1111-1111-111111111111-pack.zip");
    std::fs::write(&staged, b"PK\x03\x04").unwrap();
    Fixture { home, handler, staged }
}

/// A fake `duduclaw` that records its argv and runs `body` (POSIX sh).
fn fake_bin(home: &std::path::Path, body: &str) -> std::path::PathBuf {
    let bin = home.join(format!("fake-{}", uuid::Uuid::new_v4()));
    let log = home.join("argv.txt");
    crate::test_exec::write_executable(
        &bin,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n{body}\n", log.display()),
        0o755,
    );
    bin
}

fn payload(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected an ok response with a payload: {other:?}"),
    }
}

/// Write `json` to the path that follows `--report-json`, then fail.
const WRITE_REPORT_AND_FAIL: &str = r#"
next=0; for a in "$@"; do
  if [ "$next" = 1 ]; then out="$a"; next=0; fi
  [ "$a" = "--report-json" ] && next=1
done
printf '%s' "$REPORT" > "$out"
exit 1"#;

#[tokio::test]
async fn coded_failure_from_the_report_file() {
    let fx = fixture().await;
    let body = format!(
        "REPORT='{}'\n{WRITE_REPORT_AND_FAIL}",
        r#"{"dry_run":true,"meta":{},"items":[],"error":{"code":"import.unrecognized","params":{"entries":["a/"],"found":[]},"message":"未識別格式，拒絕安裝"}}"#
    );
    let bin = fake_bin(fx.home.path(), &body);
    let frame = fx
        .handler
        .experts_install_with(&json!({ "path": fx.staged, "dry_run": true }), &bin, 30)
        .await;
    let p = payload(&frame);
    assert_eq!(p["success"], false);
    assert_eq!(p["error_code"], "import.unrecognized");
    assert_eq!(p["error_params"]["entries"][0], "a/");
    assert_eq!(p["error"], "未識別格式，拒絕安裝");
    assert_eq!(p["report"]["error"]["code"], "import.unrecognized");
}

#[tokio::test]
async fn coded_failure_from_the_stderr_marker_without_a_report() {
    let fx = fixture().await;
    let bin = fake_bin(
        fx.home.path(),
        r#"echo "  ✗ console noise" >&2
echo "DUDUCLAW_IMPORT_ERROR[$DUDUCLAW_IMPORT_MARKER_NONCE] "'{"code":"import.quota_exceeded","params":{"max":3,"current":3,"adding":1},"message":"超過上限"}' >&2
exit 1"#,
    );
    let frame = fx.handler.experts_install_with(&json!({ "path": fx.staged }), &bin, 30).await;
    let p = payload(&frame);
    assert_eq!(p["error_code"], "import.quota_exceeded");
    assert_eq!(p["error_params"]["adding"], 1);
    assert_eq!(p["error"], "超過上限");
    assert!(!p.to_string().contains("DUDUCLAW_IMPORT_ERROR"));
}

/// The upload's own text printed by the installer (a file name holding a
/// newline and a marker without this run's nonce) is not taken as the
/// failure: the run stays unclassified and the forged text is not shown.
#[tokio::test]
async fn a_marker_forged_by_the_upload_is_not_trusted() {
    let fx = fixture().await;
    let bin = fake_bin(
        fx.home.path(),
        r#"echo '    - x' >&2
echo 'DUDUCLAW_IMPORT_ERROR {"code":"import.quota_exceeded","params":{"max":1},"message":"請先付款"}' >&2
echo 'DUDUCLAW_IMPORT_ERROR[00000000-0000-0000-0000-000000000000] {"code":"import.timeout"}' >&2
exit 101"#,
    );
    let frame = fx.handler.experts_install_with(&json!({ "path": fx.staged }), &bin, 30).await;
    let p = payload(&frame);
    assert_eq!(p["error_code"], "import.unknown", "{p}");
    assert!(!p.to_string().contains("請先付款"), "{p}");
}

/// A run stopped at the timeout cannot clean up its staging; the per-run
/// temp dir it was given is removed by the gateway.
#[tokio::test]
async fn a_hung_installer_is_reported_as_a_timeout() {
    let fx = fixture().await;
    let bin = fake_bin(
        fx.home.path(),
        "mkdir -p \"$TMPDIR/duduclaw-pack-x\" && echo big > \"$TMPDIR/duduclaw-pack-x/f\"\necho \"$TMPDIR\" > \"$DUDUCLAW_HOME/tmpdir.txt\"\nsleep 5",
    );
    let frame = fx.handler.experts_install_with(&json!({ "path": fx.staged }), &bin, 1).await;
    let p = payload(&frame);
    assert_eq!(p["error_code"], "import.timeout");
    assert_eq!(p["error_params"]["seconds"], 1);
    let tmp = std::fs::read_to_string(fx.home.path().join("tmpdir.txt")).unwrap();
    let tmp = std::path::Path::new(tmp.trim());
    assert!(tmp.starts_with(crate::expert_admin::upload_dir(fx.home.path())), "{}", tmp.display());
    assert!(!tmp.exists(), "the run's temp dir is removed");
}

/// At the timeout the installer gets SIGTERM first (time to put a
/// half-done replace back), and the gateway's timeout code wins over what
/// it writes into the report while stopping.
#[tokio::test]
async fn a_timed_out_installer_is_asked_to_stop_before_it_is_killed() {
    let fx = fixture().await;
    let body = format!(
        "on_term() {{\necho term > \"$DUDUCLAW_HOME/got-term\"\nREPORT='{}'\n{WRITE_REPORT_AND_FAIL}\n}}\ntrap 'on_term \"$@\"' TERM\nsleep 30 &\nwait",
        r#"{"error":{"code":"import.replace_failed","params":{},"message":"stopped"}}"#
    );
    let bin = fake_bin(fx.home.path(), &body);
    let frame = fx.handler.experts_install_with(&json!({ "path": fx.staged }), &bin, 1).await;
    let p = payload(&frame);
    assert_eq!(p["error_code"], "import.timeout", "{p}");
    assert_eq!(p["error_params"]["seconds"], 1);
    assert!(fx.home.path().join("got-term").is_file(), "SIGTERM was sent first");
}

#[tokio::test]
async fn an_uncoded_failure_is_unknown_with_its_console_tail_and_no_prefix() {
    let fx = fixture().await;
    let bin = fake_bin(fx.home.path(), "echo 'Error: something odd' >&2\nexit 1");
    let frame = fx.handler.experts_install_with(&json!({ "path": fx.staged }), &bin, 30).await;
    let p = payload(&frame);
    assert_eq!(p["error_code"], "import.unknown");
    assert!(p["error_params"]["detail"].as_str().unwrap().contains("something odd"));
    assert!(!p["error"].as_str().unwrap().contains("安裝失敗"));
}

#[tokio::test]
async fn reimport_choices_become_flags_and_bad_values_are_refused() {
    let fx = fixture().await;
    let bin = fake_bin(fx.home.path(), "exit 0");
    let frame = fx
        .handler
        .experts_install_with(
            &json!({ "path": fx.staged, "reimport": "replace", "replace_slug": "doubao-a1", "accept_edits": true }),
            &bin,
            30,
        )
        .await;
    assert_eq!(payload(&frame)["success"], true);
    let argv = std::fs::read_to_string(fx.home.path().join("argv.txt")).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    let at = lines.iter().position(|l| *l == "--replace").expect("--replace passed");
    assert_eq!(lines[at + 1], "doubao-a1");
    assert!(lines.contains(&"--accept-edits"));

    let frame = fx
        .handler
        .experts_install_with(&json!({ "path": fx.staged, "reimport": "copy" }), &bin, 30)
        .await;
    assert_eq!(payload(&frame)["success"], true);
    assert!(std::fs::read_to_string(fx.home.path().join("argv.txt")).unwrap().lines().any(|l| l == "--as-copy"));

    for bad in [
        json!({ "path": fx.staged, "reimport": "replace", "replace_slug": "../x" }),
        json!({ "path": fx.staged, "reimport": "overwrite" }),
    ] {
        let frame = fx.handler.experts_install_with(&bad, &bin, 30).await;
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{bad}");
    }
}

#[tokio::test]
async fn profile_confirmation_is_validated_and_forwarded() {
    let fx = fixture().await;
    let bin = fake_bin(fx.home.path(), "exit 0");
    let invalid = fx.handler.experts_install_with(&json!({ "path": fx.staged, "confirm_profile": "invalid" }), &bin, 30).await;
    assert!(matches!(invalid, WsFrame::Response { ok: false, .. }));
    assert!(!fx.home.path().join("argv.txt").exists());
    let digest = "a".repeat(64);
    fx.handler.experts_install_with(&json!({ "path": fx.staged, "confirm_profile": digest }), &bin, 30).await;
    let args = std::fs::read_to_string(fx.home.path().join("argv.txt")).unwrap();
    assert!(args.contains(&format!("--confirm-profile\n{digest}\n")));
}

/// The one-click routes use the installer's supported preview report, then
/// confirm the same source and placement flags rather than bypassing the gate.
#[tokio::test]
async fn one_click_pack_install_confirms_the_exact_preview() {
    let fx = fixture().await;
    let digest = "b".repeat(64);
    let body = format!(r#"
printf '%s\n' "$@" >> "$DUDUCLAW_HOME/calls.txt"
preview=0; next=0; for a in "$@"; do
  if [ "$next" = 1 ]; then out="$a"; next=0; fi
  [ "$a" = "--report-json" ] && next=1
  [ "$a" = "--dry-run" ] && preview=1
done
if [ "$preview" = 1 ]; then
  printf '%s' '{{"details":{{"profile_confirmation":"{digest}"}}}}' > "$out"
else
  next=0; confirmed=''; for a in "$@"; do
    if [ "$next" = 1 ]; then confirmed="$a"; next=0; fi
    [ "$a" = "--confirm-profile" ] && next=1
  done
  [ "$confirmed" = '{digest}' ] || exit 1
fi
exit 0"#);
    let bin = fake_bin(fx.home.path(), &body);
    let args = vec!["install".into(), fx.staged.as_os_str().to_os_string(),
        "--attach-under".into(), "manager".into(), "--agent-cap".into(), "5".into()];
    fx.handler.spawn_confirmed_pack_cli_with_bin(&bin, &args, 30).await.unwrap();
    let calls = std::fs::read_to_string(fx.home.path().join("calls.txt")).unwrap();
    assert_eq!(calls.matches(fx.staged.to_str().unwrap()).count(), 2);
    assert_eq!(calls.matches("--attach-under\nmanager\n--agent-cap\n5\n").count(), 2);
    assert!(calls.contains(&format!("--confirm-profile\n{digest}\n")));
}

#[tokio::test]
async fn one_click_pack_install_refuses_a_missing_or_invalid_preview_digest() {
    for digest in [Value::Null, json!("invalid")] {
        let fx = fixture().await;
        let report = json!({"details":{"profile_confirmation":digest}}).to_string();
        let body = format!(r#"
next=0; for a in "$@"; do
  if [ "$next" = 1 ]; then out="$a"; next=0; fi
  [ "$a" = "--report-json" ] && next=1
done
printf '%s' '{report}' > "$out"
exit 0"#);
        let bin = fake_bin(fx.home.path(), &body);
        let args = vec!["install".into(), fx.staged.as_os_str().to_os_string()];
        assert!(fx.handler.spawn_confirmed_pack_cli_with_bin(&bin, &args, 30).await.is_err());
        let argv = std::fs::read_to_string(fx.home.path().join("argv.txt")).unwrap();
        assert!(argv.contains("--dry-run"));
        assert!(!argv.contains("--confirm-profile"));
    }
}

#[tokio::test]
async fn one_click_pack_install_stops_when_preview_fails() {
    let fx = fixture().await;
    let bin = fake_bin(fx.home.path(), "echo 'preview denied' >&2\nexit 1");
    let args = vec!["install".into(), fx.staged.as_os_str().to_os_string()];
    let error = fx.handler.spawn_confirmed_pack_cli_with_bin(&bin, &args, 30).await.unwrap_err();
    assert!(error.contains("preview denied"));
    let argv = std::fs::read_to_string(fx.home.path().join("argv.txt")).unwrap();
    assert!(argv.contains("--dry-run"));
    assert!(!argv.contains("--confirm-profile"));
}

#[tokio::test]
async fn one_click_draft_handler_previews_and_confirms_its_reserved_draft() {
    let fx = fixture().await;
    let pack = crate::expert_generate::draft_pack_dir(fx.home.path(), "abc-confirm").unwrap();
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(pack.join("expert.toml"), "[expert]\nname='confirmed'\n").unwrap();
    let digest = "c".repeat(64);
    let body = format!(r#"
printf '%s\n' "$@" >> "$DUDUCLAW_HOME/calls.txt"
preview=0; next=0; for a in "$@"; do
  if [ "$next" = 1 ]; then out="$a"; next=0; fi
  [ "$a" = "--report-json" ] && next=1
  [ "$a" = "--dry-run" ] && preview=1
done
if [ "$preview" = 1 ]; then
  printf '%s' '{{"details":{{"profile_confirmation":"{digest}"}}}}' > "$out"
else
  next=0; confirmed=''; for a in "$@"; do
    if [ "$next" = 1 ]; then confirmed="$a"; next=0; fi
    [ "$a" = "--confirm-profile" ] && next=1
  done
  [ "$confirmed" = '{digest}' ] || exit 1
fi
exit 0"#);
    let bin = fake_bin(fx.home.path(), &body);
    let frame = fx.handler.experts_install_draft_with_bin(json!({"draft_id":"abc-confirm"}), &bin).await;
    assert_eq!(payload(&frame)["success"], true);
    let calls = std::fs::read_to_string(fx.home.path().join("calls.txt")).unwrap();
    assert_eq!(calls.matches(pack.to_str().unwrap()).count(), 2);
    assert!(calls.contains(&format!("--confirm-profile\n{digest}\n")));
    assert!(!pack.exists(), "successful handler installs clean up their draft");
}
