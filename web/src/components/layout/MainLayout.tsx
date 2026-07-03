import { useEffect } from 'react';
import { Outlet, useLocation } from 'react-router';
import { Sidebar } from './Sidebar';
import { Header } from './Header';
import { GuidedTour } from '@/components/tour/GuidedTour';
import { TourPrompt } from '@/components/tour/TourPrompt';
import { SoftLimitBanner } from '@/components/SoftLimitBanner';
import { CommandPalette } from '@/components/CommandPalette';
import { useTourStore } from '@/stores/tour-store';
import { useSystemStore } from '@/stores/system-store';
import { useUiModeStore } from '@/stores/ui-mode-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useSidebarStore } from '@/stores/sidebar-store';

export function MainLayout() {
  const location = useLocation();
  const hydrateTour = useTourStore((s) => s.hydrate);
  const status = useSystemStore((s) => s.status);
  const fetchStatus = useSystemStore((s) => s.fetchStatus);
  const initFromEdition = useUiModeStore((s) => s.initFromEdition);
  const mode = useUiModeStore((s) => s.mode);
  const recordVisit = useCommandPaletteStore((s) => s.recordVisit);
  const mobileNavOpen = useSidebarStore((s) => s.mobileOpen);
  const closeMobileNav = useSidebarStore((s) => s.closeMobile);

  // Restore the once-per-user tour state once the user id is known.
  useEffect(() => {
    hydrateTour();
  }, [hydrateTour]);

  // System status drives the edition badge, sidebar gating, AND the default
  // shell mode — fetch it shell-wide (not just on the dashboard page) so the
  // workspace shell sees it too. (§P0.2 / §P2.2)
  useEffect(() => {
    if (!status) fetchStatus();
  }, [status, fetchStatus]);

  // Seed the default mode from the edition once known (respects an explicit
  // prior choice — see ui-mode-store).
  useEffect(() => {
    initFromEdition(status?.edition_profile);
  }, [status?.edition_profile, initFromEdition]);

  // Feed the command palette's "recent" list from real navigation, and dismiss
  // the mobile nav drawer whenever the route changes.
  useEffect(() => {
    recordVisit(location.pathname);
    closeMobileNav();
  }, [location.pathname, recordVisit, closeMobileNav]);

  // Workspace mode runs edge-to-edge (the page centers itself); the dashboard
  // keeps its roomy gutter.
  const isWorkspace = mode === 'workspace';

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
      <Sidebar />
      <div className="flex flex-1 flex-col overflow-hidden">
        <Header />
        <main className={isWorkspace ? 'flex-1 overflow-y-auto px-4' : 'flex-1 overflow-y-auto p-6'}>
          {/* Non-blocking personal-edition soft-limit hint */}
          <SoftLimitBanner />
          {/* Re-key on route change to replay the entrance reveal */}
          <div key={location.pathname} className={isWorkspace ? 'page-enter h-full' : 'page-enter'}>
            <Outlet />
          </div>
        </main>
      </div>
      <TourPrompt />
      <GuidedTour />
      <CommandPalette />
    </div>
  );
}
