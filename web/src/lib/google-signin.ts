import { client } from './ws-client';

export interface GoogleSigninSettings {
  enabled: boolean;
  enabled_requested: boolean;
  allowed_hd: string[];
  approval: 'admin' | 'direct';
  daily_new_accounts: number;
  avatar_template: string | null;
  avatar_template_revision: number | null;
  approval_grants_mfa_enrollment: boolean;
  ack: { by: string; at: string } | null;
}
export interface GoogleSigninStatus {
  settings: GoogleSigninSettings;
  redirect_uri: string | null;
  checks: {
    client_configured: boolean;
    public_url_ok: boolean;
    domains_set: boolean;
    mfa_policy: 'enforced' | 'not_enforced' | 'unknown';
    personal_edition: boolean;
    personal_connection_domains_cover: boolean | null;
  };
}
export type GoogleSigninForm = Pick<GoogleSigninSettings, 'enabled' | 'allowed_hd' | 'approval' | 'daily_new_accounts' | 'avatar_template' | 'approval_grants_mfa_enrollment'> & { avatar_template_revision?: number };
export const googleSignin = {
  get: () => client.call('google.signin.get', {}) as Promise<GoogleSigninStatus>,
  set: (params: GoogleSigninForm & { acknowledge_risk?: boolean; step_up_csrf?: number[] }) =>
    client.call('google.signin.set', { ...params }) as Promise<{ settings: GoogleSigninSettings }>,
};

/** Sign-in request timestamps are Unix seconds. */
export interface SigninRequest {
  id: string;
  email: string;
  display_name: string;
  hd: string;
  requested_at: number;
  expires_at: number;
}
export const signinRequests = {
  list: () => client.call('users.signin_requests.list', {}) as Promise<{ requests: SigninRequest[] }>,
  approve: (id: string, mfaPreauthorize = false) => client.call('users.signin_requests.approve', {
    id, ...(mfaPreauthorize ? { mfa_preauthorize: true } : {}),
  }) as Promise<{ user_id: string }>,
  reject: (id: string) => client.call('users.signin_requests.reject', { id }) as Promise<{ rejected: boolean }>,
};

export interface MemberGoogleSigninStatus {
  available: boolean;
  linked: boolean;
  has_password: boolean;
  hd?: string;
  email_at_link?: string;
  linked_at?: number;
}
export const memberGoogleSignin = {
  reauthStart: () => client.call('me.google_signin.reauth_start', {}) as Promise<{ authorization_url: string }>,
  reauthComplete: (ticket: string) => client.call('me.google_signin.reauth_complete', { ticket }) as Promise<Record<string, never>>,
  status: () => client.call('me.google_signin.status', {}) as Promise<MemberGoogleSigninStatus>,
  linkStart: (password: string) => client.call('me.google_signin.link_start', { password }) as Promise<{ authorization_url: string }>,
  linkComplete: (ticket: string) => client.call('me.google_signin.link_complete', { ticket }) as Promise<Record<string, never>>,
  unlink: (password: string) => client.call('me.google_signin.unlink', { password }) as Promise<{ unlinked: boolean }>,
};
