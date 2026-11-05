//! Only a configured immediate proxy may supply one overwritten client address.
use std::net::IpAddr;
use axum::http::HeaderMap;
use super::{Config, Error};

pub(crate) struct Cidr { network: IpAddr, prefix: u32 }

impl Cidr {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let (address, prefix) = value.split_once('/')?;
        let network: IpAddr = address.parse().ok()?;
        let prefix: u32 = prefix.parse().ok()?;
        let bits = if network.is_ipv4() { 32 } else { 128 };
        if prefix > bits { return None; }
        Some(Self { network, prefix })
    }

    fn matches(&self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = u32::MAX.checked_shl(32 - self.prefix).unwrap_or(0);
                u32::from(network) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = u128::MAX.checked_shl(128 - self.prefix).unwrap_or(0);
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

pub(crate) fn client_ip(config: &Config, peer: IpAddr, headers: &HeaderMap) -> Result<IpAddr, Error> {
    let mut trusted = false;
    for value in &config.trusted_proxy_cidrs {
        let cidr = Cidr::parse(value).ok_or_else(Error::unavailable)?;
        trusted |= cidr.matches(peer);
    }
    if !trusted { return Ok(peer); }
    // Chains and duplicate headers are refused: the immediate proxy must replace
    // X-Forwarded-For with the client IP, never append untrusted header content.
    let mut values = headers.get_all("x-forwarded-for").iter();
    let value = values.next().ok_or_else(Error::invalid)?;
    if values.next().is_some() { return Err(Error::invalid()); }
    value.to_str().ok().and_then(|value| value.parse().ok()).ok_or_else(Error::invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a2a_proxy_headers_require_explicit_trusted_peer_and_single_ip() {
        let peer = "192.0.2.4".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.7".parse().unwrap());
        let mut config = Config::default();
        assert_eq!(client_ip(&config, peer, &headers).unwrap(), peer);
        config.trusted_proxy_cidrs = vec!["192.0.2.0/24".into()];
        assert_eq!(client_ip(&config, peer, &headers).unwrap(), "198.51.100.7".parse::<IpAddr>().unwrap());
        let outsider = "192.0.3.4".parse().unwrap();
        assert_eq!(client_ip(&config, outsider, &headers).unwrap(), outsider);
        for invalid in ["198.51.100.7, 203.0.113.1", "garbage", "198.51.100.7:80", ""] {
            headers.insert("x-forwarded-for", invalid.parse().unwrap());
            assert!(client_ip(&config, peer, &headers).is_err());
        }
        headers.insert("x-forwarded-for", "198.51.100.7".parse().unwrap());
        headers.append("x-forwarded-for", "203.0.113.1".parse().unwrap());
        assert!(client_ip(&config, peer, &headers).is_err());
        assert!(client_ip(&config, peer, &HeaderMap::new()).is_err());
    }

    #[test]
    fn a2a_proxy_cidrs_validate_prefixes_and_address_families() {
        for invalid in ["192.0.2.1", "192.0.2.1/33", "::1/129", "host/24", "::1/-1", "::1/0/0"] {
            assert!(Cidr::parse(invalid).is_none(), "{invalid}");
        }
        let cidr = Cidr::parse("2001:db8::/32").unwrap();
        assert!(cidr.matches("2001:db8:1::2".parse().unwrap()));
        assert!(!cidr.matches("2001:db9::2".parse().unwrap()));
        assert!(!cidr.matches("192.0.2.1".parse().unwrap()));
        assert!(Cidr::parse("0.0.0.0/0").unwrap().matches("203.0.113.1".parse().unwrap()));
        assert!(Cidr::parse("::1/128").unwrap().matches("::1".parse().unwrap()));
        assert!(!Cidr::parse("::1/128").unwrap().matches("::2".parse().unwrap()));
    }

    #[test]
    fn a2a_proxy_config_defaults_to_none_and_invalid_cidrs_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, "[a2a]\n").unwrap();
        assert!(Config::from_home(home.path()).unwrap().trusted_proxy_cidrs.is_empty());
        for value in ["true", "['bad']", "['192.0.2.0/33']", "['::/129']"] {
            std::fs::write(&path, format!("[a2a]\ntrusted_proxy_cidrs={value}\n")).unwrap();
            assert!(Config::from_home(home.path()).is_err());
        }
        std::fs::write(&path, "[a2a]\ntrusted_proxy_cidrs=['192.0.2.0/24', '2001:db8::/32']\n").unwrap();
        assert_eq!(Config::from_home(home.path()).unwrap().trusted_proxy_cidrs.len(), 2);
    }

}
