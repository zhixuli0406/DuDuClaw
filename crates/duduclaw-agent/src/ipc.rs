use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use duduclaw_core::error::{DuDuClawError, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Type of an IPC message exchanged between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcMessageType {
    Delegate,
    DelegateResponse,
    Notification,
    StatusQuery,
    StatusResponse,
}

/// Delivery status of an IPC message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcMessageStatus {
    Pending,
    Delivered,
    Processing,
    Completed,
    Failed,
}

/// A single IPC message flowing between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub id: String,
    pub message_type: IpcMessageType,
    pub source_agent: String,
    pub target_agent: String,
    pub payload: serde_json::Value,
    pub timestamp: DateTime<Utc>,
    pub status: IpcMessageStatus,
}

/// Broker that routes IPC messages between agents using JSON files as the
/// persistence layer and in-memory queues for fast delivery.
pub struct IpcBroker {
    ipc_dir: PathBuf,
    queues: Arc<RwLock<HashMap<String, VecDeque<IpcMessage>>>>,
}

impl IpcBroker {
    /// Create a new broker backed by the given directory.
    pub fn new(ipc_dir: PathBuf) -> Self {
        Self {
            ipc_dir,
            queues: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Validate that a string is a valid UUID v4 format.
    fn is_valid_uuid(s: &str) -> bool {
        uuid::Uuid::parse_str(s).is_ok()
    }

    /// Send an IPC message from one agent to another.
    ///
    /// The message is persisted as a JSON file under
    /// `<ipc_dir>/<target_agent>/<timestamp>_<id>.json` and enqueued in memory
    /// for the target agent.
    pub async fn send(&self, message: IpcMessage) -> Result<()> {
        // Validate message ID is a valid UUID to prevent path traversal
        if !Self::is_valid_uuid(&message.id) {
            return Err(DuDuClawError::Agent(format!(
                "invalid IPC message id (must be UUID): {}",
                message.id
            )));
        }

        info!(
            "IPC: {} -> {} ({})",
            message.source_agent, message.target_agent, message.id
        );

        // Persist to filesystem
        let target_dir = self.ipc_dir.join(&message.target_agent);
        tokio::fs::create_dir_all(&target_dir).await.map_err(|e| {
            DuDuClawError::Agent(format!(
                "failed to create IPC directory {}: {e}",
                target_dir.display()
            ))
        })?;

        let filename = format!(
            "{}_{}.json",
            message.timestamp.timestamp_millis(),
            message.id
        );
        let file_path = target_dir.join(&filename);

        let json = serde_json::to_string_pretty(&message)?;
        tokio::fs::write(&file_path, json).await.map_err(|e| {
            DuDuClawError::Agent(format!(
                "failed to write IPC message {}: {e}",
                file_path.display()
            ))
        })?;

        debug!(path = %file_path.display(), "IPC message persisted");

        // Enqueue in memory
        let mut queues = self.queues.write().await;
        queues
            .entry(message.target_agent.clone())
            .or_default()
            .push_back(message);

        Ok(())
    }

    /// Receive and drain all pending messages for an agent.
    pub async fn receive(&self, agent_id: &str) -> Vec<IpcMessage> {
        let mut queues = self.queues.write().await;
        match queues.get_mut(agent_id) {
            Some(queue) => queue.drain(..).collect(),
            None => Vec::new(),
        }
    }

    /// Check whether there are pending messages for an agent.
    pub async fn has_pending(&self, agent_id: &str) -> bool {
        let queues = self.queues.read().await;
        queues
            .get(agent_id)
            .map(|q| !q.is_empty())
            .unwrap_or(false)
    }

    /// Return the number of pending messages per agent.
    pub async fn pending_counts(&self) -> HashMap<String, usize> {
        let queues = self.queues.read().await;
        queues.iter().map(|(k, v)| (k.clone(), v.len())).collect()
    }

    /// Mark a specific message as completed and remove its persisted file.
    pub async fn complete(&self, agent_id: &str, message_id: &str) -> Result<()> {
        // Remove from in-memory queue if still present
        let mut queues = self.queues.write().await;
        if let Some(queue) = queues.get_mut(agent_id) {
            queue.retain(|m| m.id != message_id);
        }

        // Attempt to remove the corresponding JSON file(s)
        let target_dir = self.ipc_dir.join(agent_id);
        if target_dir.is_dir() {
            let mut entries = match tokio::fs::read_dir(&target_dir).await {
                Ok(e) => e,
                Err(_) => return Ok(()),
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry
                    .file_name()
                    .to_str()
                    .unwrap_or_default()
                    .to_string();
                // Use exact suffix matching: "_<message_id>.json"
                let expected_suffix = format!("_{}.json", message_id);
                if name.ends_with(&expected_suffix) {
                    if let Err(e) = tokio::fs::remove_file(entry.path()).await {
                        warn!(
                            path = %entry.path().display(),
                            error = %e,
                            "failed to remove completed IPC message file"
                        );
                    } else {
                        debug!(
                            path = %entry.path().display(),
                            "removed completed IPC message file"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Send an IPC message with source verification.
    ///
    /// Like [`send`](Self::send), but additionally enforces that
    /// `message.source_agent == caller_id`.
    pub async fn send_verified(&self, message: IpcMessage, caller_id: &str) -> Result<()> {
        if message.source_agent != caller_id {
            return Err(DuDuClawError::Agent(format!(
                "IPC source mismatch: message claims source '{}' but caller is '{}'",
                message.source_agent, caller_id
            )));
        }
        self.send(message).await
    }

    /// Create a delegation message from one agent to another.
    pub fn create_delegate(source: &str, target: &str, prompt: &str) -> IpcMessage {
        IpcMessage {
            id: uuid::Uuid::new_v4().to_string(),
            message_type: IpcMessageType::Delegate,
            source_agent: source.to_string(),
            target_agent: target.to_string(),
            payload: serde_json::json!({ "prompt": prompt }),
            timestamp: Utc::now(),
            status: IpcMessageStatus::Pending,
        }
    }

    /// Create a response to a previously received delegation message.
    pub fn create_response(original: &IpcMessage, result: serde_json::Value) -> IpcMessage {
        IpcMessage {
            id: uuid::Uuid::new_v4().to_string(),
            message_type: IpcMessageType::DelegateResponse,
            source_agent: original.target_agent.clone(),
            target_agent: original.source_agent.clone(),
            payload: serde_json::json!({
                "original_message_id": original.id,
                "result": result,
            }),
            timestamp: Utc::now(),
            status: IpcMessageStatus::Pending,
        }
    }
}
