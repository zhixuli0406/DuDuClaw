import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { toast } from '@/lib/toast';

/**
 * CelebrationLayer — one global portal that plays the §6.5 celebration moments
 * (task done, inbox zero, achievement unlock, level-up). Fire them from
 * anywhere — including outside React — via the module-level `celebrate()` API,
 * mirroring the toast bus pattern.
 *
 * Reduced-motion contract: when the user prefers reduced motion we render NO
 * particles at all; if a caller supplied a `message`, it surfaces as a toast
 * instead so the moment is still acknowledged, just calmly.
 */

export type CelebrationKind = 'confetti' | 'badge' | 'level_up' | 'inbox_zero';

export interface CelebrationOptions {
  /** Spoken acknowledgement — shown as a toast in reduced-motion mode. */
  message?: string;
  /** Override piece count for confetti bursts. */
  pieces?: number;
}

interface CelebrationEvent extends CelebrationOptions {
  id: string;
  kind: CelebrationKind;
}

type Listener = (ev: CelebrationEvent) => void;
let listeners: ReadonlyArray<Listener> = [];

function prefersReducedMotion(): boolean {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Fire a celebration from anywhere (React or not). */
export function celebrate(kind: CelebrationKind, opts: CelebrationOptions = {}): void {
  // Reduced-motion: degrade to a calm toast (or silence) — never particles.
  if (prefersReducedMotion()) {
    if (opts.message) toast.success(opts.message);
    return;
  }
  const ev: CelebrationEvent = { id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`, kind, ...opts };
  for (const l of listeners) {
    try {
      l(ev);
    } catch {
      /* a bad listener must not break dispatch */
    }
  }
}

const CONFETTI_COLORS = [
  'var(--agent-1a)',
  'var(--agent-2b)',
  'var(--agent-4a)',
  'var(--agent-5b)',
  'var(--xp)',
  'var(--status-task-icon-done)',
];

/** Mount ONCE at the app root. Renders active celebration bursts in a portal. */
export function CelebrationLayer() {
  const [events, setEvents] = useState<CelebrationEvent[]>([]);

  useEffect(() => {
    const listener: Listener = (ev) => {
      setEvents((prev) => [...prev, ev]);
      // Bursts self-clear after their longest animation (~1.8s).
      const ttl = ev.kind === 'badge' || ev.kind === 'level_up' ? 900 : 1900;
      window.setTimeout(() => {
        setEvents((prev) => prev.filter((e) => e.id !== ev.id));
      }, ttl);
    };
    listeners = [...listeners, listener];
    return () => {
      listeners = listeners.filter((l) => l !== listener);
    };
  }, []);

  if (typeof document === 'undefined' || events.length === 0) return null;

  return createPortal(
    <div
      aria-hidden="true"
      className="pointer-events-none fixed inset-0 z-[200] overflow-hidden"
    >
      {events.map((ev) =>
        ev.kind === 'badge' || ev.kind === 'level_up' ? (
          <div key={ev.id} className="absolute inset-0 grid place-items-center">
            <span className="animate-badge-pop text-6xl">
              {ev.kind === 'level_up' ? '⬆️' : '🏆'}
            </span>
          </div>
        ) : (
          <div key={ev.id} className="absolute inset-0">
            {Array.from({ length: ev.pieces ?? (ev.kind === 'inbox_zero' ? 40 : 24) }).map((_, i) => (
              <span
                key={i}
                className="confetti-piece absolute top-0 h-2 w-2 rounded-[2px]"
                style={{
                  left: `${(i * 97) % 100}%`,
                  background: CONFETTI_COLORS[i % CONFETTI_COLORS.length],
                  animationDelay: `${(i % 8) * 60}ms`,
                }}
              />
            ))}
          </div>
        ),
      )}
    </div>,
    document.body,
  );
}
