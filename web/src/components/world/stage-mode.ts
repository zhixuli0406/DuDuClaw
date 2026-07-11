/**
 * Degradation-chain decision + capability probes for the world stage (V8-T8.5).
 *
 * The *decision* is a pure function ({@link resolveStageMode}) so the fallback
 * logic is unit-testable with mocked inputs; the *probes* (WebGL support,
 * reduced-motion, viewport width) are thin runtime wrappers the component reads
 * once and feeds in.
 */

export type StageMode = 'stage' | 'static';

/** A user's remembered preference for the Home stage band. `null` = unset. */
export type StageChoice = 'stage' | 'list' | null;

/** localStorage key for the "⊞ 清單" toggle (persisted per spec). */
export const STAGE_MODE_STORAGE_KEY = 'duduclaw:home:stage-mode';

export interface StageModeInputs {
  /** `prefers-reduced-motion: reduce` is active. */
  readonly prefersReducedMotion: boolean;
  /** A WebGL rendering context can be created. */
  readonly webglAvailable: boolean;
  /** The user's remembered toggle choice (null when never set). */
  readonly userChoice: StageChoice;
  /** Viewport is below the `md` breakpoint (mobile). */
  readonly isMobile: boolean;
}

/**
 * Resolve which renderer the Home band should show.
 *
 * Static wins when ANY of: reduced-motion is on, WebGL is unavailable, or the
 * effective choice is the list view. The effective choice defaults to `list` on
 * mobile (power saving) and `stage` on desktop when the user hasn't toggled.
 */
export function resolveStageMode(inp: StageModeInputs): StageMode {
  if (inp.prefersReducedMotion) return 'static';
  if (!inp.webglAvailable) return 'static';
  const effective: 'stage' | 'list' =
    inp.userChoice ?? (inp.isMobile ? 'list' : 'stage');
  return effective === 'list' ? 'static' : 'stage';
}

/** Probe: can the browser create a WebGL context at all? (cheap, cached). */
let webglCache: boolean | null = null;
export function detectWebglSupport(): boolean {
  if (webglCache !== null) return webglCache;
  if (typeof document === 'undefined') return (webglCache = false);
  try {
    const canvas = document.createElement('canvas');
    const gl =
      canvas.getContext('webgl2') ||
      canvas.getContext('webgl') ||
      canvas.getContext('experimental-webgl');
    webglCache = !!gl;
  } catch {
    webglCache = false;
  }
  return webglCache;
}

/** Probe: read the persisted stage choice from localStorage. */
export function readStageChoice(): StageChoice {
  if (typeof localStorage === 'undefined') return null;
  try {
    const v = localStorage.getItem(STAGE_MODE_STORAGE_KEY);
    return v === 'stage' || v === 'list' ? v : null;
  } catch {
    return null;
  }
}

/** Persist the stage choice. */
export function writeStageChoice(choice: 'stage' | 'list'): void {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(STAGE_MODE_STORAGE_KEY, choice);
  } catch {
    /* private mode / quota — non-fatal, the choice just won't persist */
  }
}
