import { CALLABLE_REFRESH_INTERVAL_MS, CALLABLE_TIMEOUT_MS, callableSubject, useCallableAgentsStore } from '@/stores/callable-agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { api, type CallableAgent } from './api';
import { AGENT_TARGET_KEYS, AGENT_GUARD_READ_ONLY_METHODS, assertAgentRequestAllowed, assertAgentHttpRequestAllowed, withAgentRequestAllowed } from './agent-request-guard';
import { uploadSources } from './knowledge-api';
import { callTaskRpc } from './task-rpc';
import { meetingApi } from './meeting/api';
import { causalApi } from './causal-api';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { DuDuClawClient, type WsFrame } from './ws-client';

class Socket {
  static OPEN = 1;
  static instances: Socket[] = [];
  readyState = 1;
  sent: WsFrame[] = [];
  onopen: (() => Promise<void>) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() { Socket.instances.push(this); }
  send(line: string) { this.sent.push(JSON.parse(line) as WsFrame); }
  close() { this.onclose?.(); }
  receive(frame: WsFrame) { this.onmessage?.({ data: JSON.stringify(frame) }); }
}

function response(names: string[] = [], unrestricted = false) {
  return { agents: names.map(name => ({ name, selectable: true })) as CallableAgent[], unrestricted, default_agent: null };
}
function deferredGrants() {
  let resolve!: (value: ReturnType<typeof response>) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<ReturnType<typeof response>>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const settle = () => vi.advanceTimersByTimeAsync(0);
let client: DuDuClawClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('WebSocket', Socket);
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
  useAuthStore.setState({ user: { id: 'viewer', role: 'employee', status: 'active' } as never, bindings: [], jwt: null });
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
  vi.spyOn(api.agents, 'callable').mockResolvedValue(response());
  Socket.instances = [];
  client = new DuDuClawClient();
});
afterEach(() => {
  client.disconnect();
  useAuthStore.setState({ user: null, bindings: [] });
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function handshake() {
  const connected = client.connect('ws://localhost/ws');
  const socket = Socket.instances[Socket.instances.length - 1];
  const opened = socket.onopen?.();
  await Promise.resolve();
  const request = socket.sent.find((frame) => frame.type === 'req' && frame.method === 'connect');
  if (!request || request.type !== 'req') throw new Error('Missing connect request');
  socket.receive({ type: 'res', id: request.id, ok: true, payload: {} });
  await opened;
  await connected;
  return socket;
}

function grants(names: string[] = [], unrestricted = false) {
  useCallableAgentsStore.setState({ entries: {
    [JSON.stringify([callableSubject(), null])]: {
      ...response(names, unrestricted), loading: false, confirmedAt: Date.now(),
    },
  } });
}
function reply(socket: Socket, method: string, params: Record<string, unknown>) {
  const request = socket.sent.at(-1)!;
  expect(request).toMatchObject({ type: 'req', method, params });
  if (request.type !== 'req') throw new Error('Missing request');
  socket.receive({ type: 'res', id: request.id, ok: true, payload: 'sent' });
}
async function expectSent(method: string, params: Record<string, unknown>) {
  const socket = await handshake();
  const result = client.call(method, params);
  await settle();
  reply(socket, method, params);
  await expect(result).resolves.toBe('sent');
}

it('rejects a revoked mutation without sending a frame', async () => {
  const socket = await handshake();
  grants(['alice']);
  const result = client.call('agents.pause', { agent_id: 'alice' });
  // The call is waiting for readiness: revoke before the actual send microtask.
  grants();
  await expect(result).rejects.toThrow('Agent access is unavailable or revoked');
  expect(socket.sent).toHaveLength(1);
});

it.each(['admin', 'personal'] as const)('loads grants on demand for unrestricted %s without a mounted consumer', async edition => {
  useAuthStore.setState({ user: edition === 'admin' ? { id: 'admin', role: 'admin', status: 'active' } as never : null });
  useSystemStore.setState({ status: { edition_profile: edition === 'personal' ? 'personal' : 'enterprise' } as never });
  expect(useCallableAgentsStore.getState().entries).toEqual({});
  const pending = deferredGrants();
  vi.mocked(api.agents.callable).mockReturnValueOnce(pending.promise);
  const socket = await handshake();
  const result = client.call('agents.pause', { agent_id: 'not-in-roster' });
  await settle();
  expect(api.agents.callable).toHaveBeenCalledTimes(1);
  expect(socket.sent).toHaveLength(1);
  pending.resolve(response([], true));
  await settle();
  reply(socket, 'agents.pause', { agent_id: 'not-in-roster' });
  await expect(result).resolves.toBe('sent');
});

it.each([true, false])('joins a background refresh and uses the new allowed=%s grant', async allowed => {
  vi.mocked(api.agents.callable).mockResolvedValueOnce(response(['alice']));
  const key = JSON.stringify([callableSubject(), null]);
  await useCallableAgentsStore.getState().load(key);
  const pending = deferredGrants();
  vi.mocked(api.agents.callable).mockReturnValueOnce(pending.promise);
  const socket = await handshake();
  await vi.advanceTimersByTimeAsync(CALLABLE_REFRESH_INTERVAL_MS);
  expect(useCallableAgentsStore.getState().entries[key].loading).toBe(true);
  const sentBeforeMutation = socket.sent.length; // Includes heartbeat frames during the 60s interval.
  const result = client.call('agents.pause', { agent_id: 'alice' });
  const rejected = allowed ? null : expect(result).rejects.toThrow('revoked');
  await settle();
  expect(api.agents.callable).toHaveBeenCalledTimes(2);
  expect(socket.sent).toHaveLength(sentBeforeMutation);
  pending.resolve(response(allowed ? ['alice'] : []));
  await settle();
  if (allowed) {
    reply(socket, 'agents.pause', { agent_id: 'alice' });
    await expect(result).resolves.toBe('sent');
  } else {
    await rejected;
    expect(socket.sent).toHaveLength(sentBeforeMutation);
  }
});

it('fails closed on callable failure without sending', async () => {
  vi.mocked(api.agents.callable).mockRejectedValueOnce(new Error('offline'));
  const socket = await handshake();
  await expect(client.call('agents.pause', { agent_id: 'alice' })).rejects.toThrow('revoked');
  expect(api.agents.callable).toHaveBeenCalledTimes(1);
  expect(socket.sent).toHaveLength(1);
});

it('fails closed after 15 seconds and ignores a late unrestricted response', async () => {
  const pending = deferredGrants();
  vi.mocked(api.agents.callable).mockReturnValueOnce(pending.promise);
  const socket = await handshake();
  const rejected = expect(client.call('agents.pause', { agent_id: 'alice' })).rejects.toThrow('revoked');
  await settle();
  await vi.advanceTimersByTimeAsync(CALLABLE_TIMEOUT_MS);
  await rejected;
  pending.resolve(response([], true));
  await settle();
  expect(socket.sent).toHaveLength(1);
  expect(Object.values(useCallableAgentsStore.getState().entries)[0].unrestricted).toBe(false);
});

it.each([true, false])('reloads after an identity change during the wait (new identity allowed=%s)', async allowed => {
  const old = deferredGrants();
  const next = deferredGrants();
  vi.mocked(api.agents.callable).mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise);
  const socket = await handshake();
  const result = client.call('agents.pause', { agent_id: 'alice' });
  const rejected = allowed ? null : expect(result).rejects.toThrow('revoked');
  await settle();
  useAuthStore.setState({ user: { id: 'next', role: 'employee' } as never });
  old.resolve(response([], true));
  await settle();
  expect(api.agents.callable).toHaveBeenCalledTimes(2);
  expect(socket.sent).toHaveLength(1);
  next.resolve(response(allowed ? ['alice'] : []));
  await settle();
  if (allowed) {
    reply(socket, 'agents.pause', { agent_id: 'alice' });
    await expect(result).resolves.toBe('sent');
  } else {
    await rejected;
    expect(socket.sent).toHaveLength(1);
  }
  expect(Object.keys(useCallableAgentsStore.getState().entries)).toEqual([JSON.stringify([callableSubject(), null])]);
});

it('uses session-scoped grants when requested', async () => {
  vi.mocked(api.agents.callable).mockResolvedValueOnce(response(['alice']));
  await assertAgentRequestAllowed('agents.pause', { agent_id: 'alice' }, 'session-a');
  expect(api.agents.callable).toHaveBeenCalledWith('session-a');
  expect(useCallableAgentsStore.getState().entries[JSON.stringify([callableSubject(), null])]).toBeUndefined();
});

it.each(['agents.inspect', 'agents.list', 'memory.history', 'computer_sessions.status', 'wiki.search'])(
  'sends read-only %s without starting or waiting for callable discovery', async method => {
    await expectSent(method, { agent_id: 'alice' });
    expect(api.agents.callable).not.toHaveBeenCalled();
    const pending = deferredGrants();
    vi.mocked(api.agents.callable).mockReturnValueOnce(pending.promise);
    const load = useCallableAgentsStore.getState().load(JSON.stringify([callableSubject(), null]));
    await expectSent(method, { agent_id: 'alice' });
    expect(api.agents.callable).toHaveBeenCalledTimes(1);
    pending.resolve(response());
    await load;
  },
);

it('never treats a cached admin role or personal edition alone as unrestricted', async () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin', status: 'active' } as never });
  useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
  await expect(assertAgentRequestAllowed('agents.pause', { agent_id: 'alice' })).rejects.toThrow('revoked');
});

