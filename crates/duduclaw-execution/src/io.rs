//! Executor adapters for the portable execution contract.
use duduclaw_isolation::execution::{BackendError, BackendFuture, RunReader, RunWriter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Preserve an existing asynchronous byte stream without decoding its contents.
pub struct Reader<T>(pub T);
impl<T: AsyncRead + Unpin + Send> RunReader for Reader<T> {
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move { self.0.read(buf).await.map_err(|_| BackendError::Io) })
    }
}

/// Preserve backpressure and explicitly close the input half on shutdown.
pub struct Writer<T>(pub T);
impl<T: AsyncWrite + Unpin + Send> RunWriter for Writer<T> {
    fn write<'a>(&'a mut self, buf: &'a [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move { self.0.write(buf).await.map_err(|_| BackendError::Io) })
    }
    fn close(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async move { self.0.shutdown().await.map_err(|_| BackendError::Io) })
    }
}

/// Bound input before it reaches either an executable or privileged IPC.
pub async fn read_input(mut input: duduclaw_isolation::execution::RunInput, hard_limit: u64)
    -> Result<Vec<u8>, BackendError>
{
    if input.max_bytes > hard_limit { return Err(BackendError::Denied); }
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = input.reader.read(&mut buffer).await?;
        if count > buffer.len() { return Err(BackendError::Protocol); }
        if count == 0 { return Ok(result); }
        if count as u64 > input.max_bytes.saturating_sub(result.len() as u64) {
            return Err(BackendError::Denied);
        }
        result.extend_from_slice(&buffer[..count]);
    }
}

use std::{future::Future, pin::Pin, task::{Context, Poll}};

type ReadStep = Pin<Box<dyn Future<Output = (Box<dyn RunReader>, Result<Vec<u8>, BackendError>)> + Send>>;
/// Tokio reader over the portable reader; failures remain observable as errors.
pub struct AsyncReader {
    source: Option<Box<dyn RunReader>>,
    pending: Option<ReadStep>,
    bytes: Vec<u8>,
    offset: usize,
    native_error: Option<NativeIoError>,
}
impl AsyncReader {
    pub fn with_native_error(mut self, slot: NativeIoError) -> Self { self.native_error = Some(slot); self }
    pub fn new(source: Box<dyn RunReader>) -> Self { Self { source: Some(source), pending: None, bytes: vec![], offset: 0, native_error: None } }
}
impl AsyncRead for AsyncReader {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if buf.remaining() == 0 { return Poll::Ready(Ok(())); }
        if self.offset < self.bytes.len() {
            let count = buf.remaining().min(self.bytes.len() - self.offset);
            buf.put_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            return Poll::Ready(Ok(()));
        }
        if self.pending.is_none() {
            let Some(mut source) = self.source.take() else { return Poll::Ready(Ok(())); };
            let count = buf.remaining().min(8192);
            self.pending = Some(Box::pin(async move {
                let mut bytes = vec![0; count];
                let result = match source.read(&mut bytes).await {
                    Ok(count) if count <= bytes.len() => { bytes.truncate(count); Ok(bytes) },
                    Ok(_) => Err(BackendError::Protocol),
                    Err(reason) => Err(reason),
                };
                (source, result)
            }));
        }
        let step = self.pending.as_mut().expect("pending read");
        let (source, result) = match step.as_mut().poll(cx) { Poll::Pending => return Poll::Pending, Poll::Ready(result) => result };
        self.pending = None;
        match result {
            Ok(bytes) => {
                if !bytes.is_empty() { self.source = Some(source); }
                let count = buf.remaining().min(bytes.len());
                buf.put_slice(&bytes[..count]);
                self.bytes = bytes;
                self.offset = count;
                Poll::Ready(Ok(()))
            },
            Err(reason) => { self.source = Some(source); Poll::Ready(Err(native_error(&self.native_error, reason))) },
        }
    }
}

