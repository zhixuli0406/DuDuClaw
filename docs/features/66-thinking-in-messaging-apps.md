# Thinking display in messaging apps

People who talk to an AI employee in Telegram, LINE, Slack and the other messaging apps can see that it is thinking, and colleagues who are bound to a dashboard account can also read a short, masked summary of the thinking. Customers see the timer only, group chats show the timer only by default, and website visitors see nothing.

The summary is the same text the dashboard chat shows (see [Thinking display](64-thinking-display.md) and [Where thinking comes from](65-thinking-sources.md)): masked first (redaction tokens become 〔已遮蔽·category〕 and are never restored, credential-shaped words are replaced, the CONTRACT and guardrail checks run) and capped. It is never stored.

## What each app shows

| App | While thinking | When the answer is ready |
|---|---|---|
| Telegram, Discord, Slack, Microsoft Teams | One status message "💭 思考中… 12 秒", updated at most every 3 seconds, with the latest summary line for senders who may read it | The same message is edited into "💭 思考了 12 秒" with the summary: an expandable quote in Telegram, a spoiler in Discord (mentions disabled), a small context block in Slack, the first 300 characters in Teams. The answer follows below it |
| LINE (1:1 chats) | The LINE loading animation (renewed every 50 seconds) | One small extra message "💭 思考了 12 秒" (+ the first 300 characters of the summary) placed before the answer in the same reply call. Nothing is added when the answer already uses all five messages of a reply, or when the answer goes out by push after the reply token expired: thinking never costs an extra push |
| LINE (groups) | Nothing (LINE has no loading animation for groups) | The same extra message, timer only by default |
| WhatsApp, Feishu, WeCom, DingTalk, Google Chat | The existing "already received" status line | One line "💭 思考了 12 秒" (+ the first 300 characters of the summary) at the end of the answer, above the "Powered by DuDuClaw" footer |

"12 秒" is the time from the moment the message arrived until the answer was ready, the same number the dashboard chat shows. Nothing is shown when the model did not report any thinking. On the editable apps the "already received" status line is not posted while the thinking status shows; if the reply ends up empty, the status message is removed. Labels follow `[general] default_language` (Traditional Chinese, English or Japanese).

## Who sees what

`config.toml`:

```toml
[thinking]
channel_audience = "internal"     # internal | all | none
channel_group_display = "status"  # summary | status | off
```

| Setting | One-to-one chat | Group chat |
|---|---|---|
| `internal` (default), sender bound to a dashboard account | timer + summary | timer (`channel_group_display`) |
| `internal`, any other sender | timer | timer |
| `all` | timer + summary | timer (`channel_group_display`) |
| `none` | nothing | nothing |

- "Bound" means the sender's channel identity is verified and belongs to an active dashboard account (the same check `@` calling uses). A failed lookup counts as not bound.
- `channel_group_display` is the most a group chat shows, whoever speaks. Set it to `summary` to let bound colleagues read summaries in groups too.
- The stricter of `[thinking] display` and the answering employee's `[model] show_thinking` caps everything: `status` means the timer only, `off` means nothing. An employee called with `@` uses its own setting.
- A `config.toml` that cannot be read or parsed, or an unknown value, shows nothing.
- Website visitors (Odoo website live chat, the WebChat widget) never get thinking, whatever the settings.

Only senders who may read the summary make the runtime ask for thinking text (see [Where thinking comes from](65-thinking-sources.md)); for everyone else requests stay exactly as before. Showing thinking does not change what a turn costs.

Not verified on real accounts: how Telegram renders the expandable quote and Discord the spoiler on every client, Slack's context block in threads, the LINE extra message on a real phone, and Teams edits. The messages were checked against a local stand-in for each API.
