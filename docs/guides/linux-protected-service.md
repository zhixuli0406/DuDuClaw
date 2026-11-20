# Protected native Linux service

The per-employee isolation architecture (planned for a later release) separates the gateway, AI employees, document parsing and privileged updates. Each confirmed employee receives a permanent Linux identity; each run receives its own process, filesystem and network namespaces. An unavailable isolation component stops employee execution.

**Development status:** signed inactive-generation staging, the installer migration handoff, first-administrator setup, read-only installation checks and coordinated fixed-path removal are implemented. Complete fresh installation, signed activation, the migration platform adapter and the gateway composite-readiness adapter remain incomplete. This round has no live root installation/removal acceptance. An `active` service or staged generation is not isolation readiness. See the [implementation and acceptance record](../architecture/native-linux-isolation.md).

**What you can install today:** `service install --protected-linux` does not install spawnd, egress or the separate updater, and nothing creates the migration plan. Until the isolation fresh bootstrap lands (planned for a later release), the protected service is the v1.80 single-binary gateway service described in [Single-binary protected service (current releases)](#single-binary-protected-service-current-releases). The per-employee Linux isolation described in this guide does not apply to it.

## Fresh isolated install (v1.83.0 and later)

From v1.83.0, a new protected host is installed only through the independently verified `duduclaw-bootstrap install`. On a host that never had DuDuClaw, `sudo duduclaw service install --protected-linux` prints the steps below and changes nothing. `--migrate-from` is refused for every protected install. A host that already runs the v1.80–v1.82 single-binary protected service (or has its leftover files) is refused by this flow; it needs the upgrade path instead. The isolated install requires the TEAM edition.

Storage prerequisites checked before installation changes the host:

- Install `e2fsprogs`: `/usr/sbin/mkfs.ext4` must resolve to a regular root-managed executable with trusted ancestors.
- Make the kernel loop module available (`modprobe loop`); `/dev/loop-control` must be character device 10:237.
- Keep at least max(20 GiB, 10% of total capacity, rounded up) free on the filesystem holding `/var/lib`, in addition to satisfying the existing “2× package size + 1 GiB” space check. Device-mapper backing also needs `dmsetup` (the `dmsetup` / `lvm2` packages).

1. As root, create the download directory: `install -d -m 0700 -o root -g root /root/duduclaw-migrate`.
2. Save these as files in that directory: `duduclaw-bootstrap`, `duduclaw-isolated-linux-<arch>-<version>.tar` and its `.minisig`, `.authorization.json` and `.authorization.json.signatures.json`, plus your TEAM license `license.json` (owned by root, not writable by group or others).
3. Compare them with `sha256sum -c` against the values published on the website and in the license console. Both must agree.
4. Run `sudo /root/duduclaw-migrate/duduclaw-bootstrap install [--dns-upstream <IP:53>] [--host-public-address <IP>]...`. The installer starts the gateway itself and enables it at boot.

| Option | Meaning |
| --- | --- |
| `--dns-upstream <IP:53>` | Only on hosts without systemd-resolved: the single upstream of the dedicated DNS broker. Refused when systemd-resolved is available, and refused for loopback addresses (such as a local resolver on `127.0.0.1`) and IPv4 link-local addresses (`169.254.0.0/16`, such as a cloud VM's metadata or resolver address: the broker's network rule limits the host, not the port, so it could also reach the metadata service). On a cloud VM whose only resolver is link-local, install and enable systemd-resolved (the installer then uses it directly, without `--dns-upstream`), or give a resolver with a routable address. The installer writes this address into the DNS broker's network rule; it is never added to the employees' egress rules. To change the upstream, re-run the installer: do not edit the broker's unit file (the readiness check refuses an edited file) or add a drop-in (the isolation service's network-rule check refuses it). |
| `--host-public-address <IP>` | A public address of this host that is on no interface (cloud 1:1 NAT); repeatable, at most 64. Employees are denied connections to it. |
| `--enable-now` | No longer needed and has no effect: the installer always leaves the gateway running. Still accepted, so existing scripts keep working; the installer prints a note when it is given. |

The installer checks the host before it changes anything and lists every failed item: Ubuntu 22.04, Ubuntu 24.04 or Debian 12; systemd 249 or later; kernel 5.15 or later; cgroup v2; `/var/lib` on ext4 or XFS; valid system ID limits in `/etc/login.defs` (`SYS_UID_MAX`/`SYS_GID_MAX` are commented out in stock files; absent maxima use `UID_MIN`/`GID_MIN` minus one, with a default minimum of 1000, as shadow does); no account, group or subordinate ID range in 70000–99999; the sshd main configuration includes `/etc/ssh/sshd_config.d/*.conf`; `cron.allow`/`at.allow` do not admit DuDuClaw accounts; enough free space; a valid TEAM license; no earlier DuDuClaw installation. Hosts with an external account service (LDAP, SSSD and similar) are not supported by this installer.

It then confirms that nothing runs as the service account, publishes the verified update trust root, generation and fixed-path programs, creates the service accounts, directories and units (the gateway unit included, not yet enabled), and writes the egress and DNS configuration from the signed policy. It starts the services in the same order a boot does: the sockets and spawnd, the consistency gate (spawnd is open), egress, the gateway, then the update service. It waits until the update service reports the installed version (up to 2 minutes) and spawnd reports every readiness check passed (up to 2.5 minutes). If spawnd is not ready in time, the message lists the checks that have not passed yet. The installer then enables the nightly update timer (`duduclaw-auto-upgrade.timer`), runs the composite readiness check and only then enables the gateway at boot. The install reports success only after the full readiness check passes, with the gateway running.

**Resume.** Progress is recorded in `/var/lib/duduclaw-protected/isolation-bootstrap.json`. If the install stops (an error, a reboot, a power loss), fix the reported cause and run the same `duduclaw-bootstrap install` command again; it continues at the first unfinished step. You may omit the options on a resume; different values are refused. A different bundle is refused until you roll back. While an install is unfinished, `service install`, `upgrade` and `uninstall --protected-linux` are refused.

**Rollback.** Before the last step (commit), `sudo /root/duduclaw-migrate/duduclaw-bootstrap rollback` (with the same bundle files still in the directory) returns the host to its state before the install. It stops and disables the services, then removes the installed programs and units, the update service's trust, state and generations, spawnd's state, `egress.json`, `dns-broker.json`, the root key and the gateway data the install created. It keeps the system accounts, the empty gateway data directory with your license, the protected-mode marker and the record that prevents installing an older bundle. Removal never follows links or crosses mounts; if anything remains, the paths are listed and the same command can be run again. After the commit, rollback is refused; use `service uninstall --protected-linux`.

## What changes for customers

The changes below apply once the isolation architecture is available. They do not apply to current installations.

- Employees no longer execute as the gateway account when isolation is required. One employee runs one job at a time; additional conversations and scheduled jobs queue.
- Employees can read their own CLI login tokens. They cannot use another employee's home or the gateway's credentials through the isolated execution path. Personal connections require a confirmed Personal employee with one owner; shared employees cannot use personal credentials.
- Outbound traffic uses a separate proxy. Private networks, host loopback and cloud metadata are blocked. Public destinations are not restricted by a domain allowlist. Tools that cannot use the proxy fail to connect.
- Untrusted documents use disposable low-privilege parsing workers. Workers have no network and receive bounded inputs through file descriptors.
- Updates use a separate root service and signed component manifests. A downloaded archive or accepted update request is not a completed update.
- Migrating from v1.79.3 requires fresh credentials and administrator review. Old accounts, bindings, policies, hooks, MCP servers and schedules do not become active merely because they were imported.

The protection assumes the kernel, systemd, gateway, spawn service, trampoline, egress service and updater remain trusted. It does not provide VM-level hostile multitenant isolation, protect against host root compromise, or isolate persistent files between runs of the same employee. Ordinary user installation remains outside this isolation guarantee.

## Supported host targets and observed tests

Use native Linux with systemd as PID 1, unified cgroup v2 and the initial user namespace. Containers, remapped/rootless namespaces and cgroup v1/hybrid are refused. Official CLI binaries require glibc 2.31 or later; that binary baseline alone does not establish isolation support.

The target distributions are Ubuntu 22.04, Ubuntu 24.04 and Debian 12. The recorded **package-level** root tests on 2026-11-05 were:

| Distribution | Tested kernel | systemd | Landlock ABI | Result |
| --- | --- | --- | --- | --- |
| Ubuntu 22.04 | 5.15.0-1120-gcp | 249.11 | 1 | 66/66 package tests passed |
| Ubuntu 22.04, GCP default/HWE | 6.8.0-1070-gcp | 249.11 | 4 | 66/66 package tests passed |
| Ubuntu 24.04 | 6.8.0-1071-gcp | 255.4 | 4 | 66/66 package tests passed |
| Ubuntu 24.04, GCP image default | 7.0.0-1014-gcp | 255.4 | 8 | Unsupported: all 16 trampoline tests refused; 50 other tests passed |
| Debian 12 | 6.1.0-54-cloud-amd64 | 252.39 | 2 | 66/66 package tests passed |

Kernel 7.0 is **not currently accepted**: the trampoline refuses Landlock ABIs above 7. Check the kernel actually booted; a distribution label or GCP image name is insufficient. Use an accepted kernel configuration for validation; do not disable Landlock or widen the ABI check locally. These results do not cover a full installation, real vendor CLI execution, complete migration or update recovery.

The host needs trusted system tools including `getent`, `id`, `useradd`, util-linux, systemd tools (including `systemd-tmpfiles`) and curl. Loop-backed employee storage requires ext4 or XFS on a supported non-thin backing-device chain, with room for employee quotas and the TCB reserve. Unknown backing layouts or inadequate capacity refuse readiness. The passwd and group lines of `/etc/nsswitch.conf` may name only `files` and `systemd` (the default on Ubuntu and Debian, and on RHEL/Fedora with the authselect `minimal` profile). Hosts with any other module there (LDAP/SSSD `sss`, Google Compute Engine OS Login `oslogin`/`cache_oslogin`, `compat`, bracketed action clauses such as `[SUCCESS=merge]`) are not supported and refuse readiness: such a module can introduce identities in the employee UID range and this release cannot reserve that range in an external directory.

## Single-binary protected service (current releases)

Until the isolation components can be installed (planned for a later release), `service install --protected-linux` installs the v1.80 single-binary protected gateway: the root-owned program `/opt/duduclaw/bin/duduclaw`, the launcher `/usr/local/libexec/duduclaw/protected-launcher`, the unit `duduclaw-gateway.service` and the version floor `/var/lib/duduclaw-protected/version-floor`. Until the isolation architecture is installed on the host (planned for a later release), AI employees run under the gateway's service account as in v1.80, with the v1.80 credential protection; employees are created directly from the dashboard as in v1.80. The isolation path described in this guide (spawnd launches, Personal employees, employee proposals) engages only on hosts where the isolation architecture is actually installed.