it.each([...AGENT_TARGET_KEYS])('fences unknown mutation target key %s', async key => {
  await expect(assertAgentRequestAllowed('future.mutate', { [key]: 'revoked' })).rejects.toThrow('revoked');
});

it('checks every nested agent and both handoff endpoints, without treating human assignees as agents', async () => {
  grants(['alice']);
  await expect(assertAgentRequestAllowed('plans.create', {
    agent_id: 'alice', steps: [{ assignee_kind: 'agent', assignee: 'revoked' }],
  })).rejects.toThrow('revoked');
  await expect(assertAgentRequestAllowed('agents.handoff', { from_agent: 'alice', to_agent: 'revoked' })).rejects.toThrow('revoked');
  await expect(assertAgentRequestAllowed('future.mutate', { agent_ids: ['alice', 'revoked'] })).rejects.toThrow('revoked');
  await expect(assertAgentRequestAllowed('plans.create', {
    agent_id: 'alice', steps: [{ assignee_kind: 'user', assignee: 'human-id' }],
  })).resolves.toBeUndefined();
});

it('does not infer read-only status from a method name', async () => {
  await expect(assertAgentRequestAllowed('future.get_and_delete', { agent_id: 'alice' })).rejects.toThrow('revoked');
  await expect(assertAgentRequestAllowed('computer_sessions.view', { agent_id: 'alice' })).rejects.toThrow('revoked');
});

