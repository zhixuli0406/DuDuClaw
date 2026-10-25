//! SOURCE-FIRST publication fault matrix. Never executed in this source phase.
use super::*;
use crate::mcp_oauth::home_store::publication::{self, Fault, FaultGuard, GATE_FILE};

const READER_HOME: &str = "DUDU_H1_OWNED_READER_FIXTURE";
const READER_GOOGLE: &str = "DUDU_H1_READER_EXPECT_GOOGLE";
struct OwnedChild {
    child: std::process::Child,
    reaped: bool,
    marker: std::path::PathBuf,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl OwnedChild {
    fn poll(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.try_wait().unwrap();
        if status.is_some() {
            self.reaped = true;
        }
        status
    }
    fn finish(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = self.poll() {
                assert!(status.success());
                assert!(self.marker.is_file(), "zero matching child tests must fail");
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned reader exceeded deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
fn spawn_reader(home: &Path, expected_google: bool) -> OwnedChild {
    let marker = home.join("reader-started");
    match std::fs::remove_file(&marker) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("owned marker cleanup failed: {e}"),
    }
    let filter = format!(
        "{}::gate_blocks_an_independent_reader_after_writer_closed_and_when_waiting_for_source_lock",
        module_path!().split_once("::").unwrap().1
    );
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &filter, "--test-threads=1"])
        .env_clear()
        .env(READER_HOME, home)
        .env(READER_GOOGLE, if expected_google { "yes" } else { "no" })
        .env("HOME", home)
        .env("TMPDIR", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    OwnedChild {
        child,
        reaped: false,
        marker,
    }
}

#[test]
fn publication_faults_never_issue_a_start_and_reopen_obeys_durable_gate() {
    for fault in [
        Fault::GateCreate,
        Fault::GateWrite,
        Fault::GateFileSync,
        Fault::GateDirSync,
        Fault::DataOpen,
        Fault::DataWrite,
        Fault::DataFileSync,
        Fault::DataRename,
        Fault::DataDirSync,
        Fault::FinalProof,
        Fault::GateUnlink,
        Fault::CleanupDirSync,
    ] {
        let d = fixture();
        let original = bytes(d.path());
        let guard = FaultGuard::at(d.path(), fault);
        let result = run(d.path(), |g, s| {
            publication_transaction(g, |r| {
                r.begin_connect(s, RawDigest::of(b"state"), NOW + 500, NOW)
            })
        });
        assert!(result.is_err(), "{fault:?}");
        let expected = if matches!(fault, Fault::GateUnlink | Fault::CleanupDirSync) {
            Error::CommittedCleanupUnconfirmed
        } else if matches!(fault, Fault::DataDirSync | Fault::FinalProof) {
            Error::PublishedDurabilityUnconfirmed
        } else {
            Error::NotPublished
        };
        assert!(matches!(result, Err(e) if e == expected), "{fault:?}");
        if !matches!(
            fault,
            Fault::DataDirSync | Fault::FinalProof | Fault::GateUnlink | Fault::CleanupDirSync
        ) {
            assert_eq!(bytes(d.path()), original, "{fault:?}");
        }
        drop(guard);
        let expected_google =
            !d.path().join(GATE_FILE).exists() && !matches!(fault, Fault::CleanupDirSync);
        let mut independent = spawn_reader(d.path(), expected_google);
        independent.finish();
        // Drop/reopen all locks, not merely a RAM poison check.
        let tokens = load_public(d.path()).unwrap();
        assert!(tokens.iter().any(|t| t.provider_id == "github"));
        if d.path().join(GATE_FILE).exists() {
            assert!(tokens.iter().all(|t| t.provider_id != "google"));
            assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
            assert!(
                revoke_google(d.path()).is_err(),
                "ordinary writer must not erase gate"
            );
            let i = publication::inspect_publication(d.path()).unwrap();
            assert!(publication::stabilize_exact_publication(d.path(), &i).is_err());
            publication::abort_publication_with_revoke(d.path(), &i).unwrap();
            assert!(!d.path().join(GATE_FILE).exists());
            assert!(
                load_public(d.path())
                    .unwrap()
                    .iter()
                    .all(|t| t.provider_id != "google")
            );
        }
        assert!(!d.path().join(".keyfile").exists());
    }
}

#[test]
fn start_plan_binds_producer_ticket_and_rejects_tampered_proof_before_any_write() {
    for field in [
        "expires_at",
        "state_hash",
        "ticket_id",
        "operation",
        "generation",
    ] {
        let d = fixture();
        let original = bytes(d.path());
        let result = run(d.path(), |g, s| {
            publication_transaction(g, |r| {
                let mut made = r.begin_connect(s, RawDigest::of(b"state"), NOW + 500, NOW)?;
                made.tamper_for_test(field);
                Ok(made)
            })
        });
        assert!(result.is_err(), "{field}");
        assert_eq!(bytes(d.path()), original);
        assert!(!d.path().join(GATE_FILE).exists());
    }
}

#[test]
fn unplanned_google_change_and_noop_ticket_producer_cannot_escape_storage_gate() {
    let d = fixture();
    let original = bytes(d.path());
    assert!(
        run(d.path(), |g, s| transaction(g, |r| {
            let _unissued = r.begin_connect(s, RawDigest::of(b"s"), NOW + 500, NOW)?;
            Ok(())
        }))
        .is_err()
    );
    assert_eq!(bytes(d.path()), original);
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| {
            let made = r.begin_connect(s, RawDigest::of(b"s"), NOW + 500, NOW)?;
            r.rows = serde_json::from_slice(&original).unwrap();
            Ok(made)
        }))
        .is_err()
    );
    assert_eq!(bytes(d.path()), original);
}

