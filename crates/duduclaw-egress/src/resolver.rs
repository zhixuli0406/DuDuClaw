//! Host-only DNS. Neither route uses libc DNS or an egress resolver IP exception.
use std::{future::Future, net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr}, pin::Pin, time::Duration};
use duduclaw_isolation::{bounded::{List, Text}, egress::{DnsRoute, EgressError}};
use serde::{Deserialize, Serialize};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpStream, UnixListener, UnixStream}, task::JoinSet};

use crate::dns_wire;

pub const RESOLVED_SOCKET: &str = "/run/systemd/resolve/io.systemd.Resolve";
pub const DNS_BROKER_SOCKET: &str = "/run/duduclaw-dns-broker/resolve.sock";
pub const MAX_DNS_RESPONSE_BYTES: usize = 64 * 1024;
pub const MAX_DNS_ADDRESSES: usize = 64;
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
// systemd v249 resolved-def.h: DNS, NO_SEARCH, NO_SYNTHESIZE, NO_CACHE.
const RESOLVE_FLAGS: u64 = 1 | (1 << 8) | (1 << 11) | (1 << 12);
const RESOLVE_METHOD: &str = "io.systemd.Resolve.ResolveHostname";

pub trait Resolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str, route: DnsRoute)
        -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, EgressError>> + Send + 'a>>;
}

/// Routes are installed by root; availability failure never changes the route.
#[derive(Default)]
pub struct HostResolver;

impl HostResolver {
    pub fn new() -> Self { Self }
    pub async fn probe(&self, route: DnsRoute) -> Result<(), EgressError> {
        tokio::time::timeout(DNS_TIMEOUT, trusted_socket(route)).await.map_err(|_| EgressError::Timeout)??;
        Ok(())
    }
}

impl Resolver for HostResolver {
    fn resolve<'a>(&'a self, host: &'a str, route: DnsRoute)
        -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, EgressError>> + Send + 'a>> {
        Box::pin(async move {
            let host = dns_name(host)?;
            tokio::time::timeout(DNS_TIMEOUT, async {
                let mut socket = trusted_socket(route).await?;
                let request = ResolveRequest { method: RESOLVE_METHOD.into(), parameters: ResolveParameters {
                    name: Text::new(host).map_err(|_| EgressError::InvalidRequest)?, family: 0, flags: RESOLVE_FLAGS,
                }};
                write_json(&mut socket, &request).await?;
                let response = read_json(&mut socket, MAX_DNS_RESPONSE_BYTES).await?;
                decode_response(&response)
            }).await.map_err(|_| EgressError::Timeout)?
        })
    }
}

async fn trusted_socket(route: DnsRoute) -> Result<UnixStream, EgressError> {
    use std::{os::unix::fs::{MetadataExt, FileTypeExt}, path::Path};
    let (path, user) = match route {
        DnsRoute::ResolvedVarlink => (RESOLVED_SOCKET, "systemd-resolve"),
        DnsRoute::DedicatedDnsBroker => (DNS_BROKER_SOCKET, "duduclaw-dns"),
    };
    let uid = nix::unistd::User::from_name(user).map_err(|_| EgressError::Unavailable)?
        .ok_or(EgressError::Unavailable)?.uid.as_raw();
    if uid == 0 { return Err(EgressError::Unavailable); }
    for ancestor in Path::new(path).ancestors() {
        let meta = std::fs::symlink_metadata(ancestor).map_err(|_| EgressError::Unavailable)?;
        if meta.file_type().is_symlink() || (meta.uid() != 0 && meta.uid() != uid)
            || (ancestor != Path::new(path) && (!meta.is_dir() || meta.mode() & 0o022 != 0))
            || (ancestor == Path::new(path) && !meta.file_type().is_socket()) { return Err(EgressError::Unavailable); }
    }
    let socket = UnixStream::connect(path).await.map_err(|_| EgressError::Unavailable)?;
    let peer = socket.peer_cred().map_err(|_| EgressError::Unavailable)?;
    // systemd socket activation captures root credentials at listen time.
    if peer.uid() != 0 && peer.uid() != uid { return Err(EgressError::Unavailable); }
    Ok(socket)
}

