import { useEffect } from 'react';
import { Outlet, useLocation } from 'react-router';
import { Sidebar } from './Sidebar';
import { Header } from './Header';
import { GuidedTour } from '@/components/tour/GuidedTour';
import { TourPrompt } from '@/components/tour/TourPrompt';
import { SoftLimitBanner } from '@/components/SoftLimitBanner';
import { useTourStore } from '@/stores/tour-store';

export function MainLayout() {
  const location = useLocation();
  const hydrateTour = useTourStore((s) => s.hydrate);

  // Restore the once-per-user tour state once the user id is known.
  useEffect(() => {
    hydrateTour();
  }, [hydrateTour]);

  return (
    <div className="flex h-screen overflow-hidden">
      {/* Fixed ambient stage the glass surfaces refract */}
      <div className="app-ambient" aria-hidden="true" />
      <Sidebar />
      <div className="flex flex-1 flex-col overflow-hidden">
        <Header />
        <main className="flex-1 overflow-y-auto p-6">
          {/* Non-blocking personal-edition soft-limit hint */}
          <SoftLimitBanner />
          {/* Re-key on route change to replay the entrance reveal */}
          <div key={location.pathname} className="page-enter">
            <Outlet />
          </div>
        </main>
      </div>
      <TourPrompt />
      <GuidedTour />
    </div>
  );
}