**New host.** With no recorded version floor, the installer continues only when it can show the host never had DuDuClaw installed: there is no `duduclaw` account and none of these paths exist: `/var/lib/duduclaw`, `/var/lib/duduclaw-protected/root.key`, `/var/lib/duduclaw-protected/version-floor`, `/var/lib/duduclaw-protected/version-pending`, `/opt/duduclaw/bin/duduclaw`, the launcher, `/etc/systemd/system/duduclaw-gateway.service`, `/etc/duduclaw/migration-import.json`, `/var/lib/duduclaw-migrate`, `/var/lib/duduclaw-updated/state.json` and `/opt/duduclaw-isolated/current`. If any of them exists or cannot be checked, the missing floor may belong to a v1.79.3 installation and the installer refuses with the migration message. Do not delete these paths to get past the refusal; they can hold data and keys.

`--migrate-from` checks the source tree first, before the installer creates the account or anything else: stop the user-mode gateway, because a source that changes while it is read is refused. Before creating the account the installer writes the root-only marker `/var/lib/duduclaw-protected/fresh-install-pending`. If the install then fails, rerunning the same command continues it, as long as the marker is intact and the version floor, root key, `/etc/duduclaw/migration-import.json`, `/var/lib/duduclaw-migrate`, `/var/lib/duduclaw-updated/state.json` and `/opt/duduclaw-isolated/current` are still absent. If the earlier run had already copied the data, add `--force-migrate` (the earlier copy is kept as a backup). The marker is removed once the floor is recorded, and by `uninstall`.

The marker is the installer's record that a fresh install was started on this host; it must be a root-owned regular file with mode 0600 and the exact content the installer wrote. If it exists but does not pass that check, the installer does not treat the host as new and refuses. Root can inspect it with `sudo ls -l /var/lib/duduclaw-protected/fresh-install-pending`; remove it as root (`sudo rm /var/lib/duduclaw-protected/fresh-install-pending`) only when the host has no version floor and no root key and you intend a fresh install. After removal the installer still applies the new-host conditions above.

```sh
# Run the verified binary from a root-owned location (see "Installation and readiness").
sudo ./duduclaw service install --protected-linux --enable-now
```

**v1.80.x host.** A host whose recorded floor is 1.80.0 or later keeps the v1.80 upgrade:

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux [--version X.Y.Z]
```

Without `--version` it uses the latest release. The command checks the configured AI CLIs first (see below), then downloads the official release, verifies its checksum, signature and embedded version, replaces the program atomically, refreshes the unit and launcher when their content differs, recertifies automatic recovery, records the new floor and restarts an active gateway. The target must be newer than the program that runs the command, and that program must not be older than the recorded floor (its unit and launcher would be older), so run the installed `/opt/duduclaw/bin/duduclaw`. To move the service to a newer binary you have already verified, run that binary's `sudo ./duduclaw service install --protected-linux` instead; reinstalling keeps the data and root key only while the version floor exists. On a single-binary host, `service uninstall --protected-linux` removes the floor but keeps the data, root key and account, so a later install is refused as a host with an unknown floor (an isolation installation keeps its floor; see "Upgrade and uninstall").

**Dashboard.** The dashboard cannot update a protected install without the separate update service. It says so, shows the command above and sends no request.

Once the separate update service manages the install (both `/var/lib/duduclaw-updated/state.json` and `/opt/duduclaw-isolated/current` exist), `service upgrade --protected-linux` requires `--version` and only sends a request to that service. It never replaces the program itself, including when the service cannot be reached.

## Installation and readiness

Run from a human administrator's terminal, outside an AI session. Obtain the complete release for x86_64 or aarch64 and verify it in a root-owned staging directory with non-writable ancestors before executing any program as root. Linux archives contain separate component binaries; copying only `duduclaw` cannot install this architecture.

A complete installation requires these roles and inputs:

| Area | Required installation state |
| --- | --- |
| System identities | `duduclaw`, `duduclaw-runsup`, `duduclaw-egress`, `duduclaw-dl`, `duduclaw-dns`; static system IDs below `SYS_UID_MAX`, outside DynamicUser and employee ranges |
| Employee identities | Permanently recorded IDs, normally 70000–99999; allocated only after employee confirmation |
| Trusted programs | Gateway, spawnd, trampoline, egress/forwarder/DNS broker, updater/frozen launcher, parser, importer, migration command and MCP stdio bridge; root-owned executable files |
| Services | Gateway, spawnd socket/service, egress socket/service, optional DNS broker, updater, parse socket/template and TCB/employee slices |
| Self-test account | `duduclaw-selftest`: the isolation self-test drops to it; system ID like the other service accounts, no home, no login shell, own group, no supplementary groups, locked |
| Self-test unit | `/etc/systemd/system/duduclaw-isolation-selftest.service`, root:root 0644, exactly the shipped text with no drop-in; no `[Install]`: the installer never enables or starts it, spawnd starts it |
| SSH denial probe | `/usr/lib/duduclaw/duduclaw-isolation-probe`, root:root 0755, signed component; runs unprivileged as the isolation test account `duduclaw-selftest` with read access to the journal (group `systemd-journal`), never as root; no account of its own |
| SSH denial probe unit | `/etc/systemd/system/duduclaw-isolation-probe@.service` template, root:root 0644, exactly the shipped text; no `[Install]`: the installer never enables or starts it, one instance per employee account and sshd port (`duduclaw-isolation-probe@duduclaw-emp-<uid>.<port>.service`) |
| Parser roots | `/usr/lib/duduclaw/parse-root` and `/usr/lib/duduclaw/parse-root-tree`, root:root 0755; never populated with host data. `parse-root` is initially empty; `parse-root-tree` holds only the verified parser, its runtime libraries and the parser unit's empty mount-point directories (`.tmp`, `dev`, `proc`, `run`, `sys`, `tmp`, `var`, `var/tmp`), all created by the installer |
| Signed state | Bootstrap trust, compatible generation, runtime registry, updater slots/state and migration/readiness integration |

The parser may leave the known empty mountpoint directories described in its [installation reference](../../crates/duduclaw-parse-worker/README.md). Unexpected content must be investigated; do not recursively clear either parser root. Spawnd owns employee SSH, cron, at and user-manager lockdown and verifies effective host state. Do not hand-edit those controls to make a failed check pass.

If `/usr/sbin/sshd` is installed, spawnd proves that SSH is denied for each employee account itself; no installer receipt is involved. It first checks sshd's effective configuration for the account itself, then starts the SSH denial probe unit for that account and sshd port through systemd. The probe runs as the unprivileged isolation test account `duduclaw-selftest` with read access to the journal, and attempts a real public-key login and a real port-forward as the account against the running sshd; both must be refused by the `DenyUsers duduclaw-emp-*` rule, as confirmed by sshd's own journal records. An account is treated as able to log in only when sshd itself logs that it accepted the probe's key; a success that sshd did not log (for example another program answering on the SSH port) is only inconclusive. The proof is kept in memory and re-proven whenever sshd restarts, the sshd configuration in effect for the account, the account, the installed probe or its unit, the signed generation or the boot changes, after every spawnd restart, and in any case at least every 6 hours (every 10 minutes when spawnd cannot identify the running sshd through systemd); the renewal starts before the old proof expires, so a healthy host shows no gap. Until an employee's proof arrives, only that employee waits (not ready); other employees are unaffected. Creating a new employee does not wait for its proof inside spawnd: while the first proof runs, spawnd answers "pending" and the gateway sends the same allocation again for up to two minutes without undoing anything. If the proof is still not finished, the item stays *applying* and Migration review reports that the account's security check is not finished; retrying the item resumes the same account while spawnd keeps running and within ten minutes (after that, or after a spawnd restart, the unfinished account is retired, the apply fails like any retired account and the creation must be proposed again). At most four new employees can be pending at once; a further creation is refused until one of them resolves. If the proof shows the account can log in, the account is permanently retired.

Hosts with sshd also need `/usr/bin/ssh`, `/usr/bin/ssh-keygen` and `/usr/bin/journalctl`, the `duduclaw-selftest` account (created by the installer) and the `systemd-journal` group, sshd listening on 127.0.0.1 (at the first `Port` of its configuration) and sshd logging to the systemd journal. sshd with `UsePAM no` is not supported: sshd then rejects the (expired) employee account before applying `DenyUsers`, so the probe cannot confirm the denial and the employee never becomes ready. Hosts without sshd skip the probe.

The installer validates the complete fixed-path component set before preparing accounts, directories and units. Linux archives now include the frozen updater launcher. Offline release tooling can assemble a bootstrap inbox from signed metadata and artifacts. The independently authenticated `duduclaw-updated --bootstrap-stage` verifier checks its compiled root pin, threshold root signatures, target signature and all component hashes before publishing an **inactive generation**. It does not select `current`, create trust/state/slots, verify the legacy transition dual signature or activate services. No complete fresh-install recipe is available yet.

Run `sudo duduclaw service readiness --protected-linux` (the `ops` alias is equivalent) to inspect binaries, system accounts, directory permissions, unit contents and actual systemd properties. The report separates these checks from composite isolation verification. Every failed group lists its reason on a detail line. Until I1 supplies that adapter, `MissingComposite` always refuses readiness; the command cannot certify employee execution. Custom DNS drop-ins are not integrated and currently refuse the unit check. Reinstalling an already managed generation performs these read-only checks instead of rewriting its programs or units.

The command reports two separate results. **Platform readiness** (the four installer groups and the nine host items) is all that `sudo duduclaw service readiness --protected-linux --platform-only` checks; when every item passes it ends with "平台檢查均已通過。" (platform checks passed) and never claims that employees can run. Without `--platform-only`, the command also reads the gateway's own readiness view, which the running gateway rewrites at least every 30 seconds (and on every change) in `/run/duduclaw-gateway-update/isolation-readiness.json` (fixed component, reason and producer names only). The gateway view never changes a platform result. There are three outcomes once every platform item passes:

| Gateway view | Last line | Exit |
|---|---|---|
| From this boot, at most 90 s old, ready | "所有就緒檢查均已通過，可以執行員工工作。" (or the migration-pending sentence) | 0 |
| Fresh, not ready | `[資訊]` line listing each not-ready component as `name(reason)` and the boot assembly's missing producers, then "平台檢查均已通過，但 Gateway 尚未就緒，員工工作仍暫停。" | non-zero |
| Missing, stale, from another boot or invalid (gateway stopped or just started) | "[資訊] 無法取得 Gateway 的就緒狀態（Gateway 未執行、剛啟動，或狀態檔無效）。", then a refusal that employees cannot be confirmed to run | non-zero |

The installer's own readiness steps use the platform result only, because right after installation the gateway's isolation tool service may still be assembling in the background. The gateway journal names the same missing producers, with a fixed reason where one is known, for example `ConnectorProviders(catalog_file_unsafe)` (the connector catalog `isolation-connectors.db` or one of its SQLite sidecars is not a private, single-link file owned by the gateway account) or `ExecutionPlans(invalid_helper_runtime)` (a signed helper runtime does not match its fixed protocol; a policy without any helper runtime is valid, and those tool families are then simply not offered). The gateway retries the assembly in the background, so after the cause has been fixed no restart is needed.

## Migrate from v1.79.3

Do not replace the old binary in place or copy its databases into the new home. The old environment shares a UID with employees, so its authority data must be treated as untrusted. The installer now routes the v1.79.3-to-v1.80 boundary, including an unknown installed floor on a host that is not provably new (see the single-binary section above), into G's migration flow using the fixed root-owned plan `/var/lib/duduclaw-migrate/plan.json`. Missing or invalid plans refuse before stopping the old service; there is no legacy in-place fallback.

The administrator flow is:

1. Obtain the independently published SHA-256 values for the bootstrap verifier and migration package, plus the root-metadata fingerprint, from both the official website and authenticated license console. Values must agree. Save downloads as files in a root-owned 0700 directory; use the system `sha256sum -c` before executing the verifier. The old archive signature alone does not establish the new trust root. Do not pipe downloads into a shell.
2. The verified installer validates bootstrap signatures, prepares the signed platform and creates an immutable root-owned migration plan. Never generate authority or a success receipt by editing that plan yourself.
3. The migration runner verifies the platform **before stopping the old service**. It then disables old activation paths, quarantines the legacy tree, writes a verified encrypted snapshot, rotates keys and starts a clean import-only gateway. Low-privilege importers turn recognized content into bounded records; authority becomes inactive proposals.
4. Once isolation is verified, establish the fresh administrator account through the installer-provided setup flow. Old sessions, passwords, MFA material and CLI login state are not restored. Enroll MFA before reviewing high-risk proposals.
5. In **System → Migration review**, inspect full values, approve or reject proposals, retry failed applications and complete required credential rotation. Employee IDs and storage are allocated only after confirmation. On a host with SSH, the first allocation of each employee takes up to about a minute while the account's SSH refusal is verified; if the page reports that the verification is not finished, retry the item. Reconnect personal accounts under their new owner.
6. Completion requires fresh application readback and zero outstanding confirmations/rotation obligations. `AwaitingAdministrator` means migration is still pending. Keep the root-only recovery snapshot until the documented retention/recovery checks allow removal.

Supported source versions: conversation history (`sessions.db`), goals and assignment schedules (`tasks.db`) and scheduled tasks (`cron_tasks.db`) are imported from a v1.79.3 or v1.80.x home. Each store must match one known version exactly; a database changed by hand, half-upgraded or from another release is refused as a whole, and the importer log names the closest known version and the tables, indexes or triggers that differ (never their contents). Tasks owned by an external A2A client are never imported, and a scheduled task's original requester, creator and delegate are not carried over. The v1.79.3 definitions were reconstructed from the release notes, not captured from a v1.79.3 installation; they still need verification on a real v1.79.3 home.

For a complete verified installation, the existing runner interface is:

```sh
sudo /usr/lib/duduclaw/current/duduclaw-migrate run --plan /var/lib/duduclaw-migrate/plan.json
sudo /usr/lib/duduclaw/current/duduclaw-migrate status --plan /var/lib/duduclaw-migrate/plan.json
```

These are interface examples, not an end-to-end validated deployment recipe. A verified installer must provide the fixed plan and signed platform; this source tree does not yet generate a complete usable installation. The runner still refuses before outage without `duduclaw-migration-platform`. First-administrator issuance is now connected through a root-authenticated Unix socket and a loopback setup page: the token is carried in the URL fragment, and account creation plus its chosen password is atomic. Remote administrators need an SSH tunnel to that loopback port. The durable root-owned `/etc/duduclaw/migration-import.json` keeps a reboot from falling back to ordinary administrator setup when the volatile `/run` manifest disappears. None of these interfaces establishes successful full migration. Do not use `prepare --stop-after-keys` as an upgrade shortcut; it deliberately leaves the installation offline.

The review page requires an active Admin session, password and a fresh enrolled authenticator code. Its authorization lasts five minutes and is bound to the session. High-risk proposals require individual approval; bulk approval is limited to low-risk items. Approval and successful application are separate states. Unsupported categories remain blocked and require supported re-entry workflows. No old pending approval becomes a new authorization.

## Install AI CLIs for the service account

The service runs as `duduclaw`, with `HOME=/var/lib/duduclaw` and `PATH=/usr/sbin:/usr/bin:/sbin:/bin`. AI CLIs must be installed in one of those system PATH directories and be readable and executable by that account. `ProtectHome=yes` hides `/home`, `/root` and `/run/user`; a CLI such as `/home/operator/.local/bin/claude` is unavailable even if root can find it. A symlink in a system directory that points into a hidden home does not solve this.

For Claude Code, replace `<ver>` with the version you intend to install before running these commands:

```sh
cd / && sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>
sudo -u duduclaw -H claude auth login
```

Keep both `-H` and `--prefix /usr`: an operator's `~/.npmrc` prefix can otherwise install the CLI back into the operator's home. Install any configured Codex, Grok, AGY or Gemini CLI in the same service-readable system PATH and authenticate it as the service account using its own login procedure.

At the end of `service install --protected-linux` and `service upgrade --protected-linux`, the command checks the configured primary inference runtime and runtimes selected in agents' `agent.toml` files, reading the destination configuration after any migration. It checks executable access as the service account with the service PATH and excludes hidden paths. A CLI found only in the operator's home is reported as unavailable to the service. If the primary runtime is missing, the command prints a warning and exits nonzero. Use `--allow-missing-runtime` on either command only when deliberately completing runtime setup later; this flag does not install or authenticate a CLI. A nonzero runtime check at the end of installation does not undo installation work already performed. `service upgrade --protected-linux` runs this check first, before downloading a release or submitting an updater request, and changes nothing when it fails.

Migration copies only `.duduclaw` data, **not AI CLI installations or their login credentials**. When Claude is installed but the service account has no login, installation prints the service-account login command above. Authenticate that account separately; do not copy credentials from the operator's home. On gateway startup, a missing configured inference binary produces an ERROR with repair guidance; runtime status and doctor also show the protected-service instructions.

## Model connection data for isolated employees

An isolated employee that uses the OpenAI-compatible runtime reads its endpoints and API keys only from `/home/employee/.duduclaw-api/providers.json` in its own home; gateway accounts, configuration and environment variables are never used. The host administrator writes or removes this document as root:

```sh
sudo duduclaw service provider set --protected-linux --employee <employee-label> < providers.json
sudo duduclaw service provider set --protected-linux --employee-id <32 lowercase hex digits> < providers.json
sudo duduclaw service provider set --protected-linux --employee <employee-label> --clear
```

The document is read **from standard input only**: never from a command-line argument, an environment variable or a path. Running the command at a terminal without redirecting a file is refused with a hint showing the format. The limit is 65,536 bytes. Format:

```json
{"providers":[{"name":"main","api_key":"<key>","base_url":"https://provider.example/v1"}]}
```

- 1 to 32 providers with unique names; `base_url` must be https without user information, query or fragment; `protocol` (`chat_completions`, `responses`, `anthropic` or `gemini`) is optional. An invalid document is refused with a fixed message that never repeats its content.
- The key is stored in **the employee's own home** (owned by the employee, mode 0600) and is readable by that employee's runtime. Use a key dedicated to this employee; never a gateway or shared account key.
- With this command, every change needs host root. The write happens only while no run is live on the host and the employee is not in use; otherwise try again later. Every run rewrites the file. Success shows only the employee, the number of providers, and each provider's name and `https://host[:port]`, never the key.
- Remove the document with `--clear` (standard input is not read). Disabling the employee removes it as well; retiring the employee destroys it with the employee's storage.

