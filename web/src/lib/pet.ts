/**
 * Desktop-pet bridge — typed wrappers over the Tauri `pet_*` commands (WP-P2/P3).
 *
 * The pet studio + mascot overlay are the only callers. Everything degrades
 * gracefully outside Tauri (a plain browser): {@link isTauri} is false and the
 * command wrappers reject, so the UI can show a "desktop-only" state instead of
 * crashing. The app runs with `withGlobalTauri: true`, so we reach the runtime
 * through `window.__TAURI__` rather than an npm package.
 */

/** Render mode of a pet pack. */
export type PetMode = 'procedural' | 'sprite';

/** A pet as shown in the studio list. */
export interface PetSummary {
  slug: string;
  displayName: string;
  mode: PetMode;
  active: boolean;
}

/** A single behavior weight (state → frequency, optional precondition). */
export interface PetBehavior {
  state: string;
  frequency: number;
  condition: string | null;
}

/** Everything the overlay runtime needs to render + animate a pet. */
export interface PetRuntimePayload {
  slug: string;
  displayName: string;
  mode: PetMode;
  imageDataUrl: string | null;
  behaviors: PetBehavior[];
}

/** Result of generating a pet: summary + cutout preview + which remover ran. */
export interface GeneratedPet {
  slug: string;
  displayName: string;
  mode: PetMode;
  imageDataUrl: string | null;
  /** 'birefnet' | 'silueta' | 'passthrough' (passthrough = no removal ran). */
  removerLabel: string;
}

/** Which local segmentation models are installed. */
export interface ModelStatus {
  birefnetPresent: boolean;
  siluetaPresent: boolean;
  birefnetUrl: string;
  siluetaUrl: string;
}

type TauriGlobal = {
  core?: { invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
  event?: {
    listen?: (
      event: string,
      handler: (e: { payload: unknown }) => void
    ) => Promise<() => void>;
  };
  window?: {
    getCurrentWindow?: () => { startDragging?: () => Promise<void> };
  };
};

function tauri(): TauriGlobal | undefined {
  return (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__;
}

/** True when running inside the Tauri desktop shell (pet features available). */
export function isTauri(): boolean {
  return typeof tauri()?.core?.invoke === 'function';
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const fn = tauri()?.core?.invoke;
  if (typeof fn !== 'function') {
    throw new Error('pet commands are only available in the desktop app');
  }
  return (await fn(cmd, args)) as T;
}

/** List all pet packs. */
export const petList = () => invoke<PetSummary[]>('pet_list');

/**
 * Generate a pet from a base64 (or data-URL) photo. `externalCutout` skips
 * background removal when the caller already supplies a transparent PNG.
 */
export const petGenerate = (name: string, photoBase64: string, externalCutout = false) =>
  invoke<GeneratedPet>('pet_generate', { name, photoBase64, externalCutout });

/** The currently active pet, if any. */
export const petActiveGet = () => invoke<PetSummary | null>('pet_active_get');

/** Activate a pet (or clear with `null`); shows the pet window + broadcasts. */
export const petActivate = (slug: string | null) => invoke<void>('pet_activate', { slug });

/** Delete a pet pack. */
export const petRemove = (slug: string) => invoke<void>('pet_remove', { slug });

/** Load the active pet's full runtime payload (manifest + image data URL). */
export const petLoadActive = () => invoke<PetRuntimePayload | null>('pet_load_active');

/** Which local segmentation models are installed. */
export const petModelStatus = () => invoke<ModelStatus>('pet_model_status');

/** Download a segmentation model (`'birefnet'` | `'silueta'`); returns SHA-256. */
export const petModelDownload = (variant: 'birefnet' | 'silueta') =>
  invoke<string>('pet_model_download', { variant });

/** Event name broadcast when the active pet changes (matches the Rust const). */
export const PET_CHANGED_EVENT = 'pet://changed';

/** Subscribe to active-pet changes. Returns an unlisten fn (no-op outside Tauri). */
export async function onPetChanged(handler: () => void): Promise<() => void> {
  const listen = tauri()?.event?.listen;
  if (typeof listen !== 'function') return () => {};
  return listen(PET_CHANGED_EVENT, () => handler());
}

/**
 * Start a native window drag (moves the OS window under the cursor). No-op
 * outside Tauri. Used by the overlay's hand-rolled drag gesture instead of
 * `data-tauri-drag-region` (which conflicts with click detection — Tauri
 * #9751/#9901).
 */
export async function startWindowDrag(): Promise<void> {
  const w = tauri()?.window?.getCurrentWindow?.();
  if (w?.startDragging) {
    try {
      await w.startDragging();
    } catch {
      /* window drag can reject if the gesture was already consumed — ignore */
    }
  }
}
