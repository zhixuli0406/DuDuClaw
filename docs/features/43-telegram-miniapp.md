# Telegram Mini App approval card

> Read the full approval — action description, simulated consequences, countdown — and decide inside Telegram, without switching to the dashboard.

---

## The problem it solves

When a high-risk action needs a human's consent, DuDuClaw pushes a card to Telegram with two buttons: approve and deny. The buttons are enough, but the card cannot carry what the judgment actually needs — the complete action description, the simulated consequences, and how long remains before the request auto-denies. Those details used to live only in the dashboard, so "decide on the phone" turned into "switch to a browser, log in, then find the right entry".

This feature moves those three things into Telegram. The card gains a third button, 「🔎 查看詳情」 (view details). Tapping it expands a small screen above the conversation; you read the full picture, then approve or deny on the spot. The whole exchange never leaves Telegram.

> **This is a spike, off by default.** Its scope is this single approval-detail card: the goal is to first prove that "putting an interface inside the channel" is a workable path, then decide whether to extend it to task, spending, and status screens.

---

## How it works

```
High-risk action needs consent
        |
        v
Card pushed to your 1:1 chat with the bot
   [approve]  [deny]  [🔎 view details]
        |  tap the third button
        v
Mini App opens above the conversation
        |  Telegram attaches signed initData
        v
Gateway recomputes the HMAC and verifies it
        |  match --> details arrive via POST
        v
Full description + simulated consequences
           + live countdown
        |  approve / deny
        v
Screen closes; the card in the chat
collapses in place to a one-line result
```

### What the screen shows

- **What kind of decision this is**: a label inline with the card content (「⚠️ 高風險動作需要你同意」 — a high-risk action needs your approval).
- **Who wants to do what**: the AI teammate's name plus a plain-language description of the action ("run a high-risk tool", "install a new skill/tool", ...).
- **The content**: the complete description, not truncated the way a chat bubble is.
- **What happens next if you approve**: one to three consequence steps the system simulated beforehand. An approval without a simulation simply omits this section — nothing is made up.
- **Time remaining**: a countdown updated every second, turning red when less than a third is left. No answer before the deadline means an automatic deny.
- **Two large buttons**: approve this action / deny this action. On success the screen closes by itself and you are back in the conversation.

The screen's colors follow Telegram's theme (dark mode, light mode, and custom themes all apply), so it never punches a white rectangle into a dark chat.

---

## Enabling it (all three conditions required)

| Condition | How to set it | When it is not met |
|---|---|---|
| Feature switch | `config.toml`: `[miniapp]` `enabled = true` | Endpoint returns 404; the card keeps its original two buttons |
| Public URL is **https** | `config.toml`: `[dashboard] public_url = "https://your.domain"` | No details button (Telegram hard-requires https for Mini Apps) |
| Card delivered to a **1:1 private chat** | Decided by the push destination | No details button (Telegram permits `web_app` buttons in private chats only) |

```toml
# ~/.duduclaw/config.toml
[miniapp]
enabled = true

[dashboard]
public_url = "https://ai.example.com"
```

Restart the gateway after the change. If any of the three conditions fails, the card is **exactly** what it was before this feature existed — no missing button, no undelivered message.

An address like `http://localhost:18789` is a valid dashboard location but not a valid Mini App location. Deployments without a public https domain (most local installs) need to do nothing; the feature stays silent for them.

---

## Security model

### Proving identity

When Telegram opens a Mini App it attaches a signed `initData` string containing the user id and the open time; the signing key derives from **your own bot token**. DuDuClaw recomputes it exactly as the official documentation specifies:

```text
data_check_string = every field except hash, sorted alphabetically, joined with \n
secret_key        = HMAC_SHA256(<bot_token>, "WebAppData")
hash              = hex(HMAC_SHA256(data_check_string, secret_key))
```

The result is compared with Telegram's `hash` in constant time. **On mismatch, nothing comes back** — not even confirmation that the approval exists, let alone a trimmed-down view.

Three more gates sit behind the signature:

1. `auth_date` older than 1 hour counts as expired (a screen left open too long must be reopened from the conversation); a timestamp absurdly far in the future is rejected the same way.
2. The `user` field must contain a numeric id, or the request is refused.
3. The endpoint enforces a per-IP, per-minute request cap.

The `?id=<number>` in the URL **is not a credential.** Knowing the id shows nobody anything — the page itself carries no data; data is only obtainable through the signature-verified POST.

### Who can decide

**There is no second permission system.** The Telegram user id the Mini App identifies is the same id reported when a card button is pressed, so a submitted decision travels the exact same path as the buttons:

- Someone with a verified binding in the dashboard whose role is admin or supervisor can decide;
- A regular staff account cannot — even when that person is the recipient (separation of duties);
- On a deployment where nobody has bound a channel account yet, only the account the card was delivered to can decide.

Viewing details applies the same rule, not a looser read-only one: whoever cannot decide cannot see the content either.

### Secrets stay put

The bot token serves only as HMAC key material — never logged, never echoed in responses, never embedded in the page. The page itself exposes no backend details; every error message is written for humans ("please reopen this from the conversation").

---

## Known limitations

- **Groups never get the button.** Telegram shows `web_app` buttons only in a user's private chat with the bot. A card pushed to a group keeps the original approve/deny pair — deliberately: forcing the button in would make the whole message undeliverable.
- **https is mandatory.** Telegram's rule, not this project's choice.
- **Only this one approval card.** Task, spending, and status cards still use their existing buttons and dashboard deep links.
- **Only Telegram.** LINE LIFF, Teams Dialog, and Feishu Web App offer equivalent capability but were not built this round; Slack, WhatsApp, Google Chat, and Discord have no equivalent mechanism and will stay on the "card buttons + native forms" track.
- **The page fetches the platform SDK from telegram.org.** That is the only supported way to obtain `Telegram.WebApp` (`initData`, theme parameters, window close). If the SDK cannot load, the page does not break — it reads the same data from the URL fragment (`tgWebAppData`) and only loses auto-close. Beyond that the page has zero external resources: all CSS and JS are inlined; no CDN, no fonts, no images.

---

## Verifying on a real deployment

A local machine has no public https, so the webview will not open; run these steps on a deployment with a public address:

1. Set `[miniapp] enabled = true` and an https `[dashboard] public_url`, then restart the gateway.
2. Open `https://your.domain/miniapp/approval?id=test` in a browser — you should see 「請從對話中開啟」 (please open this from the conversation), which proves the page is alive and hands out no data without an identity.
3. Trigger an action that needs human approval so a card lands in your **private chat** with the bot.
4. The card should show a third button, 「🔎 查看詳情」; tapping it opens the full content and the countdown.
5. Approve or deny inside the screen → the original card in the chat collapses in place to a one-line result (the same behavior as pressing the buttons).
6. Negative test: hand the same URL to someone without decision permission; opening it should show only 「您沒有查看這筆決定的權限」 (you do not have permission to view this decision).

---

## Related features

- [High-risk action approval and simulation](34-goal-loop.md)
- [Notification governance and quiet hours](40-notification-governance.md)
- [Human takeover](42-human-takeover.md)
