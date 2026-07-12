# White-label branding & distributor console

DuDuClaw lets a licensed reseller rebrand the dashboard — product name, logo,
subtitle, and company info — while the upstream vendor credit
(嘟嘟數位科技有限公司 / DuDu Digital Technology Co., Ltd.) stays visible on the
About page. The credit is assembled from a compiled-in constant on every
response: it is never read from config and cannot be written through any RPC.

## For resellers (white-label branding)

Requirements: a license whose tier grants the `white_label` feature, activated
on the instance (`duduclaw license activate <blob>`).

1. Open **Settings → 品牌設定 (Branding)** as an admin.
   Without a white-label license the tab is read-only and shows an upgrade notice.
2. Fill in product name, subtitle, company name, website, support email,
   description.
3. Upload a logo — PNG, JPEG, or WebP up to 512 KB. SVG is rejected
   (script-injection surface). The image is stored as a base64 data URI in
   `~/.duduclaw/branding.json`; no external hosting involved.
4. Optionally set an **accent colour** (`#rrggbb`) — the dashboard derives its
   primary/accent CSS scale from that single hex; leave it empty to keep amber.
5. Optionally write an **About HTML block** — a rich "about the distributor"
   section rendered on the About page. It is sanitized server-side with a
   conservative allowlist (`ammonia`): a small set of formatting tags, `<a>`
   links forced to `rel="nofollow noopener noreferrer" target="_blank"`, and
   `<img>` restricted to the same `data:image/png|jpeg|webp` (≤512 KB,
   magic-byte-checked) rule as the logo. `style` / `class` / `id` / `on*` and
   `<script>` are stripped. The editor previews exactly what will be stored via
   the `branding.preview` RPC. Over 64 KB is rejected.
6. Save. The sidebar mark, login page, browser title, favicon, accent colour,
   and About block update immediately. **Reset** restores the DuDuClaw defaults.

The **About** page (`/about`) shows your branding on top and the fixed
"軟體開發｜嘟嘟數位科技有限公司" block plus version and license tier below.

Validation is fail-closed: unknown fields are rejected, text fields have
CJK-safe length caps, the logo must match its declared magic bytes, and
`branding.set` / `branding.reset` are denied outright when the license
snapshot does not grant `white_label`.

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

### Shipping your branding to customers

A reseller usually wants their brand to appear on their *customers'* instances,
which do not hold a white-label license. A **signed branding bundle** solves
this: the branding is signed by the owner's issuer key, and any instance that
finds a valid bundle at `~/.duduclaw/branding.bundle.json` applies it
automatically — no license required to *display* it (editing still needs the
white-label license). The upstream vendor credit is always layered on top and a
bundle cannot blank it out.

**Producing a bundle**

- *Self-service (online):* on the reseller's own white-label instance, click
  **Generate distribution bundle** (RPC `branding.bundle.create`). The gateway
  sends its `subscription_id` + machine fingerprint + current branding to the
  owner gateway's `POST /v1/branding/sign` (self-gated on the issuer key, per-IP
  rate-limited 10/min), which re-checks the subscription (active + fingerprint,
  same gate as refresh; revoked/expired ⇒ refused), authoritatively re-sanitizes
  the branding, signs it, and returns the bundle for download.
- *Owner co-sign (offline):* when the reseller instance can't reach the owner,
  the owner's **/manage/distributors** page has a **Co-sign bundle** dialog
  (RPC `distributor.bundle.sign`): paste the distributor's branding JSON and the
  owner signs it locally with the issuer key.

**Distributing & applying**

Drop the resulting `branding.bundle.json` into the customer's `~/.duduclaw/`
(e.g. as part of your product installer). On start-up the gateway resolves
branding in this order:

1. local `branding.json` (set via the licensed editor) — wins if present;
2. `branding.bundle.json` whose signature verifies against the baked issuer key;
3. built-in DuDuClaw defaults.

The active source is reported to the dashboard as a top-level `source` field
(`local` / `bundle` / `default`). A bundle whose signature, schema, or field
validation fails is ignored with a single warning (fail-closed → defaults), and
the bundle body is re-sanitized on read so a hand-edited file cannot smuggle
unsafe HTML past the owner's signing check.

## RPC surface

| Method | Access |
|---|---|
| `branding.get`, `about.get` | any logged-in user (response carries `source`, `about_html`, `accent_color`) |
| `branding.set`, `branding.reset`, `branding.preview`, `branding.bundle.create` | admin + `white_label` feature (fail-closed) |
| `distributor.status/list/add/update/remove/issue/revoke`, `distributor.bundle.sign` | admin |

## HTTP control-plane surface (owner gateway, issuer-key-gated)

| Endpoint | Purpose |
|---|---|
| `POST /v1/license/refresh` | Re-sign a license (phone-home). |
| `GET /v1/license/crl` | Signed revocation list. |
| `POST /v1/branding/sign` | Sign a branding bundle for an entitled subscription (rate-limited 10/min/IP). |
