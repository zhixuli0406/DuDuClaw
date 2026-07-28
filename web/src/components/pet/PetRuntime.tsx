import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { PetRuntimePayload, SpriteAnimation } from '@/lib/pet';
import { onPetAgentSignal, openPetContextMenu, startWindowDrag } from '@/lib/pet';

/**
 * PetRuntime — the desktop-pet animator (WP-P3 / WP-P5-lite).
 *
 * Renders the active pet two ways:
 *  - **sprite mode** (pixel-art pets): plays the baked Codex Pets spritesheet,
 *    stepping frames of the row that matches the current state (idle / running /
 *    waving / waiting …) on a `<canvas>` with nearest-neighbour scaling.
 *  - **procedural mode** (single cutout): brings one background-removed image to
 *    life with pure WAAPI + a hand-rolled spring.
 *
 * Either way it owns the interaction state machine — idle / drag / fall / click /
 * working / notify / sleep — driven by weighted-random idle variants, a
 * hard-chained press→drag→release→fall gesture, and external agent signals.
 *
 * Drag uses a gesture (mousedown + move > {@link DRAG_THRESHOLD}px → native
 * window drag), NOT `data-tauri-drag-region`, so a plain click still registers
 * (Tauri #9751/#9901). Right-click pops the native pet menu. All motion is gated
 * on `prefers-reduced-motion`.
 */

/** Movement (px) before a press becomes a drag rather than a click. */
const DRAG_THRESHOLD = 4;
/** Idle time (ms) with no interaction before the pet dozes off. */
const SLEEP_AFTER_MS = 60_000;
/**
 * Fraction of the overlay window's short edge the pet is drawn at. The native
 * host resizes the WINDOW for 小/標準/大 (`pet_set_scale`), so the pet must
 * track the viewport — a fixed pixel size only grows the invisible frame.
 * The margin absorbs the drop shadow and the hop/jump animation overshoot.
 */
const DISPLAY_FRACTION = 0.92;
/** Lower bound (px) so the pet never collapses on a degenerate viewport. */
const DISPLAY_MIN = 48;

/** Longest edge (px) to draw the pet at, tracking live window resizes. */
export function useDisplayMax(): number {
  const [max, setMax] = useState(() =>
    typeof window === 'undefined'
      ? 170
      : Math.max(
          DISPLAY_MIN,
          Math.round(Math.min(window.innerWidth, window.innerHeight) * DISPLAY_FRACTION),
        ),
  );
  useEffect(() => {
    const onResize = () => {
      setMax(
        Math.max(
          DISPLAY_MIN,
          Math.round(Math.min(window.innerWidth, window.innerHeight) * DISPLAY_FRACTION),
        ),
      );
    };
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);
  return max;
}

export type PetState =
  | 'idle'
  | 'drag'
  | 'fall'
  | 'click'
  | 'working'
  | 'notify'
  | 'sleep';

/** External agent signal → pet reaction (real agent events wire in here). */
export interface PetAgentSignal {
  /** 'working' = busy animation, 'notify' = raise-a-flag, 'idle' = clear. */
  state: 'working' | 'notify' | 'idle';
}

/** Map a runtime state to a spritesheet row name (falls back to idle). */
function stateToRow(state: PetState): string {
  switch (state) {
    case 'working':
      return 'running';
    case 'notify':
    case 'click':
      return 'waving';
    case 'drag':
    case 'fall':
      return 'jumping';
    case 'sleep':
      return 'waiting';
    default:
      return 'idle';
  }
}

interface PetRuntimeProps {
  pet: PetRuntimePayload;
  /** Show the notify placard / hop (e.g. pending approvals count). */
  pendingCount?: number;
  /** Click handler (opens the main window in the overlay host). */
  onActivate?: () => void;
}

function prefersReducedMotion(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  );
}

/** WAAPI is available and motion is allowed. */
function canAnimate(el: HTMLElement | null): el is HTMLElement {
  return !!el && typeof el.animate === 'function' && !prefersReducedMotion();
}

