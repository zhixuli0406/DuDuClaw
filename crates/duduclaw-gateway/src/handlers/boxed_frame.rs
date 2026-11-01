//! Keep child-future construction out of RPC dispatch poll frames.

use std::future::Future;
use std::pin::Pin;

// Box::pin(build()) in the caller still reserves a full-sized temporary on
// its stack in debug builds. Construct it in this non-inlined frame instead.
// Generic output also covers the knowledge routes' Option/Result responses.
#[inline(never)]
pub(super) fn boxed_frame<'a, F, Fut>(build: F) -> Pin<Box<dyn Future<Output = Fut::Output> + Send + 'a>>
where
    F: FnOnce() -> Fut,
    Fut: Future + Send + 'a,
{
    Box::pin(build())
}
