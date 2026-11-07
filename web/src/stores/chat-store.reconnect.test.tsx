import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, screen, fireEvent } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WebChatPage } from '@/pages/WebChatPage';
import { api, type ChatSessionHistory } from '@/lib/api';
import { toast } from '@/lib/toast';
import { useChatStore } from './chat-store';
import { callableSubject, useCallableAgentsStore } from './callable-agents-store';
import { useAuthStore } from './auth-store';
import { useAgentsStore } from './agents-store';
import { useConversationsStore } from './conversations-store';

class Socket {
  static OPEN = 1;
  static CONNECTING = 0;
  static instances: Socket[] = [];
  readyState = 0;
  sent: Record<string, unknown>[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() { Socket.instances.push(this); }
  send(value: string) { this.sent.push(JSON.parse(value)); }
  close() { this.readyState = 3; this.onclose?.(); }
  open() { this.readyState = 1; this.onopen?.(); }
  receive(value: Record<string, unknown>) { this.onmessage?.({ data: JSON.stringify(value) }); }
}
const current = () => Socket.instances.at(-1)!;
const visible = () => document.dispatchEvent(new Event('visibilitychange'));
const hello = (socket: Socket, id: string) => socket.receive({ type: 'session_info', session_id: id, agent_name: 'Sales' });
const saved: ChatSessionHistory = { session_id: 'webchat:old#agent:sales#conv:view', agent_id: 'sales', channel: 'webchat', privacy_mode: 'own', messages: [
  { id: 1, role: 'user', content: 'question', timestamp: '2026-11-05T00:00:00Z', tokens: 0 },
  { id: 2, role: 'assistant', content: 'Saved answer from the background turn', timestamp: '2026-11-05T00:02:00Z', tokens: 20 },
] };

// A turn becomes recoverable only after its user row is durably stored.
function sendAccepted(...args: Parameters<ReturnType<typeof useChatStore.getState>['send']>) {
  useChatStore.getState().send(...args);
  const request = current().sent.at(-1)!;
  current().receive({ type: 'turn_accepted', session_id: saved.session_id, conv: request.conv,
    msg_id: request.msg_id, user_row_id: 1 });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('WebSocket', Socket);
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
  useChatStore.getState().disconnect();
  useAuthStore.setState({ jwt: 'test-token', user: { id: 'owner', role: 'admin' } as never, bindings: [] });
  useAgentsStore.setState({ loaded: true, agents: [{ name: 'sales', display_name: 'Sales', status: 'active', role: 'specialist' }] as never });
  useConversationsStore.setState({ viewOnly: false });
  useChatStore.setState({ sessionId: null, acceptedSessionId: null, ownSessionId: null, selectedAgentId: 'sales', convId: 'view', messages: [], resumedExplicitly: false,
    isStreaming: false, activeMessageId: null, thinking: null, stepTree: [], steps: [] });
  Socket.instances = [];
  vi.spyOn(api.chatSessions, 'history').mockResolvedValue(saved);
  useChatStore.getState().connect();
  current().open();
  hello(current(), 'webchat:old');
  // Explicit chat routes require a confirmed session-scoped grant before sending.
  useCallableAgentsStore.setState({ entries: Object.fromEntries(
    ['webchat:old', 'webchat:new', saved.session_id].map(session => [JSON.stringify([callableSubject(), session]), {
      agents: [{ name: 'sales', display_name: 'Sales', call_text: '@sales', via: 'binding' as const, level: 'operator' as const, selectable: true, assignable: true }], unrestricted: false, loading: false, confirmedAt: Date.now(),
    }]),
  ) });
});
afterEach(() => {
  cleanup();
  useChatStore.getState().disconnect();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it('replaces a stale OPEN transport on foreground, subscribes to the same conversation and renders its saved answer', async () => {
  sendAccepted('question');
  renderWithProviders(<WebChatPage />);
  const old = current();
  act(visible);
  const next = current();
  expect(next).not.toBe(old);
  expect(old.readyState).toBe(3);
  expect(useChatStore.getState().sessionId).toBe(saved.session_id);
  act(() => { next.open(); hello(next, 'webchat:new'); });
  expect(next.sent.at(-1)).toEqual({ type: 'subscribe', session_id: saved.session_id, conv: 'view' });
  await act(async () => { next.receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' }); });
  expect(api.chatSessions.history).toHaveBeenCalledWith(saved.session_id);
  expect(screen.getByText('Saved answer from the background turn')).toBeInTheDocument();
  expect(useChatStore.getState().isStreaming).toBe(false);
  expect(useChatStore.getState().thinking).toBeNull();
  expect(useChatStore.getState().activeMessageId).toBeNull();
  old.receive({ type: 'assistant_done', content: 'STALE PRIVATE REPLY' });
  expect(screen.queryByText('STALE PRIVATE REPLY')).not.toBeInTheDocument();
});

it('skips backoff on foreground, avoids duplicate connecting sockets, and stays disconnected after logout', () => {
  current().close();
  visible();
  expect(Socket.instances).toHaveLength(2);
  useChatStore.getState().connect();
  expect(Socket.instances).toHaveLength(2);
  vi.advanceTimersByTime(30_000);
  expect(Socket.instances).toHaveLength(2);
  useChatStore.getState().disconnect();
  visible();
  expect(Socket.instances).toHaveLength(2);
});

it.each(['account', 'conversation', 'new message'])('does not apply delayed history after a %s change', async mode => {
  let finish!: (value: ChatSessionHistory) => void;
  vi.mocked(api.chatSessions.history).mockReturnValue(new Promise(resolve => { finish = resolve; }));
  sendAccepted('question');
  visible();
  current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  if (mode === 'account') useAuthStore.setState({ user: { id: 'other', role: 'admin' } as never });
  if (mode === 'conversation') useChatStore.getState().reset();
  if (mode === 'new message') sendAccepted('new question');
  const messages = useChatStore.getState().messages;
  finish(saved);
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages).toBe(messages);
});

it('uses the existing unanswered-history poll after reconnect while a turn is still running', async () => {
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: saved.messages.slice(0, 1) });
  sendAccepted('question');
  renderWithProviders(<WebChatPage />);
  act(visible);
  act(() => { current().open(); hello(current(), 'webchat:new'); });
  await act(async () => { current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' }); });
  expect(screen.getByText(/This message has no reply yet/)).toBeInTheDocument();
  vi.mocked(api.chatSessions.history).mockResolvedValue(saved);
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  expect(screen.getByText('Saved answer from the background turn')).toBeInTheDocument();
  expect(screen.queryByText(/This message has no reply yet/)).not.toBeInTheDocument();
});

it('recovers an attachment reply when stored user text includes backend file references', async () => {
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: [
    { ...saved.messages[0], content: 'question\n\n[Attachment: /agent/attachments/report.txt]' }, saved.messages[1],
  ] });
  sendAccepted('question', [{ name: 'report.txt', mime: 'text/plain', dataBase64: 'YQ==' }]);
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages.at(-1)?.content).toBe(saved.messages[1].content);
  expect(useChatStore.getState().isStreaming).toBe(false);
});

