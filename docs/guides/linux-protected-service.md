# Personal connections on a native Linux service

Official Linux CLI binaries require **glibc ≥ 2.31**: Ubuntu 20.04+, Debian 11+, or RHEL/Rocky Linux 9+. Use Docker on older systems. This baseline applies to the CLI/gateway, including the desktop CLI sidecar; the desktop GUI also requires its own WebKitGTK/system libraries.

Use this mode to enable **My connections** (personal Google and remote MCP OAuth) on a native Linux host. It runs the gateway as a dedicated `duduclaw` account and hands it a root-provisioned key at startup. Ordinary user services do not receive that key. See also [personal Google connections](google-personal-connections.md).

## Prepare the host

Use Linux with systemd as PID 1, a unified cgroup v2 hierarchy, GNU coreutils, GNU tar, curl, minisign, `getent`, `id`, `useradd`, and util-linux `setpriv`. Run installation from a human operator's terminal with sudo, outside an AI session and outside a container. Use the initial user namespace: both `/proc/self/uid_map` and `/proc/self/gid_map` must contain exactly one mapping `0 0 4294967295` (spacing may vary). Rootless containers and remapped user namespaces are refused.

The dedicated account must have a nonzero uid/gid and a `nologin` or `false` shell, no sudo commands, no linger, and no membership in `sudo`, `wheel`, `admin`, `adm`, `docker`, `lxd`, `incus` or `libvirt`. `systemd-journal` is allowed. Do not use this account for other services: each protected start terminates every non-zombie process of that uid before handing over the key.

## Install and start

Replace `X.Y.Z` with a published release version. Confirm `/opt` and its ancestors are root-owned and not writable by other accounts. The following sudo command downloads the official archive and its `.minisig`, verifies the exact archive bytes with the published key, and extracts the binary inside a root-owned mode-0700 directory under `/opt`. Only the verified binary is executed, inside that root shell. The published assets are `duduclaw-linux-x64.tar.gz` and `duduclaw-linux-arm64.tar.gz`; each has a matching `.minisig`.

```sh
sudo /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin /bin/sh -eu <<'ROOT'
version=X.Y.Z
case "$(uname -m)" in
  x86_64) asset=duduclaw-linux-x64.tar.gz ;;
  aarch64|arm64) asset=duduclaw-linux-arm64.tar.gz ;;
  *) echo 'Unsupported architecture' >&2; exit 1 ;;
esac
umask 077
stage=$(mktemp -d /opt/duduclaw-install.XXXXXXXX)
trap 'rm -f -- "$stage/$asset" "$stage/$asset.minisig" "$stage/duduclaw"; rmdir -- "$stage"' EXIT
cd "$stage"
base="https://github.com/zhixuli0406/DuDuClaw/releases/download/v$version"
curl -q --proto '=https' --tlsv1.2 -fL "$base/$asset" -o "$asset"
curl -q --proto '=https' --tlsv1.2 -fL "$base/$asset.minisig" -o "$asset.minisig"
minisign -Vm "$asset" -x "$asset.minisig" \
  -P 'RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh'
tar -xzf "$asset" --no-same-owner --no-same-permissions duduclaw
chown root:root duduclaw
chmod 0755 duduclaw
"$stage/duduclaw" service install --protected-linux --enable-now
ROOT
sudo systemctl status duduclaw-gateway.service
sudo journalctl -u duduclaw-gateway.service -n 100 --no-pager
```

Never run the installer with sudo from a user-writable npm installation or home directory: an AI employee could replace the binary before the kernel starts it as root. If the resolved running executable or an ancestor is not root-owned or permits non-root writes, installation warns and requires `--i-verified-this-binary`. That acknowledgement cannot make an already executed untrusted binary safe; use the verified root-owned staging procedure above.

Without `--enable-now`, installation only installs the service; start it with `sudo systemctl enable --now duduclaw-gateway.service`. Open the dashboard and complete setup, then enable **Security → Credential protection**. The dashboard should report **Separate key (AI employees cannot read it)** before anyone connects a personal account. A CLI process does not inherit the gateway's key, so CLI credential status is not the gateway's status.

The root-owned executable is `/opt/duduclaw/bin/duduclaw`; the embedded launcher is installed at `/usr/local/libexec/duduclaw/protected-launcher`. The unit is `/etc/systemd/system/duduclaw-gateway.service`. Data lives in `/var/lib/duduclaw/.duduclaw`; the root key is `/var/lib/duduclaw-protected/root.key` (root:root, mode 0400, one link, 32 bytes). The unit uses `KillMode=control-group` and a read-only system filesystem with only the data and key directories writable.

## Install AI CLIs for the service account

The service runs as `duduclaw`, with `HOME=/var/lib/duduclaw` and `PATH=/usr/sbin:/usr/bin:/sbin:/bin`. AI CLIs must be installed in one of those system PATH directories and be readable and executable by that account. `ProtectHome=yes` hides `/home`, `/root` and `/run/user`; a CLI such as `/home/operator/.local/bin/claude` is unavailable even if root can find it. A symlink in a system directory that points into a hidden home does not solve this.