### From the dashboard

The gateway also accepts the same document from the dashboard (the gateway side is in place; the dashboard page ships in a following package):

- Only a current Admin may set or remove it, after a fresh step-up: password plus second factor, valid for five minutes and bound to that dashboard session (the same step-up as connector accounts and migration decisions). Any allocated isolated employee can be targeted, including another person's Personal employee, as for connector accounts. Every attempt is audited with the actor, the employee, the result and the providers' names and endpoint origins, never the key or the endpoint path.
- The gateway relays the key to root in memory only; root writes it into the employee's home exactly as the host command does. The key is never written to the gateway's disk, databases, configuration, logs, audit or error messages, and the gateway wipes its in-memory copies after the hand-off. The same rules apply as for the host command; in addition, names, keys and endpoints containing a quote, a backslash or a control character are refused.
- The dashboard status shows, per employee, only the provider names, protocols, endpoint origins (`https://host[:port]`) and the time of the last confirmed dashboard change. **It reflects only changes made through the dashboard**: documents written or removed with the host command above are not visible there. Another person's Personal employees are hidden unless the Admin reveals them with the step-up (audited).
- While a run is live on the host or the employee is in use, the change is refused as busy; retry when the employee is idle. When the isolation service does not confirm a change, the result is reported as unconfirmed and the status stays as it was; sending the same change again is safe.

### When an employee cannot run

When an isolated run fails, the gateway journal has exactly one warning `isolated run refused` with the fields `runtime`, `agent` (the employee label), `reason`, `class`, `exit` (exit code, `signal`, `timeout` or `incomplete`) and `stderr_len`:

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep 'isolated run refused'
```

For the OpenAI-compatible runtime, `reason` is the worker's fixed reason (the list is in `crates/duduclaw-api-worker/README.md`, "Diagnostics"). Other runtimes, and any output that is not exactly one known reason, log `reason="unrecognised"`. The runtime's own output is never logged or shown. In a chat channel, a person allowed to see operator diagnostics sees one of these fixed sentences; everyone else sees the general failure message, and public website visitors only the visitor message:

| Sentence shown (zh-TW) | `class` | What to check |
| --- | --- | --- |
| 這位 AI 員工尚未設定模型連線資料，或設定內容無法使用；請管理員到『受保護模式 → 模型連線資料』檢查。 | `employee_credentials` | Model connection data exists for this employee, is valid, and names the requested provider (see above). |
| 連不到這位 AI 員工設定的模型服務；請管理員檢查端點位址與對外連線。 | `provider_transport` | `base_url` host, TLS, and that the egress policy allows that host. |
| 模型服務拒絕了這次請求；請管理員檢查這位 AI 員工的金鑰與模型名稱。 | `provider_refused` | API key, model name, account quota at the provider. |
| 模型服務的回覆無法使用；請管理員檢查端點是否為相容的服務。 | `provider_response` | `base_url` and `protocol` match the provider's API (`chat_completions`, `responses`, `anthropic`, `gemini`). |
| 這次工作超過時間上限而停止。 | `timeout` | The run's time limit, or a slow provider. |
| 受保護模式尚無這次工作的隔離執行授權，或所選執行設定尚未通過驗證；請由管理員檢查服務狀態。 | any other, or `unrecognised` | Readiness and the journal (`reason`, `exit`); see "Troubleshoot readiness". |

A turn refused **before a run was started** logs exactly one warning `isolated run not started` instead, with the fields `runtime`, `agent` (the employee label) and `site`, a fixed label naming where it was refused. A run that started but ended without the line above (its pipes were missing, or it exited successfully with an unusable result) logs `isolated run refused after start` with the same fields. A failed protected user turn has exactly one of these three lines; expected background refusals are described below. No request content, path or identifier other than the employee label is logged. The sentence shown to the person does not change.

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated run (not started|refused)'
```

