//! Bounded NUL-framed Varlink enumeration and lookups, independent of userdbctl.
use super::{denied, GroupEntry, PasswdEntry};
use duduclaw_isolation::ids::UidRange;
use serde_json::Value;
use std::io;

pub(super) const MAX_HOST_BYTES: u64 = 8 * 1024 * 1024;
#[cfg(target_os = "linux")]
pub(super) const COMMAND_SECONDS: u64 = 5;
const MAX_RECORDS: usize = 65_536;

// Typed fields reject duplicate keys and wrong types, including explicit null.
// Identity records may contain other systemd fields that we do not consume.
fn present<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(de: D) -> Result<Option<T>, D::Error> {
    T::deserialize(de).map(Some)
}
fn present_object<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(de: D) -> Result<Option<T>, D::Error> {
    struct Object<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Object<T> {
        type Value = Option<T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a Varlink object")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Some)
        }
    }
    de.deserialize_map(Object(std::marker::PhantomData))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    #[serde(default)]
    continues: bool,
    #[serde(default, deserialize_with = "present")]
    error: Option<String>,
    #[serde(default, deserialize_with = "present_object")]
    parameters: Option<Parameters>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Parameters {
    #[serde(default, deserialize_with = "present_object")]
    record: Option<Canonical>,
    #[serde(default, deserialize_with = "present")]
    incomplete: Option<bool>,
}
// Preserve the entire record, rejecting duplicate keys at every nesting level.
// A plain Value would silently discard conflicting attributes on deserialization.
struct Canonical(Value);
impl<'de> serde::Deserialize<'de> for Canonical {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Canonical;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("unique-key JSON") }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Canonical, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, Canonical>()? {
                    if object.insert(key, value.0).is_some() { return Err(serde::de::Error::custom("duplicate key")); }
                }
                Ok(Canonical(Value::Object(object)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Canonical, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<Canonical>()? { values.push(value.0); }
                Ok(Canonical(Value::Array(values)))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Canonical, E> { Ok(Canonical(value.into())) }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Canonical, E> { Ok(Canonical(value.into())) }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Canonical, E> { Ok(Canonical(value.into())) }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Canonical, E> { Ok(Canonical(value.into())) }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Canonical, E> {
                serde_json::Number::from_f64(value).map(|n| Canonical(Value::Number(n)))
                    .ok_or_else(|| serde::de::Error::custom("invalid number"))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Canonical, E> { Ok(Canonical(Value::Null)) }
        }
        de.deserialize_any(Visitor)
    }
}

struct Identity {
    provider: String,
    record: Value,
    incomplete: bool,
    user: bool,
    id: u32,
    name: String,
}

fn optional_id(record: &Value, key: &str) -> io::Result<Option<u32>> {
    record.get(key).map(|value| value.as_u64().and_then(|id| u32::try_from(id).ok())
        .ok_or_else(denied)).transpose()
}

fn optional_name<'a>(record: &'a Value, key: &str) -> io::Result<Option<&'a str>> {
    record.get(key).map(|value| value.as_str()
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control)).ok_or_else(denied)).transpose()
}

fn strings<'a>(record: &'a Value, key: &str) -> io::Result<Vec<&'a str>> {
    let Some(value) = record.get(key) else { return Ok(Vec::new()) };
    value.as_array().ok_or_else(denied)?.iter().map(|value| value.as_str()
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control)).ok_or_else(denied)).collect()
}

const NSS: &str = "io.systemd.NameServiceSwitch";

pub(super) struct LocalIdentity<'a> {
    pub ranges: &'a [UidRange],
    pub users: &'a [PasswdEntry],
    pub groups: &'a [GroupEntry],
    pub shadow: &'a str,
}

impl LocalIdentity<'_> {
    fn reserved_id(&self, id: u32) -> bool {
        self.ranges.iter().any(|range| id >= range.first && id <= range.last)
    }

    fn reserved_name(&self, name: &str) -> bool {
        name.starts_with("duduclaw-emp-")
            || self.users.iter().any(|user| user.name == name && self.reserved_id(user.uid))
            || self.groups.iter().any(|group| group.name == name && self.reserved_id(group.gid))
    }

    // Also inspect per-machine/binding objects: an override must not hide a
    // reserved identity behind an innocuous top-level uid or name. User aliases
    // are defined at the top level and in status.*; conservatively inspect them
    // everywhere (including perMachine, binding and group records).
    fn touches_reserved(&self, value: &Value) -> io::Result<bool> {
        match value {
            Value::Object(object) => {
                for key in ["uid", "gid"] {
                    if optional_id(value, key)?.is_some_and(|id| self.reserved_id(id)) { return Ok(true); }
                }
                for key in ["userName", "groupName"] {
                    if optional_name(value, key)?.is_some_and(|name| self.reserved_name(name)) { return Ok(true); }
                }
                for key in ["aliases", "members", "administrators", "memberOf"] {
                    if strings(value, key)?.iter().any(|name| self.reserved_name(name)) { return Ok(true); }
                }
                for child in object.values() { if self.touches_reserved(child)? { return Ok(true); } }
            }
            Value::Array(values) => {
                for child in values { if self.touches_reserved(child)? { return Ok(true); } }
            }
            _ => {},
        }
        Ok(false)
    }

    fn validate(&self, identity: &Identity) -> io::Result<()> {
        let record = &identity.record;
        // Relevance comes from both sides: deleting an employee from an
        // ordinary group's returned members must not bypass comparison.
        let local_mapping = !identity.user && self.groups.iter().any(|group|
            (group.gid == identity.id || group.name == identity.name)
                && group.members.iter().any(|name| name.starts_with("duduclaw-emp-")));
        if !local_mapping && !self.touches_reserved(record)? { return Ok(()); }
        self.validate_echo(identity)
    }

    fn validate_echo(&self, identity: &Identity) -> io::Result<()> {
        let record = &identity.record;
        // Trust the queried socket service, never a self-asserted JSON service.
        // PID1's DynamicUser is allowed to enumerate, but never to own this
        // namespace. All other non-NSS providers follow the same refusal rule.
        if identity.provider != NSS || identity.incomplete { return Err(denied()); }
        validate_nss_metadata(record)?;
        if identity.user {
            let users: Vec<_> = self.users.iter().filter(|user| user.uid == identity.id || user.name == identity.name).collect();
            let [user] = users.as_slice() else { return Err(denied()) };
            if user.uid != identity.id || user.name != identity.name
                || optional_id(record, "gid")? != Some(user.gid)
                || record.get("homeDirectory").and_then(Value::as_str) != Some(user.home.as_str())
                || record.get("shell").and_then(Value::as_str) != Some(user.shell.as_str()) {
                return Err(denied());
            }
            // A protected uid/name must be the canonical employee identity;
            // primary-group collisions by ordinary users are refused as well.
            if !self.reserved_id(user.uid) || user.name != super::account_name(user.uid)
                || user.gid != user.uid { return Err(denied()); }
            let primary: Vec<_> = self.groups.iter().filter(|group| group.gid == user.gid || group.name == user.name).collect();
            if primary.len() != 1 || primary[0].gid != user.gid || primary[0].name != user.name { return Err(denied()); }
            if record.get("memberOf").is_some() {
                let mut actual = strings(record, "memberOf")?;
                let mut expected: Vec<_> = self.groups.iter().filter(|group| group.members.contains(&user.name))
                    .map(|group| group.name.as_str()).collect();
                actual.sort_unstable(); expected.sort_unstable();
                if actual != expected { return Err(denied()); }
            }
            validate_shadow(record, &user.name, self.shadow)?;
        } else {
            let groups: Vec<_> = self.groups.iter().filter(|group| group.gid == identity.id || group.name == identity.name).collect();
            let [group] = groups.as_slice() else { return Err(denied()) };
            if group.gid != identity.id || group.name != identity.name { return Err(denied()); }
            if (self.reserved_id(group.gid) || group.name.starts_with("duduclaw-emp-"))
                && (!self.reserved_id(group.gid) || group.name != super::account_name(group.gid)) { return Err(denied()); }
            let mut actual = strings(record, "members")?;
            let mut expected: Vec<_> = group.members.iter().map(String::as_str).collect();
            actual.sort_unstable(); expected.sort_unstable();
            // /etc/group cannot attest additional gshadow administrators or
            // passwords. Refuse those rather than treating them as an echo.
            if actual != expected || !strings(record, "administrators")?.is_empty()
                || record.get("privileged").is_some() { return Err(denied()); }
        }
        Ok(())
    }
}

