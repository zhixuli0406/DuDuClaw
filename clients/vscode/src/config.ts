// Shared configuration: gateway URL resolution, Secret Storage keys, and the
// two plain HTTP helpers every auth call needs. Kept dependency-free (only
// `vscode`) so every other module can import it without cycles.
import * as vscode from 'vscode';

export const SECRET_ACCESS = 'duduclaw.accessToken';
export const SECRET_REFRESH = 'duduclaw.refreshToken';
const NEW_GATEWAY = 'http://127.0.0.1:18889';
const LEGACY_GATEWAY = 'http://127.0.0.1:18789';
const STATE_IMPLICIT_GATEWAY = 'duduclaw.implicitGatewayV2';
let implicitGateway = NEW_GATEWAY;

async function isDuduClawGateway(base: string): Promise<boolean> {
  try {
    const response = await fetch(`${base}/healthz`, {
      signal: AbortSignal.timeout(500),
    });
    const body = await response.json() as { service?: unknown };
    return body.service === 'duduclaw-gateway';
  } catch {
    return false;
  }
}

function explicitGatewayUrl(): string | undefined {
  const inspected = vscode.workspace
    .getConfiguration('duduclaw')
    .inspect<string>('gatewayUrl');
  return inspected?.workspaceFolderValue
    ?? inspected?.workspaceValue
    ?? inspected?.globalValue;
}

/** Preserve the former implicit endpoint for an active legacy profile while
 * assigning the new endpoint to fresh installs. The health marker prevents a
 * different product on 18789 from being mistaken for DuDuClaw. */
export async function initializeGatewayUrl(context: vscode.ExtensionContext): Promise<void> {
  if (explicitGatewayUrl()) return;
  const remembered = context.globalState.get<string>(STATE_IMPLICIT_GATEWAY);
  if (remembered) {
    implicitGateway = remembered;
    return;
  }

  const [currentLive, legacyLive, accessToken] = await Promise.all([
    isDuduClawGateway(NEW_GATEWAY),
    isDuduClawGateway(LEGACY_GATEWAY),
    context.secrets.get(SECRET_ACCESS),
  ]);
  // A legacy token belongs to the old endpoint unless the user explicitly
  // changes the setting. Never redirect it to a different DuDuClaw instance.
  implicitGateway = accessToken
    ? LEGACY_GATEWAY
    : currentLive
      ? NEW_GATEWAY
      : legacyLive
        ? LEGACY_GATEWAY
        : NEW_GATEWAY;
  await context.globalState.update(STATE_IMPLICIT_GATEWAY, implicitGateway);
}

/** Revalidate the selected endpoint immediately before any password, refresh
 * token, access token, or authenticated WebSocket leaves the extension. An
 * explicit URL selects the destination; it does not prove which service is
 * currently listening there. */
export async function ensureGatewayIdentity(): Promise<void> {
  const selected = gatewayUrl();
  if (!(await isDuduClawGateway(selected))) {
    throw new Error(
      `refusing to send credentials: ${selected} is not a DuDuClaw gateway`,
    );
  }
}

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
  const raw = explicitGatewayUrl() ?? implicitGateway;
  return raw.replace(/\/+$/, '');
}

export async function apiPost(path: string, body: unknown): Promise<Response> {
  await ensureGatewayIdentity();
  return fetch(`${gatewayUrl()}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
}

export async function apiGet(path: string, token: string): Promise<Response> {
  await ensureGatewayIdentity();
  return fetch(`${gatewayUrl()}${path}`, {
    headers: { authorization: `Bearer ${token}` },
  });
}
