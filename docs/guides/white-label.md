# Distributor name and logo

In v1.78.0, a distributor's customers can receive a product name and logo on any license tier. The gateway automatically receives and verifies the brand associated with the active subscription. Open-source and unlicensed installs have no subscription and use the original DuDuClaw brand.

Customers do not need to install a branding file or change configuration. There is no customer-side name or logo setter in config, HTTP or RPC. Old `branding.json` and `branding.bundle.json` files are ignored. Contact your distributor to change or remove the supplied brand.

## Receiving brand updates

A brand is effective only while its signature verifies, its subscription matches the active license, and the license passes expiry and offline-grace checks. Without an effective brand, the gateway uses DuDuClaw. A failed fetch or invalid document does not prevent startup.

A failed refresh keeps the current verified brand while the license remains valid and its subscription still matches. An explicit removal from the distributor, an invalid license, a subscription change or a cached document that fails verification removes it. The dashboard checks for changes during session refresh, reconnect and tab focus; desktop checks every 30 minutes. An update can also restore the original DuDuClaw brand.

## Where the brand appears

The dashboard uses the name and logo, including product mascot positions, login and welcome screens, page title, favicon and LINE QR posters. Channel footers, OTP and OAuth pages use the supplied name. Product copy supports English, Traditional Chinese and Japanese. Legal notices and privacy links keep their original text; CLI and logs keep their existing name.

When branded, About shows the distributor logo and name first, the [distributor notice](distributor-notice.md) next, then a fixed **Software development** block with vendor, website, version and license type. **嘟嘟數位科技有限公司 / Dudu Technology Ltd.** and the vendor website are compile-time constants and cannot be changed or hidden by the distributor. Branding covers only the product name and logo; it does not provide a subtitle, accent colour, custom company fields or custom About HTML.

Desktop window titles, tray tooltips and update dialogs use the runtime name. Installer names, the macOS Dock name and application icons remain compile-time assets. Changing them requires a separately packaged desktop installer; runtime brand updates do not change them. Logos preserve their aspect ratio inside the display box.

## Read-only integration

`about.get` includes `brand` only when a brand is effective. Public read-only `GET /api/brand` returns `product_name`, `logo_light` and `logo_dark`, without license or customer information. It returns a top-level object for an effective brand, or `null` to use the original DuDuClaw identity.

`about.get` includes `brand_revision`; `{ "brand_revision_only": true }` returns only that field. `/api/brand` exposes the same revision in `X-Brand-Revision`, with an ETag, `If-None-Match`/304 support and a 60-second public cache. Requests, including conditional requests, are limited to 30 per IP per minute; excess requests receive 429 with `Retry-After: 60`.
