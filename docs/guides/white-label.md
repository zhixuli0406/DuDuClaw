# Distributor key console (white-label branding removed)

> **White-label branding was removed in 1.72.2.** The product name, logo,
> subtitle, accent colour, company fields, the About HTML block, signed
> branding bundles and the `branding.*` RPCs no longer exist, and
> `branding.json` / `branding.bundle.json` files are ignored. Every dashboard
> shows the DuDuClaw identity and the fixed upstream vendor credit
> (嘟嘟數位科技有限公司 / Dudu Technology Ltd.), which is compiled into the
> binary and cannot be changed through config or any RPC.
>
> A distributor build can carry one block of distributor information on the
> About page. The distributor maintains it in the license console; it is not
> a dashboard setting: see [distributor-notice.md](distributor-notice.md).

This page now describes only the vendor-side distributor key console.

## For the vendor (distributor console)

The admin-only **/manage/distributors** page books resellers and issues
machine-bound OEM license keys.

1. Configure the issuer signing key (the same 32-byte Ed25519 seed format
   produced by the license keygen tooling, base64, single line):

   ```toml
   # ~/.duduclaw/config.toml
   [distributor]
   issuer_key_path = "/path/to/license-signing-v2.key"
   ```

   Unset = issuance is refused with an explicit error; the console shows a
   setup card instead. The key content never appears in logs or responses.

2. Add a distributor, then **Issue key**: paste the machine fingerprint the
   reseller gets from `duduclaw license fingerprint`, pick a term (default
   365 days), and copy the generated blob. The reseller activates it with
   `duduclaw license activate <blob>`.

   Every issued license is self-verified against the binary's baked v2 public
   key before it is recorded — a mismatched key pair fails loudly.

   **Take the fingerprint from 1.66.1 or later.**

   - **macOS**: since 1.66.1 the fingerprint binds to `IOPlatformUUID`. macOS 26
     (Darwin 27) hides every interface's MAC from ordinary non-entitled
     processes (all of them read as `02:00:00:00:00:00`), so selecting a better
     interface cannot help and the identifier itself changed. **Every existing
     macOS install's fingerprint therefore changes.**
   - **Linux / Windows**: the fingerprint is unchanged. Selection only skips
     all-zero, placeholder, and multicast addresses; a host whose first
     interface was already usable computes exactly the 1.66.0 value.

   **Moving machines, or after an upgrade**: run
   `duduclaw license fingerprint` on the new (or upgraded) machine and have the
   key re-issued against that value, or run `duduclaw license rebind` (which
   calls the control plane's `/v1/license/rebind` to re-sign). A license issued
   against an older fingerprint **keeps working** — the gateway accepts the
   legacy binding and logs a WARN recommending a re-issue.
   `duduclaw license status` shows a `Fingerprint binding:` line reading either
   `strong` or `legacy (re-issue recommended)`. To see every fingerprint this
   machine accepts, run `duduclaw license fingerprint --all`; line 1 is always
   the strong fingerprint, so scripts can keep reading the first line.

   If `fingerprint` / `status` warn that no hardware identity is readable, the
   machine has neither a platform UUID nor a usable NIC address (a container or
   restricted environment) and the fingerprint binds to the hostname alone. It
   is still stable and fine to issue against — it just provides no
   hardware-level copy protection.

   **Moving machines, or after an OS upgrade**: run
   `duduclaw license fingerprint` on the new (or upgraded) machine and have the
   key re-issued against that value. A license issued against an older
   fingerprint **keeps working** on 1.66.1 — the gateway accepts the legacy
   binding and logs a WARN recommending a re-issue. `duduclaw license status`
   shows a `Fingerprint binding:` line reading either `strong` or
   `legacy (re-issue recommended)`. To see every fingerprint this machine
   accepts, run `duduclaw license fingerprint --all`; line 1 is always the
   strong fingerprint, so scripts can keep reading the first line.

3. **Revoke** marks the key revoked in the local ledger (`distributor.db`) and
   is written to the security audit log. Propagation to an already-activated
   instance happens through the phone-home refresh and the signed CRL described
   below — the UI states the timing honestly rather than pretending instant
   revocation.

### Keeping issued keys alive (refresh & revocation)

When an issuer key is configured, the owner gateway also serves a lightweight
control-plane for the keys it signs, so they never trip the 60-day offline
downgrade and revocations propagate. Two public endpoints self-gate on
`[distributor] issuer_key_path` (unset ⇒ `404`, so a plain gateway exposes
nothing):

| Endpoint | Purpose |
|---|---|
| `POST /v1/license/refresh` | Re-signs the caller's license with `last_phone_home = now`. **Never extends the term** — a lapsed key is renewed by re-issuing, not refreshing. Returns `revoked` for a revoked key, `403` for a fingerprint mismatch or an expired key. |
| `GET /v1/license/crl` | A signed Certificate Revocation List (Ed25519 over the same canonical payload the client verifies) listing every revoked `subscription_id`. TTL 7 days. |

The distributor's instance needs **no code change** — point it at the owner
gateway with one environment variable:

```bash
# On the reseller's DuDuClaw instance
export DUDUCLAW_CONTROL_URL=https://your-gateway.example.com
```

With that set, the reseller's gateway phones home on its per-tier schedule and
polls the CRL, so:

- **Refresh** keeps the license alive indefinitely (each successful phone-home
  re-stamps `last_phone_home`), and the owner console shows the **last refresh**
  time per key as a still-alive signal.
- **Revocation** reaches the reseller within the phone-home interval (roughly a
  week for the OEM tier) and, independently, within the CRL polling window (24h)
  — whichever lands first.

The distributor console shows an **Endpoint active** badge and this setup
snippet whenever an issuer key is configured.

> Honest condition: if a reseller does **not** set `DUDUCLAW_CONTROL_URL` (and
> no cloud control-plane is reachable), the key still downgrades to the
> open-source tier after 60 days without a successful phone-home. The refresh
> endpoint removes the downgrade only for instances that point at it.

#### Baking the endpoint into the key (`[distributor] public_url`)

To spare the reseller from setting `DUDUCLAW_CONTROL_URL` at all, declare the
owner gateway's externally-reachable URL once:

```toml
# ~/.duduclaw/config.toml (owner instance)
[distributor]
issuer_key_path = "/path/to/license-signing-v2.key"
public_url      = "https://your-gateway.example.com"
```

From then on, `distributor.issue` embeds that URL into the key as a self-carried
`control_url`. The distributor instance resolves its control-plane in this
order: `DUDUCLAW_CONTROL_URL` env → the key's `control_url` → the built-in
default. So a key issued with `public_url` set phones home and refreshes with
**zero client configuration**, and `duduclaw license refresh` works without any
environment variable. `control_url` is not part of the signed payload (editing
it needs local write access to the 0600 `license.json`, and every refresh
response is itself signature-verified — the worst case is "URL unreachable", the
same as today). A refresh preserves the original `control_url`.

## RPC surface

| Method | Access |
|---|---|
| `about.get` | any logged-in user (fixed product and vendor fields, version, tier; `distributor_notice` only from a distributor build or a signed runtime copy for a distributor-issued license) |
| `distributor.status/list/add/update/remove/issue/revoke/upgrade` | admin |

## HTTP control-plane surface (owner gateway, issuer-key-gated)

| Endpoint | Purpose |
|---|---|
| `POST /v1/license/refresh` | Re-sign a license (phone-home). |
| `GET /v1/license/crl` | Signed revocation list. |
