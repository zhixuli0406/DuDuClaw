import { client } from './ws-client';
import type { StepUpToken } from './step-up';

export interface OdooMemberCredential {
  user_id: string;
  email: string;
  display_name: string;
  login: string;
  state: 'ok' | 'needs_reverify' | 'invalid';
  verified_at: string | null;
  last_used_at: string | null;
}

export const memberSignin = {
  attest: (userIds: string[], token: StepUpToken) => client.call('users.google_signin.attest', {
    user_ids: userIds, step_up_csrf: token,
  }) as Promise<{ attested: number }>,
  unlink: (userId: string) => client.call('users.google_signin.unlink', { user_id: userId }) as Promise<{ unlinked: boolean }>,
  retryAvatar: (userId: string) => client.call('users.signin_onboarding.retry_avatar', { user_id: userId }) as Promise<{ queued: boolean }>,
  credentials: () => client.call('odoo.member_credentials.list', {}) as Promise<{ credentials: OdooMemberCredential[]; enabled: boolean }>,
  setEnabled: (enabled: boolean, acknowledgeRisk = false) => client.call('odoo.member_credentials.set_enabled', { enabled, acknowledge_risk: acknowledgeRisk }) as Promise<{ enabled: boolean }>,
  revoke: (userId: string) => client.call('odoo.member_credentials.revoke', { user_id: userId }) as Promise<{ revoked: boolean }>,
};