#[test]
fn legacy_google_is_hidden_but_non_google_read_survives_gate_and_all_shared_writes_refuse() {
    let d = fixture();
    std::fs::write(d.path().join(GATE_FILE), b"{partial-gate").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            d.path().join(GATE_FILE),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    assert_eq!(load_public(d.path()).unwrap().len(), 1);
    let original = bytes(d.path());
    assert!(remove_public(d.path(), "github").is_err());
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    assert_eq!(original, bytes(d.path()));
    let i = publication::inspect_publication(d.path()).unwrap();
    assert!(publication::stabilize_exact_publication(d.path(), &i).is_err());
    publication::abort_publication_with_revoke(d.path(), &i).unwrap();
    assert_eq!(load_public(d.path()).unwrap().len(), 1);
}

#[test]
fn exact_nonconnect_recovery_only_syncs_same_target_and_never_replays_a_mutation() {
    let d = fixture();
    let fault = FaultGuard::at(d.path(), Fault::DataDirSync);
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    drop(fault);
    let committed_bytes = bytes(d.path());
    let i = publication::inspect_publication(d.path()).unwrap();
    publication::stabilize_exact_publication(d.path(), &i).unwrap();
    assert_eq!(committed_bytes, bytes(d.path()));
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    home_source::validate_home_source(d.path(), &receipt).unwrap();
}

#[test]
fn final_ready_without_ticket_still_retains_connect_proof_and_refuses_generic_recovery() {
    let d = fixture();
    let t = ticket(d.path());
    let claim = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&t, s, NOW))
    })
    .unwrap();
    let prepared = payload(&["scope-a"]);
    let finish = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.begin_claimed_finish(&claim, s, &prepared, None, NOW))
    })
    .unwrap();
    run(d.path(), |g, s| {
        publication_transaction(g, |r| {
            r.publish_claimed_payload(&finish, &claim, s, prepared, None, NOW)
        })
    })
    .unwrap();
    let fault = FaultGuard::at(d.path(), Fault::DataDirSync);
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .finish_change(&finish, s, NOW)))
        .is_err()
    );
    drop(fault);
    let rows: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert!(rows[1]["google_home"]["active_connect"].is_null());
    let mut independent = spawn_reader(d.path(), false);
    independent.finish();
    let i = publication::inspect_publication(d.path()).unwrap();
    assert!(publication::stabilize_exact_publication(d.path(), &i).is_err());
    assert!(
        load_public(d.path())
            .unwrap()
            .iter()
            .all(|t| t.provider_id != "google")
    );
    publication::abort_publication_with_revoke(d.path(), &i).unwrap();
    assert!(
        run(d.path(), |g, s| transaction(g, |r| r
            .validate_claim(&claim, s, NOW)))
        .is_err()
    );
}

