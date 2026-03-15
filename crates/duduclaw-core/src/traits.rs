use async_trait::async_trait;

use crate::error::Result;
use crate::types::*;

/// Abstraction over a messaging channel (Telegram, LINE, Discord, etc.).
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable name of this channel.
    fn name(&self) -> &str;

    /// Establish the connection (e.g. start polling or open a websocket).
    async fn connect(&self) -> Result<()>;

    /// Send a text message to the given chat.
    async fn send_message(&self, chat_id: &str, text: &str) -> Result<()>;

    /// Gracefully disconnect from the channel.
    async fn disconnect(&self) -> Result<()>;

    /// Whether the channel is currently connected.
    fn is_connected(&self) -> bool;

    /// Whether this channel instance owns the given chat id.
    fn owns_chat_id(&self, chat_id: &str) -> bool;
}

/// Abstraction over a container runtime (Docker, Podman, etc.).
#[async_trait]
pub trait ContainerRuntime: Send + Sync {
    /// Create a new container from the given configuration.
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId>;

    /// Start a previously created container.
    async fn start(&self, id: &ContainerId) -> Result<()>;

    /// Stop a running container, waiting up to `timeout` for graceful shutdown.
    async fn stop(&self, id: &ContainerId, timeout: std::time::Duration) -> Result<()>;

    /// Remove a container and its resources.
    async fn remove(&self, id: &ContainerId) -> Result<()>;

    /// Retrieve the stdout/stderr logs of a container.
    async fn logs(&self, id: &ContainerId) -> Result<String>;

    /// Perform a health check on the runtime itself.
    async fn health_check(&self) -> Result<RuntimeHealth>;
}

/// Abstraction over a memory / knowledge engine.
#[async_trait]
pub trait MemoryEngine: Send + Sync {
    /// Store a new memory entry for the given agent.
    async fn store(&self, agent_id: &str, entry: MemoryEntry) -> Result<()>;

    /// Search memories for the given agent, returning at most `limit` results.
    async fn search(&self, agent_id: &str, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;

    /// Produce a summary of the agent's memories within the given time window.
    async fn summarize(&self, agent_id: &str, window: TimeWindow) -> Result<String>;
}
