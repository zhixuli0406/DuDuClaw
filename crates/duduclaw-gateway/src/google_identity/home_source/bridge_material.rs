//! Apps Script credential owner. No secret, result capability or raw reply exits.
use crate::google_identity::{
    Refusal,
    effect_authority::{BridgeEffectPermit, ClosedNativeFailure},
};
use crate::google_workspace::{
    GoogleApiError,
    native_call::{NativeCallTemplate, ParsedNativeArguments},
};
use super::w3_observation::FileProof;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;
mod program {
    fn bridge_cell_to_string(value: &serde_json::Value) -> String {
        super::render::cell_to_string(value)
    }
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_workspace/bridge_program.inc.rs"
    ));
}
mod render {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_workspace/native_program.inc.rs"
    ));
}
fn changed() -> Refusal {
    Refusal::hard(
        "identity_changed",
        "公司 Apps Script 來源已變更，請重新選擇並核准。",
    )
}
enum ResolverProof {
    Snapshot,
    Env { name: String, digest: [u8; 32] },
    File(FileProof),
}
pub(crate) struct Resolved {
    url: Zeroizing<String>,
    secret: Zeroizing<String>,
    resolver: ResolverProof,
    home: PathBuf,
}
pub(crate) async fn resolve(
    home: &Path,
    url: String,
    stored: String,
    deadline: tokio::time::Instant,
    permit: &crate::google_identity::effect_authority::GooglePreflightPermit<'_>,
) -> Result<Resolved, Refusal> {
    use duduclaw_security::secret_manager::{
        SecretUri, SecretBackend, SecretManagerConfig, FileSecretAdapter,
    };
    use sha2::{Digest, Sha256};
    check(deadline)?;
    permit.validate()?;
    deployment(&url)?;
    let pending =
        tokio::time::timeout_at(deadline, crate::google_personal::grants::acquire(home)).await;
    check(deadline)?;
    permit.validate()?;
    let _resolution_lease = pending.map_err(|_| changed())?.map_err(|_| changed())?;
    // Preserve from_single's one decrypt attempt, then exactly one pointer hop.
    let decrypted = if stored.starts_with("secret://") {
        None
    } else {
        duduclaw_security::keyfile::decrypt_keyfile_value(stored.trim(), home).map(Zeroizing::new)
    };
    check(deadline)?;
    permit.validate()?;
    let selected = decrypted
        .as_deref()
        .map(String::as_str)
        .unwrap_or(stored.trim());
    let (secret, resolver) = if selected.starts_with("secret://") {
        let uri = SecretUri::parse(selected).map_err(|_| changed())?;
        match uri.backend {
            SecretBackend::File => {
                let path = FileSecretAdapter::new(home)
                    .closed_vetted_path(&uri.name)
                    .map_err(|_| changed())?;
                let (proof, bytes) = FileProof::capture(Path::new(&uri.name), 64 * 1024)?;
                if FileSecretAdapter::new(home)
                    .closed_vetted_path(&uri.name)
                    .map_err(|_| changed())?
                    != path
                {
                    return Err(changed());
                }
                let text = std::str::from_utf8(&bytes).map_err(|_| changed())?;
                let value = text
                    .strip_suffix('\n')
                    .map(|s| s.strip_suffix('\r').unwrap_or(s))
                    .unwrap_or(text);
                (Zeroizing::new(value.to_owned()), ResolverProof::File(proof))
            }
            SecretBackend::Env => {
                let value = std::env::var(&uri.name).map_err(|_| changed())?;
                let digest = Sha256::digest(value.as_bytes()).into();
                (
                    Zeroizing::new(value),
                    ResolverProof::Env {
                        name: uri.name,
                        digest,
                    },
                )
            }
            _ => {
                let (_, bytes) = FileProof::capture(&home.join("config.toml"), 1024 * 1024)?;
                let config = SecretManagerConfig::from_toml_str(
                    std::str::from_utf8(&bytes).map_err(|_| changed())?,
                )
                .map_err(|_| changed())?;
                check(deadline)?;
                permit.validate()?;
                let pending = tokio::time::timeout_at(
                    deadline,
                    duduclaw_security::secret_ref::SecretRef::from_single(selected)
                        .resolve(&config, home),
                )
                .await;
                check(deadline)?;
                permit.validate()?;
                (
                    Zeroizing::new(
                        pending
                            .map_err(|_| changed())?
                            .ok_or_else(changed)?
                            .expose_owned(),
                    ),
                    ResolverProof::Snapshot,
                )
            }
        }
    } else {
        (Zeroizing::new(selected.to_owned()), ResolverProof::Snapshot)
    };
    drop(decrypted);
    drop(stored);
    check(deadline)?;
    permit.validate()?;
    if secret.is_empty() || secret.len() > 64 * 1024 {
        return Err(changed());
    }
    let resolved = Resolved {
        url: Zeroizing::new(url),
        secret,
        resolver,
        home: home.into(),
    };
    resolved.validate()?;
    Ok(resolved)
}
fn check(deadline: tokio::time::Instant) -> Result<(), Refusal> {
    if tokio::time::Instant::now() >= deadline {
        Err(Refusal::hard(
            "selection_expired",
            "Google 帳號準備逾時，請重新操作。",
        ))
    } else {
        Ok(())
    }
}
fn safe_url(value: &str, host: &str) -> Result<reqwest::Url, Refusal> {
    let url = reqwest::Url::parse(value).map_err(|_| changed())?;
    if url.scheme() != "https"
        || url.host_str() != Some(host)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|p| p != 443)
    {
        return Err(changed());
    }
    Ok(url)
}
fn deployment(value: &str) -> Result<reqwest::Url, Refusal> {
    let url = safe_url(value, "script.google.com")?;
    let pieces: Vec<_> = url.path().split('/').collect();
    if pieces.len() != 5
        || pieces[1] != "macros"
        || pieces[2] != "s"
        || pieces[3].is_empty()
        || !pieces[3]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        || pieces[4] != "exec"
        || url.query().is_some()
    {
        return Err(changed());
    }
    Ok(url)
}
fn result_handoff(value: &str) -> Result<reqwest::Url, Refusal> {
    if value.len() > 8192 {
        return Err(changed());
    }
    let url = safe_url(value, "script.googleusercontent.com")?;
    if url.path() != "/macros/echo" || url.query().is_none_or(str::is_empty) {
        return Err(changed());
    }
    Ok(url)
}
impl Resolved {
    pub(crate) fn validate(&self) -> Result<(), Refusal> {
        use sha2::{Digest, Sha256};
        match &self.resolver {
            ResolverProof::Snapshot => Ok(()),
            ResolverProof::File(p) => p.validate(),
            ResolverProof::Env { name, digest } => {
                let value = std::env::var(name).map_err(|_| changed())?;
                if <[u8; 32]>::from(Sha256::digest(value.as_bytes())) != *digest {
                    Err(changed())
                } else {
                    Ok(())
                }
            }
        }
    }
    pub(crate) fn weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.url.capacity()
            + self.secret.capacity()
            + self.home.as_os_str().len()
            + match &self.resolver {
                ResolverProof::Snapshot => 0,
                ResolverProof::Env { name, .. } => name.capacity(),
                ResolverProof::File(p) => p.weight(),
            }
    }
    pub(crate) fn source_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(self.url.as_bytes()))
    }
    pub(crate) async fn execute_native(
        &self,
        permit: BridgeEffectPermit<'_>,
        template: &NativeCallTemplate,
    ) -> Result<CompletedNativeOutput, ClosedNativeFailure> {
        permit
            .validate(self, template)
            .map_err(|e| ClosedNativeFailure::validation(e, false))?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .build()
            .map_err(|_| ClosedNativeFailure::refused(false))?;
        let mut io = Transport {
            material: self,
            permit,
            template,
            client,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(60),
            sent: false,
            handoff: None,
            failure: None,
        };
        use ParsedNativeArguments::*;
        let pending = match template.parsed() {
            GmailSearch { query, max } => program::gmail_search(&mut io, query, *max)
                .await
                .map(render::Outcome::GmailSearch),
            GmailRead { message_id } => program::gmail_read(&mut io, message_id)
                .await
                .map(render::Outcome::GmailRead),
            GmailDraft {
                to,
                subject,
                body,
                cc,
            } => program::gmail_create_draft(&mut io, to, subject, body, cc.as_deref())
                .await
                .map(render::Outcome::GmailDraft),
            CalendarList { min, max_time, max } => {
                program::calendar_list_events(&mut io, min.as_deref(), max_time.as_deref(), *max)
                    .await
                    .map(render::Outcome::CalendarList)
            }
            CalendarCreate {
                summary,
                start,
                end,
                description,
                attendees,
                with_meet,
            } => program::calendar_create_event(
                &mut io,
                summary,
                start,
                end,
                description.as_deref(),
                attendees.as_deref(),
                *with_meet,
            )
            .await
            .map(render::Outcome::CalendarCreate),
            SheetsRead { spreadsheet, range } => program::sheets_read(&mut io, spreadsheet, range)
                .await
                .map(render::Outcome::SheetsRead),
            SheetsAppend {
                spreadsheet,
                range,
                values,
            } => program::sheets_append(&mut io, spreadsheet, range, values.clone())
                .await
                .map(render::Outcome::SheetsAppend),
            _ => Err(GoogleApiError::Api {
                status: 400,
                message: "這個工具需要 Google 直接連線，Apps Script 不支援。".into(),
            }),
        };
        io.at("after_program")?;
        let mut value = match pending {
            Ok(outcome) => render::render(outcome, &mut io),
            Err(error) => crate::google_workspace::native_call::tool_error(&error.to_string()),
        };
        io.at("after_render")?;
        if let Some(content) = value.get_mut("content").and_then(Value::as_array_mut) {
            content.insert(0,json!({"type":"text","text":"本次使用已核准的公司 Apps Script 連線；系統未驗證此連線的 Google 主體。"}));
        }
        io.reject(&value)?;
        io.at("after_scrub")?;
        if serde_json::to_vec(&json!({"ok":true,"result":&value}))
            .map_or(true, |v| v.len() > 512 * 1024)
        {
            return Err(ClosedNativeFailure::Unconfirmed);
        }
        #[cfg(test)]
        io.permit.test_completed(&value);
        io.at("before_completed")?;
        Ok(CompletedNativeOutput(value))
    }
}
pub(crate) struct CompletedNativeOutput(Value);
impl CompletedNativeOutput {
    pub(crate) fn into_value(self) -> Value {
        self.0
    }
}
struct Transport<'a, 't> {
    material: &'a Resolved,
    permit: BridgeEffectPermit<'a>,
    template: &'t NativeCallTemplate,
    client: reqwest::Client,
    deadline: tokio::time::Instant,
    sent: bool,
    handoff: Option<Zeroizing<String>>,
    failure: Option<ClosedNativeFailure>,
}
impl Transport<'_, '_> {
    fn fail(&self) -> ClosedNativeFailure {
        ClosedNativeFailure::refused(self.sent)
    }
    fn at(&self, _stage: &str) -> Result<(), ClosedNativeFailure> {
        #[cfg(test)]
        self.permit.test_boundary(_stage);
        if self.failure.is_some() || tokio::time::Instant::now() >= self.deadline {
            return Err(self.fail());
        }
        self.permit
            .validate(self.material, self.template)
            .map_err(|e| ClosedNativeFailure::validation(e, self.sent))
    }
    fn reject(&self, value: &Value) -> Result<(), ClosedNativeFailure> {
        let bad = [self.material.secret.as_str(), self.material.url.as_str()]
            .into_iter()
            .chain(self.handoff.iter().map(|v| v.as_str()))
            .any(|s| render::contains_value_representation(value, s));
        if bad { Err(self.fail()) } else { Ok(()) }
    }
    async fn response(
        &self,
        mut response: reqwest::Response,
    ) -> Result<Value, ClosedNativeFailure> {
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > 4 * 1024 * 1024)
        {
            return Err(self.fail());
        }
        let mut bytes = Vec::new();
        loop {
            let next = tokio::time::timeout_at(self.deadline, response.chunk()).await;
            self.at("after_chunk")?;
            match next.map_err(|_| self.fail())?.map_err(|_| self.fail())? {
                Some(chunk) => {
                    if bytes
                        .len()
                        .checked_add(chunk.len())
                        .is_none_or(|n| n > 4 * 1024 * 1024)
                    {
                        return Err(self.fail());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                None => {
                    self.at("after_eof")?;
                    break;
                }
            }
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| self.fail())?;
        self.at("after_decode")?;
        self.reject(&value)?;
        if value.get("error").is_some() {
            return Err(ClosedNativeFailure::Unconfirmed);
        }
        Ok(value)
    }
    async fn call_once(
        &mut self,
        action: crate::google_apps_script::BridgeAction,
        params: Value,
    ) -> Result<Value, ClosedNativeFailure> {
        self.at("before_builder")?;
        if self.sent || action.as_str() != self.template.tool() {
            return Err(self.fail());
        }
        self.reject(&params)?;
        let original = deployment(&self.material.url).map_err(|_| self.fail())?;
        #[cfg(test)]
        let original = self
            .permit
            .test_endpoint(original.as_str())
            .ok_or_else(|| self.fail())?;
        let body = json!({"secret":self.material.secret.as_str(),"action":action.as_str(),"params":params});
        let serialized = serde_json::to_vec(&body).map_err(|_| self.fail())?;
        if original
            .as_str()
            .len()
            .checked_add(serialized.len())
            .is_none_or(|n| n > 1024 * 1024)
        {
            return Err(self.fail());
        }
        let request = self
            .client
            .post(original)
            .header("content-type", "application/json")
            .body(serialized)
            .build()
            .map_err(|_| self.fail())?;
        self.at("before_send")?;
        self.sent = true;
        let pending = tokio::time::timeout_at(self.deadline, self.client.execute(request)).await;
        self.at("after_send")?;
        let response = pending.map_err(|_| self.fail())?.map_err(|_| self.fail())?;
        if matches!(response.status().as_u16(), 302 | 303) {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| self.fail())?
                .to_str()
                .map_err(|_| self.fail())?;
            let url = result_handoff(location).map_err(|_| self.fail())?;
            // A fixed result origin must not receive the original POST secret in
            // its URL. Reject before saving/building a GET; never redact and send.
            if handoff_contains_secret(location, &url, &self.material.secret) {
                return Err(self.fail());
            }
            self.handoff = Some(Zeroizing::new(location.to_owned()));
            drop(response);
            #[cfg(test)]
            let url = self
                .permit
                .test_endpoint(url.as_str())
                .ok_or_else(|| self.fail())?;
            // A fresh credential-free GET; never reuse the POST builder/body.
            let request = self.client.get(url).build().map_err(|_| self.fail())?;
            self.at("before_result_get")?;
            let pending =
                tokio::time::timeout_at(self.deadline, self.client.execute(request)).await;
            self.at("after_result_get")?;
            let response = pending.map_err(|_| self.fail())?.map_err(|_| self.fail())?;
            self.response(response).await
        } else {
            self.response(response).await
        }
    }
}
#[async_trait::async_trait]
impl program::LocalIo for Transport<'_, '_> {
    async fn call(
        &mut self,
        action: crate::google_apps_script::BridgeAction,
        params: Value,
    ) -> Result<Value, GoogleApiError> {
        match self.call_once(action, params).await {
            Ok(value) => Ok(value),
            Err(error) => {
                self.failure = Some(error);
                Err(GoogleApiError::Api {
                    status: 502,
                    message: "Google 操作結果無法確認，請先檢查是否已完成，勿直接重試。".into(),
                })
            }
        }
    }
}
impl render::LocalRender for Transport<'_, '_> {
    fn inspect_curated(&mut self, value: &Value) -> Result<(), GoogleApiError> {
        if self.reject(value).is_err() {
            self.failure = Some(ClosedNativeFailure::Unconfirmed);
            Err(self.refuse())
        } else {
            Ok(())
        }
    }
    fn refuse(&mut self) -> GoogleApiError {
        GoogleApiError::Api {
            status: 502,
            message: "Google operation unconfirmed.".into(),
        }
    }
}

#[cfg(test)]
mod w3_source_oracles {
    use super::*;
    #[test]
    fn w3_bridge_result_url_has_one_closed_handoff_shape() {
        assert!(
            result_handoff(
                "https://script.googleusercontent.com/macros/echo?user_content_key=fixture"
            )
            .is_ok()
        );
        for bad in [
            "http://script.googleusercontent.com/macros/echo?x=1",
            "https://script.googleusercontent.com.evil.test/macros/echo?x=1",
            "https://a@script.googleusercontent.com/macros/echo?x=1",
            "https://script.googleusercontent.com:444/macros/echo?x=1",
            "https://script.googleusercontent.com/macros/echo?x=1#f",
            "https://script.googleusercontent.com/exec?x=1",
            "/macros/echo?x=1",
            "https://script.googleusercontent.com/macros/echo",
        ] {
            assert!(result_handoff(bad).is_err());
        }
    }
}

fn handoff_contains_secret(raw: &str, url: &reqwest::Url, secret: &str) -> bool {
    render::contains_representation(raw, secret)
        || render::contains_representation(url.as_str(), secret)
        || url.query_pairs().any(|(key, value)| {
            render::contains_representation(&key, secret)
                || render::contains_representation(&value, secret)
        })
}
