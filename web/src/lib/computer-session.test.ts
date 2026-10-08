import { describe, expect, it, beforeEach } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import {
  VIEW_PATH,
  computerSessionApi,
  computerViewUrl,
  formatClock,
  sessionPhase,
  type ComputerSessionStatus,
} from './computer-session';

const TICKET = '0123456789abcdef0123456789abcdef';

describe('computerViewUrl', () => {
  it('derives the viewer URL from the dashboard socket, keeping only the ticket', () => {
    expect(computerViewUrl('wss://gw.example.com/ws', 'https://x', VIEW_PATH, TICKET)).toBe(
      `wss://gw.example.com/ws/computer-view?ticket=${TICKET}`,
    );
    expect(computerViewUrl('ws://user:pw@127.0.0.1:18789/ws?jwt=secret#x', '', VIEW_PATH, TICKET)).toBe(
      `ws://127.0.0.1:18789/ws/computer-view?ticket=${TICKET}`,
    );
  });

  it('falls back to the page origin before the socket connected', () => {
    expect(computerViewUrl('', 'https://dash.example.com', VIEW_PATH, TICKET)).toBe(
      `wss://dash.example.com/ws/computer-view?ticket=${TICKET}`,
    );
    expect(computerViewUrl('', 'http://127.0.0.1:5173', VIEW_PATH, TICKET)).toBe(
      `ws://127.0.0.1:5173/ws/computer-view?ticket=${TICKET}`,
    );
  });

  it('refuses paths and tickets the gateway never issues', () => {
    expect(computerViewUrl('ws://h/ws', '', '/ws', TICKET)).toBeNull();
    expect(computerViewUrl('ws://h/ws', '', 'https://evil.example/x', TICKET)).toBeNull();
    expect(computerViewUrl('ws://h/ws', '', VIEW_PATH, 'abc&x=1')).toBeNull();
    expect(computerViewUrl('ws://h/ws', '', VIEW_PATH, TICKET.toUpperCase())).toBeNull();
    expect(computerViewUrl('javascript:alert(1)', '', VIEW_PATH, TICKET)).toBeNull();
    expect(computerViewUrl('', 'not a url', VIEW_PATH, TICKET)).toBeNull();
  });
});

describe('formatClock / sessionPhase', () => {
  it('formats seconds and unknowns', () => {
    expect(formatClock(5)).toBe('0:05');
    expect(formatClock(605)).toBe('10:05');
    expect(formatClock(3725)).toBe('1:02:05');
    expect(formatClock(null)).toBe('—');
    expect(formatClock(-1)).toBe('—');
  });

  it('ranks hold over takeover over pause', () => {
    const base = {
      ok: true,
      active: true,
      session_id: 'cu-1',
      state: 'running',
      actions_used: 0,
      max_actions: 50,
      seconds_left: 60,
      keep_alive_minutes: 0,
      paused_seconds_left: null,
      takeover: null,
      hold: null,
      viewers: 0,
      stream_available: true,
    } as const satisfies ComputerSessionStatus;
    expect(sessionPhase(null)).toBe('none');
    expect(sessionPhase({ ok: true, active: false })).toBe('none');
    expect(sessionPhase(base)).toBe('running');
    expect(sessionPhase({ ...base, state: 'paused' })).toBe('paused');
    const takeover = { holder: 'a', mine: false, idle_seconds_left: 1 };
    expect(sessionPhase({ ...base, takeover })).toBe('human');
    expect(
      sessionPhase({ ...base, takeover, hold: { reason: 'injection_suspected', categories: [], since: 0 } }),
    ).toBe('held');
  });
});

describe('computerSessionApi', () => {
  beforeEach(() => mockWsClient.call.mockReset().mockResolvedValue({ ok: true }));

  it('sends the agent id and a trimmed, capped note', async () => {
    await computerSessionApi.status('alice');
    expect(mockWsClient.call).toHaveBeenLastCalledWith('computer_sessions.status', { agent_id: 'alice' });
    await computerSessionApi.handBack('alice', '  ' + 'x'.repeat(300) + '  ');
    const [, params] = mockWsClient.call.mock.calls.at(-1)!;
    expect(params.note).toHaveLength(200);
    await computerSessionApi.handBack('alice', '   ');
    expect(mockWsClient.call).toHaveBeenLastCalledWith('computer_sessions.hand_back', { agent_id: 'alice' });
  });
});
