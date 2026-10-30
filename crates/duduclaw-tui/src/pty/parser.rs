//! The parser thread owns all VT parsing and scrollback. UI locks never cover parsing.
use super::{SCROLLBACK, filter::Filter};
use std::{
    io,
    sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering}, mpsc},
    thread,
    time::{Duration, Instant},
};

/// Only visible cells are published; no scrollback cloning on the UI thread.
pub struct Screen {
    size: (u16, u16),
    cells: Vec<vt100::Cell>,
    scrollback: usize,
    cursor: (u16, u16),
    hidden: bool,
    application_cursor: bool,
    bracketed_paste: bool,
}
impl Screen {
    pub(crate) fn capture(screen: &vt100::Screen) -> Self {
        let (rows, cols) = screen.size();
        Self {
            size: (rows, cols),
            cells: (0..rows).flat_map(|row| (0..cols).map(move |col| screen.cell(row, col).unwrap().clone())).collect(),
            scrollback: screen.scrollback(), cursor: screen.cursor_position(), hidden: screen.hide_cursor(),
            application_cursor: screen.application_cursor(), bracketed_paste: screen.bracketed_paste(),
        }
    }
    pub fn size(&self) -> (u16, u16) { self.size }
    pub fn cell(&self, row: u16, col: u16) -> Option<&vt100::Cell> {
        if row >= self.size.0 || col >= self.size.1 { return None; }
        self.cells.get(usize::from(row) * usize::from(self.size.1) + usize::from(col))
    }
    pub fn scrollback(&self) -> usize { self.scrollback }
    pub fn cursor_position(&self) -> (u16, u16) { self.cursor }
    pub fn hide_cursor(&self) -> bool { self.hidden }
    pub fn application_cursor(&self) -> bool { self.application_cursor }
    pub fn bracketed_paste(&self) -> bool { self.bracketed_paste }
    #[cfg(test)]
    pub fn contents(&self) -> String {
        (0..self.size.0).map(|row| {
            (0..self.size.1).map(|col| self.cell(row, col).unwrap().contents()).collect::<String>().trim_end().to_owned()
        }).collect::<Vec<_>>().join("\n").trim_end().to_owned()
    }
}

pub(super) struct ParserWorker {
    latest: Arc<Mutex<Option<Arc<Screen>>>>,
    size: Arc<AtomicU32>,
    scroll: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
}
impl ParserWorker {
    pub fn spawn(output: mpsc::Receiver<Vec<u8>>, rows: u16, cols: u16) -> io::Result<Self> {
        let latest = Arc::new(Mutex::new(None));
        let size = Arc::new(AtomicU32::new(pack(rows, cols)));
        let scroll = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let (published, dimensions, offset, stop) = (latest.clone(), size.clone(), scroll.clone(), stopped.clone());
        thread::Builder::new().name("tui-pty-parse".into()).spawn(move || {
            let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK);
            let mut filter = Filter::default();
            let mut last_publish = Instant::now();
            let mut dirty = true;
            let mut disconnected = false;
            let mut requested_scroll = 0;
            while !stop.load(Ordering::Acquire) {
                let packed = dimensions.load(Ordering::Acquire);
                let size = ((packed >> 16) as u16, packed as u16);
                if parser.screen().size() != size {
                    parser.screen_mut().set_size(size.0, size.1); dirty = true;
                }
                let wanted = offset.load(Ordering::Acquire);
                if wanted != requested_scroll {
                    parser.screen_mut().set_scrollback(wanted);
                    requested_scroll = wanted; dirty = true;
                }
                // A bounded read chunk is the maximum work between control checks.
                match output.recv_timeout(Duration::from_millis(10)) {
                    Ok(bytes) => {
                        filter.process(&bytes, size.0, size.1, |safe| parser.process(safe));
                        dirty = true;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
                    Err(mpsc::RecvTimeoutError::Timeout) => {},
                }
                if dirty && (disconnected || last_publish.elapsed() >= Duration::from_millis(33)) {
                    let screen = Arc::new(Screen::capture(parser.screen()));
                    // Swap pointers only. Build and destroy snapshots outside this lock.
                    let previous = published.lock().unwrap_or_else(|e| e.into_inner()).replace(screen);
                    drop(previous);
                    last_publish = Instant::now(); dirty = false;
                }
                // Retain the final screen and allow resize/scroll after child exit.
                if disconnected { thread::sleep(Duration::from_millis(10)); }
            }
        })?;
        Ok(Self { latest, size, scroll, stopped })
    }
    pub fn screen(&self) -> Option<Arc<Screen>> {
        self.latest.try_lock().ok().and_then(|mut latest| latest.take())
    }
    pub fn resize(&self, rows: u16, cols: u16) { self.size.store(pack(rows, cols), Ordering::Release); }
    pub fn scroll(&self, offset: usize) { self.scroll.store(offset.min(SCROLLBACK), Ordering::Release); }
}
fn pack(rows: u16, cols: u16) -> u32 { (u32::from(rows) << 16) | u32::from(cols) }
impl Drop for ParserWorker {
    fn drop(&mut self) { self.stopped.store(true, Ordering::Release); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ui_snapshot_read_never_waits_for_publication_lock() {
        let (_tx, rx) = mpsc::sync_channel(16);
        let worker = ParserWorker::spawn(rx, 24, 80).unwrap();
        let _lock = worker.latest.lock().unwrap();
        let start = Instant::now();
        for _ in 0..1000 { assert!(worker.screen().is_none()); }
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn output_flood_does_not_hold_ui_or_control_locks() {
        let (tx, rx) = mpsc::sync_channel(16);
        let worker = ParserWorker::spawn(rx, 24, 80).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let stopping = done.clone();
        let producer = thread::spawn(move || {
            let bytes = b"\x1b[65535L\x1b[65535Ttext\r\n".repeat(100);
            while !stopping.load(Ordering::Acquire) {
                match tx.try_send(bytes.clone()) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => thread::yield_now(),
                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                }
            }
        });
        let start = Instant::now();
        let deadline = start + Duration::from_secs(3);
        let mut frames = 0;
        while frames < 2 {
            let before = Instant::now();
            if worker.screen().is_some() { frames += 1; }
            worker.resize(24, 80);
            worker.scroll(0);
            assert!(before.elapsed() < Duration::from_millis(250));
            assert!(Instant::now() < deadline, "no frame published under flood");
            thread::sleep(Duration::from_millis(1));
        }
        done.store(true, Ordering::Release);
        producer.join().unwrap();
        let before = Instant::now();
        drop(worker);
        assert!(before.elapsed() < Duration::from_millis(250));
    }
}
