//! P2-C real-Docker workspace tests (design §10.2, §10.3, §6.4), opt-in:
//! `DUDU_COMPUTER_USE_IMAGE=duduclaw-computer-use:latest cargo test -p duduclaw-gateway --lib
//! --no-default-features -- --ignored --test-threads=1 computer_workspace --nocapture`.
//!
//! Isolation rules: every test has a fresh home (so its own home label); only
//! containers carrying that label are touched, only with `docker rm -f <id>`;
//! nothing is pruned, no `-v`. Each test compares `duduclaw-lwm-exp` before
//! and after and fails loudly if it changed. Measurements print as `MEASURE`
//! lines; a test that cannot run here prints `BLOCKED_ENV(...)` and fails.
#![cfg(unix)]

use super::*;
use crate::computer_workspaces::{self as cw, WorkspaceStore};

fn image() -> String {
    std::env::var("DUDU_COMPUTER_USE_IMAGE").expect("set DUDU_COMPUTER_USE_IMAGE")
}

async fn docker(args: &[&str]) -> (bool, String, String) {
    match crate::computer_use_orchestrator::docker_output(args, Duration::from_secs(60), "test")
        .await
    {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
        Err(e) => (false, String::new(), e.to_string()),
    }
}

/// `duduclaw-lwm-exp` fingerprint (or "absent").
async fn lwm() -> String {
    let (ok, out, _) = docker(&[
        "inspect",
        "-f",
        "{{.Id}} {{.State.Running}} {{.State.StartedAt}}",
        "duduclaw-lwm-exp",
    ])
    .await;
    if ok { out } else { "absent".into() }
}