fn validate_nss_metadata(record: &Value) -> io::Result<()> {
    // NSS emits flat records, plus privileged hashes and per-machine service
    // attribution. Unknown login fields or overriding sections are not echoes.
    for key in record.as_object().ok_or_else(denied)?.keys() {
        if !matches!(key.as_str(), "uid" | "gid" | "userName" | "groupName" | "homeDirectory" | "shell"
            | "locked" | "notAfterUSec" | "passwordChangeNow" | "lastPasswordChangeUSec"
            | "passwordChangeMinUSec" | "passwordChangeMaxUSec" | "passwordChangeWarnUSec"
            | "passwordChangeInactiveUSec" | "privileged" | "members" | "memberOf" | "administrators"
            | "realName" | "service" | "status") { return Err(denied()); }
    }
    if record.get("service").is_some_and(|service| service.as_str() != Some(NSS)) { return Err(denied()); }
    if let Some(status) = record.get("status") {
        for status in status.as_object().ok_or_else(denied)?.values() {
            let status = status.as_object().ok_or_else(denied)?;
            if status.len() != 1 || status.get("service").and_then(Value::as_str) != Some(NSS) { return Err(denied()); }
        }
    }
    Ok(())
}

fn validate_shadow(record: &Value, name: &str, shadow: &str) -> io::Result<()> {
    let rows: Vec<_> = shadow.lines().map(|line| line.split(':').collect::<Vec<_>>())
        .filter(|fields| fields[0] == name).collect();
    let [fields] = rows.as_slice() else { return Err(denied()) };
    if fields.len() != 9 { return Err(denied()); }
    let day = |index: usize| -> io::Result<i64> {
        if fields[index].is_empty() { return Ok(-1); }
        fields[index].parse::<i64>().ok().filter(|value| *value >= -1).ok_or_else(denied)
    };
    let mut expected = serde_json::Map::new();
    let expiry = day(7)?;
    // systemd v249/v252/v255 map sp_expire 0/1 to locked; a password's
    // !/* prefix alone is not an account lock (passwordless login may exist).
    if expiry >= 0 { expected.insert("locked".into(), (expiry <= 1).into()); }
    let changed = day(2)?;
    if changed >= 0 { expected.insert("passwordChangeNow".into(), (changed == 0).into()); }
    for (key, days, minimum) in [
        ("notAfterUSec", expiry, 2), ("lastPasswordChangeUSec", changed, 1),
        ("passwordChangeMinUSec", day(3)?, 1), ("passwordChangeMaxUSec", day(4)?, 1),
        ("passwordChangeWarnUSec", day(5)?, 1), ("passwordChangeInactiveUSec", day(6)?, 1),
    ] {
        if days >= minimum {
            let usec = (days as u64).checked_mul(86_400_000_000).filter(|value| *value < u64::MAX - 1).ok_or_else(denied)?;
            expected.insert(key.into(), usec.into());
        }
    }
    let password = fields[1];
    if !password.is_empty() && !password.starts_with(['!', '*']) {
        expected.insert("privileged".into(), serde_json::json!({"hashedPassword": [password]}));
    }
    for key in ["locked", "notAfterUSec", "passwordChangeNow", "lastPasswordChangeUSec", "passwordChangeMinUSec",
        "passwordChangeMaxUSec", "passwordChangeWarnUSec", "passwordChangeInactiveUSec", "privileged"] {
        if record.get(key) != expected.get(key) { return Err(denied()); }
    }
    Ok(())
}

struct Replies {
    pending: Vec<u8>,
    records: Vec<Identity>,
    done: bool,
    bytes_left: u64,
    records_left: usize,
}

impl Replies {
    fn new(bytes_left: u64, records_left: usize) -> Self {
        Self { pending: Vec::new(), records: Vec::new(), done: false, bytes_left, records_left }
    }