it('keeps the transcript and accepted turn after a subscription refusal', () => {
  sendAccepted('question');
  visible(); current().open(); hello(current(), 'webchat:new');
  const before = useChatStore.getState();
  current().receive({ type: 'error', code: 'subscription_unavailable', message: 'conversation not found', conv: 'view' });
  expect(useChatStore.getState()).toBe(before);
});

it('refreshes a subscribed conversation when another tab completes a turn with an unknown message id', async () => {
  sendAccepted('question');
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: [...saved.messages,
    { id: 3, role: 'user', content: 'Other tab question', timestamp: '2026-11-05T00:03:00Z', tokens: 0 },
    { id: 4, role: 'assistant', content: 'Other tab answer', timestamp: '2026-11-05T00:04:00Z', tokens: 4 },
  ] });
  current().receive({ type: 'assistant_done', session_id: saved.session_id, conv: 'view', msg_id: 'other-tab', content: 'Other tab answer' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages.at(-1)?.content).toBe('Other tab answer');
});

it.each(['turn_accepted', 'progress', 'assistant_done'])('uses the canonical @ routed session from %s when reconnecting', type => {
  useAuthStore.setState({ user: { id: 'owner', role: 'employee' } as never });
  current().receive({ type: 'session_info', session_id: 'webchat:old', agent_name: 'Sales',
    resolved_agent: { id: 'sales', name: 'Sales', icon: '' }, assignment_error: null });
  sendAccepted('@support question');
  const request = current().sent.at(-1)!;
  expect(request.agent).toBeUndefined();
  const actual = 'webchat:old#agent:support#conv:view';
  current().receive({ type, session_id: actual, conv: request.conv, msg_id: request.msg_id, content: 'Support answer', tokens_used: 1 });
  expect(useChatStore.getState().acceptedSessionId).toBe(actual);
  visible(); current().open(); hello(current(), 'webchat:new');
  expect(current().sent.at(-1)).toEqual({ type: 'subscribe', session_id: actual, conv: 'view' });
});

