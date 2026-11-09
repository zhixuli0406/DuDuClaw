/**
 * Error text for the restore / adoption / purge screens (v1.81 F5/F6e-UI).
 *
 * The gateway answers `{ code, message }` with a zh-TW `message` written for
 * people; older handlers answer a plain string. Known codes get a friendly,
 * translated sentence; the server's own message is kept beside it. A value
 * with no usable text falls back to the dashboard's `formatError`.
 */
import { formatError } from './toast';
import { sanitizeErrorDetail } from './error-message';

export const KNOWN_RESTORE_ERRORS = [
  'FACTOR_NOT_STRONGEST',
  'FACTOR_NOT_PREEXISTING',
  'FACTOR_INVALID',
  'CHALLENGE_EXPIRED',
  'CREDENTIAL_CHANGED',
  'SECOND_ADMIN_REQUIRED',
  'SELF_APPROVAL_COOLING',
  'NO_SECOND_ADMIN',
  'ADMIN_LOG_UNAVAILABLE',
  'ROOT_UNAVAILABLE',
  'ROOT_GONE',
  'ROOT_RETIRED',
  'ROOT_INCONSISTENT',
  'RESTORE_WINDOW_CLOSED',
  'REQUEST_PENDING',
  'EARLY_PURGE_CONFIRM_REQUIRED',
  'STEP_UP_REQUIRED',
  'PASSKEY_CANCELLED',
  'PASSKEY_UNAVAILABLE',
] as const;
export type KnownRestoreError = (typeof KNOWN_RESTORE_ERRORS)[number];

const CODE_RE = /^[A-Z][A-Z0-9_]{2,63}$/;

/** Machine code of a rejection, if any. */
export function errorCode(err: unknown): string | null {
  if (err == null) return null;
  if (typeof err === 'string') return CODE_RE.test(err) ? err : null;
  if (typeof err === 'object') {
    const e = err as { code?: unknown; message?: unknown };
    if (typeof e.code === 'string' && e.code) return e.code;
    if (typeof e.message === 'string' && CODE_RE.test(e.message)) return e.message;
  }
  return null;
}

export function isKnownRestoreError(code: string | null): code is KnownRestoreError {
  return code !== null && (KNOWN_RESTORE_ERRORS as readonly string[]).includes(code);
}

/** The server's human sentence (sanitized), never a bare code. */
export function serverMessage(err: unknown): string | null {
  let raw: unknown = null;
  if (typeof err === 'string') raw = err;
  else if (err && typeof err === 'object' && typeof (err as { message?: unknown }).message === 'string') {
    raw = (err as { message: string }).message;
  }
  if (typeof raw !== 'string' || !raw.trim() || CODE_RE.test(raw.trim())) return null;
  const clean = sanitizeErrorDetail(raw, 400);
  return clean || null;
}

export interface DescribedError {
  code: string | null;
  /** i18n id of the friendly sentence, when the code is known. */
  friendlyId: string | null;
  /** The server's own words (zh-TW), when present. */
  detail: string | null;
  /** Fallback one-liner when there is neither. */
  fallback: string;
}

export function describeRestoreError(err: unknown): DescribedError {
  const code = errorCode(err);
  const friendlyId = isKnownRestoreError(code) ? `restoreFlow.error.${code}` : null;
  const detail = serverMessage(err);
  return { code, friendlyId, detail, fallback: friendlyId || detail ? '' : formatError(err) };
}

/** The owner must ask an Admin: their factor cannot prove this request. */
export function needsAdminHelp(code: string | null): boolean {
  return code === 'FACTOR_NOT_PREEXISTING' || code === 'CREDENTIAL_CHANGED';
}
