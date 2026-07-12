import { useEffect } from 'react';
import { Outlet, useLocation } from 'react-router';
import { Sidebar } from './Sidebar';
import { Header } from './Header';
import { MobileBottomNav } from './MobileBottomNav';
import { GuidedTour } from '@/components/tour/GuidedTour';
import { TourPrompt } from '@/components/tour/TourPrompt';
import { SoftLimitBanner } from '@/components/SoftLimitBanner';
import { LicenseExpiryBanner } from '@/components/LicenseExpiryBanner';
import { CommandPalette } from '@/components/CommandPalette';
import { GrowthMount } from '@/components/growth/GrowthMount';
import { PanelProvider, PropertiesPanel, CelebrationLayer, usePanel } from '@/components/ui';
import { useTourStore } from '@/stores/tour-store';
import { useSystemStore } from '@/stores/system-store';
import { useBrandingStore } from '@/lib/branding';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useSidebarStore } from '@/stores/sidebar-store';

/**
 * AppShell — the three-pane frame (dashboard-redesign-v2 §4.1, paperclip P1):
 * left Sidebar │ center (Header breadcrumbs + Outlet) │ right PropertiesPanel.
 * The right column only occupies space when a page has injected panel content
 * (`usePanel().setPanel(...)`), so pages that don't use it read full-width. The
 * CelebrationLayer portal is mounted once here for the §6.5 moments.
 */
function AppShell() {
  const location = useLocation();
  const { content } = usePanel();
  const mobileNavOpen = useSidebarStore((s) => s.mobileOpen);
  const closeMobileNav = useSidebarStore((s) => s.closeMobile);

  return (
    <div className="flex h-screen overflow-hidden">
      {/* Fixed ambient stage the glass surfaces refract */}
      <div className="app-ambient" aria-hidden="true" />
      {/* Mobile nav drawer backdrop (below md only) */}
      {mobileNavOpen && (
        <button
          type="button"
          aria-hidden="true"
          tabIndex={-1}
          onClick={closeMobileNav}
          className="fixed inset-0 z-40 cursor-default bg-stone-900/30 backdrop-blur-[2px] md:hidden dark:bg-black/50"
        />
      )}

      {/* Left column */}
      <Sidebar />

      {/* Center column */}
      <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
        <Header />
        <main className="flex-1 overflow-y-auto p-6 pb-20 md:pb-6">
          {/* Non-blocking personal-edition soft-limit hint */}
          <SoftLimitBanner />
          {/* Proactive license-expiry warning (30/7-day window + expired) */}
          <LicenseExpiryBanner />
          {/* Re-key on route change to replay the entrance reveal */}
          <div key={location.pathname} className="page-enter">
            <Outlet />
          </div>
        </main>
      </div>

      {/* Right column — mounts only when a page supplies panel content. Renders
          the collapsible 320px desktop column and the mobile bottom sheet. */}
      {content && <PropertiesPanel />}

      <TourPrompt />
      <GuidedTour />
      <CommandPalette />
      {/* Zone A quick access on small screens (§4.3) */}
      <MobileBottomNav />
      {/* Global celebration portal (§6.5). Reduced-motion → calm toast. */}
      <CelebrationLayer />
      {/* Gamification driver (V10): one shared growth.snapshot poll + the
          once-per-day settlement dialog. Renders no visible chrome itself. */}
      <GrowthMount />
    </div>
  );
}

export function MainLayout() {
  const location = useLocation();
  const hydrateTour = useTourStore((s) => s.hydrate);
  const status = useSystemStore((s) => s.status);
  const fetchStatus = useSystemStore((s) => s.fetchStatus);
  const recordVisit = useCommandPaletteStore((s) => s.recordVisit);
  const closeMobileNav = useSidebarStore((s) => s.closeMobile);
  const fetchBranding = useBrandingStore((s) => s.fetch);

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

  // Feed the command palette's "recent" list from real navigation, and dismiss
  // the mobile nav drawer whenever the route changes.
  useEffect(() => {
    recordVisit(location.pathname);
    closeMobileNav();
  }, [location.pathname, recordVisit, closeMobileNav]);

  return (
    <PanelProvider>
      <AppShell />
    </PanelProvider>
  );
}
