# Personal Edition data portability: self-hosted ↔ managed, either direction

> Applies to: DuDuClaw Personal Edition. A managed Personal Edition instance and a self-hosted
> Personal Edition are **the exact same artifact**, so data moves freely between them. There is
> no vendor lock-in.

## Why it's portable

Personal Edition (`EditionProfile::Personal`) is a **self-contained, single-owner** deployment
unit. Cloud "managed" hosting is simply "the same Personal Edition, running on our
infrastructure" — the container carries `DUDUCLAW_EDITION=personal` and nothing more. That
means all of your state lives under one directory, `~/.duduclaw/`:

| Content | Path |
|------|------|
| Agents (SOUL.md / CLAUDE.md / agent.toml / .claude) | `~/.duduclaw/agents/` |
| Memory (episodic / semantic SQLite + FTS5) | `~/.duduclaw/memory*.sqlite` |
| Configuration | `~/.duduclaw/config.toml`, `~/.duduclaw/inference.toml` |
| License | `~/.duduclaw/license.json` |
| Tasks / automation / events | `~/.duduclaw/*.jsonl`, `events.db` |
| Active outgoing-call leases (including WAL/SHM) | `~/.duduclaw/egress_leases.db`, `egress_leases.db-wal`, `egress_leases.db-shm`, `egress_leases.lock` |

## Available today: manual migration with tar

You can move a full Personal Edition state with standard tooling right now:

```bash
# 1. Pack it up from the source (a self-hosted or a managed-export directory)
tar -C "$HOME" -czf duduclaw-export.tar.gz .duduclaw

# 2. Move it to the target machine and unpack (stop the gateway first)
tar -C "$HOME" -xzf duduclaw-export.tar.gz

# 3. Start it. Personal Edition loads the existing agents and memory directly
duduclaw start
```

> Managed customers can request an export from the Dashboard and get a `~/.duduclaw/` tarball in
> the same format, ready to unpack into a self-hosted setup; the reverse works too. Because both
> sides are the same Personal Edition artifact, **no conversion step is needed**.

## Things to watch when you switch

- **License**: `license.json` is bound to the machine fingerprint (hostname + MAC). The
  Personal Edition core keeps working after a machine change (Apache 2.0); if you have a Pro
  add-on module, the licence has to be rebound to the new machine by whoever issued it.
- **Channel tokens**: channel bot tokens live inside the encrypted config and move along with
  everything else; remember to update the webhook URL when the IP or domain changes.
- **EditionProfile**: all builds derive the maximum edition from the verified licence
  (Business, OEM, Partner and Self-Host Pro allow `enterprise`). A missing or invalid licence
  means `personal`. `DUDUCLAW_EDITION` (`personal` or `enterprise`) takes precedence over an
  embedding host's `GatewayConfig.edition`, but both can only narrow that maximum; an attempted
  elevation is ignored with one warning per source per process. For example, `personal` can
  restrict a Business install, while `enterprise` cannot upgrade a Personal install.
  Debug assertions do not allow elevation; tests use signed temporary licences. The separate fixture API requires the non-default `test-edition-override` feature and debug assertions, and release builds reject that feature. There is no dashboard switch or
  config-file key: `agent.toml [edition] profile` is not read. Self-hosted enterprise operation
  requires a valid licence with that entitlement.

## Roadmap (planned)

- A Dashboard "one-click export my data" (一鍵匯出我的資料) button that generates the tarball.
- One-click import of a managed export's tarball on startup.
- Automated round-trip consistency verification between managed and self-hosted.
