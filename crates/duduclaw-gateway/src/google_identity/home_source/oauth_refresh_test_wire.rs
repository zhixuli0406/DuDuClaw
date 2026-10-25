//! C1 cfg(test) owned loopback fixture; all tests UNRUN.
//! No runtime/network/key authorization follows from this source.
use super::*;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Instant,
};
fn endpoints() -> &'static Mutex<HashMap<PathBuf, String>> {
    static E: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    E.get_or_init(Default::default)
}
pub(super) fn endpoint(home: &Path) -> Option<String> {
    endpoints()
        .lock()
        .unwrap()
        .get(&home.canonicalize().ok()?)
        .cloned()
}
fn readiness() -> &'static Mutex<HashMap<PathBuf, Arc<AtomicBool>>> {
    static READY: OnceLock<Mutex<HashMap<PathBuf, Arc<AtomicBool>>>> = OnceLock::new();
    READY.get_or_init(Default::default)
}
/// Arm only the private Wire belonging to this owned home. Preparation and
/// unrelated endpoint guards do not spend the fixture's accept budget.
pub(super) fn send_ready(home: &Path) {
    if let Some(home) = home.canonicalize().ok() {
        if let Some(ready) = readiness().lock().unwrap().get(&home) {
            ready.store(true, Ordering::SeqCst);
        }
    }
}
/// Only rebinds a canonical owned test home to an already-owned loopback
/// fixture. No ENV, production URL input or captured-client relaxation.
pub(crate) struct EndpointGuard {
    home: PathBuf,
    url: String,
}
impl EndpointGuard {
    pub(crate) fn owned_loopback(home: &Path, url: &str) -> Self {
        let parsed = url::Url::parse(url).unwrap();
        assert_eq!(parsed.scheme(), "http");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert!(
            parsed.port().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none()
        );
        let home = home.canonicalize().unwrap();
        assert!(
            endpoints()
                .lock()
                .unwrap()
                .insert(home.clone(), url.to_owned())
                .is_none()
        );
        Self {
            home,
            url: url.to_owned(),
        }
    }
}
impl Drop for EndpointGuard {
    fn drop(&mut self) {
        let mut map = endpoints().lock().unwrap();
        if map.get(&self.home) == Some(&self.url) {
            map.remove(&self.home);
        }
    }
}

