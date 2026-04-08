import type { UserRole } from '@/stores/auth-store';

export const ROLE_LEVELS: Record<UserRole, number> = {
  admin: 3,
  manager: 2,
  employee: 1,
};

export function hasMinRole(
  userRole: UserRole | undefined,
  minRole: UserRole | undefined,
): boolean {
  if (!minRole) return true;
  if (!userRole) return false;
  return ROLE_LEVELS[userRole] >= ROLE_LEVELS[minRole];
}
