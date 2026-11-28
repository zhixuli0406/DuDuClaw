/** Preserve the server's plain Chinese refusal, never technical identifiers or English errors. */
export function settingsRefusal(error: unknown, fallback: string): string {
  const message = typeof error === 'string' ? error
    : error && typeof error === 'object' && 'message' in error && typeof error.message === 'string' ? error.message : '';
  const plain = message.trim();
  return /\p{Script=Han}/u.test(plain) && /^[\p{Script=Han}\p{N}\p{P}\p{Z}\s]+$/u.test(plain.replace(/\b(?:AI|Google|Odoo)\b/g, ''))
    ? plain : fallback;
}

/** CAS refusals require a fresh settings load before editing can resume. */
export function isSettingsConflict(message: string): boolean {
  return message === '設定頁的資料已過期，請重新整理後再儲存。'
    || message === '設定已被其他人變更，請重新整理後再儲存。';
}

/** Only structured, user-facing warning messages may be displayed. */
export function settingsWarningMessage(value: unknown, fallback: string): string {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return fallback;
  const warning = value as Record<string, unknown>;
  return typeof warning.code === 'string' && typeof warning.message === 'string'
    ? warning.message : fallback;
}
