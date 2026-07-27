import { useCallback, useEffect, useRef, useState } from 'react';
import type { PetRuntimePayload } from '@/lib/pet';
import { startWindowDrag } from '@/lib/pet';

/**
 * PetRuntime — the P0 procedural desktop-pet animator (WP-P3).
 *
 * Renders a single background-removed image and brings it to life with pure
 * WAAPI + a hand-rolled spring, no sprite sheet required. It owns the P0 state
 * machine — idle / drag / fall / click / working / notify / sleep — driven three
 * ways: weighted-random idle variants, physically hard-chained press→drag→
 * release→fall, and external events (agent status, pending approvals).
 *
 * Drag uses a gesture (mousedown + move > {@link DRAG_THRESHOLD}px → native
 * window drag), NOT `data-tauri-drag-region`, so a plain click still registers
 * (Tauri #9751/#9901). All motion is gated on `prefers-reduced-motion`.
 */

/** Movement (px) before a press becomes a drag rather than a click. */
const DRAG_THRESHOLD = 4;
/** Idle time (ms) with no interaction before the pet dozes off. */
const SLEEP_AFTER_MS = 60_000;

export type PetState =
  | 'idle'
  | 'drag'
  | 'fall'
  | 'click'
  | 'working'
  | 'notify'
  | 'sleep';

/** External agent signal → pet reaction (P1 wires real agent events here). */
export interface PetAgentSignal {
  /** 'working' = busy animation, 'notify' = raise-a-flag, 'idle' = clear. */
  state: 'working' | 'notify' | 'idle';
}

interface PetRuntimeProps {
  pet: PetRuntimePayload;
  /** Show the notify badge / hop (e.g. pending approvals count). */
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
  const [state, setState] = useState<PetState>('idle');
  const stateRef = useRef<PetState>('idle');
  stateRef.current = state;

  // Gesture bookkeeping.
  const pressRef = useRef<{ x: number; y: number; dragging: boolean } | null>(null);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const idleAnimRef = useRef<Animation | null>(null);

  const hasPending = pendingCount > 0;

  // ── Idle / sleep breathing (WAAPI, reduced-motion aware) ──────────────────
  const startBreathing = useCallback((sleeping: boolean) => {
    idleAnimRef.current?.cancel();
    idleAnimRef.current = null;
    const el = spriteRef.current;
    if (!canAnimate(el)) return;
    // Sleep = slower, shallower, slightly drooped; idle = a gentle breath.
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
  }, []);

  // Occasional idle sway — a weighted-random variant on top of breathing.
  useEffect(() => {
    if (state !== 'idle') return;
    if (prefersReducedMotion() || typeof Element === 'undefined') return;
    let timer: ReturnType<typeof setTimeout>;
    const scheduleSway = () => {
      // Weighted: mostly wait, sometimes a small tilt.
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
  }, [state]);

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

  // ── External agent signals (P1 hook) ──────────────────────────────────────
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<PetAgentSignal>).detail;
      if (!detail) return;
      if (detail.state === 'working') setState('working');
      else if (detail.state === 'notify') setState('notify');
      else setState('idle');
    };
    window.addEventListener('pet:agent-signal', handler);
    return () => window.removeEventListener('pet:agent-signal', handler);
  }, []);

  // Working = a subtle busy bob loop.
  useEffect(() => {
    if (state !== 'working') return;
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
  }, [state]);

  // ── One-shot spring animations ────────────────────────────────────────────
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
    // Squash on impact, then a couple of decaying bounces (spring settle).
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
      // Native drag ended → play the spring landing, then rest.
      setState('fall');
      playLandingSpring();
      window.setTimeout(() => setState('idle'), 620);
    } else {
      // A tap.
      setState('click');
      playClickBounce();
      onActivate?.();
      window.setTimeout(() => setState('idle'), 480);
    }
    resetIdleTimer();
  }, [onActivate, playClickBounce, playLandingSpring, resetIdleTimer]);

  const label = pet.displayName || 'pet';

  return (
    <div
      className="flex h-screen w-screen cursor-pointer select-none items-center justify-center bg-transparent"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endPress}
      onPointerCancel={endPress}
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
      <div
        className={hasPending && state === 'idle' ? 'dudu-hop relative' : 'relative'}
      >
        <div ref={spriteRef} className="will-change-transform">
          {pet.imageDataUrl ? (
            <img
              src={pet.imageDataUrl}
              alt={label}
              draggable={false}
              className="pointer-events-none block max-h-[170px] max-w-[170px] object-contain drop-shadow-[0_6px_10px_rgba(0,0,0,0.28)]"
            />
          ) : (
            <div className="grid size-[120px] place-items-center rounded-full bg-muted text-4xl">
              🐾
            </div>
          )}
        </div>
        {hasPending && (
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
