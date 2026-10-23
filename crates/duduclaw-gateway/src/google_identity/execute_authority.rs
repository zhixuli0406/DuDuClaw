//! The account selected before credential waits must still be authorized at
//! the last pre-effect boundary. No change may silently select another account.
use std::path::Path;

use duduclaw_core::google_identity::{Effective, EffectiveIdentity, Fallback, effective_for};
use sha2::{Digest, Sha256};

use super::{ExecuteRequest, ExecuteState, Refusal};
use crate::google_identity::{delegation_key, requester_gate, resolve};
use crate::google_workspace::GoogleBackend;

#[derive(PartialEq)]
pub(super) struct Authority {
    selected: EffectiveIdentity,
    agent_file: [u8; 32],
    integrations: Option<toml::Value>,
    resolver_config: Option<toml::Value>,
    person: Option<requester_gate::Person>,
    delegation_record: Option<[u8; 32]>,
    home_credential: Option<[u8; 32]>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::google_identity::{CredentialKind, CredentialRef};
    use serde_json::json;

    #[tokio::test]
    async fn bridge_revalidates_resolved_material_and_keeps_unchanged_home_backend() {
        const KEY: &str = "ddc_dev_0123456789abcdef0123456789abcdef";
        for mutation in ["removed", "rotated", "url", "resolver", "unchanged"] {
            let home = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(home.path().join("agents/unit-bridge")).unwrap();
            std::fs::create_dir(home.path().join("secrets")).unwrap();
            let secret_file = home.path().join("secrets/unit-bridge");
            std::fs::write(&secret_file, "unit-bridge-secret").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&secret_file, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            std::fs::write(
                home.path().join("agents/unit-bridge/agent.toml"),
                "[agent]\nname='unit-bridge'\n[google]\nidentity='home'\n",
            )
            .unwrap();
            let raw = format!(
                "[integrations]\ngoogle_workspace=true\n[integrations.google_apps_script]\nurl='https://script.google.com/macros/s/unit/exec'\nsecret='secret://file/{}'\n[mcp_keys.{KEY}]\nclient_id='unit-bridge'\nscopes=['google:read']\nis_external=true\ncreated_at='{}'\n",
                secret_file.display(),
                chrono::Utc::now().to_rfc3339(),
            );
            std::fs::write(home.path().join("config.toml"), &raw).unwrap();
            let state = ExecuteState::new(home.path().to_path_buf(), None);
            let request = ExecuteRequest {
                tool: "google_status".into(),
                arguments: json!({}),
                credential_ref: CredentialRef {
                    kind: CredentialKind::Legacy,
                    sha256: format!("{:x}", Sha256::digest(KEY.as_bytes())),
                },
            };
            let authority = Authority::capture(&state, "unit-bridge").await.unwrap();
            // Resolve only a synthetic file under this temporary home's
            // allowed secret root. No keychain, provider or bridge HTTP call.
            let selected = crate::google_apps_script::config_for_home(home.path())
                .await
                .unwrap()
                .unwrap();
            match mutation {
                "removed" => std::fs::remove_file(&secret_file).unwrap(),
                "rotated" => std::fs::write(&secret_file, "unit-rotated-secret").unwrap(),
                "url" => std::fs::write(
                    home.path().join("config.toml"),
                    raw.replace("/s/unit/exec", "/s/unit-new/exec"),
                )
                .unwrap(),
                "resolver" => std::fs::write(
                    home.path().join("config.toml"),
                    format!("{raw}\n[secret_manager]\nbackend='local'\n"),
                )
                .unwrap(),
                _ => {}
            }
            let result = authority
                .validate(
                    &state,
                    "unit-bridge",
                    &request,
                    false,
                    GoogleBackend::AppsScript(selected),
                )
                .await;
            if mutation == "unchanged" {
                let GoogleBackend::AppsScript(validated) = result.unwrap() else {
                    panic!("Home bridge backend was lost")
                };
                assert_eq!(
                    validated.url,
                    "https://script.google.com/macros/s/unit/exec"
                );
                assert_eq!(validated.secret.expose(), "unit-bridge-secret");
            } else {
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("{mutation} did not refuse changed bridge authority"),
                };
                assert_eq!(error.code, "identity_changed", "{mutation}");
                assert!(!error.message_zh.contains("unit-bridge-secret"));
            }
        }
    }
}

fn changed() -> Refusal {
    Refusal::hard(
        "identity_changed",
        "Google 帳號或授權設定已變更，本次操作已停止。請確認設定後重新操作。",
    )
}

fn file_digest(path: &Path, max: u64) -> Result<Option<[u8; 32]>, Refusal> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= max => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        _ => return Err(changed()),
    }
    let bytes = zeroize::Zeroizing::new(std::fs::read(path).map_err(|_| changed())?);
    if bytes.len() as u64 > max {
        return Err(changed());
    }
    Ok(Some(Sha256::digest(bytes.as_slice()).into()))
}

