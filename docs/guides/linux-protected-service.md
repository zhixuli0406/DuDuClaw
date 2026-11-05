# Protected native Linux service

The v1.80 architecture separates the gateway, AI employees, document parsing and privileged updates. Each confirmed employee receives a permanent Linux identity; each run receives its own process, filesystem and network namespaces. An unavailable isolation component stops employee execution.

**Development status:** signed inactive-generation staging, the installer migration handoff, first-administrator setup, read-only installation checks and coordinated fixed-path removal are implemented. Complete fresh installation, signed activation, the migration platform adapter and the gateway composite-readiness adapter remain incomplete. This round has no live root installation/removal acceptance. An `active` service or staged generation is not isolation readiness. See the [implementation and acceptance record](../architecture/native-linux-isolation.md).

## What changes for customers

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

The host needs trusted system tools including `getent`, `id`, `useradd`, util-linux, systemd tools and curl. Loop-backed employee storage requires ext4 or XFS on a supported non-thin backing-device chain, with room for employee quotas and the TCB reserve. Unknown backing layouts or inadequate capacity refuse readiness. External IAM/NSS must permanently reserve the employee UID range.

## Installation and readiness

Run from a human administrator's terminal, outside an AI session. Obtain the complete release for x86_64 or aarch64 and verify it in a root-owned staging directory with non-writable ancestors before executing any program as root. Linux archives contain separate component binaries; copying only `duduclaw` cannot install this architecture.

A complete installation requires these roles and inputs:

| Area | Required installation state |
| --- | --- |
| System identities | `duduclaw`, `duduclaw-runsup`, `duduclaw-egress`, `duduclaw-dl`, `duduclaw-dns`; static system IDs below `SYS_UID_MAX`, outside DynamicUser and employee ranges |
| Employee identities | Permanently recorded IDs, normally 70000–99999; allocated only after employee confirmation |
| Trusted programs | Gateway, spawnd, trampoline, egress/forwarder/DNS broker, updater/frozen launcher, parser, importer, migration command and MCP stdio bridge; root-owned executable files |
| Services | Gateway, spawnd socket/service, egress socket/service, optional DNS broker, updater, parse socket/template and TCB/employee slices |
| Parser roots | `/usr/lib/duduclaw/parse-root` and `/usr/lib/duduclaw/parse-root-tree`, root:root 0755; initially empty, never populated with host data |
| Signed state | Bootstrap trust, compatible generation, runtime registry, updater slots/state and migration/readiness integration |

The parser may leave the known empty mountpoint directories described in its [installation reference](../../crates/duduclaw-parse-worker/README.md). Unexpected content must be investigated; do not recursively clear either parser root. Spawnd owns employee SSH, cron, at and user-manager lockdown and verifies effective host state. Do not hand-edit those controls to make a failed check pass.

The installer validates the complete fixed-path component set before preparing accounts, directories and units. Linux archives now include the frozen updater launcher. Offline release tooling can assemble a bootstrap inbox from signed metadata and artifacts. The independently authenticated `duduclaw-updated --bootstrap-stage` verifier checks its compiled root pin, threshold root signatures, target signature and all component hashes before publishing an **inactive generation**. It does not select `current`, create trust/state/slots, verify the legacy transition dual signature or activate services. No complete fresh-install recipe is available yet.

Run `sudo duduclaw service readiness --protected-linux` (the `ops` alias is equivalent) to inspect binaries, system accounts, directory permissions, unit contents and actual systemd properties. The report separates these checks from composite isolation verification. Until I1 supplies that adapter, `MissingComposite` always refuses readiness; the command cannot certify employee execution. Custom DNS drop-ins are not integrated and currently refuse the unit check. Reinstalling an already managed generation performs these read-only checks instead of rewriting its programs or units.

## Migrate from v1.79.3

Do not replace the old binary in place or copy its databases into the new home. The old environment shares a UID with employees, so its authority data must be treated as untrusted. The installer now routes the v1.79.3-to-v1.80 boundary, including an unknown installed floor, into G's migration flow using the fixed root-owned plan `/var/lib/duduclaw-migrate/plan.json`. Missing or invalid plans refuse before stopping the old service; there is no legacy in-place fallback.

The administrator flow is:

