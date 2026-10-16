# duduclaw-maint-tools

The two programs a support technician meets when they sign in to a DuDuClaw OS device over SSH during a maintenance window: a fixed menu, and the root service that hands that menu a filtered copy of the gateway and local-model logs.

They belong to the maintenance SSH account `duduclaw-maint` (uid/gid 4800). The account, its dropbear configuration, the socket units and the polkit rule ship in the DuDuClaw-OS repo; this crate only builds the binaries. Until an OS release installs the account, nothing here is reachable on a device.

## What the technician can do

`duduclaw-maint-shell` is both the account's login shell and dropbear's forced command. With no command it shows a numbered menu (zh-TW / English); with a command (`ssh duduclaw-maint@duduclaw.local logs gateway`) the command must be one of these, word for word:

| Command | Runs |
|---|---|
| `status` | `systemctl --failed`, then `systemctl status -n 0` for the gateway, sysd, local model, kiosk, firewall and dropbear socket (each only if installed) |
| `logs gateway` / `logs gateway follow` | gateway journal through `duduclaw-maint-logs` |
| `logs model` / `logs model follow` | local-model journal through `duduclaw-maint-logs` |
| `resources` | `uptime`, `free -m`, `df -h / /data` |
| `network` | `ip -brief address`, `ip route`, `resolvectl status` (if installed) |
| `restart model` | `systemctl restart duduclaw-llama-server.service` |
| `restart gateway confirm` | `systemctl restart duduclaw-gateway.service` (ends the maintenance window; the interactive menu asks for `yes` first) |

Anything else (`/bin/sh`, `sh -c id`, `scp`, the sftp server path, `status; id`, extra arguments) prints the list above and exits with code 2. There is no reboot, no general shell, and no way to pass an argument to a child: client input only selects one of nine actions. Every `systemctl` call carries `--no-pager --no-ask-password`, children run with a cleared environment (`PATH`, `LANG=C.UTF-8`, `SYSTEMD_PAGER=cat`, `SYSTEMD_PAGERSECURE=1`, `SYSTEMD_COLORS=0`), programs are run by absolute path from `/usr/bin`, `/bin`, `/usr/sbin`, `/sbin`, and `TERM` is never read. Ctrl-C stops the current command or log follow and returns to the menu. Each action is written to the journal as `SYSLOG_IDENTIFIER=duduclaw-maint` (a refused command is recorded by its length only). The two restarts need the OS's polkit rule; without it they fail immediately instead of prompting.

`duduclaw-maint-logs` runs as root, one instance per connection on `/run/duduclaw-maint-logs.sock`:

1. The peer's uid (from `SO_PEERCRED`, not from anything the client sends) must be 4800. Anything else, including an unreadable credential, gets `error: not permitted`.
2. One request line: `gateway`, `gateway-follow`, `model` or `model-follow`. Anything else gets `error: unknown request`.
3. It runs `journalctl --no-pager -o json --output-fields=MESSAGE -n 1000 [-f] _SYSTEMD_UNIT=<unit> _UID=1000` (unit `duduclaw-gateway.service` or `duduclaw-llama-server.service`).
4. Per record: a `MESSAGE` that starts with `password:` after leading whitespace (ASCII case-insensitive) is dropped entirely, which removes the gateway's first-run bootstrap password line; otherwise control and bidi characters become spaces, credentials are masked with `duduclaw_core::secret_redact::redact_secrets`, the text is cut to 4 KiB, and the service prints `<local time with offset> <message>`.
5. Bounds: records over 64 KiB are skipped unread, at most 1000 lines without `follow`, a follow stops as soon as the client disconnects, the token must arrive within 10 seconds in total, and a client that stops reading for 30 seconds is dropped.

Known limits, by design: `redact_secrets` masks credential *shapes* (vendor-prefixed keys, bot tokens, JWTs, `Bearer <token>`, secret URL parameters). It does not mask free-form `key=value` text such as `password=hunter2`; the gateway is expected not to log those, and anything in these two journals is already readable by uid 1000, which the maintenance account must not exceed. `systemctl status` shows the units' process command lines; the design's `ProtectProc=invisible` on the dropbear session hides processes of other users from it.

## Building

These binaries are built by the DuDuClaw OS image, not by the platform release (`scripts/release.sh` and `release.yml` do not package them).

```bash
cargo build --release -p duduclaw-maint-tools
# target/release/duduclaw-maint-shell  → install as /usr/libexec/duduclaw/maint-shell
# target/release/duduclaw-maint-logs   → install as /usr/libexec/duduclaw/maint-logs
cargo test -p duduclaw-maint-tools
```

The only DuDuClaw dependency is `duduclaw-core` (for `secret_redact` and `truncate_bytes`), so the OS recipe needs a snapshot with both crates. The `duduclaw-sysd` recipe's flattened single-crate snapshot does not fit; the `duduclaw-cli` recipe's mini-workspace snapshot does (`refresh-src.sh` copies the root `Cargo.toml` with `members` narrowed to what the target reaches, here `duduclaw-core` and `duduclaw-maint-tools`). Its `*-crates.inc` must then be regenerated for this crate's dependency closure. Which of the two layouts the OS recipe uses is the OS PR's decision.

The tests run on macOS and Linux; the Linux-only parts (`SO_PEERCRED`, the journald socket) sit behind `cfg` and traits, and on other targets the binaries only print that they need DuDuClaw OS.

## What the OS side has to provide

Copied from the design so the OS PRs have one checklist:

- `duduclaw-maint` account: uid/gid 4800, no supplementary groups, password field `*`, shell `/usr/libexec/duduclaw/maint-shell` listed in `/etc/shells`.
- dropbear: `-c /usr/libexec/duduclaw/maint-shell` (plus the rest of the design's `ExecStart`).
- `duduclaw-maint-logs.socket`: `ListenStream=/run/duduclaw-maint-logs.sock`, `SocketUser=root`, `SocketGroup=duduclaw-maint`, `SocketMode=0660`, `Accept=yes`, `MaxConnections=4`.
- `duduclaw-maint-logs@.service`: root, `StandardInput=socket`, `NoNewPrivileges=yes`, `ProtectSystem=strict`, `PrivateNetwork=yes`, `IPAddressDeny=any`, `RuntimeMaxSec=3600`. Recommended in addition: `StandardError=journal`, so a panic message never reaches the client, and `CollectMode=inactive-or-failed`, so failed instances do not pile up.
- polkit rule allowing `restart` of exactly `duduclaw-gateway.service` and `duduclaw-llama-server.service` for `duduclaw-maint`.

## License

Apache-2.0, like the rest of the workspace.
