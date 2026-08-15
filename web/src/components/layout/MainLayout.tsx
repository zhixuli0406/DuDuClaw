import { Suspense, useEffect } from 'react';
import { NavLink, Outlet, useLocation, useNavigate } from 'react-router';
import { AppSidebar } from './AppSidebar';
import { crumbsFor } from './nav-model';
import { MobileBottomNav } from './MobileBottomNav';
import { GuidedTour } from '@/components/tour/GuidedTour';
import { TourPrompt } from '@/components/tour/TourPrompt';
import { SoftLimitBanner } from '@/components/SoftLimitBanner';
import { LicenseExpiryBanner } from '@/components/LicenseExpiryBanner';
import { CommandPalette } from '@/components/CommandPalette';
import { AssignSheet } from '@/components/workspace/AssignSheet';
import { DevPanel } from '@/components/devpanel';
import { GrowthMount } from '@/components/growth/GrowthMount';
import { PanelProvider, PropertiesPanel, CelebrationLayer, usePanel } from '@/components/ui';
import {
  SidebarProvider,
  Sidebar,
  SidebarInset,
  SidebarTrigger,
  NavProgress,
} from '@/components/mds';
import { ArrowUpCircle, Bell, X } from 'lucide-react';
import { useIntl } from 'react-intl';
import { useApprovalsStore } from '@/stores/approvals-store';
import { useTourStore } from '@/stores/tour-store';
import { useSystemStore } from '@/stores/system-store';
import { useBrandingStore, useEffectiveName } from '@/lib/branding';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useNavProgressStore } from '@/stores/nav-progress-store';
import { useUpdateStore } from '@/stores/update-store';

/**
 * Mobile inbox bell (2026-08-04, D17). Silent while nothing is pending — an
 * always-on icon in a 12px-tall bar is just furniture; one that appears when
 * there is a decision waiting is a signal.
 */
function MobileInboxBell() {
  const intl = useIntl();
  const count = useApprovalsStore((s) => s.pendingCount);
  if (count <= 0) return null;
  const label = intl.formatMessage({ id: 'nav.inbox.pending' }, { count });
  return (
    <NavLink
      to="/inbox"
      aria-label={label}
      title={label}
      className="relative ml-auto grid size-8 shrink-0 place-items-center rounded-md text-brand outline-none transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring/50"
    >
      <Bell className="size-4" />
      <span className="absolute -right-0.5 -top-0.5 inline-flex min-w-4 items-center justify-center rounded-full bg-brand px-1 text-[9px] font-medium leading-none tabular-nums text-brand-foreground">
        {count > 99 ? '99+' : count}
      </span>
    </NavLink>
  );
}

/**
 * Route-transition fallback. Flips the NavProgress store on while a lazy page
 * chunk loads (reflecting *real* pending navigation, spec §3), and shows a light
 * spinner in the content area. The shell around it stays mounted because this
 * boundary sits inside `SidebarInset`, closer than the app-level Suspense.
 */
function RouteFallback() {
  const setActive = useNavProgressStore((s) => s.setActive);
  useEffect(() => {
    setActive(true);
    return () => setActive(false);
  }, [setActive]);
  return (
    <div className="flex flex-1 items-center justify-center py-20" role="status" aria-live="polite">
      <span className="size-6 animate-spin rounded-full border-2 border-brand/30 border-t-brand" />
    </div>
  );
}

