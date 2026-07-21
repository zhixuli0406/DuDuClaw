import { describe, it, expect } from 'vitest';
import {
  buildUserMessageFrame,
  frameBelongsToConversation,
  historyToMessages,
  isResumeNotFound,
  useChatStore,
} from './chat-store';

describe('buildUserMessageFrame (L1 per-agent routing)', () => {
  it('omits the agent key when no partner is selected (byte-compatible)', () => {
    const frame = buildUserMessageFrame({
      content: 'hello',
      sessionId: 'webchat:x',
      agentId: null,
      attachments: [],
    });
    expect(frame).toEqual({
      type: 'user_message',
      content: 'hello',
      session_id: 'webchat:x',
      attachments: [],
    });
    expect('agent' in frame).toBe(false);
  });

  it('includes the agent id when a partner is selected', () => {
    const frame = buildUserMessageFrame({
      content: 'hi',
      sessionId: 'webchat:x',
      agentId: 'sales-bot',
      attachments: [],
    });
    expect(frame.agent).toBe('sales-bot');
    expect(frame.type).toBe('user_message');
  });

  it('includes the conv nonce when set, omits it when null', () => {
    const withConv = buildUserMessageFrame({
      content: 'hi',
      sessionId: 'webchat:x',
      agentId: null,
      attachments: [],
      convId: 'c-42',
    });
    expect(withConv.conv).toBe('c-42');

    const without = buildUserMessageFrame({
      content: 'hi',
      sessionId: 'webchat:x',
      agentId: null,
      attachments: [],
      convId: null,
    });
    expect('conv' in without).toBe(false);
  });

  it('maps attachments to the wire shape', () => {
    const frame = buildUserMessageFrame({
      content: '',
      sessionId: null,
      agentId: null,
      attachments: [{ name: 'a.png', mime: 'image/png', dataBase64: 'AAA' }],
    });
    expect(frame.attachments).toEqual([{ filename: 'a.png', mime: 'image/png', data_base64: 'AAA' }]);
  });

  it('resume: a resumed session id + selected agent yields a resume-shaped frame', () => {
    // Resuming = the next user_message carries the past session id (different
    // from the connection's own) plus the owning agent so the gateway continues
    // that server-side session.
    const frame = buildUserMessageFrame({
      content: 'continue please',
      sessionId: 'webchat:past-42',
      agentId: 'sales-bot',
      attachments: [],
    });
    expect(frame.session_id).toBe('webchat:past-42');
    expect(frame.agent).toBe('sales-bot');
    expect(frame.type).toBe('user_message');
  });
});

describe('historyToMessages (WP3 resume)', () => {
  it('maps roles, timestamps and tokens from the history wire shape', () => {
    const msgs = historyToMessages([
      { role: 'user', content: 'hi', timestamp: '2026-07-12T00:00:00Z' },
      { role: 'assistant', content: 'hello', timestamp: '2026-07-12T00:00:05Z', tokens: 12 },
      { role: 'system', content: 'note', timestamp: '2026-07-12T00:00:10Z' },
    ]);
    expect(msgs.map((m) => m.role)).toEqual(['user', 'assistant', 'system']);
    expect(msgs[0].timestamp).toBe(Date.parse('2026-07-12T00:00:00Z'));
    expect(msgs[1].tokens).toBe(12);
    // No tokens on a message → the key is omitted (matches live-stream shape).
    expect('tokens' in msgs[0]).toBe(false);
    // Every mapped message gets a unique local id.
    expect(new Set(msgs.map((m) => m.id)).size).toBe(3);
  });

  it('collapses unknown roles to user and survives a bad timestamp', () => {
    const before = Date.now();
    const [m] = historyToMessages([{ role: 'tool', content: 'x', timestamp: 'not-a-date' }]);
    expect(m.role).toBe('user');
    expect(m.timestamp).toBeGreaterThanOrEqual(before);
  });
});