    fn feed(&mut self, bytes: &[u8], id_key: &str, name_key: &str) -> io::Result<()> {
        self.bytes_left = self.bytes_left.checked_sub(bytes.len() as u64).ok_or_else(denied)?;
        for chunk in bytes.split_inclusive(|byte| *byte == 0) {
            if self.done { return Err(denied()); }
            if chunk.last() != Some(&0) {
                self.pending.extend_from_slice(chunk);
                continue;
            }
            self.pending.extend_from_slice(&chunk[..chunk.len() - 1]);
            if self.pending.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') { return Err(denied()); }
            let reply: Reply = serde_json::from_slice(&self.pending).map_err(|_| denied())?;
            self.pending.clear();
            if let Some(error) = reply.error {
                if error != "io.systemd.UserDatabase.NoRecordFound" || reply.continues
                    || !self.records.is_empty()
                    || reply.parameters.is_some_and(|p| p.record.is_some() || p.incomplete.is_some()) {
                    return Err(denied());
                }
            } else {
                let parameters = reply.parameters.ok_or_else(denied)?;
                let record = parameters.record.ok_or_else(denied)?.0;
                let user = match (id_key, name_key) {
                    ("uid", "userName") => true,
                    ("gid", "groupName") => false,
                    _ => return Err(denied()),
                };
                for key in ["uid", "gid"] { optional_id(&record, key)?; }
                for key in ["userName", "groupName"] { optional_name(&record, key)?; }
                let id = optional_id(&record, id_key)?.ok_or_else(denied)?;
                let name = optional_name(&record, name_key)?.ok_or_else(denied)?.to_owned();
                self.records_left = self.records_left.checked_sub(1).ok_or_else(denied)?;
                self.records.push(Identity { provider: String::new(), record,
                    incomplete: parameters.incomplete.unwrap_or(false), user, id, name });
            }
            self.done = !reply.continues;
        }
        Ok(())
    }

    fn finish(mut self, provider: &str) -> io::Result<Vec<Identity>> {
        if !self.done || !self.pending.is_empty() { return Err(denied()); }
        for record in &mut self.records { record.provider = provider.to_owned(); }
        Ok(self.records)
    }
}

// Never collapse a provider or its security attributes before validation.
fn merge_records(target: &mut std::collections::BTreeSet<(u32, String)>, records: Vec<Identity>,
    local: &LocalIdentity<'_>, deadline: std::time::Instant) -> io::Result<()> {
    for record in records {
        if std::time::Instant::now() >= deadline { return Err(denied()); }
        local.validate(&record)?;
        if std::time::Instant::now() >= deadline { return Err(denied()); }
        target.insert((record.id, record.name));
    }
    Ok(())
}

fn fixed_alias(service: &str, target: &std::path::Path) -> bool {
    matches!(service, "io.systemd.NameServiceSwitch" | "io.systemd.DropIn")
        && target.as_os_str() == "io.systemd.Multiplexer"
}

// The caller supplies even not-yet-created accounts. Also cover local employee
// entries, including legacy names whose numeric IDs lie in a reserved range.
fn lookup_targets(local: &LocalIdentity<'_>, employees: &[(u32, u32)], user: bool,
    deadline: std::time::Instant) -> io::Result<std::collections::BTreeSet<(u32, String)>> {
    let mut targets = std::collections::BTreeSet::new();
    for &(uid, gid) in employees {
        if std::time::Instant::now() >= deadline { return Err(denied()); }
        targets.insert((if user { uid } else { gid }, super::account_name(uid)));
    }
    if user {
        for entry in local.users {
            if std::time::Instant::now() >= deadline { return Err(denied()); }
            if local.reserved_id(entry.uid) || entry.name.starts_with("duduclaw-emp-") {
                targets.insert((entry.uid, entry.name.clone()));
            }
        }
    } else {
        for entry in local.groups {
            if std::time::Instant::now() >= deadline { return Err(denied()); }
            if local.reserved_id(entry.gid) || entry.name.starts_with("duduclaw-emp-") {
                targets.insert((entry.gid, entry.name.clone()));
            }
        }
    }
    Ok(targets)
}

// Enumeration and every targeted lookup share one deadline and reply budget.
// A targeted hit must be an exact local NSS echo even if its returned record
// omits every reserved attribute (e.g. a provider synthesizing aliases on demand).
fn provider_records(service: &str, local: &LocalIdentity<'_>, employees: &[(u32, u32)],
    deadline: std::time::Instant, budget: &mut (u64, usize),
    mut query: impl FnMut(&str, &str, &str, Value, &mut Replies) -> io::Result<()>)
    -> io::Result<Vec<Vec<(u32, String)>>> {
    let mut results = Vec::new();
    for (user, method, id_key, name_key) in [
        (true, "GetUserRecord", "uid", "userName"),
        (false, "GetGroupRecord", "gid", "groupName"),
    ] {
        let mut merged = std::collections::BTreeSet::new();
        let mut run = |selector: Option<(&str, Value)>| -> io::Result<()> {
            if std::time::Instant::now() >= deadline { return Err(denied()); }
            let mut parameters = serde_json::json!({"service": service});
            if let Some((key, value)) = &selector { parameters[*key] = value.clone(); }
            let mut parser = Replies::new(budget.0, budget.1);
            query(method, id_key, name_key, parameters, &mut parser)?;
            *budget = (parser.bytes_left, parser.records_left);
            let records = parser.finish(service)?;
            if let Some((key, value)) = selector {
                if records.len() > 1 { return Err(denied()); }
                for record in &records {
                    if std::time::Instant::now() >= deadline
                        || (key == id_key && Value::from(record.id) != value)
                        || (key == name_key && Value::from(record.name.clone()) != value) {
                        return Err(denied());
                    }
                    local.validate_echo(record)?;
                }
            }
            merge_records(&mut merged, records, local, deadline)
        };
        run(None)?;
        let targets = lookup_targets(local, employees, user, deadline)?;
        for name in targets.iter().map(|(_, name)| name).collect::<std::collections::BTreeSet<_>>() {
            run(Some((name_key, Value::from(name.clone()))))?;
        }
        for id in targets.iter().map(|(id, _)| *id).collect::<std::collections::BTreeSet<_>>() {
            run(Some((id_key, Value::from(id))))?;
        }
        results.push(merged.into_iter().collect());
    }
    Ok(results)
}