it('never gates identity discovery or auth bootstrap', async () => {
  await expectSent('agents.callable', { agent_id: 'alice' });
  for (const method of ['connect', 'connect.challenge', 'users.me', 'users.change_password', 'auth.cli_login.start']) {
    expect(assertAgentRequestAllowed(method, { agent_id: 'alice' })).toBeUndefined();
  }
  expect(api.agents.callable).not.toHaveBeenCalled();
});

it.each(['tasks', 'causal', 'meeting'] as const)('waits for grants before HTTP %s sends and reads the current JWT', async kind => {
  const pending = deferredGrants();
  const next = deferredGrants();
  vi.mocked(api.agents.callable).mockReturnValueOnce(pending.promise).mockReturnValueOnce(next.promise);
  const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ ok: true, payload: 'sent' }) });
  vi.stubGlobal('fetch', fetcher);
  const result = kind === 'tasks'
    ? callTaskRpc('tasks.goal_create', { agent_id: 'alice', attachments: [{ name: 'note', mime: 'text/plain', data_base64: 'YQ==' }] })
    : kind === 'causal' ? causalApi.importMemory('alice', 'memory') : meetingApi.cancel('alice', 'job');
  await settle();
  expect(fetcher).not.toHaveBeenCalled();
  useAuthStore.setState({ jwt: 'new-token' });
  pending.resolve(response([], true));
  await settle();
  expect(fetcher).not.toHaveBeenCalled();
  next.resolve(response(['alice']));
  await result;
  expect(fetcher).toHaveBeenCalledTimes(1);
  const headers = new Headers(fetcher.mock.calls[0][1].headers);
  expect(headers.get('Authorization')).toBe('Bearer new-token');
});

