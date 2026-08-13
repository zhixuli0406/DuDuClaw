=== DuDuClaw WebChat Widget ===
Contributors: zhixuli0406
Tags: chat, ai, customer support, chatbot, self-hosted
Requires at least: 6.0
Tested up to: 6.7
Stable tag: 0.1.0
Requires PHP: 7.4
License: GPLv2 or later
License URI: https://www.gnu.org/licenses/gpl-2.0.html

Add a chat bubble that lets visitors talk to the AI employees on YOUR self-hosted DuDuClaw gateway. No third-party service, no telemetry.

== Description ==

[DuDuClaw](https://github.com/zhixuli0406/DuDuClaw) is a self-hosted, multi-runtime AI agent platform. This plugin embeds a floating chat widget on your site so visitors can talk to your AI employee directly.

**External service disclosure**: the widget's front-end connects ONLY to the DuDuClaw gateway URL you configure in Settings → DuDuClaw WebChat — typically a server you own and operate. No other host is ever contacted. The plugin collects no analytics and sets no tracking cookies. Chat content is transmitted to (and only to) your own gateway; your gateway's own privacy policy applies.

== Installation ==

1. Install and activate the plugin.
2. On your DuDuClaw gateway, enable widget mode in `config.toml`:
   `[webchat]`
   `public_widget = true`
   `widget_key = "<at least 16 characters>"`
   and add your site's domain to `[gateway] allowed_origins`.
3. In WP Admin → Settings → DuDuClaw WebChat, enter the gateway URL (must be https and reachable by visitors) and the same widget key.

Note: the widget key appears in your public page source by design — it authorizes exactly one thing (anonymous visitor chat, rate-limited by the gateway) and nothing else.

== Frequently Asked Questions ==

= Does this send my visitors' messages to a third party? =
No. Messages go only to the gateway URL you configure — your own server.

= Why is nothing showing up? =
The widget renders only when both gateway URL and widget key are configured. Also confirm your site's domain is in the gateway's `allowed_origins` and `public_widget = true`.

== Changelog ==

= 0.1.0 =
* First release: floating chat widget over the DuDuClaw WebChat protocol, visitor mode with public widget key.
