# ADR-003: Excluded messaging channels (Signal / personal WeChat / Viber)

- Status: Accepted
- Date: 2026-07-08
- Deciders: DuDuClaw maintainers

## Context

DuDuClaw ships nine channels (Telegram, LINE, Discord, Slack, WhatsApp, Feishu,
Google Chat, Microsoft Teams, WebChat). During the 2026-07 channel-gap review
three further platforms were evaluated and **deliberately excluded**. Recording
that decision here prevents the same options being re-investigated every quarter,
and — more importantly — stops someone shipping a risky unofficial dependency
without knowing the trade-off.

## Decision

Do **not** build first-party connectors for Signal, personal-account WeChat, or
Viber at this time.

### Signal

- No official bot API exists. `signal-cli` is the community integration path.
- Correction to earlier research: `signal-cli` uses the **official `libsignal`**
  library (not a reverse-engineered protocol), but it still drives a *personal*
  Signal account — there is no sanctioned bot platform. That carries
  rate-limiting and account-unregistration risk, and Signal's terms are oriented
  toward human use.
- Verdict: low ROI relative to the operational risk of a personal-account bridge.
  Revisit only if Signal publishes an official bot/business API.
- Source: https://signal.org/docs/

### WeChat (personal accounts)

- No official API for personal accounts; unofficial bridges routinely trigger
  account bans.
- The **enterprise** path (WeCom / 企業微信) is a *separate*, sanctioned product
  with official webhook + WebSocket bot support and is tracked as its own
  (non-excluded) backlog item for cross-strait SMB use. This ADR excludes only
  the personal-account variant.

### Viber

- Since 2024-02-05 Viber's bot/business messaging is **commercial-contract only**
  at roughly **€100/month** minimum.
- Correction to earlier research: the core fact (paid commercial gate) holds; the
  "~15-minute retry" operational detail cited earlier was inaccurate and is not
  relied upon here.
- Verdict: the fixed monthly floor makes it uneconomic for the individual /
  one-person-company target user until there is concrete demand.
- Source: https://developers.viber.com/docs/api/rest-bot-api/

## Consequences

- These three are marked "evaluated, declined" so future planning skips re-triage.
- Users needing Signal/Viber reach should route through Matrix bridges (Matrix is
  a separate, non-excluded candidate) or email, not a first-party connector.
- If any of the blocking conditions change (Signal official bot API; a Viber
  free/low tier), reopen with a new ADR superseding this one.
