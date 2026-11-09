//! Run only on disposable root-capable Linux acceptance VMs. No installer here.
#![cfg(target_os = "linux")]
#![allow(unsafe_code)]
use std::{net::SocketAddr, time::Duration};
use duduclaw_egress::{controller::{EgressController, RootEgressController}, resolver::{HostResolver, Resolver}, session::{read_frame, write_frame}, transport::CheckedSocket};
use duduclaw_isolation::{bounded::{List, Text}, egress::*, ids::RunId};

#[test]
#[ignore = "root Linux VM: seccomp is installed only inside a separate test process"]
fn iso_root_seccomp_real_syscalls() {
    assert_eq!(nix::unistd::geteuid().as_raw(),0);
    if std::env::var_os("ISO_C_SECCOMP_CHILD").is_none() {
        let status=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored","--exact","iso_root_seccomp_real_syscalls","--nocapture"])
            .env("ISO_C_SECCOMP_CHILD","1").status().unwrap();
        assert!(status.success(),"seccomp subprocess failed: {status}");
        return;
    }
    // Construct runtime and descriptors before installing the daemon's TSYNC filter.
    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (mut a,mut b)=std::os::unix::net::UnixStream::pair().unwrap();
    assert_eq!(unsafe {libc::prctl(libc::PR_SET_NO_NEW_PRIVS,1,0,0,0)},0);
    duduclaw_egress::seccomp::install().unwrap();
    for family in [libc::AF_VSOCK,libc::AF_NETLINK,libc::AF_PACKET] {
        assert_eq!(unsafe {libc::socket(family,libc::SOCK_STREAM,0)},-1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(libc::EPERM));
    }
    assert_eq!(unsafe {libc::unshare(libc::CLONE_NEWUSER)},-1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(libc::EPERM));
    assert!(std::os::unix::net::UnixStream::pair().is_err());
    assert!(std::fs::OpenOptions::new().write(true).open("/dev/null").is_err());
    assert!(std::fs::File::open("/dev/null").is_ok());
    std::io::Write::write_all(&mut a,b"filtered").unwrap();
    let mut bytes=[0;8]; std::io::Read::read_exact(&mut b,&mut bytes).unwrap();
    assert_eq!(&bytes,b"filtered");
    runtime.block_on(async {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        let (client,server)=tokio::join!(tokio::net::TcpStream::connect(address),listener.accept());
        assert!(client.is_ok()); assert!(server.is_ok());
        tokio::time::sleep(Duration::from_millis(1)).await;
    });
}

#[tokio::test]
#[ignore = "root VM with systemd-resolved Varlink and ISO_EGRESS_DNS_NAME public DNS fixture"]
async fn iso_root_resolved_varlink() {
    assert_eq!(nix::unistd::geteuid().as_raw(),0);
    let name=std::env::var("ISO_EGRESS_DNS_NAME").expect("public DNS name (use a CNAME fixture too)");
    let resolver=HostResolver::new();
    resolver.probe(DnsRoute::ResolvedVarlink).await.unwrap();
    let addresses=resolver.resolve(&name,DnsRoute::ResolvedVarlink).await.unwrap();
    assert!(!addresses.is_empty());
    assert!(addresses.into_iter().all(duduclaw_egress::policy::is_public_ip));
}

#[test]
#[ignore = "root VM with installed dedicated broker and ISO_EGRESS_DNS_NAME public DNS fixture"]
fn iso_root_dedicated_dns_broker() {
    if std::env::var_os("ISO_C_BROKER_CHILD").is_none() {
        use std::os::unix::process::CommandExt;
        assert_eq!(nix::unistd::geteuid().as_raw(),0);
        let user=nix::unistd::User::from_name("duduclaw-egress").unwrap().expect("install service account");
        let mut child=std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--ignored","--exact","iso_root_dedicated_dns_broker","--nocapture"])
            .env("ISO_C_BROKER_CHILD","1");
        // Order matters: supplementary groups must be cleared before setuid.
        let (uid,gid)=(user.uid,user.gid);
        unsafe {child.pre_exec(move || {
            nix::unistd::setgroups(&[])?;
            nix::unistd::setgid(gid)?;
            nix::unistd::setuid(uid)?;
            Ok(())
        });}
        assert!(child.status().unwrap().success());
        return;
    }
    assert_ne!(nix::unistd::geteuid().as_raw(),0);
    assert!(nix::unistd::getgroups().unwrap().is_empty());
    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let name=std::env::var("ISO_EGRESS_DNS_NAME").expect("public DNS fixture name");
        let resolver=HostResolver::new();
        resolver.probe(DnsRoute::DedicatedDnsBroker).await.unwrap();
        let addresses=resolver.resolve(&name,DnsRoute::DedicatedDnsBroker).await.unwrap();
        assert!(!addresses.is_empty());
        assert!(addresses.into_iter().all(duduclaw_egress::policy::is_public_ip));
    });
}