/** Slim in-flow update prompt (migrated from the former global Header). */
function UpdateBanner() {
  const intl = useIntl();
  const navigate = useNavigate();
  const notification = useUpdateStore((s) => s.notification);
  const dismissed = useUpdateStore((s) => s.dismissed);
  const restarting = useUpdateStore((s) => s.restarting);
  const dismiss = useUpdateStore((s) => s.dismiss);
  if (restarting) {
    return (
      <div className="flex items-center gap-2 rounded-lg border border-success/30 bg-success/10 px-3 py-1.5 text-xs text-success">
        <span className="size-3 animate-spin rounded-full border-2 border-success/40 border-t-success" />
        {intl.formatMessage({ id: 'update.restarting' })}
      </div>
    );
  }
  if (!notification?.available || dismissed) return null;
  return (
    <div className="flex items-center gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-1.5">
      <ArrowUpCircle className="size-4 shrink-0 text-warning" />
      <span className="text-xs text-warning">
        {intl.formatMessage({ id: 'update.notification' }, { version: notification.latest_version })}
      </span>
      <button
        onClick={() => navigate('/manage/updates')}
        className="ml-auto whitespace-nowrap rounded-md bg-warning px-2 py-0.5 text-xs font-medium text-white transition-colors hover:bg-warning/90"
      >
        {intl.formatMessage({ id: 'update.viewDetails' })}
      </button>
      <button
        onClick={dismiss}
        title={intl.formatMessage({ id: 'update.dismiss' })}
        className="rounded p-0.5 text-warning/70 transition-colors hover:bg-warning/15 hover:text-warning"
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}

/**
 * AppShell — the Multica two-pane frame (WP0.4, spec §5.1): the inset sidebar
 * island │ the page-canvas SidebarInset. The former global Header is gone — its
 * duties moved into the sidebar (search / theme / bell / cost) — so pages own
 * their own PageHeader. During the migration, pages that don't yet carry a
 * PageHeader still render inside the padded scroll container unchanged; only the
 * outer frame changed. The right PropertiesPanel column mounts only when a page
 * injects content (`usePanel().setPanel(...)`).
 */
function AppShell() {
  const location = useLocation();
  const { content } = usePanel();
  const navActive = useNavProgressStore((s) => s.active);
  const brandName = useEffectiveName();

  return (
    <SidebarProvider>
      <Sidebar variant="inset">
        <AppSidebar />
      </Sidebar>

      <SidebarInset>
        <NavProgress active={navActive} />

        {/* Mobile-only lightweight bar — the drawer trigger lives here since the
            global desktop topbar is gone and old pages have no PageHeader yet. */}
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-3 md:hidden">
          <SidebarTrigger />
          <span className="truncate text-sm font-medium text-foreground">{brandName}</span>
          {/* 收件匣 left the bottom bar on 2026-08-04 (D17). On a phone the
              drawer's bell is two taps away, so the signal lives here instead —
              and only appears when something is actually waiting. */}
          <MobileInboxBell />
        </div>

        <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
          <div className="empty:hidden [&>*+*]:mt-2 [&:not(:empty)]:px-4 [&:not(:empty)]:pt-3 md:[&:not(:empty)]:px-6">
            <SoftLimitBanner />
            <LicenseExpiryBanner />
            <UpdateBanner />
          </div>
          {/* Re-key on route change to replay the entrance reveal */}
          <div key={location.pathname} className="page-enter flex flex-1 flex-col p-4 pb-20 md:p-6 md:pb-6">
            <Suspense fallback={<RouteFallback />}>
              <Outlet />
            </Suspense>
          </div>
        </div>
      </SidebarInset>

      {/* Right column — mounts only when a page supplies panel content. */}
      {content && <PropertiesPanel />}

      <TourPrompt />
      <GuidedTour />
      <CommandPalette />
      {/* The one 交辦 panel (UX plan I-1a). Mounted once here so every entry
          point — sidebar, mobile ＋, task board, /goals, agent cards — opens
          the same surface instead of three different create forms. */}
      <AssignSheet />
      {/* W3-4 developer panel (Stripe Workbench pattern E1/E2) — `~`-summoned,
          manager+ only (gated inside the component itself). */}
      <DevPanel />
      {/* Zone A quick access on small screens (§4.3) */}
      <MobileBottomNav />
      {/* Global celebration portal (§6.5). Reduced-motion → calm toast. */}
      <CelebrationLayer />
      {/* Gamification driver (V10): one shared growth.snapshot poll + the
          once-per-day settlement dialog. Renders no visible chrome itself. */}
      <GrowthMount />
    </SidebarProvider>
  );
}

export function MainLayout() {
  const location = useLocation();
  const intl = useIntl();
  const brandName = useEffectiveName();
  const hydrateTour = useTourStore((s) => s.hydrate);
  const status = useSystemStore((s) => s.status);
  const fetchStatus = useSystemStore((s) => s.fetchStatus);
  const recordVisit = useCommandPaletteStore((s) => s.recordVisit);
  const fetchBranding = useBrandingStore((s) => s.fetch);

  // Router-level `document.title` sync (UX audit phase4-manage-settings /
  // phase4-daily-work: `document.title` never changed with the route across all
  // 16+38 audited pages — `lib/branding.ts` only ever sets one static
  // "{brand} Dashboard" title, once, at branding-load time). Fixed once here
  // instead of per-page: reuse the same `crumbsFor` lookup the breadcrumb header
  // already relies on to resolve the current route's nav-model label, translate
  // it, and compose "{頁名} · {brand}". Routes `crumbsFor` doesn't recognise
  // (e.g. `/welcome`, which sits outside the nav model) fall back to the bare
  // brand name rather than a stale/wrong page name.
  useEffect(() => {
    const trail = crumbsFor(location.pathname);
    const page = trail[trail.length - 1];
    document.title = page ? `${intl.formatMessage({ id: page.labelId })} · ${brandName}` : brandName;
  }, [location.pathname, intl, brandName]);

  // Restore the once-per-user tour state once the user id is known.
  useEffect(() => {
    hydrateTour();
  }, [hydrateTour]);

  // Refresh the authoritative branding (white-label) now that we're past auth +
  // WS handshake; the cache already primed the pre-auth surfaces (LoginPage).
  useEffect(() => {
    fetchBranding();
  }, [fetchBranding]);

  // System status drives the edition badge + sidebar gating — fetch it
  // shell-wide so every page sees it.
  useEffect(() => {
    if (!status) fetchStatus();
  }, [status, fetchStatus]);

  // Feed the command palette's "recent" list from real navigation.
  useEffect(() => {
    recordVisit(location.pathname);
  }, [location.pathname, recordVisit]);

  return (
    <PanelProvider>
      <AppShell />
    </PanelProvider>
  );
}
