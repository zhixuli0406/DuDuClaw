import { client } from '@/lib/ws-client';

export const sessionAdminApi = {
  policy: () => client.call('sessions.policy') as Promise<{ days: number }>,
  updatePolicy: (days: number) => client.call('sessions.policy_update', { days }) as Promise<{ days: number }>,
  revokeUser: (user_id: string) => client.call('users.revoke_sessions', { user_id }) as Promise<{ revoked: true }>,
};
