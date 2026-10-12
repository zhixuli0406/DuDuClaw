import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';

/**
 * Change the logged-in user's own email, then pick it up everywhere: the
 * access token and the WS connection both carry the email they were issued
 * with, so refresh the token (which also reloads the store's user) and redo
 * the handshake.
 */
export async function changeOwnEmail(email: string, currentPassword?: string): Promise<string> {
  const res = await api.users.changeEmail(email, currentPassword);
  if (res.status === 'unchanged') return res.email;
  await useAuthStore.getState().refresh();
  const conn = useConnectionStore.getState();
  conn.disconnect();
  await conn.connectWithAuth(() => useAuthStore.getState().jwt ?? undefined);
  return res.email;
}

/** Map a `users.change_email` rejection to an i18n message id. */
export function emailChangeErrorId(err: unknown): string {
  const msg = (err instanceof Error ? err.message : String(err ?? '')).toLowerCase();
  if (msg.includes('already in use')) return 'account.email.error.taken';
  if (msg.includes('current password')) return 'account.email.error.wrongPassword';
  if (msg.includes('email')) return 'account.email.error.invalid';
  return 'account.email.error.failed';
}
