# duduclaw-maint-tools

The two programs a support technician meets when they sign in to a DuDuClaw OS device over SSH during a maintenance window: a fixed menu, and the root helper that hands that menu a filtered copy of the gateway and local-model logs and performs the menu's two restarts.

They belong to the maintenance SSH account `duduclaw-maint` (uid/gid 4800). The account, its dropbear configuration and the helper's socket units ship in the DuDuClaw-OS repo; this crate only builds the binaries. Until an OS release installs the account, nothing here is reachable on a device.

## What the technician can do

`duduclaw-maint-shell` is both the account's login shell and dropbear's forced command. With no command it shows a numbered menu (zh-TW / English); with a command (`ssh duduclaw-maint@duduclaw.local logs gateway`) the command must be one of these, word for word:

For a shell request, Dropbear's global forced command sets `SSH_ORIGINAL_COMMAND` to an empty string. The entry point treats that and an absent variable as interactive mode. Every nonempty value, including whitespace-only or non-UTF-8 input, still goes through the command allowlist.

| Command | Runs |
|---|---|
| `status` | `systemctl --failed`, then `systemctl status -n 0` for the gateway, sysd, local model, kiosk, firewall and dropbear socket (each only if installed) |
| `logs gateway` / `logs gateway follow` | gateway journal through `duduclaw-maint-helper` |
| `logs model` / `logs model follow` | local-model journal through `duduclaw-maint-helper` |
| `resources` | `uptime`, `free -m`, `df -h / /data` |
| `network` | `ip -brief address`, `ip route`, `resolvectl status` (if installed) |
| `restart model` | asks the helper for `restart-model` (`duduclaw-llama-server.service`) |
| `restart gateway confirm` | asks the helper for `restart-gateway` (`duduclaw-gateway.service`); warns first that this ends the maintenance window and the connection, and the interactive menu asks for `yes` |

