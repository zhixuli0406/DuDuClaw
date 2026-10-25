//! Private credential owner sender. No bearer/session/raw response outlet.
use crate::google_identity::effect_authority::RemoteRequestBound;
use duduclaw_llm::{OwnedScopedFailure, OwnedScopedReply};
use serde_json::Value;
mod program {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_identity/closed_mcp_program.inc.rs"
    ));
}
pub(crate) struct CompletedMcpReply(OwnedScopedReply);
impl CompletedMcpReply {
    pub(crate) fn into_reply(self) -> OwnedScopedReply {
        self.0
    }
}
struct Transport<'a, 'b> {
    view: super::HomeOAuthBearer<'a>,
    bound: &'b RemoteRequestBound,
    client: reqwest::Client,
    session: Option<String>,
    captured_sessions: Vec<String>,
    may_have_sent: bool,
    posts: usize,
}
impl Transport<'_, '_> {
    fn failure(&self) -> OwnedScopedFailure {
        if self.may_have_sent {
            OwnedScopedFailure::Unconfirmed
        } else {
            OwnedScopedFailure::Denied
        }
    }
    fn check(&self) -> Result<(), OwnedScopedFailure> {
        let token = self.view.material.access.as_str();
        if token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
        {
            return Err(self.failure());
        }
        self.view
            .permit
            .validate_mcp(self.view.material, self.bound)
            .map_err(|_| self.failure())
    }
    fn at(&self, _stage: &str) -> Result<(), OwnedScopedFailure> {
        #[cfg(test)]
        self.view.permit.test_mcp_boundary(_stage);
        self.check()
    }
    fn secrets(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.view.material.access.as_str())
            .chain(self.captured_sessions.iter().map(String::as_str))
    }
    fn scrub_value(&self, value: &mut Value) -> Result<(), OwnedScopedFailure> {
        fn scrub(value: &mut Value, secret: &str) -> bool {
            match value {
                Value::String(s) => {
                    *s = s.replace(secret, "•••");
                    true
                }
                Value::Array(a) => a.iter_mut().all(|v| scrub(v, secret)),
                Value::Object(o) => {
                    !o.keys().any(|k| k.contains(secret))
                        && o.values_mut().all(|v| scrub(v, secret))
                }
                _ => true,
            }
        }
        fn remains(value: &Value, secret: &str) -> bool {
            match value {
                Value::String(s) => s.contains(secret),
                Value::Array(a) => a.iter().any(|v| remains(v, secret)),
                Value::Object(o) => o
                    .iter()
                    .any(|(k, v)| k.contains(secret) || remains(v, secret)),
                _ => false,
            }
        }
        for secret in self.secrets() {
            if secret.is_empty() || !scrub(value, secret) {
                return Err(OwnedScopedFailure::Unconfirmed);
            }
        }
        if self.secrets().any(|secret| remains(value, secret)) {
            return Err(OwnedScopedFailure::Unconfirmed);
        }
        Ok(())
    }
    async fn send(
        &mut self,
        frame: &[u8],
        capture_session: bool,
    ) -> Result<(String, Vec<u8>), OwnedScopedFailure> {
        self.at("before_builder")?;
        let expected = match self.posts {
            0 => self.bound.initialize(),
            1 => self.bound.notification(),
            2 => self.bound.final_frame(),
            _ => return Err(self.failure()),
        };
        if frame != expected || capture_session != (self.posts == 0) {
            return Err(self.failure());
        }
        if self.posts >= 3 || frame.len() > 16 * 1024 * 1024 {
            return Err(self.failure());
        }
        let mut builder = self
            .client
            .post(self.bound.endpoint())
            .bearer_auth(self.view.material.access.as_str())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in self.bound.headers() {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(session) = &self.session {
            let mut value =
                reqwest::header::HeaderValue::from_str(session).map_err(|_| self.failure())?;
            value.set_sensitive(true);
            builder = builder.header("mcp-session-id", value);
        }
        let request = builder
            .body(frame.to_vec())
            .build()
            .map_err(|_| self.failure())?;
        self.at("before_send")?;
        self.may_have_sent = true;
        self.posts += 1;
        let pending =
            tokio::time::timeout_at(self.bound.deadline(), self.client.execute(request)).await;
        self.at("after_send")?;
        let mut response = pending
            .map_err(|_| self.failure())?
            .map_err(|_| self.failure())?;
        {
            if let Some(value) = response.headers().get("mcp-session-id") {
                let session = value.to_str().map_err(|_| self.failure())?;
                if session.is_empty() {
                    return Err(self.failure());
                }
                // Every response can rotate a session. Retain all three
                // possible values until the last representation scrub.
                if !self.captured_sessions.iter().any(|old| old == session) {
                    self.captured_sessions.push(session.to_owned());
                }
                self.session = Some(session.to_owned());
            }
        }
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|n| n > 16 * 1024 * 1024)
        {
            return Err(self.failure());
        }
        let kind = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut bytes = Vec::new();
        loop {
            let next = tokio::time::timeout_at(self.bound.deadline(), response.chunk()).await;
            self.at("after_chunk")?;
            match next
                .map_err(|_| self.failure())?
                .map_err(|_| self.failure())?
            {
                Some(chunk) => {
                    if bytes
                        .len()
                        .checked_add(chunk.len())
                        .is_none_or(|n| n > 16 * 1024 * 1024)
                    {
                        return Err(self.failure());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                None => {
                    self.at("after_eof")?;
                    break;
                }
            }
        }
        if !status.is_success() {
            return Err(self.failure());
        }
        Ok((kind, bytes))
    }
}
#[async_trait::async_trait]
impl program::LocalIo for Transport<'_, '_> {
    async fn post(
        &mut self,
        frame: &[u8],
        capture_session: bool,
    ) -> Result<(String, Vec<u8>), OwnedScopedFailure> {
        self.send(frame, capture_session).await
    }
    fn checkpoint(&mut self) -> Result<(), OwnedScopedFailure> {
        #[cfg(test)]
        if self.may_have_sent {
            self.view.permit.test_mcp_boundary("after_codec");
        }
        self.check()
    }
    fn scrub(&self, value: &mut Value) -> Result<(), OwnedScopedFailure> {
        self.scrub_value(value)
    }
    fn identifier(&self, value: &str) -> Result<(), OwnedScopedFailure> {
        if self.secrets().any(|s| value.contains(s)) {
            Err(OwnedScopedFailure::Unconfirmed)
        } else {
            Ok(())
        }
    }
}
impl super::HomeOAuthBearer<'_> {
    pub(crate) async fn execute_mcp(
        self,
        bound: &RemoteRequestBound,
    ) -> Result<CompletedMcpReply, OwnedScopedFailure> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| OwnedScopedFailure::Denied)?;
        let mut io = Transport {
            view: self,
            bound,
            client,
            session: None,
            captured_sessions: Vec::new(),
            may_have_sent: false,
            posts: 0,
        };
        io.check()?;
        let pending = program::run(
            &mut io,
            bound,
            Some("本次使用已核准的公司共用 Google 連線；系統未驗證此連線的 Google 主體。".into()),
        )
        .await;
        io.at("after_program")?;
        let reply = pending?;
        // Program finishes every known codec and final bearer/session scrub.
        io.at("before_completed")?;
        #[cfg(test)]
        io.view.permit.test_mcp_completed(&reply);
        io.check()?;
        Ok(CompletedMcpReply(reply))
    }
}

#[cfg(test)]
mod completed_type_contract {
    use super::*;
    #[test]
    fn completed_mcp_reply_has_no_raw_factory_clone_debug_or_deserialization() {
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
        struct IfFromReply;
        impl<T: ?Sized + From<OwnedScopedReply>> Ambiguous<IfFromReply> for T {}
        let _ = <CompletedMcpReply as Ambiguous<_>>::marker;
    }
}
