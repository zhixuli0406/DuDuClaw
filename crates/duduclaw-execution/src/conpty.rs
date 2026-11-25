//! Startup handshake for portable-pty's fixed INHERIT_CURSOR flag on Windows.
use std::{io::{self, Read, Write}, sync::mpsc};

type Writer = Box<dyn Write + Send>;
const QUERY: &[u8] = b"\x1b[6n";

/// Answer and consume only conhost's first cursor query, across read boundaries.
/// portable-pty 0.9 has no flag override. Hold normal input until the cursor
/// reply is flushed, so a prompt cannot fill the input pipe ahead of the reply.
/// These halves own pipe handles only, never the master / pseudoconsole.
pub fn startup_io(reader: Box<dyn Read + Send>, writer: Writer) -> (Box<dyn Read + Send>, Writer) {
    let (ready, waiting) = mpsc::channel();
    (Box::new(StartupReader { reader, writer: Some(writer), ready: Some(ready),
        pending: Vec::new(), available: 0, eof: false }),
        Box::new(StartupWriter { waiting, writer: None }))
}

struct StartupReader {
    reader: Box<dyn Read + Send>,
    writer: Option<Writer>,
    ready: Option<mpsc::Sender<Writer>>,
    pending: Vec<u8>,
    available: usize,
    eof: bool,
}
impl Read for StartupReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() { return Ok(0); }
        loop {
            if self.available > 0 {
                let count = self.available.min(buf.len());
                buf[..count].copy_from_slice(&self.pending[..count]);
                self.pending.drain(..count);
                self.available -= count;
                return Ok(count);
            }
            if self.eof { return Ok(0); }
            if self.writer.is_none() { return self.reader.read(buf); }
            let mut chunk = [0; 8192];
            let count = match self.reader.read(&mut chunk) {
                Ok(count) => count,
                Err(error) => {
                    // Release a writer blocked on startup; subsequent reads
                    // still drain the underlying pipe during console teardown.
                    self.ready.take();
                    self.writer.take();
                    return Err(error);
                },
            };
            if count == 0 {
                self.ready.take();
                self.writer.take();
                self.eof = true;
                self.available = self.pending.len();
                continue;
            }
            self.pending.extend_from_slice(&chunk[..count]);
            if let Some(index) = self.pending.windows(QUERY.len()).position(|bytes| bytes == QUERY) {
                self.pending.drain(index..index + QUERY.len());
                let mut writer = self.writer.take().unwrap();
                if let Err(error) = writer.write_all(b"\x1b[1;1R").and_then(|()| writer.flush()) {
                    self.ready.take();
                    return Err(error);
                }
                // Dropping the input half must not prevent the startup reply
                // or stop output draining; a closed receiver simply drops it.
                let _ = self.ready.take().unwrap().send(writer);
                self.available = self.pending.len();
            } else {
                let partial = (1..QUERY.len()).rev()
                    .find(|&len| self.pending.ends_with(&QUERY[..len])).unwrap_or(0);
                self.available = self.pending.len() - partial;
            }
        }
    }
}

struct StartupWriter {
    waiting: mpsc::Receiver<Writer>,
    writer: Option<Writer>,
}
impl StartupWriter {
    fn ready(&mut self) -> io::Result<&mut Writer> {
        if self.writer.is_none() {
            self.writer = Some(self.waiting.recv().map_err(|_| io::Error::new(
                io::ErrorKind::BrokenPipe, "ConPTY closed before its startup cursor query"))?);
        }
        Ok(self.writer.as_mut().unwrap())
    }
}
impl Write for StartupWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { self.ready()?.write(buf) }
    fn flush(&mut self) -> io::Result<()> { self.ready()?.flush() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, sync::{Arc, Mutex}};

    struct Fragmented { bytes: Cursor<Vec<u8>>, size: usize }
    impl Read for Fragmented {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let size = buf.len().min(self.size);
            self.bytes.read(&mut buf[..size])
        }
    }
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    #[test]
    fn startup_answers_split_query_before_input_and_preserves_other_output() {
        for size in 1..=32 {
            let captured = Arc::new(Mutex::new(Vec::new()));
            let reader = Fragmented { bytes: Cursor::new(b"\x1b[?25h\x1b\x1b[6nready\x1b[6n\x1b[6".to_vec()), size };
            let (mut reader, mut writer) = startup_io(Box::new(reader), Box::new(Capture(captured.clone())));
            let writing = std::thread::spawn(move || writer.write_all(b"prompt\r").unwrap());
            let mut output = Vec::new();
            reader.read_to_end(&mut output).unwrap();
            writing.join().unwrap();
            assert_eq!(output, b"\x1b[?25h\x1bready\x1b[6n\x1b[6");
            assert_eq!(*captured.lock().unwrap(), b"\x1b[1;1Rprompt\r");
        }
    }

    #[test]
    fn eof_without_query_releases_waiting_writer_and_keeps_partial_escape() {
        let (mut reader, mut writer) = startup_io(Box::new(Cursor::new(b"tail\x1b[6")), Box::new(io::sink()));
        let writing = std::thread::spawn(move || writer.write_all(b"prompt").unwrap_err().kind());
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        assert_eq!(output, b"tail\x1b[6");
        assert_eq!(writing.join().unwrap(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn dropped_input_still_answers_startup_and_drains_output() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (mut reader, writer) = startup_io(Box::new(Cursor::new(b"\x1b[6nready")), Box::new(Capture(captured.clone())));
        drop(writer);
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        assert_eq!(output, b"ready");
        assert_eq!(*captured.lock().unwrap(), b"\x1b[1;1R");
    }
}
