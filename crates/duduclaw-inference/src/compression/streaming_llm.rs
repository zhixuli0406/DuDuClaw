//! StreamingLLM — attention sink + sliding window KV-cache management.
//!
//! Enables infinite-length conversation without running out of memory by:
//! 1. Keeping the first few tokens (attention sinks) permanently in KV-cache
//! 2. Maintaining a sliding window of recent tokens
//! 3. Evicting old tokens outside the window
//!
//! This is applied at the session/conversation level, not the model level.
//! The model sees: [sink tokens] + [recent window tokens]
//!
//! Reference: "Efficient Streaming Language Models with Attention Sinks" (MIT, 2024)

use serde::{Deserialize, Serialize};

use super::CompressionStats;

/// StreamingLLM configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamingLlmConfig {
    /// Enable StreamingLLM-style session management.
    pub enabled: bool,

    /// Number of initial tokens to keep as attention sinks.
    pub sink_size: usize,

    /// Size of the recent token window.
    pub window_size: usize,

    /// When total tokens exceed this, trigger eviction.
    pub eviction_threshold: usize,
}

impl Default for StreamingLlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sink_size: 4,
            window_size: 2048,
            eviction_threshold: 4096,
        }
    }
}

/// A message in the conversation window.
#[derive(Debug, Clone)]
pub struct WindowMessage {
    pub role: String,
    pub content: String,
    pub token_estimate: usize,
}

/// StreamingLLM session window manager.
pub struct StreamingWindow {
    config: StreamingLlmConfig,
    /// Sink messages (always kept — typically the system prompt).
    sink: Vec<WindowMessage>,
    /// Recent messages (sliding window).
    window: Vec<WindowMessage>,
    /// Tokens in sink (fixed after sink fills).
    sink_tokens: usize,
    /// Tokens in window (updated on push/evict — avoids O(N) recount).
    window_tokens: usize,
    /// Total messages evicted over the session lifetime.
    evicted_count: usize,
}

impl StreamingWindow {
    pub fn new(config: StreamingLlmConfig) -> Self {
        Self {
            config,
            sink: Vec::new(),
            window: Vec::new(),
            sink_tokens: 0,
            window_tokens: 0,
            evicted_count: 0,
        }
    }

    /// Add a message to the window.
    ///
    /// If this is one of the first `sink_size` messages, it becomes a permanent
    /// attention sink. Otherwise, it enters the sliding window and may trigger
    /// eviction of old messages.
    pub fn push(&mut self, role: &str, content: &str) {
        let token_estimate = crate::util::estimate_tokens(content);
        let msg = WindowMessage {
            role: role.to_string(),
            content: content.to_string(),
            token_estimate,
        };

        if self.sink.len() < self.config.sink_size {
            self.sink_tokens += token_estimate;
            self.sink.push(msg);
        } else {
            self.window_tokens += token_estimate;
            self.window.push(msg);
            self.maybe_evict();
        }
    }

    /// Get all messages in the current window (sink + recent).
    pub fn messages(&self) -> Vec<&WindowMessage> {
        let mut msgs: Vec<&WindowMessage> = self.sink.iter().collect();
        msgs.extend(self.window.iter());
        msgs
    }

    /// Get formatted messages for inference.
    pub fn formatted_messages(&self) -> Vec<(String, String)> {
        self.messages()
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect()
    }

    /// Get current window statistics.
    pub fn stats(&self) -> CompressionStats {
        let current = self.sink_tokens + self.window_tokens;
        let total_if_unbounded = current + self.evicted_count * 100;
        CompressionStats::new(total_if_unbounded, current, "streaming-llm", false)
    }

    /// Current total token estimate (sink + window).
    pub fn total_tokens(&self) -> usize {
        self.sink_tokens + self.window_tokens
    }

    /// Number of messages evicted so far.
    pub fn evicted_count(&self) -> usize {
        self.evicted_count
    }

    /// Number of messages currently in the window (including sinks).
    pub fn message_count(&self) -> usize {
        self.sink.len() + self.window.len()
    }

    /// Evict old messages from the window if over threshold.
    fn maybe_evict(&mut self) {
        while (self.sink_tokens + self.window_tokens) > self.config.eviction_threshold
            && self.window_tokens > self.config.window_size
            && !self.window.is_empty()
        {
            let evicted = self.window.remove(0);
            self.window_tokens -= evicted.token_estimate;
            self.evicted_count += 1;
        }
    }
}

/// Rough token estimation (same as router.rs).
#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> StreamingLlmConfig {
        StreamingLlmConfig {
            enabled: true,
            sink_size: 2,
            window_size: 100,
            eviction_threshold: 150,
        }
    }

    #[test]
    fn sink_messages_preserved() {
        let mut window = StreamingWindow::new(test_config());
        window.push("system", "You are a helpful assistant.");
        window.push("system", "Context: important info");
        window.push("user", "Hello!");
        window.push("assistant", "Hi there!");

        let msgs = window.messages();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "system");
    }

    #[test]
    fn eviction_removes_old_window_messages() {
        let config = StreamingLlmConfig {
            enabled: true,
            sink_size: 1,
            window_size: 50,
            eviction_threshold: 80,
        };
        let mut window = StreamingWindow::new(config);

        // Add sink
        window.push("system", "System prompt");

        // Add many messages to trigger eviction
        for i in 0..20 {
            window.push("user", &format!("Message number {i} with some extra text to increase token count"));
        }

        // Should have evicted some messages
        assert!(window.evicted_count() > 0);
        // Sink should still be there
        assert_eq!(window.messages()[0].role, "system");
    }

    #[test]
    fn empty_window_works() {
        let window = StreamingWindow::new(test_config());
        assert_eq!(window.message_count(), 0);
        assert_eq!(window.total_tokens(), 0);
        assert!(window.messages().is_empty());
    }
}
