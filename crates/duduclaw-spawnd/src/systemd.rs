//! Root-selected systemd policy and fd-only launch transport (design §4.2–§4.4).
use std::{collections::BTreeMap, io};
use duduclaw_isolation::{execution::{ResourceLimits, RunTimeouts}, ids::{EmployeeId, RunId}};

pub const TRAMPOLINE: &str = "/usr/lib/duduclaw/duduclaw-run";
pub const SPAWND: &str = "/usr/lib/duduclaw/duduclaw-spawnd";
// CAP_CHOWN is required for per-run devpts (R7-M1). No CAP_FOWNER is granted.
pub const TRAMPOLINE_CAPABILITIES: u64 = (1 << 0) | (1 << 6) | (1 << 7)
    | (1 << 8) | (1 << 12) | (1 << 21);
pub const TRAMPOLINE_NAMESPACES: u64 = 0x0002_0000 | 0x0800_0000 | 0x0400_0000
    | 0x4000_0000 | 0x2000_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Property {
    Bool(bool), U32(u32), U64(u64), Text(String), Strings(Vec<String>),
    AllowStrings(Vec<String>), DeviceLimits(Vec<(String, u64)>),
    DeviceAccess(Vec<(String, String)>),
}

/// Only trusted root policy can construct this; none of its fields come from IPC.
#[derive(Debug, Clone)]
pub struct UnitPolicy {
    pub name: String,
    pub properties: BTreeMap<&'static str, Property>,
}

fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid unit policy") }
pub fn run_unit(run: RunId) -> String {
    let hex: String = run.0.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("duduclaw-run-{hex}.service")
}
/// Exact namespace parser used only for host-side restart reconciliation.
pub fn parse_run_unit(name: &str) -> Option<RunId> {
    let hex = name.strip_prefix("duduclaw-run-")?.strip_suffix(".service")?;
    if hex.len() != 32 { return None; }
    let nibble = |byte| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let mut bytes = [0u8; 16];
    for (output, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        *output = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(RunId(bytes))
}
pub fn employee_slice(employee: EmployeeId) -> String {
    let hex: String = employee.0.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("duduclaw-employees-{hex}.slice")
}

impl UnitPolicy {
    /// The IO device is a root-derived block device, never an employee path.
    pub fn new(run: RunId, employee: EmployeeId, limits: &ResourceLimits,
        timeouts: &RunTimeouts, device_major: u32, device_minor: u32) -> io::Result<Self> {
        if limits.memory_bytes == 0 || limits.cpu_quota_percent == 0 || limits.tasks == 0
            || limits.open_files < 1024 + 2 * 72 + 16 || limits.ptys == 0
            || limits.io_read_bytes_per_sec == 0 || limits.io_write_bytes_per_sec == 0
            || limits.io_read_ops_per_sec == 0 || limits.io_write_ops_per_sec == 0
            || limits.output_buffer_bytes == 0 || limits.output_total_bytes == 0
            || limits.memfd_total_bytes == 0 || limits.memfd_total_bytes > 64 * 1024 * 1024
            || timeouts.startup_ms == 0 || timeouts.runtime_ms == 0
            || timeouts.termination_grace_ms == 0 || device_major == 0 {
            return Err(invalid());
        }
        let micros = |ms: u64| ms.checked_mul(1000).ok_or_else(invalid);
        let mut properties = BTreeMap::new();
        for key in ["PrivateTmp", "PrivateDevices", "PrivateIPC", "ProtectKernelTunables",
            "ProtectKernelModules", "ProtectKernelLogs", "ProtectControlGroups",
            "RestrictSUIDSGID", "LockPersonality", "NoNewPrivileges", "MemoryAccounting",
            "CPUAccounting", "TasksAccounting", "IOAccounting"] {
            properties.insert(key, Property::Bool(true));
        }
        // HX (D8 revision §2.4 F6, §2.5): spawnd creates rt-init/rt-runtime/hx
        // leaves from outside the unit; ProtectControlGroups stays set above, so
        // nothing inside the run reaches cgroupfs by path. A helper OOM kill must
        // not stop the run (OOMPolicy=continue). All three are read back at start.
        properties.insert("Delegate", Property::Bool(true));
        for (key, value) in [("Type", "exec"), ("ProtectSystem", "strict"), ("OOMPolicy", "continue"),
            ("ProtectHome", "tmpfs"), ("KillMode", "control-group"),
            ("DevicePolicy", "closed"), ("User", "root"), ("Group", "root"),
            ("StandardInput", "null"), ("StandardOutput", "null"), ("StandardError", "null")] {
            properties.insert(key, Property::Text(value.into()));
        }
        properties.insert("Slice", Property::Text(employee_slice(employee)));
        properties.insert("UMask", Property::U32(0o077));
        for (key, value) in [("LimitCORE", 0), ("LimitCORESoft", 0),
            ("CapabilityBoundingSet", TRAMPOLINE_CAPABILITIES),
            ("AmbientCapabilities", 0), ("RestrictNamespaces", TRAMPOLINE_NAMESPACES),
            ("LimitNOFILE", u64::from(limits.open_files)),
            ("LimitNOFILESoft", u64::from(limits.open_files)),
            ("MemoryMax", limits.memory_bytes), ("MemorySwapMax", 0),
            ("CPUQuotaPerSecUSec", u64::from(limits.cpu_quota_percent) * 10_000),
            ("TasksMax", u64::from(limits.tasks)),
            ("RuntimeMaxUSec", micros(timeouts.runtime_ms)?),
            ("TimeoutStartUSec", micros(timeouts.startup_ms)?),
            ("TimeoutStopUSec", micros(timeouts.termination_grace_ms)?)] {
            properties.insert(key, Property::U64(value));
        }
        properties.insert("SystemCallArchitectures", Property::Strings(vec!["native".into()]));
        properties.insert("RestrictAddressFamilies", Property::AllowStrings(
            ["AF_UNIX", "AF_INET", "AF_INET6", "AF_NETLINK"].map(String::from).to_vec()));
        properties.insert("Environment", Property::Strings(Vec::new()));
        // PID 1 stops these units even if the daemon is SIGKILLed before its
        // asynchronous shutdown work can run. Startup still reconciles residue.
        properties.insert("BindsTo", Property::Strings(vec!["duduclaw-spawnd.service".into()]));
        properties.insert("After", Property::Strings(vec!["duduclaw-spawnd.service".into()]));
        properties.insert("DeviceAllow", Property::DeviceAccess(vec![
            ("char-pts".into(), "rw".into()), ("/dev/ptmx".into(), "rw".into())]));
        let device = format!("/dev/block/{device_major}:{device_minor}");
        for (key, ceiling) in [("IOReadBandwidthMax", limits.io_read_bytes_per_sec),
            ("IOWriteBandwidthMax", limits.io_write_bytes_per_sec),
            ("IOReadIOPSMax", limits.io_read_ops_per_sec),
            ("IOWriteIOPSMax", limits.io_write_ops_per_sec)] {
            properties.insert(key, Property::DeviceLimits(vec![(device.clone(), ceiling)]));
        }
        Ok(Self { name: run_unit(run), properties })
    }
    /// Every property `Systemd::start` reads back after StartTransientUnit. No
    /// property is set without being compared against PID 1's actual value.
    pub fn readback(&self) -> impl Iterator<Item = (&'static str, &Property)> {
        self.properties.iter().map(|(key, value)| (*key, value))
    }
}

/// The DNS second layer must not exempt private resolver IPs (r7 supersedes §4.3).
pub fn egress_second_layer_valid(address_allow: &[String], dns_route: &str) -> bool {
    address_allow.is_empty() && matches!(dns_route, "resolved_varlink" | "dedicated_dns_broker")
}

/// Strict cgroup v2 parser: absence/duplicates/unknown populated values deny.
pub fn cgroup_empty(events: &str) -> io::Result<bool> {
    let mut populated = None;
    for line in events.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 2 { return Err(invalid()); }
        if fields[0] == "populated" {
            if populated.is_some() { return Err(invalid()); }
            populated = Some(match fields[1] { "0" => false, "1" => true, _ => return Err(invalid()) });
        }
    }
    populated.map(|value| !value).ok_or_else(invalid)
}

