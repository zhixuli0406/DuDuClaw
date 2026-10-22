# Distributor information on the About page

A distributor build of DuDuClaw can show one block of information about the
authorised distributor on the dashboard About page (`/about`), above the fixed
vendor block. Public builds do not carry one, and their About page and
`about.get` response are unchanged.

The distributor maintains the content itself on the "Distributor information"
page of the license console (the organisation's admin role). It reaches
customers' dashboards two ways:

- **Runtime update.** About one minute after start and then every six hours, a
  licensed gateway asks the control-plane for the current notice of the
  distributor that issued its license. The answer is signed with the key that
  signs licenses and bound to that license; the gateway verifies it, sanitizes
  it again and keeps the signed document in `<home>/distributor_notice.json`.
  Editing that file only breaks the signature and falls back to the compiled
  notice. A distributor's edit therefore reaches online customers within about
  six hours, without a new build.
- **Build input.** The release pipeline pulls the notice from the control-plane
  and compiles it in, for offline installs and gateways that have not reached
  the control-plane yet. It embeds one only when exactly one active
  distributor has a notice; with several, the shared image embeds none
  (customers still get their own distributor's notice at runtime).

When both exist the runtime one wins; a signed "distributor cleared it" also
overrides the compiled copy. No setting or RPC on an installed system can
change the block, so an end customer cannot replace it.

White-label branding (custom product name, logo, colours) stays removed; see
[white-label.md](white-label.md). This block does not change the product name,
the vendor credit, the version or the license tier shown on the page.

## Build input

```bash
DUDUCLAW_DISTRIBUTOR_NOTICE_PATH=/absolute/path/to/notice.html \
  cargo build --release -p duduclaw-cli
```

`crates/duduclaw-gateway/build.rs` reads the variable:

- Unset: no notice (every public build).
- Set: it must be an absolute path to a regular file (symlinks are refused),
  UTF-8 (a leading byte-order mark is dropped), at most **64 KB including
  embedded images**. A set-but-empty variable, a missing file or any other
  violation fails the build with a message saying why.
- Cargo rebuilds when the file or the variable changes.
- The HTML is sanitized during the build (rules below). Anything the sanitizer
  removed is listed as `warning: distributor notice: …` lines in the cargo
  output. If nothing displayable remains, or the sanitized HTML grows past
  64 KB (every link gains `rel` and `target`), the build fails.

This input is separate from `DUDUCLAW_INSTALLATION_POLICY_PATH`. The
installation policy only blocks channels, runtimes and providers and must not
carry identities.

## HTML that is kept

- Elements: `p`, `br`, `h1`–`h4`, `ul`, `ol`, `li`, `strong`, `em`, `u`, `s`,
  `a`, `blockquote`, `code`, `pre`, `hr`, `img`, `div`, `span`, `table`,
  `thead`, `tbody`, `tr`, `th`, `td`. `<script>` and `<style>` are removed
  together with their content; every other element is removed and its text
  kept.
- Attributes: only `a[href]` and `img[src]`, `img[alt]`. `style`, `class`,
  `id` and every `on*` handler are removed. Comments are removed.
- Links: absolute `http://`, `https://` or `mailto:` only, without a
  `user:password@` part. Every link gets
  `rel="nofollow noopener noreferrer" target="_blank"`.
- Images: only `data:image/png`, `data:image/jpeg` or `data:image/webp`
  base64 URIs whose bytes match the declared type, at most 512 KB each (the
  64 KB document limit is usually the tighter one). SVG and remote images are
  dropped.

The gateway sanitizes the compiled HTML again when it first serves it. If that
second pass refuses it, `about.get` leaves the field out and the gateway logs
a warning.

## How the dashboard shows it

- A card above the vendor block with the fixed heading 「總經銷資訊」 and the
  caption 「由授權總經銷提供」 (localized in English and Japanese). The heading
  is product copy, not part of the notice.
- The notice renders only inside an iframe with
  `sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"`:
  no scripts, no forms, no top-level navigation. Links open in a new tab.
- Why `allow-same-origin` is safe here: without `allow-scripts` nothing in
  the frame can execute (no `<script>`, no inline handlers, no `javascript:`
  URLs), so the frame cannot use the dashboard origin. The token only lets
  the dashboard read the rendered height to size the card. `allow-scripts`
  must never be added: combined with `allow-same-origin` it would let the
  frame lift its own sandbox.
  A content security policy inside the frame blocks every network request, so
  images must be embedded as data URIs.
- The frame sizes itself to the notice (measured on load, when the frame
  body resizes and when an image loads), between 64 px and 600 px; a longer
  notice scrolls inside the frame. The height change animates briefly unless
  the system asks for reduced motion.
- The frame follows the dashboard's light or dark theme. The notice's own
  styles are removed, so the dashboard's base styles apply.

## Checking a build

Call `about.get` from a logged-in dashboard session. A distributor build
returns `distributor_notice` with the sanitized HTML; a public build returns
no such key.

## Runtime update details

- Endpoint: `POST /v1/license/distributor-notice` with `subscription_id`,
  `machine_fingerprint` and a fresh `nonce`. The control-plane echoes the
  nonce; an answer without the matching nonce is discarded (replay guard).
- `distributor_notice: null` means the license has no distributor, and the
  gateway returns to the compiled notice. Otherwise it is a signed document
  (`kind`, `subscription_id`, `distributor_id`, `version`, `html` or `null`,
  `sha256`, `issued_at`, `public_key_id`, `signature`).
- Refused: a bad signature or unknown key, a document for another license,
  `html` not matching `sha256` or over 64 KB, a version older than the one in
  force, HTML the sanitizer refuses. The gateway keeps what it shows and logs
  a warning.
- Replacing or revoking the license deletes the cached file; the compiled
  notice applies until the next fetch brings the new license's.
- A disabled distributor gets a signed "show nothing".
