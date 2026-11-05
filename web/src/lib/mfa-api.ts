import { passkeyUnavailableReason } from '@/lib/passkey';
import { client } from '@/lib/ws-client';
import { ApiError, apiPost, useAuthStore, type FactorAvailability, type SessionTokens } from '@/stores/auth-store';

export interface Enrollment {
  enrollment_token: string;
  challenge_ref?: string;
  proof_required?: boolean;
  secret?: string;
  otpauth_uri?: string;
}
export interface EnrollmentInput {
  password?: string;
  enrollment_token?: string;
  proof_code?: string;
  code?: string;
}
export interface RecoveryResult { recovery_codes: string[] }
export interface MfaPolicy { degraded: boolean; last_good_enforced?: boolean | null; enforced_at?: number | null }
export interface MfaOverview {
  enforced: boolean | null;
  policy: MfaPolicy;
  users: Array<{ user_id: string; email: string; enabled: boolean; channel_bindings: Array<{
    channel: string; channel_user_id: string; verified: boolean; verified_at: number | null;
  }> }>;
}
export interface MfaStatus extends FactorAvailability { enabled: boolean; enforced: boolean; email_hint?: string }
export async function accountRequest<T>(path: string, body?: Record<string, unknown>): Promise<T> {
  const jwt = useAuthStore.getState().jwt;
  if (!jwt) throw new ApiError(401, 'Session expired');
  const response = await fetch(path, { method: body ? 'POST' : 'GET', cache: 'no-store', credentials: 'same-origin',
    headers: { Authorization: `Bearer ${jwt}`, 'Content-Type': 'application/json' },
    ...(body ? { body: JSON.stringify(body) } : {}) });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new ApiError(response.status, 'Request failed');
  return data as T;
}
export const mfaApi = {
  status: () => client.call('mfa.status') as Promise<MfaStatus>,
  begin: (input: EnrollmentInput) => client.call('mfa.enroll_begin', { ...input }) as Promise<Enrollment>,
  confirm: (input: EnrollmentInput) => client.call('mfa.enroll_confirm', { ...input }) as Promise<RecoveryResult & SessionTokens>,
  regenerate: (code: string) => client.call('mfa.recovery_regenerate', { code }) as Promise<RecoveryResult>,
  disable: (code: string, password: string) => client.call('mfa.disable', { code, password }) as Promise<{ enabled: false } & SessionTokens>,
  overview: () => client.call('mfa.overview') as Promise<MfaOverview>,
  reset: (actor: string, user_id: string, code: string) => {
    if (actor === user_id) return Promise.reject(new Error('MFA_SELF_RESET'));
    return client.call('users.mfa_reset', { user_id, code });
  },
  resetPassword: (actor: string, user_id: string, password: string) => {
    if (actor === user_id) return Promise.reject(new Error('MFA_SELF_RESET'));
    return client.call('users.mfa_reset', { user_id, password });
  },
  issueCode: (user_id: string, challenge_ref: string) => client.call('users.mfa_enrollment_code', { user_id, challenge_ref }) as Promise<{ enrollment_code: string; expires_in: number }>,
  verify: (challenge_id: string, code: string, recovery: boolean) =>
    apiPost<SessionTokens>('/api/mfa/verify', { challenge_id, [recovery ? 'recovery_code' : 'code']: code }),
  emailSend: (input: Record<string, unknown>) => apiPost<{ sent: boolean; expires_in: number }>('/api/mfa/email/send', input),
  emailVerify: (input: Record<string, unknown>) => apiPost<SessionTokens & { recovery_codes?: string[] }>('/api/mfa/email/verify', input),
  emailBegin: (password: string) => client.call('mfa.email_begin', { password }) as Promise<{ enrollment_token: string }>,
  emailConfirm: (enrollment_token: string, code: string) => client.call('mfa.email_confirm', { enrollment_token, code }) as Promise<SessionTokens & { recovery_codes?: string[] }>,
  emailRemove: (password: string) => client.call('mfa.email_remove', { password }) as Promise<SessionTokens>,
  httpBegin: (input: EnrollmentInput) => apiPost<Enrollment>('/api/mfa/enroll/begin', { ...input }),
  httpConfirm: (input: EnrollmentInput) => apiPost<SessionTokens & RecoveryResult>('/api/mfa/enroll/confirm', { ...input }),
};

// Never render raw transport errors or echo a submitted credential.
export function mfaErrorId(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 503) return 'mfa.error.unavailable';
    if (error.status === 429) return 'mfa.error.attempts';
    if (error.status === 401 || error.status === 403) return 'mfa.error.expired';
  }
  const message = error instanceof Error ? error.message : '';
  if (message === 'PASSKEY_UNAVAILABLE') return `mfa.passkey.${passkeyUnavailableReason() ?? 'unavailable'}`;
  if (message === 'PASSKEY_CANCELLED' || (error instanceof DOMException && error.name === 'NotAllowedError')) return 'mfa.passkey.cancelled';
  if (/嘗試次數|too many/i.test(message)) return 'mfa.error.attempts';
  if (/過期|失效|expired|驗證失敗/i.test(message)) return 'mfa.error.expired';
  return 'mfa.error.generic';
}