fn dns_name(host: &str) -> Result<String, EgressError> {
    match crate::policy::normalize_host(host)? {
        crate::policy::Host::Dns(name) => Ok(name),
        crate::policy::Host::Ip(_) => Err(EgressError::InvalidRequest),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest { method: String, parameters: ResolveParameters }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveParameters { name: Text<253>, family: u16, flags: u64 }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Address {
    family: u16,
    address: List<u8, 16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ifindex: Option<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveResult {
    addresses: List<Address, MAX_DNS_ADDRESSES>,
    name: Text<253>,
    flags: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<ResolveResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Text<128>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    continues: Option<bool>,
}

fn decode_response(bytes: &[u8]) -> Result<Vec<IpAddr>, EgressError> {
    if bytes.is_empty() || bytes.len() > MAX_DNS_RESPONSE_BYTES { return Err(EgressError::Unavailable); }
    let response: ResolveResponse = serde_json::from_slice(bytes).map_err(|_| EgressError::Unavailable)?;
    if response.error.is_some() || response.continues == Some(true) { return Err(EgressError::Unavailable); }
    let result = response.parameters.ok_or(EgressError::Unavailable)?;
    // Protocol bits must describe DNS only, and synthesized records are forbidden.
    if result.flags & 31 != 1 || result.flags & (1 << 19) != 0 { return Err(EgressError::Unavailable); }
    if result.addresses.as_slice().is_empty() { return Err(EgressError::Unavailable); }
    result.addresses.as_slice().iter().map(|entry| {
        match (entry.family, entry.address.as_slice()) {
            (2, [a, b, c, d]) => Ok(IpAddr::V4(Ipv4Addr::new(*a, *b, *c, *d))),
            (10, bytes) if bytes.len() == 16 => {
                let octets: [u8; 16] = bytes.try_into().map_err(|_| EgressError::Unavailable)?;
                Ok(IpAddr::V6(Ipv6Addr::from(octets)))
            }
            _ => Err(EgressError::Unavailable),
        }
    }).collect()
}

// Read through the NUL delimiter without an unbounded read_until allocation.
async fn read_json(socket: &mut UnixStream, cap: usize) -> Result<Vec<u8>, EgressError> {
    let mut bytes = Vec::new();
    loop {
        let byte = socket.read_u8().await.map_err(|_| EgressError::Unavailable)?;
        if byte == 0 { return if bytes.is_empty() { Err(EgressError::InvalidRequest) } else { Ok(bytes) }; }
        if bytes.len() == cap { return Err(EgressError::ResourceLimit); }
        bytes.push(byte);
    }
}

async fn write_json<T: Serialize>(socket: &mut UnixStream, value: &T) -> Result<(), EgressError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| EgressError::Unavailable)?;
    if bytes.len() > MAX_DNS_RESPONSE_BYTES { return Err(EgressError::ResourceLimit); }
    bytes.push(0);
    socket.write_all(&bytes).await.map_err(|_| EgressError::Unavailable)
}

/// Dedicated service seam: root installs a protected AF_UNIX listener and pins
/// the upstream IP and egress service uid. This must run in a DIFFERENT process
/// and unit from egress, under its own non-root uid. No arbitrary bytes, qtype,
/// destination or port are accepted from its caller. Upstream transport is TCP53.
pub async fn serve_dns_broker(listener: UnixListener, upstream: SocketAddr, allowed_uid: u32)
    -> Result<(), EgressError> {
    let broker_uid = nix::unistd::geteuid().as_raw();
    if upstream.port() != 53 || upstream.ip().is_unspecified() || upstream.ip().is_multicast()
        || allowed_uid == 0 || broker_uid == 0 || broker_uid == allowed_uid {
        return Err(EgressError::InvalidRequest);
    }
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted.map_err(|_| EgressError::Unavailable)?;
                let peer = socket.peer_cred().map_err(|_| EgressError::Unavailable)?;
                if peer.uid() != allowed_uid || tasks.len() >= 32 { continue; }
                tasks.spawn(async move {
                    let _ = tokio::time::timeout(DNS_TIMEOUT, broker_request(socket, upstream)).await;
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

async fn broker_request(mut socket: UnixStream, upstream: SocketAddr) -> Result<(), EgressError> {
    let bytes = read_json(&mut socket, 1024).await?;
    let request: ResolveRequest = serde_json::from_slice(&bytes).map_err(|_| EgressError::InvalidRequest)?;
    if request.method != RESOLVE_METHOD || request.parameters.family != 0
        || request.parameters.flags != RESOLVE_FLAGS { return Err(EgressError::InvalidRequest); }
    let host = dns_name(request.parameters.name.as_str())?;
    let mut addresses = Vec::new();
    // Never accept a partial answer when either address-family lookup failed.
    for kind in [1, 28] {
        let query = dns_wire::query(&host, kind)?;
        let mut stream = TcpStream::connect(upstream).await.map_err(|_| EgressError::Unavailable)?;
        stream.write_u16(query.len() as u16).await.map_err(|_| EgressError::Unavailable)?;
        stream.write_all(&query).await.map_err(|_| EgressError::Unavailable)?;
        let length = stream.read_u16().await.map_err(|_| EgressError::Unavailable)? as usize;
        if length < 12 { return Err(EgressError::Unavailable); }
        let mut response = vec![0; length];
        stream.read_exact(&mut response).await.map_err(|_| EgressError::Unavailable)?;
        addresses.extend(dns_wire::answer(&response, &host, kind)?);
        if addresses.len() > MAX_DNS_ADDRESSES { return Err(EgressError::ResourceLimit); }
    }
    if addresses.is_empty() { return Err(EgressError::Unavailable); }
    let addresses = addresses.into_iter().map(|ip| {
        let (family, bytes) = match ip {
            IpAddr::V4(ip) => (2, ip.octets().to_vec()),
            IpAddr::V6(ip) => (10, ip.octets().to_vec()),
        };
        Ok(Address { family, address: List::new(bytes).map_err(|_| EgressError::Unavailable)?, ifindex: None })
    }).collect::<Result<Vec<_>, EgressError>>()?;
    write_json(&mut socket, &ResolveResponse { parameters: Some(ResolveResult {
        addresses: List::new(addresses).map_err(|_| EgressError::ResourceLimit)?,
        name: Text::new(host).map_err(|_| EgressError::InvalidRequest)?, flags: 1,
    }), error: None, continues: None }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(addresses: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"parameters": {
            "addresses": addresses, "name": "example.com", "flags": 1,
        }})).unwrap()
    }

    #[test]
    fn resolver_parses_both_families_without_filtering_private_answers() {
        let bytes = response(serde_json::json!([
            {"family": 2, "address": [8, 8, 8, 8]},
            {"family": 2, "address": [127, 0, 0, 1]},
            {"family": 10, "address": [32, 1, 72, 96, 72, 96, 0, 0, 0, 0, 0, 0, 0, 0, 136, 136], "ifindex": 2}
        ]));
        let values = decode_response(&bytes).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(values[1], "127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn resolver_rejects_malformed_empty_oversized_and_partial_replies() {
        for addresses in [serde_json::json!([]), serde_json::json!([{"family":2,"address":[1,2,3]}]),
            serde_json::json!([{"family":40,"address":[1,2,3,4]}]),
            serde_json::json!([{"family":2,"address":[256,2,3,4]}])] {
            assert!(decode_response(&response(addresses)).is_err());
        }
        assert!(decode_response(&vec![b' '; MAX_DNS_RESPONSE_BYTES + 1]).is_err());
        let addresses = vec![serde_json::json!({"family":2,"address":[8,8,8,8]}); MAX_DNS_ADDRESSES + 1];
        assert!(decode_response(&response(serde_json::json!(addresses))).is_err());
        for extra in [serde_json::json!({"continues":true}), serde_json::json!({"error":"error"})] {
            let mut value: serde_json::Value = serde_json::from_slice(&response(serde_json::json!([
                {"family":2,"address":[8,8,8,8]}]))).unwrap();
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            assert!(decode_response(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }

    #[test]
    fn resolver_rejects_synthesized_or_non_dns_answers() {
        for flags in [0, 2, 3, 1 | (1 << 19)] {
            let mut value: serde_json::Value = serde_json::from_slice(&response(serde_json::json!([
                {"family":2,"address":[8,8,8,8]}]))).unwrap();
            value["parameters"]["flags"] = flags.into();
            assert!(decode_response(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }

    #[tokio::test]
    async fn resolver_stream_read_is_bounded_and_requires_nul() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(b"12345\0").await.unwrap();
        assert_eq!(read_json(&mut b, 4).await, Err(EgressError::ResourceLimit));
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(b"{}\0").await.unwrap();
        assert_eq!(read_json(&mut b, 2).await.unwrap(), b"{}");
    }
    #[tokio::test]
    async fn broker_tcp_queries_both_families_and_keeps_private_answers_for_policy() {
        let upstream=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=upstream.local_addr().unwrap();
        let dns=tokio::spawn(async move {
            for kind in [1,28] {
                let (mut tcp,_)=upstream.accept().await.unwrap();
                let length=tcp.read_u16().await.unwrap();
                let mut query=vec![0;length as usize];tcp.read_exact(&mut query).await.unwrap();
                assert_eq!(query,dns_wire::query("alias.example",kind).unwrap());
                let mut response=query;
                response[2]=0x81;response[3]=0x80;response[7]=1;
                response.extend_from_slice(&[0xc0,12]);
                response.extend_from_slice(&kind.to_be_bytes());
                response.extend_from_slice(&[0,1,0,0,0,60]);
                let bytes=if kind==1 {vec![8,8,8,8]}else {"fd00:ec2::254".parse::<Ipv6Addr>().unwrap().octets().to_vec()};
                response.extend_from_slice(&(bytes.len() as u16).to_be_bytes());response.extend(bytes);
                tcp.write_u16(response.len() as u16).await.unwrap();tcp.write_all(&response).await.unwrap();
            }
        });
        let (mut client,server)=UnixStream::pair().unwrap();
        let broker=tokio::spawn(broker_request(server,address));
        write_json(&mut client,&ResolveRequest{method:RESOLVE_METHOD.into(),parameters:ResolveParameters{
            name:Text::new("Alias.Example.".into()).unwrap(),family:0,flags:RESOLVE_FLAGS}}).await.unwrap();
        let bytes=tokio::time::timeout(Duration::from_secs(2),read_json(&mut client,MAX_DNS_RESPONSE_BYTES)).await.unwrap().unwrap();
        assert_eq!(decode_response(&bytes).unwrap(),vec!["8.8.8.8".parse::<IpAddr>().unwrap(),"fd00:ec2::254".parse::<IpAddr>().unwrap()]);
        broker.await.unwrap().unwrap();dns.await.unwrap();
    }

    #[tokio::test]
    async fn broker_rejects_arbitrary_method_route_flags_and_partial_dns() {
        for request in [
            serde_json::json!({"method":"connect","parameters":{"name":"example.com","family":0,"flags":RESOLVE_FLAGS}}),
            serde_json::json!({"method":RESOLVE_METHOD,"parameters":{"name":"example.com","family":0,"flags":RESOLVE_FLAGS,"upstream":"127.0.0.1"}}),
            serde_json::json!({"method":RESOLVE_METHOD,"parameters":{"name":"example.com","family":2,"flags":RESOLVE_FLAGS}}),
            serde_json::json!({"method":RESOLVE_METHOD,"parameters":{"name":"example.com","family":0,"flags":0}}),
        ] {
            let (mut client,server)=UnixStream::pair().unwrap();
            write_json(&mut client,&request).await.unwrap();
            assert_eq!(broker_request(server,"127.0.0.1:53".parse().unwrap()).await,Err(EgressError::InvalidRequest));
        }
        let upstream=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=upstream.local_addr().unwrap();
        let dns=tokio::spawn(async move {
            for kind in [1,28] {
                let (mut tcp,_)=upstream.accept().await.unwrap();
                let length=tcp.read_u16().await.unwrap();let mut packet=vec![0;length as usize];
                tcp.read_exact(&mut packet).await.unwrap();
                packet[2]=0x81;packet[3]=if kind==1 {0x80}else {0x82};
                if kind==1 {packet[7]=1;packet.extend_from_slice(&[0xc0,12,0,1,0,1,0,0,0,60,0,4,8,8,8,8]);}
                tcp.write_u16(packet.len() as u16).await.unwrap();tcp.write_all(&packet).await.unwrap();
            }
        });
        let (mut client,server)=UnixStream::pair().unwrap();
        write_json(&mut client,&ResolveRequest{method:RESOLVE_METHOD.into(),parameters:ResolveParameters{
            name:Text::new("example.com".into()).unwrap(),family:0,flags:RESOLVE_FLAGS}}).await.unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(2),broker_request(server,address)).await.unwrap(),Err(EgressError::Unavailable));
        dns.await.unwrap();
        assert_eq!(client.read_u8().await.unwrap_err().kind(),std::io::ErrorKind::UnexpectedEof);
    }

}
