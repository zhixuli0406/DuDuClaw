//! Parsers for the "is the OS really set up for maintenance SSH" checks that
//! `ssh_maint_open` runs before it writes anything (design §4.4).
//!
//! Each check reads what is in effect (systemd's merged unit, the loaded
//! nftables ruleset), never the files that should have produced it, and
//! compares by token or JSON field, never by substring:
//! `/run/duduclaw-maint-ssh` contains `-s`, so "the ExecStart line contains
//! `-s`" proves nothing.

use std::collections::BTreeSet;
use std::net::IpAddr;

use serde_json::Value;

/// The dropbear binary the merged `dropbear@.service` must run.
pub const DROPBEAR_PATH: &str = "/usr/sbin/dropbear";

/// The exact argv of the merged `ExecStart=` (OS drop-in
/// `dropbear@.service.d/60-duduclaw-maint.conf`). Any extra, missing or
/// reordered token refuses the window.
pub const EXPECTED_DROPBEAR_ARGV: [&str; 19] = [
    "/usr/sbin/dropbear",
    "-i",
    "-r",
    "/data/system/dropbear/dropbear_ed25519_host_key",
    "-w",
    "-s",
    "-j",
    "-k",
    "-m",
    "-T",
    "3",
    "-I",
    "1800",
    "-G",
    "duduclaw-maint",
    "-D",
    "/run/duduclaw-maint-ssh",
    "-c",
    "/usr/libexec/duduclaw/maint-shell",
];
// `argv[]=` in `systemctl show` output is space-joined, so a token holding a
// space cannot be told from two tokens; none of the expected tokens has one,
// and the drop-in that sets them is root-owned on a read-only root.

/// The unit both dropbear units must be bound to.
pub const SYSD_UNIT: &str = "duduclaw-sysd.service";

/// nftables objects the OS firewall must have loaded.
pub const OUTPUT_CHAIN: &str = "output";
pub const INPUT_CHAIN: &str = "input";
pub const MAINT_EGRESS_CHAIN: &str = "duduclaw_maint_egress";
pub const SRC4_SET: &str = "maintenance_src4";
pub const SRC6_SET: &str = "maintenance_src6";
pub const PORT_SET: &str = "maintenance_tcp_ports";

/// Allowed source ranges for port 22 (owner decision Q6).
pub const EXPECTED_SRC4: [(&str, u8); 5] = [
    ("10.0.0.0", 8),
    ("172.16.0.0", 12),
    ("192.168.0.0", 16),
    ("169.254.0.0", 16),
    ("100.64.0.0", 10),
];
pub const EXPECTED_SRC6: [(&str, u8); 2] = [("fc00::", 7), ("fe80::", 10)];

/// Parse `systemctl show <unit> -p ExecStart --value`. Exactly one command
/// record is accepted (`{ path=… ; argv[]=… ; … }`); returns its path and
/// argv tokens.
pub fn parse_exec_start(stdout: &str) -> Result<(String, Vec<String>), String> {
    let text = stdout.trim();
    if text.matches('{').count() != 1 || text.matches('}').count() != 1 {
        return Err("ExecStart must hold exactly one command".to_string());
    }
    let inner = text
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .ok_or_else(|| "ExecStart is not in systemd's { … } form".to_string())?;
    let mut path = None;
    let mut argv = None;
    for field in inner.split(" ; ") {
        let field = field.trim();
        if let Some(v) = field.strip_prefix("path=") {
            if path.replace(v.to_string()).is_some() {
                return Err("ExecStart has two path= fields".to_string());
            }
        } else if let Some(v) = field.strip_prefix("argv[]=") {
            let tokens: Vec<String> = v.split_ascii_whitespace().map(str::to_string).collect();
            if argv.replace(tokens).is_some() {
                return Err("ExecStart has two argv[]= fields".to_string());
            }
        }
    }
    match (path, argv) {
        (Some(p), Some(a)) => Ok((p, a)),
        _ => Err("ExecStart has no path= or argv[]= field".to_string()),
    }
}