it('does not let an earlier routed turn change the newest conversation resume target', () => {
  sendAccepted('@support first');
  const first = current().sent.at(-1)!;
  current().receive({ type: 'turn_accepted', conv: first.conv, msg_id: first.msg_id, session_id: 'webchat:old#agent:support#conv:view' });
  sendAccepted('second');
  const second = current().sent.at(-1)!;
  current().receive({ type: 'turn_accepted', conv: second.conv, msg_id: second.msg_id, session_id: saved.session_id });
  current().receive({ type: 'progress', conv: first.conv, msg_id: first.msg_id, session_id: 'webchat:old#agent:support#conv:view', content: 'older work' });
  expect(useChatStore.getState().acceptedSessionId).toBe(saved.session_id);
});

it('retries a transient history RPC failure after foreground reconnect and shows the saved reply', async () => {
  vi.mocked(api.chatSessions.history).mockRejectedValueOnce(new Error('RPC handshake not ready')).mockResolvedValue(saved);
  sendAccepted('question');
  renderWithProviders(<WebChatPage />);
  act(visible);
  act(() => { current().open(); hello(current(), 'webchat:new'); });
  await act(async () => { current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' }); });
  expect(useChatStore.getState().isStreaming).toBe(true);
  await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
  expect(api.chatSessions.history).toHaveBeenCalledTimes(2);
  expect(screen.getByText('Saved answer from the background turn')).toBeInTheDocument();
  expect(useChatStore.getState().isStreaming).toBe(false);
});

it('does not retry history after the viewer switches conversations during the retry delay', async () => {
  vi.mocked(api.chatSessions.history).mockRejectedValue(new Error('offline'));
  sendAccepted('question');
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  useChatStore.getState().reset();
  await vi.advanceTimersByTimeAsync(5_000);
  expect(api.chatSessions.history).toHaveBeenCalledTimes(1);
});

it('bounds reconnect retries and clears stale thinking while retaining the unanswered message', async () => {
  vi.mocked(api.chatSessions.history).mockRejectedValue(new Error('RPC unavailable'));
  sendAccepted('question');
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await vi.advanceTimersByTimeAsync(10_000);
  expect(api.chatSessions.history).toHaveBeenCalledTimes(3);
  expect(useChatStore.getState().messages.at(-1)?.content).toBe('question');
  expect(useChatStore.getState().thinking).toBeNull();
  expect(useChatStore.getState().isStreaming).toBe(false);
});

it.each([false, true])('loads the saved reply through unanswered polling after reconnect retries expire (attachments: %s)', async attachments => {
  vi.mocked(api.chatSessions.history)
    .mockRejectedValueOnce(new Error('RPC unavailable'))
    .mockRejectedValueOnce(new Error('RPC unavailable'))
    .mockRejectedValueOnce(new Error('RPC unavailable'))
    .mockResolvedValue({ ...saved, messages: [
      { ...saved.messages[0], content: attachments ? 'question\n\n[Attachment: /agent/attachments/report.txt]' : 'question' },
      saved.messages[1],
    ] });
  // Browser timestamp intentionally differs from the server's stored timestamp.
  vi.setSystemTime(new Date('2026-11-05T00:00:02Z'));
  sendAccepted('question', attachments
    ? [{ name: 'report.txt', mime: 'text/plain', dataBase64: 'YQ==' }] : undefined);
  renderWithProviders(<WebChatPage />);
  act(visible);
  act(() => { current().open(); hello(current(), 'webchat:new'); });
  await act(async () => { current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' }); });
  await act(async () => { await vi.advanceTimersByTimeAsync(3_000); });
  expect(api.chatSessions.history).toHaveBeenCalledTimes(3);
  expect(useChatStore.getState().isStreaming).toBe(false);
  expect(useChatStore.getState().messages.at(-1)?.historyId).toBe(1);
  expect(screen.getByText(/This message has no reply yet/)).toBeInTheDocument();
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
  expect(api.chatSessions.history).toHaveBeenCalledTimes(4);
  expect(screen.getByText('Saved answer from the background turn')).toBeInTheDocument();
  expect(screen.queryByText(/This message has no reply yet/)).not.toBeInTheDocument();
});

it('bounds recovery after a correlated subscription refusal without erasing the accepted message', async () => {
  vi.mocked(api.chatSessions.history).mockRejectedValue(new Error('Conversation missing after restart'));
  sendAccepted('question');
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'error', code: 'subscription_unavailable', session_id: saved.session_id, conv: 'view' });
  await vi.advanceTimersByTimeAsync(3_000);
  expect(api.chatSessions.history).toHaveBeenCalledTimes(3);
  expect(useChatStore.getState().messages.at(-1)?.content).toBe('question');
  expect(useChatStore.getState().thinking).toBeNull();
  expect(useChatStore.getState().isStreaming).toBe(false);
});

it.each(['logout', 'account switch'])('sends explicit logout before closing on %s', mode => {
  const old = current();
  sendAccepted('question');
  if (mode === 'logout') useAuthStore.getState().logout();
  else useAuthStore.setState({ user: { id: 'new-owner', role: 'admin' } as never });
  expect(old.sent.at(-1)).toEqual({ type: 'logout' });
  expect(old.readyState).toBe(3);
  expect(useChatStore.getState().messages).toEqual([]);
  expect(useChatStore.getState().connectionState).toBe('disconnected');
  visible();
  expect(current()).toBe(old);
});

it('does not revoke accepted turns during foreground reconnect or a normal disconnect', () => {
  const old = current();
  sendAccepted('question');
  visible();
  expect(old.sent.some(frame => frame.type === 'logout')).toBe(false);
  const next = current(); next.open(); hello(next, 'webchat:new');
  useChatStore.getState().disconnect();
  expect(next.sent.some(frame => frame.type === 'logout')).toBe(false);
});

it.each(['', 'question'])('never recovers an unacknowledged attachment turn from old text (%s)', async content => {
  useChatStore.getState().send(content, [{ name: 'new.txt', mime: 'text/plain', dataBase64: 'YQ==' }]);
  const local = useChatStore.getState().messages;
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages).toBe(local);
  expect(useChatStore.getState().isStreaming).toBe(false);
  await useConversationsStore.getState().refreshUnanswered(() => true);
  expect(useChatStore.getState().messages).toBe(local);
});

it.each(['text', 'attachment', 'attachment only'])('recovers a persisted %s turn when turn_accepted was lost', async kind => {
  const content = kind === 'attachment only' ? '' : 'question';
  useChatStore.getState().send(content, kind === 'text' ? undefined
    : [{ name: 'new.txt', mime: 'text/plain', dataBase64: 'YQ==' }]);
  const request = current().sent.at(-1)!;
  expect(useChatStore.getState().messages.at(-1)?.historyId).toBeUndefined();
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: [
    { ...saved.messages[0], client_msg_id: 'older-turn', content },
    { ...saved.messages[1], content: 'Old answer to identical text' },
    { ...saved.messages[0], id: 3, client_msg_id: request.msg_id as string,
      content: kind === 'text' ? content : `${content}\n\n[Attachment: /agent/attachments/new.txt]` },
    { ...saved.messages[1], id: 4, content: 'Correct answer after lost acknowledgement' },
  ] });
  visible(); current().open(); hello(current(), 'webchat:new');
  expect(current().sent.at(-1)).toEqual({ type: 'subscribe', session_id: saved.session_id, conv: 'view' });
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages.at(-1)?.content).toBe('Correct answer after lost acknowledgement');
  expect(useChatStore.getState().messages.at(-2)).toMatchObject({ historyId: 3, turnId: request.msg_id });
  expect(useChatStore.getState().isStreaming).toBe(false);
});

