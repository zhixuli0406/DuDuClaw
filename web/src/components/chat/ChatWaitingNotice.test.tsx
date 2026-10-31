import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, screen, render } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import { renderWithProviders } from '@/test/render';
import { api, type ChatSessionHistory } from '@/lib/api';
import { useChatStore, historyToMessages } from '@/stores/chat-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { useAuthStore } from '@/stores/auth-store';
import { ChatWaitingNotice } from './ChatWaitingNotice';

vi.mock('@/lib/knowledge-citations', async importOriginal => ({
  ...await importOriginal<typeof import('@/lib/knowledge-citations')>(),
  fetchSessionCitations: vi.fn().mockResolvedValue([]),
}));

class FakeSocket {
  static OPEN = 1;
  static last: FakeSocket;
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() { FakeSocket.last = this; }
  send() {}
  close() {}
}
const user = { id: 1, role: 'user', content: 'question', timestamp: '2026-11-01T00:00:00Z', tokens: 0 };
const answer = { id: 2, role: 'assistant', content: 'answer', timestamp: '2026-11-01T00:01:00Z', tokens: 4 };
const history = (replied = false): ChatSessionHistory => ({
  session_id: 'webchat:past', agent_id: 'dudu', channel: 'webchat', privacy_mode: 'own',
  messages: replied ? [user, answer] : [user],
});
const deliver = (frame: Record<string, unknown>) => act(() => {
  FakeSocket.last.onmessage?.({ data: JSON.stringify(frame) });
});
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const resume = () => useChatStore.getState().resumeSession('webchat:past', historyToMessages([user]));

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(Date.parse(user.timestamp));
  vi.stubGlobal('WebSocket', FakeSocket);
  useChatStore.getState().disconnect();
  useChatStore.getState().reset();
  useChatStore.setState({ sessionId: 'webchat:own', ownSessionId: 'webchat:own', historyReplyRevision: 0 });
  useConversationsStore.setState({ viewOnly: false });
  useChatStore.getState().connect();
  vi.spyOn(api.chatSessions, 'history').mockResolvedValue(history());
});
afterEach(() => {
  cleanup();
  useChatStore.getState().disconnect();
  useAuthStore.setState({ jwt: null });
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('reopened unanswered conversation', () => {
  it('renders the requested zh-TW notice and expiry text', async () => {
    resume();
    render(<IntlProvider locale="zh-TW" messages={zhTW}><ChatWaitingNotice /></IntlProvider>);
    expect(screen.getByRole('status')).toHaveTextContent('這則訊息還沒有回覆。AI 員工可能還在處理，回覆到了會自動顯示。');
    await advance(30 * 60_000);
    expect(screen.getByRole('status')).toHaveTextContent('一直沒有收到回覆。可以重新傳送，或請管理員查看閘道紀錄。');
  });

  it('ignores a stale snapshot that does not include the unanswered user row', async () => {
    resume();
    const before = useChatStore.getState().messages;
    vi.mocked(api.chatSessions.history).mockResolvedValue({ ...history(), messages: [answer] });
    renderWithProviders(<ChatWaitingNotice />);
    await advance(5_000);
    expect(useChatStore.getState().messages).toBe(before);
    expect(screen.getByRole('status')).toHaveTextContent('This message has no reply yet');
  });

  it('shows a notice, loads a reply and stops polling', async () => {
    resume();
    renderWithProviders(<ChatWaitingNotice />);
    expect(screen.getByRole('status')).toHaveTextContent('This message has no reply yet');
    vi.mocked(api.chatSessions.history).mockResolvedValue(history(true));
    await advance(5_000);
    expect(useChatStore.getState().messages.map(m => m.content)).toEqual(['question', 'answer']);
    expect(screen.queryByRole('status')).toBeNull();
    await advance(60_000);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(1);
  });

  it('uses 5 seconds for 2 minutes, then 30 seconds, and expires at 30 minutes', async () => {
    resume();
    renderWithProviders(<ChatWaitingNotice />);
    await advance(120_000);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(24);
    await advance(29_999);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(24);
    await advance(1);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(25);
    await advance(30 * 60_000 - 150_000);
    expect(screen.getByRole('status')).toHaveTextContent('No reply has arrived');
    const calls = vi.mocked(api.chatSessions.history).mock.calls.length;
    await advance(120_000);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(calls);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('refreshes immediately for the same session with an old conv, even after expiry', async () => {
    resume();
    renderWithProviders(<ChatWaitingNotice />);
    await advance(30 * 60_000);
    vi.mocked(api.chatSessions.history).mockResolvedValue(history(true));
    const calls = vi.mocked(api.chatSessions.history).mock.calls.length;
    deliver({ type: 'assistant_done', conv: 'old', msg_id: 'old', session_id: 'webchat:other', content: 'other' });
    expect(api.chatSessions.history).toHaveBeenCalledTimes(calls);
    deliver({ type: 'assistant_done', conv: 'old', msg_id: 'old', session_id: 'webchat:past', content: 'wire answer' });
    await advance(0);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(calls + 1);
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('answer');
  });

  it.each(['leave', 'unmount', 'send'] as const)('cleans up on %s and fences an in-flight read', async mode => {
    let resolve!: (value: ChatSessionHistory) => void;
    vi.mocked(api.chatSessions.history).mockReturnValue(new Promise(done => { resolve = done; }));
    resume();
    const view = renderWithProviders(<ChatWaitingNotice />);
    await advance(5_000);
    act(() => {
      if (mode === 'leave') useChatStore.getState().reset();
      if (mode === 'send') useChatStore.getState().send('new question');
      if (mode === 'unmount') view.unmount();
    });
    const before = useChatStore.getState().messages;
    await act(async () => { resolve(history(true)); });
    expect(useChatStore.getState().messages).toBe(before);
    await advance(30_000);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(1);
    if (mode !== 'send') expect(vi.getTimerCount()).toBe(0);
  });

  it('does not overwrite even a newer turn that has already finished', async () => {
    let resolve!: (value: ChatSessionHistory) => void;
    vi.mocked(api.chatSessions.history).mockReturnValue(new Promise(done => { resolve = done; }));
    resume();
    renderWithProviders(<ChatWaitingNotice />);
    await advance(5_000);
    act(() => useChatStore.getState().send('new question'));
    const { activeMessageId, convId } = useChatStore.getState();
    deliver({ type: 'assistant_done', conv: convId, msg_id: activeMessageId, content: 'new answer' });
    await act(async () => { resolve(history(true)); });
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('new answer');
  });

  it('never overlaps reads, ignores stale history and bounds a hanging request', async () => {
    let resolve!: (value: ChatSessionHistory) => void;
    vi.mocked(api.chatSessions.history).mockReturnValue(new Promise(done => { resolve = done; }));
    resume();
    renderWithProviders(<ChatWaitingNotice />);
    const before = useChatStore.getState().messages;
    await advance(30 * 60_000);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(1);
    await act(async () => { resolve(history(true)); });
    expect(useChatStore.getState().messages).toBe(before);
    expect(api.chatSessions.history).toHaveBeenCalledTimes(1);
  });
});

describe('streaming silence', () => {
  it('counts minutes, ignores keepalives and other turns, and clears on real activity', async () => {
    useChatStore.getState().send('question');
    const { convId, activeMessageId } = useChatStore.getState();
    renderWithProviders(<ChatWaitingNotice />);
    await advance(59_999);
    expect(screen.queryByRole('status')).toBeNull();
    deliver({ type: 'progress', kind: 'keepalive', conv: convId, msg_id: activeMessageId });
    await advance(1);
    expect(screen.getByRole('status')).toHaveTextContent('Waiting for 1 min');
    deliver({ type: 'thinking', conv: 'other', msg_id: 'other' });
    await advance(60_000);
    expect(screen.getByRole('status')).toHaveTextContent('Waiting for 2 min');
    deliver({ type: 'step', phase: 'start', tool: 'Read', conv: convId, msg_id: activeMessageId });
    expect(screen.queryByRole('status')).toBeNull();
    await advance(60_000);
    expect(screen.getByRole('status')).toHaveTextContent('Waiting for 1 min');
    deliver({ type: 'error', code: 'identity_unavailable', conv: convId, msg_id: activeMessageId, message: 'unavailable' });
    expect(useChatStore.getState().isStreaming).toBe(false);
    expect(screen.queryByRole('status')).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('keeps counting across disconnect and reconnect when the turn is gone', async () => {
    useAuthStore.setState({ jwt: 'test-token' });
    useChatStore.getState().send('question');
    renderWithProviders(<ChatWaitingNotice />);
    await advance(30_000);
    act(() => FakeSocket.last.onclose?.());
    await advance(10_000);
    act(() => FakeSocket.last.onopen?.());
    deliver({ type: 'session_info', session_id: 'webchat:reconnected', agent_name: 'DuDu' });
    await advance(20_000);
    expect(useChatStore.getState().isStreaming).toBe(true);
    expect(screen.getByRole('status')).toHaveTextContent('Waiting for 1 min');
  });
});