export function PetRuntime({ pet, pendingCount = 0, onActivate }: PetRuntimeProps) {
  const spriteRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [state, setState] = useState<PetState>('idle');
  const stateRef = useRef<PetState>('idle');
  stateRef.current = state;

  // Gesture bookkeeping.
  const pressRef = useRef<{ x: number; y: number; dragging: boolean } | null>(null);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const idleAnimRef = useRef<Animation | null>(null);

  const sheet = pet.spriteSheet ?? null;
  const isSprite = !!sheet && !!pet.imageDataUrl;

  const hasPending = pendingCount > 0;
  // A notify raised by pending items is dismissable; re-arms when more arrive.
  const [notifyDismissed, setNotifyDismissed] = useState(false);
  useEffect(() => {
    if (pendingCount > 0) setNotifyDismissed(false);
  }, [pendingCount]);
  const showPlacard = state === 'notify';

  // ── Sprite playback (pixel-art pets) ──────────────────────────────────────
  const anims = useMemo(() => {
    const map = new Map<string, SpriteAnimation>();
    for (const a of sheet?.animations ?? []) map.set(a.state, a);
    return map;
  }, [sheet]);

  // Load the spritesheet image once.
  const imgRef = useRef<HTMLImageElement | null>(null);
  const [imgReady, setImgReady] = useState(false);
  useEffect(() => {
    if (!isSprite || !pet.imageDataUrl) return;
    const img = new Image();
    img.onload = () => {
      imgRef.current = img;
      setImgReady(true);
    };
    img.src = pet.imageDataUrl;
    return () => {
      imgRef.current = null;
      setImgReady(false);
    };
  }, [isSprite, pet.imageDataUrl]);

  // Display size — scale the frame to fit the live viewport, preserving aspect.
  const displayMax = useDisplayMax();
  const display = useMemo(() => {
    if (!sheet) return { w: displayMax, h: displayMax };
    const { frameWidth: fw, frameHeight: fh } = sheet;
    const scale = displayMax / Math.max(fw, fh);
    return { w: Math.round(fw * scale), h: Math.round(fh * scale) };
  }, [sheet, displayMax]);

  // Frame stepping loop: draw the current row's frames onto the canvas.
  useEffect(() => {
    if (!isSprite || !imgReady || !sheet) return;
    const canvas = canvasRef.current;
    const img = imgRef.current;
    if (!canvas || !img) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.imageSmoothingEnabled = false;

    const rowName = stateToRow(state);
    const anim = anims.get(rowName) ?? anims.get('idle');
    if (!anim) return;
    const { frameWidth: fw, frameHeight: fh } = sheet;

    const drawFrame = (col: number) => {
      ctx.clearRect(0, 0, fw, fh);
      ctx.drawImage(img, col * fw, anim.row * fh, fw, fh, 0, 0, fw, fh);
    };

    // Reduced motion (or a single frame): hold the first frame.
    if (prefersReducedMotion() || anim.frames <= 1) {
      drawFrame(0);
      return;
    }
    let col = 0;
    drawFrame(0);
    const interval = window.setInterval(() => {
      col = (col + 1) % anim.frames;
      drawFrame(col);
    }, Math.max(1000 / Math.max(anim.fps, 1), 40));
    return () => window.clearInterval(interval);
  }, [isSprite, imgReady, sheet, anims, state]);

  // ── Idle / sleep breathing (procedural mode only — sprite frames handle it) ─
  const startBreathing = useCallback(
    (sleeping: boolean) => {
      idleAnimRef.current?.cancel();
      idleAnimRef.current = null;
      if (isSprite) return;
      const el = spriteRef.current;
      if (!canAnimate(el)) return;
      const frames: Keyframe[] = sleeping
        ? [
            { transform: 'translateY(2%) scale(1)' },
            { transform: 'translateY(3%) scale(0.99)' },
            { transform: 'translateY(2%) scale(1)' },
          ]
        : [
            { transform: 'translateY(0) scale(1)' },
            { transform: 'translateY(-1.5%) scale(1.03)' },
            { transform: 'translateY(0) scale(1)' },
          ];
      idleAnimRef.current = el.animate(frames, {
        duration: sleeping ? 5200 : 3200,
        iterations: Infinity,
        easing: 'ease-in-out',
      });
    },
    [isSprite]
  );

  // Occasional idle sway — a weighted-random variant (procedural mode only).
  useEffect(() => {
    if (isSprite || state !== 'idle') return;
    if (prefersReducedMotion() || typeof Element === 'undefined') return;
    let timer: ReturnType<typeof setTimeout>;
    const scheduleSway = () => {
      const delay = 4000 + Math.random() * 6000;
      timer = setTimeout(() => {
        const el = spriteRef.current;
        if (canAnimate(el) && stateRef.current === 'idle') {
          el.animate(
            [
              { transform: 'rotate(0deg)' },
              { transform: `rotate(${Math.random() > 0.5 ? 3 : -3}deg)` },
              { transform: 'rotate(0deg)' },
            ],
            { duration: 900, easing: 'ease-in-out' }
          );
        }
        scheduleSway();
      }, delay);
    };
    scheduleSway();
    return () => clearTimeout(timer);
  }, [state, isSprite]);

  // Drive the persistent breathing loop when entering idle / sleep.
  useEffect(() => {
    if (state === 'idle') startBreathing(false);
    else if (state === 'sleep') startBreathing(true);
    else {
      idleAnimRef.current?.cancel();
      idleAnimRef.current = null;
    }
  }, [state, startBreathing]);

  // ── Idle → sleep timer ────────────────────────────────────────────────────
  const resetIdleTimer = useCallback(() => {
    if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
    idleTimerRef.current = setTimeout(() => {
      if (stateRef.current === 'idle') setState('sleep');
    }, SLEEP_AFTER_MS);
  }, []);

  useEffect(() => {
    resetIdleTimer();
    return () => {
      if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
    };
  }, [resetIdleTimer]);

  // ── External agent signals ────────────────────────────────────────────────
  // Two sources, same handler: a `pet:agent-signal` DOM CustomEvent (testable /
  // in-page) and — when hosted in Tauri — a `pet://agent-signal` app event the
  // gateway can push. Only pending-approval "notify" is wired to a real signal
  // today (via `pendingCount`); "working" awaits a live agent-status feed.
  const applySignal = useCallback((detail: PetAgentSignal | undefined) => {
    if (!detail) return;
    if (detail.state === 'working') setState('working');
    else if (detail.state === 'notify') {
      setNotifyDismissed(false);
      setState('notify');
    } else if (stateRef.current === 'working' || stateRef.current === 'notify') {
      setState('idle');
    }
  }, []);

  useEffect(() => {
    const handler = (e: Event) => applySignal((e as CustomEvent<PetAgentSignal>).detail);
    window.addEventListener('pet:agent-signal', handler);
    // Also accept the same signal pushed as a Tauri app event (host-forwarded).
    let unlisten = () => {};
    let alive = true;
    void onPetAgentSignal((p) => applySignal(p)).then((fn) => {
      if (alive) unlisten = fn;
      else fn();
    });
    return () => {
      alive = false;
      unlisten();
      window.removeEventListener('pet:agent-signal', handler);
    };
  }, [applySignal]);

  // Pending approvals raise the notify placard (CatPaw-style attention flag).
  useEffect(() => {
    if (hasPending && !notifyDismissed) {
      if (stateRef.current === 'idle' || stateRef.current === 'sleep') setState('notify');
    } else if (stateRef.current === 'notify') {
      setState('idle');
    }
  }, [hasPending, notifyDismissed]);

  // Working bob (procedural mode only; sprite mode uses the running row).
  useEffect(() => {
    if (isSprite || state !== 'working') return;
    const el = spriteRef.current;
    if (!canAnimate(el)) return;
    const anim = el.animate(
      [
        { transform: 'translateY(0)' },
        { transform: 'translateY(-4%)' },
        { transform: 'translateY(0)' },
      ],
      { duration: 700, iterations: Infinity, easing: 'ease-in-out' }
    );
    return () => anim.cancel();
  }, [state, isSprite]);

  // ── One-shot spring animations (both modes — transform the wrapper) ────────
  const playClickBounce = useCallback(() => {
    const el = spriteRef.current;
    if (!canAnimate(el)) return;
    el.animate(
      [
        { transform: 'translateY(0) scale(1)' },
        { transform: 'translateY(-14%) scale(1.06, 0.94)' },
        { transform: 'translateY(0) scale(0.96, 1.04)' },
        { transform: 'translateY(0) scale(1)' },
      ],
      { duration: 480, easing: 'cubic-bezier(0.34, 1.56, 0.64, 1)' }
    );
  }, []);

  const playLandingSpring = useCallback(() => {
    const el = spriteRef.current;
    if (!canAnimate(el)) return;
    el.animate(
      [
        { transform: 'translateY(-10%) scale(1)', offset: 0 },
        { transform: 'translateY(0) scale(1.12, 0.86)', offset: 0.3 },
        { transform: 'translateY(-6%) scale(0.96, 1.05)', offset: 0.55 },
        { transform: 'translateY(0) scale(1.04, 0.97)', offset: 0.75 },
        { transform: 'translateY(0) scale(1)', offset: 1 },
      ],
      { duration: 620, easing: 'ease-out' }
    );
  }, []);

  // ── Pointer gesture: click vs drag ────────────────────────────────────────
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0) return; // let right-click through to contextmenu
      pressRef.current = { x: e.clientX, y: e.clientY, dragging: false };
      if (stateRef.current === 'sleep') setState('idle');
      resetIdleTimer();
    },
    [resetIdleTimer]
  );

  const onPointerMove = useCallback((e: React.PointerEvent) => {
    const press = pressRef.current;
    if (!press || press.dragging) return;
    const dist = Math.hypot(e.clientX - press.x, e.clientY - press.y);
    if (dist > DRAG_THRESHOLD) {
      press.dragging = true;
      setState('drag');
      void startWindowDrag();
    }
  }, []);

  const endPress = useCallback(() => {
    const press = pressRef.current;
    pressRef.current = null;
    if (!press) return;
    if (press.dragging) {
      setState('fall');
      playLandingSpring();
      window.setTimeout(() => setState('idle'), 620);
    } else {
      setState('click');
      playClickBounce();
      onActivate?.();
      window.setTimeout(() => setState('idle'), 480);
    }
    resetIdleTimer();
  }, [onActivate, playClickBounce, playLandingSpring, resetIdleTimer]);

  // Right-click → native pet menu (收回/切換/大小/工作室).
  const onContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    void openPetContextMenu();
  }, []);

  // Click the placard: acknowledge the notification and open the app.
  const onPlacardClick = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      setNotifyDismissed(true);
      setState('idle');
      onActivate?.();
    },
    [onActivate]
  );

  const label = pet.displayName || 'pet';

  return (
    <div
      className="flex h-screen w-screen cursor-pointer select-none items-center justify-center bg-transparent"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endPress}
      onPointerCancel={endPress}
      onContextMenu={onContextMenu}
      onPointerEnter={() => {
        if (stateRef.current === 'sleep') setState('idle');
      }}
      data-pet-state={state}
      role="button"
      tabIndex={0}
      aria-label={label}
      title={label}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') onActivate?.();
      }}
    >
      <div className={hasPending && state === 'idle' ? 'dudu-hop relative' : 'relative'}>
        <div ref={spriteRef} className="will-change-transform">
          {isSprite ? (
            <canvas
              ref={canvasRef}
              width={sheet?.frameWidth}
              height={sheet?.frameHeight}
              style={{
                width: display.w,
                height: display.h,
                imageRendering: 'pixelated',
              }}
              className="pointer-events-none block drop-shadow-[0_6px_10px_rgba(0,0,0,0.28)]"
              aria-hidden="true"
            />
          ) : pet.imageDataUrl ? (
            <img
              src={pet.imageDataUrl}
              alt={label}
              draggable={false}
              style={{ maxWidth: displayMax, maxHeight: displayMax }}
              className="pointer-events-none block object-contain drop-shadow-[0_6px_10px_rgba(0,0,0,0.28)]"
            />
          ) : (
            <div className="grid size-[120px] place-items-center rounded-full bg-muted text-4xl">
              🐾
            </div>
          )}
        </div>

        {/* Notify placard (CatPaw-style attention sign) — click to acknowledge. */}
        {showPlacard && (
          <button
            type="button"
            onClick={onPlacardClick}
            aria-label={`${label}: ${pendingCount || ''}`.trim()}
            className="dudu-placard absolute -right-3 -top-8 grid place-items-center"
          >
            <svg width="46" height="42" viewBox="0 0 46 42" role="img" aria-hidden="true">
              {/* stick */}
              <rect x="21" y="22" width="4" height="18" rx="2" fill="#a8825b" />
              {/* board */}
              <rect
                x="3"
                y="2"
                width="40"
                height="26"
                rx="6"
                fill="var(--status-agent-paused, #f59e0b)"
                stroke="#fff"
                strokeWidth="2"
              />
              <text
                x="23"
                y="20"
                textAnchor="middle"
                fontSize="16"
                fill="#fff"
                fontWeight="bold"
              >
                {pendingCount > 0 ? (pendingCount > 9 ? '9+' : String(pendingCount)) : '!'}
              </text>
            </svg>
          </button>
        )}

        {hasPending && !showPlacard && (
          <span
            className="absolute -right-1 -top-1 grid min-h-[22px] min-w-[22px] place-items-center rounded-full bg-[var(--status-agent-paused)] px-1.5 text-[11px] font-bold text-white shadow-[var(--shadow-pop)] ring-2 ring-background"
            aria-hidden="true"
          >
            {pendingCount > 99 ? '99+' : pendingCount}
          </span>
        )}
      </div>
    </div>
  );
}