it('recovers a turn without turn_accepted through the unanswered poll after reconnect', async () => {
  useChatStore.getState().send('question');
  const request = current().sent.at(-1)!;
  const local = useChatStore.getState().messages;
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages).toBe(local);
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: [
    { ...saved.messages[0], client_msg_id: request.msg_id as string }, saved.messages[1],
  ] });
  await useConversationsStore.getState().refreshUnanswered(() => true);
  expect(useChatStore.getState().messages.at(-1)?.content).toBe(saved.messages[1].content);
});

it.each(['session', 'message id', 'role'])('rejects lost-ack history with the wrong %s', async mismatch => {
  useChatStore.getState().send('question');
  const request = current().sent.at(-1)!;
  const local = useChatStore.getState().messages;
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved,
    session_id: mismatch === 'session' ? 'webchat:another-session' : saved.session_id,
    messages: [
      { ...saved.messages[0], role: mismatch === 'role' ? 'assistant' : 'user',
        client_msg_id: mismatch === 'message id' ? 'another-turn' : request.msg_id as string }, saved.messages[1],
    ],
  });
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages).toBe(local);
  useChatStore.setState({ isStreaming: false, activeMessageId: null });
  await useConversationsStore.getState().refreshUnanswered(() => true);
  expect(useChatStore.getState().messages).toBe(local);
});

