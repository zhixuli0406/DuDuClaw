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

- **License**: `license.json` is bound to the machine fingerprint (macOS: hardware UUID; Linux/Windows: hostname + MAC). The
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

## macOS license identity and rebinding

New macOS fingerprints use `macos-uuid-v2:<32 hex characters>`, derived from `IOPlatformUUID` without the hostname. `duduclaw license fingerprint`, the dashboard activation card, and rebind requests use this stable value. Activation installs a signed license; it does not itself ask the control plane to issue one. Historical hostname-based UUID candidates remain accepted and are marked legacy. Real globally-administered unicast MAC candidates remain Hardware binding evidence. Locally administered and multicast MACs, absent-MAC, all-zero MAC, shared `02:00:00:00:00:00` and broadcast `FF:FF:FF:FF:FF:FF` are Weak and migration-only: matching one leaves the installation isolated at OpenSource, with an Admin warning that an approved re-bind is required. Placeholder UUIDs are also Weak. This cannot reconstruct a previous hostname: if an old binding no longer matches, contact the issuer and use `duduclaw license rebind` once the issuer supports the new format.

Older binaries cannot validate a license rebound to the new format. Upgrade every process that reads the license before migrating; arrange a compatible rebind with the issuer before downgrading.

The UUID read retries at most three times, with a five-second timeout per attempt. If it still fails, a UUID-bound license is not accepted through a MAC fallback; the CLI and dashboard refuse to provide a fingerprint for new issuance until the UUID is readable. No UUID file is cached: an owner-only file can still be rewritten by another process running as that owner and cannot prove that a UUID came from this machine. Linux and Windows retain hostname + MAC bindings: real globally-administered unicast MACs remain Hardware and continue to validate bound paid tiers without migration. An invalid installed license leaves core features available as OpenSource and shows an Admin-only dashboard warning; `duduclaw license status` and `duduclaw doctor` report the reason locally.

**Control-plane compatibility:** refresh retains the installed binding in `machine_fingerprint`. A different macOS stable UUID is sent as `new_fingerprint`; this proposal is **only effective after the account holder approves the re-bind in the console**. Rebind sends `old_fingerprint`, `new_fingerprint` and a fresh nonce. Fingerprints are public identifiers, not proof of ownership. A signed `status: "pending"` preserves the installed license and reports that console approval is needed; HTTP 202 also requires this signed body. Empty or unverified responses are refused. Linux/Windows and unbound OEM refresh bindings stay unchanged. If macOS cannot read the UUID, bound-license refresh and rebind stop instead of proposing MAC.

Before accepting an active response, Gateway and CLI verify both signatures, the exact nonce and original subscription, and a binding equal to the original or proposed binding (explicit rebind requires the proposed binding), plus expiry, candidate strength, offline grace and tier/deployment compatibility. Active, revoked and pending responses all require a v2 envelope. A verified revocation takes effect only when its `effective_from` time has arrived, persists a revocation marker, and removes the installed license. A forged or stale response cannot change the license or marker.

**Issuer response signature contract:** only numeric `response_signature_version: 2` is accepted. Missing, legacy, malformed and unknown versions are refused. The body carries `status`, `reason`, RFC3339 `effective_from`, `nonce`, `subscription_id`, `original_fingerprint`, `proposed_fingerprint`, `license_digest`, `public_key_id` and base64 Ed25519 `response_signature`. The compact UTF-8 JSON array to sign is `["duduclaw:license-response:v2",2,status,reason,effective_from,nonce,subscription,original,proposed,license_digest]`. If no proposal was sent, use the original binding as the proposed binding. For active responses, `license_digest` is lowercase SHA-256 of the compact serialized `License`, in its Rust field order with omitted optional fields, including its signature and `control_url`; the response and license must use the same trusted `public_key_id`. For pending and revoked responses, the digest is an empty string and the response's `public_key_id` selects a key in the trusted registry. Use `duduclaw_license::refresh::response_payload` as the reference serializer. The issuer must authenticate the account holder, wait for console approval, update the binding and sign both the license and response. Deploy compatible issuer signing before upgrading clients.

Gateway and CLI activation, import, refresh, rebind and revocation share a cross-process advisory lock. Network requests capture the complete license-file and revocation-marker digests while locked, then release the lock; responses reacquire it and refuse to write if either digest changed. Gateway also checks its in-memory revision. Writes use unique temporary files, file fsync, atomic rename and, on Unix, directory fsync. This protects cooperating product processes; a process with direct write access can ignore an advisory lock. Windows native persistence, live issuer deployment, console approval, email delivery and customer migration still require separate acceptance. The license artifact itself continues to exclude `control_url` from its signature, but the mandatory response signature covers that URL. Outbound requests also apply the [pinned host and DNS policy](../features/05-security-defense.md#license-binding-and-refresh); a custom issuer requires explicit local `DUDUCLAW_CONTROL_URL` configuration, even if the license carries its URL. A server ignoring `new_fingerprint` retains the old binding; weak bindings stay OpenSource until an approved migration succeeds.

## Roadmap (planned)

- A Dashboard "one-click export my data" (一鍵匯出我的資料) button that generates the tarball.
- One-click import of a managed export's tarball on startup.
- Automated round-trip consistency verification between managed and self-hosted.