#[test]
fn fresh_host_expiry_after_durable_ready_keeps_gate_and_requires_fresh_revoked_abort() {
    let d = fixture();
    let t = ticket(d.path());
    let claim = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.claim(&t, s, NOW))
    })
    .unwrap();
    let p = payload(&["scope-a"]);
    let finish = run(d.path(), |g, s| {
        publication_transaction(g, |r| r.begin_claimed_finish(&claim, s, &p, None, NOW))
    })
    .unwrap();
    run(d.path(), |g, s| {
        publication_transaction(g, |r| r.publish_claimed_payload(&finish, &claim, s, p, None, NOW))
    })
    .unwrap();
    let before: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    // Producer's precheck may have used an earlier host instant; the final
    // barrier must use a fresh host clock rather than that caller argument.
    let clock = publication::ClockGuard::after_preflight(NOW, NOW + 501);
    assert!(
        run(d.path(), |g, s| publication_transaction(g, |r| r
            .finish_change(&finish, s, NOW)))
        .is_err()
    );
    drop(clock);
    assert!(d.path().join(GATE_FILE).exists());
    let mut independent = spawn_reader(d.path(), false);
    independent.finish();
    let i = publication::inspect_publication(d.path()).unwrap();
    assert!(publication::stabilize_exact_publication(d.path(), &i).is_err());
    publication::abort_publication_with_revoke(d.path(), &i).unwrap();
    let after: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    assert_ne!(
        before[1]["google_home"]["source_generation"],
        after[1]["google_home"]["source_generation"]
    );
    assert!(after[1].get("access_token").is_none());
}

