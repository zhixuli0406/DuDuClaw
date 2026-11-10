# Protected native Linux service

The v1.80 architecture separates the gateway, AI employees, document parsing and privileged updates. Each confirmed employee receives a permanent Linux identity; each run receives its own process, filesystem and network namespaces. An unavailable isolation component stops employee execution.

**Development status:** signed inactive-generation staging, the installer migration handoff, first-administrator setup, read-only installation checks and coordinated fixed-path removal are implemented. Complete fresh installation, signed activation, the migration platform adapter and the gateway composite-readiness adapter remain incomplete. This round has no live root installation/removal acceptance. An `active` service or staged generation is not isolation readiness. See the [implementation and acceptance record](../architecture/native-linux-isolation.md).

**What you can install today:** `service install --protected-linux` does not install spawnd, egress or the separate updater, and nothing creates the migration plan. Until the isolation fresh bootstrap lands (planned v1.82.0), the protected service is the v1.80 single-binary gateway service described in [Single-binary protected service (current releases)](#single-binary-protected-service-current-releases). The per-employee isolation described in this guide does not apply to it.

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

## Single-binary protected service (current releases)

Until the isolation components can be installed (planned v1.82.0), `service install --protected-linux` installs the v1.80 single-binary protected gateway: the root-owned program `/opt/duduclaw/bin/duduclaw`, the launcher `/usr/local/libexec/duduclaw/protected-launcher`, the unit `duduclaw-gateway.service` and the version floor `/var/lib/duduclaw-protected/version-floor`. AI employees run as the gateway account, as in v1.80.

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

Without `--version` it uses the latest release. The command checks the configured AI CLIs first (see below), then downloads the official release, verifies its checksum, signature and embedded version, replaces the program atomically, refreshes the unit and launcher when their content differs, recertifies automatic recovery, records the new floor and restarts an active gateway. The target must be newer than the program that runs the command, and that program must not be older than the recorded floor (its unit and launcher would be older), so run the installed `/opt/duduclaw/bin/duduclaw`. To move the service to a newer binary you have already verified, run that binary's `sudo ./duduclaw service install --protected-linux` instead; reinstalling keeps the data and root key only while the version floor exists. `service uninstall --protected-linux` removes the floor but keeps the data, root key and account, so a later install is refused as a host with an unknown floor.

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
| Parser roots | `/usr/lib/duduclaw/parse-root` and `/usr/lib/duduclaw/parse-root-tree`, root:root 0755; initially empty, never populated with host data |
| Signed state | Bootstrap trust, compatible generation, runtime registry, updater slots/state and migration/readiness integration |

The parser may leave the known empty mountpoint directories described in its [installation reference](../../crates/duduclaw-parse-worker/README.md). Unexpected content must be investigated; do not recursively clear either parser root. Spawnd owns employee SSH, cron, at and user-manager lockdown and verifies effective host state. Do not hand-edit those controls to make a failed check pass.

The installer validates the complete fixed-path component set before preparing accounts, directories and units. Linux archives now include the frozen updater launcher. Offline release tooling can assemble a bootstrap inbox from signed metadata and artifacts. The independently authenticated `duduclaw-updated --bootstrap-stage` verifier checks its compiled root pin, threshold root signatures, target signature and all component hashes before publishing an **inactive generation**. It does not select `current`, create trust/state/slots, verify the legacy transition dual signature or activate services. No complete fresh-install recipe is available yet.

Run `sudo duduclaw service readiness --protected-linux` (the `ops` alias is equivalent) to inspect binaries, system accounts, directory permissions, unit contents and actual systemd properties. The report separates these checks from composite isolation verification. Until I1 supplies that adapter, `MissingComposite` always refuses readiness; the command cannot certify employee execution. Custom DNS drop-ins are not integrated and currently refuse the unit check. Reinstalling an already managed generation performs these read-only checks instead of rewriting its programs or units.

## Migrate from v1.79.3

Do not replace the old binary in place or copy its databases into the new home. The old environment shares a UID with employees, so its authority data must be treated as untrusted. The installer now routes the v1.79.3-to-v1.80 boundary, including an unknown installed floor on a host that is not provably new (see the single-binary section above), into G's migration flow using the fixed root-owned plan `/var/lib/duduclaw-migrate/plan.json`. Missing or invalid plans refuse before stopping the old service; there is no legacy in-place fallback.

The administrator flow is:

1. Obtain the independently published SHA-256 values for the bootstrap verifier and migration package, plus the root-metadata fingerprint, from both the official website and authenticated license console. Values must agree. Save downloads as files in a root-owned 0700 directory; use the system `sha256sum -c` before executing the verifier. The old archive signature alone does not establish the new trust root. Do not pipe downloads into a shell.
2. The verified installer validates bootstrap signatures, prepares the signed platform and creates an immutable root-owned migration plan. Never generate authority or a success receipt by editing that plan yourself.
3. The migration runner verifies the platform **before stopping the old service**. It then disables old activation paths, quarantines the legacy tree, writes a verified encrypted snapshot, rotates keys and starts a clean import-only gateway. Low-privilege importers turn recognized content into bounded records; authority becomes inactive proposals.
4. Once isolation is verified, establish the fresh administrator account through the installer-provided setup flow. Old sessions, passwords, MFA material and CLI login state are not restored. Enroll MFA before reviewing high-risk proposals.
5. In **System → Migration review**, inspect full values, approve or reject proposals, retry failed applications and complete required credential rotation. Employee IDs and storage are allocated only after confirmation. Reconnect personal accounts under their new owner.
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

## Upgrade and uninstall

The separate updater cannot be installed yet (planned v1.82.0); current hosts upgrade as described in the single-binary section above. After migration and complete updater provisioning, an Admin requests a stable release in the dashboard or uses the installed CLI:

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

The launcher validates the executable, its ancestors and its own installation as root-owned and not writable by other users. It checks the dedicated account, clears its remaining processes, opens the key, then drops uid/gid, supplementary groups and all capabilities and enables `no_new_privs`. Before intake the gateway becomes non-dumpable. It verifies its systemd parent, the exact cgroup v2 entry `0::/system.slice/duduclaw-gateway.service`, executable ownership and permissions, privilege state and denied key-path access. Native installation, the launcher before opening the key, and Rust attestation all refuse non-initial user namespaces. `/run/.containerenv`, `/run/systemd/container`, or `container=` in PID 1’s environment also cause refusal. The root installer and launcher must read `/proc/1/environ`; the unprivileged gateway may skip only a permission-denied read. Detected containers retain the container rules rather than gaining native protection by setting a mode variable.

Under the default `revoke` policy, known Docker/Podman/containerd socket paths must be absent or inaccessible: both the write-access probe and a nonblocking connection must be denied. `DOCKER_HOST` must be unset. Custom slices, cgroup v1/hybrid, unexpected sudo output and uncertain probes fail closed. Startup refusal appears in the journal and/or credential-protection card; do not weaken permissions to bypass it.

The unit additionally applies:

```ini
InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes
SystemCallArchitectures=native
```

As specified by [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html), each `-` prefix ignores a missing path. These settings hide common engine endpoints, isolate hardware devices, prevent kernel/module/cgroup changes and set-ID creation, and restrict personality changes and syscall architectures. `RestrictNamespaces` is deliberately omitted: the gateway’s Codex/bwrap task sandbox needs user, mount, PID and network namespaces.

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