Background model calls after a successful turn can produce `authorize.utility_call` (or another utility-call site) and, only for the employee's post-turn self-improvement call without a captured chat identity, `authorization.ingress.no_scope`. These refusals are expected in protected mode and do not mean the user's turn failed. They log `isolated background call not available in protected mode` at DEBUG, with one INFO notice per employee and site for the life of the service process (`protected mode: background model calls are not run for this employee`, subject to a bounded notice limit). A `no_scope` refusal for a chat turn, scheduled task, reminder or delegated work is a real refusal and is still logged as a warning (WARN), even inside an autonomous scope.

| `site` group | Labels | What to check |
| --- | --- | --- |
| Coarse gates (nothing consumed yet) | `preflight.model`, `preflight.runtime_policy`, `preflight.model_policy`, `preflight.redaction`, `preflight.interceptor`; `authorize.agent_dir_missing`, `authorize.effort`, `authorize.utility_call`, `authorize.work_dir_override`, `authorize.agent_dir`, `authorize.capabilities_policy`, `authorize.native_gate`, `authorize.runtime_kind`, `authorize.explore_lane`, `authorize.tool_restrictions`, `authorize.codex_home`, `authorize.product_policy`, `authorize.interceptor`, `authorize.runtime_id`, `authorize.max_turns`; `step_limit.agent_dir`, `step_limit.unavailable` | The employee's `agent.toml` (`[model] effort`, `[capabilities]`), the product policy for the runtime and model, redaction and CCR settings. |
| Authorization (the one-time run authority) | `authorization.ingress.*` — the person asking: `no_scope`, `consumed`, `capture.*` (the sign-in check made when the message arrived: `no_web_credential`, `web_identity`, `channel_conflict`, `channel_mapping`, `requester`, `user_db`, `user_inactive`, `epoch`, `blocking`), `agent_path`, `home_mismatch`, `no_turn`, `no_session`, `no_attended_proof`, `attended_proof_invalid`, `nested_dispatch`, `principal_mismatch`, `proof_expired`, `identity_recheck`, `blocking`; `authorization.issue.*` — issuing it: `boot_owner`, `product_authority`, `lifecycle`, `snapshot`, `epoch`, `principal`, `binding`, `policy_revision`, `tools`, `human`, `selectors`, `capability`, `users_recheck`, `policy_changed`, `lifecycle_recheck`, `snapshot_recheck`, `facts_recheck`; and `authorization.consumed`, `authorization.installed_mismatch`, `authorization.agent_label`, `authorization.home`, `authorization.principal_context`, `authorization.background_issue`, `authorization.ingress_issue` | The person's account is active, needs no password change and may assign work to this employee; the sign-in is still valid; the employee's binding (Shared organization or Personal owner) and allocation. |
| Plan | `request.encode`, `plan.readiness`, `plan.boot_owner`, `plan.home_mismatch`, `plan.employee_unconfirmed`, `plan.root_publication`, `plan.blocking`, `plan.unavailable` | Readiness; the employee is confirmed; the root publication offers this runtime to this employee. |
| Launch | `launch.readiness`, `launch.boot_owner`, `launch.boundary_unhealthy`, `launch.employee_unconfirmed`, `launch.employee_mismatch`, `launch.product_prepare`, `launch.binding`, `launch.blocking`, `launch.resolver`, `launch.connectors`, `launch.worker_plans`, `launch.factories`, `launch.dispatcher`, `launch.protected_launch`, `launch.authority_pairing`; `launch.bind.<step>` (after the run was reserved; `<step>` as in `isolated launch preparation refused`, below) | The employee's protected tool boundary (tool projection, connectors); for `launch.bind.*`, the table below. |
| Spawn | `spawn.prepare`, `spawn.not_protected`, `spawn.admission`; `spawn.backend.unavailable`, `.denied`, `.would_deadlock`, `.not_found`, `.timeout`, `.io`, `.protocol`; `spawn.start.permission_denied`, `.not_found`, `.timed_out`, `.other` | `spawn.backend.*` is the fixed class of the isolation service's (or its control socket's) refusal. The isolation service does not log a refused start, so a quiet `duduclaw-spawnd` journal does not rule this group out. |
| After start (`isolated run refused after start`) | `run.stdin_missing`, `run.stdout_missing`, `run.stderr_missing`, `run.input`, `run.output_drain`, `run.stderr_drain`, `run.wait`, `run.refused`; `output.parse`, `output.empty`, `output.utf8` | The runtime's result; `run.*` labels after the `isolated run refused` line add nothing new. |
| `unlabelled` | — | A refusal without a site: a defect, please report it. |

`spawn.backend.*` does not tell whether the gateway or the isolation service refused the start. When the gateway refuses it itself (before or instead of sending the start request), it also logs one warning `isolated spawn refused locally`, just before the `isolated run not started` line, with one fixed field `step` (no identifiers, paths or content). If a `spawn.backend.*` line has no such warning before it, the isolation service refused (it does not log that, see the Spawn row above).

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (spawn refused locally|run not started)'
```

| When | `step` | What it means |
| --- | --- | --- |
| Reserving the run | `admission`, `tracked_runs`, `reservation_reply`, `reservation_queue`, `endpoint_directory`, `endpoint_socket`, `endpoint_connect`, `peer_identity` | Readiness changed; the gateway's run table is full; the reservation reply was unusable (its expiry must be within 30 s of the gateway's clock); the employee already has 32 unfinished runs in this gateway; the control socket's directory, socket or peer checks failed. |
| Between reservation and start | `launch_prepare`, `policy_binding`, `task_binding`, `pty_size` | The protected launch context refused (the gateway re-checking the person's authority for this run, or the tool boundary issuing the run's capability: readiness, the employee's binding, the runtime's MCP registration); the prepared plan does not match the reserved run; the task was bound after the reservation ended; an invalid terminal size. |
| Building the start request | `admission`, `workspace_delivery`, `workspace`, `entry`, `reservation_state`, `resource_limits`, `inputs`, `attachments`, `request_invalid`, `connection_state`, `live_check`, `fd_plan` | Readiness changed; the workspace proof does not match; the plan names a workspace other than the employee's own; the run is unknown, belongs to another employee, was already started or its 30-second reservation expired; the runtime record has a zero output or input budget; the request exceeds the input budget; too many attachments; the request is invalid (for example a step limit outside 1–1000); the control connection is closed; the live authority check refused; the descriptor plan does not match. |
| After sending it | `admission_after_send`, `start_reply` | Readiness changed while the start was in flight (the run is cancelled); an unexpected reply. |
| After start, before the run's tools are served | `take_io`, `take_mcp`, `launch_activate` | The started run's standard streams (or terminal) were not handed over within the queue and startup time, or its MCP endpoint could not be taken from that handoff — the endpoint is then never activated; the protected launch context refused the run's MCP endpoint (see `isolated launch activation refused`, below). The started run is cancelled and its capability revoked. |
| Other control requests | `cancel_state`, `live_status_reservation`, `work_op`, `helper_op` | A cancel abandoned or already refused; a status reply naming another reservation; an operation not allowed on that path. |

When the step is `launch_prepare`, the gateway logs one more warning just before it, `isolated launch preparation refused`, with one fixed field `step` naming the check that refused (no identifiers, paths or content). The turn's `isolated run not started` line then shows `site="launch.bind.<step>"` instead of `spawn.backend.denied`.

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (launch (preparation|activation) refused|spawn refused locally|run not started)'
```

| `step` | What it means |
| --- | --- |
| `recheck.expired` | The run authority expired. It is issued when the protected run is requested, for 30 minutes and never beyond the web sign-in's own expiry; sign in again. A system clock jump also shows here. |
| `recheck.agent` | The employee is no longer in the gateway's live employee list. |
| `recheck.identity` | Since the run was authorized, the employee's lifecycle (allocation, confirmation, the root read-back) or the person's account facts (organization, access generation, policy epoch, sign-in epoch) changed — for example an employee approval or binding change still being applied, sign-out on all devices, or a second-factor change. |
| `recheck.policy_revision` | A policy input exists but cannot be read or parsed, the employee's `agent.toml` lacks `[agent] status = "active"`, or `config.toml` is missing. Inputs: the employee's effective `agent.toml`, `CONTRACT.toml` (optional), `preset_bindings.toml`, `org.toml`, `config.toml`, the custom redaction profile, `KILLSWITCH.toml`. |
| `recheck.revision_changed` | A policy input changed since the run was authorized: `agent.toml` `[capabilities]`, `[permissions]`, `[agent]` `reports_to`/`department`/`role`; `CONTRACT.toml`; the preset binding; the `org.toml` chain; `config.toml` `[delegation]`, `[acp]`, `[provenance]`, `[integrations]`, `[redaction]`; the custom redaction profile; `KILLSWITCH.toml`. Sessions, memory, `.claude/`, `.mcp.json`, `SOUL.md` and skills are not inputs. |
| `recheck.epoch`, `epoch_after_snapshot` | The person's account facts changed during the check (before or after the status request below). |
| `recheck.background` | Scheduled or delegated work only: its background record no longer authorizes the run. |
| `live_snapshot` | The isolation service's status reply for the reserved run failed or did not arrive within 250 ms (the run's control connection is then closed). Check the `duduclaw-spawnd` journal and host load. |
| `binding_mismatch` | The isolation service reports the run for another employee, or no longer in the reserved state. |
| `revoked_before`, `revoked_after` | The run was cancelled while it was being prepared (for example the turn was stopped). |
| `verify_blocking` | The gateway's pool for blocking authority checks stayed busy for 2 s, or the check took longer than 10 s (slow `users.db`/`apply.db` or root read-back). |
| `prepared_missing`, `state_lock`, `associations` | The launch was prepared twice, its state was poisoned, or the gateway's table of bound runs (4096) is full or already holds the run: a defect, please report it. |

After the gateway's own checks pass, the tool boundary registers the run (`inner_prepare.<substep>`); its steps:

