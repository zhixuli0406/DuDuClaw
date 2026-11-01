import { client } from '@/lib/ws-client';
import { ApiError, apiPost, type SessionTokens } from '@/stores/auth-store';

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
export const mfaApi = {
  status: () => client.call('mfa.status') as Promise<{ enabled: boolean; enforced: boolean }>,
  begin: (input: EnrollmentInput) => client.call('mfa.enroll_begin', { ...input }) as Promise<Enrollment>,
  confirm: (input: EnrollmentInput) => client.call('mfa.enroll_confirm', { ...input }) as Promise<RecoveryResult & SessionTokens>,
  regenerate: (code: string) => client.call('mfa.recovery_regenerate', { code }) as Promise<RecoveryResult>,
  disable: (code: string, password: string) => client.call('mfa.disable', { code, password }) as Promise<{ enabled: false } & SessionTokens>,
  overview: () => client.call('mfa.overview') as Promise<MfaOverview>,
  reset: (actor: string, user_id: string, code: string) => {
    if (actor === user_id) return Promise.reject(new Error('MFA_SELF_RESET'));
    return client.call('users.mfa_reset', { user_id, code });
  },
  issueCode: (user_id: string, challenge_ref: string) => client.call('users.mfa_enrollment_code', { user_id, challenge_ref }) as Promise<{ enrollment_code: string; expires_in: number }>,
  verify: (challenge_id: string, code: string, recovery: boolean) =>
    apiPost<SessionTokens>('/api/mfa/verify', { challenge_id, [recovery ? 'recovery_code' : 'code']: code }),
  httpBegin: (input: EnrollmentInput) => apiPost<Enrollment>('/api/mfa/enroll/begin', { ...input }),
  httpConfirm: (input: EnrollmentInput) => apiPost<SessionTokens & RecoveryResult>('/api/mfa/enroll/confirm', { ...input }),
};

// Never render raw transport errors or echo a submitted credential.
export function mfaErrorId(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 429) return 'mfa.error.attempts';
    if (error.status === 401 || error.status === 403) return 'mfa.error.expired';
  }
  const message = error instanceof Error ? error.message : '';
  if (/嘗試次數|too many/i.test(message)) return 'mfa.error.attempts';
  if (/過期|失效|expired|驗證失敗/i.test(message)) return 'mfa.error.expired';
  return 'mfa.error.generic';
}