// A failed D-Bus reply may still have changed PID 1's reference count. Keep
// that unit quarantined on this connection until disconnect, rather than retry
// an acquire and risk the systemd 249 duplicate-reference busy loop.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy)]
enum ReferenceState { Held, Uncertain }
#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct UnitReferences { units: BTreeMap<String, ReferenceState> }
#[cfg(any(target_os = "linux", test))]
impl UnitReferences {
    fn acquire(&mut self, name: &str, reuse: bool, operation: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        match self.units.get(name) {
            Some(ReferenceState::Held) if reuse => return Ok(()),
            Some(_) => return Err(invalid()),
            None => {},
        }
        self.units.insert(name.into(), ReferenceState::Uncertain);
        operation()?;
        self.units.insert(name.into(), ReferenceState::Held);
        Ok(())
    }
    fn release(&mut self, name: &str, operation: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        match self.units.get(name) {
            None => return Ok(()),
            Some(ReferenceState::Uncertain) => return Err(invalid()),
            Some(ReferenceState::Held) => {},
        }
        self.units.insert(name.into(), ReferenceState::Uncertain);
        operation()?;
        self.units.remove(name);
        Ok(())
    }
}
#[cfg(any(target_os = "linux", test))]
fn cleanup_cgroup_empty(events: io::Result<String>, inactive: bool) -> io::Result<bool> {
    match events {
        Ok(events) => cgroup_empty(&events),
        // During deactivation the kernel may remove the cgroup before PID 1
        // publishes inactive. Retry; absence alone is not cleanup proof.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(inactive),
        Err(error) => Err(error),
    }
}

/// Non-initial devpts instances share this budget; retain ten percent headroom.
pub fn pty_budget(maximum: &str, reserved: &str) -> io::Result<u32> {
    let parse = |text: &str| -> io::Result<u32> {
        let text = text.trim();
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) { return Err(invalid()); }
        text.parse().map_err(|_| invalid())
    };
    let available = parse(maximum)?.checked_sub(parse(reserved)?).ok_or_else(invalid)?;
    let budget = u64::from(available) * 9 / 10;
    if budget == 0 { return Err(invalid()); }
    Ok(budget as u32)
}

