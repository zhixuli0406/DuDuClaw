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

// ── Auto-selection policy (design §2.5) ──────────────────────────────────────

/**
 * What the picker should do after probing the remembered gateway + scanning the
 * LAN. `auto` carries the target; `list` reveals the manual picker; `local`
 * starts and connects to the bundled sidecar.
 */
export type PickerAction =
  | { kind: 'auto'; target: GatewayRecord; from: 'remembered' | 'discovered' }
  | { kind: 'list' }
  | { kind: 'local' };

/** Inputs to {@link decideAction} — kept as a plain record so it stays pure. */
export interface DecideInput {
  /** The last-connected gateway, if any. */
  remembered: GatewayRecord | null;
  /** Whether {@link DecideInput.remembered}'s `/healthz` responded ok. */
  rememberedHealthy: boolean;
  /** Gateways found on the LAN this scan. */
  discovered: GatewayRecord[];
}

/**
 * Pure auto-selection policy (design §2.5 "自動選擇策略"):
 *
 *  1. remembered gateway is healthy → auto-connect it (picker never shown)
 *  2. remembered gateway is unreachable → fall to the picker (user decides)
 *  3. no remembered, exactly 1 discovered → auto-connect it (toast)
 *  4. no remembered, ≥2 discovered → show the picker list
 *  5. no remembered, 0 discovered → start + connect the local sidecar
 *
 * Deterministic and side-effect-free so it can be unit-tested; the page performs
 * the actual health probe / scan and feeds the results in here.
 */
export function decideAction(input: DecideInput): PickerAction {
  const { remembered, rememberedHealthy, discovered } = input;
  if (remembered) {
    return rememberedHealthy
      ? { kind: 'auto', target: remembered, from: 'remembered' }
      : { kind: 'list' };
  }
  if (discovered.length === 1) {
    return { kind: 'auto', target: discovered[0], from: 'discovered' };
  }
  if (discovered.length >= 2) return { kind: 'list' };
  return { kind: 'local' };
}

/**
 * True when the desktop shell was navigated back to the picker to *switch*
 * gateways (tray "切換 Gateway" appends `switch=1`). In that case the page must
 * show the picker instead of silently auto-connecting the remembered gateway.
 * Robust across hash/path routing — just looks for the marker anywhere in the
 * current URL.
 */
export function isSwitchIntent(): boolean {
  return typeof window !== 'undefined' && window.location.href.includes('switch=1');
}