#[cfg(target_os = "linux")]
pub(super) fn identities(directory: &std::path::Path, local: &LocalIdentity<'_>,
    employees: &[(u32, u32)]) -> io::Result<Vec<Vec<(u32, String)>>> {
    use std::{collections::BTreeSet, fs, os::unix::fs::{FileTypeExt, MetadataExt}, time::{Duration, Instant}};
    let deadline = Instant::now() + Duration::from_secs(COMMAND_SECONDS);
    let mut budget = (MAX_HOST_BYTES, MAX_RECORDS);
    let mut users = BTreeSet::new();
    let mut groups = BTreeSet::new();
    let mut completed = 0;
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        if index >= 256 || Instant::now() >= deadline { return Err(denied()); }
        let path = entry?.path();
        let service = path.file_name().and_then(|s| s.to_str()).ok_or_else(denied)?;
        let mut meta = fs::symlink_metadata(&path)?;
        if meta.uid() != 0 { return Err(denied()); }
        // userdbd's two fixed aliases select separate NSS/drop-in backends.
        // Query them by service name, but never follow an arbitrary symlink.
        let endpoint = if meta.file_type().is_symlink() {
            if !fixed_alias(service, &fs::read_link(&path)?) { return Err(denied()); }
            let endpoint = directory.join("io.systemd.Multiplexer");
            meta = fs::symlink_metadata(&endpoint)?;
            endpoint
        } else { path.clone() };
        // Socket write permission allows clients to connect; unlike directories,
        // a mode 0666 socket does not let them replace the trusted endpoint.
        if meta.uid() != 0 || !meta.file_type().is_socket() { return Err(denied()); }
        if service == "io.systemd.Multiplexer" { continue; }
        let mut records = provider_records(service, local, employees, deadline, &mut budget,
            |method, id, name, parameters, parser|
                query(&endpoint, method, id, name, parameters, deadline, parser))?.into_iter();
        users.extend(records.next().ok_or_else(denied)?);
        groups.extend(records.next().ok_or_else(denied)?);
        completed += 1;
    }
    require_complete_provider(completed)?;
    if Instant::now() >= deadline { return Err(denied()); }
    Ok(vec![users.into_iter().collect(), groups.into_iter().collect()])
}

fn require_complete_provider(completed: usize) -> io::Result<()> {
    if completed == 0 { return Err(denied()); }
    Ok(())
}

