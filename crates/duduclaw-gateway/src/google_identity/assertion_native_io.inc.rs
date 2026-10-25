// The same business source is compiled privately below this owner.
use crate::google_workspace::native_call::NativeCallTemplate;
use crate::google_workspace::GoogleApiError;
use crate::google_identity::effect_authority::ClosedNativeFailure;
use serde_json::{Value, json};
mod program {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_workspace/native_program.inc.rs"
    ));
}

pub(crate) struct CompletedNativeOutput(Value);
impl CompletedNativeOutput {
    pub(crate) fn into_value(self) -> Value {
        self.0
    }
}
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const AGGREGATE_LIMIT: usize = 16 * 1024 * 1024;
const RECIPE_LIMIT: usize = 1024 * 1024;
const MAX_REQUESTS: usize = 26;

struct Transport<'a, 't> {
    view: super::Bearer<'a>,
    template: &'t NativeCallTemplate,
    client: reqwest::Client,
    requests: usize,
    received: usize,
    may_have_sent: bool,
    failure: Option<ClosedNativeFailure>,
}
impl Transport<'_, '_> {
    fn marker() -> GoogleApiError {
        GoogleApiError::Api {
            status: 502,
            message: "Google operation could not be confirmed.".into(),
        }
    }
    fn fail(&mut self) -> GoogleApiError {
        if self.failure.is_none() {
            self.failure = Some(ClosedNativeFailure::refused(self.may_have_sent));
        }
        Self::marker()
    }
    fn check(&mut self) -> Result<(), GoogleApiError> {
        // The RFC 6750 token alphabet cannot hide a reflection through JSON escaping.
        if self.view.material.access.as_str().is_empty()
            || !self
                .view
                .material
                .access
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
        {
            if self.failure.is_none() {
                self.failure = Some(ClosedNativeFailure::refused(self.may_have_sent));
            }
        }
        if let Err(error) =
            super::validate_native(&self.view.permit, self.view.material, self.template)
        {
            self.failure = Some(ClosedNativeFailure::validation(error, self.may_have_sent));
        }
        if self.failure.is_some() {
            Err(Self::marker())
        } else {
            Ok(())
        }
    }
    fn at(&mut self, _stage: &str) -> Result<(), GoogleApiError> {
        #[cfg(test)]
        self.view.permit.test_native_boundary(_stage);
        self.check()
    }
    fn endpoint(&mut self, original: &str) -> Result<reqwest::Url, GoogleApiError> {
        let url = match reqwest::Url::parse(original) {
            Ok(u) => u,
            Err(_) => return Err(self.fail()),
        };
        let valid = url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.port().is_none_or(|p| p == 443)
            && match url.host_str() {
                Some("gmail.googleapis.com") => url.path().starts_with("/gmail/v1/users/me/"),
                Some("www.googleapis.com") => {
                    url.path()
                        .starts_with("/calendar/v3/calendars/primary/events")
                        || url.path().starts_with("/drive/v3/files")
                }
                Some("sheets.googleapis.com") => url.path().starts_with("/v4/spreadsheets/"),
                Some("forms.googleapis.com") => url.path().starts_with("/v1/forms/"),
                Some("tasks.googleapis.com") => url.path().starts_with("/tasks/v1/"),
                Some("docs.googleapis.com") => url.path().starts_with("/v1/documents/"),
                Some("slides.googleapis.com") => url.path().starts_with("/v1/presentations/"),
                _ => false,
            };
        if !valid {
            return Err(self.fail());
        }
        #[cfg(test)]
        {
            return match self.view.permit.test_endpoint(original) {
                Some(url) => Ok(url),
                None => Err(self.fail()),
            };
        }
        #[cfg(not(test))]
        Ok(url)
    }
    async fn send(
        &mut self,
        step: program::NativeStep<'_>,
    ) -> Result<(u16, Vec<u8>), GoogleApiError> {
        self.at("before_builder")?;
        if !std::ptr::eq(step.template(), self.template)
            || self.requests >= MAX_REQUESTS
            || self.view.material.access.as_str().is_empty()
        {
            return Err(self.fail());
        }
        if program::contains_representation(step.url(), self.view.material.access.as_str())
            || step.query().iter().any(|(k, v)| {
                program::contains_representation(k, self.view.material.access.as_str())
                    || program::contains_representation(v, self.view.material.access.as_str())
            })
        {
            return Err(self.fail());
        }
        let serialized = match step.body() {
            Some(v) => match serde_json::to_vec(v) {
                Ok(b) => b,
                Err(_) => return Err(self.fail()),
            },
            None => Vec::new(),
        };
        if serialized.len() > RECIPE_LIMIT
            || program::contains_representation(
                &String::from_utf8_lossy(&serialized),
                self.view.material.access.as_str(),
            )
        {
            return Err(self.fail());
        }
        if step.body().is_some_and(|body| {
            program::contains_value_representation(body, self.view.material.access.as_str())
        }) {
            return Err(self.fail());
        }
        let endpoint = self.endpoint(step.url())?;
        // The raw header, builder and authenticated request never leave owner.
        let mut builder = self
            .client
            .request(step.method().clone(), endpoint)
            .bearer_auth(self.view.material.access.as_str())
            .query(step.query());
        if let Some(body) = step.body() {
            builder = builder.json(body);
        }
        let request = match builder.build() {
            Ok(r) => r,
            Err(_) => return Err(self.fail()),
        };
        if request
            .url()
            .as_str()
            .len()
            .checked_add(serialized.len())
            .is_none_or(|n| n > RECIPE_LIMIT)
        {
            return Err(self.fail());
        }
        self.at("before_send")?;
        self.requests += 1;
        self.may_have_sent = true;
        let pending = self.client.execute(request).await;
        self.at("after_send")?;
        let mut response = match pending {
            Ok(r) => r,
            Err(_) => return Err(self.fail()),
        };
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|n| n > RESPONSE_LIMIT as u64)
        {
            return Err(self.fail());
        }
        let mut bytes = Vec::new();
        loop {
            let next = response.chunk().await;
            self.at("after_chunk")?;
            match next {
                Ok(Some(chunk)) => {
                    let n = match bytes.len().checked_add(chunk.len()) {
                        Some(n) => n,
                        None => return Err(self.fail()),
                    };
                    let total = match self.received.checked_add(chunk.len()) {
                        Some(n) => n,
                        None => return Err(self.fail()),
                    };
                    if n > RESPONSE_LIMIT || total > AGGREGATE_LIMIT {
                        return Err(self.fail());
                    }
                    bytes.extend_from_slice(&chunk);
                    self.received = total;
                }
                Ok(None) => {
                    self.at("after_eof")?;
                    break;
                }
                Err(_) => return Err(self.fail()),
            }
        }
        Ok((status, bytes))
    }
    fn status_error(&mut self, status: u16, bytes: &[u8]) -> GoogleApiError {
        if self.template.tool().ends_with("create_draft")
            || matches!(
                self.template.tool(),
                "calendar_create_event"
                    | "sheets_append"
                    | "gtasks_create"
                    | "gtasks_complete"
                    | "docs_append"
            )
        {
            return self.fail();
        }
        // Decode and scrub before a curated error helper can truncate a secret.
        let raw = String::from_utf8_lossy(bytes);
        let clean = if let Ok(mut value) = serde_json::from_slice::<Value>(bytes) {
            if !program::scrub_final(&mut value, self.view.material.access.as_str()) {
                return self.fail();
            }
            match serde_json::to_string(&value) {
                Ok(s) => s,
                Err(_) => return self.fail(),
            }
        } else {
            raw.replace(self.view.material.access.as_str(), "•••")
        };
        match status {
            401 => GoogleApiError::Unauthorized,
            403 => GoogleApiError::Forbidden(program::scope_guidance(&clean)),
            429 => GoogleApiError::RateLimited,
            _ => GoogleApiError::Api {
                status,
                message: duduclaw_core::truncate_chars(&program::extract_api_message(&clean), 240),
            },
        }
    }
}
#[async_trait::async_trait]
impl program::LocalIo for Transport<'_, '_> {
    async fn json(&mut self, step: program::NativeStep<'_>) -> Result<Value, GoogleApiError> {
        let (status, bytes) = self.send(step).await?;
        if !(200..300).contains(&status) {
            let error = self.status_error(status, &bytes);
            self.at("after_decode")?;
            return Err(error);
        }
        let decoded = if bytes.iter().all(u8::is_ascii_whitespace) {
            Ok(Value::Null)
        } else {
            serde_json::from_slice::<Value>(&bytes)
        };
        self.at("after_decode")?;
        match decoded {
            Ok(value) => Ok(value),
            Err(_) => Err(self.fail()),
        }
    }
    async fn text(&mut self, step: program::NativeStep<'_>) -> Result<String, GoogleApiError> {
        let (status, bytes) = self.send(step).await?;
        if !(200..300).contains(&status) {
            let error = self.status_error(status, &bytes);
            self.at("after_decode")?;
            return Err(error);
        }
        let decoded = String::from_utf8_lossy(&bytes).into_owned();
        self.at("after_decode")?;
        Ok(decoded)
    }
    fn checkpoint(&mut self) -> Result<(), GoogleApiError> {
        self.check()
    }
    fn reject_derived(&mut self, candidate: &str) -> Result<(), GoogleApiError> {
        self.check()?;
        if program::contains_representation(candidate, self.view.material.access.as_str()) {
            Err(self.fail())
        } else {
            Ok(())
        }
    }
    fn sanitize(&self, value: &str) -> String {
        value.replace(self.view.material.access.as_str(), "•••")
    }
    fn closed(&self) -> bool {
        true
    }
}
impl super::Bearer<'_> {
    pub(crate) async fn execute_native(
        self,
        template: &NativeCallTemplate,
    ) -> Result<CompletedNativeOutput, ClosedNativeFailure> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| ClosedNativeFailure::refused(false))?;
        let mut io = Transport {
            view: self,
            template,
            client,
            requests: 0,
            received: 0,
            may_have_sent: false,
            failure: None,
        };
        io.check().map_err(|_| {
            io.failure
                .take()
                .unwrap_or(ClosedNativeFailure::refused(false))
        })?;
        let mut value = Box::pin(program::run(template, &mut io)).await;
        let _ = io.at("after_program");
        if let Some(error) = io.failure.take() {
            return Err(error);
        }
        // The fixed source notice is part of the owner-scrubbed result.
        if let Some(content) = value.get_mut("content").and_then(Value::as_array_mut) {
            content.insert(
                0,
                json!({"type":"text","text":super::source_notice(io.view.material)}),
            );
        }
        if !program::scrub_final(&mut value, io.view.material.access.as_str()) {
            return Err(ClosedNativeFailure::Unconfirmed);
        }
        let _ = io.at("after_scrub");
        if let Some(error) = io.failure.take() {
            return Err(error);
        }
        if serde_json::to_vec(&json!({"ok":true,"result":&value}))
            .map_or(true, |b| b.len() > 512 * 1024)
        {
            return Err(ClosedNativeFailure::Unconfirmed);
        }
        io.check().map_err(|_| {
            io.failure
                .take()
                .unwrap_or(ClosedNativeFailure::Unconfirmed)
        })?;
        #[cfg(test)]
        io.view.permit.test_completed(&value);
        io.check().map_err(|_| {
            io.failure
                .take()
                .unwrap_or(ClosedNativeFailure::Unconfirmed)
        })?;
        Ok(CompletedNativeOutput(value))
    }
}
impl program::LocalRender for Transport<'_, '_> {
    fn inspect_curated(&mut self, value: &Value) -> Result<(), GoogleApiError> {
        self.check()?;
        if program::key_contains_secret(value, self.view.material.access.as_str()) {
            Err(self.fail())
        } else {
            Ok(())
        }
    }
    fn refuse(&mut self) -> GoogleApiError {
        self.fail()
    }
}

