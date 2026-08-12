/**
 * ThinkingOrb — MDS primitive rendering DuDuClaw's dotted "thought orb"
 * loading/status animations on a plain 2D canvas. The particle-system math
 * itself (a from-scratch rewrite of the `thinking-orbs` MIT package's
 * algorithms — see that file's header for the full attribution) lives in
 * `./thinking-orb-engine`; this file owns the React-facing concerns: the
 * reduced-motion gate, live theming, and the canvas animation lifecycle.
 *
 * Reduced motion (web/DESIGN.md §1.7 — canvas animation is JS-driven and
 * MUST gate on `prefers-reduced-motion`): this component owns its own live
 * `matchMedia('(prefers-reduced-motion: reduce)')` gate and renders a
 * static, motion-free glyph instead of ever mounting the canvas when
 * reduced motion is preferred — the same JS-gate shape already used by
 * `components/home/WorldStagePlaceholder.tsx` (`useMediaMatch`) and
 * `components/ui/CelebrationLayer.tsx`.
 *
 * Theming: the ink color is read live from the mds `--foreground` CSS
 * custom property (never a hardcoded hex) via `getComputedStyle`, so it
 * automatically tracks the `.dark` class wherever it sits in the ancestor
 * chain — no manual theme-detection code needed.
 */
import { useEffect, useRef, useState, type RefObject } from 'react';
import { cn } from '@/lib/utils';
import { resolvePreset, type ThinkingOrbSize, type ThinkingOrbState } from './thinking-orb-engine';

export type { ThinkingOrbSize, ThinkingOrbState } from './thinking-orb-engine';

export interface ThinkingOrbProps {
  /** Which animation to show. */
  state: ThinkingOrbState;
  /** Tuned size preset — 64 or 20 CSS px (separate designs, not a scale factor). */
  size: ThinkingOrbSize;
  /**
   * Accessible label. Required — this primitive carries no built-in i18n
   * strings; the caller (e.g. `chat/ThinkingOrbIndicator`) owns
   * localization and passes the resolved text down.
   */
  label: string;
  className?: string;
  /**
   * Mark as purely decorative when an ancestor already announces the same
   * state as text (e.g. a `role="status"` pill) — omits `role`/`aria-label`
   * and sets `aria-hidden` instead, avoiding double announcement.
   * @default false
   */
  decorative?: boolean;
}

// ─────────────────────────────────────────────────────────────────────────
// React component: reduced-motion gate, theming, canvas lifecycle
// ─────────────────────────────────────────────────────────────────────────

/** Live-updating `prefers-reduced-motion` gate — independent of any browser default. */
function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() =>
    typeof window !== 'undefined' && typeof window.matchMedia === 'function'
      ? window.matchMedia('(prefers-reduced-motion: reduce)').matches
      : false,
  );

  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
    const onChange = () => setReduced(mq.matches);
    onChange();
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  return reduced;
}

function readForegroundToken(el: Element): string {
  const value = getComputedStyle(el).getPropertyValue('--foreground').trim();
  return value || 'currentColor';
}

/**
 * Live-updating ink color resolved from the mds `--foreground` CSS custom
 * property. Stored in a ref (not state) so a theme flip repaints on the
 * next animation frame without tearing down/restarting the rAF loop.
 */
function useThemedInkColor(canvasRef: RefObject<HTMLCanvasElement | null>) {
  const inkRef = useRef('currentColor');
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const update = () => {
      inkRef.current = readForegroundToken(canvas);
    };
    update();
    const mq = typeof matchMedia === 'function' ? matchMedia('(prefers-color-scheme: dark)') : null;
    mq?.addEventListener('change', update);
    let observer: MutationObserver | null = null;
    if (typeof MutationObserver !== 'undefined') {
      observer = new MutationObserver(update);
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ['class', 'data-theme'],
        subtree: true,
      });
    }
    return () => {
      mq?.removeEventListener('change', update);
      observer?.disconnect();
    };
  }, [canvasRef]);
  return inkRef;
}

/** Static, motion-free replacement painted when reduced motion is preferred. */
function StaticOrbGlyph({
  size,
  label,
  className,
  decorative,
}: {
  size: ThinkingOrbSize;
  label: string;
  className?: string;
  decorative: boolean;
}) {
  return (
    <span
      role={decorative ? undefined : 'img'}
      aria-label={decorative ? undefined : label}
      aria-hidden={decorative ? true : undefined}
      data-thinking-state="static"
      className={cn(
        'inline-flex shrink-0 items-center justify-center rounded-full border border-dashed border-muted-foreground/40',
        className,
      )}
      style={{ width: size, height: size }}
    >
      <span
        className="block rounded-full bg-muted-foreground/60"
        style={{ width: Math.max(4, size * 0.28), height: Math.max(4, size * 0.28) }}
      />
    </span>
  );
}

export function ThinkingOrb({ state, size, label, className, decorative = false }: ThinkingOrbProps) {
  const reducedMotion = usePrefersReducedMotion();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const inkRef = useThemedInkColor(canvasRef);

  useEffect(() => {
    if (reducedMotion) return;
    const canvas = canvasRef.current;
    if (!canvas) return;

    const dpr = Math.min(2, (typeof devicePixelRatio !== 'undefined' && devicePixelRatio) || 1);
    canvas.width = Math.round(size * dpr);
    canvas.height = Math.round(size * dpr);
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const { draw, speed, opts } = resolvePreset(state, size);
    const paint = (tSeconds: number) => {
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, size, size);
      draw(ctx, size, tSeconds, inkRef.current, opts);
    };

    const startedAt = performance.now();
    let rafId = 0;
    let running = false;
    const frame = () => {
      paint(((performance.now() - startedAt) / 1000) * speed);
      if (running) rafId = requestAnimationFrame(frame);
    };
    const start = () => {
      if (running) return;
      running = true;
      rafId = requestAnimationFrame(frame);
    };
    const stop = () => {
      running = false;
      cancelAnimationFrame(rafId);
    };

    paint(0);

    // Pause automatically when scrolled offscreen or the tab is hidden.
    let intersecting = true;
    const io =
      typeof IntersectionObserver !== 'undefined'
        ? new IntersectionObserver(([entry]) => {
            intersecting = entry.isIntersecting;
            if (intersecting && document.visibilityState !== 'hidden') start();
            else stop();
          })
        : null;
    io?.observe(canvas);
    const onVisibility = () => {
      if (document.visibilityState === 'hidden') stop();
      else if (intersecting) start();
    };
    document.addEventListener('visibilitychange', onVisibility);
    if (!io) start();

    return () => {
      stop();
      io?.disconnect();
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [state, size, reducedMotion, inkRef]);

  if (reducedMotion) {
    return <StaticOrbGlyph size={size} label={label} className={className} decorative={decorative} />;
  }

  return (
    <canvas
      ref={canvasRef}
      role={decorative ? undefined : 'img'}
      aria-label={decorative ? undefined : label}
      aria-hidden={decorative ? true : undefined}
      data-thinking-state="canvas"
      className={className}
      style={{ width: size, height: size, display: 'block' }}
    />
  );
}
