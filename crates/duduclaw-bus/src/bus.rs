use std::sync::Arc;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::types::Message;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info};

/// Message bus combining broadcast (pub/sub) and mpsc (point-to-point) channels.
pub struct MessageBus {
    // broadcast for pub/sub (events to multiple listeners)
    broadcast_tx: broadcast::Sender<Message>,
    // mpsc for command queue (point-to-point)
    command_tx: mpsc::Sender<Message>,
    command_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Message>>>,
}

impl MessageBus {
    /// Create a new message bus with the given channel capacities.
    pub fn new(broadcast_capacity: usize, command_capacity: usize) -> Self {
        let (broadcast_tx, _) = broadcast::channel(broadcast_capacity);
        let (command_tx, command_rx) = mpsc::channel(command_capacity);

        info!(
            broadcast_capacity,
            command_capacity, "MessageBus created"
        );

        Self {
            broadcast_tx,
            command_tx,
            command_rx: Arc::new(tokio::sync::Mutex::new(command_rx)),
        }
    }

    /// Publish a message to all subscribers.
    ///
    /// If there are no active subscribers the message is silently dropped and
    /// `Ok(())` is returned.
    pub fn publish(&self, message: Message) -> Result<()> {
        debug!(
            message_id = %message.id,
            channel = %message.channel,
            "Publishing message to broadcast"
        );

        match self.broadcast_tx.send(message) {
            Ok(_) => Ok(()),
            Err(_) if self.broadcast_tx.receiver_count() == 0 => {
                debug!("No broadcast subscribers — message dropped");
                Ok(())
            }
            Err(e) => Err(DuDuClawError::Channel(format!(
                "broadcast send failed: {e}"
            ))),
        }
    }

    /// Send a command message (point-to-point).
    pub async fn send_command(&self, message: Message) -> Result<()> {
        debug!(
            message_id = %message.id,
            channel = %message.channel,
            "Sending command message"
        );

        self.command_tx.send(message).await.map_err(|e| {
            DuDuClawError::Channel(format!("command send failed: {e}"))
        })?;

        Ok(())
    }

    /// Subscribe to broadcast messages.
    pub fn subscribe(&self) -> broadcast::Receiver<Message> {
        debug!("New broadcast subscriber added");
        self.broadcast_tx.subscribe()
    }

    /// Receive next command message.
    pub async fn recv_command(&self) -> Option<Message> {
        let mut rx = self.command_rx.lock().await;
        rx.recv().await
    }

    /// Get number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.broadcast_tx.receiver_count()
    }
}