/// The merged ExecStart equals [`EXPECTED_DROPBEAR_ARGV`] token for token.
pub fn exec_start_ok(stdout: &str) -> Result<(), String> {
    let (path, argv) = parse_exec_start(stdout)?;
    if path != DROPBEAR_PATH {
        return Err(format!("dropbear ExecStart runs {path:?}, expected {DROPBEAR_PATH}"));
    }
    if argv.iter().map(String::as_str).ne(EXPECTED_DROPBEAR_ARGV) {
        return Err("dropbear ExecStart arguments differ from the maintenance configuration".to_string());
    }
    Ok(())
}

/// `systemctl show <unit> -p BindsTo --value` lists `unit` as a whole token.
pub fn binds_to_ok(stdout: &str, unit: &str) -> bool {
    stdout.split_ascii_whitespace().any(|t| t == unit)
}

/// What the chain object itself must say: `Some((hook, policy))` for a base
/// chain (policy `None` = any), `None` for a regular chain.
type ChainKind<'a> = Option<(&'a str, Option<&'a str>)>;

/// The rules of `chain` in an `nft -j list chain …` dump, in order, after
/// checking the chain is the expected kind (a base chain must be a `filter`
/// chain on the given hook, with the given policy when one is required).
fn chain_rules<'a>(dump: &'a Value, chain: &str, kind: ChainKind<'_>) -> Result<Vec<&'a [Value]>, String> {
    let entries = dump
        .get("nftables")
        .and_then(Value::as_array)
        .ok_or_else(|| "nft output has no nftables array".to_string())?;
    let mut saw_chain = false;
    let mut rules = Vec::new();
    for entry in entries {
        if let Some(c) = entry.get("chain") {
            if is_ours(c) && c.get("name").and_then(Value::as_str) == Some(chain) {
                check_chain_kind(c, chain, kind)?;
                saw_chain = true;
            }
        }
        if let Some(r) = entry.get("rule") {
            if is_ours(r) && r.get("chain").and_then(Value::as_str) == Some(chain) {
                let expr = r
                    .get("expr")
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("a rule in chain {chain} has no expr array"))?;
                rules.push(expr.as_slice());
            }
        }
    }
    if !saw_chain {
        return Err(format!("chain inet filter {chain} is not loaded"));
    }
    Ok(rules)
}

fn check_chain_kind(c: &Value, chain: &str, kind: ChainKind<'_>) -> Result<(), String> {
    let field = |k: &str| c.get(k).and_then(Value::as_str);
    match kind {
        Some((hook, policy)) => {
            if field("type") != Some("filter") || field("hook") != Some(hook) {
                return Err(format!("chain {chain} is not a filter chain on the {hook} hook"));
            }
            if let Some(policy) = policy {
                if field("policy") != Some(policy) {
                    return Err(format!("chain {chain} does not have policy {policy}"));
                }
            }
        }
        None => {
            if c.get("hook").is_some() {
                return Err(format!("chain {chain} must be a regular chain, not a base chain"));
            }
        }
    }
    Ok(())
}

fn is_ours(obj: &Value) -> bool {
    obj.get("family").and_then(Value::as_str) == Some("inet") && obj.get("table").and_then(Value::as_str) == Some("filter")
}

/// Statements other than counters (which carry changing numbers).
fn statements(rule: &[Value]) -> Vec<&Value> {
    rule.iter().filter(|e| e.get("counter").is_none()).collect()
}

fn is_match(expr: &Value, left: &Value, right: &Value) -> bool {
    let Some(m) = expr.get("match") else { return false };
    m.get("op").and_then(Value::as_str) == Some("==") && m.get("left") == Some(left) && m.get("right") == Some(right)
}

fn meta(key: &str) -> Value {
    serde_json::json!({ "meta": { "key": key } })
}

fn payload(protocol: &str, field: &str) -> Value {
    serde_json::json!({ "payload": { "protocol": protocol, "field": field } })
}

fn has_verdict(rule: &[Value], verdict: &str) -> bool {
    rule.iter().any(|e| e.get(verdict).is_some())
}