#[cfg(target_os = "linux")]
fn query(path: &std::path::Path, method: &str, id: &str, name: &str, parameters: Value,
    deadline: std::time::Instant, parser: &mut Replies) -> io::Result<()> {
    use std::{io::{Read, Write}, os::{fd::AsRawFd, unix::net::UnixStream}, time::{Duration, Instant}};
    use nix::sys::socket::{self, AddressFamily, SockFlag, SockType, UnixAddr, sockopt};
    if Instant::now() >= deadline { return Err(denied()); }
    let fd = socket::socket(AddressFamily::Unix, SockType::Stream,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None)?;
    // AF_UNIX nonblocking connect fails immediately when its backlog is full.
    // Any connect error denies enumeration instead of risking an unbounded wait.
    socket::connect(fd.as_raw_fd(), &UnixAddr::new(path)?)?;
    if socket::getsockopt(&fd, sockopt::PeerCredentials)?.uid() != 0 { return Err(denied()); }
    let mut stream = UnixStream::from(fd);
    let mut request = serde_json::to_vec(&serde_json::json!({
        "method": format!("io.systemd.UserDatabase.{method}"),
        "parameters": parameters, "more": true,
    })).map_err(|_| denied())?;
    request.push(0);
    let mut written = 0;
    loop {
        if Instant::now() >= deadline { return Err(denied()); }
        if written < request.len() {
            match stream.write(&request[written..]) {
                Ok(0) => return Err(denied()),
                Ok(count) => written += count,
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
                Err(error) => return Err(error),
            }
        } else {
            let mut buffer = [0; 8192];
            match stream.read(&mut buffer) {
                Ok(0) => return Err(denied()),
                Ok(count) => {
                    parser.feed(&buffer[..count], id, name)?;
                    if Instant::now() >= deadline { return Err(denied()); }
                    if parser.done { return Ok(()); }
                    continue;
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
                Err(error) => return Err(error),
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> io::Result<Vec<(u32, String)>> {
        let mut replies = Replies::new(MAX_HOST_BYTES, MAX_RECORDS);
        replies.feed(bytes, "uid", "userName")?;
        Ok(pairs(replies.finish(NSS)?))
    }

    fn pairs(records: Vec<Identity>) -> Vec<(u32, String)> {
        records.into_iter().map(|record| (record.id, record.name)).collect()
    }

    #[test]
    fn multi_record_replies_allow_fragmentation_and_explicit_final_false() {
        let bytes = b"{\"parameters\":{\"record\":{\"uid\":1,\"userName\":\"a\"}},\"continues\":true}\0{\"parameters\":{\"record\":{\"uid\":2,\"userName\":\"b\"}},\"continues\":false}\0";
        for chunk_size in [1, 7, bytes.len()] {
            let mut replies = Replies::new(MAX_HOST_BYTES, MAX_RECORDS);
            for chunk in bytes.chunks(chunk_size) { replies.feed(chunk, "uid", "userName").unwrap(); }
            assert_eq!(pairs(replies.finish(NSS).unwrap()), vec![(1, "a".into()), (2, "b".into())]);
        }
        let mut groups = Replies::new(MAX_HOST_BYTES, MAX_RECORDS);
        groups.feed(b"{\"parameters\":{\"record\":{\"gid\":7,\"groupName\":\"g\"}}}\0", "gid", "groupName").unwrap();
        assert_eq!(pairs(groups.finish(NSS).unwrap()), vec![(7, "g".into())]);
    }

    #[test]
    fn no_record_found_is_the_only_empty_success() {
        assert!(parse(b"{\"error\":\"io.systemd.UserDatabase.NoRecordFound\"}\0").unwrap().is_empty());
        assert!(parse(b"{\"error\":\"io.systemd.UserDatabase.NoRecordFound\",\"parameters\":{}}\0").unwrap().is_empty());
        for bytes in [b"{\"error\":\"io.systemd.UserDatabase.ServiceNotAvailable\"}\0".as_slice(),
            b"{\"error\":\"io.systemd.UserDatabase.NoRecordFound\",\"continues\":true}\0",
            b"{\"parameters\":{}}\0", b"null\0", b"{}\0", b"{bad}\0",
            b"{\"parameters\":{\"record\":{\"uid\":-1,\"userName\":\"a\"}}}\0",
            b"{\"parameters\":{\"record\":{\"uid\":1,\"userName\":\"a\"}},\"continues\":null}\0"] {
            assert!(parse(bytes).is_err());
        }
    }

    #[test]
    fn truncated_and_unexpected_trailing_replies_are_denied() {
        for bytes in [b"".as_slice(), b"{\"error\":\"io.systemd.UserDatabase.NoRecordFound\"}",
            b"{\"parameters\":{\"record\":{\"uid\":1,\"userName\":\"a\"}},\"continues\":true}\0",
            b"{\"error\":\"io.systemd.UserDatabase.NoRecordFound\"}\0{}\0"] {
            assert!(parse(bytes).is_err());
        }
    }

    #[test]
    fn duplicate_critical_fields_and_null_errors_are_denied() {
        for bytes in [
            b"{\"error\":null,\"parameters\":{\"record\":{\"uid\":1,\"userName\":\"a\"}}}\0".as_slice(),
            b"{\"error\":\"bad\",\"error\":\"io.systemd.UserDatabase.NoRecordFound\"}\0",
            b"{\"continues\":true,\"continues\":false,\"parameters\":{\"record\":{\"uid\":1,\"userName\":\"a\"}}}\0",
            b"{\"parameters\":{\"record\":{\"uid\":1,\"uid\":2,\"userName\":\"a\"}}}\0",
            b"{\"parameters\":{\"record\":{\"uid\":1,\"userName\":\"a\",\"userName\":\"b\"}}}\0",
            b"[false,\"io.systemd.UserDatabase.NoRecordFound\",{}]\0",
            b"{\"parameters\":[{\"uid\":1,\"userName\":\"a\"}]}\0",
            b"{\"parameters\":{\"record\":[1,1,\"a\",\"a\"]}}\0",
        ] { assert!(parse(bytes).is_err()); }
    }

    #[test]
    fn byte_and_record_budgets_apply_across_frames() {
        assert!(parse(&vec![b' '; MAX_HOST_BYTES as usize + 1]).is_err());
        let frame = b"{\"parameters\":{\"record\":{\"uid\":1,\"userName\":\"a\"}},\"continues\":true}\0";
        let mut replies = Replies::new(frame.len() as u64 * 2 - 1, MAX_RECORDS);
        replies.feed(frame, "uid", "userName").unwrap();
        assert!(replies.feed(frame, "uid", "userName").is_err());
        let mut replies = Replies::new(MAX_HOST_BYTES, 1);
        replies.feed(frame, "uid", "userName").unwrap();
        assert!(replies.feed(frame, "uid", "userName").is_err());
    }

    #[test]
    fn duplicate_nested_security_attributes_are_denied() {
        for record in [
            r#"{"uid":70000,"userName":"duduclaw-emp-70000","locked":true,"locked":false}"#,
            r#"{"uid":70000,"userName":"duduclaw-emp-70000","privileged":{"hashedPassword":[],"hashedPassword":["evil"]}}"#,
            r#"{"uid":70000,"userName":"duduclaw-emp-70000","status":{"machine":{"service":"a","service":"b"}}}"#,
        ] {
            assert!(parse(format!("{{\"parameters\":{{\"record\":{record}}}}}\0").as_bytes()).is_err());
        }
    }

    fn employee_record() -> Value {
        serde_json::json!({"uid":70000,"gid":70000,"userName":"duduclaw-emp-70000",
            "homeDirectory":"/nonexistent","shell":"/usr/sbin/nologin","locked":true,
            "status":{"machine":{"service":NSS}}})
    }

    fn identity(provider: &str, record: Value, user: bool) -> Identity {
        let mut replies = Replies::new(MAX_HOST_BYTES, MAX_RECORDS);
        let mut bytes = serde_json::to_vec(&serde_json::json!({"parameters":{"record":record}})).unwrap();
        bytes.push(0);
        let (id, name) = if user { ("uid", "userName") } else { ("gid", "groupName") };
        replies.feed(&bytes, id, name).unwrap();
        replies.finish(provider).unwrap().pop().unwrap()
    }

    fn with_local(check: impl FnOnce(&LocalIdentity<'_>)) {
        let users = super::super::parse_passwd("duduclaw-emp-70000:x:70000:70000::/nonexistent:/usr/sbin/nologin\n").unwrap();
        let groups = super::super::parse_group("duduclaw-emp-70000:x:70000:\nwheel:x:10:\n").unwrap();
        check(&LocalIdentity { ranges: &[UidRange { first:70000,last:99999 }], users: &users, groups: &groups,
            shadow: "duduclaw-emp-70000:!*::::::1:\n" });
    }

    // Exercise the production provider loop with a request-sensitive fixture,
    // feeding its replies through the same canonical framed parser as sockets.
    fn fixture_provider(service: &str, local: &LocalIdentity<'_>, employees: &[(u32, u32)],
        budget: &mut (u64, usize), mut reply: impl FnMut(&str, &Value) -> Vec<u8>)
        -> io::Result<Vec<Vec<(u32, String)>>> {
        provider_records(service, local, employees,
            std::time::Instant::now() + std::time::Duration::from_secs(5), budget,
            |method, id, name, parameters, parser| {
                assert_eq!(parameters["service"], service);
                let bytes = reply(method, &parameters);
                for chunk in bytes.chunks(7) { parser.feed(chunk, id, name)?; }
                Ok(())
            })
    }

    fn fixture_reply(record: Option<Value>) -> Vec<u8> {
        let reply = match record {
            Some(record) => serde_json::json!({"parameters":{"record":record}}),
            None => serde_json::json!({"error":"io.systemd.UserDatabase.NoRecordFound"}),
        };
        let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(0); bytes
    }

    #[test]
    fn lookup_only_aliases_and_numeric_hits_are_refused() {
        with_local(|local| {
            for user in [true, false] {
                for numeric in [false, true] {
                    for provider in [NSS, "io.systemd.DynamicUser", "org.example.Provider"] {
                        let mut hit = false;
                        let result = fixture_provider(provider, local, &[(70000, 70000)],
                            &mut (MAX_HOST_BYTES, MAX_RECORDS), |method, parameters| {
                                let selected = method == if user { "GetUserRecord" } else { "GetGroupRecord" };
                                let key = if numeric { if user { "uid" } else { "gid" } }
                                    else if user { "userName" } else { "groupName" };
                                let mut record = if method == "GetUserRecord" { serde_json::json!({"uid":1000,"userName":"ordinary"}) }
                                    else { serde_json::json!({"gid":1000,"groupName":"ordinary"}) };
                                if selected && parameters.get(key).is_some() {
                                    hit = true;
                                    if numeric { record[if user { "uid" } else { "gid" }] = 70000.into(); }
                                    else { record["status"] = serde_json::json!({"machine":{"aliases":["duduclaw-emp-70000"]}}); }
                                    fixture_reply(Some(record))
                                } else if parameters.as_object().unwrap().len() == 1 {
                                    // Enumeration reveals an ordinary, alias-free identity.
                                    fixture_reply(Some(record))
                                } else { fixture_reply(None) }
                            });
                        assert!(hit, "{provider} user={user} numeric={numeric}");
                        assert!(result.is_err());
                    }
                }
            }
        });
    }

    #[test]
    fn dynamic_only_no_record_and_exact_nss_lookup_echoes_succeed() {
        with_local(|local| {
            for provider in [NSS, "io.systemd.DynamicUser"] {
                let mut requests = Vec::new();
                let result = fixture_provider(provider, local, &[(70000, 70000), (80000, 80000)],
                    &mut (MAX_HOST_BYTES, MAX_RECORDS), |method, parameters| {
                        requests.push((method.to_owned(), parameters.clone()));
                        let existing = parameters.get("uid").or(parameters.get("gid")) == Some(&70000.into())
                            || parameters.get("userName").or(parameters.get("groupName"))
                                == Some(&"duduclaw-emp-70000".into());
                        let record = if method == "GetUserRecord" { employee_record() }
                            else { serde_json::json!({"gid":70000,"groupName":"duduclaw-emp-70000"}) };
                        fixture_reply((provider == NSS && existing).then_some(record))
                    });
                assert!(result.is_ok());
                assert_eq!(requests.len(), 10);
                for (method, name, id) in [("GetUserRecord", "userName", "uid"), ("GetGroupRecord", "groupName", "gid")] {
                    for number in [70000, 80000] {
                        assert!(requests.iter().any(|(m, p)| m == method && p[name] == super::super::account_name(number)));
                        assert!(requests.iter().any(|(m, p)| m == method && p[id] == number));
                    }
                }
            }
        });
    }

    #[test]
    fn targets_cover_new_accounts_and_local_legacy_names_with_deadline() {
        with_local(|local| {
            let users = super::super::parse_passwd("legacy-user:x:70001:70002::/nonexistent:/usr/sbin/nologin\n").unwrap();
            let groups = super::super::parse_group("legacy-group:x:70002:\n").unwrap();
            let local = LocalIdentity { users: &users, groups: &groups, ..*local };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let users = lookup_targets(&local, &[(80000,80001)], true, deadline).unwrap();
            let groups = lookup_targets(&local, &[(80000,80001)], false, deadline).unwrap();
            assert!(users.contains(&(70001, "legacy-user".into())));
            assert!(users.contains(&(80000, "duduclaw-emp-80000".into())));
            assert!(groups.contains(&(70002, "legacy-group".into())));
            assert!(groups.contains(&(80001, "duduclaw-emp-80000".into())));
            assert!(lookup_targets(&local, &[], true, std::time::Instant::now()).is_err());
            assert!(lookup_targets(&local, &[], false, std::time::Instant::now()).is_err());
        });
    }

    #[test]
    fn lookups_share_budgets_and_refuse_errors_malformed_or_unrelated_echoes() {
        with_local(|local| {
            for bad in [b"{bad}\0".to_vec(),
                b"{\"error\":\"io.systemd.UserDatabase.ServiceNotAvailable\"}\0".to_vec(),
                fixture_reply(Some(serde_json::json!({"uid":1000,"userName":"ordinary"}))),
                b"{\"parameters\":{\"record\":{\"uid\":70000,\"userName\":\"ordinary\",\"uid\":1}}}\0".to_vec()] {
                assert!(fixture_provider(NSS, local, &[(70000,70000)], &mut (MAX_HOST_BYTES, MAX_RECORDS),
                    |_, p| if p.as_object().unwrap().len() == 1 { fixture_reply(None) } else { bad.clone() }).is_err());
            }
            let empty = fixture_reply(None);
            let mut budget = (empty.len() as u64 * 2 - 1, MAX_RECORDS);
            assert!(fixture_provider(NSS, local, &[], &mut budget, |_, _| empty.clone()).is_err());
            let mut budget = (MAX_HOST_BYTES, 1);
            assert!(fixture_provider(NSS, local, &[], &mut budget,
                |method, _| if method == "GetUserRecord" { fixture_reply(Some(employee_record())) }
                    else { fixture_reply(None) }).is_err());
            let mut called = false;
            assert!(provider_records(NSS, local, &[], std::time::Instant::now(),
                &mut (MAX_HOST_BYTES, MAX_RECORDS), |_, _, _, _, _| { called = true; Ok(()) }).is_err());
            assert!(!called);
        });
    }

    #[test]
    fn same_pair_from_another_provider_is_refused_before_dedupe() {
        with_local(|local| {
            for provider in ["io.systemd.DropIn", "io.systemd.DynamicUser", "org.example.Provider"] {
                for mutate in [false, true] {
                    let mut other = employee_record();
                    if mutate { other["shell"] = "/bin/sh".into(); }
                    // A forged service field cannot override socket attribution.
                    other["service"] = NSS.into();
                    let records = vec![identity(NSS, employee_record(), true), identity(provider, other, true)];
                    assert!(merge_records(&mut Default::default(), records, local,
                        std::time::Instant::now() + std::time::Duration::from_secs(5)).is_err());
                }
            }
            let records = vec![identity(NSS, employee_record(), true), identity(NSS, employee_record(), true)];
            let mut merged = Default::default();
            merge_records(&mut merged, records, local,
                std::time::Instant::now() + std::time::Duration::from_secs(5)).unwrap();
            assert_eq!(merged.len(), 1);
        });
    }

    #[test]
    fn nss_same_pair_with_different_login_attributes_is_refused() {
        with_local(|local| {
            for (key, value) in [
                ("gid", serde_json::json!(10)), ("homeDirectory", "/root".into()),
                ("shell", "/bin/sh".into()), ("locked", false.into()), ("locked", Value::Null),
                ("notAfterUSec", 0.into()), ("memberOf", serde_json::json!(["wheel"])),
                ("privileged", serde_json::json!({"hashedPassword":["$6$foreign"]})),
                ("binding", serde_json::json!({"machine":{"uid":1}})),
                ("perMachine", serde_json::json!([{"shell":"/bin/sh"}])),
                ("service", "org.example.Provider".into()),
                ("status", serde_json::json!({"machine":{"service":"org.example.Provider"}})),
            ] {
                let mut changed = employee_record(); changed[key] = value;
                let records = vec![identity(NSS, employee_record(), true), identity(NSS, changed, true)];
                assert!(merge_records(&mut Default::default(), records, local,
                    std::time::Instant::now() + std::time::Duration::from_secs(5)).is_err(), "{key}");
            }
            let mut missing = employee_record(); missing.as_object_mut().unwrap().remove("locked");
            assert!(local.validate(&identity(NSS, missing, true)).is_err());
            let mut incomplete = identity(NSS, employee_record(), true); incomplete.incomplete = true;
            assert!(local.validate(&incomplete).is_err());
        });
    }

    #[test]
    fn reserved_names_outside_range_and_primary_gid_collisions_are_refused() {
        with_local(|local| {
            for provider in [NSS, "io.systemd.DynamicUser", "io.systemd.DropIn"] {
                for record in [
                    serde_json::json!({"uid":1000,"gid":1000,"userName":"duduclaw-emp-70000"}),
                    serde_json::json!({"uid":1000,"gid":70000,"userName":"ordinary"}),
                    serde_json::json!({"uid":1000,"gid":1000,"userName":"ordinary",
                        "perMachine":[{"uid":70000}]}),
                ] { assert!(local.validate(&identity(provider, record, true)).is_err()); }
                assert!(local.validate(&identity(provider,
                    serde_json::json!({"gid":1000,"groupName":"duduclaw-emp-70000"}), false)).is_err());
            }
        });
    }

    fn with_aliases(mut record: Value, section: &str, aliases: Value) -> Value {
        match section {
            "regular" => record["aliases"] = aliases,
            "status" => record["status"]["machine"]["aliases"] = aliases,
            "binding" => record["binding"] = serde_json::json!({"machine":{"aliases":aliases}}),
            "perMachine" => record["perMachine"] = serde_json::json!([{"aliases":aliases}]),
            _ => panic!("unknown alias section"),
        }
        record
    }

    #[test]
    fn reserved_aliases_are_refused_before_identity_dedupe() {
        with_local(|local| {
            for user in [true, false] {
                let ordinary = if user { serde_json::json!({"uid":1000,"userName":"ordinary"}) }
                    else { serde_json::json!({"gid":1000,"groupName":"ordinary"}) };
                for provider in [NSS, "io.systemd.DropIn", "io.systemd.DynamicUser", "org.example.Provider"] {
                    for section in ["regular", "status", "perMachine", "binding"] {
                        for alias in ["duduclaw-emp-70000", "duduclaw-emp-80000"] {
                            let record = with_aliases(ordinary.clone(), section, serde_json::json!([alias]));
                            // The same ordinary identity already exists in the
                            // result: its alias must be checked before dedupe.
                            let mut merged = std::collections::BTreeSet::from([(1000, "ordinary".into())]);
                            assert!(merge_records(&mut merged, vec![identity(provider, record, user)], local,
                                std::time::Instant::now() + std::time::Duration::from_secs(5)).is_err(),
                                "{provider} {section} {alias} user={user}");
                        }
                    }
                }
            }
        });
    }

    #[test]
    fn aliases_matching_local_reserved_id_names_are_refused() {
        with_local(|local| {
            let users = super::super::parse_passwd("legacy-employee:x:70001:70001::/nonexistent:/usr/sbin/nologin\n").unwrap();
            let groups = super::super::parse_group("legacy-group:x:70001:\n").unwrap();
            let local = LocalIdentity { users: &users, groups: &groups, ..*local };
            for alias in ["legacy-employee", "legacy-group"] {
                let record = serde_json::json!({"uid":1000,"userName":"ordinary","aliases":[alias]});
                assert!(local.validate(&identity("org.example.Provider", record, true)).is_err());
            }
        });
    }

    #[test]
    fn malformed_aliases_are_refused_even_for_unreserved_records() {
        with_local(|local| {
            for user in [true, false] {
                let ordinary = if user { serde_json::json!({"uid":1000,"userName":"ordinary"}) }
                    else { serde_json::json!({"gid":1000,"groupName":"ordinary"}) };
                for section in ["regular", "status", "perMachine", "binding"] {
                    for aliases in [Value::Null, "duduclaw-emp-70000".into(), 7.into(),
                        serde_json::json!({"name":"duduclaw-emp-70000"}),
                        serde_json::json!(["ordinary-alias", null]), serde_json::json!([7])] {
                        let record = with_aliases(ordinary.clone(), section, aliases);
                        for provider in [NSS, "org.example.Provider"] {
                            assert!(local.validate(&identity(provider, record.clone(), user)).is_err(),
                                "{provider} {section} user={user}: {record}");
                        }
                    }
                    let record = with_aliases(ordinary.clone(), section, serde_json::json!(["ordinary-alias"]));
                    assert!(local.validate(&identity("org.example.Provider", record, user)).is_ok());
                }
            }
        });
    }

    #[test]
    fn nss_employee_echo_cannot_carry_any_alias_field() {
        with_local(|local| {
            for user in [true, false] {
                let record = if user { employee_record() }
                    else { serde_json::json!({"gid":70000,"groupName":"duduclaw-emp-70000"}) };
                assert!(local.validate(&identity(NSS, record.clone(), user)).is_ok());
                for section in ["regular", "status", "perMachine", "binding"] {
                    for aliases in [serde_json::json!([]), serde_json::json!(["ordinary-alias"]),
                        serde_json::json!(["duduclaw-emp-70000"])] {
                        let changed = with_aliases(record.clone(), section, aliases);
                        assert!(local.validate(&identity(NSS, changed, user)).is_err(), "{section} user={user}");
                    }
                }
            }
        });
    }

    #[test]
    fn group_echo_members_and_supplementary_mapping_are_checked() {
        with_local(|local| {
            let group = serde_json::json!({"gid":70000,"groupName":"duduclaw-emp-70000"});
            assert!(local.validate(&identity(NSS, group.clone(), false)).is_ok());
            for provider in ["io.systemd.DynamicUser", "io.systemd.DropIn"] {
                assert!(local.validate(&identity(provider, group.clone(), false)).is_err());
            }
            for (key, value) in [("members", serde_json::json!(["foreign"])),
                ("administrators", serde_json::json!(["foreign"]))] {
                let mut changed = group.clone(); changed[key] = value;
                assert!(local.validate(&identity(NSS, changed, false)).is_err());
            }
            // Membership in an unreserved group is still a protected mapping.
            let wheel = serde_json::json!({"gid":10,"groupName":"wheel","members":["duduclaw-emp-70000"]});
            for provider in [NSS, "io.systemd.DropIn"] {
                assert!(local.validate(&identity(provider, wheel.clone(), false)).is_err());
            }
            let groups = super::super::parse_group("duduclaw-emp-70000:x:70000:\nwheel:x:10:duduclaw-emp-70000\n").unwrap();
            let local = LocalIdentity { groups: &groups, ..*local };
            assert!(local.validate(&identity(NSS, wheel, false)).is_ok());
            for missing in [serde_json::json!({"gid":10,"groupName":"wheel"}),
                serde_json::json!({"gid":10,"groupName":"wheel","members":[]})] {
                assert!(local.validate(&identity(NSS, missing, false)).is_err());
            }
        });
    }

    #[test]
    fn nss_echo_of_unlocked_half_created_account_allows_recovery() {
        let users = super::super::parse_passwd("duduclaw-emp-70000:x:70000:70000::/nonexistent:/bin/sh\n").unwrap();
        with_local(|local| {
            let local = LocalIdentity { users: &users, shadow:"duduclaw-emp-70000:!:2:0:99999:7:::\n", ..*local };
            let mut record = employee_record();
            record.as_object_mut().unwrap().remove("locked");
            record["shell"] = "/bin/sh".into();
            record["passwordChangeNow"] = false.into();
            record["lastPasswordChangeUSec"] = (2_u64 * 86_400_000_000).into();
            record["passwordChangeMaxUSec"] = (99999_u64 * 86_400_000_000).into();
            record["passwordChangeWarnUSec"] = (7_u64 * 86_400_000_000).into();
            assert!(local.validate(&identity(NSS, record, true)).is_ok());
            let group_only = LocalIdentity { users: &[], shadow:"", ..local };
            assert!(group_only.validate(&identity(NSS,
                serde_json::json!({"gid":70000,"groupName":"duduclaw-emp-70000"}), false)).is_ok());
            assert!(group_only.validate(&identity(NSS, employee_record(), true)).is_err());
        });
    }

    #[test]
    fn canonical_record_and_socket_provider_survive_parsing() {
        let record = employee_record();
        let parsed = identity("org.example.Provider", record.clone(), true);
        assert_eq!(parsed.record, record);
        assert_eq!(parsed.provider, "org.example.Provider");
        with_local(|local| {
            let dynamic = serde_json::json!({"uid":61234,"gid":61234,"userName":"unrelated"});
            assert!(local.validate(&identity("io.systemd.DynamicUser", dynamic, true)).is_ok());
        });
    }

    #[test]
    fn existing_directory_requires_at_least_one_complete_provider() {
        assert!(require_complete_provider(0).is_err());
        assert!(require_complete_provider(1).is_ok());
        assert!(require_complete_provider(2).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires root Linux for trusted Unix socket peers; creates temporary fixture sockets only"]
    fn iso_root_userdb_provider_completion_and_multiplexer_only() {
        use std::{fs, io::{Read, Write}, os::unix::{fs::MetadataExt, net::UnixListener}, time::{Duration, Instant}};
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                for name in ["io.systemd.Multiplexer", "io.systemd.DynamicUser"] {
                    let _ = fs::remove_file(self.0.join(name));
                }
                let _ = fs::remove_dir(&self.0);
            }
        }
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let fixture = Fixture(std::env::temp_dir().join(format!("spawnd-userdb-{}-{nonce}", std::process::id())));
        fs::create_dir(&fixture.0).unwrap();
        assert_eq!(fs::metadata(&fixture.0).unwrap().uid(), 0);
        let local = LocalIdentity { ranges: &[UidRange { first:70000,last:99999 }], users:&[],groups:&[],shadow:"" };
        assert!(identities(&fixture.0, &local, &[]).is_err());
        let multiplexer = UnixListener::bind(fixture.0.join("io.systemd.Multiplexer")).unwrap();
        assert!(identities(&fixture.0, &local, &[]).is_err());
        drop(multiplexer);
        fs::remove_file(fixture.0.join("io.systemd.Multiplexer")).unwrap();
        for interrupt_groups in [false, true] {
            let listener = UnixListener::bind(fixture.0.join("io.systemd.DynamicUser")).unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                for method in ["GetUserRecord", "GetGroupRecord"] {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            Err(error) => panic!("fixture accept: {error}"),
                        }
                    };
                    stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                    let mut request = Vec::new();
                    loop {
                        let mut byte = [0]; stream.read_exact(&mut byte).unwrap();
                        if byte[0] == 0 { break; }
                        request.push(byte[0]); assert!(request.len() <= 1024);
                    }
                    let request: Value = serde_json::from_slice(&request).unwrap();
                    assert_eq!(request["method"], format!("io.systemd.UserDatabase.{method}"));
                    assert_eq!(request["parameters"]["service"], "io.systemd.DynamicUser");
                    if interrupt_groups && method == "GetGroupRecord" { continue; }
                    stream.write_all(b"{\"error\":\"io.systemd.UserDatabase.NoRecordFound\"}\0").unwrap();
                }
            });
            let result = identities(&fixture.0, &local, &[]);
            server.join().unwrap();
            if interrupt_groups { assert!(result.is_err()); }
            else { assert_eq!(result.unwrap(), vec![Vec::new(), Vec::new()]); }
            fs::remove_file(fixture.0.join("io.systemd.DynamicUser")).unwrap();
        }
    }

    #[test]
    fn userdbd_aliases_cannot_escape_or_redirect_to_another_service() {
        use std::path::Path;
        for service in ["io.systemd.NameServiceSwitch", "io.systemd.DropIn"] {
            assert!(fixed_alias(service, Path::new("io.systemd.Multiplexer")));
            for target in ["../io.systemd.Multiplexer", "./io.systemd.Multiplexer", "io.systemd.Multiplexer/",
                "/run/systemd/userdb/io.systemd.Multiplexer", "io.systemd.DynamicUser"] {
                assert!(!fixed_alias(service, Path::new(target)));
            }
        }
        assert!(!fixed_alias("io.systemd.DynamicUser", Path::new("io.systemd.Multiplexer")));
    }
}