it('rejects ambiguous client ids during reconnect and unanswered polling until an exact row is acknowledged', async () => {
  useChatStore.getState().send('question');
  const request = current().sent.at(-1)!;
  const local = useChatStore.getState().messages;
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: [
    { ...saved.messages[0], client_msg_id: request.msg_id as string }, saved.messages[1],
    { ...saved.messages[0], id: 3, client_msg_id: request.msg_id as string },
    { ...saved.messages[1], id: 4, content: 'Reply to the acknowledged row' },
  ] });
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages).toBe(local);
  expect(useChatStore.getState().isStreaming).toBe(false);
  await useConversationsStore.getState().refreshUnanswered(() => true);
  expect(useChatStore.getState().messages).toBe(local);
  current().receive({ type: 'turn_accepted', session_id: saved.session_id,
    conv: 'view', msg_id: request.msg_id, user_row_id: 3 });
  await useConversationsStore.getState().refreshUnanswered(() => true);
  expect(useChatStore.getState().messages.at(-1)?.content).toBe('Reply to the acknowledged row');
});

it('requires the exact acknowledged row even when an old row has identical content', async () => {
  sendAccepted('question');
  const request = current().sent.at(-1)!;
  current().receive({ type: 'turn_accepted', session_id: saved.session_id, conv: 'view', msg_id: request.msg_id, user_row_id: 99 });
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: [
    { ...saved.messages[0], client_msg_id: request.msg_id as string }, saved.messages[1],
  ] });
  const local = useChatStore.getState().messages;
  visible(); current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  await useConversationsStore.getState().refreshUnanswered(() => true);
  expect(useChatStore.getState().messages).toBe(local);
  vi.mocked(api.chatSessions.history).mockResolvedValue({ ...saved, messages: [
    ...saved.messages, { ...saved.messages[0], id: 99, content: 'question\n\n[Attachment: new.txt]' },
    { ...saved.messages[1], id: 100, content: 'Correct new answer' },
  ] });
  await useConversationsStore.getState().refreshUnanswered(() => true);
  expect(useChatStore.getState().messages.at(-1)?.content).toBe('Correct new answer');
});

it.each([['close', true], ['visible', true], ['close', false], ['visible', false]] as const)('continues the original conversation after %s (acknowledged: %s)', (mode, acknowledged) => {
  if (acknowledged) sendAccepted('question');
  else useChatStore.getState().send('question');
  const original = current().sent.at(-1)!;
  if (mode === 'close') { current().close(); vi.advanceTimersByTime(1_000); }
  else visible();
  current().open(); hello(current(), 'webchat:new');
  useChatStore.getState().send('follow up');
  expect(current().sent.at(-1)).toMatchObject({ type: 'user_message', session_id: saved.session_id, conv: original.conv });
  expect(useChatStore.getState().ownSessionId).toBe('webchat:new');
});

it('adopts the new connection session for an empty page', () => {
  current().close(); vi.advanceTimersByTime(1_000);
  current().open(); hello(current(), 'webchat:new');
  useChatStore.getState().send('first question');
  expect(current().sent.at(-1)).toMatchObject({ session_id: 'webchat:new' });
  expect(current().sent.some(frame => frame.type === 'subscribe')).toBe(false);
});

it('refuses an offline send visibly without hiding an earlier accepted reply', async () => {
  sendAccepted('question');
  current().close();
  const notice = vi.spyOn(toast, 'error');
  expect(useChatStore.getState().send('keep this draft')).toBe(false);
  expect(notice).toHaveBeenCalledWith(expect.any(String));
  vi.advanceTimersByTime(1_000);
  current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages.at(-1)?.content).toBe(saved.messages[1].content);
});

