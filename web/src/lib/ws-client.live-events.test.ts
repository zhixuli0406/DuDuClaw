import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
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

const originalWebSocket = globalThis.WebSocket;
let client: DuDuClawClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('WebSocket', Socket);
  Socket.instances = [];
  client = new DuDuClawClient();
});
afterEach(() => {
  client.disconnect();
  vi.useRealTimers();
  vi.stubGlobal('WebSocket', originalWebSocket);
});

async function handshake(bootId?: string) {
  const connected = client.connect('ws://localhost/ws');
  const socket = Socket.instances[Socket.instances.length - 1];
  const opened = socket.onopen?.();
  await Promise.resolve();
  const request = socket.sent.find((frame) => frame.type === 'req' && frame.method === 'connect');
  if (!request || request.type !== 'req') throw new Error('Missing connect request');
  socket.receive({ type: 'res', id: request.id, ok: true, payload: { boot_id: bootId } });
  await opened;
  await connected;
  return socket;
}

describe('live-event reconciliation metadata', () => {
  it('notifies stores after a successful handshake and on a changed event boot', async () => {
    const ready = vi.fn();
    const lost = vi.fn();
    client.subscribe('connection.ready', ready);
    client.subscribe('events_lost', lost);
    const socket = await handshake('boot-a');
    expect(ready).toHaveBeenCalledTimes(1);
    socket.receive({ type: 'event', event: 'task.created', payload: {}, boot_id: 'boot-a', seq: 1 });
    expect(lost).not.toHaveBeenCalled();
    socket.receive({ type: 'event', event: 'task.created', payload: {}, boot_id: 'boot-b', seq: 2 });
    expect(lost).toHaveBeenCalledWith({ reason: 'gateway_restarted' });
  });

  it('reconciles after reconnect even when boot_id did not change', async () => {
    const ready = vi.fn();
    client.subscribe('connection.ready', ready);
    await handshake('boot-a');
    await handshake('boot-a');
    expect(ready).toHaveBeenCalledTimes(2);
  });

  it('accepts older gateways that omit boot_id', async () => {
    const lost = vi.fn();
    client.subscribe('events_lost', lost);
    const socket = await handshake();
    socket.receive({ type: 'event', event: 'task.created', payload: {} });
    expect(lost).not.toHaveBeenCalled();
    expect(client.state).toBe('authenticated');
  });
});