describe('isResumeNotFound (WP3 resume miss)', () => {
  it('matches the gateway resume-miss error, case-insensitively', () => {
    expect(isResumeNotFound('conversation not found')).toBe(true);
    expect(isResumeNotFound('Conversation not found')).toBe(true);
    expect(isResumeNotFound('error: conversation not found for id x')).toBe(true);
  });

  it('does not match unrelated errors or empty input', () => {
    expect(isResumeNotFound('rate limited')).toBe(false);
    expect(isResumeNotFound('')).toBe(false);
    expect(isResumeNotFound(null)).toBe(false);
    expect(isResumeNotFound(undefined)).toBe(false);
  });
});

describe('frameBelongsToConversation (misrouted-reply attribution guard)', () => {
  it('accepts a frame whose conv matches the current conversation', () => {
    expect(frameBelongsToConversation('c-1', 'c-1')).toBe(true);
  });

  it('drops a frame whose conv belongs to a different conversation', () => {
    // The reported bug: conversation A's reply (conv c-1) arriving while the user
    // has opened conversation B (c-2) must NOT render into B.
    expect(frameBelongsToConversation('c-1', 'c-2')).toBe(false);
  });

  it('accepts untagged frames (legacy gateway / connection-level)', () => {
    expect(frameBelongsToConversation(undefined, 'c-2')).toBe(true);
    expect(frameBelongsToConversation(null, 'c-2')).toBe(true);
    expect(frameBelongsToConversation('', 'c-2')).toBe(true);
    expect(frameBelongsToConversation(123, 'c-2')).toBe(true);
  });
});

describe('conversation nonce lifecycle (misrouted-reply fix)', () => {
  it('reset() (New conversation) mints a new conv nonce and does not delete the old', () => {
    useChatStore.setState({
      ownSessionId: 'webchat:conn-own',
      sessionId: 'webchat:conn-own',
      selectedAgentId: null,
      convId: 'c-old',
    });
    useChatStore.getState().reset();
    const st = useChatStore.getState();
    expect(st.convId).not.toBe('c-old');
    expect(st.sessionId).toBe('webchat:conn-own');
    expect(st.messages).toEqual([]);
  });

  it('selectAgent() and resumeSession() each bump the conv nonce', () => {
    useChatStore.setState({
      ownSessionId: 'webchat:conn-own',
      sessionId: 'webchat:conn-own',
      selectedAgentId: null,
      convId: 'c-base',
    });
    useChatStore.getState().selectAgent('sales-bot');
    const afterSelect = useChatStore.getState().convId;
    expect(afterSelect).not.toBe('c-base');

    useChatStore.getState().resumeSession('webchat:past-42', []);
    expect(useChatStore.getState().convId).not.toBe(afterSelect);
  });
});

describe('G1 — leaving a resumed session restores the connection own session', () => {
  // reset() / selectAgent() only mutate store state here (no live socket in the
  // test env). We seed `ownSessionId` directly — it is normally set from the
  // first `session_info` frame over the wire.
  it('reset() after a resume points sessionId back at ownSessionId', () => {
    useChatStore.setState({
      ownSessionId: 'webchat:conn-own',
      sessionId: 'webchat:conn-own',
      selectedAgentId: null,
    });
    useChatStore.getState().resumeSession('webchat:past-42', []);
    expect(useChatStore.getState().sessionId).toBe('webchat:past-42');

    useChatStore.getState().reset();
    expect(useChatStore.getState().sessionId).toBe('webchat:conn-own');
  });

  it('selectAgent() after a resume points sessionId back at ownSessionId', () => {
    useChatStore.setState({
      ownSessionId: 'webchat:conn-own',
      sessionId: 'webchat:conn-own',
      selectedAgentId: null,
    });
    useChatStore.getState().resumeSession('webchat:past-42', []);
    expect(useChatStore.getState().sessionId).toBe('webchat:past-42');

    useChatStore.getState().selectAgent('sales-bot');
    expect(useChatStore.getState().sessionId).toBe('webchat:conn-own');
    expect(useChatStore.getState().selectedAgentId).toBe('sales-bot');
  });
});