it('keeps the composer draft when the transport closes before the close callback runs', () => {
  renderWithProviders(<WebChatPage />);
  const input = screen.getByRole('combobox');
  fireEvent.change(input, { target: { value: 'keep this draft' } });
  current().readyState = 3;
  fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  expect(input).toHaveValue('keep this draft');
  expect(useChatStore.getState().messages).toEqual([]);
});

it('bounds an interrupted turn even when no replacement socket opens, and accepts its late reply', () => {
  sendAccepted('question');
  const request = current().sent.at(-1)!;
  current().close();
  vi.advanceTimersByTime(10_000);
  expect(useChatStore.getState().isStreaming).toBe(false);
  expect(useChatStore.getState().activeMessageId).toBeNull();
  expect(useChatStore.getState().messages[0]).toMatchObject({ delivery: 'failed', deliveryError: expect.any(String) });
  current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'assistant_done', session_id: saved.session_id, conv: request.conv,
    msg_id: request.msg_id, content: 'late answer', tokens_used: 2 });
  expect(useChatStore.getState().messages[0]).toMatchObject({ delivery: 'done' });
  expect(useChatStore.getState().messages[0].deliveryError).toBeUndefined();
  expect(useChatStore.getState().messages.at(-1)?.content).toBe('late answer');
});

it('preserves a turn that resumes delivering before the disconnect deadline', () => {
  sendAccepted('question');
  const request = current().sent.at(-1)!;
  current().close(); vi.advanceTimersByTime(1_000);
  current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'assistant_chunk', session_id: saved.session_id, conv: request.conv,
    msg_id: request.msg_id, content: 'working' });
  vi.advanceTimersByTime(10_000);
  expect(useChatStore.getState().isStreaming).toBe(true);
  current().receive({ type: 'assistant_done', session_id: saved.session_id, conv: request.conv,
    msg_id: request.msg_id, content: 'finished', tokens_used: 2 });
  expect(useChatStore.getState().messages[0].delivery).toBe('done');
});

function exhaustReconnects() {
  current().close();
  for (let attempt = 0; attempt < 10; attempt += 1) {
    vi.advanceTimersByTime(30_000);
    current().close();
  }
}

it.each(['online', 'pageshow', 'visible'])('restarts an exhausted retry budget on %s and removes listeners on disconnect', event => {
  exhaustReconnects();
  const count = Socket.instances.length;
  vi.advanceTimersByTime(60_000);
  expect(Socket.instances).toHaveLength(count);
  const wake = () => event === 'visible' ? visible() : window.dispatchEvent(new Event(event));
  wake();
  expect(Socket.instances).toHaveLength(count + 1);
  current().close(); vi.advanceTimersByTime(1_000);
  expect(Socket.instances).toHaveLength(count + 2);
  useChatStore.getState().disconnect();
  wake(); vi.advanceTimersByTime(60_000);
  expect(Socket.instances).toHaveLength(count + 2);
});


it('restarts slow history recovery when the interruption deadline changes the transcript', async () => {
  const finish: ((value: ChatSessionHistory) => void)[] = [];
  vi.mocked(api.chatSessions.history).mockImplementation(() => new Promise(resolve => finish.push(resolve)));
  sendAccepted('question');
  const request = current().sent.at(-1)!;
  current().receive({ type: 'assistant_chunk', session_id: saved.session_id, conv: request.conv,
    msg_id: request.msg_id, content: 'partial' });
  current().close(); vi.advanceTimersByTime(1_000);
  current().open(); hello(current(), 'webchat:new');
  current().receive({ type: 'subscribed', session_id: saved.session_id, conv: 'view' });
  expect(finish).toHaveLength(1);
  vi.advanceTimersByTime(9_000);
  expect(finish).toHaveLength(2);
  for (const resolve of finish) resolve(saved);
  await Promise.resolve(); await Promise.resolve();
  expect(useChatStore.getState().messages.at(-1)?.content).toBe(saved.messages[1].content);
  expect(useChatStore.getState().isStreaming).toBe(false);
});