For Claude Code, replace `<ver>` with the version you intend to install before running these commands:

```sh
cd / && sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>
sudo -u duduclaw -H claude auth login
```

Keep both `-H` and `--prefix /usr`: an operator's `~/.npmrc` prefix can otherwise install the CLI back into the operator's home. Install any configured Codex, Grok, AGY or Gemini CLI in the same service-readable system PATH and authenticate it as the service account using its own login procedure.

At the end of `service install --protected-linux` and `service upgrade --protected-linux`, the command checks the configured primary inference runtime and runtimes selected in agents' `agent.toml` files, reading the destination configuration after any migration. It checks executable access as the service account with the service PATH and excludes hidden paths. A CLI found only in the operator's home is reported as unavailable to the service. If the primary runtime is missing, the command prints a warning and exits nonzero. Use `--allow-missing-runtime` on either command only when deliberately completing runtime setup later; this flag does not install or authenticate a CLI. A nonzero runtime check does not undo installation or upgrade work already performed.

Migration copies only `.duduclaw` data, **not AI CLI installations or their login credentials**. When Claude is installed but the service account has no login, installation prints the service-account login command above. Authenticate that account separately; do not copy credentials from the operator's home. On gateway startup, a missing configured inference binary produces an ERROR with repair guidance; runtime status and doctor also show the protected-service instructions.

## Migrate a user installation

Back up the old data and stop its gateway before copying. As the owner of the old user service, run:

```sh
systemctl --user stop duduclaw.service
systemctl --user disable duduclaw.service
sudo /opt/duduclaw/bin/duduclaw service install --protected-linux \
  --migrate-from "$HOME/.duduclaw" --enable-now
```

Complete the verified installation above first, omitting `--enable-now` if you intend to migrate before the first gateway start. If the new gateway has already created data, preserve it and explicitly use `--force-migrate` when replacing it. Run migration from the original user’s own terminal, not a root login shell. Both `SUDO_UID` and `SUDO_GID` must be present, valid and nonzero. The installer resolves that UID’s home through `getpwuid`; the only accepted source is its `.duduclaw` directory. The supplied path must canonically equal that directory, and every source directory and file must be owned by `SUDO_UID`. It refuses sources under `/var/lib/duduclaw-protected`, `/var/lib/duduclaw`, `/etc`, `/root`, `/proc` and `/sys`. Pinned `openat` traversal refuses symlinks, including ancestor symlinks.

The installer does not run those user commands as another person. It copies that directory into `/var/lib/duduclaw/.duduclaw` and assigns it to the dedicated account. An existing destination is refused unless `--force-migrate` is given. With `--force-migrate`, the old destination is retained as `/var/lib/duduclaw/.duduclaw.previous-<UUID>`. Symlinks, hard links and special files are refused. Stop the system service before replacing its data and preserve a backup first. Review absolute paths in migrated configuration: paths under the old user's home are unavailable with `ProtectHome=yes`.

Re-enable credential protection and ask everyone to reconnect personal Google and remote MCP accounts. Old personal ciphertext belongs to the previous key and cannot be unlocked by this installation. Configure AI runtime binaries in root-owned system locations; the old user's CLI installations, login files and shell startup files are not automatically available to the new account.

## Upgrade and uninstall

Run the upgrade through the already-installed root-owned executable:

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux
# Select a published version instead of latest:
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux --version X.Y.Z
```

The command refuses unless `/proc/self/exe` is exactly `/opt/duduclaw/bin/duduclaw` and the executable and its ancestor chain are root-owned and protected against non-root writes. It downloads the official GitHub Linux release archive (latest by default), verifies its exact bytes with the updater’s embedded minisign public key, and extracts into a root-owned mode-0700 temporary directory. It atomically replaces the installed binary using a temporary file in the same directory, fsync and rename; the new binary is never executed as root during upgrade. Verification failure leaves the installed binary unchanged. It restarts the system unit only if it was active. The existing root key and data remain. The dashboard reports manual updates and refuses binary replacement in this mode.

```sh
sudo /opt/duduclaw/bin/duduclaw service uninstall --protected-linux
```

Uninstall stops/disables the system unit and removes the unit, executable and launcher. It **does not delete** `/var/lib/duduclaw` or `/var/lib/duduclaw-protected`, including the root key. It removes the version floor and pending version intent from that directory. Reinstallation can reuse the retained data and key.

Upgrade accepts only a stable semver `X.Y.Z` strictly newer than the running version. Installation and upgrade also refuse versions below `/var/lib/duduclaw-protected/version-floor` (root:root, 0600), atomically recording the highest installed version after replacing the binary. A missing floor defaults to the running version; a malformed floor is refused. There is no rollback option. Emergency rollback requires root to deliberately uninstall (which removes the floor), then perform the verified first-install procedure above with the older archive. Data and the root key remain. An interrupted replacement also reserves its verified target in a root-only write intent (`version-pending`); repeat the verified installation procedure with that version or a newer one, or deliberately uninstall to clear it.

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
