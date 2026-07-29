/**
 * Map a stored session id to the conversation channel it came from.
 *
 * The gateway keys every conversation session by a channel prefix (see the
 * channel modules: `telegram:<chat>`, `discord:<channel>`,
 * `discord:thread:<id>`, `line:<user>`, `slack:dm:<..>`, `whatsapp:<sender>`,
 * `feishu:<chat>`, `googlechat:<space>`, `teams:<conversation>`,
 * `dingtalk:<..>`, `webchat:<conn>#agent:<id>#conv:<nonce>`). Anything without
 * a known prefix is an internal work session (cron / delegation / goal runs)
 * and is NOT a user conversation — the chat page's session list filters those
 * out via {@link isConversationSession}.
 */
const CHANNEL_LABELS: Record<string, string> = {
  webchat: 'Web',
  telegram: 'Telegram',
  discord: 'Discord',
  line: 'LINE',
  slack: 'Slack',
  whatsapp: 'WhatsApp',
  feishu: 'Feishu',
  googlechat: 'Google Chat',
  teams: 'Teams',
  dingtalk: 'DingTalk',
};

export interface SessionChannel {
  /** Stable channel key — the session-id prefix (e.g. `telegram`). */
  key: string;
  /** Display label (brand names, locale-independent). */
  label: string;
}

/** Resolve the originating channel of a session id; null when unknown. */
export function sessionChannel(sessionId: string): SessionChannel | null {
  const sep = sessionId.indexOf(':');
  if (sep <= 0) return null;
  const key = sessionId.slice(0, sep).toLowerCase();
  const label = CHANNEL_LABELS[key];
  return label ? { key, label } : null;
}

/** True when the session is a user conversation on a known channel. */
export function isConversationSession(sessionId: string): boolean {
  return sessionChannel(sessionId) !== null;
}
