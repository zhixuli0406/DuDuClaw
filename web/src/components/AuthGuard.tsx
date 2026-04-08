import { useEffect } from 'react';
import { Navigate, Outlet } from 'react-router';
import { useAuthStore, type UserRole } from '@/stores/auth-store';
import { ROLE_LEVELS } from '@/lib/roles';

/**
 * AuthGuard — redirects to /login if not authenticated.
 * Uses `initialized` flag from auth store to ensure `loadFromStorage`
 * runs only once across all mounts (R2 fix for re-mount re-fetching).
 */
export function AuthGuard() {
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const initialized = useAuthStore((s) => s.initialized);
  const loadFromStorage = useAuthStore((s) => s.loadFromStorage);

  useEffect(() => {
    if (!initialized) {
      loadFromStorage();
    }
  }, [initialized, loadFromStorage]);

  if (!initialized) {
    return (
      <div className="flex h-screen items-center justify-center bg-stone-50 dark:bg-stone-950">
        <div className="h-8 w-8 animate-spin rounded-full border-4 border-amber-500 border-t-transparent" />
      </div>
    );
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