/// `nft -j list chain inet filter output`: a filter base chain on the
/// output hook whose **first** rule is `oif "lo" jump duduclaw_maint_egress`
/// (counters allowed). Nothing may run before it, so no earlier rule or
/// jumped-to chain can accept the account's loopback traffic first. The OS
/// ruleset must therefore list the maintenance jump before podman's.
pub fn output_chain_ok(raw: &str) -> Result<(), String> {
    let dump: Value = serde_json::from_str(raw).map_err(|e| format!("nft output is not JSON: {e}"))?;
    let rules = chain_rules(&dump, OUTPUT_CHAIN, Some(("output", None)))?;
    let lo = Value::String("lo".to_string());
    let jump = serde_json::json!({ "jump": { "target": MAINT_EGRESS_CHAIN } });
    let first_is_jump = rules.first().is_some_and(|rule| {
        let s = statements(rule);
        s.len() == 2 && is_match(s[0], &meta("oif"), &lo) && *s[1] == jump
    });
    if !first_is_jump {
        return Err(format!("the first output rule is not `oif \"lo\" jump {MAINT_EGRESS_CHAIN}`"));
    }
    Ok(())
}

/// `nft -j list chain inet filter duduclaw_maint_egress`: a rule `meta skuid
/// <uid> reject` and nothing that accepts.
pub fn maint_egress_chain_ok(raw: &str, uid: u32, user: &str) -> Result<(), String> {
    let dump: Value = serde_json::from_str(raw).map_err(|e| format!("nft output is not JSON: {e}"))?;
    let rules = chain_rules(&dump, MAINT_EGRESS_CHAIN, None)?;
    // nft prints the uid as a number, or as the user name when it resolves it.
    let by_number = Value::from(uid);
    let by_name = Value::String(user.to_string());
    // Exactly the one rule: anything else in this chain (a jump, a queue, an
    // accept) could change what happens to the account's loopback traffic.
    let [rule] = rules.as_slice() else {
        return Err(format!("chain {MAINT_EGRESS_CHAIN} must hold exactly one rule, `meta skuid {uid} reject`"));
    };
    let s = statements(rule);
    let ok = s.len() == 2
        && (is_match(s[0], &meta("skuid"), &by_number) || is_match(s[0], &meta("skuid"), &by_name))
        && s[1].get("reject").is_some();
    if !ok {
        return Err(format!("chain {MAINT_EGRESS_CHAIN} has no `meta skuid {uid} reject` rule"));
    }
    Ok(())
}

/// `nft -j list chain inet filter input`: a filter base chain on the input
/// hook with policy drop, in which
/// - every rule matching `tcp dport @maintenance_tcp_ports` either rejects or
///   drops, or accepts only together with a source match on
///   `@maintenance_src4` / `@maintenance_src6`, and never jumps, goes to,
///   queues or uses a verdict map;
/// - no other rule accepts port 22 (a literal `22`, an anonymous set or a
///   range containing it) without such a source match;
/// - no rule is a bare `accept` with no match at all;
/// - an IPv4 source-restricted accept exists.
///
/// (An image still carrying the 2026-10 `tcp dport @maintenance_tcp_ports
/// accept` with no source is refused.)
pub fn input_chain_ok(raw: &str) -> Result<(), String> {
    let dump: Value = serde_json::from_str(raw).map_err(|e| format!("nft output is not JSON: {e}"))?;
    let rules = chain_rules(&dump, INPUT_CHAIN, Some(("input", Some("drop"))))?;
    let port_set = Value::String(format!("@{PORT_SET}"));
    let src4 = Value::String(format!("@{SRC4_SET}"));
    let src6 = Value::String(format!("@{SRC6_SET}"));
    let mut v4_accept = false;
    for rule in rules {
        let v4 = rule.iter().any(|e| is_match(e, &payload("ip", "saddr"), &src4));
        let v6 = rule.iter().any(|e| is_match(e, &payload("ip6", "saddr"), &src6));
        let accepts = has_verdict(rule, "accept");
        let uses_port_set = rule.iter().any(|e| is_match(e, &payload("tcp", "dport"), &port_set));
        if uses_port_set {
            if ["jump", "goto", "queue", "vmap"].iter().any(|v| has_verdict(rule, v)) {
                return Err("a maintenance port rule hands the decision to something else".to_string());
            }
            if accepts && !v4 && !v6 {
                return Err("a maintenance port rule accepts without a source restriction".to_string());
            }
            v4_accept |= accepts && v4;
            continue;
        }
        if accepts && statements(rule).iter().all(|e| e.get("accept").is_some()) {
            return Err("the input chain has an unconditional accept".to_string());
        }
        if (accepts || has_verdict(rule, "vmap")) && matches_port_22(rule) && !v4 && !v6 {
            return Err("an input rule accepts port 22 without the maintenance source sets".to_string());
        }
    }
    if !v4_accept {
        return Err(format!("input chain has no `ip saddr @{SRC4_SET}` maintenance rule"));
    }
    Ok(())
}

