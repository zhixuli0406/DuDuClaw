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
        req: &ChatRequest,
        mut stream: BoxStream<'static, Result<StreamEvent, LlmError>>,
    ) -> Result<ChatResponse, LlmError> {
        let mut splitter = ThinkSplitter::new();
        let mut live_text = false;
        while let Some(ev) = stream.next().await {
            let ev = match ev {
                Ok(ev) => ev,
                Err(e) => {
                    self.close_open();
                    return Err(e);
                }
            };
            match ev {
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
                    return Ok(with_estimated_usage(req, resp));
                }
                _ => {}
            }
        }
        // A stream without its `Done` event is a broken response.
        self.close_open();
        Err(LlmError::Parse("stream ended without a final response".into()))
    }

    fn close_open(&self) {
        let mut rec = self.rec();
        if rec.is_open() {
            rec.end_as(self.text_kind, None, None);
        }
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

/// A streamed response whose provider sent no usage chunk (input and output
/// both 0) gets a token estimate instead of 0, and the call is flagged as
/// estimated for cost telemetry (`cost_telemetry::mark_usage_estimated`).
/// A response that carries any usage is returned unchanged.
fn with_estimated_usage(req: &ChatRequest, mut resp: ChatResponse) -> ChatResponse {
    if resp.usage.input_tokens != 0 || resp.usage.output_tokens != 0 {
        return resp;
    }
    let output: u64 = resp
        .parts
        .iter()
        .map(|p| match p {
            ContentPart::Text(t) => duduclaw_llm::estimate_tokens(t),
            ContentPart::Reasoning { text, .. } => duduclaw_llm::estimate_tokens(text),
            ContentPart::ToolCall { name, args, .. } => {
                duduclaw_llm::estimate_tokens(name)
                    + duduclaw_llm::estimate_tokens(&args.to_string())
            }
            _ => 0,
        })
        .sum();
    resp.usage.input_tokens = req.estimate_input_tokens();
    resp.usage.output_tokens = output;
    crate::cost_telemetry::mark_usage_estimated();
    tracing::debug!(
        provider = %resp.provider,
        input = resp.usage.input_tokens,
        output = resp.usage.output_tokens,
        "streamed response carried no usage — recorded an estimate"
    );
    resp
}

#[async_trait]
impl<P: ChatProvider> ChatProvider for ReasoningTap<P> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        if self.live && super::policy().wants_live() {
            match self.inner.stream(req).await {
                Ok(stream) => return self.drain(req, stream).await,
                // A backend that cannot stream this request (no SSE, rejects
                // `stream_options`, …) must not fail a turn that worked
                // before thinking display: answer this call without
                // streaming. An error after the first event is returned as
                // is (the call may already have had effects upstream).
                Err(e) => tracing::debug!(
                    provider = self.inner.id(),
                    error = %e,
                    "thinking display: streaming refused — falling back to a plain call"
                ),
            }
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
