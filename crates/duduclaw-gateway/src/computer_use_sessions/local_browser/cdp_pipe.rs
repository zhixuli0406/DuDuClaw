//! NUL-framed CDP over inherited descriptors 3/4. There is no TCP fallback.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, mpsc, oneshot},
};

const MAX_FRAME: usize = 16 * 1024 * 1024;
type Reply = oneshot::Sender<Result<Value, String>>;

pub(crate) struct CdpPipe {
    next: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Reply>>>,
    writer: Mutex<tokio::net::unix::OwnedWriteHalf>,
    reader: tokio::task::JoinHandle<()>,
}

impl CdpPipe {
    pub(crate) fn new(socket: tokio::net::UnixStream) -> (Arc<Self>, mpsc::Receiver<Value>) {
        let (mut read, write) = socket.into_split();
        let pending: Arc<Mutex<HashMap<u64, Reply>>> = Arc::new(Mutex::new(HashMap::new()));
        let replies = pending.clone();
        let (events, receiver) = mpsc::channel(128);
        let reader = tokio::spawn(async move {
            let mut frame = Vec::new();
            let mut chunk = [0u8; 8192];
            'read: loop {
                let Ok(n) = read.read(&mut chunk).await else {
                    break;
                };
                if n == 0 {
                    break;
                }
                for byte in &chunk[..n] {
                    if *byte != 0 {
                        frame.push(*byte);
                        if frame.len() > MAX_FRAME {
                            break 'read;
                        }
                        continue;
                    }
                    let Ok(value) = serde_json::from_slice::<Value>(&frame) else {
                        break 'read;
                    };
                    frame.clear();
                    if let Some(id) = value.get("id").and_then(Value::as_u64) {
                        if let Some(reply) = replies.lock().await.remove(&id) {
                            let result = if value.get("error").is_some() {
                                Err("browser protocol command refused".into())
                            } else {
                                Ok(value.get("result").cloned().unwrap_or_else(|| json!({})))
                            };
                            let _ = reply.send(result);
                        }
                    } else if events.try_send(value).is_err() {
                        // Losing a security event is fatal; no permissive backlog drop.
                        break 'read;
                    }
                }
            }
            for (_, reply) in replies.lock().await.drain() {
                let _ = reply.send(Err("browser pipe closed".into()));
            }
        });
        (
            Arc::new(Self {
                next: AtomicU64::new(1),
                pending,
                writer: Mutex::new(write),
                reader,
            }),
            receiver,
        )
    }

    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, String> {
        self.call_checked(method, params, session, || Ok(())).await
    }

    pub(crate) async fn call_checked<F: FnOnce() -> Result<(), String> + Send>(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        gate: F,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut encoded = serde_json::to_vec(&message).map_err(|_| "browser command invalid")?;
        if encoded.len() > 128 * 1024 {
            return Err("browser command too large".into());
        }
        encoded.push(0);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.len() >= 64 {
                return Err("browser busy".into());
            }
            pending.insert(id, sender);
        }
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let mut writer = self.writer.lock().await;
            gate()?;
            writer
                .write_all(&encoded)
                .await
                .map_err(|_| "browser pipe unavailable".to_owned())?;
            drop(writer);
            receiver
                .await
                .map_err(|_| "browser pipe closed".to_owned())?
        })
        .await
        .unwrap_or_else(|_| Err("browser command timed out".into()));
        self.pending.lock().await.remove(&id);
        result
    }
}
impl Drop for CdpPipe {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn a_revoked_queued_writer_sends_no_browser_command() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (pipe, _events) = CdpPipe::new(client);
        let held = pipe.writer.lock().await;
        let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_pipe = pipe.clone();
        let flag = revoked.clone();
        let queued = tokio::spawn(async move {
            worker_pipe
                .call_checked(
                    "Input.insertText",
                    json!({"text":"synthetic"}),
                    Some("page"),
                    move || {
                        if flag.load(Ordering::Acquire) {
                            Err("local_call_revoked".into())
                        } else {
                            Ok(())
                        }
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while pipe.pending.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        revoked.store(true, Ordering::Release);
        drop(held);
        assert_eq!(queued.await.unwrap().unwrap_err(), "local_call_revoked");
        assert!(pipe.pending.lock().await.is_empty());
        assert_eq!(
            server.try_read(&mut [0; 32]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    #[tokio::test]
    async fn pipe_handles_fragmented_reply_and_session_event_without_listening() {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let (pipe, mut events) = CdpPipe::new(client);
        let fake = tokio::spawn(async move {
            let mut request = Vec::new();
            loop {
                let byte = server.read_u8().await.unwrap();
                if byte == 0 {
                    break;
                }
                request.push(byte);
            }
            let req: Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(req["sessionId"], "page-a");
            let reply = format!("{{\"id\":{},\"result\":{{\"ok\":true}}}}\0", req["id"]);
            for part in reply.as_bytes().chunks(3) {
                server.write_all(part).await.unwrap();
            }
            server
                .write_all(b"{\"method\":\"Target.targetDestroyed\",\"params\":{}}\0")
                .await
                .unwrap();
        });
        assert_eq!(
            pipe.call("Synthetic.probe", json!({}), Some("page-a"))
                .await
                .unwrap()["ok"],
            true
        );
        assert_eq!(
            events.recv().await.unwrap()["method"],
            "Target.targetDestroyed"
        );
        fake.await.unwrap();
    }
}
