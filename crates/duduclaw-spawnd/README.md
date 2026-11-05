# duduclaw-spawnd

Root-owned launch coordination for the planned native Linux employee isolation.
The daemon accepts only the gateway account on the fixed seqpacket socket and
serializes capability-holding runs for each employee. It never executes employee
code itself or falls back to the gateway identity.

This package is an integration component, not a complete protected installation.
The B2 trampoline, C egress and H storage adapters must supply verified readiness,
sealed bootstrap descriptors and cleanup evidence. The shipped binary refuses
launches until those adapters are connected; installing its units does not enable
employee isolation. The production Host includes root account recovery, durable
UID/run journals, real systemd transient units, detached mount/executable
preparation, bounded lifecycle jobs and an independent intrusion monitor.
See [Package B1](../../docs/architecture/native-linux-isolation.md#package-b1)
for adapter contracts and remaining cross-package acceptance.

## Build and verification

Only Linux runs the service. The policy and admission tests are portable.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --target x86_64-unknown-linux-gnu --offline
```

Privileged tests are opt-in with the `iso_root_` prefix; run them only in a
disposable Linux VM. The service accepts no paths, argv, environment, uid or
systemd properties from its caller. `--check` exits unsuccessfully if any required
integration/readback is missing. It prints the root-owned readiness evidence from
`/run/duduclaw-spawnd/readiness.json`; boot identity and a one-second expiry prevent
stale evidence from authorizing a launch. It never treats process liveness as readiness.

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1
```

Those privileged tests are compiled but have not been run in this worktree.
They require a disposable root Linux VM and the installation prerequisites named
in each ignore reason. The account test modifies login ACLs; the systemd resource
test requires the complete trusted service hierarchy.

## UserDB account verification

Account checks enumerate Varlink providers directly; `userdbctl` is not required.
Every provider also receives named `GetUserRecord`/`GetGroupRecord` lookups
(`userName`/`groupName`) and numeric lookups (`uid`/`gid`) for the account being
checked or created, all registry employees when that list is available, and local
employee user/group entries. Each request includes the socket's `service`.
This catches providers that synthesize reserved aliases only on named lookup,
as permitted by the [user-record specification](https://github.com/systemd/systemd/blob/main/docs/USER_RECORD.md).
Only `io.systemd.UserDatabase.NoRecordFound` means no hit. Every targeted hit
must be an exact local NSS echo matching the queried name or ID; non-NSS hits,
other errors, timeouts and malformed replies are refused using the same strict
canonical parser and provider validation as enumeration.
Each record retains its socket provider and complete JSON until validation, with
duplicate JSON keys refused. Before merging identity pairs, any non-NSS provider
(including PID1's DynamicUser) touching a reserved UID/GID, a `duduclaw-emp-*`
name or alias, or a local employee account/group name is refused; unrelated
non-NSS identities remain allowed. Employee membership mappings are also protected.
`aliases` must be an array of strings wherever encountered, including top-level,
`status.*`, `perMachine[*]` and `binding.*` objects in user and group records.
The user-record specification defines aliases at top-level and in `status.*`;
the other locations and group records are checked conservatively.
NSS echoes touching this namespace must match local `/etc/passwd`, `/etc/group` and
`/etc/shadow` identity, home, shell, account expiry, password state and the
`members`/`memberOf` group mappings in these records. The separate `GetMemberships`
API is not queried. Employee accounts/groups have no aliases, so an NSS echo
carrying an alias field (even an empty array) is refused. Relevant incomplete
records and unsupported security attributes fail closed.
Recovery compares the current local state so a matching half-created account can
still be locked down; the normal admission checks continue to require lockdown.

An absent `/run/systemd/userdb` is allowed. An existing directory requires at
least one provider to complete both user and group enumeration and all targeted
lookups, including explicit
`NoRecordFound` replies. A DynamicUser-only installation satisfies this condition;
an empty or Multiplexer-only directory is refused. There is no Multiplexer
fallback. The 5-second total deadline, 8 MiB/65,536-record budgets, 256-entry limit,
fixed alias restrictions, root-owned socket and root peer checks remain enforced.
Enumeration and all targeted lookups across providers share those total budgets;
DynamicUser may answer non-dynamic employee lookups with `NoRecordFound`.
The NSS shadow mapping follows systemd's
[user-record-nss.c](https://github.com/systemd/systemd/blob/v252/src/shared/user-record-nss.c);
`service` can appear under `status[machine-id]` in
[v249](https://github.com/systemd/systemd/blob/v249/src/userdb/userwork.c),
[v252](https://github.com/systemd/systemd/blob/v252/src/userdb/userwork.c) and
[v255](https://github.com/systemd/systemd/blob/v255/src/userdb/userwork.c), so it is
checked as metadata, never used to override the queried provider.

These review fixes have portable regression tests. The earlier root-account
acceptance on Ubuntu 22.04/24.04 and Debian 12 must be repeated with this patch;
compilation and fixture tests do not establish live-provider acceptance.

## License

Apache-2.0, as declared by the workspace.
