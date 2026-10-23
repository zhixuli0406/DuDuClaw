import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseComputerWatch, useChatStore } from './chat-store';

class Socket {
  static OPEN = 1;
  static last: Socket;
  readyState = 1;
  onmessage: ((e: { data: string }) => void) | null = null;
  constructor() { Socket.last = this; }
  send() {}
  close() {}
}
const href = '/computer/alice?l=eyJj.Iabc';
const deliver = (frame: Record<string, unknown>) => Socket.last.onmessage?.({ data: JSON.stringify(frame) });
describe('host browser watch frame', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal('WebSocket', Socket);
    useChatStore.getState().disconnect();
    useChatStore.getState().connect();
    useChatStore.setState({ messages: [], isStreaming: false });
    useChatStore.getState().send('open browser');
  });
  afterEach(() => {
    useChatStore.getState().disconnect();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });
  it('preserves a structured button through the final assistant reply', () => {
    const conv = useChatStore.getState().convId;
    deliver({ type:'computer_watch', agent_id:'alice', href, conv });
    expect(useChatStore.getState().messages.at(-1)?.computerWatch).toEqual({ agentId:'alice', href });
    deliver({ type:'assistant_done', content:'Opened', tokens_used:0, conv });
    expect(useChatStore.getState().messages.at(-1)).toMatchObject({ content:'Opened', computerWatch:{ href } });
  });
  it('drops other conversations, missing attribution and frames after completion', () => {
    deliver({ type:'computer_watch', agent_id:'alice', href, conv:'other' });
    deliver({ type:'computer_watch', agent_id:'alice', href });
    const conv = useChatStore.getState().convId;
    deliver({ type:'assistant_done', content:'done', tokens_used:0, conv });
    deliver({ type:'computer_watch', agent_id:'alice', href, conv });
    expect(useChatStore.getState().messages.every(m => !m.computerWatch)).toBe(true);
  });
  it.each([
    { agent_id:'alice', href:'https://evil.example/computer/alice?l=a.b' },
    { agent_id:'alice', href:'/computer/bob?l=a.b' },
    { agent_id:'alice', href:'/computer/alice' },
    { agent_id:'alice', href:'/computer/alice?l=a.b&jwt=secret' },
    { agent_id:'../alice', href },
  ])('refuses an unsafe or mismatched link %j', raw => {
    expect(parseComputerWatch(raw)).toBeUndefined();
    deliver({ type:'computer_watch', ...raw, conv:useChatStore.getState().convId });
    expect(useChatStore.getState().messages.every(m => !m.computerWatch)).toBe(true);
  });
});
