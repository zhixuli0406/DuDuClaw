import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { shouldRenderChatFrame, useChatStore } from './chat-store';

class FakeSocket {
  static OPEN = 1;
  static last: FakeSocket;
  readyState = 1;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() { FakeSocket.last = this; }
  send(frame: string) { this.sent.push(frame); }
  close() {}
}

function deliver(frame: Record<string, unknown>) {
  FakeSocket.last.onmessage?.({ data: JSON.stringify(frame) });
}

function send(content: string): Record<string, unknown> {
  useChatStore.getState().send(content);
  return JSON.parse(FakeSocket.last.sent.at(-1)!);
}

beforeEach(() => {
  vi.stubGlobal('WebSocket', FakeSocket);
  useChatStore.getState().disconnect();
  useChatStore.getState().connect();
  useChatStore.setState({
    messages: [], thinking: null, isStreaming: false, activeMessageId: null,
    sessionId: 'webchat:own', ownSessionId: 'webchat:own', selectedAgentId: null,
    resumedExplicitly: false,
  });
});

afterEach(() => {
  useChatStore.getState().disconnect();
  vi.unstubAllGlobals();
});

describe('concurrent WebChat frame attribution', () => {
  it('keeps the foreground turn running when another session reports cancellation or a limit', () => {
    const first = send('first');
    useChatStore.getState().reset();
    const second = send('second');
    const before = useChatStore.getState().sessionsRevision;
    for (const code of ['cancelled', 'turn_limit']) {
      deliver({ type: 'error', message: code, code, conv: first.conv, msg_id: first.msg_id, session_id: 'webchat:first' });
    }
    expect(useChatStore.getState().messages.map(message => message.content)).toEqual(['second']);
    expect(useChatStore.getState().isStreaming).toBe(true);
    expect(useChatStore.getState().activeMessageId).toBe(second.msg_id);
    expect(useChatStore.getState().sessionsRevision).toBe(before + 1);
    deliver({ type: 'assistant_done', content: 'second reply', tokens_used: 2, conv: second.conv, msg_id: second.msg_id });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('second reply');
    expect(useChatStore.getState().isStreaming).toBe(false);
  });

  it('drops progress and resume confirmations from another session', () => {
    const first = send('first');
    useChatStore.getState().reset();
    send('second');
    deliver({ type: 'progress', content: 'first progress', conv: first.conv, msg_id: first.msg_id });
    deliver({ type: 'session_info', session_id: 'webchat:first', agent_name: 'Other', conv: first.conv, msg_id: first.msg_id });
    expect(useChatStore.getState().steps).toEqual([]);
    expect(useChatStore.getState().sessionId).toBe('webchat:own');
  });

  it('rejects late frames from an earlier message even within the same conversation', () => {
    const first = send('first');
    deliver({ type: 'assistant_done', content: 'first reply', tokens_used: 1, conv: first.conv, msg_id: first.msg_id });
    const second = send('second');
    deliver({ type: 'error', code: 'cancelled', message: 'late', conv: second.conv, msg_id: first.msg_id });
    deliver({ type: 'assistant_chunk', content: 'late', conv: second.conv, msg_id: first.msg_id });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('second');
    expect(useChatStore.getState().isStreaming).toBe(true);
    deliver({ type: 'error', code: 'turn_limit', message: 'limit', conv: second.conv, msg_id: second.msg_id });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('limit');
    expect(useChatStore.getState().isStreaming).toBe(false);
  });

  it.each(['turn_not_found', 'duplicate_message'])('keeps an accepted turn alive after %s targets it', (code) => {
    const turn = send('first');
    // Both the fixed protocol and older gateways that echoed the target id.
    for (const correlation of [{ request_id: turn.msg_id }, { msg_id: turn.msg_id }]) {
      deliver({ type: 'error', code, message: 'control failed', conv: turn.conv, ...correlation });
      expect(useChatStore.getState().activeMessageId).toBe(turn.msg_id);
      expect(useChatStore.getState().isStreaming).toBe(true);
    }
    deliver({ type: 'assistant_done', content: 'real reply', tokens_used: 1, conv: turn.conv, msg_id: turn.msg_id });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('real reply');
    expect(useChatStore.getState().isStreaming).toBe(false);
  });

  it('correlates admission limits to the rejected request instead of another active turn', () => {
    const turn = send('first');
    for (const correlation of [{ request_id: 'other' }, { msg_id: 'other' }, {}]) {
      deliver({ type: 'error', code: 'turn_limit', message: 'limit', conv: turn.conv, ...correlation });
      expect(useChatStore.getState().activeMessageId).toBe(turn.msg_id);
      expect(useChatStore.getState().isStreaming).toBe(true);
    }
    deliver({ type: 'error', code: 'turn_limit', message: 'own limit', request_id: turn.msg_id, conv: turn.conv });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('own limit');
    expect(useChatStore.getState().activeMessageId).toBeNull();
  });

  it('does not finish the turn on cancel acknowledgement or a cleanup notice', () => {
    const turn = send('first');
    deliver({ type: 'cancel_accepted', request_id: 'cancel-1' });
    deliver({ type: 'notice', code: 'delivery_cleanup_failed', msg_id: turn.msg_id, conv: turn.conv });
    expect(useChatStore.getState().activeMessageId).toBe(turn.msg_id);
    deliver({ type: 'assistant_done', content: 'real reply', tokens_used: 1, conv: turn.conv, msg_id: turn.msg_id });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('real reply');
    deliver({ type: 'notice', code: 'delivery_cleanup_failed', msg_id: turn.msg_id, conv: turn.conv });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('real reply');
  });

  it('still accepts legacy untagged errors', () => {
    send('first');
    deliver({ type: 'error', message: 'legacy connection error' });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('legacy connection error');
    expect(useChatStore.getState().isStreaming).toBe(false);
  });

  it('uses session ids when a frame carries no conversation or message id', () => {
    const current = {
      convId: 'c1', activeMessageId: 'm1', sessionId: 'webchat:own',
      selectedAgentId: 'sales', resumedExplicitly: false,
    };
    expect(shouldRenderChatFrame({ type: 'error', session_id: 'webchat:other' }, current)).toBe(false);
    expect(shouldRenderChatFrame({ type: 'progress', session_id: 'webchat:own#agent:sales#conv:c1' }, current)).toBe(true);
    expect(shouldRenderChatFrame({ type: 'progress', session_id: 'webchat:own#agent:sales#conv:c2' }, current)).toBe(false);
    expect(shouldRenderChatFrame({ type: 'error', session_id: 'webchat:past' }, { ...current, sessionId: 'webchat:past', resumedExplicitly: true })).toBe(true);
    // An @ call can change the routed session while keeping its client ids.
    expect(shouldRenderChatFrame({ type: 'assistant_done', session_id: 'webchat:rerouted', conv: 'c1', msg_id: 'm1' }, current)).toBe(true);
  });
});