| `step` | What to check |
| --- | --- |
| `inner_prepare.revoked`, `inner_prepare.revoked_after` | The run was cancelled while the tool boundary prepared it. |
| `inner_prepare.reservation.<class>` (`unavailable`, `denied`, `would_deadlock`, `not_found`, `timeout`, `io`, `protocol`) | The gateway's record of the reservation: `denied` — the 30-second reservation expired, its control connection closed, or it was already started; `not_found` — the run is unknown to this gateway. |
| `inner_prepare.plan_class`, `inner_prepare.plan_runtime`, `inner_prepare.plan_pty` | The plan prepared for this turn names another execution class, runtime or terminal mode than the spawn: a defect, please report it. |
| `inner_prepare.registry.publication_read`, `inner_prepare.registry.generation` | The gateway could not read root's runtime publication `/run/duduclaw-spawnd/publication.json` (owner, group, mode, other boot, or older than its 1-second validity: check that `duduclaw-spawnd` is running and not stalled), or it names another installed generation (restart the gateway after an update). |
| `inner_prepare.registry.runtime_missing`, `.runtime_duplicate`, `.mcp_disabled`, `.script_executable` | The publication has no (or two) records for this runtime, the record has `mcp: false`, or its executable is a script: the installed signed policy does not offer this runtime with tools. |
| `inner_prepare.registry.acceptance_missing` | The installed bundle has no MCP acceptance record for this runtime: install a bundle built with `--accept-mcp`. |
| `inner_prepare.registry.acceptance.<check>` (`runtime`, `version`, `runtime_digest`, `bridge_digest`, `projection_digest`, `inherited_fd`, `initialize_exit`, `parent_exec_exit`, `mcp_child_exec_exit`, `exclusive_config_exit`, `transcript_digest`), `inner_prepare.registry.projection_invalid`, `inner_prepare.registration_runtime`, `inner_prepare.projection` | The installed bundle's MCP acceptance record does not match the published runtime (it was accepted for another runtime binary, version or bridge, or by another release's policy tool). Reinstall the matching bundle; `duduclaw-isolation-policy verify` on the bundle names the same check. |
| `inner_prepare.issue.live_snapshot` | The live authority re-check at capability issuance failed (same causes as `recheck.*` and `live_snapshot` above). |
| `inner_prepare.issue.not_reserved`, `.reservation`, `.expiry`, `.time` | The isolation service no longer reports this exact reservation as reserved, or it expired (also after a system clock jump). |
| `inner_prepare.issue.principal`, `.binding`, `.policy_revision`, `.agent_slug`, `.binding_principal` | The authority captured for the turn differs from the live one (the person, the employee's binding or the policy changed meanwhile), or the person may not act for this employee's binding. |
| `inner_prepare.issue.grants` | The turn's resource grants (workspace, connectors, web origins) are refused for this employee. |
| `inner_prepare.issue.encode.<rule>` (`resource_mode`, `interval`, `duplicate_grant`, `other`) | The run capability does not encode: `interval` — the turn's authorization expired before the capability was issued (or the clock jumped); the others are a defect (please report it). |
| `inner_prepare.issue.random`, `.signer`, `.state_lock`, `.capacity`, `.duplicate`, `inner_prepare.employee_mismatch`, `.prepared_missing`, `.state_lock`, `.pending_duplicate`, `.argv`, `.clock`, `.registry.unreported` | The OS random source or the wall clock failed, or a defect (please report it). |
| `inner_prepare.verify_blocking` | As `verify_blocking` above, for the tool boundary's blocking checks. |

After the run has started, the tool boundary activates its MCP endpoint. A refusal there logs `isolated launch activation refused` with one fixed `step` (`inner_activate.<substep>`), then `isolated spawn refused locally step="launch_activate"`; the turn's site is `launch.activate.<step>`. The started run is cancelled and the run's capability revoked.

| `step` | What to check |
| --- | --- |
| `inner_activate.mcp_fd_missing`, `inner_activate.mcp_receipt.<class>` | The isolation service did not hand over the run's MCP endpoint (the run ended early, or its I/O handoff is missing). Check the `duduclaw-spawnd` journal. |
| `inner_activate.bridge.registry.*` | As `inner_prepare.registry.*` above, read again after the start. |
| `inner_activate.bridge.publish.*` (the `registry` names above), `.publish.policy`, `.publish.derive`, `.publish.record`, `.publish.registration_changed` | The publication changed between preparation and activation (an update or an entitlement change), or no longer offers this runtime to this employee. |
| `inner_activate.bridge.publish.run_receipt_read` | Root's per-run MCP receipt `/run/duduclaw-spawnd/mcp-<run>.json` is missing, from another boot, or older than 60 s. Check the `duduclaw-spawnd` journal. |
| `inner_activate.bridge.publish.receipt.<group>` (`generation`, `run`, `employee`, `runtime`, `digests`, `projection_digest`, `mount`, `acceptance`), `inner_activate.bridge.receipt_binding`, `.bridge.registration_runtime`, `.bridge.projection`, `inner_activate.projection_digest` | Root's read-back of the mounted tool configuration does not match what was prepared for this run. Reinstall the matching bundle; if it persists, report it. |
| `inner_activate.bridge.platform` | Not Linux. |
| `inner_activate.boundary_activate.live_mismatch`, `.live_snapshot`, `.time` | When the endpoint arrived the run was no longer running, the person's authority had changed, the live check failed, or the capability had expired. |
| `inner_activate.delivery`, `.receipt_control`, `.boundary_activate.delivery`, `.boundary_activate.unknown`, `.boundary_activate.binding`, `.boundary_activate.revoked`, `inner_activate.pending_missing`, `.revoked_after`, `.active_duplicate` | The endpoint does not belong to this run's reservation, or the run was cancelled meanwhile; otherwise a defect. |
| `inner_activate.boundary_activate.random`, `.boundary_activate.state_lock`, `inner_activate.state_lock`, `.clock`, `.verify_blocking` | The OS random source, the wall clock or the blocking-check pool failed, or a defect. |

Once activated, the gateway serves the run's MCP endpoint until the run is released. If that session ends abnormally first, the gateway logs one warning `isolated MCP session ended` with one fixed field `step` (no identifiers, paths or content). A run that ends normally (its runtime closes MCP between requests, then the run is released) logs nothing. The line comes from the session's own task, so the turn's `isolated run refused` line does not carry its step as a site: read the two lines together. An API-worker turn refused with `reason="MCP disconnected"` or `"MCP authority changed"` is usually explained by this line.

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (MCP session ended|run refused)'
```

| `step` | What to check |
| --- | --- |
| `second_request` | The runtime sent a request while another one was still unanswered (other than that request's cancel). Before this release, `tools/list` sent right after `notifications/initialized` ended every protected API-worker run this way; if it recurs, report it. |
| `peer_closed_before_request` | The runtime side never sent anything: the runtime failed before starting MCP, `/runtime/duduclaw-mcp-bridge` could not start, or descriptor 3 was not inherited. The turn's `isolated run refused` line (`reason`, `exit`) names the runtime's side. |
| `peer_closed_in_request` | The runtime closed while a request was being handled or answered: it exited, was cancelled or timed out, or its bridge died mid-call. |
| `revoked` | The run's capability was revoked by the 1-second sweep (the run's live check failed), the loss of the run's control connection, or the gateway's boundary stopping. The run's own release never logs this line. |
| `context.live_mismatch`, `context.live_snapshot`, `context.time`, `context.expired_after`, `context.revoked` | The live re-check of the run's authority (at every request and every 100 ms) refused: the isolation service no longer reports the run as running, the person's authority, the employee's binding or the policy changed (the same causes as `recheck.*` above), the status read failed or took over 250 ms, or the capability's window ended. |
| `context.serial_wait`, `context.blocking`, `context.clock` | One live re-check took over 2 s, the blocking-check pool stayed busy for 2 s or a check took over 10 s (slow `users.db`/`apply.db`, root read-back or status reply), or the wall clock failed. Check host load and the `duduclaw-spawnd` journal. |
| `admission_timeout`, `readiness` | The first live check took longer than the request limit (305 s); the employee's tool services are incomplete for this run's capability. |
| `frame_invalid`, `frame_too_large`, `frame_partial`, `window_limit` | The runtime sent something other than one JSON-RPC 2.0 message per line, a line over 256 KiB, a cut-off line, or more than 120 messages within 60 s. |
| `dispatcher.<class>` (`invalid`, `invalid_time`, `revoked`, `binding_mismatch`, `unavailable`) | The tool dispatcher refused a message (for example malformed tool arguments, an unsupported notification, or the run's turn no longer resolving), with its class. |
| `request_timeout` | One tool call, including waiting for its approval, took longer than 305 s. |
| `response_encode`, `write_timeout`, `write_failed`, `read_failed` | A response over 4 MiB, the runtime not reading its responses for 5 s, or the socket failed. |
| `socket_missing`, `socket_setup`, `agent_slug`, `subscribe` | The session could not start (the run was revoked meanwhile), or a defect. |

In protected mode, a refused turn whose cause the text classifier cannot name ends with the reason code `safety_gate` instead of `unknown`.

## Upgrade and uninstall

The separate updater cannot be installed yet (planned for a later release); current hosts upgrade as described in the single-binary section above. After migration and complete updater provisioning, an Admin requests a stable release in the dashboard or uses the installed CLI:

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux --version X.Y.Z
```

Only version and request ID cross the updater boundary. The updater verifies signed metadata, hashes, platform, expiry, version floor and serial; it activates a complete generation and checks live loaded versions/protocols before committing. Track the request through restarts. Only `committed` is success. Missing service/trust/readiness refuses the update, without invoking the old installer. Ordinary updates cannot downgrade; emergency rollback requires its separate signed authorization.

The removal entry point is `sudo duduclaw service uninstall --protected-linux`. For an isolation installation it first preserves unit definitions in `/var/lib/duduclaw-protected/uninstall-units`, creates persistent `/etc` masks, stops timers, sockets and services, then reads the masks back. It removes only the fixed installed binary paths after verifying stopped units, known empty cgroups, no service/employee processes and no mounts under known storage roots. It does not force or lazily unmount anything. Keys, data, system accounts, permanent UID mappings, updater state/generations and version floors remain for recovery and anti-rollback; retained masks prevent automatic restart. H's arbitrary custom storage-root adapter is still missing, so this is not complete removal acceptance for custom storage layouts. The uninstall was verified on a real Ubuntu 24.04 host after an employee had run; do not use an older uninstaller or delete retained state to bypass a refusal. After `service uninstall --protected-linux`, `duduclaw-bootstrap install` refuses to install again on that host, and this version provides no command to re-enable the product or clear the retained state; contact the vendor.

Employees' runs are part of the removal: their run units (`duduclaw-run-<id>.service`) and per-employee slices (`duduclaw-employees-<id>.slice`, created when an employee first runs) are stopped after the services and before `duduclaw-employees.slice`, `duduclaw-tcb.slice` and `duduclaw.slice`. These are transient units and are never masked; the masked parent slice and the masked, stopped spawnd keep them from starting. Every unit is asked to stop before anything is checked, so a refusal leaves the services stopped and masked, never masked and still running. The refusal names the first unit that blocked it: one still active after 60 seconds (stop it with `sudo systemctl stop <unit>`, then run the same uninstall command again), a unit the mask did not reach, or a `duduclaw` unit whose name is not one of the product's shapes (never stopped or masked; inspect it first). Running uninstall again is safe: units already masked or stopped are accepted. A host an older build left with units masked but still running (refusal "無法證明服務啟動已封鎖；未移除執行檔。"): run this build's uninstall again; it stops everything itself and also removes the masks the older build left at per-employee slice names. With only the older build at hand, first stop each per-employee slice still active (`systemctl list-units --all 'duduclaw-employees-*'`, then `sudo systemctl stop <slice>`), then run its uninstall again.

## Automatic upgrades

The nightly timer requests the separate updater at 03:30 local time with up to 20 minutes of random delay. `Persistent=false` prevents a missed night from running at morning boot. Its fixed helper invokes `/usr/libexec/duduclaw-updated-launch --check-latest`; `--dry-run` discovers a candidate without submitting Apply.

Candidate lookup runs as `duduclaw-dl`. The unsigned release index selects only a stable, non-draft candidate; the root service still requires the signed manifest. The requester does not install downloaded programs and has no legacy fallback. Enable the schedule only after the complete updater and trust state are installed and verified. Do not copy this development helper onto a v1.79.3 system. Use a reviewed, pinned copy of the [timer installer](../../scripts/linux-protected/auto-upgrade/README.md), not moving `main`.

## Troubleshoot readiness

The following commands inspect installed components; they do not certify the whole installation:

```sh
sudo duduclaw service readiness --protected-linux
sudo /usr/lib/duduclaw/duduclaw-spawnd --check
sudo systemctl status duduclaw-gateway.service duduclaw-spawnd.service duduclaw-egress.service duduclaw-updated.service
sudo journalctl -u duduclaw-gateway.service -u duduclaw-spawnd.service -u duduclaw-egress.service -u duduclaw-updated.service -n 100 --no-pager
```

| Symptom | Action |
| --- | --- |
| `UnsupportedKernel` | Check booted kernel and Landlock ABI; ABI 8/kernel 7.0 currently refuses. Keep the fail-closed check. |
| Missing, stale or incompatible readiness | Verify the complete generation, protocol versions, account registry and all services. `active` alone is insufficient. |
| UID/account/SSH/IAM failure | Resolve reserved-range conflicts, aliases, supplementary groups, login/cron/at or linger state. An NSS module other than `files`/`systemd` for passwd or group (for example SSSD or GCE OS Login) is not supported. An employee that stays not ready on an sshd host: check the journal for `SSH denial probe duduclaw-emp-<uid>: …` and `journalctl -u 'duduclaw-isolation-probe@*'`; never remove the `DenyUsers` drop-in. |
| Storage/quota failure | Verify supported backing-device chain, hard quotas, available capacity and reserved host space. Do not replace hard quotas with disk-usage polling. |
| DNS/egress failure | Verify the root-selected resolved Varlink or DNS broker route. Do not add private resolver IP exceptions to the egress service. DNS readiness (`host_dns_route_unverified`) is spawnd's own live check, not an installer receipt: about once a minute spawnd asks egress to resolve the signed policy's `dns_probe_name` through that route and needs at least one public answer. It stays unverified with a signed policy older than v4, while egress is not running, or when that name does not resolve from this host (check outbound DNS and the egress journal's `dns_probe` line). |
| Parser root failure | Inspect owner/mode and unexpected contents of both parser roots; never copy host `/etc` or data into them. |
| Migration platform or setup link unavailable | Keep the existing environment intact and obtain the complete verified integration. Do not copy old authority or bypass the migration gate. |
| Pending review/rotation | Finish supported confirmations, retries and fresh credential setup in Migration review; approval alone does not complete migration. |
| Update accepted but not committed | Follow the durable request status and readiness error; do not start a concurrent manual binary replacement. |

### Egress run record

Each isolated run with network access ends with exactly one JSON line in the egress journal: `run_id`, `profile_id`, `bytes` (both directions), `connection_attempts`, `terminal_code` and, from v1.83, `terminal_detail`, a fixed label for where the run ended. The line never contains a host name, address or request content.

```sh
sudo journalctl -u duduclaw-egress.service --no-pager | grep terminal_code
```

The per-second limits (`frames_per_second`, `bytes_per_second`) slow a run down; they do not end it. The run ends with a rate label only when one frame could not be admitted within `idle_timeout_ms`.

| `terminal_code` | `terminal_detail` | Meaning |
| --- | --- | --- |
| `closed` | `peer_closed` | The sandbox side closed the connection between frames after the work ended. |
| `unavailable` | `peer_closed` | A frame was interrupted or the connection failed. |
| `unavailable` | `revoked` | The spawn service ended the run, or the control connection was lost. |
| `resource_limit` | `total_bytes`, `total_connections` | The run reached the `run_caps` total. Raise the cap in `/etc/duduclaw/egress.json` only if the volume is expected. |
| `resource_limit` | `max_tunnels` | The run had `max_tunnels` connections open at once. |
| `resource_limit` | `frame_rate`, `byte_rate` | One frame could not pass the rate limit within `idle_timeout_ms` (for example, `bytes_per_second` below the 8 KiB frame size). |
| `resource_limit` | `invalid_limits` | The profile limits or run caps are invalid. |
| `timeout` | `idle_timeout`, `frame_timeout`, `write_timeout` | No traffic for `idle_timeout_ms`, a frame that was not completed, or a forwarder that did not accept a frame in time. |
| `invalid_request` | `invalid_frame`, `frame_too_large`, `protocol_state` | The forwarder sent a frame that does not decode, a frame larger than `max_frame_bytes`, or a frame in the wrong state. Report it as a defect. |
| any | `internal` | Internal egress failure. Report it with the journal line. |

When a destination accepts no data for `idle_timeout_ms`, egress closes only that connection, and the run continues. The CONNECT header limits (`max_header_bytes`, `max_headers`) apply only to the CONNECT request inside the employee sandbox. That request is refused with HTTP 431 and does not end the run.

### Account verification lines in the spawnd journal

When `registry_verified`, `accounts_locked`, `ssh_live_negative_checks`, `cron_at_acl_and_jobs_verified` and `external_iam_reservation_verified` fail together, spawnd's journal names the failing step. Each line is printed once, and again only when the step or account changes:

```text
duduclaw-spawnd: employee account verification failed: step=<label> account=<duduclaw-emp-NNNNN|->
duduclaw-spawnd: allocation failed: step=<label>
```

The first line comes from the host-wide account check. It checks every `duduclaw-emp-*` account in the employee range, including retired (tombstoned) ones. `account=-` means a host-level step. The second line is printed once for each failed employee allocation, at most 5 times per label in 10 seconds; further failures with that label are counted and reported when the 10 seconds end as one `duduclaw-spawnd: allocation failed: step=<label> suppressed=<count>` line. A pending SSH probe or the pending-reservation limit is not a failure and is not logged. The label is a fixed identifier: neither line contains file contents or error text. The readiness output and `readiness.json` keep their check names.

| Label | What failed |
| --- | --- |
| `login_defs`, `subids` | `/etc/login.defs`, `/etc/subuid` or `/etc/subgid` is unreadable or malformed. |
| `registry_rows` | The employee registry does not fit the host's uid ranges, or a reservation cannot be resumed. |
| `nss_modules` | passwd or group uses an NSS module other than `files`/`systemd`. |
| `enumerate` | `getent passwd`/`group` is unreadable, or has an alias or a foreign entry in the employee range. |
| `passwd_row`, `shadow_row`, `groups` | The account's passwd row (`/nonexistent`, `/usr/sbin/nologin`), shadow row (`!*`, expired) or group list (its own gid only) is wrong. |
| `cron_acl`, `at_acl` | `/etc/cron.deny` or `/etc/at.deny` does not deny the account, or an allow file overrides it. |
| `cron_spool`, `cron_tables` | The account has a user crontab, or a line in `/etc/crontab` or `/etc/cron.d` runs as it (an unreadable or non-root file there also fails). |
| `at_spool` | An at spool directory (`/var/spool/cron/atjobs`, `/var/spool/cron/atspool`, `/var/spool/at`, `/var/spool/at/spool`) holds an entry owned by the account, or exists but cannot be read, or lists an entry that disappears or cannot be inspected before its owner is checked. atd removes a job from `atjobs` when it starts the job, so a disappearing entry is not treated as harmless. The next refresh repeats the check, which clears a passing change such as another account's job finishing. |
| `linger`, `sessions` | A linger file exists, or `user@<uid>.service` or a logind session of the account is active. |
| `ssh_config`, `ssh_probe` | `sshd -t`/`sshd -T` does not deny the account or names no port, or the SSH denial probe has no Denied result. |
| `userdb_service`, `userdb_user`, `userdb_group` | `/run/systemd/userdb` is unreadable, or a userdb user or group record touches the employee namespace. |
| `local_identity`, `candidate`, `owner_inventory` | Allocation: an identity or alias already exists for the new account, a candidate uid lookup failed, or spawnd's file-owner inventory failed. |
| `login_barriers`, `create_group`, `create_user`, `lock_user`, `quarantine` | Allocation or recovery: the deny entries or sshd drop-in, groupadd, useradd (or a mail spool it created), the usermod/chage/loginctl lockdown, or stopping the account's processes failed. |
| `reserve`, `create_account`, `readback` | An allocation step failed with no finer label. For example, `reserve` means the employee uid range is exhausted. |
| `existing_row`, `persist_reservation`, `persist_final`, `store_poisoned`, `clock` | Allocation: the employee already has a retired or unresumable registry row; writing the registry before account creation or after the outcome failed; the registry store refuses writes after an earlier failed write in this process; or the monotonic clock could not be read. |
| `unlabelled` | An error outside spawnd's labelled checks. Allocation never produces it. |

The `at` package may stay installed: spawnd reads its spool as root and never runs `/usr/bin/at`. Do not edit deny files, spools or accounts by hand to clear a line. Fix the cause; the next refresh then prints `evidence complete`. A failed allocation retires its uid permanently and leaves the proposal Failed. After the cause is fixed, retry the failed item's compensation in Migration review. A new proposal then receives the next uid.

### Start refusal lines in the spawnd journal

When the gateway logs `isolated run not started` (for example `site="spawn.backend.unavailable"`), spawnd's journal has one line for that refused start:

```text
duduclaw-spawnd: start refused: step=<label>
```

The line has no employee id, run id, path or request content. The wire code sent to the gateway is unchanged. Labels from the admission gate can come from `ReserveRun`, `StartRun` or the queued run's admission.

The line is rate limited per label: within 10 seconds the first 5 refusals with a label are printed. Further refusals with that label are counted, and when the 10 seconds end spawnd prints one line with the count instead:

```text
duduclaw-spawnd: start refused: step=<label> suppressed=<count>
```

Every refusal is either printed or included in a `suppressed=` count. The printed lines plus the counts add up to the number of refused starts.

| Label | What refused the start |
| --- | --- |
| `frame`, `peer`, `request`, `fds`, `run_id` | Control socket: an undecodable frame, a peer that is not the gateway, a request that fails its own validation, descriptors that do not match it, or no random run id. |
| `clock`, `update_drain`, `stopping`, `not_reserved`, `reservation`, `pty`, `queue_full`, `launch_policy`, `input_bytes`, `input_fds`, `internal` | Admission: the clock went back, an update drain is closed, a stop of this employee is running, no or a wrong/expired reservation, a PTY mismatch, a full queue, an invalid launch policy, or no room in the input budgets. |
| `employee_not_admitted` | The employee is not a Ready row in spawnd's registry. |
| `not_ready(<check>)`, `report_stale`, `report_invalid`, `pty_budget`, `landlock_evidence`, `protocol`, `boot_clock`, `policy_missing`, `poisoned` | Host readiness: the named readiness.json check is false, the evidence report is older than 1 s or of another boot, there is no PTY budget or Landlock evidence, no verified policy is loaded, or spawnd needs a restart. |
| `scan_stale`, `scan_failed`, `scan_busy`, `intruder`, `view_busy` | The UID monitor's scan is old, failed or locked, it saw a process of this employee outside a run, or the admission view was locked. |
| `account_unknown`, `ssh_pending`, `account(<step>)` | The employee has no registry row, its SSH denial probe has no result yet, or its account verification failed at `<step>` (the labels of the table above). |
| `entitlement_missing`, `runtime_not_entitled`, `runtime_ambiguous`, `pty_not_allowed`, `turn_limit` | Entitlement: no entitlement for the employee, no or two entitled profiles of the requested runtime, PTY not allowed, or the step limit is outside the signed range. |
| `queue_timeout(<blocker>)`, `queue_cancelled(<blocker>)` | A queued run expired, or the gateway cancelled it, while `<blocker>` held it back: an admission label, `host_runs`, `employee_busy`, `host_ptys`, `cleanup` or `cleanup_incomplete`. |
| `uid_monitor_register`, `worker_mailbox`, `run_fence`, `cleanup`, `cleanup_incomplete`, `storage_usage`, `disk_quota`, `run_journal`, `storage_attach`, `storage_receipt`, `workspace` | Start, before any unit: monitor registration, the lifecycle mailbox, a cancelled run, the cleanup readback, storage usage or quota, the run journal, the image attach or its receipt, or the workspace. |
| `mcp_channel`, `egress_channel`, `egress_register`, `output_relay`, `stdin_pipe`, `supervisor_identity`, `nonce`, `bootstrap_plan`, `uid_monitor`, `mcp_projection`, `anchors`, `mounts`, `mcp_record`, `native_profile`, `helper_mounts`, `seal`, `trampoline_prepare`, `bootstrap_invalid`, `unit_policy` | Start, before any unit: a channel or pipe, the egress service's registration (unknown profile or no egress control connection), the bootstrap plan, the MCP projection, the publication anchors, the mounts, the native profile, the trampoline or the unit policy. |
| `unit_start`, `unit_cgroup`, `bind_unit`, `launch_poll`, `startup_timeout` | systemd refused the unit, the unit's cgroup or the trampoline binding failed, the launch failed before Running, or Running was not reached within the startup budget. |
| `host_gate`, `host_policy`, `host_start` | Test adapters only. |

An `employee_not_admitted` for an employee that is Ready in the allocation journal was a defect before this release: spawnd read its admitted employees only at start, so an employee allocated after spawnd started was refused until spawnd restarted. spawnd now publishes its admitted employees before it answers an allocation, entitlement, disable or retire. A `ReserveRun` sent after an allocation or entitlement reply is checked against the updated employees and entitlement. After a disable or retire reply, the employee is refused.

Container engines and hardware-dependent features remain unavailable through the protected employee environment. Every required component must pass its own checks before work resumes; neither a successful package test nor this guide is production acceptance.

## Released v1.80.x single-binary protected service

The following sections describe the protected service as released in v1.80.x, where the gateway is a single root-installed binary and `service upgrade --protected-linux` verifies and replaces it in-process. They remain the reference for those releases and for the credential-protection controls (engine socket policy and automatic recovery) that the isolation architecture keeps. Current builds keep this in-process upgrade for single-binary installs (see "Single-binary protected service" above). Only installs managed by the separate updater send an updater request instead, and those never replace the binary from the CLI.

Official GitHub updates bind the minisign trusted comment exactly to `duduclaw v{version} {archive-file-name}`, including the platform and architecture. Provider updates use `duduclaw-pro` for Pro archives and their own pinned signing key. Both the payload and the global signature covering the comment must verify. All archive installation paths require a verifier-created `VerifiedReleaseIdentity` bound to the archive digest, version, file and signed comment; an integrity-only check cannot authorize installation. Before replacement, the extracted binary must match the requested version. Protected upgrades inspect exactly one build-time record with fixed header `NUL + DUDUCLAW_RELEASE_ID:V1:`, a stable `X.Y.Z`, and suffix `NUL + DUDUCLAW_RELEASE_END + NUL`, without running downloaded code as root. Every occurrence of the magic `DUDUCLAW_RELEASE_ID` must be that complete record; missing, malformed, duplicate or conflicting records are refused. Older binaries without this record are refused too. Release signing checks the actual archived binary's marker against the tag, including Pro packaging. Ordinary updates check the binary’s `version` output. Any mismatch aborts before replacement or writing the protected version intent/floor. These checks apply when the installed updater includes this fix; older updaters must first be replaced through an administrator-reviewed installation.

Provider integration: replace the integrity-only call with `verify_release_archive_with_pubkey(bytes, checksum, signature, pinned_key, expected_version, expected_file)` and pass its result to `install_verified_binary(bytes, &identity)`. The expected version/file must come from the selected release, rather than the downloaded signature. The former single-argument installation API is intentionally unavailable.

## Automatic upgrades (optional)

`scripts/linux-protected/auto-upgrade/` is an optional nightly helper included in the source since v1.80.0. It also works with a v1.79.3 protected install because it only calls the existing `service upgrade --protected-linux --version` command. It runs at 03:30 local time plus up to 20 minutes of random delay. It installs only newer official signed releases through the existing verified upgrade command, restarting an active gateway. Choose a time without ongoing work. `Persistent=false` avoids catching up after a morning boot during office hours. The dashboard cannot install updates in this mode: it shows the new version and the manual `sudo` command, whether or not the timer is installed. Without this timer, upgrades remain manual.

Source tags up to v1.79.3 do not contain this directory; release tags from v1.80.0 do. Obtain the directory from a specific full commit SHA, such as the commit of the `v1.80.0` tag (shown as `<commit>` below; use the one you reviewed), and review it; do not fetch from moving `main` ([source location](https://github.com/zhixuli0406/DuDuClaw/tree/<commit>/scripts/linux-protected/auto-upgrade)). Linux release archives do not include `scripts/`. Review the four script/unit files and copy the directory to `/root/duduclaw-auto-upgrade` using the commands below. The installer rejects symlinks, non-regular files, ownership other than root:root, and group/other write permission on the source directory/files and every resolved ancestor through `/`; sticky `/tmp` directories are also rejected. It snapshots all four files into a root-only 0700 directory under `/run`, rechecks regular-file type and size (1 byte through 1 MiB), installs from that copy and cleans it up. Ownership checks do not authenticate code: compare the root copy against the pinned commit before executing it.

```sh
# Replace <commit> with the reviewed full commit SHA; obtain files from that SHA.
# Run from the reviewed checkout; the destination must not already exist.
sudo cp -r --no-preserve=ownership scripts/linux-protected/auto-upgrade /root/duduclaw-auto-upgrade
sudo chown -R root:root /root/duduclaw-auto-upgrade
sudo chmod -R go-w /root/duduclaw-auto-upgrade
sudo sh /root/duduclaw-auto-upgrade/install.sh
sudo /usr/local/sbin/duduclaw-auto-upgrade --dry-run
systemctl list-timers duduclaw-auto-upgrade.timer
sudo journalctl -u duduclaw-auto-upgrade.service
```
Do not execute `install.sh` as root directly from a user-writable directory. Another process could replace reviewed files before copying, so compare the root-controlled copy too. The installer does not verify source provenance; copying unreviewed code into root ownership does not make it safe. See the directory's README.

Dependencies are GNU coreutils (including `sort -V`), curl, grep, sed, tr and systemd, plus the existing upgrade command's host preparation requirements. The helper reads the root `version-floor`; a missing or invalid floor, or any `version-pending`, fails without upgrading and needs manual attention. It selects the highest plain `vX.Y.Z` tag among the latest 30 GitHub releases, ignoring `desktop-v*` and suffixed tags. Equal or older versions exit successfully without downloading the large archive. API failure or no eligible tags prevents upgrades. This is not a complete history scan; investigate manually if those 30 entries contain no gateway release.

Extraction supports minified and pretty-printed JSON, checks the outer array envelope and matches plain version tags; it does not validate the full JSON grammar or parse the `prerelease` flag. Tags with suffixes such as `-rc` are excluded, but a plain `vX.Y.Z` marked prerelease remains eligible. Unauthenticated API requests do not return drafts. If a release is published before its Linux archives finish uploading, that night's upgrade fails and the next night retries; check the journal.

API responses are capped at 8 MiB with a 60-second timeout. On older curl, `--max-filesize` only applies with a known Content-Length, so the response is streamed into a temporary file in a root-only 0700 directory under private `/tmp`, checked for actual size before parsing, and cleaned on exit. `PrivateTmp=yes` supplies writable private `/tmp`; host writes are limited to `/opt/duduclaw` and `/var/lib/duduclaw-protected`. The service has a one-hour start timeout for download retries plus install/restart. A plain `vX.Y.Z` marked prerelease on GitHub will still be installed if newer and signature verification succeeds. The archive minisign signature is the trust anchor; version selection itself is not signed.

Remove the schedule, helper and units, keeping the gateway and its data. If an upgrade is running, uninstall disables the timer but refuses removal; wait for completion and retry:

```sh
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

## Protection and limits

**Feature limits:** Protected native mode blocks container engines with `InaccessiblePaths` and isolates devices with `PrivateDevices`. Container-backed task sandboxes, computer use and hardware-dependent features are unavailable. This is intentional: AI employees must not reach a container engine.

The launcher validates the executable, its ancestors and its own installation as root-owned and not writable by other users. It checks the dedicated account, clears its remaining processes, opens the key, then drops uid/gid, supplementary groups and all capabilities and enables `no_new_privs`. Before intake the gateway becomes non-dumpable. It verifies its systemd parent, the exact cgroup v2 entry (only `0::/system.slice/duduclaw-gateway.service`, or `0::/duduclaw.slice/duduclaw-tcb.slice/duduclaw-gateway.service` when the unit carries `Slice=duduclaw-tcb.slice`, as the isolated layout's unit does), executable ownership and permissions, privilege state and denied key-path access. Native installation, the launcher before opening the key, and Rust attestation all refuse non-initial user namespaces. `/run/.containerenv`, `/run/systemd/container`, or `container=` in PID 1’s environment also cause refusal. The root installer and launcher must read `/proc/1/environ`; the unprivileged gateway may skip only a permission-denied read. Detected containers retain the container rules rather than gaining native protection by setting a mode variable.

Under the default `revoke` policy, known Docker/Podman/containerd socket paths must be absent or inaccessible: both the write-access probe and a nonblocking connection must be denied. `DOCKER_HOST` must be unset. Custom slices, cgroup v1/hybrid, unexpected sudo output and uncertain probes fail closed. Startup refusal appears in the journal and/or credential-protection card; do not weaken permissions to bypass it.

The unit additionally applies:

```ini
InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
LockPersonality=yes
SystemCallArchitectures=native
```

As specified by [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html), each `-` prefix ignores a missing path. These settings hide common engine endpoints, isolate hardware devices, prevent kernel/module/cgroup changes, and restrict personality changes and syscall architectures. `RestrictSUIDSGID` is deliberately omitted: on systemd 255 it makes every `openat2` call fail, and the gateway opens files with `openat2` path-resolution restrictions. The launcher creates no set-ID files and the gateway runs with `no_new_privs`. `RestrictNamespaces` is deliberately omitted: the gateway’s Codex/bwrap task sandbox needs user, mount, PID and network namespaces.

At startup and every 60 seconds, the gateway checks common engine socket reachability and supplementary groups. Under the default `revoke` policy, a failed periodic check zeroizes the held key in place, changes protection to Refused and logs a warning; personal connections remain paused until a fresh root intake succeeds. The monitor keeps running after revocation; automatic recovery is described below. `/proc/net/unix` is also inspected for listening abstract sockets. A slash-delimited name component must equal `docker`, `containerd`, `podman`, `buildkit` or `lxd`, or start with one of those names followed by `.`, `-` or `_`, to become a candidate; arbitrary substrings do not match. Abstract name matches alone do not cause refusal: the gateway makes a nonblocking connection with a short timeout and checks `SO_PEERCRED`; only a reachable peer with uid 0 is considered a risk. Unreachable candidates are ignored. The table is read as raw bytes, so a non-UTF-8 name cannot blind checks of other sockets. Every row must parse completely: malformed fields, invalid flags, ambiguous names or newline continuations refuse intake and revoke a held key. A table read failure also fails closed in every state; after revocation it resets the consecutive-pass count. Before accepting a safe table result, a mandatory `NETLINK_SOCK_DIAG` dump checks length-delimited `UNIX_DIAG_NAME` values, including non-engine names, for embedded newlines. This catches even a newline followed by a syntactically valid forged row. An unavailable, malformed, interrupted, oversized (over 8 MiB) or timed-out (250 ms) dump also fails closed; the kernel must support UNIX socket diagnostics. Both checks are periodic observations, not an atomic snapshot of all sockets.

A reachable abstract socket matching exactly `@docker_cli_` followed by 32 lowercase hexadecimal digits with peer uid 0 is always classified as an engine socket. The default `revoke` policy revokes the key; an administrator-selected `warn` policy keeps it and displays a persistent warning. The socket name itself provides no exemption. Name recognition is only a diagnostic hint: this usually means someone ran the `docker` command as root on this host (for example, `sudo docker compose …`). With `revoke`, personal accounts pause while it runs and normally recover a few minutes after it ends, once checks pass and automatic recovery is available. To avoid this, run docker as a regular user in the docker group, or run development containers on another host.

AI employees' Bash/tool processes still run as the gateway's uid. Protection relies on non-dumpable memory, the root-owned key and binary, no sudo/linger, and denied engine sockets. Employees can still modify their writable data, stop the gateway, request authorized operations and read access tokens issued for their own turn. The host must not grant the `duduclaw` account access to container engines or other privileged brokers. These checks cover common endpoints at startup and once per minute; they are not an unconditional isolation guarantee. Unrecognized abstract socket names, engine sockets at other paths, privileged brokers, other privilege paths, host root compromise and malicious code already running inside the gateway are not covered. A new exposure can exist between checks. The root-owned executable check prevents modification by the service uid; it is not signature verification against a hostile root administrator.

The key is not a general sandbox. While it is held, the existing in-process ONNX restriction also applies: NER redaction, local embeddings and Kokoro are unavailable; use regex redaction where required. Backups containing both the data and root key can decrypt credentials. Replacing only the dashboard's protection key does not prevent replay of old data; after a suspected root-key leak, stop the service, securely rotate the root key as host administrator, reset protection and reconnect accounts.

Native systemd installation, sudo variants and socket probes require acceptance testing on the target distribution. Do not treat unit tests or macOS compilation as native Linux acceptance.

## Choose the engine socket policy

The default `engine_socket_action = "revoke"` refuses key intake or revokes a held key when an engine control interface is reachable. A host administrator can select `warn` when availability on a shared development host is necessary. This exception applies only to a confirmed `engine_socket_reachable` finding in native Linux mode, at intake and during the 60-second checks. All other failures still refuse or revoke the key, including supplementary groups, executable permissions, cgroup/unit, user namespace, container environment, accessible key paths, unreadable or malformed socket tables and failed diagnostics. It does not change systemd isolation or grant access to container-backed features.

With “Warn only”, DuDuClaw keeps personal Google and other accounts available when a program on this host exposes a container engine control interface. Programs that control the container engine effectively have full host privileges; if an AI employee can reach it, the employee may be able to read the decryption key. Use this setting only when you can confirm AI employees cannot reach the engine and you need containers on the same host. Safer options are to move development containers to another host or run docker as a regular user in the docker group.

Run these commands from your own terminal, outside an AI session. Use the verified root-owned binary; if `duduclaw` is not in the root PATH, replace it with `/opt/duduclaw/bin/duduclaw`:

```sh
sudo duduclaw service protected-linux policy show
sudo duduclaw service protected-linux policy engine-socket warn --yes
sudo duduclaw service protected-linux policy engine-socket revoke
```

The command requires root and refuses AI sessions. Switching to `warn` prints the risk explanation and requires `--yes`. The sole authority is `/etc/duduclaw/protected-policy.toml`, never the service account's `config.toml`. The gateway opens it through pinned directory/file descriptors at intake and before each periodic decision. It requires a regular root-owned file, one hard link, a bounded size, strict syntax, and root-owned parent directories; neither the file nor its parents may be writable by group or others. Missing, unreadable or invalid policy falls back to `revoke`. The root command writes a temporary file, synchronizes it, renames it atomically and synchronizes the directory.

Policy changes append records to the root-owned `/etc/duduclaw/protected-policy-audit.jsonl`: a synchronized intent before writing the policy, then a change record after the atomic policy write. The gateway imports these records into its security audit index on the next read. Under `warn`, a reachable engine logs a WARN with probe details and the credential-protection panel keeps a warning visible while the condition remains. The warning's start and end each create an audit event; repeated 60-second observations do not create repeated transition events. The panel shows the current policy, risk and exact command to switch, with no write control. Changing policy takes effect on the next check; a previously revoked key still needs a fresh root-launcher intake through the recovery process below.

Warning transitions are synchronized to `<home>/state/engine-socket-warning-events.json` before import and acknowledged after both audit copies succeed. This service-owned journal is diagnostic and never grants policy authority. The root audit log is bounded at 4 MiB; the root command refuses further changes when full until an administrator archives it. Existing policy directories must be readable and traversable by the service; the command refuses restrictive existing directories rather than silently changing their permissions.

Root policy changes and runtime warnings have separate event allowlists and `root:` / `runtime:` ID namespaces. Every import pass rebuilds the root slice first from the pinned root-owned log, deduplicating IDs within that rebuild; service-owned JSONL and SQLite receipts never acknowledge root imports. Malformed runtime rows are skipped, counted and warned without blocking root imports. The dashboard re-reads policy changes from the root log, ignores service-owned change copies, and labels imported events `root` or `runtime`; runtime diagnostics remain forgeable by the service uid.

## Recover after an engine socket exposure

Another program on the host, often a container started by a developer, can expose a root-owned engine control interface even when the `duduclaw` account is not in the Docker group. The warning and credential-protection status identify the path and condition (write access, successful connection or unexpected errno), or the abstract socket name and peer uid. The latest revocation reason, detail and UTC time are saved in `<home>/state/root-key-status.json` (`<home>` is normally `/var/lib/duduclaw/.duduclaw`). No key material is recorded. Use the finding to locate and stop the program exposing that interface. Run development containers on a different host; if the finding names an old containerd shim, upgrade containerd.

Automatic recovery defaults to enabled, but first requires verification that the installed root-owned unit exactly matches the current template, including the start limit and restart-on-exit-75 directive. A root-owned reload-success marker must contain exactly the same unit bytes. Install and upgrade invalidate the marker before changing the unit and atomically write it only after `daemon-reload` succeeds and a root-run `systemctl show` confirms the effective start interval, burst and forced-restart exit status, including any drop-in overrides. If these checks fail, the marker remains absent and recovery stays off. Missing, unreadable or different units disable automatic restart and report that an administrator must run `service upgrade --protected-linux`. Upgrade atomically replaces the unit and launcher when their content differs and runs `daemon-reload`; existing installations must complete this step too. Automatic recovery applies to `engine_socket_reachable` and `engine_socket_probe_failed` (probe failures previously shared the reachable code). Both policies still refuse incomplete probes; recovery requires subsequent successful checks. After revocation, the same checks continue every 60 seconds. Three consecutive passes request one normal graceful gateway shutdown; any failed check resets the count. The gateway exits with code 75, and systemd restarts the root launcher. The launcher and fresh gateway repeat the full intake checks before receiving a key. Recovery never restores a key in the refused process and never restarts a process that still holds it. Executable ownership or permissions, supplementary groups, cgroup/unit, user namespace, container environment and accessible key-path failures require administrator repair; they do not trigger automatic recovery.

The root-managed systemd unit enforces the hourly limit with `StartLimitIntervalSec=3600` and `StartLimitBurst=4`: four total starts per systemd counting interval, shared by recovery, normal failures and manual starts. The initial start consumes one slot, leaving at most three further starts in that first interval; a later interval permits at most four. This deliberately gives failures no separate allowance. If systemd reaches `start-limit-hit`, it stops automatic restart attempts; waiting alone does not schedule another attempt. After repairing the cause, the host administrator must start the service again once allowed (or explicitly reset the failed state). See the [systemd start-limit reference](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html#StartLimitIntervalSec=interval). The rolling-hour JSON budget in `<home>/state/root-key-recovery.json` is advisory only, as are the lock and diagnostic history: employees with the service uid can alter or suppress them, but cannot reset systemd's counter. State files are opened relative to a pinned directory descriptor, with no symlink following and owner, mode and single-link checks. Diagnostics use known reason/recovery values, validated RFC3339 timestamps and at most 512 bytes of escaped detail; CLI, logs and RPC payloads escape terminal controls and invisible formatting characters. If the state cannot be read or written, automatic recovery is blocked and personal connections stay paused; inspect the reported error and repair storage access. The restart also briefly interrupts other gateway connections. To retain manual restart behavior, put this in `config.toml`:

```toml
[credential_protection]
auto_recover = false
```

Newly installed units include `RestartForceExitStatus=75` alongside `Restart=on-failure`, and the two start-limit settings in `[Unit]`. `service upgrade --protected-linux` writes the unit and launcher templates embedded in **the version that runs the upgrade**. The first upgrade from v1.79.3 runs the old program, so it replaces only the executable and keeps the old unit: that unit already restarts code 75 through `Restart=on-failure`, but has no hourly limit, so automatic recovery stays off. After that upgrade, run the `service install --protected-linux` command below once to write the new unit and launcher and enable automatic recovery; later upgrades refresh them automatically when their content differs. A host administrator can reset the systemd counter; the home JSON cannot. After verifying and installing the new release, a host administrator can refresh the embedded launcher and unit with `sudo /opt/duduclaw/bin/duduclaw service install --protected-linux`. This preserves the data and root key, reloads systemd and restarts an active gateway. If the unit has local overrides, review them too.
