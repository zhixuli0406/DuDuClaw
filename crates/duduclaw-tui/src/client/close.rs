//! A bounded closing handshake shared by RPC and WebChat.
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
pub(crate) async fn socket<S>(socket: &mut WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        // close() sends and flushes Close; continue reading until the peer replies.
        if socket.close(None).await.is_err() {
            let _ = socket.flush().await;
            return;
        }
        while let Some(message) = socket.next().await {
            match message {
                Ok(Message::Close(_)) => {
                    let _ = socket.flush().await;
                    break;
                }
                Err(_) => break,
                _ => {}
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;
    #[tokio::test]
    async fn sends_close_and_waits_for_peer_without_tcp_reset() {
        let (a, b) = tokio::io::duplex(4096);
        let (mut client, mut server) = tokio::join!(
            WebSocketStream::from_raw_socket(a, Role::Client, None),
            WebSocketStream::from_raw_socket(b, Role::Server, None)
        );
        let peer = async {
            assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
            server.flush().await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(socket(&mut client), peer);
        })
        .await
        .unwrap();
    }
}