#[cfg(test)]
mod completed_type_contract {
    use super::*;
    #[test]
    fn completed_output_does_not_supply_clone_debug_or_deserialization() {
        trait Ambiguous<A> {
            fn marker() {}
        }
        impl<T: ?Sized> Ambiguous<()> for T {}
        struct IfClone;
        impl<T: ?Sized + Clone> Ambiguous<IfClone> for T {}
        struct IfDebug;
        impl<T: ?Sized + std::fmt::Debug> Ambiguous<IfDebug> for T {}
        struct IfDeserialize;
        impl<T: ?Sized + serde::de::DeserializeOwned> Ambiguous<IfDeserialize> for T {}
        struct IfFromValue;
        impl<T: ?Sized + From<Value>> Ambiguous<IfFromValue> for T {}
        let _ = <CompletedNativeOutput as Ambiguous<_>>::marker;
    }
}

#[cfg(test)]
impl super::Bearer<'_> {
    pub(crate) async fn test_foreign_native_step(
        self,
        expected: &NativeCallTemplate,
        other: &NativeCallTemplate,
    ) -> bool {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let mut io = Transport {
            view: self,
            template: expected,
            client,
            requests: 0,
            received: 0,
            may_have_sent: false,
            failure: None,
        };
        let result = program::submit_foreign_fixture_step(&mut io, other).await;
        result.is_err() && !io.may_have_sent && io.requests == 0
    }
}
