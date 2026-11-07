//! The final delivery fence belongs below SplitSink's one-message buffer.
use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(in crate::webchat) type SocketGate = Arc<Mutex<Option<WritePermit>>>;

pub(in crate::webchat) struct WritePermit {
    pub users: Arc<duduclaw_auth::UserDb>,
    pub generation: u64,
    pub policy: crate::webchat::access::TargetPolicy,
    pub route: subscriptions::Route,
    pub deadline: tokio::time::Instant,
    pub home: PathBuf,
    pub receiving_credential: Option<duduclaw_auth::jwt::Claims>,
}

pub(in crate::webchat) struct GuardedSocket<S> {
    inner: S,
    gate: SocketGate,
}

impl<S> GuardedSocket<S> {
    pub(in crate::webchat) fn new(inner: S, gate: SocketGate) -> Self { Self { inner, gate } }
}

impl<S: futures_util::Stream + Unpin> futures_util::Stream for GuardedSocket<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S: futures_util::Sink<Message> + Unpin> futures_util::Sink<Message> for GuardedSocket<S> {
    type Error = ();
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), ()>> {
        Pin::new(&mut self.inner).poll_ready(cx).map(|result| result.map_err(|_| ()))
    }

    fn start_send(self: Pin<&mut Self>, message: Message) -> Result<(), ()> {
        let this = self.get_mut();
        let gate = this.gate.lock().unwrap();
        let Some(permit) = gate.as_ref() else {
            return Pin::new(&mut this.inner).start_send(message).map_err(|_| ());
        };
        let result = permit.users.with_acl_generation(permit.generation, || {
            permit.policy.with_current(|| {
                permit.route.with_live(|| {
                    let now = chrono::Utc::now().timestamp().max(0) as u64;
                    let valid = |claims: &duduclaw_auth::jwt::Claims| webchat_credential_expiry(&permit.home, claims)
                        .is_ok_and(|expiry| expiry.is_none_or(|expiry| now < expiry));
                    if permit.route.origin_credential.as_ref().is_some_and(|claims| !valid(claims)) {
                        permit.route.source.refuse();
                    }
                    if tokio::time::Instant::now() >= permit.deadline
                        || permit.receiving_credential.as_ref().is_some_and(|claims| !valid(claims))
                        || !permit.route.source.latched_valid() { return Err(()); }
                    Pin::new(&mut this.inner).start_send(message).map_err(|_| ())
                }).unwrap_or(Err(()))
            }).unwrap_or_else(|_| { permit.route.source.refuse(); Err(()) })
        });
        match result {
            Ok(result) => result,
            Err(_) => { permit.route.source.refuse(); Err(()) }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), ()>> {
        Pin::new(&mut self.inner).poll_flush(cx).map(|result| result.map_err(|_| ()))
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), ()>> {
        Pin::new(&mut self.inner).poll_close(cx).map(|result| result.map_err(|_| ()))
    }
}
