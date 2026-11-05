# Personal connections on a native Linux service

> The installation and user-data migration sections below describe the existing v1.79.3 service. The development update path is separate and incomplete; see [Upgrade and uninstall](#upgrade-and-uninstall) before changing an existing installation.

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

**Development branch: protected updates are not ready for deployment.** The dashboard and CLI now send requests to the separate root update service. Provisioning through the isolation migration package is required; replacing only a v1.79.3 binary or timer does not install this service or its trust root.

An Admin can request a newer stable version from the dashboard and follow its durable request id. CLI requests require an explicit version:

```sh
sudo duduclaw service upgrade --protected-linux --version X.Y.Z
```

Only the version and request id cross the updater socket. The caller cannot select a path, URL, command or trust key. The service authenticates the peer and verifies the signed manifest, artifact hashes, architecture, expiry, version floor and serial. Acceptance, downloading and staging do not mean installation succeeded. An interrupted reply leaves the dashboard tracking the request; only `committed` is displayed as success. An unavailable service does not fall back to the old installer.

The Linux activator now switches a complete generation, restarts fixed services and compares live version/protocol/executable readbacks before advancing the floor. It resumes interrupted transactions and keeps work admission closed when readiness is uncertain. The named cross-package drain, migration/runtime and readiness endpoints still need integration; absent endpoints refuse the update. The migration bootstrap's dual-signature flow, real FIDO2 signatures and native Linux update acceptance are not complete. Do not enable unattended updates from this source yet.

For an existing **v1.79.3 installation**, retain the verified, pinned v1.79.3 executable and its matching operational instructions. That historical executable downloads and verifies the minisign-signed archive, atomically replaces its binary and restarts an active service; its dashboard only supports manual updates. Its root-owned `version-floor` and `version-pending` prevent ordinary downgrades and reserve an interrupted target. Those historical files are not the new updater's state or trust store.

The existing v1.79.3 uninstall command remains:

```sh
sudo /opt/duduclaw/bin/duduclaw service uninstall --protected-linux
```

It stops/disables the old system unit and removes its unit, executable and launcher. It **does not delete** `/var/lib/duduclaw` or `/var/lib/duduclaw-protected`, including the root key; it removes the old version floor and pending intent. This command is not a cleanup or rollback procedure for the new isolation installation. New-isolation migration and recovery instructions must be supplied by the migration package before deployment.

## Automatic upgrades (development only)

The current `scripts/linux-protected/auto-upgrade/` keeps the existing timer schedule: 03:30 local time, up to 20 minutes of random delay, and `Persistent=false` so a missed run is not replayed after a morning boot. Its helper now invokes the fixed root-owned `/usr/libexec/duduclaw-updated-launch --check-latest`; `--dry-run` adds candidate discovery without submitting Apply.

Candidate discovery runs curl as the unprivileged `duduclaw-dl` account, clears inherited configuration, caps the API response at 1 MiB and parses complete JSON. The latest-release record must have `draft=false`, `prerelease=false` and a stable version. The unsigned index chooses only a candidate: installation still requires an officially signed manifest accepted by the root service. Equal or older candidates do not submit an update. Network, account, state or service failures stop the request; there is no legacy fallback. The requester unit has a six-minute timeout and does not write release files.

Do not copy this development helper onto a v1.79.3 installation or enable its timer yet. An existing v1.79.3 nightly deployment must retain its previously reviewed pinned helper until the complete isolation migration is available. Never fetch an operational helper from moving `main`.

The source installer still checks root:root ownership, ordinary file types and non-writable ancestors, rejects symlinks and sticky `/tmp`, and installs from a private root snapshot. These checks do not authenticate source provenance. Review the exact commit and root-controlled copy before any eventual deployment; do not execute a root installer from a user-writable checkout. See [the helper README](../../scripts/linux-protected/auto-upgrade/README.md).

To remove an already installed schedule, keeping the gateway and its data, use its reviewed installer copy:

```sh
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

If an upgrade is running, uninstall disables the timer but refuses removal; wait for completion and retry.

## Protection and limits

**Feature limits:** Protected native mode blocks container engines with `InaccessiblePaths` and isolates devices with `PrivateDevices`. Container-backed task sandboxes, computer use and hardware-dependent features are unavailable. This is intentional: AI employees must not reach a container engine.

The launcher validates the executable, its ancestors and its own installation as root-owned and not writable by other users. It checks the dedicated account, clears its remaining processes, opens the key, then drops uid/gid, supplementary groups and all capabilities and enables `no_new_privs`. Before intake the gateway becomes non-dumpable. It verifies its systemd parent, the exact cgroup v2 entry `0::/system.slice/duduclaw-gateway.service`, executable ownership and permissions, privilege state and denied key-path access. Native installation, the launcher before opening the key, and Rust attestation all refuse non-initial user namespaces. `/run/.containerenv`, `/run/systemd/container`, or `container=` in PID 1’s environment also cause refusal. The root installer and launcher must read `/proc/1/environ`; the unprivileged gateway may skip only a permission-denied read. Detected containers retain the container rules rather than gaining native protection by setting a mode variable.

Known Docker/Podman/containerd socket paths must be absent or inaccessible: both the write-access probe and a nonblocking connection must be denied. `DOCKER_HOST` must be unset. Custom slices, cgroup v1/hybrid, unexpected sudo output and uncertain probes fail closed. Startup refusal appears in the journal and/or credential-protection card; do not weaken permissions to bypass it.

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

At startup and every 60 seconds, the gateway checks common engine socket reachability and supplementary groups. A failed periodic check zeroizes the held key in place, changes protection to Refused and logs a warning; personal connections stay unavailable until restart. `/proc/net/unix` is also inspected for listening abstract sockets. A slash-delimited name component must equal `docker`, `containerd`, `podman`, `buildkit` or `lxd`, or start with one of those names followed by `.`, `-` or `_`, to become a candidate; arbitrary substrings do not match. Abstract name matches alone do not cause refusal: the gateway makes a nonblocking connection with a short timeout and checks `SO_PEERCRED`; only a reachable peer with uid 0 is considered a risk. Unreachable candidates and malformed unrelated rows are ignored. Failure to read `/proc/net/unix` refuses key intake; during periodic checks it logs a warning and retains only that probe's previous verdict, while path and group checks still run.

AI employees' Bash/tool processes still run as the gateway's uid. Protection relies on non-dumpable memory, the root-owned key and binary, no sudo/linger, and denied engine sockets. Employees can still modify their writable data, stop the gateway, request authorized operations and read access tokens issued for their own turn. The host must not grant the `duduclaw` account access to container engines or other privileged brokers. These checks cover common endpoints at startup and once per minute; they are not an unconditional isolation guarantee. Unrecognized abstract socket names, engine sockets at other paths, privileged brokers, other privilege paths, host root compromise and malicious code already running inside the gateway are not covered. A new exposure can exist between checks. The root-owned executable check prevents modification by the service uid; it is not signature verification against a hostile root administrator.

The key is not a general sandbox. While it is held, the existing in-process ONNX restriction also applies: NER redaction, local embeddings and Kokoro are unavailable; use regex redaction where required. Backups containing both the data and root key can decrypt credentials. Replacing only the dashboard's protection key does not prevent replay of old data; after a suspected root-key leak, stop the service, securely rotate the root key as host administrator, reset protection and reconnect accounts.

Native systemd installation, sudo variants and socket probes require acceptance testing on the target distribution. Do not treat unit tests or macOS compilation as native Linux acceptance.

## Planned v1.80 migration boundary

The legacy protected installer refuses a direct binary upgrade from a release before v1.80 to v1.80 or later, including prereleases. Missing version-floor metadata also refuses that transition; an upgrade checks the running version as well as the stored floor. A fresh v1.80 installation must use the new installer rather than this legacy path. The separately verified migration/bootstrap workflow is not connected in this implementation yet. Do not copy the old databases into a new protected installation to bypass this refusal.

Administrators can open **System → Migration review**, enroll MFA under Account security, then enter their password and a fresh authenticator code. Review full values before approving. Only low-risk proposals can be bulk-approved; pending proposals can be bulk-rejected. Refresh shows actual application readback. Failed or interrupted applications can be retried with the same proposal binding. Supported configuration credentials can be replaced after application; retained credentials keep migration incomplete. Unsupported categories cannot be approved yet. The root migration command and gateway startup integration are not a released upgrade path; do not run them against a live installation until the signed installer and Linux acceptance are complete. See [implementation notes](../architecture/native-linux-isolation.md).

The unreleased importer now has a DynamicUser launcher and an exclusive import-only gateway startup. Old accounts and MCP configuration enter the review list without activation. Confirmed stdio MCP changes and employee instruction files use product validation, publication and readback; credential-bearing or unsupported proposals require re-entry or remain blocked. Content for unconfirmed employees remains archived. These components still need the signed platform adapter and Linux end-to-end acceptance before a full upgrade is available.

Skill, hook and selected policy changes also use product validation and operation-bound readback. Compensation refuses files or departments replaced by ordinary edits. Old approvals are expired. Telegram bindings, time schedules and database read-only grants additionally require verified Shared employee allocation; those integrations are still blocked without the isolation platform.
