# Distributor name and logo

In v1.78.0, a distributor can set a product name and logo in the license
console. Only a gateway whose effective license tier is `Oem` uses them, and
only after verifying the signed document for its subscription. Without a
valid document it shows the original DuDuClaw brand. A failed fetch or invalid
document never prevents startup. The name and logo are the entire scope:
subtitle, accent colour, company fields, custom About HTML and `branding.*`
RPCs remain removed. Old `branding.json` and `branding.bundle.json` are ignored.
Customers have no setter in config, HTTP or RPC.

The distributor edits the name and logos alongside its notice in the license
console, previews them and publishes. A signed runtime copy reaches the gateway
through the same feed cadence and pinned Ed25519 keys as the distributor notice.
This public repo provides the reader; console editing is maintained separately.

## Signed brand contract

- Schema: `duduclaw.distributor_brand.v1`; signing uses the same canonical JSON
  and Ed25519 procedure as the notice, with a separate document and vectors.
- Endpoint: `POST /v1/license/distributor-brand`. Request fields are
  `subscription_id`, `machine_fingerprint` and a fresh `nonce`, exactly as for
  the notice. The response echoes `nonce` and returns `distributor_brand`
  (`null` when absent, otherwise the signed document). The signature binds
  `subscription_id`, `version`, `nonce`, `issued_at`, `product_name`,
  `logo_light` and optional `logo_dark`, with the notice's signing metadata.
  Canonical field order is `kind`, `subscription_id`, `distributor_id`, `version`, `product_name`, `logo_light`, `logo_dark`, `issued_at`, `nonce`, `public_key_id`.
  `signature` is excluded from signing; absent `logo_dark` is serialized as
  `null`. A `null` response document clears the brand; names and light logos
  cannot be cleared individually.
- A subscription mismatch, wrong nonce, unknown key, invalid signature or
  version rollback is refused. The cache is `<home>/distributor_brand.json`;
  cached content is verified again before use. No brand is compiled in.
- Names contain 1–40 characters, no controls, newlines, URL or `<` / `>`.
  Logos must be base64 PNG, JPEG or WebP data URIs whose magic bytes match the
  declared MIME type; each decoded logo is at most 128 KiB. SVG and remote
  images are refused. One invalid field rejects the entire document.
- `about.get` adds `brand` only when effective. Public read-only `GET /api/brand`
  exposes only `product_name`, `logo_light` and `logo_dark`, without license or
  customer information. The response is a top-level object, or `null` when
  ineffective, which means the original identity.

The gateway keeps the highest accepted version per subscription/distributor in
`<home>/distributor_brand_versions.json` (0600 on Unix, atomic replacement).
It checks this file before exposing a cached brand at boot; an older cache is
ignored until a valid fetch replaces it. Clearing the brand or replacing the
license does not delete this watermark. Required name/light-logo fields remain
strings: an absent brand must be a `null` document, not nullable fields.

`about.get` includes `brand_revision`; `{ "brand_revision_only": true }` returns
only that small field. `/api/brand` exposes the same revision in
`X-Brand-Revision`, with an ETag, `If-None-Match`/304 support and a 60-second
public cache. Requests, including conditional requests, are limited to 30 per
IP per minute (429 with `Retry-After: 60`). The dashboard checks revisions on
its existing session refresh, reconnect and tab focus; desktop checks every
30 minutes. A changed revision causes the brand to reload, including a return
to DuDuClaw when the response is `null`.

## Where the brand appears

The dashboard uses the name and logo, including product mascot positions,
login and welcome screens, page title, favicon and LINE QR posters. Channel
footers, OTP and OAuth pages use the escaped name. Translation loading changes
product copy in English, Traditional Chinese and Japanese; legal notices and
privacy links keep their original text. CLI and logs keep their existing name.

When branded, About shows the distributor logo and name first, the distributor
notice next, then a fixed **Software development** block with vendor, website,
version and license type. **嘟嘟數位科技有限公司 / Dudu Technology Ltd.** and the
vendor website are compile-time constants and cannot be changed or hidden by
the distributor. The separate sanitized [distributor notice](distributor-notice.md)
does not override that block.

Desktop window titles, tray tooltips and update dialogs use the runtime name.
Installer names, the macOS Dock name and application icons remain compile-time
assets; changing those requires separate packaging and is outside v1.78.0.
Logos preserve any aspect ratio and fit inside their display box.

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
| `about.get` | any logged-in user (fixed vendor fields, version, tier; `brand` only for verified Oem branding; `distributor_notice` only from a distributor build or a signed runtime copy for a distributor-issued license) |
| `distributor.status/list/add/update/remove/issue/revoke/upgrade` | admin |

## HTTP control-plane surface (owner gateway, issuer-key-gated)

| Endpoint | Purpose |
|---|---|
| `POST /v1/license/refresh` | Re-sign a license (phone-home). |
| `GET /v1/license/crl` | Signed revocation list. |