/// A fresh home with the feature on for alice and bob; `under_user_home`
/// puts it below `$HOME` (where a real `~/.duduclaw` lives).
fn ws_home(under_user_home: bool, extra_caps: &str) -> tempfile::TempDir {
    let tmp = if under_user_home {
        let base = std::path::PathBuf::from(std::env::var("HOME").expect("HOME"));
        tempfile::Builder::new()
            .prefix(".p2c-ws-test-")
            .tempdir_in(base)
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    write_config(
        tmp.path(),
        &format!(
            "[computer_use]\nimage = \"{}\"\n[computer_use.workspaces]\nenabled = true\nmin_free_bytes = 0\n",
            image()
        ),
    );
    duduclaw_core::ensure_identity_key(tmp.path()).unwrap();
    for a in ["alice", "bob"] {
        write_agent(
            tmp.path(),
            a,
            &format!(
                "[capabilities]\ncomputer_use = true\n[capabilities.computer_use_config]\nworkspace = true\n{extra_caps}"
            ),
        );
    }
    tmp
}

fn mgr(home: &std::path::Path, idle: Duration) -> ComputerUseSessions {
    ComputerUseSessions::with_parts(
        home.to_path_buf(),
        super::super::backend::orchestrator_factory(),
        idle,
    )
}

fn ws(spec: &str) -> StartRequest {
    StartRequest {
        workspace: Some(spec.into()),
        ..Default::default()
    }
}

/// Container ids carrying this home's label.
async fn ours(home: &std::path::Path) -> Vec<String> {
    let s = super::containers_of(home).await;
    s.lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect()
}

/// Remove only this home's containers, by id.
async fn cleanup(home: &std::path::Path) {
    for id in ours(home).await {
        let _ = docker(&["rm", "-f", &id]).await;
    }
}

fn data_file(home: &std::path::Path, id: &str, rel: &str) -> std::path::PathBuf {
    home.canonicalize()
        .unwrap()
        .join("computer_workspaces")
        .join(id)
        .join("data")
        .join(rel)
}

fn sha(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Run `body`, then clean up, then check `duduclaw-lwm-exp` and that no
/// container of this home is left; the body's error (if any) is raised last.
async fn guarded<F: std::future::Future<Output = Result<(), String>>>(
    home: &std::path::Path,
    body: F,
) {
    let before = lwm().await;
    let outcome = body.await;
    cleanup(home).await;
    let after = lwm().await;
    let left = ours(home).await;
    eprintln!("MEASURE lwm before={before} after={after}");
    assert_eq!(
        before, after,
        "STOP: duduclaw-lwm-exp changed during the test"
    );
    assert!(left.is_empty(), "containers of this home left: {left:?}");
    if let Err(e) = outcome {
        panic!("{e}");
    }
}

/// H8 main line, home under `$HOME`: start(new) → write → same hash inside
/// the container → idle reap (compute gone, data kept) → new manager
/// (restart) → start(same id) → same hash in container and through the tool.
/// Also checks that the browser's `sandbox` user cannot read the workspace
/// (root-only tmpfs parent), root can and the hash matches, and the
/// read-only rootfs and existing flags are intact.
#[tokio::test]
#[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn computer_workspace_real_h8_survives_reap_and_restart() {
    let tmp = ws_home(true, "");
    let home = tmp.path().to_path_buf();
    guarded(&home, async {
        let m = mgr(&home, Duration::from_secs(2));
        let started = m
            .start("alice", ws("new"))
            .await
            .map_err(|e| format!("start: {e:?}"))?;
        let id = started["workspace_id"]
            .as_str()
            .ok_or("no workspace_id")?
            .to_string();
        eprintln!("MEASURE mount_under_user_home=ok home={}", home.display());
        let body = "# 週報\n- item\n";
        let w = m
            .workspace_write("alice", &id, "report.md", body, None)
            .await
            .map_err(|e| format!("write: {e:?}"))?;
        let c = ours(&home).await.first().cloned().ok_or("no container")?;
        let (ok, out, err) =
            docker(&["exec", &c, "sha256sum", "/workspace/files/report.md"]).await;
        if !ok || !out.starts_with(w["sha256"].as_str().unwrap_or("?")) {
            return Err(format!("in-container hash: {ok} {out} {err}"));
        }
        let (ok_s, out_s, err_s) =
            docker(&["exec", "-u", "sandbox", &c, "cat", "/workspace/files/report.md"]).await;
        let (ok_ls, _, _) = docker(&["exec", "-u", "sandbox", &c, "ls", "/workspace"]).await;
        eprintln!(
            "MEASURE sandbox_user_can_read_workspace={ok_s} can_list_parent={ok_ls} stdout_len={} stderr={err_s:?}",
            out_s.len()
        );
        if ok_s || ok_ls || !out_s.is_empty() {
            return Err(format!("sandbox user reached the workspace: cat={ok_s} ls={ok_ls}"));
        }
        let (_, perms, _) = docker(&[
            "exec",
            &c,
            "stat",
            "-c",
            "%U:%G %a",
            "/workspace",
            "/workspace/files",
            "/workspace/files/report.md",
        ])
        .await;
        eprintln!("MEASURE in_container_owner_mode={perms:?}");
        if !perms.lines().next().is_some_and(|l| l.trim() == "root:root 700") {
            return Err(format!("tmpfs parent is not root-only: {perms:?}"));
        }
        for target in ["/workspace/files/x", "/workspace/x", "/etc/x"] {
            let (wr, _, _) =
                docker(&["exec", &c, "sh", "-c", &format!("echo x > {target}")]).await;
            // `/workspace/x` lands in the 64 KiB root-only tmpfs; the other
            // two are the read-only bind and the read-only rootfs.
            if wr && target != "/workspace/x" {
                return Err(format!("container could write {target}"));
            }
        }
        let (_, flags, _) = docker(&[
            "inspect",
            "-f",
            "{{.HostConfig.ReadonlyRootfs}} {{json .HostConfig.SecurityOpt}} {{json .HostConfig.Tmpfs}}",
            &c,
        ])
        .await;
        eprintln!("MEASURE container_flags={}", flags.trim());
        if !flags.starts_with("true ")
            || !flags.contains("no-new-privileges")
            || !flags.contains("\"/tmp\"")
            || !flags.contains("\"/workspace\"")
        {
            return Err(format!("container flags changed: {flags}"));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let reaped = m.reap_once().await;
        let t_reaped = chrono::Utc::now();
        let left = ours(&home).await;
        let size = std::fs::metadata(data_file(&home, &id, "report.md"))
            .map(|m| m.len())
            .unwrap_or(0);
        eprintln!(
            "MEASURE compute_reclaimed_at={t_reaped} containers_left={} data_bytes={size}",
            left.len()
        );
        if reaped != 1 || !left.is_empty() || size == 0 {
            return Err(format!("reap: reaped={reaped} left={left:?} size={size}"));
        }
        drop(m);
        let m2 = mgr(&home, Duration::from_secs(120));
        m2.start("alice", ws(&id))
            .await
            .map_err(|e| format!("restart start: {e:?}"))?;
        let c2 = ours(&home)
            .await
            .first()
            .cloned()
            .ok_or("no container after restart")?;
        let (ok, out, _) =
            docker(&["exec", &c2, "sha256sum", "/workspace/files/report.md"]).await;
        let r = m2
            .workspace_read("alice", &id, "report.md")
            .await
            .map_err(|e| format!("read: {e:?}"))?;
        m2.stop("alice", None)
            .await
            .map_err(|e| format!("stop: {e:?}"))?;
        if !ok
            || !out.starts_with(w["sha256"].as_str().unwrap_or("?"))
            || r["sha256"] != w["sha256"]
        {
            return Err(format!("after restart: {out} / {}", r["sha256"]));
        }
        Ok(())
    })
    .await;
}

/// HT3 (a): `docker kill` → the next op fails, the reaper collects the
/// session, the data is intact.
#[tokio::test]
#[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn computer_workspace_real_ht3_killed_container() {
    let tmp = ws_home(false, "");
    let home = tmp.path().to_path_buf();
    guarded(&home, async {
        let m = mgr(&home, Duration::from_secs(2));
        let id = m
            .start("alice", ws("new"))
            .await
            .map_err(|e| format!("{e:?}"))?["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        m.workspace_write("alice", &id, "a.txt", "keep", None)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let c = ours(&home).await.first().cloned().ok_or("no container")?;
        docker(&["kill", &c]).await;
        if m.screenshot("alice", None).await.is_ok() {
            return Err("screenshot of a killed container succeeded".into());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let reaped = m.reap_once().await;
        let row = WorkspaceStore::open(&home)
            .unwrap()
            .get(&id)
            .unwrap()
            .unwrap();
        let data = std::fs::read_to_string(data_file(&home, &id, "a.txt")).unwrap_or_default();
        if reaped != 1
            || !ours(&home).await.is_empty()
            || data != "keep"
            || row.lease_holder.is_some()
        {
            return Err(format!(
                "reaped={reaped} data={data:?} holder={:?}",
                row.lease_holder
            ));
        }
        Ok(())
    })
    .await;
}

/// HT3 (b): the manager vanishes without ending its session (gateway
/// killed); once the lease has expired, the boot sweep removes the leftover
/// container by its labels; the data is intact.
#[tokio::test]
#[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn computer_workspace_real_ht3_boot_sweep_removes_orphan() {
    let tmp = ws_home(false, "");
    let home = tmp.path().to_path_buf();
    guarded(&home, async {
        let m = mgr(&home, Duration::from_secs(120));
        let id = m
            .start("alice", ws("new"))
            .await
            .map_err(|e| format!("{e:?}"))?["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        m.workspace_write("alice", &id, "a.txt", "keep", None)
            .await
            .map_err(|e| format!("{e:?}"))?;
        // Simulate a killed gateway: no end, no Drop (which would rm -f).
        std::mem::forget(m);
        if ours(&home).await.len() != 1 {
            return Err("container should still run".into());
        }
        // The start lease (90 s + the start budget, never renewed by the
        // dead gateway) runs out: clock moved forward rather than waited.
        WorkspaceStore::open(&home)
            .unwrap()
            .expire_stale_leases(
                cw::unix_now() + super::super::workspace::INITIAL_LEASE_TTL_SECS + 10,
            )
            .unwrap();
        sweep::reconcile_workspaces_unchecked(&home).await;
        let removed = sweep::sweep_once_with(&home, true).await;
        let data = std::fs::read_to_string(data_file(&home, &id, "a.txt")).unwrap_or_default();
        if removed != 1 || !ours(&home).await.is_empty() || data != "keep" {
            return Err(format!("removed={removed} data={data:?}"));
        }
        Ok(())
    })
    .await;
}

/// HT3 (c): A stops, B (another manager) starts the same workspace at once:
/// B's container lives (and survives a sweep), A's is gone.
#[tokio::test]
#[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn computer_workspace_real_ht3_stop_then_takeover() {
    let tmp = ws_home(false, "");
    let home = tmp.path().to_path_buf();
    guarded(&home, async {
        let a = mgr(&home, Duration::from_secs(120));
        let id = a
            .start("alice", ws("new"))
            .await
            .map_err(|e| format!("{e:?}"))?["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let a_c = ours(&home).await;
        a.stop("alice", None).await.map_err(|e| format!("{e:?}"))?;
        let b = mgr(&home, Duration::from_secs(120));
        b.start("alice", ws(&id))
            .await
            .map_err(|e| format!("B start: {e:?}"))?;
        sweep::sweep_once_with(&home, true).await;
        let now = ours(&home).await;
        let b_alive = now.len() == 1 && !a_c.contains(&now[0]);
        let epoch = WorkspaceStore::open(&home)
            .unwrap()
            .get(&id)
            .unwrap()
            .unwrap()
            .lease_epoch
            .to_string();
        let (_, label, _) = docker(&[
            "inspect",
            "-f",
            &format!("{{{{index .Config.Labels \"{}\"}}}}", cw::LEASE_LABEL),
            now.first().map(String::as_str).unwrap_or("x"),
        ])
        .await;
        b.stop("alice", None).await.map_err(|e| format!("{e:?}"))?;
        if !b_alive || label != epoch {
            return Err(format!("A={a_c:?} now={now:?} label={label} epoch={epoch}"));
        }
        Ok(())
    })
    .await;
}

// HT4 (a mount Docker refuses) is not here: on Docker Desktop for macOS every
// writable user location (/Users, /Volumes, /private, /tmp, /var/folders) is
// shared by default, so no refused mount can be produced on this host —
// BLOCKED_ENV, reported instead of faked.

const CDP_SET: &str = r#"
import sys
sys.path.insert(0, '/usr/local/bin')
import duduclaw_cdp as c
mark = sys.stdin.readline().strip()
for path, url in c.list_page_targets(5):
    if url.startswith('https://'):
        ws = c.WebSocket(path, 5)
        js = "document.cookie='%s=1; max-age=3600; path=/'; localStorage.setItem('%s','1'); document.cookie + '|' + localStorage.getItem('%s')" % (mark, mark, mark)
        print(c.evaluate_isolated(ws, js).get('result', {}).get('value'))
        break
"#;

fn scan_tree(dir: &std::path::Path, marks: &[&str], hits: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        if m.is_dir() {
            scan_tree(&p, marks, hits);
        } else if m.is_file() {
            let bytes = std::fs::read(&p).unwrap_or_default();
            for mark in marks {
                if bytes.windows(mark.len()).any(|w| w == mark.as_bytes()) {
                    hits.push(format!("{} in {}", mark, p.display()));
                }
            }
        }
    }
}

/// §6.4: cookie / localStorage marker set in the page, another marker typed
/// by the employee, a normal workspace file written; after the session, no
/// marker in any home file, any registry row, or a tar of the home.
#[tokio::test]
#[ignore = "needs Docker, outbound network and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn computer_workspace_real_secret_markers_never_persist() {
    let tmp = ws_home(false, "allowed_domains = [\"example.com\"]\n");
    let home = tmp.path().to_path_buf();
    let cookie_mark = format!("DUDU_MARK_{}", uuid::Uuid::new_v4().as_simple());
    let typed_mark = format!("DUDU_TYPED_{}", uuid::Uuid::new_v4().as_simple());
    guarded(&home, async {
        let m = mgr(&home, Duration::from_secs(120));
        let started = m
            .start("alice", ws("new"))
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = started["workspace_id"].as_str().unwrap().to_string();
        if started["reachable_hosts"]
            .as_array()
            .is_none_or(|a| a.is_empty())
        {
            let _ = m.stop("alice", None).await;
            return Err("BLOCKED_ENV(network): example.com did not resolve".into());
        }
        m.action(
            "alice",
            None,
            None,
            &ActionRequest::Navigate {
                url: "https://example.com/".into(),
            },
        )
        .await
        .map_err(|e| format!("BLOCKED_ENV(network)? navigate: {e:?}"))?;
        let c = ours(&home).await.first().cloned().ok_or("no container")?;
        let set = crate::computer_use_orchestrator::docker_output_with_stdin(
            &["exec", "-i", &c, "python3", "-c", CDP_SET],
            format!("{cookie_mark}\n").as_bytes(),
            Duration::from_secs(30),
            "cdp",
        )
        .await
        .map_err(|e| e.to_string())?;
        let echoed = String::from_utf8_lossy(&set.stdout).to_string();
        eprintln!(
            "MEASURE marker_set_in_page={}",
            echoed.contains(&cookie_mark)
        );
        if !echoed.contains(&cookie_mark) {
            let _ = m.stop("alice", None).await;
            return Err(format!(
                "marker was not set in the page: {echoed} {}",
                String::from_utf8_lossy(&set.stderr)
            ));
        }
        let _ = m
            .action(
                "alice",
                None,
                None,
                &ActionRequest::Type {
                    text: typed_mark.clone(),
                },
            )
            .await;
        let _ = m.screenshot("alice", None).await;
        m.workspace_write("alice", &id, "notes.md", "ordinary work file", None)
            .await
            .map_err(|e| format!("{e:?}"))?;
        m.stop("alice", None).await.map_err(|e| format!("{e:?}"))?;
        let marks = [cookie_mark.as_str(), typed_mark.as_str()];
        let mut hits = Vec::new();
        scan_tree(&home, &marks, &mut hits);
        let conn =
            rusqlite::Connection::open(home.canonicalize().unwrap().join("computer_workspaces.db"))
                .unwrap();
        for table in ["workspaces", "workspace_events", "workspace_write_intents"] {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let cols = stmt.column_count();
            let rows: Vec<String> = stmt
                .query_map([], |r| {
                    Ok((0..cols)
                        .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .flatten()
                .collect();
            for row in rows {
                for mark in marks {
                    if row.contains(mark) {
                        hits.push(format!("{mark} in table {table}"));
                    }
                }
            }
        }
        let tar = std::process::Command::new("tar")
            .arg("-cf")
            .arg("-")
            .arg("-C")
            .arg(&home)
            .arg(".")
            .output()
            .unwrap();
        for mark in marks {
            if tar.stdout.windows(mark.len()).any(|w| w == mark.as_bytes()) {
                hits.push(format!("{mark} in tar"));
            }
        }
        eprintln!(
            "MEASURE marker_hits={} tar_bytes={}",
            hits.len(),
            tar.stdout.len()
        );
        if !hits.is_empty() {
            return Err(format!("markers persisted: {hits:?}"));
        }
        Ok(())
    })
    .await;
}

/// HT6: three stable public pages, a screenshot each, `weekly-report.md`,
/// stop, restart, start the same workspace, read it back unchanged.
#[tokio::test]
#[ignore = "needs Docker, outbound network and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
async fn computer_workspace_real_ht6_weekly_report() {
    let tmp = ws_home(
        false,
        "allowed_domains = [\"example.com\", \"example.org\", \"www.iana.org\"]\n",
    );
    let home = tmp.path().to_path_buf();
    guarded(&home, async {
        let m = mgr(&home, Duration::from_secs(120));
        let started = m
            .start("alice", ws("new"))
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = started["workspace_id"].as_str().unwrap().to_string();
        let mut report = String::from("# 每週三頁報告\n");
        for url in [
            "https://example.com/",
            "https://example.org/",
            "https://www.iana.org/help/example-domains",
        ] {
            let nav = m
                .action(
                    "alice",
                    None,
                    None,
                    &ActionRequest::Navigate { url: url.into() },
                )
                .await
                .map_err(|e| format!("BLOCKED_ENV(network)? {url}: {e:?}"))?;
            let shot = m
                .screenshot("alice", None)
                .await
                .map_err(|e| format!("shot: {e:?}"))?;
            report.push_str(&format!(
                "- {} host={} masked={} at={}\n",
                url,
                nav["host"].as_str().unwrap_or("?"),
                shot["fully_masked"],
                chrono::Utc::now().to_rfc3339()
            ));
        }
        let w = m
            .workspace_write("alice", &id, "weekly-report.md", &report, None)
            .await
            .map_err(|e| format!("{e:?}"))?;
        m.stop("alice", None).await.map_err(|e| format!("{e:?}"))?;
        drop(m);
        let m2 = mgr(&home, Duration::from_secs(120));
        m2.start("alice", ws(&id))
            .await
            .map_err(|e| format!("{e:?}"))?;
        let r = m2
            .workspace_read("alice", &id, "weekly-report.md")
            .await
            .map_err(|e| format!("{e:?}"))?;
        let c = ours(&home).await.first().cloned().ok_or("no container")?;
        let (_, inside, _) =
            docker(&["exec", &c, "sha256sum", "/workspace/files/weekly-report.md"]).await;
        m2.stop("alice", None).await.map_err(|e| format!("{e:?}"))?;
        eprintln!(
            "MEASURE weekly_report_bytes={} sha={}",
            report.len(),
            w["sha256"]
        );
        if r["sha256"] != w["sha256"]
            || !inside.starts_with(w["sha256"].as_str().unwrap_or("?"))
            || w["sha256"] != json!(sha(report.as_bytes()))
        {
            return Err(format!("readback mismatch: {} / {inside}", r["sha256"]));
        }
        Ok(())
    })
    .await;
}
