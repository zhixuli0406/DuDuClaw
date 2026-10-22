# Thinking display

When you send a message in the dashboard WebChat, the reply area shows that the AI employee is thinking right away, with a counter that keeps running until the answer arrives. If the model reports a thinking summary, you can read a masked, shortened version of it after the answer lands. In messaging apps, colleagues bound to a dashboard account can read the masked summary and everyone else sees the timer only; see [Thinking display in messaging apps](66-thinking-in-messaging-apps.md).

Other languages: [繁體中文](zh-TW/64-thinking-display.md) · [日本語](ja-JP/64-thinking-display.md)

## What you see in WebChat

- **While the reply is on its way:** "Thinking… 12s" next to the animated orb. The counter starts the moment you press send, so it works with every runtime, including ones whose model reports no thinking at all (Grok, Antigravity, Gemini CLI). Past a minute it reads "Thinking… 1m 5s". When the model reports token counts, "~1.2k tokens" appears next to it, and one line below shows the newest progress line the model reported, or else the tool step that is running.
- **After the reply arrives:** the live row becomes "Thought for 12s · ~1.2k tokens" at the top of the task-insights box above the answer. The number is the wall time from sending to the answer arriving, the same number the live counter last showed. Earlier answers in the conversation keep a smaller version of the same line.
- **Expanding it:** only possible when some thinking text was shown. Each block is labelled "Thinking summary", "Raw reasoning (secrets masked)" or "Progress", with its own duration; a block that was cut short ends with "(truncated)".
- **When there is no text:** a model that does not provide a summary shows "This model doesn't provide a thinking summary" and has no expand arrow. Thinking that the outbound checks refused shows "Some thinking was withheld by safety rules"; the text that was already on screen for that block is removed. Raw reasoning from open models with redaction off shows "Enable redaction to show raw reasoning from open models".

The text is written to read as a summary of the thinking, not the full chain of reasoning. Providers only return summaries (or nothing) for most current models.

Screen readers hear one fixed "Thinking…" announcement; the ticking seconds and the progress line are hidden from them so they are not interrupted every second. With reduced motion turned on, the orb is a still image and nothing fades, but the counter keeps counting.

## What is shown to whom

| Viewer | Timer | Tokens | Thinking text |
|---|---|---|---|
| Signed-in dashboard user in WebChat | yes | yes | yes, with `display = "summary"` |
| Website chat widget visitor (public widget key) | no (the gateway sends no thinking frames) | no | no |
| Odoo website Live Chat visitor | nothing | no | no |
| Messaging apps (LINE, Telegram, Discord, Slack, …) | a thinking status message or a "💭 思考了 N 秒" line, see [Thinking display in messaging apps](66-thinking-in-messaging-apps.md) | no | only senders bound to a dashboard account, by default in 1:1 chats only (`[thinking] channel_audience`) |
| ACP, internal sessions | at most the one status line "💭 已收到，正在思考（已 N 秒）。" ([notification governance](40-notification-governance.md)) | no | never |

The timer in WebChat is measured in the browser, so a signed-in user sees it even with thinking display off.

## Masking

Thinking text is masked on the gateway before it is sent, never in the browser:

- `<REDACT:…>` tokens become `〔已遮蔽·category〕` and are **not** restored to the original value (normal replies restore them; thinking text does not).
- With redaction on, the redaction rules run over the text (without writing to the vault and without the on-device NER model).
- Anything shaped like an API key or secret is replaced.
- The outbound reply checks and the employee's `CONTRACT.toml` run on the text. When they refuse it, that block and every later block of the turn show the fixed "withheld" note, and one entry with the rule name only goes to `security_audit.jsonl`. The answer itself is still sent.
- Text is cut to 1,500 characters per block (1,200 for raw open-model reasoning) and to `[thinking] max_chars` per turn. The last few characters of a block that is still streaming are held back until the block ends, so a half-written secret is never shown.

Thinking text is not stored anywhere: not in the conversation history, not in the run log, not in cost telemetry. The browser keeps it in memory for the open page only. The Run log page keeps one row per turn with the seconds and token count, for example "Thought for 8s · ~1.2k tokens".

## Settings

```toml
# config.toml
[thinking]
display = "summary"              # summary | status | off (gateway-wide ceiling)
max_chars = 4000                 # thinking text per turn, all blocks (200–20000)
anthropic_display = "summarized" # summarized | updates (beta, only some Claude models)
```

```toml
# agents/<id>/agent.toml
[model]
show_thinking = "summary"        # summary | status | off; unset = follow [thinking] display
```

- `summary`: timer, tokens and masked thinking text.
- `status`: timer and tokens, no text.
- `off`: the gateway sends no thinking frames; WebChat still shows its own timer.
- The stricter of the two values wins (`off` < `status` < `summary`), so an employee can only narrow what the gateway allows.
- A `config.toml` that exists but cannot be read or parsed, or an unknown `display` value, is treated as `status` (no text). An unknown `show_thinking` value is ignored with a warning.
- Both files are read on every turn, so a change applies from the next reply. There is no dedicated dashboard control yet; edit the values in Settings → Advanced → Advanced config editor (see the [dashboard settings map](../guides/dashboard-settings.md#thinking-display)).

Showing thinking does not change cost. Thinking tokens are billed by the provider whether or not they are shown; `[model] effort` is the only setting here that changes how much a model thinks. DuDuClaw never turns thinking on for a model that does not think by default just to display it.

## Current limits

- Runtimes start reporting thinking in a later release. Until then WebChat shows the timer for every reply, and the "Thought for" line has no tokens and nothing to expand.
- `assistant_done` carries `thinking: {tokens, blocks, kinds}` only when thinking frames were sent in that turn.
