# Distributor information on the About page

A distributor build of DuDuClaw can show one block of information about the
authorised distributor on the dashboard About page (`/about`), above the fixed
vendor block. Public builds do not carry one, and their About page and
`about.get` response are unchanged.

The block is a **build input**. Its HTML is compiled into the gateway binary
when the distributor build is produced. No setting, RPC or file on an installed
system can change it, so an end customer cannot replace it with other content.
The flip side: changing the text means producing and shipping a new build.

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
  `sandbox="allow-popups allow-popups-to-escape-sandbox"`: no scripts, an
  opaque origin, no forms, no top-level navigation. Links open in a new tab.
  A content security policy inside the frame blocks every network request, so
  images must be embedded as data URIs.
- The frame is 280 px high and the notice scrolls inside it when longer. A
  frame without scripts cannot report its content height, so the height is
  fixed.
- The frame follows the dashboard's light or dark theme. The notice's own
  styles are removed, so the dashboard's base styles apply.

## Checking a build

Call `about.get` from a logged-in dashboard session. A distributor build
returns `distributor_notice` with the sanitized HTML; a public build returns
no such key.
