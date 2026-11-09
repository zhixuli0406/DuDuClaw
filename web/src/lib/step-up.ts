/**
 * Dashboard step-up (password + authenticator code, five minutes, bound to
 * this session) for protected-mode Admin actions — v1.81 F5/F6e-UI.
 *
 * The proof is the migration / isolation dashboard step-up the gateway
 * already uses (`UserDb::migration_step_up`, written by
 * `isolation.connector_step_up` or `migration.confirmations.step_up`). Its
 * session-bound CSRF token is read back from `isolation.employee_list`
 * (`step_up_valid`, `csrf_token`, `mfa_enrolled`). The same token is what the
 * Admin restore RPCs take as `csrf_token` and what a P1 break-glass read or
 * decision takes as `step_up_csrf`.
 *
 * Nothing here stores the password or the code.
 */
import { client } from './ws-client';

/** 32-byte CSRF token, as the gateway serializes `[u8; 32]`. */
export type StepUpToken = number[];

export interface StepUpStatus {
  step_up_valid: boolean;
  csrf_token: StepUpToken | null;
  mfa_enrolled: boolean;
}

export function isStepUpToken(value: unknown): value is StepUpToken {
  return Array.isArray(value) && value.length === 32 && value.every((n) => Number.isInteger(n) && n >= 0 && n <= 255);
}

/** Pick the step-up fields out of any reply that carries them. */
export function readStepUpStatus(value: unknown): StepUpStatus {
  const v = (value ?? {}) as Record<string, unknown>;
  const token = isStepUpToken(v.csrf_token) ? v.csrf_token : null;
  return {
    step_up_valid: v.step_up_valid === true && token !== null,
    csrf_token: token,
    mfa_enrolled: v.mfa_enrolled === true,
  };
}

export const stepUpApi = {
  /** Current proof for this session (no password involved). */
  status: async (): Promise<StepUpStatus> => readStepUpStatus(await client.call('isolation.employee_list', {})),
  /** Prove identity again. The password and code are sent once and never kept. */
  verify: (password: string, code: string) =>
    client.call('isolation.connector_step_up', { password, code }) as Promise<{ verified: boolean }>,
};
