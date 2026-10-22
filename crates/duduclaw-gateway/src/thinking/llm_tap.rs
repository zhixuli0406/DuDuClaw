//! A read-only [`ChatProvider`] decorator that reports a tool loop's
//! reasoning as thinking events.
//!
//! Wraps the provider a `duduclaw-llm` tool loop calls (openai-compat
//! runtime, operator Direct-API loop). It never changes a request or the
//! response handed back to the loop: reasoning parts (and their signatures /
//! redacted data) go back exactly as the provider returned them — only a
//! read-only copy of the text is reported, and signatures are never read.
//!
//! - Default: after each `complete()`, every `Reasoning` part is one block
//!   (`text_kind`, or `Omitted` when empty), every `RedactedReasoning` part a
//!   `Redacted` block; for open models (`Raw`) a leading inline `<think>`
//!   block in the answer text is reported too.
//! - `live` providers, when the turn's viewer reads thinking text live
//!   ([`super::policy`]`().wants_live()`): the call goes through the
//!   provider's `stream()` instead, `ReasoningDelta`s (and inline `<think>`
//!   text) are reported as they arrive, and the stream's final
//!   [`StreamEvent::Done`] response is what the loop gets.

use std::sync::Mutex;

use async_trait::async_trait;
use duduclaw_llm::{ChatProvider, ChatRequest, ChatResponse, ContentPart, LlmError, StreamEvent};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;

use super::event::ThinkingKind;
use super::recorder::Recorder;
use super::think_tags::{ThinkSplitter, leading_think_text};

/// See the module docs.
pub struct ReasoningTap<P> {
    inner: P,
    text_kind: ThinkingKind,
    live: bool,
    /// One recorder for the whole loop, so block ids stay unique across
    /// rounds.
    rec: Mutex<Recorder>,
}

impl<P> ReasoningTap<P> {
    /// `source` names the producer (`openai_compat`, `anthropic_api`);
    /// `text_kind` is what a reasoning text is (`Raw` for open models,
    /// `Summary` for vendor summaries); `live` allows the streaming path.
    pub fn new(inner: P, source: &'static str, text_kind: ThinkingKind, live: bool) -> Self {
        Self::with_recorder(inner, Recorder::new(source), text_kind, live)
    }

    pub fn with_recorder(inner: P, rec: Recorder, text_kind: ThinkingKind, live: bool) -> Self {
        Self {
            inner,
            text_kind,
            live,
            rec: Mutex::new(rec),
        }
    }

    fn rec(&self) -> std::sync::MutexGuard<'_, Recorder> {
        self.rec.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Report a finished response's reasoning (non-streaming path).
    fn report_parts(&self, resp: &ChatResponse) {
        let mut rec = self.rec();
        let mut any = false;
        for part in &resp.parts {
            match part {
                ContentPart::Reasoning { text, .. } => {
                    any = true;
                    let shown = Some(text.as_str()).filter(|t| !t.trim().is_empty());
                    let kind = if shown.is_some() {
                        self.text_kind
                    } else {
                        ThinkingKind::Omitted
                    };
                    rec.oneshot(kind, shown, None);
                }
                ContentPart::RedactedReasoning { .. } => {
                    any = true;
                    rec.oneshot(ThinkingKind::Redacted, None, None);
                }
                _ => {}
            }
        }
        if !any && self.text_kind == ThinkingKind::Raw {
            if let Some(inline) = leading_think_text(&resp.text()) {
                rec.oneshot(ThinkingKind::Raw, Some(inline), None);
            }
        }
    }

    /// Drain a stream, reporting reasoning as it arrives; the `Done`
    /// response is returned untouched.
    async fn drain(
        &self,
        mut stream: BoxStream<'static, Result<StreamEvent, LlmError>>,
    ) -> Result<ChatResponse, LlmError> {
        let mut splitter = ThinkSplitter::new();
        let mut live_text = false;
        while let Some(ev) = stream.next().await {
            match ev? {
                StreamEvent::ReasoningDelta(d) => {
                    live_text |= self.live_delta(&d);
                }
                StreamEvent::TextDelta(t) if self.text_kind == ThinkingKind::Raw => {
                    let reasoning = splitter.push(&t);
                    live_text |= self.live_delta(&reasoning);
                }
                StreamEvent::Done(resp) => {
                    let tail = splitter.finish();
                    live_text |= self.live_delta(&tail);
                    let mut rec = self.rec();
                    if rec.is_open() {
                        rec.end_as(self.text_kind, None, None);
                    }
                    drop(rec);
                    if !live_text {
                        self.report_parts(&resp);
                    }
                    return Ok(resp);
                }
                _ => {}
            }
        }
        // A stream without its `Done` event is a broken response.
        let mut rec = self.rec();
        if rec.is_open() {
            rec.end_as(self.text_kind, None, None);
        }
        Err(LlmError::Parse("stream ended without a final response".into()))
    }

    /// Report one live reasoning delta; `true` when it carried text.
    fn live_delta(&self, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        let mut rec = self.rec();
        if !rec.is_open() {
            rec.start(self.text_kind);
        }
        rec.delta(delta);
        true
    }
}

#[async_trait]
impl<P: ChatProvider> ChatProvider for ReasoningTap<P> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        if self.live && super::policy().wants_live() {
            let stream = self.inner.stream(req).await?;
            return self.drain(stream).await;
        }
        let resp = self.inner.complete(req).await?;
        self.report_parts(&resp);
        Ok(resp)
    }

    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        // The tool loops only call `complete()`; pass through untouched.
        self.inner.stream(req).await
    }
}

#[cfg(test)]
#[path = "llm_tap_tests.rs"]
mod tests;