Anything else (`/bin/sh`, `sh -c id`, `scp`, the sftp server path, `status; id`, extra arguments) prints the list above and exits with code 2. There is no reboot, no general shell, and no way to pass an argument to a child: client input only selects one of nine actions. Every `systemctl` call carries `--no-pager --no-ask-password`, children run with a cleared environment (`PATH`, `LANG=C.UTF-8`, `SYSTEMD_PAGER=cat`, `SYSTEMD_PAGERSECURE=1`, `SYSTEMD_COLORS=0`), programs are run by absolute path from `/usr/bin`, `/bin`, `/usr/sbin`, `/sbin`, and `TERM` is never read. Child output (stdout and stderr through one pipe, at most 4 MiB) and helper output go through a UTF-8-aware filter before reaching the terminal: control characters, the UTF-8 encoded C1 range U+0080–U+009F and bidi formatting characters become spaces, because part of that text comes from uid 1000 (a unit's `sd_notify` `Status:` line, for example). Ctrl-C stops the current command or log follow and returns to the menu. Each action is written to the journal as `SYSLOG_IDENTIFIER=duduclaw-maint` (a refused command is recorded by its length only). The menu itself never restarts anything: the two restarts are requests to the helper, and the menu prints the helper's one-line answer (`ok: …` succeeds, anything else exits 1). There is no polkit anywhere (owner decision 2026-10-16: adding polkit to the base edition can renumber existing system accounts).

`duduclaw-maint-helper` runs as root, one instance per connection on `/run/duduclaw-maint-helper.sock`:

1. The peer's uid (from `SO_PEERCRED`, not from anything the client sends) must be 4800. Anything else, including an unreadable credential, gets `error: not permitted`.
2. One request line, exactly one of six tokens: `gateway`, `gateway-follow`, `model`, `model-follow`, `restart-gateway`, `restart-model`. Anything else gets `error: unknown request`. The token must arrive within 10 seconds in total.
3. Log tokens run `journalctl --no-pager -o json --output-fields=MESSAGE -n 1000 [-f] _SYSTEMD_UNIT=<unit> _UID=1000` (unit `duduclaw-gateway.service` or `duduclaw-llama-server.service`). Per record: a `MESSAGE` with any line starting with `password:` after leading whitespace or invisible characters (ASCII case-insensitive) is dropped entirely, which removes the gateway's first-run bootstrap password line; otherwise control and bidi characters become spaces, credentials are masked with `duduclaw_core::secret_redact::redact_secrets`, the text is cut to 4 KiB, and the helper prints `<local time with offset> <message>`. Records over 64 KiB are skipped unread, at most 1000 lines without `follow`, a follow stops as soon as the client disconnects, and a client that stops reading for 30 seconds is dropped.
4. Restart tokens run `systemctl --no-pager --no-ask-password restart <unit>` with a constant argv (`duduclaw-gateway.service` or `duduclaw-llama-server.service`, nothing else), all output discarded, waiting up to 120 seconds, and answer with one line: `ok: restarted <unit>`, `error: too soon, try again in N s`, `error: restart failed (exit N)`, `error: restart still running after 120 s` or `error: restart unavailable`. Each result is written to the journal as `SYSLOG_IDENTIFIER=duduclaw-maint-helper`.
5. Rate limit: one restart per unit per 60 seconds (`RESTART_MIN_INTERVAL_SECS`). Because every connection is its own process, the last restart time is kept per unit in `/run/duduclaw-maint-helper/restart-<gateway|model>.stamp` (root, 0600, opened with `O_NOFOLLOW`, read and rewritten under an exclusive `flock`), measured on `CLOCK_BOOTTIME` so changing the wall clock does not reset it. An attempt counts even if systemctl then fails. The directory must be a real root-owned 0700 directory and each stamp a root-owned 0600 regular file with one link; otherwise, or if the stamp cannot be read or written, the restart is refused with `restart unavailable` (fail closed) and the helper instance exits non-zero, so the fault shows in `systemctl --failed`. A stamp from the future is clamped once: that request is refused as too soon, the stamp is rewritten to now and a warning goes to the journal, so a bogus value cannot block restarts until reboot.

Known limits, by design: `redact_secrets` masks credential *shapes* (vendor-prefixed keys, bot tokens, JWTs, `Bearer <token>`, secret URL parameters). It does not mask free-form `key=value` text such as `password=hunter2`; the gateway is expected not to log those, and anything in these two journals is already readable by uid 1000, which the maintenance account must not exceed. `systemctl status` shows the units' process command lines; the design's `ProtectProc=invisible` on the dropbear session hides processes of other users from it.

## Building

These binaries are built by the DuDuClaw OS image, not by the platform release (`scripts/release.sh` and `release.yml` do not package them).

```bash
cargo build --release -p duduclaw-maint-tools
# target/release/duduclaw-maint-shell  → install as /usr/libexec/duduclaw/maint-shell
# target/release/duduclaw-maint-helper → install as /usr/libexec/duduclaw/maint-helper
cargo test -p duduclaw-maint-tools
```

The only DuDuClaw dependency is `duduclaw-core` (for `secret_redact` and `truncate_bytes`), so the OS recipe needs a snapshot with both crates. The `duduclaw-sysd` recipe's flattened single-crate snapshot does not fit; the `duduclaw-cli` recipe's mini-workspace snapshot does (`refresh-src.sh` copies the root `Cargo.toml` with `members` narrowed to what the target reaches, here `duduclaw-core` and `duduclaw-maint-tools`). Its `*-crates.inc` must then be regenerated for this crate's dependency closure. Which of the two layouts the OS recipe uses is the OS PR's decision.

The tests run on macOS and Linux; the Linux-only parts (`SO_PEERCRED`, the journald socket) sit behind `cfg` and traits, and on other targets the binaries only print that they need DuDuClaw OS.

## What the OS side has to provide

Copied from the design so the OS PRs have one checklist:

- `duduclaw-maint` account: uid/gid 4800, no supplementary groups, password field `*`, shell `/usr/libexec/duduclaw/maint-shell` listed in `/etc/shells`.
- dropbear: `-c /usr/libexec/duduclaw/maint-shell` (plus the rest of the design's `ExecStart`).
- `duduclaw-maint-helper.socket`: `ListenStream=/run/duduclaw-maint-helper.sock`, `SocketUser=root`, `SocketGroup=duduclaw-maint`, `SocketMode=0660`, `Accept=yes`, `MaxConnections=4`.
- `duduclaw-maint-helper@.service`: `ExecStart=/usr/libexec/duduclaw/maint-helper`, root, `StandardInput=socket`, `NoNewPrivileges=yes`, `ProtectSystem=strict`, `PrivateNetwork=yes`, `IPAddressDeny=any`, `RuntimeMaxSec=3600`, and, required for the rate limit, `RuntimeDirectory=duduclaw-maint-helper`, `RuntimeDirectoryMode=0700`, `RuntimeDirectoryPreserve=yes` (without the directory every restart is refused as `restart unavailable`; without Preserve it would be removed between connections and the limit would reset). Recommended in addition: `StandardError=journal`, so a panic message never reaches the client, and `CollectMode=inactive-or-failed`, so failed instances do not pile up. The service must not run in `system-dropbear.slice`, so the window closing after a gateway restart does not kill the restart in progress.
- No polkit and no polkit rule on either edition.

## License

Apache-2.0, like the rest of the workspace.