#[test]
fn recovery_mismatch_and_failed_abort_preserve_gate_and_never_restore_old_generation() {
    let d = fixture();
    let f = FaultGuard::at(d.path(), Fault::DataDirSync);
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    drop(f);
    let i = publication::inspect_publication(d.path()).unwrap();
    let mut changed: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    changed[1]["access_token"] = json!("enc:v1:external-change");
    std::fs::write(
        d.path().join(super::super::super::TOKEN_FILE),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(publication::stabilize_exact_publication(d.path(), &i).is_err());
    let f = FaultGuard::at(d.path(), Fault::DataDirSync);
    assert_eq!(
        publication::abort_publication_with_revoke(d.path(), &i),
        Err(Error::PublishedDurabilityUnconfirmed)
    );
    drop(f);
    assert!(d.path().join(GATE_FILE).exists());
    publication::abort_publication_with_revoke(d.path(), &i).unwrap();
    assert!(
        load_public(d.path())
            .unwrap()
            .iter()
            .all(|t| t.provider_id != "google")
    );
}

#[test]
fn gate_blocks_an_independent_reader_after_writer_closed_and_when_waiting_for_source_lock() {
    if let Some(path) = std::env::var_os(READER_HOME) {
        let home = std::path::PathBuf::from(path);
        let blocked = home.join(GATE_FILE).exists();
        std::fs::write(home.join("reader-started"), b"started").unwrap();
        let tokens = load_public(&home).unwrap();
        assert_eq!(
            tokens.iter().any(|t| t.provider_id == "google"),
            std::env::var(READER_GOOGLE).unwrap() == "yes"
        );
        assert!(tokens.iter().any(|t| t.provider_id == "github"));
        if blocked {
            assert!(home_source::prepare_home_selection(&home, &["scope-a"]).is_err());
        }
        assert!(!home.join(".keyfile").exists());
        return;
    }
    let d = fixture();
    let f = FaultGuard::at(d.path(), Fault::DataDirSync);
    assert!(home_source::prepare_home_selection(d.path(), &["scope-a"]).is_err());
    drop(f);
    let mut child = home_source::with_source_guard(d.path(), |_| {
        let mut child = spawn_reader(d.path(), false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !d.path().join("reader-started").exists() {
            assert!(std::time::Instant::now() < deadline);
            assert!(child.poll().is_none());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            child.poll().is_none(),
            "second process must wait for source lock"
        );
        Ok(child)
    })
    .unwrap();
    child.finish();
    // Fully close writer/reader locks and repeat from a fresh process.
    let mut reopened = spawn_reader(d.path(), false);
    reopened.finish();
}

#[test]
fn cleanup_unconfirmed_returns_no_receipt_but_absent_gate_exposes_only_confirmed_durable_data() {
    for fault in [Fault::GateUnlink, Fault::CleanupDirSync] {
        let d = fixture();
        let probe = test_probe::Guard::install(d.path());
        let f = FaultGuard::at(d.path(), fault);
        assert!(matches!(
            home_source::prepare_home_selection(d.path(), &["scope-a"]),
            Err(Error::CommittedCleanupUnconfirmed)
        ));
        drop(f);
        let committed = bytes(d.path());
        if d.path().join(GATE_FILE).exists() {
            assert!(
                load_public(d.path())
                    .unwrap()
                    .iter()
                    .all(|t| t.provider_id != "google")
            );
            let i = publication::inspect_publication(d.path()).unwrap();
            publication::stabilize_exact_publication(d.path(), &i).unwrap();
        }
        let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
        home_source::validate_home_source(d.path(), &receipt).unwrap();
        assert_eq!(committed, bytes(d.path()));
        assert_eq!(probe.counts(), [0, 0]);
    }
}

#[test]
fn retiring_old_tickets_and_new_start_proof_cannot_be_mislabeled_as_nonconnect() {
    for action in ["start", "refresh", "fence", "revoke"] {
        let d = fixture();
        let _ticket = ticket(d.path());
        let original = bytes(d.path());
        let result = run(d.path(), |g, s| {
            publication_transaction(g, |r| {
                if action == "refresh" {
                    let identity = r.refresh_identity(s)?;
                    let mut made =
                        r.finish_refresh(&identity, s, payload(&["scope-a", "scope-b"]))?;
                    made.strip_ticket_variant_for_test();
                    return Ok(made);
                }
                if action == "revoke" {
                    let mut made = r.revoke()?;
                    made.strip_ticket_variant_for_test();
                    return Ok(made);
                }
                // Map only within this test to unit while retaining the original
                // producer fingerprint. Transaction must still reject the plan.
                if action == "start" {
                    let mut made = r.begin_connect(s, RawDigest::of(b"new"), NOW + 500, NOW)?;
                    made.strip_ticket_variant_for_test();
                    return Ok(made.unit_for_test());
                }
                let mut made = r.fence_change(s, s.binding().clone())?;
                made.strip_ticket_variant_for_test();
                Ok(made.unit_for_test())
            })
        });
        assert!(result.is_err(), "{action}");
        assert_eq!(original, bytes(d.path()));
        assert!(!d.path().join(GATE_FILE).exists());
    }
}

#[test]
fn incomplete_or_unknown_gate_ticket_proofs_never_stabilize_even_with_a_matching_pending_row() {
    for field in ["proof", "expires_at", "mutation_binding", "operation"] {
        let d = fixture();
        let f = FaultGuard::at(d.path(), Fault::DataDirSync);
        assert!(
            run(d.path(), |g, s| publication_transaction(g, |r| r
                .begin_connect(s, RawDigest::of(b"s"), NOW + 500, NOW)))
            .is_err()
        );
        drop(f);
        let path = d.path().join(GATE_FILE);
        let mut gate: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        match field {
            "proof" => {
                gate["plan"]["op"].as_object_mut().unwrap().remove("proof");
            }
            "expires_at" => {
                gate["plan"]["op"]["proof"]["ticket"]
                    .as_object_mut()
                    .unwrap()
                    .remove("expires_at");
            }
            "mutation_binding" => {
                gate["plan"]["op"]["proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("mutation_binding");
            }
            _ => gate["plan"]["op"]["operation"] = json!("unknown_operation"),
        }
        std::fs::write(path, serde_json::to_vec(&gate).unwrap()).unwrap();
        let original = bytes(d.path());
        let i = publication::inspect_publication(d.path()).unwrap();
        assert!(
            publication::stabilize_exact_publication(d.path(), &i).is_err(),
            "{field}"
        );
        assert_eq!(original, bytes(d.path()));
        let mut independent = spawn_reader(d.path(), false);
        independent.finish();
        publication::abort_publication_with_revoke(d.path(), &i).unwrap();
        assert!(
            load_public(d.path())
                .unwrap()
                .iter()
                .all(|t| t.provider_id != "google")
        );
    }
}

#[test]
fn ordinary_committed_migration_has_no_gate_and_reopen_preserves_exact_cipher_and_generation() {
    let d = fixture();
    let probe = test_probe::Guard::install(d.path());
    let before: Value = serde_json::from_slice(&bytes(d.path())).unwrap();
    let receipt = home_source::prepare_home_selection(d.path(), &["scope-a"]).unwrap();
    assert!(!d.path().join(GATE_FILE).exists());
    let committed = bytes(d.path());
    let after: Value = serde_json::from_slice(&committed).unwrap();
    assert_eq!(before[0], after[0]);
    assert_eq!(before[1]["access_token"], after[1]["access_token"]);
    let mut independent = spawn_reader(d.path(), true);
    independent.finish();
    assert_eq!(committed, bytes(d.path()));
    home_source::validate_home_source(d.path(), &receipt).unwrap();
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!d.path().join(".keyfile").exists());
}

/// Reused by H1 lifecycle fault source cases. The child is still only the
/// same owned, exact-filter test executable; this helper does not prove a
/// particular lock instruction was reached (see r4 contract limitation).
pub(super) fn assert_independent_google_blocked(home: &Path) {
    let mut child = spawn_reader(home, false);
    child.finish();
}
