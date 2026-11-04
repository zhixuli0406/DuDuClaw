//! Unit tests over the in-memory store (no real credential store touched).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) struct TempHome(PathBuf);

impl TempHome {
    pub(super) fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("duduclaw-cp-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
    /// Become "the gateway" for this home (the lock is per home, per process).
    pub(super) fn hold_gateway_lock(&self) {
        duduclaw_core::gateway_instance::acquire(&self.0).unwrap();
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A protection over a shared in-memory store, so the test can tamper with
/// the store behind the protection's back.
fn protection(home: &TempHome) -> (CredentialProtection, std::sync::Arc<MemoryKeyStore>) {
    let store = std::sync::Arc::new(MemoryKeyStore::app_bound());
    let cp = CredentialProtection::with_store(home.path(), Box::new(store.clone()));
    (cp, store)
}

#[test]
fn fresh_home_is_file_level_and_refuses_person_identity() {
    let home = TempHome::new("fresh");
    home.hold_gateway_lock();
    let (cp, _) = protection(&home);
    let s = cp.status();
    assert_eq!(s.state, ProtectionState::NotEnabled);
    assert_eq!(s.level, ProtectionLevel::File);
    assert_eq!(s.platform_level, ProtectionLevel::AppBound);
    assert!(!s.person_identity_allowed);
    assert_eq!(s.summary_zh, "憑證保護：檔案");
    let err = cp.person_identity_gate().unwrap_err();
    assert_eq!(err.code, "not_enabled");
}

#[test]
fn enable_then_seal_round_trips_and_marker_holds_no_key() {
    let home = TempHome::new("enable");
    home.hold_gateway_lock();
    let (cp, store) = protection(&home);
    let s = cp.enable(false).unwrap();
    assert_eq!(s.state, ProtectionState::Ready);
    assert_eq!(s.level, ProtectionLevel::AppBound);
    assert!(s.person_identity_allowed);
    assert_eq!(s.summary_zh, "憑證保護：系統金鑰圈（只有 DuDuClaw 程式讀得到）");

    let key = cp.person_identity_gate().unwrap();
    let sealed = key.seal(b"refresh-token-123").unwrap();
    assert!(sealed.starts_with(&format!("{SEALED_PREFIX}{}:", key.key_id())));
    assert_eq!(&key.unseal(&sealed).unwrap()[..], b"refresh-token-123");
    assert!(!format!("{key:?}").contains(&hex(&store.read(&cp.account()).unwrap())));

    let raw = store.read(&cp.account()).unwrap();
    let marker_text = std::fs::read_to_string(marker::marker_path(home.path())).unwrap();
    assert!(!marker_text.contains(&hex(&raw)), "marker must not contain the key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(marker::marker_path(home.path())).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn enable_is_idempotent_when_ready() {
    let home = TempHome::new("idem");
    home.hold_gateway_lock();
    let (cp, _) = protection(&home);
    let first = cp.enable(false).unwrap();
    let second = cp.enable(false).unwrap();
    assert_eq!(first.key_id, second.key_id, "a second enable must not rotate the key");
}

#[test]
fn only_the_gateway_lock_holder_may_unseal() {
    let home = TempHome::new("notgw");
    let (cp, _) = protection(&home);
    cp.enable(false).unwrap();
    // Status works anywhere (it never hands out the key)…
    assert_eq!(cp.status().state, ProtectionState::Ready);
    // …but the key itself does not leave a non-gateway process.
    assert_eq!(cp.load_key_for_gateway().unwrap_err().code, "not_gateway");
    assert_eq!(cp.person_identity_gate().unwrap_err().code, "not_gateway");
}

#[test]
fn overwritten_key_is_detected_and_refused() {
    let home = TempHome::new("tamper");
    home.hold_gateway_lock();
    let (cp, store) = protection(&home);
    cp.enable(false).unwrap();
    let key = cp.person_identity_gate().unwrap();
    let sealed = key.seal(b"secret").unwrap();

    // Another same-user process overwrites the item (design §2.3 E7).
    store.tamper(&cp.account(), &[7u8; 32]);
    let s = cp.status();
    assert_eq!(s.state, ProtectionState::KeyChanged);
    assert!(!s.person_identity_allowed);
    assert_eq!(cp.person_identity_gate().unwrap_err().code, "key_changed");

    // A key forged *with* a matching marker still fails the in-process pin.
    let forged = [9u8; 32];
    store.tamper(&cp.account(), &forged);
    let engine = CryptoEngine::new(&forged).unwrap();
    let mut m = match marker::read_marker(home.path()) {
        marker::MarkerRead::Present(m) => m,
        other => panic!("{other:?}"),
    };
    m.key_id = marker::key_id(&forged);
    m.canary = marker::make_canary(&engine).unwrap();
    marker::write_marker(home.path(), &m).unwrap();
    assert_eq!(cp.status().state, ProtectionState::KeyChanged);

    // And a record sealed under the old key reports key_changed, not garbage.
    let forged_key = ProtectedKey { backend: BackendKind::MacosKeychain, level: ProtectionLevel::AppBound, key: Zeroizing::new(forged), key_id: m.key_id.clone() };
    assert_eq!(forged_key.unseal(&sealed).unwrap_err().code, "key_changed");
}

#[test]
fn reset_rotates_even_when_ready() {
    let home = TempHome::new("rotate");
    let (cp, _) = protection(&home);
    let first = cp.enable(false).unwrap();
    let out = cp.enable_detailed(true).unwrap();
    assert!(out.changed);
    assert_eq!(out.previous, ProtectionState::Ready);
    assert_ne!(out.status.key_id, first.key_id);
    let again = cp.enable_detailed(false).unwrap();
    assert!(!again.changed, "a plain enable on Ready does nothing");
}

/// The documented limit (design §6): replacing key AND marker, then
/// restarting the gateway (simulated by clearing the in-process pin), passes
/// `status()`. What still catches it is the old sealed values, which callers
/// must treat as `key_changed`; `reset` then rotates away from the planted key.
#[test]
fn key_and_marker_swap_after_restart_is_caught_only_by_old_sealed_values() {
    let home = TempHome::new("swap");
    home.hold_gateway_lock();
    let (cp, store) = protection(&home);
    cp.enable(false).unwrap();
    let sealed = cp.person_identity_gate().unwrap().seal(b"token").unwrap();

    let planted = [5u8; 32];
    store.tamper(&cp.account(), &planted);
    let mut m = match marker::read_marker(home.path()) {
        marker::MarkerRead::Present(m) => m,
        other => panic!("{other:?}"),
    };
    m.key_id = marker::key_id(&planted);
    m.canary = marker::make_canary(&CryptoEngine::new(&planted).unwrap()).unwrap();
    marker::write_marker(home.path(), &m).unwrap();
    pin_reset(&cp.account()); // the gateway restarted

    assert_eq!(cp.status().state, ProtectionState::Ready, "not detectable from the store alone");
    let key = cp.person_identity_gate().unwrap();
    assert_eq!(key.unseal(&sealed).unwrap_err().code, "key_changed");

    let rotated = cp.enable(true).unwrap();
    assert_ne!(rotated.key_id.as_deref(), Some(marker::key_id(&planted).as_str()));
}

#[test]
fn wrong_length_key_counts_as_changed() {
    let home = TempHome::new("len");
    let (cp, store) = protection(&home);
    cp.enable(false).unwrap();
    store.tamper(&cp.account(), b"short");
    assert_eq!(cp.status().state, ProtectionState::KeyChanged);
}

#[test]
fn missing_item_and_interaction_required_are_distinct_states() {
    let home = TempHome::new("missing");
    let (cp, store) = protection(&home);
    cp.enable(false).unwrap();
    store.force_read_error(Some(StoreError::InteractionRequired));
    let s = cp.status();
    assert_eq!(s.state, ProtectionState::NeedsAuthorization);
    assert!(s.detail_zh.contains("duduclaw credentials authorize"));
    store.force_read_error(None);
    store.remove_behind_back(&cp.account());
    assert_eq!(cp.status().state, ProtectionState::Missing);
}

#[test]
fn broken_or_foreign_marker_is_invalid() {
    let home = TempHome::new("marker");
    let (cp, _) = protection(&home);
    cp.enable(false).unwrap();
    std::fs::write(marker::marker_path(home.path()), b"{not json").unwrap();
    assert_eq!(cp.status().state, ProtectionState::Invalid);

    // A marker copied from another home names another account.
    let other = TempHome::new("marker-other");
    let (cp2, _) = protection(&other);
    cp2.enable(false).unwrap();
    std::fs::copy(marker::marker_path(other.path()), marker::marker_path(home.path())).unwrap();
    assert_eq!(cp.status().state, ProtectionState::Invalid);
}

#[test]
fn re_enable_over_a_broken_state_needs_reset() {
    let home = TempHome::new("reset");
    let (cp, store) = protection(&home);
    let old = cp.enable(false).unwrap();
    store.remove_behind_back(&cp.account());
    assert_eq!(cp.enable(false).unwrap_err().code, "reset_required");
    let new = cp.enable(true).unwrap();
    assert_eq!(new.state, ProtectionState::Ready);
    assert_ne!(old.key_id, new.key_id);
}

#[test]
fn disable_removes_key_and_marker() {
    let home = TempHome::new("disable");
    let (cp, store) = protection(&home);
    cp.enable(false).unwrap();
    cp.disable().unwrap();
    assert_eq!(cp.status().state, ProtectionState::NotEnabled);
    assert!(store.read(&cp.account()).is_err());
    assert!(!marker::marker_path(home.path()).exists());
}

#[test]
fn unsupported_platform_refuses_enable_and_gate() {
    let home = TempHome::new("unsupported");
    home.hold_gateway_lock();
    let cp = CredentialProtection::with_store(home.path(), Box::new(UnsupportedStore));
    let s = cp.status();
    assert_eq!(s.state, ProtectionState::Unsupported);
    assert_eq!(s.level, ProtectionLevel::File);
    assert!(!s.person_identity_allowed);
    assert_eq!(cp.enable(false).unwrap_err().code, "unsupported");
    assert_eq!(cp.person_identity_gate().unwrap_err().code, "unsupported");
}

#[test]
fn level_policy_admits_user_store_but_not_file() {
    assert!(ProtectionLevel::AppBound >= PERSON_IDENTITY_MIN_LEVEL);
    assert!(ProtectionLevel::UserStore >= PERSON_IDENTITY_MIN_LEVEL);
    assert!(ProtectionLevel::File < PERSON_IDENTITY_MIN_LEVEL);

    let home = TempHome::new("userstore");
    home.hold_gateway_lock();
    let store = MemoryKeyStore::new(BackendKind::WindowsCredentialManager, ProtectionLevel::UserStore);
    let cp = CredentialProtection::with_store(home.path(), Box::new(store));
    let s = cp.enable(false).unwrap();
    assert_eq!(s.summary_zh, "憑證保護：系統金鑰圈（同一個帳號的程式讀得到）");
    assert!(s.person_identity_allowed);
    assert!(cp.person_identity_gate().is_ok());
}

#[test]
fn sealed_values_are_parsed_strictly() {
    for bad in ["", "enc:v1:abc", "pk:v1:", "pk:v1:abc", "pk:v1::body", "pk:v1:abc:"] {
        assert!(parse_sealed(bad).is_err(), "{bad:?} must not parse");
    }
    assert_eq!(parse_sealed("pk:v1:abc:Zm9v").unwrap(), ("abc", "Zm9v"));
}

#[test]
fn account_is_derived_from_the_home_path() {
    let a = TempHome::new("acct-a");
    let b = TempHome::new("acct-b");
    let acct = marker::account_for_home(a.path());
    assert!(acct.starts_with("home-") && acct.len() == "home-".len() + 16);
    assert_eq!(acct, marker::account_for_home(a.path()));
    assert_ne!(acct, marker::account_for_home(b.path()));
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn protection_levels_and_backend_use_stable_serde_names() {
    for (level, name) in [
        (ProtectionLevel::File, "file"),
        (ProtectionLevel::UserStore, "user_store"),
        (ProtectionLevel::ProcessIsolated, "process_isolated"),
        (ProtectionLevel::AppBound, "app_bound"),
    ] {
        assert_eq!(serde_json::to_value(level).unwrap(), name);
        assert_eq!(serde_json::from_value::<ProtectionLevel>(serde_json::json!(name)).unwrap(), level);
    }
    assert!(ProtectionLevel::UserStore < ProtectionLevel::ProcessIsolated);
    assert!(ProtectionLevel::ProcessIsolated < ProtectionLevel::AppBound);
    for (backend, name) in [
        (BackendKind::MacosKeychain, "macos_keychain"),
        (BackendKind::WindowsCredentialManager, "windows_credential_manager"),
        (BackendKind::ContainerRootKey, "container_root_key"),
        (BackendKind::None, "none"),
    ] {
        assert_eq!(serde_json::to_value(backend).unwrap(), name);
        assert_eq!(serde_json::from_value::<BackendKind>(serde_json::json!(name)).unwrap(), backend);
    }
    let home = TempHome::new("container-status-serde");
    let store = MemoryKeyStore::new(BackendKind::ContainerRootKey, ProtectionLevel::ProcessIsolated);
    let cp = CredentialProtection::with_store(home.path(), Box::new(store));
    let status = serde_json::to_value(cp.enable(false).unwrap()).unwrap();
    assert_eq!(status["level"], "process_isolated");
    assert_eq!(status["platform_level"], "process_isolated");
    assert_eq!(status["backend"], "container_root_key");
    assert_eq!(status["person_identity_allowed"], true);
}

#[test]
fn state_messages_describe_the_actual_backend() {
    for state in [ProtectionState::NotEnabled, ProtectionState::Missing,
        ProtectionState::KeyChanged, ProtectionState::Unavailable] {
        let (summary, detail) = messages::state_text(state, ProtectionLevel::File, BackendKind::ContainerRootKey, "docker_socket");
        assert!(!summary.contains("系統金鑰圈"));
        assert!(!detail.contains("系統金鑰圈"));
        assert!(!detail.contains("docker_socket"));
        match state {
            ProtectionState::NotEnabled => assert!(detail.contains("「安全」頁")),
            ProtectionState::KeyChanged => assert!(detail.contains("根金鑰遺失或被更換")),
            ProtectionState::Unavailable => assert!(detail.contains("Docker 控制介面")),
            _ => (),
        }
        for backend in [BackendKind::MacosKeychain, BackendKind::WindowsCredentialManager] {
            let (_, detail) = messages::state_text(state, ProtectionLevel::File, backend, "");
            assert!(detail.contains("系統金鑰圈"));
        }
    }
    let (summary, detail) = messages::state_text(ProtectionState::Ready, ProtectionLevel::ProcessIsolated, BackendKind::ContainerRootKey, "");
    assert!(summary.contains("獨立金鑰（AI 員工讀不到）"));
    for limit in ["啟動步驟（root）", "Google", "存取權杖", "停止運作", "Docker 主機", "根金鑰儲存位置"] {
        assert!(detail.contains(limit), "missing truthful limitation: {limit}");
    }
    let (_, detail) = messages::state_text(ProtectionState::Unsupported, ProtectionLevel::File, BackendKind::None, "");
    for text in ["一般 Docker 或 Linux", "部署指南", "先驗證發行檔", "root 管理的位置安裝", "儀表板"] {
        assert!(detail.contains(text), "missing installation guidance: {text}");
    }
}

#[test]
fn every_root_refusal_has_plain_chinese_text() {
    for code in ["inactive", "fd_contract", "fd_stat", "fd_type", "fd_owner", "fd_mode",
        "fd_links", "fd_size", "fd_read", "not_pid_one", "user_ids", "group_ids", "proc_status",
        "no_new_privs", "cap_inh", "cap_prm", "cap_eff", "cap_bnd", "cap_amb", "dumpable",
        "key_path_accessible", "docker_socket", "docker_host",
        "mode", "not_systemd_child", "cgroup_v1", "cgroup_unit", "exe_stat", "exe_type", "exe_owner", "exe_writable", "exe_ancestor_type", "exe_ancestor_owner", "exe_ancestor_writable", "supplementary_groups", "engine_socket_reachable", "engine_socket", "mount_scan_incomplete"] {
        let text = messages::refusal_text(code).unwrap();
        assert!(!text.is_ascii());
        assert!(!text.contains(code));
        let error = ProtectionError::from_state(ProtectionState::Unavailable, BackendKind::ContainerRootKey, code);
        assert!(error.message_zh.contains(text));
        assert!(!error.message_zh.contains(code));
    }
    assert!(messages::refusal_text("unknown").is_none());
}
