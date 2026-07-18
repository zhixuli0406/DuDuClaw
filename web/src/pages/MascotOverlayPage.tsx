import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { DuDu } from '@/components/mascot';
import type { DuduFace } from '@/components/mascot/faces';
import { useApprovalsStore } from '@/stores/approvals-store';

/**
 * `/mascot-overlay` — the Tauri desktop-pet mini route (§7.4). Rendered inside a
 * transparent, borderless, always-on-top second window (see
 * `src-tauri/src/main.rs`). Self-contained: no app shell, no auth guard.
 *
 * Behaviour (openhuman desktop-pet parity):
 *  - Rests `sleep` (closed eyes). Hovering wakes it (`idle`); 2s after the
 *    cursor leaves it dozes back off. Native hosts can also drive this via a
 *    `mascot:hover-state` CustomEvent.
 *  - When the owner has pending items it does a gentle "notice me" hop and wears
 *    a small badge; hovering then reads `curious` instead of `idle`.
 *  - Clicking opens/focuses the main window (Tauri command in the desktop shell;
 *    a plain navigation in a browser).
 *  - The whole surface is a drag region (`data-tauri-drag-region`) so the pet
 *    can be moved around the desktop.
 */

const SLEEP_DELAY_MS = 2000;

/** Open (or focus) the main dashboard window across desktop / web. */
function openMainWindow(): void {
  const w = window as unknown as {
    __TAURI__?: { core?: { invoke?: (cmd: string) => Promise<unknown> } };
  };
  const invoke = w.__TAURI__?.core?.invoke;
  if (typeof invoke === 'function') {
    void invoke('open_main_window').catch(() => {
      /* fall through to a plain navigation below on failure */
      window.open('/', '_blank');
    });
    return;
  }
  window.open('/', '_blank');
}

export function MascotOverlayPage() {
  const intl = useIntl();
  const pending = useApprovalsStore((s) => s.pendingCount);
  const [awake, setAwake] = useState(false);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const wake = useCallback(() => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
    setAwake(true);
  }, []);

  const dozeSoon = useCallback(() => {
    timeoutRef.current = setTimeout(() => {
      setAwake(false);
      timeoutRef.current = null;
    }, SLEEP_DELAY_MS);
  }, []);

  // Native host bridge: a `mascot:hover-state` CustomEvent can drive wake/doze
  // for hosts that render the pet click-through (cursor-passthrough panels).
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<{ hovering?: boolean }>).detail;
      if (detail?.hovering) wake();
      else dozeSoon();
    };
    window.addEventListener('mascot:hover-state', handler);
    return () => {
      window.removeEventListener('mascot:hover-state', handler);
      if (timeoutRef.current) clearTimeout(timeoutRef.current);
    };
  }, [wake, dozeSoon]);

  const hasPending = pending > 0;
  const face: DuduFace = awake ? (hasPending ? 'curious' : 'idle') : 'sleep';
  // Hop only when something is waiting and the pet is dozing (don't fidget while
  // the owner is actively hovering). The `dudu-hop` class is inert under
  // reduced-motion (gated in index.css).
  const hop = hasPending && !awake;

  return (
    <div
      data-tauri-drag-region
      data-face={face}
      className="flex h-screen w-screen cursor-pointer items-center justify-center bg-transparent"
      onMouseEnter={wake}
      onMouseLeave={dozeSoon}
      onClick={openMainWindow}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') openMainWindow();
      }}
      role="button"
      tabIndex={0}
      aria-label={intl.formatMessage({ id: 'mascot.overlay.label' })}
      title={intl.formatMessage({
        id: hasPending ? 'mascot.overlay.pending' : 'mascot.overlay.label',
      })}
    >
      <div className={hop ? 'dudu-hop relative' : 'relative'}>
        <DuDu face={face} size="lg" />
        {hasPending && (
          <span
            className="absolute -right-1 -top-1 grid min-h-[22px] min-w-[22px] place-items-center rounded-full bg-[var(--status-agent-paused)] px-1.5 text-[11px] font-bold text-white shadow-[var(--shadow-pop)] ring-2 ring-background"
            aria-hidden="true"
          >
            {pending > 99 ? '99+' : pending}
          </span>
        )}
      </div>
    </div>
  );
}
