/**
 * Platform layer — auth/session (N-1, `DESIGN-agent-os-native-apps-2026-08.md`
 * §2 L2). Single import path for the auth store + its supporting types/helpers.
 * See `connection.ts` for why this is a re-export barrel rather than a move.
 */
export {
  useAuthStore,
  ApiError,
  isRetryableAuthError,
  resetLocalSessionProbe,
  type UserRole,
  type AuthUser,
  type AgentBinding,
} from '@/stores/auth-store';