it('rejects HTTP task attachments after callable failure without calling fetch', async () => {
  vi.mocked(api.agents.callable).mockRejectedValueOnce(new Error('offline'));
  const fetcher = vi.fn();
  vi.stubGlobal('fetch', fetcher);
  await expect(callTaskRpc('tasks.goal_create', {
    agent_id: 'alice', attachments: [{ name: 'note', mime: 'text/plain', data_base64: 'YQ==' }],
  })).rejects.toThrow('revoked');
  expect(fetcher).not.toHaveBeenCalled();
});

it('checks HTTP query targets and JSON bodies but permits reads', async () => {
  await expect(assertAgentHttpRequestAllowed('/api/meeting-jobs/id/cancel?agent=alice', { method: 'POST' })).rejects.toThrow('revoked');
  await expect(assertAgentHttpRequestAllowed('/api/meeting-uploads', { method: 'POST', body: JSON.stringify({ agent_id: 'alice' }) })).rejects.toThrow('revoked');
  expect(assertAgentHttpRequestAllowed('/api/meeting-jobs?agent=alice', {})).toBeUndefined();
});

it('cannot send using grants from an identity switched before the guard continuation', async () => {
  grants(['alice']);
  const send = vi.fn();
  const result = withAgentRequestAllowed('agents.pause', { agent_id: 'alice' }, send);
  // ensureFreshGrants has resolved, but the guard has not resumed yet.
  useAuthStore.setState({ jwt: 'other-identity' });
  await expect(result).rejects.toThrow('revoked');
  expect(send).not.toHaveBeenCalled();
});

it('starts send in the final guard continuation before a queued identity switch', async () => {
  grants(['alice']);
  const send = vi.fn(() => useAuthStore.getState().jwt);
  const result = withAgentRequestAllowed('agents.pause', { agent_id: 'alice' }, send);
  queueMicrotask(() => {
    // An await of the old assertion would resume the transport after this switch.
    expect(send).toHaveBeenCalledTimes(1);
    useAuthStore.setState({ jwt: 'other-identity' });
  });
  await result;
  expect(send).toHaveReturnedWith(null);
});

it('does not exempt mail.read, which mutates read status', async () => {
  expect(AGENT_GUARD_READ_ONLY_METHODS.has('mail.read')).toBe(false);
  await expect(assertAgentRequestAllowed('mail.read', { agent_id: 'revoked' })).rejects.toThrow('revoked');
  // Production params identify only the mail; the server resolves its owner.
  expect(assertAgentRequestAllowed('mail.read', { id: 'mail-1' })).toBeUndefined();
});

it('never sends a knowledge XHR for a revoked owning agent', async () => {
  const send = vi.fn();
  vi.stubGlobal('XMLHttpRequest', class {
    upload = {};
    send = send;
    open = vi.fn();
    setRequestHeader = vi.fn();
    abort = vi.fn();
  });
  const upload = uploadSources('collection', [new File(['data'], 'a.txt')], undefined, 'revoked');
  await expect(upload.promise).rejects.toThrow('revoked');
  expect(send).not.toHaveBeenCalled();
});

it('cancels a knowledge upload while its grants are pending', async () => {
  const pending = deferredGrants();
  vi.mocked(api.agents.callable).mockReturnValueOnce(pending.promise);
  const send = vi.fn();
  vi.stubGlobal('XMLHttpRequest', class {
    upload = {};
    send = send;
    open = vi.fn();
    setRequestHeader = vi.fn();
    abort = vi.fn();
  });
  const upload = uploadSources('collection', [], undefined, 'alice');
  await settle();
  upload.abort();
  await expect(upload.promise).rejects.toThrow('aborted');
  pending.resolve(response(['alice']));
  await settle();
  expect(send).not.toHaveBeenCalled();
});
