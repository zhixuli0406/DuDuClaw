import { useEffect } from 'react';
import { Navigate, Outlet, useLocation } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';

/**
 * FirstRunGate — wraps the authenticated app (inside MainLayout, past the WS
 * handshake). When the install has zero agents, it forces the user to the
 * first-run onboarding at `/welcome` so a brand-new DuDuClaw is never a dead,
 * agent-less dashboard.
 *
 * Anti-flash: redirect only AFTER `agents.list` has resolved once (`loaded`),
 * never on the initial empty array. `/welcome` is mounted OUTSIDE this gate
 * (see App.tsx) so we don't redirect-loop.
 */
export function FirstRunGate() {
  const agents = useAgentsStore((s) => s.agents);
  const loaded = useAgentsStore((s) => s.loaded);
  const loading = useAgentsStore((s) => s.loading);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const location = useLocation();

  useEffect(() => {
    if (!loaded && !loading) {
      fetchAgents();
    }
  }, [loaded, loading, fetchAgents]);

  if (!loaded) {
    return (
      <div className="flex h-full items-center justify-center py-20" role="status" aria-live="polite">
        <span className="h-6 w-6 animate-spin rounded-full border-2 border-amber-500/30 border-t-amber-500" />
      </div>
    );
  }

  // `/license` is reachable during first-run: the welcome wizard's industry
  // step shows an unlock CTA that sends the operator here to install a Pro
  // license BEFORE any agent exists. Bouncing it back to /welcome made that
  // CTA a dead loop.
  if (
    agents.length === 0 &&
    location.pathname !== '/welcome' &&
    location.pathname !== '/license'
  ) {
    return <Navigate to="/welcome" replace />;
  }

  return <Outlet />;
}
