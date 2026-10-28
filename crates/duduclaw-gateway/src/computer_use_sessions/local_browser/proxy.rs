//! Authenticated CONNECT-only proxy. DNS results are all checked and the socket
//! is opened to the checked IP, never by resolving the hostname a second time.
use base64::Engine;
use rand::RngCore;
use std::{collections::HashSet, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct ConnectProxy {
    pub address: SocketAddr,
    username: String,
    password: String,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl ConnectProxy {
    pub(crate) async fn start(hosts: Vec<String>) -> Result<Self, String> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "proxy unavailable")?;
        let address = listener.local_addr().map_err(|_| "proxy unavailable")?;
        let username = "session".to_owned();
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let password = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let credential = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
        );
        let allowed: Arc<HashSet<String>> = Arc::new(hosts.into_iter().collect());
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move {
            // Bounded concurrent tunnels, including handshakes from unauthenticated peers.
            let slots = Arc::new(tokio::sync::Semaphore::new(64));
            loop {
                let incoming = tokio::select! { _ = token.cancelled() => break, next = listener.accept() => next };
                let Ok((socket, peer)) = incoming else { break };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let hosts = allowed.clone();
                let credential = credential.clone();
                let stop = token.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    tokio::select! { _ = stop.cancelled() => {}, _ = tunnel(socket, &hosts, &credential) => {} }
                });
            }
        });
        Ok(Self {
            address,
            username,
            password,
            cancel,
            task,
        })
    }
    pub(crate) fn challenge_response(&self, origin: &str, source: &str) -> Option<(&str, &str)> {
        (source == "Proxy" && origin == format!("http://{}", self.address))
            .then_some((&self.username, &self.password))
    }
}
impl Drop for ConnectProxy {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

fn parse_connect<'a>(
    header: &'a str,
    hosts: &HashSet<String>,
    credential: &str,
) -> Result<&'a str, u16> {
    let mut lines = header.split("\r\n");
    let first: Vec<_> = lines.next().ok_or(400u16)?.split(' ').collect();
    if first.len() != 3 || first[0] != "CONNECT" || first[2] != "HTTP/1.1" {
        return Err(403);
    }
    let mut auth = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or(400u16)?;
        if key.eq_ignore_ascii_case("proxy-authorization") {
            if auth.replace(value.trim()).is_some() {
                return Err(400);
            }
        }
        if key.eq_ignore_ascii_case("content-length") && value.trim() != "0" {
            return Err(400);
        }
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err(400);
        }
    }
    // HMAC verify gives a constant-time comparison without logging a credential.
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let auth = auth.ok_or(407u16)?;
    let mut expected = Hmac::<Sha256>::new_from_slice(credential.as_bytes()).map_err(|_| 403u16)?;
    expected.update(b"local-proxy");
    let digest = expected.finalize().into_bytes();
    let mut actual = Hmac::<Sha256>::new_from_slice(auth.as_bytes()).map_err(|_| 403u16)?;
    actual.update(b"local-proxy");
    actual.verify_slice(&digest).map_err(|_| 407u16)?;
    let host = first[1].strip_suffix(":443").ok_or(403u16)?;
    if duduclaw_core::types::normalize_navigation_host(host).as_deref() != Some(host)
        || !hosts.contains(host)
    {
        return Err(403);
    }
    Ok(host)
}

async fn tunnel(
    mut client: TcpStream,
    hosts: &HashSet<String>,
    credential: &str,
) -> Result<(), ()> {
    let mut bytes = Vec::new();
    let parsed = tokio::time::timeout(Duration::from_secs(3), async {
        while !bytes.ends_with(b"\r\n\r\n") {
            if bytes.len() >= 8192 {
                return Err(431u16);
            }
            let b = client.read_u8().await.map_err(|_| 400u16)?;
            bytes.push(b);
        }
        let header = std::str::from_utf8(&bytes).map_err(|_| 400u16)?;
        parse_connect(header, hosts, credential).map(str::to_owned)
    })
    .await
    .unwrap_or(Err(408));
    let host = match parsed {
        Ok(host) => host,
        Err(code) => {
            let challenge = if code == 407 {
                "Proxy-Authenticate: Basic realm=\"DuDuClaw local browser\"\r\n"
            } else {
                ""
            };
            let _ = client.write_all(format!("HTTP/1.1 {code} Refused\r\n{challenge}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
            return Err(());
        }
    };
    let addresses = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host.as_str(), 443)),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?
    .collect::<Vec<_>>();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|a| !duduclaw_core::net_addr::is_public_ip(&a.ip()))
    {
        return Err(());
    }
    let mut upstream = None;
    for ip in addresses.into_iter().take(8) {
        if let Ok(Ok(socket)) =
            tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(ip)).await
        {
            upstream = Some(socket);
            break;
        }
    }
    let mut upstream = upstream.ok_or(())?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .map_err(|_| ())?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .map_err(|_| ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_plain_http_ip_loopback_port_override_or_auth_smuggling() {
        let hosts = HashSet::from(["example.com".to_owned()]);
        let request = |authority: &str| {
            format!("CONNECT {authority} HTTP/1.1\r\nProxy-Authorization: Basic synthetic\r\n\r\n")
        };
        assert_eq!(
            parse_connect(&request("example.com:443"), &hosts, "Basic synthetic"),
            Ok("example.com")
        );
        for host in [
            "example.com:80",
            "example.com:444",
            "127.0.0.1:443",
            "[::1]:443",
            "example.com.:443",
            "Example.com:443",
            "other.com:443",
            "user@example.com:443",
        ] {
            assert!(
                parse_connect(&request(host), &hosts, "Basic synthetic").is_err(),
                "{host}"
            );
        }
        assert_eq!(
            parse_connect(
                "GET https://example.com HTTP/1.1\r\n\r\n",
                &hosts,
                "Basic synthetic"
            ),
            Err(403)
        );
        assert_eq!(
            parse_connect(
                "CONNECT example.com:443 HTTP/1.1\r\n\r\n",
                &hosts,
                "Basic synthetic"
            ),
            Err(407)
        );
        assert_eq!(
            parse_connect(
                &request("example.com:443").replace(
                    "\r\n\r\n",
                    "\r\nProxy-Authorization: Basic synthetic\r\n\r\n"
                ),
                &hosts,
                "Basic synthetic"
            ),
            Err(400)
        );
    }
    #[tokio::test]
    async fn only_exact_proxy_origin_receives_random_per_session_credentials() {
        let proxy = ConnectProxy::start(vec![]).await.unwrap();
        assert!(
            proxy
                .challenge_response(&format!("http://{}", proxy.address), "Proxy")
                .is_some()
        );
        assert!(
            proxy
                .challenge_response("https://example.com", "Server")
                .is_none()
        );
        assert!(
            proxy
                .challenge_response(&format!("http://{}", proxy.address), "Server")
                .is_none()
        );
        let second = ConnectProxy::start(vec![]).await.unwrap();
        assert_ne!(proxy.password, second.password);
    }
}
