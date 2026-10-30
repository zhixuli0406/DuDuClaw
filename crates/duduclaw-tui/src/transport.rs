//! Validated origins shared by REST and WebSocket clients.
use crate::{Error, Result};
use std::path::Path;
use url::{Host, Url};

#[derive(Clone, Debug)]
pub struct GatewayUrl {
    origin: Url,
    loopback: bool,
}
impl GatewayUrl {
    pub fn parse(input: &str) -> Result<Self> {
        let mut origin = Url::parse(input).map_err(|_| Error::InvalidUrl)?;
        if !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !matches!(origin.path(), "" | "/" | "/ws")
        {
            return Err(Error::InvalidUrl);
        }
        let loopback = match origin.host() {
            Some(Host::Domain("localhost")) => true,
            Some(Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
            Some(Host::Ipv6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
            Some(_) => false,
            None => return Err(Error::InvalidUrl),
        };
        let scheme = match origin.scheme() {
            "http" | "ws" if loopback => "http",
            "http" | "ws" => return Err(Error::TlsRequired),
            "https" | "wss" => "https",
            _ => return Err(Error::InvalidUrl),
        };
        origin.set_scheme(scheme).map_err(|_| Error::InvalidUrl)?;
        origin.set_path("/");
        Ok(Self { origin, loopback })
    }
    /// Same resolver used by CLI startup: env, home config, default port.
    pub fn for_home(home: &Path) -> Result<Self> {
        let (port, _) = duduclaw_core::gateway_port_for_home(home);
        Self::parse(&format!("http://127.0.0.1:{port}"))
    }
    pub fn is_loopback(&self) -> bool {
        self.loopback
    }
    pub(crate) fn endpoint(&self, path: &str) -> Url {
        let mut url = self.origin.clone();
        url.set_path(path);
        url
    }
    pub fn websocket(&self) -> Url {
        let mut url = self.endpoint("/ws");
        // Both replacements are special URL schemes and cannot fail.
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        let _ = url.set_scheme(scheme);
        url
    }
    pub(crate) fn http_client(&self) -> Result<reqwest::Client> {
        // Never follow redirects with credentials; never route local login
        // through an environment-configured proxy. TLS verification stays on.
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| Error::Network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_hosts_and_tls() {
        for url in [
            "http://127.0.0.1:8080",
            "ws://[::1]:8/ws",
            "http://localhost",
            "wss://example.com/ws",
        ] {
            assert!(GatewayUrl::parse(url).is_ok());
        }
        for url in [
            "http://localhost.evil",
            "ws://127.0.0.2",
            "http://192.168.0.1",
            "http://[::ffff:127.0.0.1]",
            "http://localhost.",
        ] {
            assert!(matches!(GatewayUrl::parse(url), Err(Error::TlsRequired)));
        }
        for url in [
            "http://user:secret@localhost",
            "http://localhost/?jwt=secret",
            "http://localhost/#secret",
            "ftp://localhost",
            "https://example.com/api",
        ] {
            assert!(GatewayUrl::parse(url).is_err());
        }
    }
    #[test]
    fn derives_endpoints_without_losing_tls_or_port() {
        let url = GatewayUrl::parse("wss://example.com:8443/ws").unwrap();
        assert_eq!(
            url.endpoint("/api/login").as_str(),
            "https://example.com:8443/api/login"
        );
        assert_eq!(url.websocket().as_str(), "wss://example.com:8443/ws");
    }
}
