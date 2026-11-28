/**
 * HTTP helpers for the channel-identity endpoints (WP-B, 2026-08-12 IA audit
 * §2-1: the third of three unrelated "binding" concepts scattered across the
 * dashboard — dashboard user ↔ channel DM identity, the data
 * `approver_links`/`mapped_role` actually read for channel-side approvals).
 * Plain REST authenticated with the same Bearer JWT the dashboard already
 * holds, following the same pattern as `components/chat/voice-api.ts` — kept
 * out of the WS-based `@/lib/api` client because these two gateway routes are
 * plain `axum` REST, not RPC.
 */
import { useAuthStore } from '@/stores/auth-store';

/** Mirrors `duduclaw_auth::otp::ChannelIdentity` (server.rs `handle_channel_identity_list`). */
export interface ChannelIdentity {
  user_id: string;
  channel: string;
  channel_user_id: string;
  verified: boolean;
  proof_status?: 'self_confirmed' | 'admin_prefilled';
  proven_at?: string | null;
  created_at: string;
  /** `'google_account'`: a Google Chat identity recognised from the person's
   * own linked Google account (read-only here; removed by disconnecting
   * Google). Absent: a binding an administrator created. */
  source?: 'google_account';
}

function authHeader(): Record<string, string> {
  const jwt = useAuthStore.getState().jwt;
  return jwt ? { Authorization: `Bearer ${jwt}` } : {};
}

async function errorText(res: Response): Promise<string> {
  try {
    const body = await res.json();
    if (body && typeof body.error === 'string') return body.error;
  } catch {
    /* not JSON */
  }
  return `HTTP ${res.status}`;
}

/**
 * GET /api/channel-identity/list?user_id=<id> — admin-only, read-only list of
 * a dashboard user's verified channel DM identities.
 */
export async function listChannelIdentities(userId: string): Promise<ChannelIdentity[]> {
  const res = await fetch(`/api/channel-identity/list?user_id=${encodeURIComponent(userId)}`, {
    headers: authHeader(),
  });
  if (!res.ok) throw new Error(await errorText(res));
  const data = await res.json().catch(() => ({}));
  return Array.isArray(data?.identities) ? (data.identities as ChannelIdentity[]) : [];
}

/**
 * POST /api/channel-identity/bind — admin-only. Binds (and immediately
 * verifies — the admin-prefill path, WP12 T12.3) a channel DM identity to a
 * dashboard user. Fails when the identity is already verified under a
 * different user (account-takeover guard, server-side).
 */
export async function bindChannelIdentity(
  userId: string,
  channel: string,
  channelUserId: string,
): Promise<void> {
  const res = await fetch('/api/channel-identity/bind', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeader() },
    body: JSON.stringify({ user_id: userId, channel, channel_user_id: channelUserId }),
  });
  if (!res.ok) throw new Error(await errorText(res));
}

/** Channels whose administrator bindings can be removed from the dashboard
 * (mirrors `server.rs::handle_channel_unbind`, which accepts Google Chat only). */
export const UNBINDABLE_CHANNELS: ReadonlyArray<string> = ['googlechat'];

/**
 * POST /api/channel-identity/unbind — admin-only. Removes an
 * administrator-created binding of `userId` (Google Chat only).
 */
export async function unbindChannelIdentity(
  userId: string,
  channel: string,
  channelUserId: string,
): Promise<void> {
  const res = await fetch('/api/channel-identity/unbind', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeader() },
    body: JSON.stringify({ user_id: userId, channel, channel_user_id: channelUserId }),
  });
  if (!res.ok) throw new Error(await errorText(res));
}