#[cfg(target_os = "linux")]
pub mod linux {
    use super::*;
    use std::{os::fd::BorrowedFd, path::{Component, Path, PathBuf}, sync::Mutex, time::{Duration, Instant}};
    use zbus::{blocking::{connection::Builder, Connection, Proxy}, zvariant::{Fd, OwnedObjectPath, OwnedValue, Value}};
    const BUS: &str = "org.freedesktop.systemd1";
    const MANAGER: &str = "org.freedesktop.systemd1.Manager";
    fn bus_error(_: impl std::fmt::Display) -> io::Error {
        io::Error::new(io::ErrorKind::Other, "systemd operation unavailable")
    }
    fn missing_unit(error: &zbus::Error) -> bool {
        matches!(error, zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit")
    }
    fn value(property: &Property) -> Value<'_> {
        match property {
            Property::Bool(v) => Value::from(*v), Property::U32(v) => Value::from(*v),
            Property::U64(v) => Value::from(*v), Property::Text(v) => Value::from(v.as_str()),
            Property::Strings(v) => Value::from(v.iter().map(String::as_str).collect::<Vec<_>>()),
            Property::AllowStrings(v) => Value::from((true, v.iter().map(String::as_str).collect::<Vec<_>>())),
            Property::DeviceLimits(v) => Value::from(v.iter().map(|(path, limit)| (path.as_str(), *limit)).collect::<Vec<_>>()),
            Property::DeviceAccess(v) => Value::from(v.iter().map(|(path, access)| (path.as_str(), access.as_str())).collect::<Vec<_>>()),
        }
    }
    pub struct Systemd { connection: Connection, references: Mutex<UnitReferences> }
    /// A host-only receipt; no cgroup paths or PIDs go back over gateway IPC.
    pub struct StartedUnit { pub name: String, path: OwnedObjectPath, pub cgroup: PathBuf }
    impl Systemd {
        pub fn connect() -> io::Result<Self> {
            let connection = Builder::system().map_err(bus_error)?
                .method_timeout(Duration::from_secs(5)).build().map_err(bus_error)?;
            Ok(Self { connection, references: Mutex::new(UnitReferences::default()) })
        }
        fn manager(&self) -> io::Result<Proxy<'_>> {
            Proxy::new(&self.connection, BUS, "/org/freedesktop/systemd1", MANAGER).map_err(bus_error)
        }
        fn start_referenced(&self, name: &str, mut properties: Vec<(&str, Value<'_>)>) -> io::Result<()> {
            if properties.iter().any(|(key, _)| *key == "AddRef") { return Err(invalid()); }
            // AddRef is atomic with creation: even an immediately exited unit
            // survives until readback. All reference RPCs share this lock.
            properties.push(("AddRef", Value::from(true)));
            let manager = self.manager()?;
            let mut references = self.references.lock().map_err(|_| invalid())?;
            references.acquire(name, false, || {
                let aux: Vec<(&str, Vec<(&str, Value<'_>)>)> = Vec::new();
                let _: OwnedObjectPath = manager.call("StartTransientUnit",
                    &(name, "fail", properties, aux)).map_err(bus_error)?;
                Ok(())
            })
        }
        /// Enumerate only exact daemon-generated names, including inactive units.
        /// Callers must reconcile this inventory before admitting a fresh run.
        pub fn list_runs(&self) -> io::Result<Vec<RunId>> {
            type ListedUnit = (String, String, String, String, String, String,
                OwnedObjectPath, u32, String, OwnedObjectPath);
            let units: Vec<ListedUnit> = self.manager()?.call("ListUnits", &()).map_err(bus_error)?;
            let mut runs: Vec<_> = units.into_iter().filter_map(|unit| parse_run_unit(&unit.0)).collect();
            runs.sort();
            runs.dedup();
            if runs.len() > 1024 { return Err(invalid()); }
            Ok(runs)
        }
        /// Recover a receipt after a failed launch or daemon restart. Missing
        /// cgroups are only cleanup proof after an inactive readback.
        pub fn inspect_run(&self, run: RunId) -> io::Result<StartedUnit> {
            let name = run_unit(run);
            let manager = self.manager()?;
            self.references.lock().map_err(|_| invalid())?.acquire(&name, true, || {
                manager.call::<_, _, ()>("RefUnit", &(name.as_str(),)).map_err(bus_error)
            })?;
            // Failed readback is uncertain cleanup, so keep our one reference.
            (|| {
                let path: OwnedObjectPath = manager.call("GetUnit", &(name.as_str(),)).map_err(bus_error)?;
                let unit = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
                let actual_name: String = unit.get_property("Id").map_err(bus_error)?;
                if actual_name != name { return Err(invalid()); }
                let service = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
                let path_text: String = service.get_property("ControlGroup").map_err(bus_error)?;
                // Cgroup removal can precede inactive, including during restart
                // inspection. Cleanup separately waits for inactive; an empty
                // path is never used as a filesystem root.
                let cgroup = if path_text.is_empty() { PathBuf::new() }
                    else { checked_cgroup_with_missing(&path_text, true)? };
                drop(service);
                drop(unit);
                Ok(StartedUnit { name: name.clone(), path, cgroup })
            })()
        }
        /// Cancellation remains available even when no launch receipt was returned.
        pub fn kill_run(&self, run: RunId) -> io::Result<()> { self.kill_name(&run_unit(run)) }
        /// Root tests only: a run-named, delegated unit whose single process
        /// (`/usr/bin/cat` on `input`) waits like a trampoline before bootstrap.
        #[cfg(test)]
        pub(crate) fn start_test_delegated(&self, run: RunId, input: BorrowedFd<'_>, output: BorrowedFd<'_>) -> io::Result<PathBuf> {
            let name = run_unit(run);
            let properties: Vec<(&str, Value<'_>)> = vec![
                ("Type", Value::from("exec")), ("Delegate", Value::from(true)), ("ProtectControlGroups", Value::from(true)),
                ("KillMode", Value::from("control-group")), ("RuntimeMaxUSec", Value::from(30_000_000u64)),
                ("StandardInput", Value::from("null")), ("StandardOutput", Value::from("null")), ("StandardError", Value::from("null")),
                ("ExecStart", Value::from(vec![("/usr/bin/cat", vec!["/usr/bin/cat"], false)])),
                ("StandardInputFileDescriptor", Value::from(Fd::from(input))),
                ("StandardOutputFileDescriptor", Value::from(Fd::from(output))),
            ];
            self.start_referenced(&name, properties)?;
            let path: OwnedObjectPath = self.manager()?.call("GetUnit", &(name.as_str(),)).map_err(bus_error)?;
            let service = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                let text: String = service.get_property("ControlGroup").map_err(bus_error)?;
                if let Ok(path) = checked_cgroup(&text) { return Ok(path); }
                if Instant::now() >= until { return Err(bus_error("cgroup timeout")); }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        /// HX open-time-credentials probe (CVE-2021-4197 fix, see helper_cgroup).
        /// PID 1 runs `/usr/bin/printf 0` as an unprivileged dynamic user with
        /// no capabilities and `stdout` = a root-opened `cgroup.procs` fd. The write
        /// moves the probe itself; printf exits 1 when the kernel refuses it.
        /// Returns the probe's exit status; any other outcome is an error.
        pub fn run_cgroup_probe(&self, nonce: [u8; 16], stdout: BorrowedFd<'_>) -> io::Result<i32> {
            let hex: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
            let name = format!("duduclaw-cgprobe-{hex}.service");
            let properties: Vec<(&str, Value<'_>)> = vec![
                ("Type", Value::from("exec")), ("DynamicUser", Value::from(true)),
                ("NoNewPrivileges", Value::from(true)), ("CapabilityBoundingSet", Value::from(0u64)),
                ("AmbientCapabilities", Value::from(0u64)), ("PrivateNetwork", Value::from(true)),
                ("KillMode", Value::from("control-group")), ("RuntimeMaxUSec", Value::from(2_000_000u64)),
                ("StandardInput", Value::from("null")), ("StandardError", Value::from("null")),
                ("Environment", Value::from(Vec::<&str>::new())),
                ("ExecStart", Value::from(vec![("/usr/bin/printf", vec!["/usr/bin/printf", "0"], false)])),
                ("StandardOutputFileDescriptor", Value::from(Fd::from(stdout))),
            ];
            self.start_referenced(&name, properties)?;
            let manager = self.manager()?;
            let result = (|| {
                let path: OwnedObjectPath = manager.call("GetUnit", &(name.as_str(),)).map_err(bus_error)?;
                let unit = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
                let service = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
                let until = Instant::now() + Duration::from_secs(3);
                loop {
                    let state: String = unit.get_property("ActiveState").map_err(bus_error)?;
                    if state == "inactive" || state == "failed" { break; }
                    if Instant::now() >= until { return Err(bus_error("probe timeout")); }
                    std::thread::sleep(Duration::from_millis(10));
                }
                let code: i32 = service.get_property("ExecMainCode").map_err(bus_error)?;
                let status: i32 = service.get_property("ExecMainStatus").map_err(bus_error)?;
                // CLD_EXITED; a signal or a missing exec is never a probe answer.
                if code != 1 { return Err(invalid()); }
                Ok(status)
            })();
            let _ = self.kill_name(&name);
            let _: zbus::Result<()> = manager.call("ResetFailedUnit", &(name.as_str(),));
            let released = self.references.lock().map_err(|_| invalid())?.release(&name, || {
                manager.call::<_, _, ()>("UnrefUnit", &(name.as_str(),)).map_err(bus_error)
            });
            let status = result?;
            released?;
            Ok(status)
        }
        /// Descriptors must be validated by B1 before this call. Runtime release
        /// remains blocked on the bootstrap handshake and B2 status readback.
        pub fn start(&self, policy: &UnitPolicy, bootstrap: BorrowedFd<'_>,
            stdout: BorrowedFd<'_>, stderr: BorrowedFd<'_>) -> io::Result<StartedUnit> {
            if parse_run_unit(&policy.name).is_none() { return Err(invalid()); }
            self.ensure_employee_slice(policy)?;
            let mut properties: Vec<(&str, Value<'_>)> = policy.properties.iter()
                .map(|(key, property)| (*key, value(property))).collect();
            properties.push(("ExecStart", Value::from(vec![(TRAMPOLINE, vec![TRAMPOLINE], false)])));
            properties.push(("StandardInputFileDescriptor", Value::from(Fd::from(bootstrap))));
            properties.push(("StandardOutputFileDescriptor", Value::from(Fd::from(stdout))));
            properties.push(("StandardErrorFileDescriptor", Value::from(Fd::from(stderr))));
            self.start_referenced(&policy.name, properties)?;
            let manager = self.manager()?;
            let result = (|| {
                let path: OwnedObjectPath = manager.call("GetUnit", &(policy.name.as_str(),)).map_err(bus_error)?;
                let service = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
                let unit_proxy = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
                for (key, expected) in policy.readback() {
                    let actual: OwnedValue = if matches!(key, "BindsTo" | "After") {
                        let actual: Vec<String> = unit_proxy.get_property(key).map_err(bus_error)?;
                        // systemd adds implicit ordering dependencies as well.
                        if !actual.iter().any(|name| name == "duduclaw-spawnd.service") { return Err(invalid()); }
                        continue;
                    } else { service.get_property(key).map_err(bus_error)? };
                    if !property_matches(expected, actual)? { return Err(invalid()); }
                }
                // Standard*FileDescriptor overrides the null defaults; systemd
                // 249 does not expose these received descriptors as properties.
                // B2 must verify bootstrap/pipe identity before releasing a run.
                let until = std::time::Instant::now() + Duration::from_secs(5);
                let cgroup = loop {
                    let path_text: String = service.get_property("ControlGroup").map_err(bus_error)?;
                    if let Ok(path) = checked_cgroup(&path_text) { break path; }
                    if std::time::Instant::now() >= until { return Err(bus_error("cgroup timeout")); }
                    std::thread::sleep(Duration::from_millis(25));
                };
                verify_run_resources(&cgroup, policy)?;
                drop(service);
                drop(unit_proxy);
                Ok(StartedUnit { name: policy.name.clone(), path, cgroup })
            })();
            if result.is_err() { let _ = self.kill_name(&policy.name); }
            result
        }
        fn ensure_employee_slice(&self, policy: &UnitPolicy) -> io::Result<()> {
            let Some(Property::Text(name)) = policy.properties.get("Slice") else { return Err(invalid()); };
            let properties: Vec<(&str, Value<'_>)> = ["MemoryAccounting", "CPUAccounting",
                "TasksAccounting", "IOAccounting", "MemoryMax", "MemorySwapMax",
                "CPUQuotaPerSecUSec", "TasksMax", "IOReadBandwidthMax", "IOWriteBandwidthMax",
                "IOReadIOPSMax", "IOWriteIOPSMax"].into_iter().map(|key| {
                    policy.properties.get(key).map(|property| (key, value(property))).ok_or_else(invalid)
                }).collect::<io::Result<_>>()?;
            let manager = self.manager()?;
            let existing: zbus::Result<OwnedObjectPath> = manager.call("GetUnit", &(name.as_str(),));
            let path = match existing {
                Ok(path) => path,
                Err(error) if missing_unit(&error) => {
                    let aux: Vec<(&str, Vec<(&str, Value<'_>)>)> = Vec::new();
                    let _: OwnedObjectPath = manager.call("StartTransientUnit",
                        &(name.as_str(), "fail", &properties, aux)).map_err(bus_error)?;
                    manager.call("GetUnit", &(name.as_str(),)).map_err(bus_error)?
                }
                Err(error) => return Err(bus_error(error)),
            };
            let slice = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Slice").map_err(bus_error)?;
            for (key, expected) in properties {
                let actual: OwnedValue = slice.get_property(key).map_err(bus_error)?;
                if actual != expected.try_to_owned().map_err(bus_error)? { return Err(invalid()); }
            }
            Ok(())
        }
        fn kill_name(&self, name: &str) -> io::Result<()> {
            let manager = self.manager()?;
            let path: OwnedObjectPath = match manager.call("GetUnit", &(name,)) {
                Ok(path) => path,
                Err(error) if missing_unit(&error) => return Ok(()),
                Err(error) => return Err(bus_error(error)),
            };
            let unit = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
            let state: String = unit.get_property("ActiveState").map_err(bus_error)?;
            if state == "inactive" { return Ok(()); }
            // Kill all descendants; never use a process id supplied by a peer.
            let _killed: zbus::Result<()> = manager.call("KillUnit", &(name, "all", 9i32));
            // A partially started unit may have no PID yet. Always stop its job,
            // even if KillUnit failed; cleanup readbacks still determine release.
            let stopped: zbus::Result<OwnedObjectPath> = manager.call("StopUnit", &(name, "replace"));
            stopped.map_err(bus_error)?;
            // KillUnit legitimately reports NoSuchProcess for half-started and
            // failed jobs. Stop completion is not cleanup proof; cleanup below
            // still requires inactive and populated=0.
            Ok(())
        }
        pub fn kill(&self, run: &StartedUnit) -> io::Result<()> { self.kill_name(&run.name) }
        /// Call only after all three cleanup readbacks, including the UID scan.
        pub fn release_reference(&self, run: &StartedUnit) -> io::Result<()> {
            let manager = self.manager()?;
            self.references.lock().map_err(|_| invalid())?.release(&run.name, || {
                manager.call::<_, _, ()>("UnrefUnit", &(run.name.as_str(),)).map_err(bus_error)
            })
        }
        /// Caller combines this with a fresh root /proc UID scan before admission.
        pub fn cleanup(&self, run: &StartedUnit) -> io::Result<(bool, bool)> {
            let unit = Proxy::new(&self.connection, BUS, run.path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
            let mut state: String = unit.get_property("ActiveState").map_err(bus_error)?;
            if state == "failed" {
                let _: () = self.manager()?.call("ResetFailedUnit", &(run.name.as_str(),)).map_err(bus_error)?;
                state = unit.get_property("ActiveState").map_err(bus_error)?;
            }
            let inactive = state == "inactive";
            if run.cgroup.as_os_str().is_empty() {
                let service = Proxy::new(&self.connection, BUS, run.path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
                let current: String = service.get_property("ControlGroup").map_err(bus_error)?;
                return Ok((inactive, inactive && current.is_empty()));
            }
            let empty = cleanup_cgroup_empty(
                std::fs::read_to_string(run.cgroup.join("cgroup.events")), inactive)?;
            Ok((inactive, empty))
        }

        /// Startup only, under RegistryStore's process-global lock. No queued
        /// request survives restart. The caller must revoke C/H attachments and
        /// scan every permanent UID before publishing startup_reconciled.
        pub fn reconcile_all(&self) -> io::Result<usize> {
            let runs = self.list_runs()?;
            let until = Instant::now() + Duration::from_secs(60);
            for id in &runs {
                if Instant::now() >= until { return Err(io::ErrorKind::TimedOut.into()); }
                let run = self.inspect_run(*id)?;
                let result: io::Result<()> = (|| {
                    self.kill(&run)?;
                    loop {
                        if self.cleanup(&run)? == (true, true) { return Ok(()); }
                        if Instant::now() >= until { return Err(io::ErrorKind::TimedOut.into()); }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                })();
                // Preserve the reference after an uncertain cleanup. A new
                // process can recover it; dropping this connection releases it.
                result?;
                self.release_reference(&run)?;
            }
            Ok(runs.len())
        }

        /// Read actual cgroup v2 files as well as systemd's configured values.
        /// A configured MemoryMin without its ancestor reservation is not ready.
        pub fn resources_ready(&self) -> io::Result<u32> {
            let manager = self.manager()?;
            let mut tcb_min = 0;
            let mut tcb_directory = PathBuf::new();
            let mut parent_directory = PathBuf::new();
            for name in ["duduclaw-tcb.slice", "duduclaw.slice"] {
                let path: OwnedObjectPath = manager.call("GetUnit", &(name,)).map_err(bus_error)?;
                let slice = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Slice").map_err(bus_error)?;
                let minimum: u64 = slice.get_property("MemoryMin").map_err(bus_error)?;
                let cgroup: String = slice.get_property("ControlGroup").map_err(bus_error)?;
                let directory = checked_cgroup(&cgroup)?;
                if minimum == 0 || minimum == u64::MAX || read_number(&directory.join("memory.min"))? != minimum {
                    return Err(invalid());
                }
                if name == "duduclaw-tcb.slice" { tcb_min = minimum; tcb_directory = directory; }
                else {
                    if minimum < tcb_min || tcb_directory.parent() != Some(directory.as_path()) { return Err(invalid()); }
                    parent_directory = directory;
                }
            }
            let path: OwnedObjectPath = manager.call("GetUnit", &("duduclaw-employees.slice",)).map_err(bus_error)?;
            let slice = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Slice").map_err(bus_error)?;
            let cgroup: String = slice.get_property("ControlGroup").map_err(bus_error)?;
            let directory = checked_cgroup(&cgroup)?;
            if directory.parent() != Some(parent_directory.as_path()) { return Err(invalid()); }
            for (property, file) in [("MemoryMax", "memory.max"), ("TasksMax", "pids.max")] {
                let limit: u64 = slice.get_property(property).map_err(bus_error)?;
                if limit == 0 || limit == u64::MAX || read_number(&directory.join(file))? != limit { return Err(invalid()); }
            }
            if read_number(&directory.join("memory.swap.max"))? != 0 { return Err(invalid()); }
            let cpu: u64 = slice.get_property("CPUQuotaPerSecUSec").map_err(bus_error)?;
            let actual = std::fs::read_to_string(directory.join("cpu.max"))?;
            let words: Vec<_> = actual.split_whitespace().collect();
            if words.len() != 2 || cpu == 0 || cpu == u64::MAX { return Err(invalid()); }
            let quota: u64 = words[0].parse().map_err(|_| invalid())?;
            let period: u64 = words[1].parse().map_err(|_| invalid())?;
            if quota == 0 || period == 0 || u128::from(quota) * 1_000_000 > u128::from(cpu) * u128::from(period) {
                return Err(invalid());
            }
            for name in ["duduclaw-gateway.service", "duduclaw-spawnd.service",
                "duduclaw-updated.service", "duduclaw-egress.service"] {
                let path: OwnedObjectPath = manager.call("LoadUnit", &(name,)).map_err(bus_error)?;
                let unit = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
                let loaded: String = unit.get_property("LoadState").map_err(bus_error)?;
                let service = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
                let actual: String = service.get_property("Slice").map_err(bus_error)?;
                if loaded != "loaded" || actual != "duduclaw-tcb.slice" { return Err(invalid()); }
                let state: String = unit.get_property("ActiveState").map_err(bus_error)?;
                if state == "active" {
                    let cgroup: String = service.get_property("ControlGroup").map_err(bus_error)?;
                    if checked_cgroup(&cgroup)?.parent() != Some(tcb_directory.as_path()) { return Err(invalid()); }
                }
            }
            host_pty_budget()
        }
    }
    pub fn host_pty_budget() -> io::Result<u32> {
        pty_budget(&std::fs::read_to_string("/proc/sys/kernel/pty/max")?,
            &std::fs::read_to_string("/proc/sys/kernel/pty/reserve")?)
    }
    fn read_number(path: &Path) -> io::Result<u64> {
        let text = std::fs::read_to_string(path)?;
        let text = text.trim();
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) { return Err(invalid()); }
        text.parse().map_err(|_| invalid())
    }
    fn verify_run_resources(directory: &Path, policy: &UnitPolicy) -> io::Result<()> {
        for (property, file) in [("MemoryMax", "memory.max"), ("MemorySwapMax", "memory.swap.max"),
            ("TasksMax", "pids.max")] {
            let Some(Property::U64(expected)) = policy.properties.get(property) else { return Err(invalid()); };
            let actual = read_number(&directory.join(file))?;
            // Kernel page rounding may narrow a byte ceiling but never widen it.
            if actual > *expected || (*expected != 0 && actual == 0) { return Err(invalid()); }
        }
        let Some(Property::U64(expected_cpu)) = policy.properties.get("CPUQuotaPerSecUSec") else { return Err(invalid()); };
        let text = std::fs::read_to_string(directory.join("cpu.max"))?;
        let words: Vec<_> = text.split_whitespace().collect();
        if words.len() != 2 { return Err(invalid()); }
        let quota: u64 = words[0].parse().map_err(|_| invalid())?;
        let period: u64 = words[1].parse().map_err(|_| invalid())?;
        if period == 0 || quota == 0 || u128::from(quota) * 1_000_000 > u128::from(*expected_cpu) * u128::from(period) {
            return Err(invalid());
        }
        let text = std::fs::read_to_string(directory.join("io.max"))?;
        for (property, field) in [("IOReadBandwidthMax", "rbps"), ("IOWriteBandwidthMax", "wbps"),
            ("IOReadIOPSMax", "riops"), ("IOWriteIOPSMax", "wiops")] {
            let Some(Property::DeviceLimits(limits)) = policy.properties.get(property) else { return Err(invalid()); };
            if limits.len() != 1 { return Err(invalid()); }
            let device = limits[0].0.strip_prefix("/dev/block/").ok_or_else(invalid)?;
            let matching: Vec<_> = text.lines().filter(|line| line.split_whitespace().next() == Some(device)).collect();
            if matching.len() != 1 { return Err(invalid()); }
            let mut found = None;
            for entry in matching[0].split_whitespace().skip(1) {
                let (key, value) = entry.split_once('=').ok_or_else(invalid)?;
                if key == field {
                    if found.is_some() { return Err(invalid()); }
                    found = Some(value.parse::<u64>().map_err(|_| invalid())?);
                }
            }
            if !found.is_some_and(|value| value > 0 && value <= limits[0].1) { return Err(invalid()); }
        }
        Ok(())
    }
    fn property_matches(expected: &Property, actual: OwnedValue) -> io::Result<bool> {
        match expected {
            Property::Strings(wanted) => {
                let mut actual: Vec<String> = actual.try_into().map_err(bus_error)?;
                let mut wanted = wanted.clone();
                actual.sort(); wanted.sort();
                Ok(actual == wanted)
            }
            Property::AllowStrings(wanted) => {
                let (allow, mut actual): (bool, Vec<String>) = actual.try_into().map_err(bus_error)?;
                let mut wanted = wanted.clone();
                actual.sort(); wanted.sort();
                Ok(allow && actual == wanted)
            }
            _ => Ok(actual == value(expected).try_to_owned().map_err(bus_error)?),
        }
    }
    fn checked_cgroup(text: &str) -> io::Result<PathBuf> {
        checked_cgroup_with_missing(text, false)
    }
    fn checked_cgroup_with_missing(text: &str, allow_missing: bool) -> io::Result<PathBuf> {
        let relative = text.strip_prefix('/').ok_or_else(invalid)?;
        if relative.is_empty() || !relative.split('/').all(|part| !part.is_empty())
            || !std::path::Path::new(relative).components().all(|part| matches!(part, Component::Normal(_))) {
            return Err(invalid());
        }
        let path = PathBuf::from("/sys/fs/cgroup").join(relative);
        match std::fs::canonicalize(&path) {
            Ok(canonical) if canonical == path => {},
            Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => {
                // cgroupfs directories are kernel-managed; still reject a
                // symlink in any existing parent before accepting removal.
                for parent in path.ancestors().skip(1) {
                    match std::fs::canonicalize(parent) {
                        Ok(canonical) if canonical == parent => break,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        _ => return Err(invalid()),
                    }
                }
            }
            _ => return Err(invalid()),
        }
        Ok(path)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::fd::{AsFd, AsRawFd};
        use nix::fcntl::OFlag;

        fn test_id() -> RunId {
            let mut bytes = [0u8; 16];
            let count = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
            assert_eq!(count, bytes.len() as isize);
            RunId(bytes)
        }
        // Test-only fixed helpers exercise PID 1's descriptor transport without
        // pretending that /usr/bin/cat is an isolated employee trampoline.
        fn test_unit(systemd: &Systemd, run: RunId, executable: &'static str,
            args: Vec<&'static str>, input: BorrowedFd<'_>, output: BorrowedFd<'_>) {
            assert!(matches!(executable, "/usr/bin/cat" | "/usr/bin/sleep" | "/usr/bin/false"));
            let properties: Vec<(&str, Value<'_>)> = vec![
                ("Type", Value::from("exec")),
                ("KillMode", Value::from("control-group")),
                ("RuntimeMaxUSec", Value::from(10_000_000u64)),
                ("StandardInput", Value::from("null")),
                ("StandardOutput", Value::from("null")),
                ("StandardError", Value::from("null")),
                ("ExecStart", Value::from(vec![(executable, args, false)])),
                ("StandardInputFileDescriptor", Value::from(Fd::from(input))),
                ("StandardOutputFileDescriptor", Value::from(Fd::from(output))),
                ("StandardErrorFileDescriptor", Value::from(Fd::from(output))),
            ];
            systemd.start_referenced(&run_unit(run), properties).unwrap();
        }
        struct TestRun(RunId);
        impl Drop for TestRun {
            fn drop(&mut self) {
                if let Ok(systemd) = Systemd::connect() {
                    let _ = systemd.kill_run(self.0);
                    if let Ok(run) = systemd.inspect_run(self.0) {
                        let until = Instant::now() + Duration::from_secs(5);
                        while Instant::now() < until {
                            match systemd.cleanup(&run) {
                                Ok((true, true)) => { let _ = systemd.release_reference(&run); break; },
                                Ok(_) => std::thread::sleep(Duration::from_millis(25)),
                                Err(_) => break,
                            }
                        }
                    }
                }
            }
        }
        #[test]
        #[ignore = "iso_root_: root disposable systemd Linux VM, no active B1 daemon/runs; --test-threads=1"]
        fn iso_root_systemd_fd_delivery_and_restart_reconciliation() {
            assert_eq!(unsafe { libc::geteuid() }, 0);
            let systemd = Systemd::connect().unwrap();
            assert!(systemd.list_runs().unwrap().is_empty());
            let (input, writer) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
            let (reader, output) = nix::unistd::pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
            nix::unistd::write(&writer, b"b1-fd-delivery\n").unwrap();
            drop(writer);
            let cat = TestRun(test_id());
            test_unit(&systemd, cat.0, "/usr/bin/cat", vec!["/usr/bin/cat"], input.as_fd(), output.as_fd());
            drop(input);
            drop(output);
            let until = Instant::now() + Duration::from_secs(5);
            let mut bytes = Vec::new();
            let mut scratch = [0u8; 128];
            while Instant::now() < until && bytes.len() < b"b1-fd-delivery\n".len() {
                match nix::unistd::read(reader.as_raw_fd(), &mut scratch) {
                    Ok(size) => bytes.extend_from_slice(&scratch[..size]),
                    Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EINTR) => {},
                    Err(error) => panic!("fd read: {error}"),
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            assert_eq!(bytes, b"b1-fd-delivery\n");
            // Repeated inspection must reuse this connection's AddRef. On
            // systemd 249 an extra RefUnit here can wedge PID 1 at release.
            systemd.inspect_run(cat.0).unwrap();
            systemd.inspect_run(cat.0).unwrap();
            assert_eq!(systemd.references.lock().unwrap().units.len(), 1);
            assert_eq!(systemd.reconcile_all().unwrap(), 1);
            assert!(systemd.references.lock().unwrap().units.is_empty());
            drop(systemd);
            drop(cat);

            // A running orphan and a failed half-start are recovered from a new
            // D-Bus connection, with no in-memory launch receipt.
            for (executable, args) in [("/usr/bin/sleep", vec!["/usr/bin/sleep", "60"]),
                ("/usr/bin/false", vec!["/usr/bin/false"])] {
                let old = Systemd::connect().unwrap();
                let orphan = TestRun(test_id());
                let input = std::fs::File::open("/dev/null").unwrap();
                let output = std::fs::OpenOptions::new().write(true).open("/dev/null").unwrap();
                test_unit(&old, orphan.0, executable, args, input.as_fd(), output.as_fd());
                std::thread::sleep(Duration::from_millis(100));
                drop(old);
                let new = Systemd::connect().unwrap();
                assert_eq!(new.list_runs().unwrap(), vec![orphan.0]);
                assert_eq!(new.reconcile_all().unwrap(), 1);
                // Missing unit cancellation is idempotent after collection.
                new.kill_run(orphan.0).unwrap();
                drop(new);
                drop(orphan);
            }
        }

        #[test]
        #[ignore = "iso_root_: root Linux VM with protected TCB slice/service templates installed and active"]
        fn iso_root_systemd_resource_hierarchy_readback() {
            assert_eq!(unsafe { libc::geteuid() }, 0);
            let systemd = Systemd::connect().unwrap();
            assert_eq!(systemd.resources_ready().unwrap(), host_pty_budget().unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::execution::DiskQuota;
    fn limits() -> ResourceLimits {
        ResourceLimits { memory_bytes: 1024 * 1024 * 1024, cpu_quota_percent: 100,
            tasks: 128, open_files: 4096, ptys: 16,
            disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 100 },
            io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024,
            io_read_ops_per_sec: 100, io_write_ops_per_sec: 100,
            output_buffer_bytes: 65536, output_total_bytes: 1024 * 1024,
            memfd_total_bytes: 64 * 1024 * 1024 }
    }
    fn timeouts() -> RunTimeouts {
        RunTimeouts { queue_ms: 30000, startup_ms: 5000, runtime_ms: 60000,
            pty_idle_ms: Some(30000), termination_grace_ms: 5000 }
    }
    #[test]
    fn run_unit_parser_accepts_only_exact_canonical_names() {
        for bytes in [[0; 16], [255; 16], [0x12; 16], [0xab; 16]] {
            let run = RunId(bytes);
            assert_eq!(parse_run_unit(&run_unit(run)), Some(run));
        }
        for name in ["", "duduclaw-run-.service",
            "duduclaw-run-0000000000000000000000000000000.service",
            "duduclaw-run-000000000000000000000000000000000.service",
            "duduclaw-run-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA.service",
            "duduclaw-run-gggggggggggggggggggggggggggggggg.service",
            "duduclaw-run-員工員工員工員工.service",
            "x-duduclaw-run-00000000000000000000000000000000.service",
            "duduclaw-run-00000000000000000000000000000000.service.extra",
            "duduclaw-run-00000000000000000000000000000000.scope",
            "duduclaw-run-00000000000000000000000000000000.service\n"] {
            assert_eq!(parse_run_unit(name), None, "{name:?}");
        }
    }
    #[test]
    fn fixed_unit_policy_includes_r7_chown_and_resource_hard_limits() {
        let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &limits(), &timeouts(), 8, 1).unwrap();
        assert_eq!(policy.properties["CapabilityBoundingSet"], Property::U64(TRAMPOLINE_CAPABILITIES));
        assert_eq!(TRAMPOLINE_CAPABILITIES & 1, 1);
        assert_eq!(TRAMPOLINE_CAPABILITIES & (1 << 3), 0);
        assert_eq!(TRAMPOLINE_NAMESPACES & 0x1000_0000, 0);
        assert_eq!(policy.properties["RuntimeMaxUSec"], Property::U64(60_000_000));
        assert_eq!(policy.properties["KillMode"], Property::Text("control-group".into()));
        assert_eq!(policy.properties["Environment"], Property::Strings(vec![]));
        assert_eq!(policy.properties["StandardOutput"], Property::Text("null".into()));
    }
    #[test]
    fn unit_policy_sets_oom_continue_delegate_protect_cgroups_and_reads_back() {
        let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &limits(), &timeouts(), 8, 1).unwrap();
        assert_eq!(policy.properties["OOMPolicy"], Property::Text("continue".into()));
        assert_eq!(policy.properties["Delegate"], Property::Bool(true));
        // Delegation never opens cgroupfs to the run: spawnd writes leaves from outside.
        assert_eq!(policy.properties["ProtectControlGroups"], Property::Bool(true));
        let readback: Vec<_> = policy.readback().collect();
        for key in ["OOMPolicy", "Delegate", "ProtectControlGroups"] {
            assert_eq!(readback.iter().filter(|(name, _)| *name == key).count(), 1, "{key}");
        }
        assert_eq!(readback.len(), policy.properties.len());
    }
    #[test]
    fn invalid_resources_never_become_unlimited() {
        let mut resource = limits();
        resource.open_files = 1183;
        assert!(UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &resource, &timeouts(), 8, 1).is_err());
        resource = limits(); resource.io_read_ops_per_sec = 0;
        assert!(UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &resource, &timeouts(), 8, 1).is_err());
        let mut deadlines = timeouts(); deadlines.runtime_ms = u64::MAX;
        assert!(UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &limits(), &deadlines, 8, 1).is_err());
    }
    #[test]
    fn cgroup_readback_is_exact_and_fail_closed() {
        assert_eq!(cgroup_empty("populated 0\nfrozen 0\n").unwrap(), true);
        assert_eq!(cgroup_empty("populated 1\n").unwrap(), false);
        for text in ["", "populated 00", "populated 0\npopulated 0", "populated 0 extra"] {
            assert!(cgroup_empty(text).is_err());
        }
    }
    #[test]
    fn references_are_single_per_connection_and_release_is_exact() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let mut references = UnitReferences::default();
        let acquire = || { calls.set(calls.get() + 1); Ok(()) };
        references.acquire("run", false, acquire).unwrap();
        for _ in 0..3 { references.acquire("run", true, acquire).unwrap(); }
        assert!(references.acquire("run", false, acquire).is_err());
        assert_eq!(calls.get(), 1);
        references.release("run", acquire).unwrap();
        references.release("run", acquire).unwrap();
        assert_eq!(calls.get(), 2);
        assert!(references.units.is_empty());
        // A new connection has no ledger entry and must acquire its own ref.
        UnitReferences::default().acquire("run", true, acquire).unwrap();
        assert_eq!(calls.get(), 3);
    }
    #[test]
    fn uncertain_reference_rpc_is_never_retried_on_same_connection() {
        for failed_acquire in [true, false] {
            let mut references = UnitReferences::default();
            if failed_acquire {
                assert!(references.acquire("run", false, || Err(io::ErrorKind::TimedOut.into())).is_err());
            } else {
                references.acquire("run", false, || Ok(())).unwrap();
                assert!(references.release("run", || Err(io::ErrorKind::TimedOut.into())).is_err());
            }
            assert!(references.acquire("run", true, || panic!("duplicate reference")).is_err());
            assert!(references.acquire("run", false, || panic!("duplicate AddRef")).is_err());
            assert!(references.release("run", || panic!("uncertain UnrefUnit retry")).is_err());
            assert_eq!(references.units.len(), 1);
        }
    }
    #[test]
    fn missing_cgroup_waits_for_inactive_and_other_errors_deny() {
        for inactive in [false, true] {
            assert_eq!(cleanup_cgroup_empty(Err(io::ErrorKind::NotFound.into()), inactive).unwrap(), inactive);
            assert!(cleanup_cgroup_empty(Err(io::ErrorKind::PermissionDenied.into()), inactive).is_err());
            assert!(cleanup_cgroup_empty(Ok("invalid".into()), inactive).is_err());
            assert!(!cleanup_cgroup_empty(Ok("populated 1\n".into()), inactive).unwrap());
        }
        assert!(cleanup_cgroup_empty(Ok("populated 0\n".into()), true).unwrap());
    }
    #[test]
    fn pty_budget_reserves_host_capacity_and_rounds_down() {
        assert_eq!(pty_budget("4096\n", "1024\n").unwrap(), 2764);
        assert_eq!(pty_budget("3", "1").unwrap(), 1);
        for (maximum, reserved) in [("0", "0"), ("1", "1"), ("1", "2"),
            ("max", "0"), ("4096", "-1"), ("4096", ""), ("1 2", "0")] {
            assert!(pty_budget(maximum, reserved).is_err());
        }
    }
    #[test]
    fn dns_second_layer_has_no_private_resolver_exemption() {
        assert!(egress_second_layer_valid(&[], "resolved_varlink"));
        assert!(egress_second_layer_valid(&[], "dedicated_dns_broker"));
        assert!(!egress_second_layer_valid(&["10.0.0.53/32".into()], "dedicated_dns_broker"));
        assert!(!egress_second_layer_valid(&[], "gateway_selected"));
    }
}
