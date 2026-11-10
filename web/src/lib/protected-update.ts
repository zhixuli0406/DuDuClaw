/** The root updater's durable result is authoritative across gateway restarts. */
export type ProtectedUpdateState =
  | 'queued' | 'downloading' | 'prepared' | 'activated' | 'committed' | 'failed' | 'maintenance';

export interface ProtectedUpdateResult {
  success: boolean;
  message: string;
  needs_restart: boolean;
  protected_request_id: string;
  protected_state: ProtectedUpdateState;
}

export function protectedUpdateFinished(state: ProtectedUpdateState): boolean {
  return state === 'committed' || state === 'failed' || state === 'maintenance';
}

/** getRandomValues also works on LAN HTTP dashboards where randomUUID is absent. */
export function newProtectedUpdateId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

const KEY = 'duduclaw-protected-update-request';
const ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function rememberProtectedUpdate(id: string | null): void {
  try {
    if (id === null) sessionStorage.removeItem(KEY);
    else if (ID.test(id)) sessionStorage.setItem(KEY, id);
  } catch { /* Polling still works when session storage is unavailable. */ }
}

export function pendingProtectedUpdate(): string | null {
  try {
    const id = sessionStorage.getItem(KEY);
    return id && ID.test(id) ? id : null;
  } catch { return null; }
}

/** Gateway result code: the host has no reachable root updater, so the gateway
 * sent NO request. Until the separate update service is installable (planned
 * v1.82.0) the administrator updates from the host terminal instead. */
export const UPDATER_NOT_INSTALLED = 'updater_not_installed';

/** Root-managed command the dashboard shows instead of updating itself. */
export const HOST_UPGRADE_COMMAND = 'sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux';

/** Only an ok payload carrying the code and no request id qualifies. */
export function isUpdaterNotInstalled(reply: unknown): boolean {
  if (!reply || typeof reply !== 'object') return false;
  const data = reply as { code?: unknown; protected_request_id?: unknown };
  return data.code === UPDATER_NOT_INSTALLED && data.protected_request_id === undefined;
}

export type ProtectedSubmission =
  | { kind: 'known'; result: ProtectedUpdateResult }
  | { kind: 'unknown'; requestId: string }
  | { kind: 'updater_not_installed' };

/** A gateway error can wrap a lost daemon reply AFTER durable acceptance.
 * Only a correlated daemon result settles the request; rejection shape is not evidence.
 */
export async function submitProtectedUpdate(
  requestId: string,
  submit: (id: string) => Promise<unknown>,
): Promise<ProtectedSubmission> {
  rememberProtectedUpdate(requestId);
  try {
    const reply = await submit(requestId);
    if (!reply || typeof reply !== 'object') return { kind: 'unknown', requestId };
    if (isUpdaterNotInstalled(reply)) {
      // Nothing was sent, so there is no request to reconcile later.
      rememberProtectedUpdate(null);
      return { kind: 'updater_not_installed' };
    }
    const data = reply as Partial<ProtectedUpdateResult>;
    const states: readonly string[] = ['queued', 'downloading', 'prepared', 'activated', 'committed', 'failed', 'maintenance'];
    if (data.protected_request_id !== requestId || typeof data.protected_state !== 'string'
      || !states.includes(data.protected_state)) return { kind: 'unknown', requestId };
    if (protectedUpdateFinished(data.protected_state)) rememberProtectedUpdate(null);
    return { kind: 'known', result: {
      success: data.protected_state === 'committed', message: '', needs_restart: false,
      protected_request_id: requestId, protected_state: data.protected_state,
    } };
  } catch {
    // Includes WsFrame errors (strings/objects), not only client-side Error instances.
    return { kind: 'unknown', requestId };
  }
}