pub(crate) struct Wire {
    home: PathBuf,
    stop: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    chunked_without_length: Arc<AtomicBool>,
    chunked_written: Arc<AtomicUsize>,
    chunked_finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Wire {
    pub(crate) fn json(home: &Path, body: Option<Value>) -> Self {
        Self::start(home, "ok", body, None)
    }
    pub(crate) fn scenario(home: &Path, scenario: &str) -> Self {
        Self::start(home, scenario, None, None)
    }
    pub(crate) fn with_hook(home: &Path, hook: impl FnOnce() + Send + 'static) -> Self {
        Self::start(home, "ok", None, Some(Box::new(hook)))
    }
    fn start(
        home: &Path,
        scenario: &str,
        body: Option<Value>,
        mut hook: Option<Box<dyn FnOnce() + Send>>,
    ) -> Self {
        let home = home.canonicalize().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        assert!(
            endpoints()
                .lock()
                .unwrap()
                .insert(home.clone(), endpoint.clone())
                .is_none()
        );
        let ready = Arc::new(AtomicBool::new(false));
        assert!(readiness().lock().unwrap().insert(home.clone(), ready.clone()).is_none());
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let chunked_without_length = Arc::new(AtomicBool::new(false));
        let chunked_written = Arc::new(AtomicUsize::new(0));
        let chunked_finished = Arc::new(AtomicBool::new(false));
        let no_length = chunked_without_length.clone();
        let written = chunked_written.clone();
        let finished = chunked_finished.clone();
        let stopped = stop.clone();
        let seen = count.clone();
        let scenario = scenario.to_owned();
        let thread = std::thread::spawn(move || {
            // Timeout sockets stay open beyond the REAL production 20s
            // deadline. Keep accepting to count any accidental second attempt.
            // Keep the original maximum leak bound. The non-timeout 5s
            // accept budget starts at send readiness, not while the caller
            // is still reading/canonicalizing a near-4MiB owned fixture.
            let leak_deadline = Instant::now() + Duration::from_secs(30);
            let mut active_deadline = None;
            let mut held_connections = Vec::new();
            while !stopped.load(Ordering::SeqCst) && Instant::now() < leak_deadline {
                if ready.load(Ordering::SeqCst) && active_deadline.is_none() {
                    active_deadline = Some(Instant::now() + Duration::from_secs(
                        if scenario == "timeout" { 30 } else { 5 }
                    ));
                }
                if active_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    break;
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("owned accept: {e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(200)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 4096];
                while request.len() < 64 * 1024 {
                    let n = match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                seen.fetch_add(1, Ordering::SeqCst);
                if let Some(hook) = hook.take() {
                    hook();
                }
                if scenario == "reset" {
                    continue;
                }
                if scenario == "timeout" {
                    held_connections.push(stream);
                    continue;
                }
                if scenario == "chunked_overflow" {
                    // Fully valid JSON with normal token fields: if only the
                    // cumulative bound is removed, ignored padding permits
                    // parsing/sealing/publication rather than another parser
                    // limit incidentally rejecting this response.
                    let body = serde_json::to_vec(&serde_json::json!({
                        "access_token":"owned-access", "refresh_token":"owned-refresh",
                        "token_type":"Bearer", "expires_in":3600,
                        "ignored_padding":"A".repeat(4 * 1024 * 1024 + 4096)
                    }))
                    .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").is_ok() {
                        no_length.store(true, Ordering::SeqCst);
                        for chunk in body.chunks(64 * 1024) {
                            let size = format!("{:X}\r\n", chunk.len());
                            if stream.write_all(size.as_bytes()).and_then(|_| stream.write_all(chunk)).and_then(|_| stream.write_all(b"\r\n")).is_err() { break; }
                            written.fetch_add(chunk.len(), Ordering::SeqCst);
                        }
                        let _ = stream.write_all(b"0\r\n\r\n");
                    }
                    finished.store(true, Ordering::SeqCst);
                    continue;
                }
                if scenario == "chunk_error" {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n100\r\n{}");
                    continue;
                }
                let mut payload=body.clone().unwrap_or_else(||serde_json::json!({"access_token":"owned-access","refresh_token":"owned-refresh","token_type":"Bearer","expires_in":3600}));
                if scenario == "partial_scope" {
                    payload["scope"] = serde_json::json!("scope-a");
                }
                if scenario == "bad_expiry" {
                    payload["expires_in"] = serde_json::json!(0);
                }
                let bytes = serde_json::to_vec(&payload).unwrap();
                let status = if scenario == "redirect" {
                    "302 Found"
                } else if scenario == "redirect303" {
                    "303 See Other"
                } else if scenario == "redirect307" {
                    "307 Temporary Redirect"
                } else if scenario == "redirect308" {
                    "308 Permanent Redirect"
                } else if scenario == "rejected" {
                    "403 Forbidden"
                } else {
                    "200 OK"
                };
                let redirect = if ["redirect", "redirect303", "redirect307", "redirect308"]
                    .contains(&scenario.as_str())
                {
                    format!("Location: {endpoint}/unexpected\r\n")
                } else {
                    String::new()
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\n{redirect}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream
                    .write_all(header.as_bytes())
                    .and_then(|_| stream.write_all(&bytes));
            }
        });
        Self {
            home,
            stop,
            count,
            chunked_without_length,
            chunked_written,
            chunked_finished,
            thread: Some(thread),
        }
    }
    pub(crate) fn url(&self) -> String {
        endpoint(&self.home).unwrap()
    }
    pub(crate) fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
    pub(crate) fn chunked_without_content_length(&self) -> bool {
        self.chunked_without_length.load(Ordering::SeqCst)
    }
    pub(crate) fn chunked_body_written(&self) -> usize {
        self.chunked_written.load(Ordering::SeqCst)
    }
    pub(crate) fn chunked_finished(&self) -> bool {
        self.chunked_finished.load(Ordering::SeqCst)
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        endpoints().lock().unwrap().remove(&self.home);
        readiness().lock().unwrap().remove(&self.home);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