/// The rule matches TCP destination port 22 directly: a literal, an
/// anonymous set or a range that contains it.
fn matches_port_22(rule: &[Value]) -> bool {
    let has_22 = |v: &Value| match v {
        Value::Number(n) => n.as_u64() == Some(22),
        Value::Object(o) => match (o.get("set"), o.get("range")) {
            (Some(Value::Array(items)), _) => items.iter().any(|i| match i {
                Value::Number(n) => n.as_u64() == Some(22),
                Value::Object(r) => in_range(r.get("range")),
                _ => false,
            }),
            (_, Some(r)) => in_range(Some(r)),
            _ => false,
        },
        _ => false,
    };
    rule.iter().any(|e| {
        let Some(m) = e.get("match") else { return false };
        m.get("left") == Some(&payload("tcp", "dport")) && m.get("right").is_some_and(has_22)
    })
}

fn in_range(r: Option<&Value>) -> bool {
    let Some(Value::Array(ends)) = r else { return false };
    match (ends.first().and_then(Value::as_u64), ends.get(1).and_then(Value::as_u64)) {
        (Some(lo), Some(hi)) => lo <= 22 && 22 <= hi,
        _ => false,
    }
}

/// Elements of an interval set from `nft -j list set …`, as canonical
/// `(address, prefix length)` pairs. Anything that is not a plain prefix (a
/// range, a single address) makes the comparison fail.
fn set_prefixes(raw: &str, set: &str) -> Result<BTreeSet<(IpAddr, u8)>, String> {
    let dump: Value = serde_json::from_str(raw).map_err(|e| format!("nft output is not JSON: {e}"))?;
    let entry = dump
        .get("nftables")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find_map(|e| e.get("set")))
        .ok_or_else(|| format!("set {set} is not loaded"))?;
    if !is_ours(entry) || entry.get("name").and_then(Value::as_str) != Some(set) {
        return Err(format!("set {set} is not loaded in inet filter"));
    }
    let elems = entry.get("elem").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = BTreeSet::new();
    for e in &elems {
        // Elements with counters or timeouts are wrapped as {"elem":{"val":…}}.
        let v = e.get("elem").and_then(|w| w.get("val")).unwrap_or(e);
        let prefix = v.get("prefix").ok_or_else(|| format!("set {set} has an element that is not a prefix"))?;
        let addr: IpAddr = prefix
            .get("addr")
            .and_then(Value::as_str)
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| format!("set {set} has an unreadable address"))?;
        let len = prefix
            .get("len")
            .and_then(Value::as_u64)
            .and_then(|l| u8::try_from(l).ok())
            .ok_or_else(|| format!("set {set} has an unreadable prefix length"))?;
        out.insert((addr, len));
    }
    Ok(out)
}

fn expected(prefixes: &[(&str, u8)]) -> BTreeSet<(IpAddr, u8)> {
    prefixes.iter().filter_map(|(a, l)| a.parse().ok().map(|a| (a, *l))).collect()
}

