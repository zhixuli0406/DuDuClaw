import { settingsRefusal } from '@/lib/settings-refusal';
import { client } from '@/lib/ws-client';
import { passkeyUnavailableReason } from '@/lib/passkey';
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
  users: Array<{ user_id: string; email: string; admin?: boolean; enabled: boolean; channel_bindings: Array<{
    channel: string; channel_user_id: string; verified: boolean; verified_at: number | null;
  }> }>;
  /** Resets in the last 30 days whose notice reached the person by no route. */
  reset_notice_failures?: Array<{ user_id: string; at: string }>;
}
/** Stable refusal codes of `users.mfa_reset` (shown through `mfa.reset.error.*`). */
export const MFA_RESET_ERRORS = [
  'MFA_RESET_SECOND_FACTOR_REQUIRED', 'MFA_RESET_USE_AUTHENTICATOR', 'MFA_RESET_NO_SECOND_FACTOR',
  'MFA_RESET_FACTOR_FAILED', 'MFA_RESET_EMAIL_UNAVAILABLE', 'MFA_RESET_RATE_LIMITED', 'MFA_RESET_COOLING',
  'MFA_RESET_ADMIN_LOG_UNAVAILABLE', 'MFA_RESET_TARGET_UNAVAILABLE', 'MFA_RESET_NOT_ADMIN', 'MFA_RESET_SELF',
  'MFA_RESET_INVALID', 'MFA_RESET_UNAVAILABLE',
] as const;
export interface MfaStatus extends FactorAvailability {
  enabled: boolean;
  enforced: boolean;
  email_hint?: string;
  /** Admin-only SAS of the installation key when the relay awaits console approval. */
  relay_key_fingerprint?: string | null;
}
export async function accountRequest<T>(path: string, body?: Record<string, unknown>): Promise<T> {
  const jwt = useAuthStore.getState().jwt;
  if (!jwt) throw new ApiError(401, 'Session expired');
  const response = await fetch(path, { method: body ? 'POST' : 'GET', cache: 'no-store', credentials: 'same-origin',
    headers: { Authorization: `Bearer ${jwt}`, 'Content-Type': 'application/json' },
    ...(body ? { body: JSON.stringify(body) } : {}) });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new ApiError(response.status, settingsRefusal(data.error ?? data, 'Request failed'));
  return data as T;
}
export const mfaApi = {
  status: () => client.call('mfa.status') as Promise<MfaStatus>,
  begin: (input: EnrollmentInput) => client.call('mfa.enroll_begin', { ...input }) as Promise<Enrollment>,
  confirm: (input: EnrollmentInput) => client.call('mfa.enroll_confirm', { ...input }) as Promise<RecoveryResult & SessionTokens>,
  regenerate: (code: string) => client.call('mfa.recovery_regenerate', { code }) as Promise<RecoveryResult>,
  disable: (code: string, password = '') => client.call('mfa.disable', { code, password }) as Promise<{ enabled: false } & SessionTokens>,
  overview: () => client.call('mfa.overview') as Promise<MfaOverview>,
  reset: (actor: string, user_id: string, code: string) => {
    if (actor === user_id) return Promise.reject(new Error('MFA_SELF_RESET'));
    return client.call('users.mfa_reset', { user_id, code });
  },
  resetPassword: (actor: string, user_id: string, password: string) => {
    if (actor === user_id) return Promise.reject(new Error('MFA_SELF_RESET'));
    return client.call('users.mfa_reset', { user_id, password });
  },
  /** No authenticator app (passkey or email only): password, then a code mailed to the actor. */
  resetEmailBegin: (actor: string, user_id: string, password: string) => {
    if (actor === user_id) return Promise.reject(new Error('MFA_SELF_RESET'));
    return client.call('users.mfa_reset', { user_id, password, send_email_code: true }) as Promise<{ email_code_sent: boolean; reset_token: string; expires_in: number }>;
  },
  resetEmailConfirm: (actor: string, user_id: string, reset_token: string, email_code: string) => {
    if (actor === user_id) return Promise.reject(new Error('MFA_SELF_RESET'));
    return client.call('users.mfa_reset', { user_id, reset_token, email_code });
  },
  /** First administrator, first-time setup: the one-time code from `duduclaw ops users mfa-setup-code` (always required). */
  bootstrap: (enrollment_token: string, setup_code: string) =>
    apiPost<{ bootstrap: boolean }>('/api/mfa/enroll/bootstrap', { enrollment_token, setup_code }),
  issueCode: (user_id: string, challenge_ref: string) => client.call('users.mfa_enrollment_code', { user_id, challenge_ref }) as Promise<{ enrollment_code: string; expires_in: number }>,
  verify: (challenge_id: string, code: string, recovery: boolean) =>
    apiPost<SessionTokens>('/api/mfa/verify', { challenge_id, [recovery ? 'recovery_code' : 'code']: code }),
  emailSend: async (input: Record<string, unknown>) => {
    try { return await apiPost<{ sent: boolean; expires_in: number }>('/api/mfa/email/send', input); }
    catch (error) {
      if (error instanceof ApiError && error.status === 429) throw new Error('MFA_EMAIL_RATE_LIMITED');
      throw error;
    }
  },
  emailVerify: (input: Record<string, unknown>) => apiPost<SessionTokens & { recovery_codes?: string[] }>('/api/mfa/email/verify', input),
  emailBegin: (input: EnrollmentInput) => client.call('mfa.email_begin', { ...input }) as Promise<Enrollment & { sent?: boolean }>,
  emailConfirm: (enrollment_token: string, code: string, proof_code?: string) => client.call('mfa.email_confirm', { enrollment_token, code, ...(proof_code ? { proof_code } : {}) }) as Promise<SessionTokens & { recovery_codes?: string[] }>,
  emailRemove: (password = '') => client.call('mfa.email_remove', { password }) as Promise<SessionTokens>,
  httpBegin: (input: EnrollmentInput) => apiPost<Enrollment>('/api/mfa/enroll/begin', { ...input }),
  httpConfirm: (input: EnrollmentInput) => apiPost<SessionTokens & RecoveryResult>('/api/mfa/enroll/confirm', { ...input }),
};

// Never render raw transport errors or echo a submitted credential.
export function mfaErrorId(error: unknown): string {
  if (error && typeof error === 'object' && !(error instanceof Error)) {
    const code = (error as { code?: unknown }).code;
    if (typeof code === 'string' && (MFA_RESET_ERRORS as readonly string[]).includes(code)) return `mfa.reset.error.${code}`;
  }
  if (error instanceof ApiError) {
    if (error.status === 503) return 'mfa.error.unavailable';
    if (error.status === 429) return 'mfa.error.attempts';
    if (error.status === 401 || error.status === 403) return 'mfa.error.expired';
  }
  const message = error instanceof Error ? error.message : '';
  if (message === 'MFA_EMAIL_RATE_LIMITED' || /寄送太頻繁/.test(message)) return 'mfa.email.rateLimited';
  if (message === 'PASSKEY_UNAVAILABLE') return `mfa.passkey.${passkeyUnavailableReason() ?? 'unavailable'}`;
  if (message === 'PASSKEY_CANCELLED' || (error instanceof DOMException && error.name === 'NotAllowedError')) return 'mfa.passkey.cancelled';
  if (/嘗試次數|too many/i.test(message)) return 'mfa.error.attempts';
  if (/過期|失效|expired|驗證失敗/i.test(message)) return 'mfa.error.expired';
  return 'mfa.error.generic';
}
