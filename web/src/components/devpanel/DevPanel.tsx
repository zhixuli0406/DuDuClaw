import { useCallback, useEffect, useRef } from 'react';
import { useIntl } from 'react-intl';
import {
  BellRing,
  GripHorizontal,
  Minus,
  Radio,
  ServerCog,
  SquareTerminal,
  X,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { hasMinRole } from '@/lib/roles';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { api } from '@/lib/api';
import {
  useDevPanelStore,
  hasUnseenCritical,
  clampDevPanelHeight,
  type DevPanelTab,
} from '@/stores/devpanel-store';
import { latestCriticalTimestamp } from '@/lib/devpanel-alerts';
import { useFocusTrap } from '@/hooks/useFocusTrap';
import { Tabs, TabsList, TabsTab } from '@/components/mds';
import { DevPanelEventsTab } from './DevPanelEventsTab';
import { DevPanelNotificationsTab } from './DevPanelNotificationsTab';
import { DevPanelSystemTab } from './DevPanelSystemTab';

/** How often the background alert poll checks for new critical events —
 *  independent of, and slower than, the events tab's own tail-poll, since
 *  this one runs at every visibility tier including fully collapsed
 *  (Stripe pattern E2: the taskbar/icon still carries an alert channel). */
const CRITICAL_POLL_MS = 20000;

/** Same predicate as `InboxList`'s local `isEditableTarget` (not shared —
 *  that one is component-local) — the `~` hotkey must not hijack a
 *  keystroke the user is typing into a text field. */
function isEditableTarget(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  const tag = el.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
}

/**
 * W3-4 developer panel (Stripe Workbench pattern E1/E2, 03-analogous-products
 * .md §7): a `~`-summoned bottom panel with three tabs (事件流/通知/系統) built
 * entirely on existing RPCs. Manager+ only — `circle` (non-technical) personas
 * never see it, same `hasMinRole` gate the rest of the manager-only chrome
 * uses (`ReportPage` et al.).
 *
 * Three-tier collapse: `expanded` pane (resizable height, focus-trapped,
 * Esc minimizes) → `minimized` taskbar → `collapsed` icon. The collapsed icon
 * still carries the critical-alert badge — a background poll of
 * `audit.unified_log` keeps running at every tier, not just while expanded.
 */
export function DevPanel() {
  const intl = useIntl();
  const user = useAuthStore((s) => s.user);
  const connectionState = useConnectionStore((s) => s.state);
  const canSee = hasMinRole(user?.role, 'manager');

  const visibility = useDevPanelStore((s) => s.visibility);
  const paneHeight = useDevPanelStore((s) => s.paneHeight);
  const activeTab = useDevPanelStore((s) => s.activeTab);
  const latestCriticalAt = useDevPanelStore((s) => s.latestCriticalAt);
  const lastSeenCriticalAt = useDevPanelStore((s) => s.lastSeenCriticalAt);
  const setPaneHeight = useDevPanelStore((s) => s.setPaneHeight);
  const setActiveTab = useDevPanelStore((s) => s.setActiveTab);
  const expand = useDevPanelStore((s) => s.expand);
  const minimize = useDevPanelStore((s) => s.minimize);
  const collapse = useDevPanelStore((s) => s.collapse);
  const toggleQuick = useDevPanelStore((s) => s.toggleQuick);
  const reportCriticalPoll = useDevPanelStore((s) => s.reportCriticalPoll);
  const acknowledgeCritical = useDevPanelStore((s) => s.acknowledgeCritical);

  const paneRef = useRef<HTMLDivElement>(null);
  const badgeOn = hasUnseenCritical({ latestCriticalAt, lastSeenCriticalAt });

  // Background critical-alert poll — every visibility tier, including fully
  // collapsed (pattern E2's "notification tray" survives the collapse).
  useEffect(() => {
    if (!canSee || connectionState !== 'authenticated') return;
    let cancelled = false;
    const poll = () => {
      api.audit
        .unifiedLog({ sources: ['channel_failure', 'security'], limit: 30 })
        .then((res) => {
          if (!cancelled) reportCriticalPoll(latestCriticalTimestamp(res?.events ?? []));
        })
        .catch(() => {
          /* silent — badge just holds its last known value this tick */
        });
    };
    poll();
    const id = setInterval(poll, CRITICAL_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [canSee, connectionState, reportCriticalPoll]);

  // Global `~` hotkey — never while the user is typing into a field.
  useEffect(() => {
    if (!canSee) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== '~') return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;
      e.preventDefault();
      toggleQuick();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [canSee, toggleQuick]);

  // Acknowledge critical events once the user is actually looking at the
  // events tab in the expanded pane — matches Intercom's rule that seeing an
  // item in its real view (not just a badge ticking) is what clears it.
  useEffect(() => {
    if (visibility === 'expanded' && activeTab === 'events') acknowledgeCritical();
  }, [visibility, activeTab, acknowledgeCritical]);

  useFocusTrap(paneRef, visibility === 'expanded');

  const onPaneKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        minimize();
      }
    },
    [minimize],
  );

  const onDragStart = useCallback(
    (e: React.PointerEvent) => {
      e.preventDefault();
      const startY = e.clientY;
      const startHeight = paneHeight;
      const onMove = (ev: PointerEvent) => {
        setPaneHeight(clampDevPanelHeight(startHeight + (startY - ev.clientY)));
      };
      const onUp = () => {
        window.removeEventListener('pointermove', onMove);
        window.removeEventListener('pointerup', onUp);
      };
      window.addEventListener('pointermove', onMove);
      window.addEventListener('pointerup', onUp);
    },
    [paneHeight, setPaneHeight],
  );

  if (!canSee) return null;

  if (visibility === 'collapsed') {
    return (
      <button
        type="button"
        onClick={expand}
        aria-label={intl.formatMessage({ id: 'devpanel.open' })}
        title={intl.formatMessage({ id: 'devpanel.open' })}
        className={cn(
          'fixed bottom-20 right-4 z-40 flex size-11 items-center justify-center rounded-full',
          'bg-surface-raised text-muted-foreground shadow-[var(--floating-shadow)] ring-1 ring-surface-border',
          'transition-colors hover:bg-surface-hover hover:text-foreground',
          'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
          'md:bottom-4',
          badgeOn && 'text-destructive ring-destructive/40',
        )}
      >
        <SquareTerminal className="size-5" aria-hidden="true" />
        {badgeOn && (
          <span
            aria-hidden="true"
            className="absolute -right-0.5 -top-0.5 size-2.5 rounded-full bg-destructive ring-2 ring-surface-raised"
          />
        )}
        {badgeOn && (
          <span className="sr-only">{intl.formatMessage({ id: 'devpanel.badge.critical' })}</span>
        )}
      </button>
    );
  }

  if (visibility === 'minimized') {
    return (
      <div
        role="toolbar"
        aria-label={intl.formatMessage({ id: 'devpanel.title' })}
        className={cn(
          'fixed inset-x-0 bottom-0 z-40 flex h-10 items-center gap-2 border-t border-surface-border bg-surface-raised px-3',
          'shadow-[var(--floating-shadow)] md:inset-x-auto md:bottom-4 md:right-4 md:h-11 md:rounded-xl md:border md:px-2.5',
        )}
      >
        <button
          type="button"
          onClick={expand}
          aria-label={intl.formatMessage({ id: 'devpanel.expand' })}
          title={intl.formatMessage({ id: 'devpanel.expand' })}
          className={cn(
            'flex items-center gap-1.5 rounded-md px-1.5 py-1 text-xs font-medium text-foreground',
            'hover:bg-surface-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
          )}
        >
          <SquareTerminal className={cn('size-4', badgeOn && 'text-destructive')} aria-hidden="true" />
          {intl.formatMessage({ id: 'devpanel.title' })}
          {badgeOn && (
            <span
              aria-hidden="true"
              className="size-1.5 rounded-full bg-destructive"
            />
          )}
        </button>
        <button
          type="button"
          onClick={collapse}
          aria-label={intl.formatMessage({ id: 'devpanel.collapse' })}
          title={intl.formatMessage({ id: 'devpanel.collapse' })}
          className="ml-auto rounded-md p-1 text-muted-foreground hover:bg-surface-hover hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          <X className="size-3.5" aria-hidden="true" />
        </button>
      </div>
    );
  }

  return (
    <div
      ref={paneRef}
      role="dialog"
      aria-modal="false"
      aria-label={intl.formatMessage({ id: 'devpanel.title' })}
      onKeyDown={onPaneKeyDown}
      style={{ height: `${paneHeight}px` }}
      className="fixed inset-x-0 bottom-0 z-40 flex flex-col border-t border-surface-border bg-surface-raised shadow-[var(--floating-shadow)]"
    >
      {/* Drag handle — top edge, resizes pane height. */}
      <div
        onPointerDown={onDragStart}
        className="flex h-2 shrink-0 cursor-ns-resize items-center justify-center hover:bg-surface-hover"
      >
        <GripHorizontal className="size-3.5 text-muted-foreground/50" aria-hidden="true" />
      </div>

      <div className="flex shrink-0 items-center gap-2 border-b border-surface-border px-3 py-1.5">
        <Tabs value={activeTab} onValueChange={(v) => setActiveTab(v as DevPanelTab)} variant="line">
          <TabsList>
            <TabsTab value="events">
              <Radio className="size-3.5" aria-hidden="true" />
              {intl.formatMessage({ id: 'devpanel.tab.events' })}
              {badgeOn && activeTab !== 'events' && (
                <span aria-hidden="true" className="size-1.5 rounded-full bg-destructive" />
              )}
            </TabsTab>
            <TabsTab value="notifications">
              <BellRing className="size-3.5" aria-hidden="true" />
              {intl.formatMessage({ id: 'devpanel.tab.notifications' })}
            </TabsTab>
            <TabsTab value="system">
              <ServerCog className="size-3.5" aria-hidden="true" />
              {intl.formatMessage({ id: 'devpanel.tab.system' })}
            </TabsTab>
          </TabsList>
        </Tabs>
        <span className="hidden text-[10px] text-muted-foreground/70 lg:inline">
          {intl.formatMessage({ id: 'devpanel.hotkeyHint' })}
        </span>
        <div className="ml-auto flex items-center gap-0.5">
          <button
            type="button"
            onClick={minimize}
            aria-label={intl.formatMessage({ id: 'devpanel.minimize' })}
            title={intl.formatMessage({ id: 'devpanel.minimize' })}
            className="rounded-md p-1 text-muted-foreground hover:bg-surface-hover hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <Minus className="size-3.5" aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={collapse}
            aria-label={intl.formatMessage({ id: 'devpanel.collapse' })}
            title={intl.formatMessage({ id: 'devpanel.collapse' })}
            className="rounded-md p-1 text-muted-foreground hover:bg-surface-hover hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <X className="size-3.5" aria-hidden="true" />
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {activeTab === 'events' && <DevPanelEventsTab />}
        {activeTab === 'notifications' && <DevPanelNotificationsTab />}
        {activeTab === 'system' && <DevPanelSystemTab />}
      </div>
    </div>
  );
}
