// Login/refresh flow, `/api/me` (role + agent bindings), and the shared
// error-classification helpers used across the chat socket, the RPC socket,
// and the webview's error rendering.
import * as vscode from 'vscode';
import { SECRET_ACCESS, SECRET_REFRESH, apiGet, apiPost, gatewayUrl } from './config';
import type { MeResponse } from './types';

export async function doLogin(context: vscode.ExtensionContext): Promise<boolean> {
  const email = await vscode.window.showInputBox({
    prompt: `DuDuClaw 登入 Email（${gatewayUrl()}）`,
    ignoreFocusOut: true,
  });
  if (!email) return false;
  const password = await vscode.window.showInputBox({
    prompt: 'DuDuClaw 密碼',
    password: true,
    ignoreFocusOut: true,
  });
  if (!password) return false;

  let res: Response;
  try {
    res = await apiPost('/api/login', { email, password });
  } catch (e) {
    vscode.window.showErrorMessage(`DuDuClaw：連不上 gateway（${gatewayUrl()}）— ${e}`);
    return false;
  }
  const data = (await res.json().catch(() => ({}))) as {
    access_token?: string;
    refresh_token?: string;
    error?: string;
  };
  if (!res.ok || !data.access_token) {
    vscode.window.showErrorMessage(`DuDuClaw：登入失敗 — ${data.error ?? res.status}`);
    return false;
  }
  await context.secrets.store(SECRET_ACCESS, data.access_token);
  if (data.refresh_token) await context.secrets.store(SECRET_REFRESH, data.refresh_token);
  vscode.window.showInformationMessage('DuDuClaw：登入成功 🐾');
  return true;
}

export async function refreshAccessToken(
  context: vscode.ExtensionContext
): Promise<string | undefined> {
  const rt = await context.secrets.get(SECRET_REFRESH);
  if (!rt) return undefined;
  try {
    const res = await apiPost('/api/refresh', { refresh_token: rt });
    if (!res.ok) return undefined;
    const data = (await res.json()) as { access_token?: string; refresh_token?: string };
    if (!data.access_token) return undefined;
    await context.secrets.store(SECRET_ACCESS, data.access_token);
    if (data.refresh_token) await context.secrets.store(SECRET_REFRESH, data.refresh_token);
    return data.access_token;
  } catch {
    return undefined;
  }
}

/**
 * GET /api/me — the current user's role plus their per-agent bindings.
 * Retries once through the refresh-token exchange on 401, mirroring the
 * retry-on-auth-failure behaviour already used by the chat/RPC sockets.
 * Returns `undefined` on any failure (network, 401 with no usable refresh
 * token, malformed body) — callers treat that as "not authed", never as
 * "role unknown but proceed".
 */
export async function fetchMe(
  context: vscode.ExtensionContext,
  retried = false
): Promise<MeResponse | undefined> {
  const token = await context.secrets.get(SECRET_ACCESS);
  if (!token) return undefined;
  let res: Response;
  try {
    res = await apiGet('/api/me', token);
  } catch {
    return undefined;
  }
  if (res.status === 401 && !retried) {
    const t = await refreshAccessToken(context);
    if (!t) return undefined;
    return fetchMe(context, true);
  }
  if (!res.ok) return undefined;
  try {
    return (await res.json()) as MeResponse;
  } catch {
    return undefined;
  }
}

/**
 * The gateway's ACL layer (`crates/duduclaw-auth/src/acl.rs`,
 * `require_role` / `require_agent_access`) returns the exact literal
 * `"permission denied"` (lowercase, no extra context — deliberately generic
 * to avoid role/agent enumeration) for every authorization failure on the
 * `/ws` RPC channel. This is the single place that recognizes it; nowhere
 * else in the extension should pattern-match on error text.
 */
export function isPermissionDeniedError(message: string): boolean {
  return /\bpermission denied\b/i.test(message);
}

/**
 * Heuristic for "this failure means the JWT is dead, try a refresh" —
 * unchanged from the pre-v2 implementation, just centralized so the chat
 * socket and any future caller share one definition instead of copies
 * drifting apart.
 */
export function isAuthError(message: string): boolean {
  return /auth|token|jwt|unauthor/i.test(message);
}

/**
 * Rewrites a raw RPC/HTTP error into user-facing zh-TW copy. Currently the
 * only class of error we can positively identify is "permission denied";
 * everything else passes through unchanged (still truncated by the caller)
 * rather than inventing copy for failures we haven't classified.
 */
export function friendlyErrorMessage(raw: string): string {
  if (isPermissionDeniedError(raw)) return '此功能需要管理者權限。';
  return raw;
}

/** `String(e)` on a caught `Error` yields `"Error: <message>"` — apply the
 * friendly-copy rewrite BEFORE truncating so the permission-denied pattern
 * (short and always near the start) is never cut off, then cap length for
 * display. */
export function renderErrorMessage(e: unknown, maxLen = 200): string {
  return friendlyErrorMessage(String(e)).slice(0, maxLen);
}
