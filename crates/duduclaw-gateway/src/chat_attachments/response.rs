//! The deadline owns cleanup even when the HTTP transport stops polling.
use std::{io, pin::Pin, sync::{Arc, Mutex}, task::{Context, Poll}, time::Duration};
use axum::body::{Body, Bytes};
use futures_util::Stream;
use super::limits::Permit;

pub(crate) const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(30);
const CHUNK: usize = 64 * 1024;

struct Payload { bytes: Vec<u8>, offset: usize, _permit: Arc<Permit> }
struct State { payload: Option<Payload>, expired: bool }
struct ResponseStream {
    state: Arc<Mutex<State>>,
    deadline: tokio::task::JoinHandle<()>,
}

pub(crate) fn body(bytes: Vec<u8>, permit: Arc<Permit>) -> Body {
    Body::from_stream(response_stream(bytes, permit, DOWNLOAD_DEADLINE))
}

fn response_stream(bytes: Vec<u8>, permit: Arc<Permit>, duration: Duration) -> ResponseStream {
    let state = Arc::new(Mutex::new(State {
        payload: Some(Payload { bytes, offset: 0, _permit: permit }), expired: false,
    }));
    let cleanup = state.clone();
    let expires = tokio::time::Instant::now() + duration;
    let deadline = tokio::spawn(async move {
        tokio::time::sleep_until(expires).await;
        let mut state = cleanup.lock().unwrap_or_else(|e| e.into_inner());
        state.expired = true;
        state.payload = None;
    });
    ResponseStream { state, deadline }
}

impl Stream for ResponseStream {
    type Item = Result<Bytes, io::Error>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.expired {
            state.expired = false;
            return Poll::Ready(Some(Err(io::Error::new(io::ErrorKind::TimedOut, "attachment download expired"))));
        }
        let Some(payload) = state.payload.as_mut() else { return Poll::Ready(None); };
        if payload.offset == payload.bytes.len() {
            state.payload = None;
            return Poll::Ready(None);
        }
        let end = (payload.offset + CHUNK).min(payload.bytes.len());
        // Copy a bounded chunk: a transport-held Bytes must not retain the
        // complete snapshot allocation after the independent deadline fires.
        let chunk = Bytes::copy_from_slice(&payload.bytes[payload.offset..end]);
        payload.offset = end;
        Poll::Ready(Some(Ok(chunk)))
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        self.deadline.abort();
        self.state.lock().unwrap_or_else(|e| e.into_inner()).payload = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use super::super::limits::{Limits, MAX_DOWNLOAD};

    #[tokio::test(start_paused = true)]
    async fn chat_attachment_unpolled_bodies_expire_and_release_all_slots() {
        let limits = Limits::new();
        let mut bodies = Vec::new();
        for user in ["alice", "alice", "bob", "bob"] {
            let permit = limits.acquire(std::path::Path::new("/deadline"), user, 1, 10).unwrap();
            let stream = response_stream(vec![42; MAX_DOWNLOAD as usize], Arc::new(permit), DOWNLOAD_DEADLINE);
            let state = stream.state.clone();
            bodies.push((Body::from_stream(stream), state));
        }
        assert!(limits.acquire(std::path::Path::new("/deadline"), "carol", 1, 10).is_err());
        tokio::time::advance(DOWNLOAD_DEADLINE + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        for (body, state) in bodies {
            assert!(state.lock().unwrap().payload.is_none());
            assert!(axum::body::to_bytes(body, MAX_DOWNLOAD as usize).await.is_err());
        }
        let permits: Vec<_> = ["alice", "alice", "bob", "bob"].into_iter().map(|user|
            limits.acquire(std::path::Path::new("/deadline"), user, 1, 10).unwrap()).collect();
        assert_eq!(permits.len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn chat_attachment_partial_body_expires_with_only_bounded_transport_chunk() {
        let limits = Limits::new();
        let permit = limits.acquire(std::path::Path::new("/partial"), "alice", 1, 10).unwrap();
        let mut stream = response_stream(vec![7; CHUNK * 3], Arc::new(permit), DOWNLOAD_DEADLINE);
        let held_chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(held_chunk.len(), CHUNK);
        tokio::time::advance(DOWNLOAD_DEADLINE + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(stream.state.lock().unwrap().payload.is_none());
        assert_eq!(stream.next().await.unwrap().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(held_chunk.iter().all(|b| *b == 7));
    }

    #[tokio::test]
    async fn chat_attachment_body_completion_and_drop_release_payload() {
        let limits = Limits::new();
        let permit = || limits.acquire(std::path::Path::new("/completion"), "alice", 1, 10).unwrap();
        let mut stream = response_stream(b"report".to_vec(), Arc::new(permit()), DOWNLOAD_DEADLINE);
        assert_eq!(&stream.next().await.unwrap().unwrap()[..], b"report");
        assert!(stream.next().await.is_none());
        assert!(stream.state.lock().unwrap().payload.is_none());
        let stream = response_stream(vec![1; CHUNK], Arc::new(permit()), DOWNLOAD_DEADLINE);
        let state = stream.state.clone();
        drop(stream);
        assert!(state.lock().unwrap().payload.is_none());
    }
}
