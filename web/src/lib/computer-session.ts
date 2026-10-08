import { client } from './ws-client';

/**
 * Live computer-use session of one AI employee (P8): status, watch, take
 * over / hand back, resume after an injection pause, stop. The gateway
 * re-reads the caller's role and bindings on every call
 * (`computer_sessions.*`), so nothing here decides who may do what; the
 * buttons only mirror the answers.
 */

export interface ComputerTakeover {
  /** Email (or id) of the person holding the lease. */
  holder: string;
  /** Whether the caller holds it. */
  mine: boolean;
  idle_seconds_left: number;
}

export interface ComputerHold {
  reason: 'injection_suspected';
  /** input_guard rule categories that matched (never page text). */
  categories: string[];
  since: number;
}

export type ComputerSessionStatus =
  | { ok: true; active: false }
  | {
      ok: true;
      active: true;
      session_id: string;
      state: 'running' | 'paused';
      actions_used: number;
      max_actions: number | null;
      seconds_left: number | null;
      keep_alive_minutes: number | null;
      paused_seconds_left: number | null;
      takeover: ComputerTakeover | null;
      hold: ComputerHold | null;
      viewers: number;
      stream_available: boolean;
    };

/** A one-time viewer ticket (valid 30 s, single use). */
export interface ComputerViewTicket {
  ok: true;
  session_id: string;
  ticket: string;
  /** Stream password (8 characters, rotated whenever the stream restarts). */
  password: string;
  /** Gateway route, always `/ws/computer-view`. */
  path: string;
  mode: 'viewonly' | 'control';
  /** Whether this viewer's keyboard and mouse reach the screen. */
  input: boolean;
  expires_in: number;
}

export const VIEW_PATH = '/ws/computer-view';

export const computerSessionApi = {
  status: (agentId: string) =>
    client.call('computer_sessions.status', { agent_id: agentId }) as Promise<ComputerSessionStatus>,
  view: (agentId: string) =>
    client.call('computer_sessions.view', { agent_id: agentId }) as Promise<ComputerViewTicket>,
  takeover: (agentId: string) =>
    client.call('computer_sessions.takeover', { agent_id: agentId }) as Promise<ComputerViewTicket>,
  handBack: (agentId: string, note?: string) =>
    client.call('computer_sessions.hand_back', {
      agent_id: agentId,
      ...(note && note.trim() ? { note: note.trim().slice(0, 200) } : {}),
    }) as Promise<{ ok: true; session_id: string }>,
  resume: (agentId: string) =>
    client.call('computer_sessions.resume', { agent_id: agentId }) as Promise<{ ok: true; session_id: string }>,
  stop: (agentId: string) =>
    client.call('computer_sessions.stop', { agent_id: agentId }) as Promise<{
      ok: true;
      session_id: string;
      ended: boolean;
    }>,
};

/**
 * The viewer WebSocket URL for a ticket: the dashboard socket's origin (or,
 * before it connected, the page's), the fixed live-view path, the ticket as
 * the only query parameter. A ticket or path the gateway would never issue
 * yields `null`, so a tampered answer can never point the viewer elsewhere.
 */
export function computerViewUrl(
  socketUrl: string,
  pageOrigin: string,
  path: string,
  ticket: string,
): string | null {
  if (path !== VIEW_PATH || !/^[0-9a-f]{32}$/.test(ticket)) return null;
  let base: URL;
  try {
    base = socketUrl
      ? new URL(socketUrl)
      : new URL(pageOrigin.replace(/^http(s?):/, 'ws$1:'));
  } catch {
    return null;
  }
  if (base.protocol !== 'ws:' && base.protocol !== 'wss:') return null;
  base.pathname = VIEW_PATH;
  base.search = `?ticket=${ticket}`;
  base.hash = '';
  base.username = '';
  base.password = '';
  return base.toString();
}

/** `m:ss` (or `h:mm:ss`) for a seconds count; `—` when unknown. */
export function formatClock(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return '—';
  const s = Math.floor(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
}

/** Which state badge a status shows. */
export function sessionPhase(
  status: ComputerSessionStatus | null,
): 'none' | 'running' | 'paused' | 'held' | 'human' {
  if (!status || !status.active) return 'none';
  if (status.hold) return 'held';
  if (status.takeover) return 'human';
  return status.state === 'paused' ? 'paused' : 'running';
}
