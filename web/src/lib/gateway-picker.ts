/**
 * Gateway picker bridge — typed wrappers over the Tauri `gateway_*` commands
 * (WP-GW). The desktop shell boots on `/gateway-picker`; these commands let the
 * page discover LAN gateways over mDNS, health-check a URL, remember the last
 * choice, and navigate the window to the selected gateway.
 *
 * Everything degrades gracefully outside Tauri: {@link isTauri} is false and the
 * wrappers reject, so the page can show a browser-only notice instead of
 * crashing. Mirrors the `withGlobalTauri: true` access pattern used by pet.ts.
 */

/** A discovered / remembered gateway candidate (mirrors the Rust struct). */
export interface GatewayRecord {
  name: string;
  host: string;
  port: number;
  version: string;
  tls: boolean;
  url: string;
}

/** Result of a `/healthz` probe. */
export interface HealthReport {
  ok: boolean;
  version: string | null;
  name: string | null;
  error: string | null;
}

/** Local sidecar status for the "本機" card. */
export interface LocalStatus {
  status: 'running' | 'stopped' | 'error';
  port: number;
  url: string;
}

/** Persisted desktop gateway settings. */
export interface DesktopGatewayState {
  last_gateway?: GatewayRecord | null;
  recent: GatewayRecord[];
}

type TauriGlobal = {
  core?: { invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
};

function tauri(): TauriGlobal | undefined {
  return (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__;
}

/** True when running inside the Tauri desktop shell (picker available). */
export function isTauri(): boolean {
  return typeof tauri()?.core?.invoke === 'function';
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const fn = tauri()?.core?.invoke;
  if (typeof fn !== 'function') {
    throw new Error('gateway picker is only available in the desktop app');
  }
  return (await fn(cmd, args)) as T;
}

/** Browse the LAN for gateways over mDNS (~3s). */
export const gatewayDiscover = () => invoke<GatewayRecord[]>('gateway_discover');

/** Probe `GET <url>/healthz` (2s timeout). */
export const gatewayHealth = (url: string) => invoke<HealthReport>('gateway_health', { url });

/** Validate + persist a selection, then navigate the main window to it. */
export const gatewaySelect = (record: GatewayRecord) => invoke<void>('gateway_select', { record });

/** Read the persisted last selection + recent list. */
export const gatewayLast = () => invoke<DesktopGatewayState>('gateway_last');

/** Live status of the local sidecar (polled by the "本機" card). */
export const gatewayLocalStatus = () => invoke<LocalStatus>('gateway_local_status');

/** (Re)start the local sidecar — used when the user explicitly picks "本機". */
export const gatewayStartLocal = () => invoke<number>('gateway_start_local');
