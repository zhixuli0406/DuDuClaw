//! One blocking writer owns child input. Limits count bytes, including in-flight writes.
use std::{
    io::{self, Write},
    sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc},
    thread,
};

pub const MAX_PENDING: usize = 1024 * 1024;
// Stop reading terminal events well before the hard cap, leaving room for a paste.
pub const BACKPRESSURE_AT: usize = 64 * 1024;

pub(super) struct InputWriter {
    tx: mpsc::Sender<Vec<u8>>,
    pending: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
}
impl InputWriter {
    pub fn spawn(mut writer: impl Write + Send + 'static) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let pending = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let (count, failure) = (pending.clone(), failed.clone());
        thread::Builder::new().name("tui-pty-write".into()).spawn(move || {
            while let Ok(bytes) = rx.recv() {
                // write_all handles short writes and Interrupted without dropping a suffix.
                if writer.write_all(&bytes).is_err() {
                    failure.store(true, Ordering::Release);
                    break;
                }
                count.fetch_sub(bytes.len(), Ordering::AcqRel);
            }
        })?;
        Ok(Self { tx, pending, failed })
    }
    pub fn pending(&self) -> usize { self.pending.load(Ordering::Acquire) }
    pub fn failed(&self) -> bool { self.failed.load(Ordering::Acquire) }
    pub fn send(&self, bytes: Vec<u8>) -> Result<(), ()> {
        if self.failed() { return Err(()); }
        // Reserve the entire chunk atomically, never a prefix. No count-based limit.
        let mut current = self.pending();
        loop {
            let next = current.checked_add(bytes.len()).filter(|n| *n <= MAX_PENDING).ok_or(())?;
            match self.pending.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        let len = bytes.len();
        if self.tx.send(bytes).is_err() {
            self.pending.fetch_sub(len, Ordering::AcqRel);
            return Err(());
        }
        Ok(())
    }
}