/// The source set holds exactly the expected prefixes.
pub fn source_set_ok(raw: &str, set: &str, want: &[(&str, u8)]) -> Result<(), String> {
    if set_prefixes(raw, set)? == expected(want) {
        Ok(())
    } else {
        Err(format!("set {set} does not hold exactly the allowed source ranges"))
    }
}

/// The host key's SHA-256 fingerprint from `dropbearkey -y -f <key>` output:
/// computed from the printed public key line (the same parser as client
/// keys), so a changed output format cannot pass an unchecked string through.
pub fn host_key_fingerprint(stdout: &str) -> Result<String, String> {
    stdout
        .lines()
        .find_map(|line| {
            let mut parts = line.split_ascii_whitespace();
            let (ty, b64) = (parts.next()?, parts.next()?);
            crate::ssh_key::parse(&format!("{ty} {b64}")).ok().map(|k| k.fingerprint)
        })
        .ok_or_else(|| "dropbearkey printed no readable public key".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn good_exec_start() -> String {
        format!(
            "{{ path={DROPBEAR_PATH} ; argv[]={} ; ignore_errors=yes ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}",
            EXPECTED_DROPBEAR_ARGV.join(" ")
        )
    }

    pub(crate) fn output_chain(extra_before: &str) -> String {
        format!(
            r#"{{"nftables":[{{"metainfo":{{"version":"1.1.1"}}}},
            {{"chain":{{"family":"inet","table":"filter","name":"output","handle":3,"type":"filter","hook":"output","prio":0,"policy":"accept"}}}},
            {extra_before}
            {{"rule":{{"family":"inet","table":"filter","chain":"output","handle":8,"expr":[{{"match":{{"op":"==","left":{{"meta":{{"key":"oif"}}}},"right":"lo"}}}},{{"counter":{{"packets":0,"bytes":0}}}},{{"jump":{{"target":"duduclaw_maint_egress"}}}}]}}}},
            {{"rule":{{"family":"inet","table":"filter","chain":"output","handle":7,"expr":[{{"match":{{"op":"==","left":{{"meta":{{"key":"oif"}}}},"right":"lo"}}}},{{"jump":{{"target":"duduclaw_podman_egress"}}}}]}}}}]}}"#
        )
    }

    pub(crate) fn egress_chain(right: &str) -> String {
        format!(
            r#"{{"nftables":[{{"chain":{{"family":"inet","table":"filter","name":"duduclaw_maint_egress","handle":9}}}},
            {{"rule":{{"family":"inet","table":"filter","chain":"duduclaw_maint_egress","handle":10,"expr":[{{"match":{{"op":"==","left":{{"meta":{{"key":"skuid"}}}},"right":{right}}}}},{{"counter":{{"packets":0,"bytes":0}}}},{{"reject":null}}]}}}}]}}"#
        )
    }

    fn port_rule(handle: u32, saddr: Option<(&str, &str)>, verdict: &str) -> String {
        let src = saddr
            .map(|(proto, set)| {
                format!(r#"{{"match":{{"op":"==","left":{{"payload":{{"protocol":"{proto}","field":"saddr"}}}},"right":"@{set}"}}}},"#)
            })
            .unwrap_or_default();
        format!(
            r#"{{"rule":{{"family":"inet","table":"filter","chain":"input","handle":{handle},"expr":[{src}{{"match":{{"op":"==","left":{{"payload":{{"protocol":"tcp","field":"dport"}}}},"right":"@maintenance_tcp_ports"}}}},{verdict}]}}}}"#
        )
    }

    pub(crate) fn input_chain(legacy_rule: bool) -> String {
        let mut rules = vec![
            port_rule(20, Some(("ip", "maintenance_src4")), r#"{"accept":null}"#),
            port_rule(21, Some(("ip6", "maintenance_src6")), r#"{"accept":null}"#),
            port_rule(22, None, r#"{"reject":{"type":"tcp reset"}}"#),
        ];
        if legacy_rule {
            rules.insert(0, port_rule(19, None, r#"{"accept":null}"#));
        }
        format!(
            r#"{{"nftables":[{{"chain":{{"family":"inet","table":"filter","name":"input","handle":1,"type":"filter","hook":"input","prio":0,"policy":"drop"}}}},{}]}}"#,
            rules.join(",")
        )
    }

    pub(crate) fn src_set(name: &str, elems: &[(&str, u8)]) -> String {
        let e: Vec<String> = elems
            .iter()
            .map(|(a, l)| format!(r#"{{"prefix":{{"addr":"{a}","len":{l}}}}}"#))
            .collect();
        let ty = if name.ends_with('4') { "ipv4_addr" } else { "ipv6_addr" };
        format!(
            r#"{{"nftables":[{{"set":{{"family":"inet","name":"{name}","table":"filter","type":"{ty}","handle":4,"flags":["interval"],"elem":[{}]}}}}]}}"#,
            e.join(",")
        )
    }

    #[test]
    fn exec_start_must_equal_the_expected_argv_token_for_token() {
        assert!(exec_start_ok(&good_exec_start()).is_ok());
        let base = EXPECTED_DROPBEAR_ARGV.join(" ");
        let wrap = |argv: &str| format!("{{ path={DROPBEAR_PATH} ; argv[]={argv} ; ignore_errors=yes ; pid=0 }}");
        // One extra token.
        assert!(exec_start_ok(&wrap(&format!("{base} -R"))).is_err());
        // One token missing (-s): `-s` still appears inside `/run/duduclaw-maint-ssh`.
        let without_s = base.replacen(" -s ", " ", 1);
        assert!(without_s.contains("-s"), "the substring trap the token check avoids");
        assert!(exec_start_ok(&wrap(&without_s)).is_err());
        // Order swapped.
        assert!(exec_start_ok(&wrap(&base.replacen("-j -k", "-k -j", 1))).is_err());
        // Unexpanded variable left in.
        assert!(exec_start_ok(&wrap(&format!("{base} $DROPBEAR_EXTRA_ARGS"))).is_err());
        // Two ExecStart commands.
        assert!(exec_start_ok(&format!("{} {}", good_exec_start(), good_exec_start())).is_err());
        // Another binary.
        assert!(exec_start_ok(&good_exec_start().replacen("path=/usr/sbin/dropbear", "path=/tmp/dropbear", 1)).is_err());
        assert!(exec_start_ok("").is_err());
        assert!(exec_start_ok("[not set]").is_err());
    }

    #[test]
    fn binds_to_is_a_token_match() {
        assert!(binds_to_ok("duduclaw-sysd.service\n", SYSD_UNIT));
        assert!(binds_to_ok("foo.service duduclaw-sysd.service", SYSD_UNIT));
        assert!(!binds_to_ok("duduclaw-sysd.service.bak", SYSD_UNIT));
        assert!(!binds_to_ok("not-duduclaw-sysd.service", SYSD_UNIT));
        assert!(!binds_to_ok("", SYSD_UNIT));
    }

    #[test]
    fn output_chain_needs_the_jump_and_no_earlier_accept() {
        assert!(output_chain_ok(&output_chain("")).is_ok());
        // Even a jump to another chain may not come first.
        let podman_first = r#"{"rule":{"family":"inet","table":"filter","chain":"output","handle":5,"expr":[{"match":{"op":"==","left":{"meta":{"key":"oif"}},"right":"lo"}},{"jump":{"target":"duduclaw_podman_egress"}}]}},"#;
        assert!(output_chain_ok(&output_chain(podman_first)).is_err());
        let early_accept = r#"{"rule":{"family":"inet","table":"filter","chain":"output","handle":5,"expr":[{"match":{"op":"==","left":{"meta":{"key":"oif"}},"right":"lo"}},{"accept":null}]}},"#;
        assert!(output_chain_ok(&output_chain(early_accept)).is_err());
        let no_jump = output_chain("").replace("duduclaw_maint_egress", "something_else");
        assert!(output_chain_ok(&no_jump).is_err());
        assert!(output_chain_ok("{}").is_err());
        assert!(output_chain_ok("not json").is_err());
        // A rule for another table does not count.
        let other_table = output_chain("").replace(r#""handle":8"#, r#""handle":8,"x":1"#).replacen(
            r#"{"rule":{"family":"inet","table":"filter","chain":"output","handle":8"#,
            r#"{"rule":{"family":"ip","table":"filter","chain":"output","handle":8"#,
            1,
        );
        assert!(output_chain_ok(&other_table).is_err());
    }

    #[test]
    fn egress_chain_needs_the_numeric_uid_reject() {
        assert!(maint_egress_chain_ok(&egress_chain("4800"), 4800, "duduclaw-maint").is_ok());
        assert!(maint_egress_chain_ok(&egress_chain(r#""duduclaw-maint""#), 4800, "duduclaw-maint").is_ok());
        assert!(maint_egress_chain_ok(&egress_chain("1000"), 4800, "duduclaw-maint").is_err());
        assert!(maint_egress_chain_ok(&egress_chain(r#""duduclaw""#), 4800, "duduclaw-maint").is_err());
        let accept = egress_chain("4800").replace(r#"{"reject":null}"#, r#"{"accept":null}"#);
        assert!(maint_egress_chain_ok(&accept, 4800, "duduclaw-maint").is_err());
        let extra_jump = egress_chain("4800").replace(
            r#"{"rule":{"family":"inet","table":"filter","chain":"duduclaw_maint_egress","handle":10"#,
            r#"{"rule":{"family":"inet","table":"filter","chain":"duduclaw_maint_egress","handle":11,"expr":[{"jump":{"target":"elsewhere"}}]}},{"rule":{"family":"inet","table":"filter","chain":"duduclaw_maint_egress","handle":10"#,
        );
        assert!(maint_egress_chain_ok(&extra_jump, 4800, "duduclaw-maint").is_err());
        let empty = r#"{"nftables":[{"chain":{"family":"inet","table":"filter","name":"duduclaw_maint_egress","handle":9}}]}"#;
        assert!(maint_egress_chain_ok(empty, 4800, "duduclaw-maint").is_err());
    }

    #[test]
    fn input_chain_refuses_a_sourceless_accept() {
        assert!(input_chain_ok(&input_chain(false)).is_ok());
        assert!(input_chain_ok(&input_chain(true)).is_err());
        // Policy accept, a regular chain, or the wrong hook.
        assert!(input_chain_ok(&input_chain(false).replace(r#""policy":"drop""#, r#""policy":"accept""#)).is_err());
        assert!(input_chain_ok(&input_chain(false).replace(r#","type":"filter","hook":"input","prio":0"#, "")).is_err());
        assert!(input_chain_ok(&input_chain(false).replace(r#""hook":"input""#, r#""hook":"forward""#)).is_err());
    }

    #[test]
    fn port_22_accepts_outside_the_maintenance_rules_are_refused() {
        let with = |extra: &str| input_chain(false).replacen(r#"{"rule":"#, &format!(r#"{extra},{{"rule":"#), 1);
        let lit = r#"{"rule":{"family":"inet","table":"filter","chain":"input","handle":5,"expr":[{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":22}},{"accept":null}]}}"#;
        assert!(input_chain_ok(&with(lit)).is_err());
        let set = r#"{"rule":{"family":"inet","table":"filter","chain":"input","handle":5,"expr":[{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":{"set":[80,22]}}},{"accept":null}]}}"#;
        assert!(input_chain_ok(&with(set)).is_err());
        let range = r#"{"rule":{"family":"inet","table":"filter","chain":"input","handle":5,"expr":[{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":{"range":[1,1024]}}},{"accept":null}]}}"#;
        assert!(input_chain_ok(&with(range)).is_err());
        let bare = r#"{"rule":{"family":"inet","table":"filter","chain":"input","handle":5,"expr":[{"counter":{"packets":0,"bytes":0}},{"accept":null}]}}"#;
        assert!(input_chain_ok(&with(bare)).is_err());
        let vmap = input_chain(false).replacen(r#"{"accept":null}"#, r#"{"vmap":{"key":{"meta":{"key":"l4proto"}},"data":"@x"}}"#, 1);
        assert!(input_chain_ok(&vmap).is_err());
        // Ordinary rules are fine: loopback, established, the dashboard port.
        let ok = [
            r#"{"rule":{"family":"inet","table":"filter","chain":"input","handle":2,"expr":[{"match":{"op":"==","left":{"meta":{"key":"iif"}},"right":"lo"}},{"accept":null}]}}"#,
            r#"{"rule":{"family":"inet","table":"filter","chain":"input","handle":3,"expr":[{"match":{"op":"in","left":{"ct":{"key":"state"}},"right":["established","related"]}},{"accept":null}]}}"#,
            r#"{"rule":{"family":"inet","table":"filter","chain":"input","handle":4,"expr":[{"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":18789}},{"accept":null}]}}"#,
        ];
        assert!(input_chain_ok(&with(&ok.join(","))).is_ok());
    }

    #[test]
    fn chain_kinds_are_checked() {
        // The output chain must be a base chain on the output hook.
        assert!(output_chain_ok(&output_chain("").replace(r#""hook":"output""#, r#""hook":"prerouting""#)).is_err());
        // The egress chain must be a regular chain.
        let base = egress_chain("4800").replace(r#""handle":9}"#, r#""handle":9,"type":"filter","hook":"output","prio":0,"policy":"accept"}"#);
        assert!(maint_egress_chain_ok(&base, 4800, "duduclaw-maint").is_err());
        // Ending evaluation before the jump.
        let ret = r#"{"rule":{"family":"inet","table":"filter","chain":"output","handle":5,"expr":[{"return":null}]}},"#;
        assert!(output_chain_ok(&output_chain(ret)).is_err());
    }

    #[test]
    fn source_sets_must_match_exactly() {
        assert!(source_set_ok(&src_set(SRC4_SET, &EXPECTED_SRC4), SRC4_SET, &EXPECTED_SRC4).is_ok());
        assert!(source_set_ok(&src_set(SRC6_SET, &EXPECTED_SRC6), SRC6_SET, &EXPECTED_SRC6).is_ok());
        // Missing VPN range, extra global range, wrong length, wrong set name.
        assert!(source_set_ok(&src_set(SRC4_SET, &EXPECTED_SRC4[..4]), SRC4_SET, &EXPECTED_SRC4).is_err());
        let mut extra = EXPECTED_SRC4.to_vec();
        extra.push(("0.0.0.0", 0));
        assert!(source_set_ok(&src_set(SRC4_SET, &extra), SRC4_SET, &EXPECTED_SRC4).is_err());
        let mut wrong = EXPECTED_SRC6.to_vec();
        wrong[1] = ("fe80::", 9);
        assert!(source_set_ok(&src_set(SRC6_SET, &wrong), SRC6_SET, &EXPECTED_SRC6).is_err());
        assert!(source_set_ok(&src_set("other_set4", &EXPECTED_SRC4), SRC4_SET, &EXPECTED_SRC4).is_err());
        let range = r#"{"nftables":[{"set":{"family":"inet","name":"maintenance_src4","table":"filter","elem":[{"range":["10.0.0.0","10.255.255.255"]}]}}]}"#;
        assert!(source_set_ok(range, SRC4_SET, &EXPECTED_SRC4).is_err());
    }

    #[test]
    fn host_key_fingerprint_is_computed_from_the_public_key_line() {
        let pubkey = include_str!("../../tests/fixtures/ssh_keys/ed25519.pub");
        let mut parts = pubkey.split_ascii_whitespace();
        let line = format!("{} {} root@duduclaw", parts.next().unwrap(), parts.next().unwrap());
        let out = format!("Public key portion is:\n{line}\nFingerprint: SHA256:ignored\n");
        assert_eq!(host_key_fingerprint(&out).unwrap(), "SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE");
        assert!(host_key_fingerprint("Fingerprint: SHA256:abc").is_err());
    }
}