1. Obtain the independently published SHA-256 values for the bootstrap verifier and migration package, plus the root-metadata fingerprint, from both the official website and authenticated license console. Values must agree. Save downloads as files in a root-owned 0700 directory; use the system `sha256sum -c` before executing the verifier. The old archive signature alone does not establish the new trust root. Do not pipe downloads into a shell.
2. The verified installer validates bootstrap signatures, prepares the signed platform and creates an immutable root-owned migration plan. Never generate authority or a success receipt by editing that plan yourself.
3. The migration runner verifies the platform **before stopping the old service**. It then disables old activation paths, quarantines the legacy tree, writes a verified encrypted snapshot, rotates keys and starts a clean import-only gateway. Low-privilege importers turn recognized content into bounded records; authority becomes inactive proposals.
4. Once isolation is verified, establish the fresh administrator account through the installer-provided setup flow. Old sessions, passwords, MFA material and CLI login state are not restored. Enroll MFA before reviewing high-risk proposals.
5. In **System → Migration review**, inspect full values, approve or reject proposals, retry failed applications and complete required credential rotation. Employee IDs and storage are allocated only after confirmation. Reconnect personal accounts under their new owner.
6. Completion requires fresh application readback and zero outstanding confirmations/rotation obligations. `AwaitingAdministrator` means migration is still pending. Keep the root-only recovery snapshot until the documented retention/recovery checks allow removal.

For a complete verified installation, the existing runner interface is:

```sh
sudo /usr/lib/duduclaw/current/duduclaw-migrate run --plan /var/lib/duduclaw-migrate/plan.json
sudo /usr/lib/duduclaw/current/duduclaw-migrate status --plan /var/lib/duduclaw-migrate/plan.json
```

These are interface examples, not an end-to-end validated deployment recipe. A verified installer must provide the fixed plan and signed platform; this source tree does not yet generate a complete usable installation. The runner still refuses before outage without `duduclaw-migration-platform`. First-administrator issuance is now connected through a root-authenticated Unix socket and a loopback setup page: the token is carried in the URL fragment, and account creation plus its chosen password is atomic. Remote administrators need an SSH tunnel to that loopback port. The durable root-owned `/etc/duduclaw/migration-import.json` keeps a reboot from falling back to ordinary administrator setup when the volatile `/run` manifest disappears. None of these interfaces establishes successful full migration. Do not use `prepare --stop-after-keys` as an upgrade shortcut; it deliberately leaves the installation offline.

The review page requires an active Admin session, password and a fresh enrolled authenticator code. Its authorization lasts five minutes and is bound to the session. High-risk proposals require individual approval; bulk approval is limited to low-risk items. Approval and successful application are separate states. Unsupported categories remain blocked and require supported re-entry workflows. No old pending approval becomes a new authorization.

## Upgrade and uninstall

After migration and complete updater provisioning, an Admin requests a stable release in the dashboard or uses the installed CLI:

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux --version X.Y.Z
```

Only version and request ID cross the updater boundary. The updater verifies signed metadata, hashes, platform, expiry, version floor and serial; it activates a complete generation and checks live loaded versions/protocols before committing. Track the request through restarts. Only `committed` is success. Missing service/trust/readiness refuses the update, without invoking the old installer. Ordinary updates cannot downgrade; emergency rollback requires its separate signed authorization.

The removal entry point is `sudo duduclaw service uninstall --protected-linux`. For an isolation installation it first preserves unit definitions in `/var/lib/duduclaw-protected/uninstall-units`, creates persistent `/etc` masks and reads them back, then stops timers, sockets and services. It removes only the fixed installed binary paths after verifying stopped units, known empty cgroups, no service/employee processes and no mounts under known storage roots. It does not force or lazily unmount anything. Keys, data, system accounts, permanent UID mappings, updater state/generations and version floors remain for recovery and anti-rollback; retained masks prevent automatic restart. H's arbitrary custom storage-root adapter is still missing, so this is not complete removal acceptance for custom storage layouts. Live root removal has not been run this round; do not use an older uninstaller or delete retained state to bypass a refusal.

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
| UID/account/SSH/IAM failure | Resolve reserved-range conflicts, aliases, supplementary groups, login/cron/at or linger state. External IAM reservation needs administrator evidence. |
| Storage/quota failure | Verify supported backing-device chain, hard quotas, available capacity and reserved host space. Do not replace hard quotas with disk-usage polling. |
| DNS/egress failure | Verify the root-selected resolved Varlink or DNS broker route. Do not add private resolver IP exceptions to the egress service. |
| Parser root failure | Inspect owner/mode and unexpected contents of both parser roots; never copy host `/etc` or data into them. |
| Migration platform or setup link unavailable | Keep the existing environment intact and obtain the complete verified integration. Do not copy old authority or bypass the migration gate. |
| Pending review/rotation | Finish supported confirmations, retries and fresh credential setup in Migration review; approval alone does not complete migration. |
| Update accepted but not committed | Follow the durable request status and readiness error; do not start a concurrent manual binary replacement. |

Container engines and hardware-dependent features remain unavailable through the protected employee environment. Every required component must pass its own checks before work resumes; neither a successful package test nor this guide is production acceptance.
