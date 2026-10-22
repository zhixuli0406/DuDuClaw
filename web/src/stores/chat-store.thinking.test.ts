import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { useChatStore } from './chat-store';

/** Minimal socket the store can drive: `send` succeeds, frames are pushed in
 *  through `onmessage`. */
class FakeSocket {
  static OPEN = 1;
  static last: FakeSocket | null = null;
  readyState = 1;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() {
    FakeSocket.last = this;
  }
  send(d: string) {
    this.sent.push(d);
  }
  close() {}
}

function deliver(frame: Record<string, unknown>) {
  FakeSocket.last!.onmessage!({ data: JSON.stringify(frame) });
}

describe('chat-store thinking lifecycle', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    vi.stubGlobal('WebSocket', FakeSocket);
    useChatStore.getState().disconnect();
    useChatStore.getState().connect();
    useChatStore.setState({ messages: [], thinking: null, isStreaming: false });
  });

  afterEach(() => {
    useChatStore.getState().disconnect();
    vi.useRealTimers();
  });

  it('send starts the turn timer before any server frame', () => {
    useChatStore.getState().send('hello');
    const st = useChatStore.getState();
    expect(st.thinking).toEqual({ startedAt: 1_000_000, blocks: [], rawNeedsRedaction: false });
    expect(st.isStreaming).toBe(true);
  });

  it('folds thinking frames of the open conversation and drops other conversations', () => {
    useChatStore.getState().send('hello');
    const conv = useChatStore.getState().convId;
    deliver({ type: 'thinking', phase: 'start', block: 't0', kind: 'summary', conv });
    deliver({ type: 'thinking', phase: 'update', block: 't0', kind: 'summary', text: 'mine', line: 'mine', conv });
    deliver({ type: 'thinking', phase: 'update', block: 't9', kind: 'summary', text: 'other', conv: 'c-elsewhere' });
    const blocks = useChatStore.getState().thinking!.blocks;
    expect(blocks.map((b) => b.id)).toEqual(['t0']);
    expect(blocks[0].text).toBe('mine');
  });

  it('assistant_done attaches the turn (wall time + meta) to the reply and clears the live turn', () => {
    useChatStore.getState().send('hello');
    const conv = useChatStore.getState().convId;
    deliver({ type: 'thinking', phase: 'end', block: 't0', kind: 'omitted', conv });
    vi.setSystemTime(1_008_200);
    deliver({
      type: 'assistant_done',
      content: 'answer',
      tokens_used: 3,
      conv,
      thinking: { tokens: 1240, blocks: 1, kinds: ['omitted'] },
    });
    const st = useChatStore.getState();
    expect(st.thinking).toBeNull();
    const reply = st.messages[st.messages.length - 1];
    expect(reply.role).toBe('assistant');
    expect(reply.thinking).toMatchObject({ startedAt: 1_000_000, endedAt: 1_008_200, turnTokens: 1240 });
    expect(reply.thinking!.blocks[0].kind).toBe('omitted');
  });

  it('a thinking frame after the reply landed is ignored', () => {
    useChatStore.getState().send('hello');
    const conv = useChatStore.getState().convId;
    deliver({ type: 'assistant_done', content: 'answer', tokens_used: 1, conv });
    deliver({ type: 'thinking', phase: 'start', block: 't0', kind: 'summary', conv });
    expect(useChatStore.getState().thinking).toBeNull();
  });

  it('an error clears the live turn', () => {
    useChatStore.getState().send('hello');
    deliver({ type: 'error', message: 'boom' });
    expect(useChatStore.getState().thinking).toBeNull();
    expect(useChatStore.getState().isStreaming).toBe(false);
  });

  it('reset, selectAgent and resumeSession clear the live turn', () => {
    useChatStore.getState().send('hello');
    useChatStore.getState().reset();
    expect(useChatStore.getState().thinking).toBeNull();

    useChatStore.getState().send('again');
    useChatStore.getState().selectAgent('sales-bot');
    expect(useChatStore.getState().thinking).toBeNull();

    useChatStore.getState().send('third');
    useChatStore.getState().resumeSession('webchat:past', []);
    expect(useChatStore.getState().thinking).toBeNull();
  });

  it('the first thinking frame after a send counts as the persistence-confirming frame', () => {
    useChatStore.getState().send('hello');
    const before = useChatStore.getState().sessionsRevision;
    deliver({ type: 'thinking', phase: 'start', block: 't0', kind: 'summary', conv: useChatStore.getState().convId });
    expect(useChatStore.getState().sessionsRevision).toBe(before + 1);
  });
});
