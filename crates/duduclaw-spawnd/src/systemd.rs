//! Root-selected systemd policy and fd-only launch transport (design §4.2–§4.4).
use std::{collections::BTreeMap, io};
use duduclaw_isolation::{execution::{ResourceLimits, RunTimeouts}, ids::{EmployeeId, RunId}};

pub const TRAMPOLINE: &str = "/usr/lib/duduclaw/duduclaw-run";
pub const SPAWND: &str = "/usr/lib/duduclaw/duduclaw-spawnd";
// CAP_CHOWN is required for per-run devpts (R7-M1). No CAP_FOWNER is granted.
// CAP_SYS_RESOURCE (24): supervisor-only oom_score_adj = -1000 and init's reset
// of value and inherited minimum to 0 (HX D8 revision §2.1 [F2]); dropped with
// the whole bounding set before any employee-uid process exists.
pub const TRAMPOLINE_CAPABILITIES: u64 = (1 << 0) | (1 << 6) | (1 << 7)
    | (1 << 8) | (1 << 12) | (1 << 21) | (1 << 24);
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
/// [`invalid`] naming what was refused: `invalid unit policy: <what>` (same
/// kind). `<what>` names a property, cgroup file or process field and carries
/// only values spawnd set or PID 1/the kernel reports for the unit, bounded
/// with [`bounded_text`] where they come from outside.
fn invalid_because(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("invalid unit policy: {what}"))
}
/// Single-line, bounded rendering for a refusal message: at most 200 bytes of
/// `text` (cut at a character boundary, `...` appended when cut), every
/// control character replaced by `?`.
#[cfg(any(target_os = "linux", test))]
fn bounded_text(text: &str) -> String {
    const MAX: usize = 200;
    let mut rendered = String::new();
    for character in text.chars() {
        let character = if character.is_control() { '?' } else { character };
        if rendered.len() + character.len_utf8() > MAX { rendered.push_str("..."); break; }
        rendered.push(character);
    }
    rendered
}
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
/// R6 update census: a unit PID 1 lists is a live spawnd run unit when its
/// name is exactly a run unit's and it is in any state but `inactive` or
/// `failed` (`active`, `activating`, `deactivating`, `reloading`, or anything
/// unknown: fail closed). Other names are not spawnd's.
pub fn run_unit_live(name: &str, active_state: &str) -> bool {
    parse_run_unit(name).is_some() && !matches!(active_state, "inactive" | "failed")
}
/// The `ListUnitsByPatterns` filter of the R6 census: exactly the run-unit
/// name pattern, in every `ActiveState` but `inactive` and `failed`. A state a
/// future systemd adds is not listed here; the census's second half (the
/// employees cgroup must be empty) still sees any process such a unit holds.
pub const RUN_UNIT_GLOB: &str = "duduclaw-run-*.service";
pub const LIVE_UNIT_STATES: [&str; 6] = ["active", "reloading", "activating", "deactivating", "maintenance", "refreshing"];
/// Hard cap on the entries one census reply may carry (spawnd never keeps more
/// run units than this); a longer reply refuses the census.
pub const MAX_CENSUS_UNITS: usize = 1024;
/// A sequence of at most `N` entries, bounded while it is deserialised: the
/// visitor fails at entry `N + 1` instead of building the whole list first
/// (PID 1's census and gate replies; on Linux its D-Bus signature is the
/// plain array's, `a…`).
#[derive(Debug)]
pub struct BoundedList<T, const N: usize>(pub Vec<T>);
impl<'de, T: serde::Deserialize<'de>, const N: usize> serde::Deserialize<'de> for BoundedList<T, N> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries<T, const N: usize>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Entries<T, N> {
            type Value = BoundedList<T, N>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "a sequence of at most {N} entries")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
                // The length hint comes from the peer: never allocate past N for it.
                let mut entries = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(N));
                while let Some(entry) = sequence.next_element()? {
                    if entries.len() == N { return Err(serde::de::Error::invalid_length(N + 1, &self)); }
                    entries.push(entry);
                }
                Ok(BoundedList(entries))
            }
        }
        deserializer.deserialize_seq(Entries(std::marker::PhantomData))
    }
}
/// The census over PID 1's filtered list (`(Id, ActiveState)` per entry):
/// more entries than [`MAX_CENSUS_UNITS`] refuses, otherwise the live run units
/// ([`run_unit_live`]) are counted.
pub fn census_live_run_units<'a>(listed: impl ExactSizeIterator<Item = (&'a str, &'a str)>) -> io::Result<u32> {
    if listed.len() > MAX_CENSUS_UNITS { return Err(invalid()); }
    let live = listed.filter(|(name, state)| run_unit_live(name, state)).count();
    u32::try_from(live).map_err(|_| invalid())
}
/// R6 `Apply` precondition over PID 1's list of `units` (`(Id, ActiveState,
/// job id)` per loaded entry; a unit PID 1 has not loaded has no process and no
/// job). `Ok(true)` only when every listed entry is one of `units`, at most once,
/// `inactive` or `failed`, with no job. `Err`: an entry PID 1 should not have
/// returned (another name, a duplicate, too many).
pub fn units_down<'a>(units: &[&str], listed: impl ExactSizeIterator<Item = (&'a str, &'a str, u32)>) -> io::Result<bool> {
    if listed.len() > units.len() { return Err(invalid()); }
    let mut seen = Vec::with_capacity(units.len());
    let mut down = true;
    for (name, state, job) in listed {
        if !units.contains(&name) || seen.contains(&name) { return Err(invalid()); }
        seen.push(name);
        down &= matches!(state, "inactive" | "failed") && job == 0;
    }
    Ok(down)
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
        // R7g (measured, Ubuntu 24.04 / systemd 255): two directives are NOT
        // set; `RUN_UNIT_DEFAULTED` reads back their default instead.
        // - `RestrictSUIDSGID=yes` makes systemd's seccomp filter return ENOSYS
        //   for every `openat2` (it cannot inspect `open_how`); the trampoline
        //   opens every path with `openat2` + RESOLVE_* and fails at its probe.
        //   What still holds against an employee creating set-id files: every
        //   mount the employee can write is nosuid (duduclaw-run/src/linux/
        //   mounts.rs: `build_root`'s root tmpfs, home/work attachments and
        //   /run, /tmp; `build_devices`' /dev tmpfs and devpts;
        //   `attach_helpers`' /helper-tmp; and the persistent home and work
        //   clones themselves, bootstrap_mounts.rs `prepare`/`select_workspace`
        //   with `MOUNT_ATTR_NOSUID`), so a set-id bit is never
        //   honoured at exec; the whole process tree runs with no_new_privs
        //   (`NoNewPrivileges` below; the trampoline refuses to install any
        //   filter without it), which also ignores set-id bits and file
        //   capabilities; the employee holds no capability at all (the
        //   trampoline drops and verifies the bounding, permitted, effective,
        //   inheritable and ambient sets). The employee-side common seccomp
        //   filter (duduclaw-run/src/seccomp.rs `common_rules`) additionally
        //   refuses chmod/fchmod/fchmodat/fchmodat2/mknod/mknodat with
        //   S_ISUID or S_ISGID in the mode argument.
        // - An explicit `User=root`/`Group=root` combined with NoNewPrivileges
        //   and any seccomp restriction makes systemd 255 drop CAP_SETUID from
        //   the permitted and effective sets. Root is a system service's
        //   default identity, so neither is set; the started process is read
        //   back as root with exactly `TRAMPOLINE_CAPABILITIES` after start.
        for key in ["PrivateTmp", "PrivateDevices", "PrivateIPC", "ProtectKernelTunables",
            "ProtectKernelModules", "ProtectKernelLogs", "ProtectControlGroups",
            "LockPersonality", "NoNewPrivileges", "MemoryAccounting",
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
            ("DevicePolicy", "closed"),
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
        if RUN_UNIT_DEFAULTED.iter().any(|(key, _)| properties.contains_key(key)) { return Err(invalid()); }
        Ok(Self { name: run_unit(run), properties })
    }
    /// Every property `Systemd::start` reads back after StartTransientUnit. No
    /// property is set without being compared against PID 1's actual value,
    /// and every [`RUN_UNIT_DEFAULTED`] property must read back its default.
    pub fn readback(&self) -> impl Iterator<Item = (&'static str, &Property)> {
        let defaulted: &[(&'static str, Property)] = &RUN_UNIT_DEFAULTED;
        self.properties.iter().map(|(key, value)| (*key, value))
            .chain(defaulted.iter().map(|(key, value)| (*key, value)))
    }
}

/// R7g: run-unit properties spawnd deliberately does not set, with the value
/// PID 1 must report for each. `User`/`Group` empty: a system service with
/// neither set runs as root:root with the full bounding set it is given (an
/// explicit `User=root` loses CAP_SETUID on systemd 255). `RestrictSUIDSGID`
/// false: its seccomp filter makes `openat2` ENOSYS. A mismatch refuses the
/// start like every other readback failure.
pub static RUN_UNIT_DEFAULTED: [(&str, Property); 3] = [
    ("User", Property::Text(String::new())),
    ("Group", Property::Text(String::new())),
    ("RestrictSUIDSGID", Property::Bool(false)),
];

/// R7g: `/proc/<MainPID>/status` of a freshly started run unit, before spawnd
/// sends the bootstrap offer (the trampoline drops nothing until then). Every
/// one of `Uid`, `Gid`, `CapPrm`, `CapEff`, `CapBnd`, `CapAmb`, `NoNewPrivs`
/// must appear exactly once (otherwise `Err`); `Ok(true)` only for real,
/// effective, saved and filesystem uid and gid 0, permitted = effective =
/// bounding = [`TRAMPOLINE_CAPABILITIES`], no ambient capability and
/// no_new_privs set.
pub fn run_process_status_ok(status: &str) -> io::Result<bool> {
    run_process_status_mismatch(status).map(|mismatch| mismatch.is_none())
}
/// [`run_process_status_ok`] naming the first field (in the order `Uid`,
/// `Gid`, `CapPrm`, `CapEff`, `CapBnd`, `CapAmb`, `NoNewPrivs`) that is not
/// as required, with its value as read: `Ok(None)` exactly when
/// `run_process_status_ok` is `Ok(true)`. A missing, duplicated or malformed
/// field is `Err` naming it (`main process <field> missing|duplicated|malformed`).
pub fn run_process_status_mismatch(status: &str) -> io::Result<Option<(&'static str, &str)>> {
    const FIELDS: [&str; 7] = ["Uid", "Gid", "CapPrm", "CapEff", "CapBnd", "CapAmb", "NoNewPrivs"];
    let mut fields: BTreeMap<&'static str, &str> = BTreeMap::new();
    for line in status.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let Some(&known) = FIELDS.iter().find(|field| **field == key) else { continue };
        if fields.insert(known, value.trim()).is_some() {
            return Err(invalid_because(format_args!("main process {known} duplicated")));
        }
    }
    let field = |key: &'static str| fields.get(key).copied()
        .ok_or_else(|| invalid_because(format_args!("main process {key} missing")));
    let root = |text: &str| {
        let ids: Vec<&str> = text.split_whitespace().collect();
        ids.len() == 4 && ids.iter().all(|id| *id == "0")
    };
    let capabilities = |key: &'static str, text: &str| -> io::Result<u64> {
        let malformed = || invalid_because(format_args!("main process {key} malformed"));
        if text.len() != 16 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err(malformed()); }
        u64::from_str_radix(text, 16).map_err(|_| malformed())
    };
    for key in ["Uid", "Gid"] {
        let text = field(key)?;
        if !root(text) { return Ok(Some((key, text))); }
    }
    for (key, required) in [("CapPrm", TRAMPOLINE_CAPABILITIES), ("CapEff", TRAMPOLINE_CAPABILITIES),
        ("CapBnd", TRAMPOLINE_CAPABILITIES), ("CapAmb", 0)] {
        let text = field(key)?;
        if capabilities(key, text)? != required { return Ok(Some((key, text))); }
    }
    let text = field("NoNewPrivs")?;
    if text != "1" { return Ok(Some(("NoNewPrivs", text))); }
    Ok(None)
}
/// R7g: `/proc/<MainPID>/cgroup` names exactly the unit's own cgroup (v2 only;
/// spawnd moves the trampoline into `rt-init` only after start). Guards the
/// status readback against a recycled PID.
pub fn run_process_cgroup_ok(membership: &str, control_group: &str) -> bool {
    membership.strip_prefix("0::").and_then(|rest| rest.strip_suffix('\n')) == Some(control_group)
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
    /// One `ListUnits*` entry: id, description, load, active and sub state,
    /// following, unit path, job id, job type, job path.
    type ListedUnit = (String, String, String, String, String, String,
        OwnedObjectPath, u32, String, OwnedObjectPath);
    /// On the bus a [`BoundedList`] is exactly the plain array of its entries.
    impl<T: zbus::zvariant::Type, const N: usize> zbus::zvariant::Type for BoundedList<T, N> {
        const SIGNATURE: &'static zbus::zvariant::Signature = <Vec<T> as zbus::zvariant::Type>::SIGNATURE;
    }
    /// A `ListUnitsByPatterns` reply (census and gate check), bounded to
    /// [`MAX_CENSUS_UNITS`] entries while it is deserialised.
    type BoundedUnits = BoundedList<ListedUnit, MAX_CENSUS_UNITS>;
    /// The largest `ListUnitsByPatterns` reply body deserialised at all
    /// ([`MAX_CENSUS_UNITS`] entries of a few hundred bytes each fit well).
    const MAX_LIST_REPLY_BYTES: usize = 1 << 20;
    /// A body over [`MAX_LIST_REPLY_BYTES`] is refused before `decode` runs;
    /// otherwise `decode`'s bounded list, or `Err` (refused).
    fn bounded_units(body_bytes: usize, decode: impl FnOnce() -> zbus::Result<BoundedUnits>) -> io::Result<Vec<ListedUnit>> {
        if body_bytes > MAX_LIST_REPLY_BYTES { return Err(invalid()); }
        Ok(decode().map_err(bus_error)?.0)
    }
    pub struct Systemd { connection: Connection, references: Mutex<UnitReferences> }
    /// A host-only receipt; no cgroup paths or PIDs go back over gateway IPC.
    pub struct StartedUnit { pub name: String, path: OwnedObjectPath, pub cgroup: PathBuf }
    #[cfg(test)]
    pub(crate) struct TestRunCleanup<'a>(pub &'a Systemd, pub RunId);
    #[cfg(test)]
    impl Drop for TestRunCleanup<'_> {
        fn drop(&mut self) {
            if let Err(error) = self.0.cleanup_test_run(self.1) {
                eprintln!("test run cleanup failed for {}: {error}", run_unit(self.1));
            }
        }
    }
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
            if properties.iter().any(|(key, _)| *key == "AddRef") { return Err(invalid_because("AddRef preset by caller")); }
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
            let units: Vec<ListedUnit> = self.manager()?.call("ListUnits", &()).map_err(bus_error)?;
            let mut runs: Vec<_> = units.into_iter().filter_map(|unit| parse_run_unit(&unit.0)).collect();
            runs.sort();
            runs.dedup();
            if runs.len() > 1024 { return Err(invalid()); }
            Ok(runs)
        }
        /// R6 update census, independent of any spawnd process's memory: how
        /// many run units PID 1 currently has loaded in a live state
        /// ([`run_unit_live`]). Every unit that has a process, a job or a
        /// pending stop is loaded, so a run of an earlier spawnd process whose
        /// asynchronous teardown is still in progress is counted. PID 1 filters
        /// (exact run-unit pattern, live states); a reply longer than
        /// [`MAX_CENSUS_UNITS`] refuses the census.
        pub fn live_run_units(&self) -> io::Result<u32> {
            let units = self.list_by_patterns(&LIVE_UNIT_STATES, &[RUN_UNIT_GLOB])?;
            census_live_run_units(units.iter().map(|unit| (unit.0.as_str(), unit.3.as_str())))
        }
        /// R6 `Apply` precondition, from the same source as the census: PID 1
        /// reports each of `units` (exact names) not loaded, or `inactive` /
        /// `failed` with no job ([`units_down`]). A bus error is `Err`.
        pub fn units_down(&self, units: &[&str]) -> io::Result<bool> {
            let listed = self.list_by_patterns(&[], units)?;
            units_down(units, listed.iter().map(|unit| (unit.0.as_str(), unit.3.as_str(), unit.7)))
        }
        /// PID 1's filtered list, bounded before and while it is deserialised
        /// ([`bounded_units`]): an oversize body or entry 1025 refuses.
        fn list_by_patterns(&self, states: &[&str], patterns: &[&str]) -> io::Result<Vec<ListedUnit>> {
            let reply = self.manager()?.call_method("ListUnitsByPatterns", &(states, patterns)).map_err(bus_error)?;
            let body = reply.body();
            bounded_units(body.len(), || body.deserialize::<BoundedUnits>())
        }
        /// `duduclaw-spawnd.service` as PID 1 sees it now: `(MainPID,
        /// InvocationID)` (lowercase hex, 32 digits) while it is `active` with a
        /// main process, otherwise `None`. Read-only properties; used by the
        /// consistency waiter instead of `/proc/<pid>` (no capability needed,
        /// unaffected by `hidepid=` and spawnd being non-dumpable).
        pub fn spawnd_identity(&self) -> io::Result<Option<(u32, String)>> {
            let name = "duduclaw-spawnd.service";
            let path: OwnedObjectPath = match self.manager()?.call("GetUnit", &(name,)) {
                Ok(path) => path,
                Err(error) if missing_unit(&error) => return Ok(None),
                Err(error) => return Err(bus_error(error)),
            };
            let unit = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
            let state: String = unit.get_property("ActiveState").map_err(bus_error)?;
            if state != "active" { return Ok(None); }
            let invocation: Vec<u8> = unit.get_property("InvocationID").map_err(bus_error)?;
            let service = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
            let pid: u32 = service.get_property("MainPID").map_err(bus_error)?;
            if pid == 0 || invocation.len() != 16 || invocation.iter().all(|byte| *byte == 0) { return Ok(None); }
            Ok(Some((pid, invocation.iter().map(|byte| format!("{byte:02x}")).collect())))
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
        #[cfg(test)]
        pub(crate) fn cleanup_test_run(&self, run: RunId) -> io::Result<()> {
            let name = run_unit(run);
            let manager = self.manager()?;
            let path: OwnedObjectPath = match manager.call("GetUnit", &(name.as_str(),)) {
                Ok(path) => path,
                Err(error) if missing_unit(&error) => return Ok(()),
                Err(error) => return Err(bus_error(error)),
            };
            self.references.lock().map_err(|_| invalid())?.acquire(&name, true, || {
                manager.call::<_, _, ()>("RefUnit", &(name.as_str(),)).map_err(bus_error)
            })?;
            self.kill_run(run)?;
            let unit = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                let state: String = unit.get_property("ActiveState").map_err(bus_error)?;
                if state == "inactive" || state == "failed" { break; }
                if Instant::now() >= until { return Err(io::ErrorKind::TimedOut.into()); }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _: () = manager.call("ResetFailedUnit", &(name.as_str(),)).map_err(bus_error)?;
            self.references.lock().map_err(|_| invalid())?.release(&name, || {
                manager.call::<_, _, ()>("UnrefUnit", &(name.as_str(),)).map_err(bus_error)
            })?;
            // Collection is asynchronous after the last reference is released.
            while self.list_runs()?.contains(&run) {
                if Instant::now() >= until { return Err(io::ErrorKind::TimedOut.into()); }
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(())
        }
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
                if code != 1 { return Err(invalid_because(format_args!("probe ExecMainCode {code} (status {status})"))); }
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
            self.start_exec(policy, TRAMPOLINE, vec![TRAMPOLINE], bootstrap, stdout, stderr)
        }
        /// [`Self::start`] with its fixed `ExecStart`; root tests alone pass a
        /// harmless executable to exercise the production policy and readback.
        fn start_exec(&self, policy: &UnitPolicy, executable: &str, argv: Vec<&str>, bootstrap: BorrowedFd<'_>,
            stdout: BorrowedFd<'_>, stderr: BorrowedFd<'_>) -> io::Result<StartedUnit> {
            if parse_run_unit(&policy.name).is_none() { return Err(invalid_because("unit name")); }
            self.ensure_employee_slice(policy)?;
            let mut properties: Vec<(&str, Value<'_>)> = policy.properties.iter()
                .map(|(key, property)| (*key, value(property))).collect();
            properties.push(("ExecStart", Value::from(vec![(executable, argv, false)])));
            properties.push(("StandardInputFileDescriptor", Value::from(Fd::from(bootstrap))));
            properties.push(("StandardOutputFileDescriptor", Value::from(Fd::from(stdout))));
            properties.push(("StandardErrorFileDescriptor", Value::from(Fd::from(stderr))));
            self.start_referenced(&policy.name, properties)?;
            let manager = self.manager()?;
            let result = (|| {
                let path: OwnedObjectPath = manager.call("GetUnit", &(policy.name.as_str(),)).map_err(bus_error)?;
                let service = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").map_err(bus_error)?;
                let unit_proxy = Proxy::new(&self.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").map_err(bus_error)?;
                // Every property is compared before refusing, so one refusal
                // names every mismatch. A property that cannot be read at all
                // (bus error) still refuses at once: a dead bus would otherwise
                // cost one method timeout per remaining property.
                let mut refusals = ReadbackRefusals::default();
                for (key, expected) in policy.readback() {
                    if matches!(key, "BindsTo" | "After") {
                        let actual: Vec<String> = unit_proxy.get_property(key).map_err(bus_error)?;
                        refusals.check(dependency_matches(key, expected, &actual));
                    } else {
                        let actual: OwnedValue = service.get_property(key).map_err(bus_error)?;
                        refusals.check(readback_matches(key, expected, actual));
                    }
                }
                refusals.finish()?;
                // Standard*FileDescriptor overrides the null defaults; systemd
                // 249 does not expose these received descriptors as properties.
                // B2 must verify bootstrap/pipe identity before releasing a run.
                let until = std::time::Instant::now() + Duration::from_secs(5);
                let (cgroup, control_group) = loop {
                    let path_text: String = service.get_property("ControlGroup").map_err(bus_error)?;
                    if let Ok(path) = checked_cgroup(&path_text) { break (path, path_text); }
                    if std::time::Instant::now() >= until { return Err(bus_error("cgroup timeout")); }
                    std::thread::sleep(Duration::from_millis(25));
                };
                verify_run_resources(&cgroup, policy)?;
                verify_main_process(&unit_proxy, &service, &control_group)?;
                drop(service);
                drop(unit_proxy);
                Ok(StartedUnit { name: policy.name.clone(), path, cgroup })
            })();
            if result.is_err() { let _ = self.kill_name(&policy.name); }
            result
        }
        fn ensure_employee_slice(&self, policy: &UnitPolicy) -> io::Result<()> {
            let Some(Property::Text(name)) = policy.properties.get("Slice") else {
                return Err(invalid_because("slice Slice not in policy"));
            };
            let properties: Vec<(&str, Value<'_>)> = ["MemoryAccounting", "CPUAccounting",
                "TasksAccounting", "IOAccounting", "MemoryMax", "MemorySwapMax",
                "CPUQuotaPerSecUSec", "TasksMax", "IOReadBandwidthMax", "IOWriteBandwidthMax",
                "IOReadIOPSMax", "IOWriteIOPSMax"].into_iter().map(|key| {
                    policy.properties.get(key).map(|property| (key, value(property)))
                        .ok_or_else(|| invalid_because(format_args!("slice {key} not in policy")))
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
                if actual != expected.try_to_owned().map_err(bus_error)? { return Err(readback_refusal("slice readback", key, &actual)); }
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
        let refuse = |what: String| invalid_because(format_args!("resource {what}"));
        for (property, file) in [("MemoryMax", "memory.max"), ("MemorySwapMax", "memory.swap.max"),
            ("TasksMax", "pids.max")] {
            let Some(Property::U64(expected)) = policy.properties.get(property) else {
                return Err(refuse(format!("{property} not in policy")));
            };
            let actual = read_number(&directory.join(file))
                .map_err(|error| named(error, format!("resource {file} not a number")))?;
            // Kernel page rounding may narrow a byte ceiling but never widen it.
            if actual > *expected || (*expected != 0 && actual == 0) {
                return Err(refuse(format!("{file} (read {actual}, {property} {expected})")));
            }
        }
        let Some(Property::U64(expected_cpu)) = policy.properties.get("CPUQuotaPerSecUSec") else {
            return Err(refuse("CPUQuotaPerSecUSec not in policy".into()));
        };
        let text = std::fs::read_to_string(directory.join("cpu.max"))?;
        let cpu_max = || refuse(format!("cpu.max (read {:?}, CPUQuotaPerSecUSec {expected_cpu})", bounded_text(&text)));
        let words: Vec<_> = text.split_whitespace().collect();
        if words.len() != 2 { return Err(cpu_max()); }
        let quota: u64 = words[0].parse().map_err(|_| cpu_max())?;
        let period: u64 = words[1].parse().map_err(|_| cpu_max())?;
        if period == 0 || quota == 0 || u128::from(quota) * 1_000_000 > u128::from(*expected_cpu) * u128::from(period) {
            return Err(cpu_max());
        }
        let text = std::fs::read_to_string(directory.join("io.max"))?;
        for (property, field) in [("IOReadBandwidthMax", "rbps"), ("IOWriteBandwidthMax", "wbps"),
            ("IOReadIOPSMax", "riops"), ("IOWriteIOPSMax", "wiops")] {
            let Some(Property::DeviceLimits(limits)) = policy.properties.get(property) else {
                return Err(refuse(format!("{property} not in policy")));
            };
            if limits.len() != 1 { return Err(refuse(format!("{property} not one device"))); }
            let device = limits[0].0.strip_prefix("/dev/block/")
                .ok_or_else(|| refuse(format!("{property} device not /dev/block/<major>:<minor>")))?;
            let io_max = || refuse(format!("io.max {field} for {device} (read {:?}, {property} {})",
                bounded_text(&text), limits[0].1));
            let matching: Vec<_> = text.lines().filter(|line| line.split_whitespace().next() == Some(device)).collect();
            if matching.len() != 1 { return Err(io_max()); }
            let mut found = None;
            for entry in matching[0].split_whitespace().skip(1) {
                let (key, value) = entry.split_once('=').ok_or_else(|| io_max())?;
                if key == field {
                    if found.is_some() { return Err(io_max()); }
                    found = Some(value.parse::<u64>().map_err(|_| io_max())?);
                }
            }
            if !found.is_some_and(|value| value > 0 && value <= limits[0].1) { return Err(io_max()); }
        }
        Ok(())
    }
    /// A refusal (`InvalidData`) from a lower check renamed to `what`; any
    /// other error (I/O, bus) is returned unchanged.
    fn named(error: io::Error, what: impl std::fmt::Display) -> io::Error {
        if error.kind() == io::ErrorKind::InvalidData { invalid_because(what) } else { error }
    }
    /// R7g: the started process itself, not only PID 1's configuration. Waits
    /// (bounded) until the `Type=exec` unit is `active` (its process has
    /// exec'd), then requires `/proc/<MainPID>/status` to show root with
    /// exactly the trampoline capability set ([`run_process_status_ok`]), the
    /// process to sit in this unit's own cgroup ([`run_process_cgroup_ok`]) and
    /// `MainPID` to be unchanged afterwards. This runs before spawnd sends the
    /// bootstrap offer, so the trampoline has not dropped anything yet; a
    /// trampoline that already exited (refused) fails the start here.
    fn verify_main_process(unit: &Proxy<'_>, service: &Proxy<'_>, control_group: &str) -> io::Result<()> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let state: String = unit.get_property("ActiveState").map_err(bus_error)?;
            if state == "active" { break; }
            // A just-queued start job may still read `inactive`.
            if !matches!(state.as_str(), "inactive" | "activating") || Instant::now() >= until {
                return Err(invalid_because(format_args!("main process not active (ActiveState {:?})", bounded_text(&state))));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: u32 = service.get_property("MainPID").map_err(bus_error)?;
        if pid == 0 { return Err(invalid_because("main process MainPID 0")); }
        let process = PathBuf::from(format!("/proc/{pid}"));
        let status = std::fs::read_to_string(process.join("status"))?;
        let membership = std::fs::read_to_string(process.join("cgroup"))?;
        let after: u32 = service.get_property("MainPID").map_err(bus_error)?;
        if after != pid { return Err(invalid_because("main process MainPID changed")); }
        if let Some((field, value)) = run_process_status_mismatch(&status)? {
            return Err(invalid_because(format_args!("main process {field} {:?}", bounded_text(value))));
        }
        if !run_process_cgroup_ok(&membership, control_group) {
            return Err(invalid_because(format_args!("main process cgroup (read {:?}, unit {:?})",
                bounded_text(&membership), bounded_text(control_group))));
        }
        Ok(())
    }
    /// One run-unit property PID 1 reported after start, against the policy
    /// ([`property_matches`]): a mismatch is `invalid unit policy: readback
    /// <key>: actual signature <sig> value <debug>` ([`readback_refusal`]); a
    /// reply that does not even convert keeps its bus error kind and gains the
    /// same naming (`… (not convertible: <bus error>)`).
    fn readback_matches(key: &str, expected: &Property, actual: OwnedValue) -> io::Result<()> {
        let refusal = readback_refusal("readback", key, &actual);
        match property_matches(expected, actual) {
            Ok(true) => Ok(()),
            Ok(false) => Err(refusal),
            Err(error) => Err(io::Error::new(error.kind(),
                format!("{refusal} (not convertible: {})", bounded_text(&error.to_string())))),
        }
    }
    /// `BindsTo`/`After`, read from the Unit interface. systemd adds implicit
    /// dependencies of its own, so the reported list must contain every
    /// policy entry (exact name, any order) and may contain more. An empty or
    /// non-list policy entry refuses. A mismatch is `invalid unit policy:
    /// readback <key>: dependency missing (actual <bounded list>)`.
    fn dependency_matches(key: &str, expected: &Property, actual: &[String]) -> io::Result<()> {
        let present = match expected {
            Property::Strings(wanted) => !wanted.is_empty() && wanted.iter().all(|name| actual.contains(name)),
            _ => false,
        };
        if present { return Ok(()); }
        Err(invalid_because(format_args!("readback {key}: dependency missing (actual {})",
            bounded_text(&format!("{actual:?}")))))
    }
    /// At most this many readback failures are named in one refusal.
    const MAX_READBACK_NAMED: usize = 16;
    /// A combined readback refusal stops adding named failures past this many
    /// bytes (each failure is itself bounded, see [`readback_refusal`]).
    const MAX_READBACK_MESSAGE: usize = 2048;
    /// Every readback failure of one start, in readback order: the first
    /// [`MAX_READBACK_NAMED`] are kept, the rest only counted.
    #[derive(Default)]
    struct ReadbackRefusals { named: Vec<io::Error>, total: usize }
    impl ReadbackRefusals {
        fn check(&mut self, result: io::Result<()>) {
            if let Err(error) = result {
                self.total += 1;
                if self.named.len() < MAX_READBACK_NAMED { self.named.push(error); }
            }
        }
        /// `Ok` only when nothing failed. Otherwise one error `invalid unit
        /// policy: readback <Key1>: …; readback <Key2>: …[; <n> more readback
        /// mismatches not shown]`, bounded to about [`MAX_READBACK_MESSAGE`]
        /// bytes; its kind is `InvalidData` unless a failure had another kind
        /// (a reply that did not convert), in which case the first such kind.
        fn finish(self) -> io::Result<()> {
            if self.total == 0 { return Ok(()); }
            const PREFIX: &str = "invalid unit policy: ";
            let kind = self.named.iter().map(io::Error::kind)
                .find(|kind| *kind != io::ErrorKind::InvalidData).unwrap_or(io::ErrorKind::InvalidData);
            let mut message = String::from(PREFIX);
            let mut shown = 0;
            for error in &self.named {
                let text = error.to_string();
                let detail = text.strip_prefix(PREFIX).unwrap_or(&text);
                let separator = if shown == 0 { "" } else { "; " };
                if shown > 0 && message.len() + separator.len() + detail.len() > MAX_READBACK_MESSAGE { break; }
                message.push_str(separator);
                message.push_str(detail);
                shown += 1;
            }
            if shown < self.total {
                message.push_str(&format!("; {} more readback mismatches not shown", self.total - shown));
            }
            Err(io::Error::new(kind, message))
        }
    }
    /// `invalid unit policy: <scope> <key>: actual signature <sig> value
    /// <debug>`, both renderings bounded ([`bounded_text`]). Unit and slice
    /// properties are spawnd's own settings or systemd defaults, not secrets.
    fn readback_refusal(scope: &str, key: &str, actual: &OwnedValue) -> io::Error {
        invalid_because(format_args!("{scope} {key}: actual signature {} value {}",
            bounded_text(&actual.value_signature().to_string()), bounded_text(&format!("{actual:?}"))))
    }
    /// List-valued properties are sets to systemd, which need not report them
    /// in the order they were set (systemd 255 reverses `DeviceAllow`): they
    /// match as multisets of exact entries ([`same_entries`]), no entry
    /// normalised. Every other property matches by exact value.
    fn property_matches(expected: &Property, actual: OwnedValue) -> io::Result<bool> {
        match expected {
            Property::Strings(wanted) => {
                let actual: Vec<String> = actual.try_into().map_err(bus_error)?;
                Ok(same_entries(actual, wanted))
            }
            Property::AllowStrings(wanted) => {
                let (allow, actual): (bool, Vec<String>) = actual.try_into().map_err(bus_error)?;
                Ok(allow && same_entries(actual, wanted))
            }
            Property::DeviceLimits(wanted) => {
                let actual: Vec<(String, u64)> = actual.try_into().map_err(bus_error)?;
                Ok(same_entries(actual, wanted))
            }
            Property::DeviceAccess(wanted) => {
                let actual: Vec<(String, String)> = actual.try_into().map_err(bus_error)?;
                Ok(same_entries(actual, wanted))
            }
            _ => Ok(actual == value(expected).try_to_owned().map_err(bus_error)?),
        }
    }
    /// Exact multiset equality: the same entries, each as often, any order.
    fn same_entries<T: Ord + Clone>(mut actual: Vec<T>, wanted: &[T]) -> bool {
        let mut wanted = wanted.to_vec();
        actual.sort();
        wanted.sort();
        actual == wanted
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
            let count = duduclaw_run::linux::syscalls::getrandom(&mut bytes, 0).unwrap();
            assert_eq!(count, bytes.len());
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
                    if let Err(error) = systemd.cleanup_test_run(self.0) {
                        eprintln!("test run cleanup failed: {error}");
                    }
                }
            }
        }
        #[test]
        fn update_census_reply_is_bounded_in_dbus_decoding() {
            use zbus::zvariant::{serialized::Context, to_bytes, ObjectPath, LE};
            let path = |text: &'static str| OwnedObjectPath::from(ObjectPath::try_from(text).unwrap());
            let entry = |n: u32| -> ListedUnit {
                (format!("duduclaw-run-{n:032x}.service"), String::new(), "loaded".into(), "active".into(),
                    "running".into(), String::new(), path("/org/freedesktop/systemd1/unit/x"), 0, String::new(), path("/"))
            };
            // The reply's own encoding (signature `a(ssssssouso)`, as PID 1 sends it).
            let encode = |entries: usize| to_bytes(Context::new_dbus(LE, 0),
                &(0..entries as u32).map(entry).collect::<Vec<ListedUnit>>()).unwrap();
            assert_eq!(<BoundedUnits as zbus::zvariant::Type>::SIGNATURE, <Vec<ListedUnit> as zbus::zvariant::Type>::SIGNATURE);
            let at_cap = encode(MAX_CENSUS_UNITS);
            assert!(at_cap.len() <= MAX_LIST_REPLY_BYTES);
            let (units, _) = at_cap.deserialize::<BoundedUnits>().unwrap();
            assert_eq!(units.0.len(), MAX_CENSUS_UNITS);
            let over = encode(MAX_CENSUS_UNITS + 1);
            assert!(over.deserialize::<BoundedUnits>().is_err());
            assert!(bounded_units(over.len(), || over.deserialize::<BoundedUnits>().map(|(units, _)| units)
                .map_err(zbus::Error::from)).is_err());
            assert_eq!(bounded_units(at_cap.len(), || at_cap.deserialize::<BoundedUnits>().map(|(units, _)| units)
                .map_err(zbus::Error::from)).unwrap().len(), MAX_CENSUS_UNITS);
            // A body over the byte bound is refused before anything is deserialised.
            assert!(bounded_units(MAX_LIST_REPLY_BYTES + 1, || panic!("deserialised an oversize body")).is_err());
            assert!(bounded_units(MAX_LIST_REPLY_BYTES, || Ok(BoundedList(vec![]))).unwrap().is_empty());
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

        #[test]
        fn run_unit_readback_refuses_explicit_identity_and_restrict_suid_sgid() {
            let owned = |value: Value<'_>| value.try_to_owned().unwrap();
            let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &r7g_limits(), &r7g_timeouts(), 8, 1).unwrap();
            let expected: BTreeMap<&str, &Property> = policy.readback().collect();
            // A system service with neither User= nor Group= reads back "" (root:root).
            for key in ["User", "Group"] {
                assert!(property_matches(expected[key], owned(Value::from(""))).unwrap(), "{key}");
                for actual in ["root", "0", "duduclaw-runsup", "nobody"] {
                    assert!(!property_matches(expected[key], owned(Value::from(actual))).unwrap(), "{key}={actual}");
                }
            }
            assert!(property_matches(expected["RestrictSUIDSGID"], owned(Value::from(false))).unwrap());
            assert!(!property_matches(expected["RestrictSUIDSGID"], owned(Value::from(true))).unwrap());
            // A wrongly typed reply is not a match either.
            assert!(!property_matches(expected["User"], owned(Value::from(0u32))).unwrap());
            assert!(!property_matches(expected["RestrictSUIDSGID"], owned(Value::from(""))).unwrap());
        }

        #[test]
        fn run_unit_readback_mismatch_names_the_property_and_actual_value() {
            let owned = |value: Value<'_>| value.try_to_owned().unwrap();
            let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &r7g_limits(), &r7g_timeouts(), 8, 1).unwrap();
            let expected: BTreeMap<&str, &Property> = policy.readback().collect();
            readback_matches("StandardInput", expected["StandardInput"], owned(Value::from("null"))).unwrap();
            readback_matches("RestrictSUIDSGID", expected["RestrictSUIDSGID"], owned(Value::from(false))).unwrap();
            // A mismatch: still InvalidData, now naming the property, the
            // actual value's D-Bus signature and the actual value.
            let error = readback_matches("StandardInput", expected["StandardInput"], owned(Value::from("fd"))).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            let message = error.to_string();
            assert!(message.starts_with("invalid unit policy"), "{message}");
            assert!(message.starts_with("invalid unit policy: readback StandardInput: actual signature s value "), "{message}");
            assert!(message.contains("fd"), "{message}");
            let error = readback_matches("RestrictSUIDSGID", expected["RestrictSUIDSGID"], owned(Value::from(true))).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().starts_with("invalid unit policy: readback RestrictSUIDSGID: actual signature b value "), "{error}");
            let error = readback_matches("LimitNOFILE", expected["LimitNOFILE"], owned(Value::from(1024u64))).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().starts_with("invalid unit policy: readback LimitNOFILE: actual signature t value "), "{error}");
            assert!(error.to_string().contains("1024"), "{error}");
            // A deny-list (allow flag false) reply for the allow-list property.
            let error = readback_matches("RestrictAddressFamilies", expected["RestrictAddressFamilies"],
                owned(Value::from((false, vec!["AF_UNIX", "AF_INET", "AF_INET6", "AF_NETLINK"])))).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().starts_with("invalid unit policy: readback RestrictAddressFamilies: actual signature (bas) value "), "{error}");
            // The rendering is bounded and single-line.
            let prefix = "invalid unit policy: readback User: actual signature s value ";
            let long = "x\n".repeat(500);
            let message = readback_matches("User", expected["User"], owned(Value::from(long.as_str()))).unwrap_err().to_string();
            assert!(message.starts_with(prefix), "{message}");
            assert!(!message.chars().any(char::is_control), "{message}");
            assert!(message.len() <= prefix.len() + 200 + "...".len(), "{}", message.len());
            assert!(message.ends_with("..."), "{message}");
            // A reply that does not convert keeps its (bus) kind, now named too.
            let error = readback_matches("SystemCallArchitectures", expected["SystemCallArchitectures"],
                owned(Value::from(7u32))).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert!(error.to_string().contains("invalid unit policy: readback SystemCallArchitectures: actual signature u value "), "{error}");
            // The slice readback uses the same naming under its own scope.
            let error = readback_refusal("slice readback", "MemoryMax", &owned(Value::from(u64::MAX)));
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().starts_with("invalid unit policy: slice readback MemoryMax: actual signature t value "), "{error}");
        }

        fn device_access(entries: &[(&str, &str)]) -> OwnedValue {
            Value::from(entries.to_vec()).try_to_owned().unwrap()
        }
        fn device_limits(entries: &[(&str, u64)]) -> OwnedValue {
            Value::from(entries.to_vec()).try_to_owned().unwrap()
        }
        fn strings(entries: &[&str]) -> OwnedValue {
            Value::from(entries.to_vec()).try_to_owned().unwrap()
        }

        #[test]
        fn run_unit_device_allow_readback_is_order_insensitive_and_exact() {
            let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &r7g_limits(), &r7g_timeouts(), 8, 1).unwrap();
            let expected = &policy.properties["DeviceAllow"];
            // As set, and as systemd 255 reports it (reverse order).
            readback_matches("DeviceAllow", expected, device_access(&[("char-pts", "rw"), ("/dev/ptmx", "rw")])).unwrap();
            readback_matches("DeviceAllow", expected, device_access(&[("/dev/ptmx", "rw"), ("char-pts", "rw")])).unwrap();
            let refused: [&[(&str, &str)]; 9] = [
                &[("/dev/ptmx", "rw"), ("char-pts", "rw"), ("/dev/null", "rw")], // extra entry
                &[("/dev/ptmx", "rw")],                                          // missing entry
                &[],                                                             // nothing
                &[("/dev/ptmx", "rw"), ("char-pts", "rw"), ("char-pts", "rw")],  // duplicate
                &[("/dev/ptmx", "rw"), ("/dev/ptmx", "rw")],                     // multiplicity
                &[("/dev/ptmx", "rwm"), ("char-pts", "rw")],                     // access widened
                &[("/dev/ptmx", "rw"), ("char-pts", "r")],                       // access changed
                &[("/dev/pts/ptmx", "rw"), ("char-pts", "rw")],                  // other spelling
                &[("/dev/ptmx", "rw"), ("char-136", "rw")],                      // other specifier
            ];
            for actual in refused {
                let error = readback_matches("DeviceAllow", expected, device_access(actual)).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{actual:?}");
                assert!(error.to_string().starts_with("invalid unit policy: readback DeviceAllow: actual signature a(ss) value "),
                    "{actual:?}: {error}");
            }
        }

        #[test]
        fn run_unit_list_readbacks_are_order_insensitive_multisets() {
            let owned = |value: Value<'_>| value.try_to_owned().unwrap();
            let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &r7g_limits(), &r7g_timeouts(), 8, 1).unwrap();
            let expected: BTreeMap<&str, &Property> = policy.readback().collect();
            // RestrictAddressFamilies: an allow-list of exactly the four, any order.
            let families = expected["RestrictAddressFamilies"];
            assert!(property_matches(families, owned(Value::from((true, vec!["AF_NETLINK", "AF_INET6", "AF_UNIX", "AF_INET"])))).unwrap());
            for actual in [vec!["AF_UNIX", "AF_INET", "AF_INET6"],
                vec!["AF_UNIX", "AF_INET", "AF_INET6", "AF_NETLINK", "AF_PACKET"],
                vec!["AF_UNIX", "AF_UNIX", "AF_INET", "AF_INET6", "AF_NETLINK"],
                vec!["AF_UNIX", "AF_INET", "AF_INET6", "AF_PACKET"]] {
                assert!(!property_matches(families, owned(Value::from((true, actual.clone())))).unwrap(), "{actual:?}");
            }
            // SystemCallArchitectures and Environment: exact multisets.
            assert!(property_matches(expected["SystemCallArchitectures"], strings(&["native"])).unwrap());
            for actual in [&["native", "native"][..], &["x86-64"][..], &["native", "x86"][..], &[][..]] {
                assert!(!property_matches(expected["SystemCallArchitectures"], strings(actual)).unwrap(), "{actual:?}");
            }
            assert!(property_matches(expected["Environment"], strings(&[])).unwrap());
            assert!(!property_matches(expected["Environment"], strings(&["A=b"])).unwrap());
            assert!(same_entries(vec!["B=2".to_string(), "A=1".to_string()], &["A=1".to_string(), "B=2".to_string()]));
            assert!(!same_entries(vec!["A=1".to_string(), "A=1".to_string()], &["A=1".to_string(), "B=2".to_string()]));
            // IO limits: one device each, exact device text and ceiling.
            for key in ["IOReadBandwidthMax", "IOWriteBandwidthMax", "IOReadIOPSMax", "IOWriteIOPSMax"] {
                let Property::DeviceLimits(limits) = expected[key] else { panic!("{key}") };
                let (device, ceiling) = (limits[0].0.as_str(), limits[0].1);
                assert_eq!(device, "/dev/block/8:1");
                assert!(property_matches(expected[key], device_limits(&[(device, ceiling)])).unwrap(), "{key}");
                for actual in [&[(device, ceiling + 1)][..], &[(device, ceiling - 1)][..], &[][..],
                    &[(device, ceiling), (device, ceiling)][..], &[(device, ceiling), ("/dev/block/8:2", ceiling)][..],
                    &[("/dev/sda1", ceiling)][..]] {
                    assert!(!property_matches(expected[key], device_limits(actual)).unwrap(), "{key} {actual:?}");
                }
            }
            // A multi-device limit list (not built by policy today) matches in any order.
            let two = Property::DeviceLimits(vec![("/dev/block/8:1".into(), 5), ("/dev/block/8:2".into(), 7)]);
            assert!(property_matches(&two, device_limits(&[("/dev/block/8:2", 7), ("/dev/block/8:1", 5)])).unwrap());
            assert!(!property_matches(&two, device_limits(&[("/dev/block/8:2", 5), ("/dev/block/8:1", 7)])).unwrap());
        }

        #[test]
        fn run_unit_dependency_readback_requires_every_policy_entry_in_any_order() {
            fn names(list: &[&str]) -> Vec<String> { list.iter().map(|name| name.to_string()).collect() }
            let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &r7g_limits(), &r7g_timeouts(), 8, 1).unwrap();
            for key in ["BindsTo", "After"] {
                let expected = &policy.properties[key];
                dependency_matches(key, expected, &names(&["duduclaw-spawnd.service"])).unwrap();
                // systemd's implicit dependencies are extra entries, in any order.
                dependency_matches(key, expected, &names(&["system.slice", "duduclaw-spawnd.service", "sysinit.target"])).unwrap();
                for actual in [&[][..], &["system.slice"][..], &["duduclaw-spawnd"][..]] {
                    let error = dependency_matches(key, expected, &names(actual)).unwrap_err();
                    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                    assert!(error.to_string().starts_with(&format!("invalid unit policy: readback {key}: dependency missing (actual ")),
                        "{error}");
                }
            }
            // A policy entry that is not a non-empty name list never matches.
            let spawnd = names(&["duduclaw-spawnd.service"]);
            assert!(dependency_matches("After", &Property::Strings(vec![]), &spawnd).is_err());
            assert!(dependency_matches("After", &Property::Text("duduclaw-spawnd.service".into()), &spawnd).is_err());
        }

        #[test]
        fn run_unit_readback_names_every_mismatch_in_one_refusal() {
            let owned = |value: Value<'_>| value.try_to_owned().unwrap();
            let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &r7g_limits(), &r7g_timeouts(), 8, 1).unwrap();
            let expected: BTreeMap<&str, &Property> = policy.readback().collect();
            ReadbackRefusals::default().finish().unwrap();
            let mut refusals = ReadbackRefusals::default();
            refusals.check(readback_matches("DeviceAllow", expected["DeviceAllow"], device_access(&[("/dev/ptmx", "rw"), ("char-pts", "rw")])));
            refusals.finish().unwrap();
            // Two simultaneous mismatches: both named, in readback order, one prefix.
            let mut refusals = ReadbackRefusals::default();
            refusals.check(readback_matches("DeviceAllow", expected["DeviceAllow"],
                device_access(&[("/dev/ptmx", "rw"), ("char-pts", "rw"), ("/dev/null", "rw")])));
            refusals.check(readback_matches("StandardInput", expected["StandardInput"], owned(Value::from("null"))));
            refusals.check(readback_matches("StandardOutput", expected["StandardOutput"], owned(Value::from("journal"))));
            let error = refusals.finish().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            let message = error.to_string();
            assert!(message.starts_with("invalid unit policy: readback DeviceAllow: actual signature a(ss) value "), "{message}");
            assert!(message.contains("/dev/null"), "{message}");
            assert!(message.contains("; readback StandardOutput: actual signature s value "), "{message}");
            assert!(message.contains("journal"), "{message}");
            assert!(!message.contains("StandardInput"), "{message}");
            assert_eq!(message.matches("invalid unit policy").count(), 1, "{message}");
            assert!(!message.contains("more readback mismatches"), "{message}");
            // A dependency mismatch and a non-convertible reply join the same
            // refusal; the latter's (bus) kind is kept.
            let mut refusals = ReadbackRefusals::default();
            refusals.check(dependency_matches("BindsTo", expected["BindsTo"], &[]));
            refusals.check(readback_matches("SystemCallArchitectures", expected["SystemCallArchitectures"], owned(Value::from(7u32))));
            let error = refusals.finish().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
            let message = error.to_string();
            assert!(message.starts_with("invalid unit policy: readback BindsTo: dependency missing (actual "), "{message}");
            assert!(message.contains("; readback SystemCallArchitectures: actual signature u value "), "{message}");
            assert!(message.contains("(not convertible: "), "{message}");
            // At most MAX_READBACK_NAMED are named; the rest are counted.
            let mut refusals = ReadbackRefusals::default();
            for _ in 0..40 {
                refusals.check(readback_matches("StandardInput", expected["StandardInput"], owned(Value::from("fd"))));
            }
            let message = refusals.finish().unwrap_err().to_string();
            assert_eq!(message.matches("readback StandardInput: ").count(), MAX_READBACK_NAMED, "{message}");
            assert!(message.ends_with(&format!("; {} more readback mismatches not shown", 40 - MAX_READBACK_NAMED)), "{message}");
            // The whole message stays bounded even when each failure is long.
            let long = "x".repeat(1000);
            let mut refusals = ReadbackRefusals::default();
            for _ in 0..40 {
                refusals.check(readback_matches("User", expected["User"], owned(Value::from(long.as_str()))));
            }
            let message = refusals.finish().unwrap_err().to_string();
            let shown = message.matches("readback User: ").count();
            assert!(shown >= 1 && shown < MAX_READBACK_NAMED, "{shown}");
            assert!(message.len() <= MAX_READBACK_MESSAGE + 64, "{}", message.len());
            assert!(message.ends_with(&format!("; {} more readback mismatches not shown", 40 - shown)), "{message}");
            assert!(!message.chars().any(char::is_control), "{message}");
        }

        #[test]
        fn non_refusal_errors_are_not_renamed() {
            let renamed = named(invalid(), "resource memory.max not a number");
            assert_eq!(renamed.kind(), io::ErrorKind::InvalidData);
            assert_eq!(renamed.to_string(), "invalid unit policy: resource memory.max not a number");
            let missing = named(io::ErrorKind::NotFound.into(), "resource memory.max not a number");
            assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        }

        fn r7g_limits() -> ResourceLimits {
            ResourceLimits { memory_bytes: 1024 * 1024 * 1024, cpu_quota_percent: 100,
                tasks: 128, open_files: 4096, ptys: 16,
                disk: duduclaw_isolation::execution::DiskQuota::LoopFilesystem { bytes: 1024, inodes: 100 },
                io_read_bytes_per_sec: 1 << 30, io_write_bytes_per_sec: 1 << 30,
                io_read_ops_per_sec: 100_000, io_write_ops_per_sec: 100_000,
                output_buffer_bytes: 65536, output_total_bytes: 1024 * 1024,
                memfd_total_bytes: 64 * 1024 * 1024 }
        }
        fn r7g_timeouts() -> RunTimeouts {
            RunTimeouts { queue_ms: 30000, startup_ms: 5000, runtime_ms: 30000,
                pty_idle_ms: Some(30000), termination_grace_ms: 5000 }
        }
        /// The whole disk holding `/` (IO ceilings name a whole disk; a
        /// partition's limit is applied to its disk and would not read back).
        fn root_whole_disk() -> (u32, u32) {
            use std::os::unix::fs::MetadataExt;
            let dev = std::fs::metadata("/").unwrap().dev();
            let major = (((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0xfff)) as u32;
            let minor = (((dev >> 12) & 0xffff_ff00) | (dev & 0xff)) as u32;
            assert_ne!(major, 0, "/ must be on a block device for the run unit's IO ceilings");
            let node = std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).unwrap();
            let disk = if node.join("partition").exists() { node.parent().unwrap().to_path_buf() } else { node };
            let text = std::fs::read_to_string(disk.join("dev")).unwrap();
            let (major, minor) = text.trim().split_once(':').unwrap();
            (major.parse().unwrap(), minor.parse().unwrap())
        }
        fn status_field<'a>(status: &'a str, key: &str) -> Vec<&'a str> {
            status.lines().find_map(|line| line.strip_prefix(key)).unwrap_or_else(|| panic!("{key}"))
                .split_whitespace().collect()
        }
        struct ProbeDirectory(PathBuf);
        impl Drop for ProbeDirectory {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        /// argv[0] the parent gives the child half; nothing else runs it.
        const PROBE_ARGV0: &str = "duduclaw-r7g-openat2-probe";
        /// The child's verdict, as its exit status. libtest itself only ever
        /// exits 0 (including "running 0 tests") or 101, so neither collides.
        const PROBE_OPENAT2_OK: i32 = 42;
        const PROBE_OPENAT2_REFUSED: i32 = 43;

        /// Child half of `iso_root_run_unit_policy_keeps_setuid_and_openat2`,
        /// run by a copy of this test binary as the run unit's main process
        /// with argv[0] = [`PROBE_ARGV0`]. A no-op anywhere else (other argv[0]
        /// or a parent other than PID 1). Performs the trampoline's own
        /// `openat2` (`fd::open_beneath`: RESOLVE_BENEATH | NO_SYMLINKS |
        /// NO_MAGICLINKS | NO_XDEV, O_PATH), notes the result on stderr
        /// (diagnostics only), stays alive for the parent's `/proc` readback,
        /// then reports by exit status alone (read back from PID 1).
        #[test]
        #[ignore = "iso_root_: child half of iso_root_run_unit_policy_keeps_setuid_and_openat2; no-op unless PID 1 started it with the probe argv[0]"]
        fn iso_root_run_unit_openat2_child() {
            use std::io::Write;
            if std::env::args_os().next().as_deref() != Some(std::ffi::OsStr::new(PROBE_ARGV0))
                || unsafe { libc::getppid() } != 1 { return; }
            let root = std::fs::File::open("/").unwrap();
            let opened = duduclaw_run::linux::fd::open_beneath(root.as_raw_fd(), c".", libc::O_PATH);
            let errno = std::io::Error::last_os_error();
            let _ = writeln!(std::io::stderr(), "r7g probe: open_beneath -> {:?} (errno after the call: {errno})",
                opened.as_ref().map(|_| ()));
            std::thread::sleep(Duration::from_secs(5));
            std::process::exit(if opened.is_ok() { PROBE_OPENAT2_OK } else { PROBE_OPENAT2_REFUSED });
        }

        /// R7g on a real host: a unit started with the production `UnitPolicy`
        /// (through `start_exec`, i.e. the production property readback and
        /// the main-process readback) runs as root with exactly the trampoline
        /// capability set, CAP_SETUID included, and `openat2` works inside it.
        #[test]
        #[ignore = "iso_root_: root disposable systemd Linux VM (DUDUCLAW_ISO_DISPOSABLE=1), / on a block device, no active B1 daemon/runs; --test-threads=1"]
        fn iso_root_run_unit_policy_keeps_setuid_and_openat2() {
            assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"), "disposable host only");
            assert_eq!(unsafe { libc::geteuid() }, 0);
            let systemd = Systemd::connect().unwrap();
            let run = test_id();
            let mut employee = [0u8; 16];
            assert_eq!(duduclaw_run::linux::syscalls::getrandom(&mut employee, 0).unwrap(), employee.len());
            let employee = EmployeeId(employee);
            let (major, minor) = root_whole_disk();
            let mut policy = UnitPolicy::new(run, employee, &r7g_limits(), &r7g_timeouts(), major, minor).unwrap();
            for key in ["User", "Group", "RestrictSUIDSGID"] { assert!(!policy.properties.contains_key(key), "{key}"); }
            // BindsTo= would pull in (or, absent, fail on) the daemon unit, which is
            // not under test; every other production property is kept.
            if systemd.spawnd_identity().unwrap().is_none() {
                for key in ["BindsTo", "After"] { assert!(policy.properties.remove(key).is_some(), "{key}"); }
            }
            // The unit's ProtectHome=/PrivateTmp= hide a test binary under
            // /home, /root or /tmp: run a root-only copy from /var/lib.
            let directory = ProbeDirectory(PathBuf::from("/var/lib").join(format!("{}.r7g-probe", policy.name)));
            {
                use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
                std::fs::DirBuilder::new().mode(0o700).create(&directory.0).unwrap();
                let probe = directory.0.join("probe");
                std::fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
                std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let probe = directory.0.join("probe");
            let probe = probe.to_str().unwrap();
            let input = std::fs::File::open("/dev/null").unwrap();
            // The child's stdout AND stderr (libtest, the loader, a panic) go
            // to this pipe, for diagnostics only; the verdict is the exit status.
            let (reader, output) = nix::unistd::pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
            let cleanup = TestRunCleanup(&systemd, run);
            let started = systemd.start_exec(&policy, probe,
                vec![PROBE_ARGV0, "--exact", "systemd::linux::tests::iso_root_run_unit_openat2_child",
                    "--ignored", "--test-threads=1", "--nocapture"],
                input.as_fd(), output.as_fd(), output.as_fd());
            drop(output);
            let mut bytes = Vec::new();
            let drain = |bytes: &mut Vec<u8>| {
                let mut scratch = [0u8; 4096];
                loop {
                    match nix::unistd::read(reader.as_raw_fd(), &mut scratch) {
                        Ok(0) | Err(nix::errno::Errno::EAGAIN) => return,
                        Ok(size) => bytes.extend_from_slice(&scratch[..size]),
                        Err(nix::errno::Errno::EINTR) => {},
                        Err(error) => panic!("probe output: {error}"),
                    }
                }
            };
            // start_exec holds a reference, so the unit stays loaded (and
            // readable) even when its readback refused and stopped it.
            let path: OwnedObjectPath = systemd.manager().unwrap().call("GetUnit", &(policy.name.as_str(),))
                .unwrap_or_else(|error| panic!("{} never loaded ({error}); start_exec: {:?}", policy.name, started.as_ref().err()));
            let unit = Proxy::new(&systemd.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Unit").unwrap();
            let service = Proxy::new(&systemd.connection, BUS, path.as_str(), "org.freedesktop.systemd1.Service").unwrap();
            // The main process, read while it is still alive (it lingers 5 s).
            let live = started.is_ok().then(|| {
                let pid: u32 = service.get_property("MainPID").unwrap();
                (pid, std::fs::read_to_string(format!("/proc/{pid}/status")))
            });
            let until = Instant::now() + Duration::from_secs(20);
            let state = loop {
                drain(&mut bytes);
                let state: String = unit.get_property("ActiveState").unwrap();
                if state == "inactive" || state == "failed" || Instant::now() >= until { break state; }
                std::thread::sleep(Duration::from_millis(50));
            };
            drain(&mut bytes);
            let code: i32 = service.get_property("ExecMainCode").unwrap();
            let status: i32 = service.get_property("ExecMainStatus").unwrap();
            let main_pid: u32 = service.get_property("ExecMainPID").unwrap();
            let diagnostics = format!("unit {} {state}: ExecMainCode={code} (1 = exited) ExecMainStatus={status} \
                ({PROBE_OPENAT2_OK} = openat2 worked, {PROBE_OPENAT2_REFUSED} = refused, 0 = probe guard skipped it, \
                101 = libtest error or a probe panic, 1 or >= 200 = failed before/at exec); start_exec: {:?}; \
                child stdout+stderr: {:?}; PID 1 and systemd-executor messages: `journalctl -b -u {}` and `journalctl -b _PID={main_pid}`",
                policy.name, started.as_ref().err(), String::from_utf8_lossy(&bytes), policy.name);
            assert!(state == "inactive" || state == "failed", "probe did not exit; {diagnostics}");
            assert_eq!((code, status), (1, PROBE_OPENAT2_OK), "openat2 inside the run unit; {diagnostics}");
            // The production readback (properties, User/Group empty,
            // RestrictSUIDSGID false, main process root + full set) accepted it.
            let started = started.unwrap_or_else(|error| panic!("production run-unit readback: {error}; {diagnostics}"));
            for key in ["User", "Group"] { assert_eq!(service.get_property::<String>(key).unwrap(), "", "{key}"); }
            assert!(!service.get_property::<bool>("RestrictSUIDSGID").unwrap());
            let (pid, status) = live.expect("read while the probe ran");
            assert_ne!(pid, 0, "{diagnostics}");
            let status = status.unwrap_or_else(|error| panic!("/proc/{pid}/status while running: {error}; {diagnostics}"));
            assert_eq!(status_field(&status, "Uid:"), ["0"; 4]);
            assert_eq!(status_field(&status, "Gid:"), ["0"; 4]);
            for key in ["CapEff:", "CapPrm:"] {
                assert_eq!(u64::from_str_radix(status_field(&status, key)[0], 16).unwrap(), TRAMPOLINE_CAPABILITIES, "{key} {status}");
            }
            // CAP_SETUID (7), the bit an explicit User=root lost on systemd 255.
            assert_ne!(TRAMPOLINE_CAPABILITIES & (1 << 7), 0);
            assert!(run_process_status_ok(&status).unwrap());
            // The proxies borrow `path`: end those borrows first.
            drop((unit, service));
            drop(started);
            drop(cleanup);
            let _ = systemd.kill_name(&employee_slice(employee));
            drop(directory);
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
    fn update_census_counts_only_live_run_units() {
        let run = run_unit(RunId([0xab; 16]));
        for state in ["active", "activating", "deactivating", "reloading", "maintenance", ""] {
            assert!(run_unit_live(&run, state), "{state:?}");
        }
        for state in ["inactive", "failed"] { assert!(!run_unit_live(&run, state), "{state:?}"); }
        for name in ["duduclaw-spawnd.service", "duduclaw-run-.service", "duduclaw-employees-ab.slice",
            "duduclaw-run-ABABABABABABABABABABABABABABABAB.service"] {
            assert!(!run_unit_live(name, "active"), "{name}");
        }
    }
    #[test]
    fn update_census_filtered_list_is_capped() {
        let names: Vec<String> = (0..=MAX_CENSUS_UNITS as u32).map(|n| {
            let mut bytes = [0u8; 16]; bytes[..4].copy_from_slice(&n.to_be_bytes()); run_unit(RunId(bytes))
        }).collect();
        // The filter PID 1 applies: exactly the run-unit pattern, every live state.
        assert_eq!(RUN_UNIT_GLOB, "duduclaw-run-*.service");
        assert!(!LIVE_UNIT_STATES.iter().any(|state| matches!(*state, "inactive" | "failed")));
        let at_cap = names[..MAX_CENSUS_UNITS].iter().map(|name| (name.as_str(), "active"));
        assert_eq!(census_live_run_units(at_cap).unwrap(), MAX_CENSUS_UNITS as u32);
        // One entry over the cap: the census is refused, never a partial count.
        assert!(census_live_run_units(names.iter().map(|name| (name.as_str(), "active"))).is_err());
        // Inactive/failed entries and foreign names are not counted.
        let mixed = [(names[0].as_str(), "failed"), (names[1].as_str(), "deactivating"), ("duduclaw-spawnd.service", "active")];
        assert_eq!(census_live_run_units(mixed.into_iter()).unwrap(), 1);
    }
    #[test]
    fn update_census_list_is_bounded_while_deserialising() {
        use std::cell::Cell;
        thread_local! { static DESERIALISED: Cell<usize> = const { Cell::new(0) }; }
        // One entry; counts how many entries the visitor deserialised.
        struct Counted;
        impl<'de> serde::Deserialize<'de> for Counted {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                <u32 as serde::Deserialize>::deserialize(deserializer)?;
                DESERIALISED.with(|count| count.set(count.get() + 1));
                Ok(Counted)
            }
        }
        type Census = BoundedList<Counted, MAX_CENSUS_UNITS>;
        let array = |entries: usize| format!("[{}]", vec!["7"; entries].join(","));
        for entries in [0, 1, MAX_CENSUS_UNITS] {
            assert_eq!(serde_json::from_str::<Census>(&array(entries)).unwrap().0.len(), entries);
        }
        // Entry 1025 fails the visitor: the remaining entries of a much longer
        // array are never deserialised, so the full list is never built.
        DESERIALISED.with(|count| count.set(0));
        assert!(serde_json::from_str::<Census>(&array(4 * MAX_CENSUS_UNITS)).is_err());
        assert_eq!(DESERIALISED.with(Cell::get), MAX_CENSUS_UNITS + 1);
        DESERIALISED.with(|count| count.set(0));
        assert!(serde_json::from_str::<Census>(&array(MAX_CENSUS_UNITS + 1)).is_err());
        assert_eq!(DESERIALISED.with(Cell::get), MAX_CENSUS_UNITS + 1);
        // Not a sequence at all: refused.
        assert!(serde_json::from_str::<Census>("{}").is_err());
    }
    #[test]
    fn update_gate_units_down_only_when_every_entry_is_inactive_without_job() {
        use duduclaw_isolation::ids::ISOLATION_GATED_UNITS as UNITS;
        // Nothing loaded, or every unit inactive/failed with no job: down.
        assert!(units_down(&UNITS, std::iter::empty()).unwrap());
        let all = |state: &'static str, job: u32| UNITS.iter().map(move |name| (*name, state, job)).collect::<Vec<_>>();
        assert!(units_down(&UNITS, all("inactive", 0).into_iter()).unwrap());
        assert!(units_down(&UNITS, [(UNITS[0], "failed", 0)].into_iter()).unwrap());
        // Any of them active, starting, stopping, or with a queued job: not down.
        for (state, job) in [("active", 0), ("activating", 0), ("deactivating", 0), ("reloading", 0), ("inactive", 7)] {
            for unit in UNITS {
                let mut listed = all("inactive", 0);
                for entry in &mut listed { if entry.0 == unit { *entry = (unit, state, job); } }
                assert!(!units_down(&UNITS, listed.into_iter()).unwrap(), "{unit} {state} {job}");
            }
        }
        // Uncertain replies refuse: a foreign name, a duplicate, too many entries.
        assert!(units_down(&UNITS, [("duduclaw-spawnd.service", "inactive", 0)].into_iter()).is_err());
        assert!(units_down(&UNITS, [(UNITS[1], "inactive", 0), (UNITS[1], "inactive", 0)].into_iter()).is_err());
        let mut long = all("inactive", 0); long.push((UNITS[0], "inactive", 0));
        assert!(units_down(&UNITS, long.into_iter()).is_err());
    }
    #[test]
    fn fixed_unit_policy_includes_r7_chown_and_resource_hard_limits() {
        let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &limits(), &timeouts(), 8, 1).unwrap();
        assert_eq!(policy.properties["CapabilityBoundingSet"], Property::U64(TRAMPOLINE_CAPABILITIES));
        assert_eq!(TRAMPOLINE_CAPABILITIES & 1, 1);
        assert_eq!(TRAMPOLINE_CAPABILITIES & (1 << 3), 0);
        // HX [F2]: supervisor-only OOM protection needs CAP_SYS_RESOURCE before the drop.
        assert_eq!(TRAMPOLINE_CAPABILITIES & (1 << 24), 1 << 24);
        #[cfg(target_os = "linux")]
        assert_eq!(duduclaw_run::linux::privileges::REQUIRED_BOUNDING_CAPABILITIES.iter().fold(0u64, |bits, cap| bits | 1 << cap),
            TRAMPOLINE_CAPABILITIES);
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
        assert_eq!(readback.len(), policy.properties.len() + RUN_UNIT_DEFAULTED.len());
    }
    #[test]
    fn run_unit_policy_sets_no_identity_and_no_restrict_suid_sgid() {
        let policy = UnitPolicy::new(RunId([1; 16]), EmployeeId([2; 16]), &limits(), &timeouts(), 8, 1).unwrap();
        for key in ["User", "Group", "RestrictSUIDSGID", "DynamicUser", "SupplementaryGroups"] {
            assert!(!policy.properties.contains_key(key), "{key}");
        }
        // The complete property set PID 1 receives; nothing else was removed or added.
        let mut expected = vec!["PrivateTmp", "PrivateDevices", "PrivateIPC", "ProtectKernelTunables",
            "ProtectKernelModules", "ProtectKernelLogs", "ProtectControlGroups", "LockPersonality",
            "NoNewPrivileges", "MemoryAccounting", "CPUAccounting", "TasksAccounting", "IOAccounting", "Delegate",
            "Type", "ProtectSystem", "OOMPolicy", "ProtectHome", "KillMode", "DevicePolicy",
            "StandardInput", "StandardOutput", "StandardError", "Slice", "UMask",
            "LimitCORE", "LimitCORESoft", "CapabilityBoundingSet", "AmbientCapabilities", "RestrictNamespaces",
            "LimitNOFILE", "LimitNOFILESoft", "MemoryMax", "MemorySwapMax", "CPUQuotaPerSecUSec", "TasksMax",
            "RuntimeMaxUSec", "TimeoutStartUSec", "TimeoutStopUSec", "SystemCallArchitectures",
            "RestrictAddressFamilies", "Environment", "BindsTo", "After", "DeviceAllow",
            "IOReadBandwidthMax", "IOWriteBandwidthMax", "IOReadIOPSMax", "IOWriteIOPSMax"];
        expected.sort_unstable();
        assert_eq!(policy.properties.keys().copied().collect::<Vec<_>>(), expected);
        for key in ["PrivateTmp", "PrivateDevices", "PrivateIPC", "ProtectKernelTunables", "ProtectKernelModules",
            "ProtectKernelLogs", "ProtectControlGroups", "LockPersonality", "NoNewPrivileges", "Delegate"] {
            assert_eq!(policy.properties[key], Property::Bool(true), "{key}");
        }
        assert_eq!(policy.properties["RestrictNamespaces"], Property::U64(TRAMPOLINE_NAMESPACES));
        assert_eq!(policy.properties["AmbientCapabilities"], Property::U64(0));
        assert_eq!(policy.properties["SystemCallArchitectures"], Property::Strings(vec!["native".into()]));
        assert_eq!(policy.properties["RestrictAddressFamilies"], Property::AllowStrings(
            ["AF_UNIX", "AF_INET", "AF_INET6", "AF_NETLINK"].map(String::from).to_vec()));
        assert_eq!(policy.properties["DevicePolicy"], Property::Text("closed".into()));
        assert_eq!(policy.properties["ProtectSystem"], Property::Text("strict".into()));
        assert_eq!(policy.properties["ProtectHome"], Property::Text("tmpfs".into()));
        // ...and each of the three reads back its default exactly once.
        let readback: Vec<_> = policy.readback().collect();
        for (key, value) in [("User", Property::Text(String::new())), ("Group", Property::Text(String::new())),
            ("RestrictSUIDSGID", Property::Bool(false))] {
            let found: Vec<_> = readback.iter().filter(|(name, _)| *name == key).collect();
            assert_eq!(found.len(), 1, "{key}");
            assert_eq!(*found[0].1, value, "{key}");
        }
    }
    const R7G_STATUS: &str = "Name:\tduduclaw-run\nUmask:\t0077\nState:\tS (sleeping)\nTgid:\t4242\nPid:\t4242\n\
        PPid:\t1\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nFDSize:\t64\nGroups:\t\nNoNewPrivs:\t1\nSeccomp:\t2\n\
        CapInh:\t0000000000000000\nCapPrm:\t00000000012011c1\nCapEff:\t00000000012011c1\n\
        CapBnd:\t00000000012011c1\nCapAmb:\t0000000000000000\n";
    #[test]
    fn run_process_readback_requires_root_and_the_full_trampoline_set() {
        assert_eq!(TRAMPOLINE_CAPABILITIES, 0x12011c1);
        assert!(run_process_status_ok(R7G_STATUS).unwrap());
        // Measured on systemd 255 with an explicit User=root: CAP_SETUID missing.
        let lost = R7G_STATUS.replace("CapPrm:\t00000000012011c1", "CapPrm:\t0000000001201141")
            .replace("CapEff:\t00000000012011c1", "CapEff:\t0000000001201141");
        assert!(!run_process_status_ok(&lost).unwrap());
        for (from, to) in [("CapEff:\t00000000012011c1", "CapEff:\t0000000001201141"),
            ("CapPrm:\t00000000012011c1", "CapPrm:\t0000000001201141"),
            ("CapEff:\t00000000012011c1", "CapEff:\t00000000012011c9"),
            ("CapBnd:\t00000000012011c1", "CapBnd:\t000001ffffffffff"),
            ("CapAmb:\t0000000000000000", "CapAmb:\t0000000000000080"),
            ("Uid:\t0\t0\t0\t0", "Uid:\t0\t70000\t0\t0"), ("Uid:\t0\t0\t0\t0", "Uid:\t0\t0\t0\t70000"),
            ("Gid:\t0\t0\t0\t0", "Gid:\t70000\t0\t0\t0"), ("Uid:\t0\t0\t0\t0", "Uid:\t0\t0\t0"),
            ("NoNewPrivs:\t1", "NoNewPrivs:\t0")] {
            assert!(!run_process_status_ok(&R7G_STATUS.replace(from, to)).unwrap(), "{to}");
        }
        // Missing, duplicated or malformed fields refuse outright.
        for key in ["Uid:", "Gid:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:", "NoNewPrivs:"] {
            let missing: String = R7G_STATUS.lines().filter(|line| !line.starts_with(key)).map(|line| format!("{line}\n")).collect();
            assert!(run_process_status_ok(&missing).is_err(), "missing {key}");
            let line = R7G_STATUS.lines().find(|line| line.starts_with(key)).unwrap();
            assert!(run_process_status_ok(&format!("{R7G_STATUS}{line}\n")).is_err(), "duplicate {key}");
        }
        for (from, to) in [("CapEff:\t00000000012011c1", "CapEff:\t12011c1"), ("CapEff:\t00000000012011c1", "CapEff:\t+0000000012011c1")] {
            assert!(run_process_status_ok(&R7G_STATUS.replace(from, to)).is_err(), "{to}");
        }
        assert!(run_process_status_ok("").is_err());
    }
    #[test]
    fn unit_policy_refusals_name_what_failed() {
        let error = invalid_because("dependency BindsTo");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "invalid unit policy: dependency BindsTo");
        assert_eq!(bounded_text("a\tb\nc"), "a?b?c");
        assert_eq!(bounded_text(&"x".repeat(200)), "x".repeat(200));
        // Cut at a character boundary, never past 200 bytes of content.
        let cut = bounded_text(&"é".repeat(150));
        assert_eq!(cut, format!("{}...", "é".repeat(100)));
        // The main-process readback names the first field that is not as required.
        assert_eq!(run_process_status_mismatch(R7G_STATUS).unwrap(), None);
        let lost = R7G_STATUS.replace("CapEff:\t00000000012011c1", "CapEff:\t0000000001201141");
        assert_eq!(run_process_status_mismatch(&lost).unwrap(), Some(("CapEff", "0000000001201141")));
        assert!(!run_process_status_ok(&lost).unwrap());
        let user = R7G_STATUS.replace("Uid:\t0\t0\t0\t0", "Uid:\t0\t70000\t0\t0");
        assert_eq!(run_process_status_mismatch(&user).unwrap().map(|(field, _)| field), Some("Uid"));
        let privs = R7G_STATUS.replace("NoNewPrivs:\t1", "NoNewPrivs:\t0");
        assert_eq!(run_process_status_mismatch(&privs).unwrap(), Some(("NoNewPrivs", "0")));
        for (status, message) in [
            (R7G_STATUS.replace("NoNewPrivs:\t1\n", ""), "invalid unit policy: main process NoNewPrivs missing"),
            (format!("{R7G_STATUS}CapBnd:\t00000000012011c1\n"), "invalid unit policy: main process CapBnd duplicated"),
            (R7G_STATUS.replace("CapEff:\t00000000012011c1", "CapEff:\t12011c1"), "invalid unit policy: main process CapEff malformed"),
        ] {
            let error = run_process_status_mismatch(&status).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), message);
            assert_eq!(run_process_status_ok(&status).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }
    #[test]
    fn run_process_cgroup_must_be_the_units_own() {
        let unit = "/duduclaw.slice/duduclaw-employees.slice/duduclaw-employees-02.slice/duduclaw-run-01.service";
        assert!(run_process_cgroup_ok(&format!("0::{unit}\n"), unit));
        for membership in [format!("0::{unit}/rt-init\n"), format!("0::{unit}"), "0::/\n".into(),
            format!("1:name=systemd:{unit}\n0::{unit}\n"), format!("0::{unit}\n0::{unit}\n"), String::new()] {
            assert!(!run_process_cgroup_ok(&membership, unit), "{membership:?}");
        }
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
