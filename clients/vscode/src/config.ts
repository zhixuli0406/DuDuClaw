// Shared configuration: gateway URL resolution, Secret Storage keys, and the
// two plain HTTP helpers every auth call needs. Kept dependency-free (only
// `vscode`) so every other module can import it without cycles.
import * as vscode from 'vscode';

export const SECRET_ACCESS = 'duduclaw.accessToken';
export const SECRET_REFRESH = 'duduclaw.refreshToken';

/** Workspace-scoped memory of the last AI employee the user talked to from
 * THIS workspace (per design doc §4 P0 — "記住 workspace 最後選擇"). */
export const STATE_LAST_AGENT = 'duduclaw.lastAgent';

/**
 * Workspace-scoped memory of the most recent WebChat session id used for a
 * given AI employee (design doc §4 P1 — "把最近 session id 按 (gatewayUrl,
 * agent) 存 workspaceState，重連時帶上以續聊"). Scoped by BOTH the current
 * gateway URL and the agent name so pointing the same workspace at a
 * different gateway, or talking to a different employee, never resumes the
 * wrong conversation. See rpc.ts's `GatewayClient` for how the id is
 * captured/derived and why it can only be a best-effort reconstruction for
 * a brand-new (never-resumed) session — the WebChat protocol only echoes the
 * server-composed session id back to the client on an actual resume, not on
 * first use (`webchat.rs`'s `compose_session_id` path sends no confirmation
 * frame).
 */
export function sessionStateKey(agent: string | undefined): string {
  return `duduclaw.lastSessionId::${gatewayUrl()}::${agent ?? '__default__'}`;
}

export function gatewayUrl(): string {
  const raw =
    vscode.workspace.getConfiguration('duduclaw').get<string>('gatewayUrl') ??
    'http://127.0.0.1:18789';
  return raw.replace(/\/+$/, '');
}

export async function apiPost(path: string, body: unknown): Promise<Response> {
  return fetch(`${gatewayUrl()}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
}

export async function apiGet(path: string, token: string): Promise<Response> {
  return fetch(`${gatewayUrl()}${path}`, {
    headers: { authorization: `Bearer ${token}` },
  });
}