type WriteStep = Pin<Box<dyn Future<Output = (Box<dyn RunWriter>, Result<usize, BackendError>)> + Send>>;
/// Tokio writer with acknowledged writes, preserving backpressure and errors.
pub struct AsyncWriter {
    sink: Option<Box<dyn RunWriter>>,
    pending: Option<WriteStep>,
    closing: bool,
    pending_bytes: Option<Vec<u8>>,
    native_error: Option<NativeIoError>,
}
impl AsyncWriter {
    pub fn with_native_error(mut self, slot: NativeIoError) -> Self { self.native_error = Some(slot); self }
    pub fn new(sink: Box<dyn RunWriter>) -> Self { Self { sink: Some(sink), pending: None, closing: false, pending_bytes: None, native_error: None } }
    fn poll_pending(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<usize>> {
        let step = self.pending.as_mut().expect("pending write");
        let (sink, result) = match step.as_mut().poll(cx) { Poll::Pending => return Poll::Pending, Poll::Ready(result) => result };
        self.pending = None;
        self.pending_bytes = None;
        if !self.closing { self.sink = Some(sink); }
        Poll::Ready(result.map_err(|reason| native_error(&self.native_error, reason)))
    }
}
impl AsyncWrite for AsyncWriter {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        if self.closing { return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())); }
        // Portable writers may already have sent data while awaiting an ack.
        // A cancelled write must never acknowledge a different caller buffer.
        if self.pending_bytes.as_deref().is_some_and(|pending| pending != &buf[..buf.len().min(8192)]) {
            return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "write cancelled with unacknowledged bytes")));
        }
        if self.pending.is_none() {
            let Some(mut sink) = self.sink.take() else { return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())); };
            let bytes = buf[..buf.len().min(8192)].to_vec();
            self.pending_bytes = Some(bytes.clone());
            self.pending = Some(Box::pin(async move {
                let result = match sink.write(&bytes).await {
                    Ok(count) if count <= bytes.len() => Ok(count),
                    Ok(_) => Err(BackendError::Protocol),
                    Err(reason) => Err(reason),
                };
                (sink, result)
            }));
        }
        self.poll_pending(cx)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.pending.is_some() { return self.poll_pending(cx).map_ok(|_| ()); }
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if !self.closing {
            if self.pending.is_some() {
                match self.poll_pending(cx) { Poll::Pending => return Poll::Pending, Poll::Ready(Err(error)) => return Poll::Ready(Err(error)), Poll::Ready(Ok(_)) => {} }
            }
            self.closing = true;
            let Some(mut sink) = self.sink.take() else { return Poll::Ready(Ok(())); };
            self.pending = Some(Box::pin(async move { let result = sink.close().await.map(|_| 0); (sink, result) }));
        }
        if self.pending.is_some() { self.poll_pending(cx).map_ok(|_| ()) } else { Poll::Ready(Ok(())) }
    }
}

#[cfg(test)]
mod async_adapter_tests {
    use super::*;
    struct FailingReader;
    impl RunReader for FailingReader {
        fn read<'a>(&'a mut self, _: &'a mut [u8]) -> BackendFuture<'a, usize> {
            Box::pin(async { Err(BackendError::Protocol) })
        }
    }
    struct FailingWriter;
    impl RunWriter for FailingWriter {
        fn write<'a>(&'a mut self, _: &'a [u8]) -> BackendFuture<'a, usize> {
            Box::pin(async { Err(BackendError::Io) })
        }
        fn close(&mut self) -> BackendFuture<'_, ()> { Box::pin(async { Err(BackendError::Protocol) }) }
    }
    struct UnacknowledgedWriter;
    impl RunWriter for UnacknowledgedWriter {
        fn write<'a>(&'a mut self, _: &'a [u8]) -> BackendFuture<'a, usize> { Box::pin(std::future::pending()) }
        fn close(&mut self) -> BackendFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    }
    #[tokio::test]
    async fn cancelled_write_cannot_acknowledge_different_bytes() {
        let mut writer = AsyncWriter::new(Box::new(UnacknowledgedWriter));
        assert!(tokio::time::timeout(std::time::Duration::from_millis(5), writer.write(b"first")).await.is_err());
        assert_eq!(writer.write(b"second").await.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }
    #[tokio::test]
    async fn portable_read_error_is_not_silent_eof() {
        let mut reader = AsyncReader::new(Box::new(FailingReader));
        assert!(reader.read(&mut [0; 16]).await.is_err());
    }
    #[tokio::test]
    async fn portable_write_and_close_errors_are_observed_by_caller() {
        let mut writer = AsyncWriter::new(Box::new(FailingWriter));
        assert!(writer.write_all(b"must acknowledge write").await.is_err());
        assert!(writer.shutdown().await.is_err());
    }
}

/// Native I/O errors stay local; the portable contract intentionally exposes
/// only bounded error categories. Preserve ErrorKind for legacy retry logic.
pub type NativeIoError = std::sync::Arc<std::sync::Mutex<Option<std::io::Error>>>;
#[derive(Clone, Default)]
pub struct NativeIoErrors {
    pub stdin: NativeIoError,
    pub stdout: NativeIoError,
    pub stderr: NativeIoError,
}
pub struct NativeWriter<T> { pub inner: T, pub error: NativeIoError }
impl<T: AsyncWrite + Unpin + Send> RunWriter for NativeWriter<T> {
    fn write<'a>(&'a mut self, buf: &'a [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            self.inner.write(buf).await.map_err(|error| {
                if let Ok(mut stored) = self.error.lock() { *stored = Some(error); }
                BackendError::Io
            })
        })
    }
    fn close(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            self.inner.shutdown().await.map_err(|error| {
                if let Ok(mut stored) = self.error.lock() { *stored = Some(error); }
                BackendError::Io
            })
        })
    }
}
fn native_error(slot: &Option<NativeIoError>, fallback: BackendError) -> std::io::Error {
    slot.as_ref().and_then(|slot| slot.lock().ok()?.take()).unwrap_or_else(|| std::io::Error::other(fallback))
}