/// Expiry/access-token changes from an ordinary OAuth refresh are expected;
/// changing its underlying refresh credential or client is new authority.
fn home_credential(home: &Path, config: &str) -> Result<[u8; 32], Refusal> {
    use crate::mcp_oauth;
    if let Ok(Some(sa)) = crate::google_service_account::parse_config(config, home) {
        return Ok(file_digest(&sa.key_file, 64 * 1024)?
            .unwrap_or_else(|| Sha256::digest(b"missing-home-key").into()));
    }
    let tokens: Vec<_> = mcp_oauth::load_tokens(home)
        .into_iter()
        .filter(|token| token.provider_id == crate::google_workspace::GOOGLE_PROVIDER)
        .map(|mut token| {
            token.scopes.sort();
            token.scopes.dedup();
            let credential = match token.refresh_token.filter(|token| !token.is_empty()) {
                Some(refresh) => ("refresh", refresh),
                None => ("access", token.access_token),
            };
            (credential, token.scopes)
        })
        .collect();
    let clients: Vec<_> = mcp_oauth::load_client_configs(home)
        .into_iter()
        .filter(|client| client.provider_id == crate::google_workspace::GOOGLE_PROVIDER)
        .collect();
    let bytes =
        zeroize::Zeroizing::new(serde_json::to_vec(&(tokens, clients)).map_err(|_| changed())?);
    Ok(Sha256::digest(bytes.as_slice()).into())
}

impl Authority {
    fn read(
        home: &Path,
        agent: &str,
        people: &dyn requester_gate::People,
    ) -> Result<Self, Refusal> {
        let agent_file = file_digest(
            &home.join("agents").join(agent).join("agent.toml"),
            4 * 1024 * 1024,
        )?
        .ok_or_else(changed)?;
        let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| changed())?;
        if raw.len() > 4 * 1024 * 1024 {
            return Err(changed());
        }
        let doc: toml::Value = toml::from_str(&raw).map_err(|_| changed())?;
        let integrations = doc.get("integrations").cloned();
        let resolver_config = doc.get("secret_manager").cloned();
        if integrations
            .as_ref()
            .and_then(|value| value.get("google_workspace"))
            .and_then(toml::Value::as_bool)
            != Some(true)
        {
            return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
        }
        let selected = effective_for(home, Some(agent));
        if let Some(error) = resolve::refusal_for(&selected.effective) {
            return Err(error);
        }
        let (person, delegation_record) = match &selected.effective {
            Effective::Delegate { email } => (
                people.by_email(email)?,
                file_digest(&delegation_key::path(home), 64 * 1024)?,
            ),
            _ => (None, None),
        };
        let home_credential = (selected.effective == Effective::Home
            || selected.fallback == Fallback::Home)
            .then(|| home_credential(home, &raw))
            .transpose()?;
        Ok(Self {
            selected,
            agent_file,
            integrations,
            resolver_config,
            person,
            delegation_record,
            home_credential,
        })
    }

    pub(super) async fn capture(state: &ExecuteState, agent: &str) -> Result<Self, Refusal> {
        let (home, users, agent) = (state.home.clone(), state.users.clone(), agent.to_owned());
        tokio::task::spawn_blocking(move || {
            let people = requester_gate::people_for(&home, users.as_ref());
            Self::read(&home, &agent, people.as_ref())
        })
        .await
        .map_err(|_| changed())?
    }

    pub(super) fn check_selected(&self, resolved: &resolve::Resolved) -> Result<(), Refusal> {
        let matches = match (resolved, &self.selected.effective) {
            (
                resolve::Resolved::Delegated { email, .. },
                Effective::Delegate { email: original },
            ) => email == original,
            (
                resolve::Resolved::UseHome {
                    fallback: false, ..
                },
                Effective::Home,
            ) => true,
            (resolve::Resolved::UseHome { fallback: true, .. }, Effective::Delegate { .. }) => {
                self.selected.fallback == Fallback::Home
            }
            _ => false,
        };
        if matches { Ok(()) } else { Err(changed()) }
    }

    pub(super) async fn validate(
        self,
        state: &ExecuteState,
        agent: &str,
        request: &ExecuteRequest,
        fallback: bool,
        backend: GoogleBackend,
    ) -> Result<GoogleBackend, Refusal> {
        // A secret:// reference is not the resolved bridge credential. Resolve
        // again before the final synchronous gates, compare to the selected
        // material, and use only this revalidated value for the service call.
        let backend = match backend {
            GoogleBackend::AppsScript(selected) => {
                let current = crate::google_apps_script::config_for_home(&state.home)
                    .await
                    .map_err(|_| changed())?
                    .ok_or_else(changed)?;
                if current != selected {
                    return Err(changed());
                }
                GoogleBackend::AppsScript(current)
            }
            direct => direct,
        };
        let (home, users, agent, reference, tool) = (
            state.home.clone(),
            state.users.clone(),
            agent.to_owned(),
            request.credential_ref.clone(),
            request.tool.clone(),
        );
        tokio::task::spawn_blocking(move || {
            // Local protection IPC can block too: do it before the final live
            // metadata/config/directory checks, never after selecting authority.
            if matches!(self.selected.effective, Effective::Delegate { .. }) {
                let protected = crate::google_identity::protection_gate(&home)?;
                if let Err(error) = delegation_key::load(&home, &protected)
                    && (!fallback || !error.fallback_eligible)
                {
                    return Err(error);
                }
            }
            crate::google_identity::credential_registry::authorize(
                &home, &reference, &agent, &tool,
            )
            .map_err(|message| Refusal::hard("credential_refused", message))?;
            let people = requester_gate::people_for(&home, users.as_ref());
            let now = Self::read(&home, &agent, people.as_ref())?;
            if now != self {
                return Err(changed());
            }
            if let Effective::Delegate { email } = &now.selected.effective {
                requester_gate::check(
                    now.selected.requesters,
                    &duduclaw_knowledge::requester::RequesterIdentity::Unknown,
                    &agent,
                    email,
                    people.as_ref(),
                )?;
                if let Err(error) = resolve::precheck(&home, email, people.as_ref())
                    && (!fallback || !error.fallback_eligible)
                {
                    return Err(error);
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| changed())??;
        Ok(backend)
    }
}
