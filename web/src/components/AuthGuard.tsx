import { useEffect } from 'react';
import { Navigate, Outlet } from 'react-router';
import { FormattedMessage } from 'react-intl';
import { WifiOff } from 'lucide-react';
import { useAuthStore, type UserRole } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import { ErrorState } from '@/components/mds/error-state';
import { ROLE_LEVELS } from '@/lib/roles';

/** The first-paint / first-handshake loading spinner. Kept bare on purpose —
 *  it appears for at most a moment while the stored session resolves and the WS
 *  completes its first handshake. `role="status"` (matching App.tsx's
 *  PageFallback) lets assistive tech announce the wait without adding copy. */
function ConnectingSpinner() {
  return (
    <div
      role="status"
      aria-live="polite"
      className="flex h-screen items-center justify-center bg-background"
    >
      <div className="h-8 w-8 animate-spin rounded-full border-4 border-brand border-t-transparent" />
    </div>
  );
}

/**
 * AuthGuard — redirects to /login if not authenticated.
 * Uses `initialized` flag from auth store to ensure `loadFromStorage`
 * runs only once across all mounts (R2 fix for re-mount re-fetching).
 *
 * Also gates rendering on WebSocket reaching `authenticated` state.
 * Without this, page-level useEffects fire before App.tsx's connect effect
 * (React commits child effects before parent effects), causing
 * `client.call(...)` to fast-reject "Not connected" on first reload.
 *
 * That WS gate must NOT collapse a mid-session disconnect into the same silent
 * spinner: an authenticated user whose gateway drops away would otherwise see
 * an endless, text-less spinner covering the whole route tree — hiding every
 * page's own ErrorState for the single most common failure ("the server went
 * down"). So we split the WS-not-ready case by `everAuthenticated`: a *first*
 * handshake (never connected yet) keeps the brief spinner; a drop *after* a
 * live connection shows a reconnecting banner with plain-language copy and a
 * manual retry.
 */
export function AuthGuard() {
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const initialized = useAuthStore((s) => s.initialized);
  const loadFromStorage = useAuthStore((s) => s.loadFromStorage);
  const wsState = useConnectionStore((s) => s.state);
  const everAuthenticated = useConnectionStore((s) => s.everAuthenticated);
  const connectWithAuth = useConnectionStore((s) => s.connectWithAuth);

  useEffect(() => {
    if (!initialized) {
      loadFromStorage();
    }
  }, [initialized, loadFromStorage]);

  // 1. Auth store still resolving the stored refresh-token → first-paint spinner.
  if (!initialized) {
    return <ConnectingSpinner />;
  }

  // 2. Authenticated user, but the WS handshake isn't `authenticated` (yet).
  if (isAuthenticated && wsState !== 'authenticated') {
    // A drop AFTER a live connection: the gateway went away or the network
    // blipped. Say so, and offer a manual retry (the client auto-reconnects,
    // but this is the way out once it has given up). Never a bare spinner here.
    if (everAuthenticated) {
      const retry = () =>
        connectWithAuth(() => useAuthStore.getState().jwt ?? undefined);
      return (
        <div className="flex h-screen items-center justify-center bg-background p-6">
          <ErrorState
            variant="inline"
            icon={WifiOff}
            title={<FormattedMessage id="auth.disconnected.title" />}
            description={<FormattedMessage id="auth.disconnected.body" />}
            retryLabel={<FormattedMessage id="auth.disconnected.retry" />}
            onRetry={retry}
            className="w-full max-w-md"
          />
        </div>
      );
    }
    // First handshake, never connected before: a brief, expected wait.
    return <ConnectingSpinner />;
  }

  if (!isAuthenticated) {
    return <Navigate to="/login" replace />;
  }

  return <Outlet />;
}

/**
 * RoleGuard — redirects to / if user lacks the required role.
 * Must be nested inside AuthGuard.
 */
export function RoleGuard({ minRole }: { minRole: UserRole }) {
  const userRole = useAuthStore((s) => s.user?.role);
  if (!userRole || ROLE_LEVELS[userRole] < ROLE_LEVELS[minRole]) {
    return <Navigate to="/" replace />;
  }
  return <Outlet />;
}

/**
 * EditionGuard — the route-level twin of `nav-visibility`'s `enterprise` flag
 * (10-ia-scatter-audit D10-B). The nav already hides Enterprise-only surfaces
 * from a Personal instance, but hiding a row does not close a route: typing the
 * URL (or following an old bookmark, or a legacy alias that was never gated at
 * all) still rendered pages such as the OEM licence-issuing console.
 *
 * Deliberately narrower than `isVisible`: it gates ONLY `enterprise` surfaces
 * — features a Personal instance genuinely does not have. `personalHidden`
 * surfaces (可靠性 / 授權 — 安全 / 日誌 dropped this gate under D9,
 * 09-edition-split-features.md §4) exist on every edition and are merely
 * kept out of the simplified IA; `ManageShell` documents that those stay
 * URL-reachable on purpose, and this guard does not change that.
 *
 * Reads the same edition source as every other consumer
 * (`system.status.edition_profile`) — no second edition mechanism. An
 * unresolved status reads as Enterprise, matching `isVisible`'s documented
 * behaviour and avoiding a redirect flash before `system.status` lands.
 *
 * Front-end only. The gateway RPC layer is the real gate (WP11-T11.6).
 */
export function EditionGuard({ enterprise = false }: { enterprise?: boolean }) {
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  if (enterprise && isPersonal) {
    return <Navigate to="/" replace />;
  }
  return <Outlet />;
}
