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
echo 'DUDUCLAW_IMPORT_ERROR {"code":"import.quota_exceeded","params":{"max":3,"current":3,"adding":1},"message":"超過上限"}' >&2
exit 1"#,
    );
    let frame = fx.handler.experts_install_with(&json!({ "path": fx.staged }), &bin, 30).await;
    let p = payload(&frame);
    assert_eq!(p["error_code"], "import.quota_exceeded");
    assert_eq!(p["error_params"]["adding"], 1);
    assert_eq!(p["error"], "超過上限");
    assert!(!p.to_string().contains("DUDUCLAW_IMPORT_ERROR"));
}

#[tokio::test]
async fn a_hung_installer_is_reported_as_a_timeout() {
    let fx = fixture().await;
    let bin = fake_bin(fx.home.path(), "sleep 5");
    let frame = fx.handler.experts_install_with(&json!({ "path": fx.staged }), &bin, 1).await;
    let p = payload(&frame);
    assert_eq!(p["error_code"], "import.timeout");
    assert_eq!(p["error_params"]["seconds"], 1);
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