it('shows an interruption notice without offering to duplicate the still-running request', () => {
  sendAccepted('question');
  renderWithProviders(<WebChatPage />);
  act(() => { current().close(); vi.advanceTimersByTime(10_000); });
  expect(screen.getByText(/The connection was interrupted/)).toBeVisible();
  expect(screen.queryByRole('button', { name: /Resend/i })).not.toBeInTheDocument();
});

it('manual reconnect resets the exhausted budget and does not duplicate lifecycle listeners', () => {
  exhaustReconnects();
  useChatStore.getState().reconnect();
  const count = Socket.instances.length;
  current().open(); hello(current(), 'webchat:manual');
  window.dispatchEvent(new Event('online'));
  expect(Socket.instances).toHaveLength(count + 1);
  current().close(); vi.advanceTimersByTime(1_000);
  expect(Socket.instances).toHaveLength(count + 2);
});

it('does not apply an old interruption deadline to a new conversation', () => {
  sendAccepted('question'); current().close();
  useChatStore.getState().reset();
  vi.advanceTimersByTime(1_000);
  current().open(); hello(current(), 'webchat:new');
  useChatStore.getState().send('new conversation');
  vi.advanceTimersByTime(10_000);
  expect(useChatStore.getState().messages).toHaveLength(1);
  expect(useChatStore.getState().messages[0].delivery).toBe('running');
  expect(useChatStore.getState().isStreaming).toBe(true);
});


it('reports a synchronous socket send failure without creating a phantom running turn', () => {
  const notice = vi.spyOn(toast, 'error');
  vi.spyOn(current(), 'send').mockImplementation(() => { throw new Error('closed'); });
  expect(useChatStore.getState().send('keep this draft')).toBe(false);
  expect(notice).toHaveBeenCalledWith(expect.any(String));
  expect(useChatStore.getState().messages).toEqual([]);
  expect(useChatStore.getState().isStreaming).toBe(false);
});


it('RM-122 continues a completed conversation after reconnect hello changes the connection session id', async () => {
  const firstSocket = current();
  expect(useChatStore.getState().sessionId).toBe('webchat:old');
  expect(useChatStore.getState().send('First question')).toBe(true);
  const first = firstSocket.sent.at(-1)!;
  expect(first).toMatchObject({ type: 'user_message', session_id: 'webchat:old', conv: 'view' });
  // The first send's base session, partner and nonce resolve to this server bucket.
  const conversationSessionId = `${first.session_id}#agent:sales#conv:${first.conv}`;
  const turn = { session_id: conversationSessionId, conv: first.conv, msg_id: first.msg_id };
  firstSocket.receive({ type: 'turn_accepted', ...turn, user_row_id: 1 });
  firstSocket.receive({ type: 'assistant_chunk', ...turn, content: 'First answer' });
  firstSocket.receive({ type: 'assistant_done', ...turn, content: 'First answer', tokens_used: 5 });
  expect(useChatStore.getState().isStreaming).toBe(false);
  expect(useChatStore.getState().acceptedSessionId).toBe(conversationSessionId);

  vi.spyOn(api.agents, 'callable').mockResolvedValue({
    agents: [{ name: 'sales', display_name: 'Sales', call_text: '@Sales', via: 'binding', level: 'operator', selectable: true, assignable: true }],
    unrestricted: false, default_agent: null,
  });
  // The page refreshes grants for the conversation; wait for confirmation before sending.
  await act(async () => { renderWithProviders(<WebChatPage />); });
  expect(screen.getByText('First question')).toBeVisible();
  expect(screen.getByText('First answer')).toBeVisible();
  act(() => firstSocket.close());
  act(() => vi.advanceTimersByTime(1_000));
  const next = current();
  expect(next).not.toBe(firstSocket);
  await act(async () => { next.open(); hello(next, 'webchat:new'); });
  expect(useChatStore.getState().ownSessionId).toBe('webchat:new');
  expect(next.sent.at(-1)).toEqual({ type: 'subscribe', session_id: conversationSessionId, conv: first.conv });

  act(() => { expect(useChatStore.getState().send('Second question')).toBe(true); });
  const second = next.sent.at(-1)!;
  expect(second).toMatchObject({ type: 'user_message', session_id: conversationSessionId, conv: first.conv });
  expect(second.session_id).not.toBe('webchat:new');
  expect(screen.getByText('First question')).toBeVisible();
  expect(screen.getByText('First answer')).toBeVisible();
  expect(screen.getByText('Second question')).toBeVisible();
});
