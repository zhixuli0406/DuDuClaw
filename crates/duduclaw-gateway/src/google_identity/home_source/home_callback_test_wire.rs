//! cfg(test) owned loopback fixture ONLY. Declared, not executed by this source
//! phase. Future Root sandbox/PG/cleanup plan must explicitly authorize it.
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

// Shared by the actual owned Wire and the regression below. This entire
// module is cfg(test); the observer receives only a live descriptor and length.
fn read_owned_request(
    stream: &mut std::net::TcpStream,
    mut progress: impl FnMut(&std::net::TcpStream, usize),
) -> Vec<u8> {
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
        progress(stream, request.len());
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
    request
}

pub(crate) struct Wire {
    home: PathBuf,
    stop: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let stopped = stop.clone();
        let seen = count.clone();
        let scenario = scenario.to_owned();
        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !stopped.load(Ordering::SeqCst) && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("owned accept: {e}"),
                };
                let _request = read_owned_request(&mut stream, |_, _| {});
                seen.fetch_add(1, Ordering::SeqCst);
                if let Some(hook) = hook.take() {
                    hook();
                }
                if scenario == "reset" {
                    continue;
                }
                if scenario == "timeout" {
                    let hold = Instant::now() + Duration::from_secs(22);
                    while !stopped.load(Ordering::SeqCst) && Instant::now() < hold {
                        std::thread::sleep(Duration::from_millis(5));
                    }
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
                } else if scenario == "rejected" {
                    "403 Forbidden"
                } else {
                    "200 OK"
                };
                let redirect = if scenario == "redirect" {
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
            thread: Some(thread),
        }
    }
    pub(crate) fn url(&self) -> String {
        endpoint(&self.home).unwrap()
    }
    pub(crate) fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        endpoints().lock().unwrap().remove(&self.home);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{net::TcpStream, os::fd::AsRawFd, sync::mpsc};

    #[test]
    fn owned_wire_normalizes_seeded_nonblocking_stream_and_reads_split_request_body() {
        const HEADER: &[u8] =
            b"POST /token HTTP/1.1\r\nHost: owned.test\r\nContent-Length: 8\r\n\r\n";
        const BODY: &[u8] = b"abcdefgh";
        const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
        const PARTIAL_HEADER: usize = 8;
        // Synchronization bounds only; actual socket I/O retains Wire's 200ms.
        const BARRIER_BOUND: Duration = Duration::from_secs(2);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect_timeout(
            &listener.local_addr().unwrap(),
            Duration::from_secs(1),
        )
        .unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        // Seed the exact platform-independent precondition before the helper.
        accepted.set_nonblocking(true).unwrap();
        let mut no_data = [0; 1];
        let seeded_failure = accepted.read(&mut no_data).unwrap_err().kind();
        assert_eq!(seeded_failure, std::io::ErrorKind::WouldBlock);
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        // The test controls segment delivery, not Nagle/delayed-ACK timing.
        client.set_nodelay(true).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let partial_body = HEADER.len() + 3;

        // Scoped threads always join, including assertion/panic paths; channel
        // waits and socket I/O are bounded, so a failed oracle cannot park one.
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                client.write_all(&HEADER[..PARTIAL_HEADER]).unwrap();
                ready_tx.send(()).unwrap();
                assert_eq!(progress_rx.recv_timeout(BARRIER_BOUND).unwrap(), 1);
                client.write_all(&HEADER[PARTIAL_HEADER..]).unwrap();
                client.write_all(&BODY[..3]).unwrap();
                release_tx.send(1).unwrap();
                assert_eq!(progress_rx.recv_timeout(BARRIER_BOUND).unwrap(), 2);
                client.write_all(&BODY[3..]).unwrap();
                release_tx.send(2).unwrap();
                let mut response = [0; RESPONSE.len()];
                client.read_exact(&mut response).unwrap();
                assert_eq!(response.as_slice(), RESPONSE);
            });
            ready_rx.recv_timeout(BARRIER_BOUND).unwrap();
            let mut observed = 0;
            let request = read_owned_request(&mut accepted, |stream, received| {
                let stage = if received == PARTIAL_HEADER {
                    1
                } else if received == partial_body {
                    2
                } else {
                    return;
                };
                // Only flags, a byte count and ErrorKind enter diagnostics.
                // SAFETY: stream owns this live descriptor during the call.
                let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
                assert!(flags >= 0, "stage={stage}, seeded_kind={seeded_failure:?}");
                assert_eq!(
                    flags & libc::O_NONBLOCK,
                    0,
                    "stage={stage}, received={received}, seeded_kind={seeded_failure:?}"
                );
                progress_tx.send(stage).unwrap();
                assert_eq!(release_rx.recv_timeout(BARRIER_BOUND).unwrap(), stage);
                observed += 1;
            });
            assert_eq!(observed, 2);
            assert_eq!(request, [HEADER, BODY].concat());
            accepted.write_all(RESPONSE).unwrap();
            writer.join().unwrap();
        });
    }
}