#[tokio::test]
#[ignore = "disposable root VM with installed egress units/config, no spawnd controller"]
async fn iso_root_installed_daemon_registration_and_revocation() {
    use duduclaw_egress::daemon::{Config, CONFIG_PATH, read_root_config};
    assert_eq!(nix::unistd::geteuid().as_raw(),0);
    let config:Config=read_root_config(CONFIG_PATH).unwrap(); config.validate().unwrap();
    let p=&config.profiles.as_slice()[0];
    assert!(config.run_caps.total_connections>=9,"fixture requires at least 9 attempts");
    assert!(p.limits.frames_per_second.get()>=32,"fixture requires at least 32 frames/s");
    let mut control=RootEgressController::connect().unwrap();
    let run=RunId([0xc3;16]);
    let fd=control.register(run,p.id).await.unwrap();
    let mut data=CheckedSocket::new(fd).unwrap();
    // No network packet is sent for any of these forbidden destinations.
    for (index,host) in ["127.0.0.1","169.254.169.254","10.1.2.3","100.64.0.1","fd00:ec2::254","::ffff:8.8.8.8","64:ff9b::808:808","2002:808:808::1","2001::1"].into_iter().enumerate() {
        write_frame(&mut data,EgressMessage::Connect{tunnel_id:TunnelId(index as u32+1),target:ConnectTarget{
            host:Text::new(host.into()).unwrap(),port:p.allowed_connect_ports.as_slice()[0]}},&p.limits).await.unwrap();
        assert!(matches!(tokio::time::timeout(Duration::from_secs(5),read_frame(&mut data,&p.limits)).await.unwrap().unwrap(),EgressMessage::Refused{code:EgressError::DestinationDenied,..}));
    }
    control.unregister(run).await.unwrap();
    let mut byte=[0];
    assert_eq!(nix::sys::socket::recv(data.raw_fd(),&mut byte,nix::sys::socket::MsgFlags::MSG_DONTWAIT).unwrap(),0);
    assert!(matches!(control.register(run,p.id).await,Err(EgressError::ResourceLimit)));
    let fd=control.register(RunId([0xc4;16]),p.id).await.unwrap();
    let data=CheckedSocket::new(fd).unwrap();
    drop(control);
    // Control disconnect must revoke even a still-open runtime descriptor.
    let mut bytes=[0;1];
    assert_eq!(tokio::time::timeout(Duration::from_secs(5),data.receive(&mut bytes)).await.unwrap().unwrap().0,0);
}

#[tokio::test]
#[ignore = "root VM and ISO_EGRESS_ECHO_ADDR operator-controlled public TCP echo server"]
async fn iso_root_concurrent_live_tunnels_and_byte_cap() {
    use std::{collections::BTreeMap,sync::Arc};
    use duduclaw_egress::{budget::{default_limits,RunCaps},session::serve_run};
    assert_eq!(nix::unistd::geteuid().as_raw(),0);
    let address:SocketAddr=std::env::var("ISO_EGRESS_ECHO_ADDR").expect("public echo endpoint").parse().unwrap();
    assert!(duduclaw_egress::policy::is_public_ip(address.ip()));
    let p=EgressProfile{id:ProfileId(duduclaw_isolation::ids::Sha256Digest([2;32])),allowed_connect_ports:List::new(vec![address.port().try_into().unwrap()]).unwrap(),dns_route:DnsRoute::ResolvedVarlink,limits:default_limits()};
    let limits=p.limits.clone();
    let (a,b)=std::os::unix::net::UnixStream::pair().unwrap();
    let mut client=CheckedSocket::new(a.into()).unwrap();
    let task=tokio::spawn(serve_run(CheckedSocket::new(b.into()).unwrap(),RunId([1;16]),p,RunCaps{total_bytes:1024,total_connections:2},Arc::new(HostResolver::new())));
    tokio::time::timeout(Duration::from_secs(20),async {
        for id in 1..=2 {
            write_frame(&mut client,EgressMessage::Connect{tunnel_id:TunnelId(id),target:ConnectTarget{host:Text::new(address.ip().to_string()).unwrap(),port:address.port().try_into().unwrap()}},&limits).await.unwrap();
        }
        let mut connected=std::collections::BTreeSet::new();
        for _ in 0..2 {
            match read_frame(&mut client,&limits).await.unwrap() {EgressMessage::Connected{tunnel_id}=>{assert!(connected.insert(tunnel_id.0));},other=>panic!("unexpected {other:?}")}
        }
        let mut totals=BTreeMap::from([(1,0),(2,0)]);
        for id in 1..=2 {
            write_frame(&mut client,EgressMessage::Data{tunnel_id:TunnelId(id),bytes:List::new(vec![id as u8;128]).unwrap()},&limits).await.unwrap();
        }
        while totals.values().any(|n| *n<128) {
            match read_frame(&mut client,&limits).await.unwrap() {
                EgressMessage::Data{tunnel_id,bytes}=>{assert!(bytes.as_slice().iter().all(|b| *b==tunnel_id.0 as u8));*totals.get_mut(&tunnel_id.0).unwrap()+=bytes.as_slice().len();},
                other=>panic!("unexpected {other:?}")
            }
        }
        assert!(totals.values().all(|n| *n==128));
        // 512 bytes charged across both directions; the next 513 must not land.
        write_frame(&mut client,EgressMessage::Data{tunnel_id:TunnelId(1),bytes:List::new(vec![9;513]).unwrap()},&limits).await.unwrap();
        let audit=task.await.unwrap();
        assert_eq!(audit.bytes,512);assert_eq!(audit.connection_attempts,2);
        assert_eq!(audit.terminal_code,EgressError::ResourceLimit);
        assert!(read_frame(&mut client,&limits).await.is_err());
    }).await.unwrap();
}
