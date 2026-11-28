/** Preserve the server's plain Chinese refusal, never technical identifiers or English errors. */
export function settingsRefusal(error: unknown, fallback: string): string {
  const message = typeof error === 'string' ? error
    : error && typeof error === 'object' && 'message' in error && typeof error.message === 'string' ? error.message : '';
  const plain = message.trim();
  return /\p{Script=Han}/u.test(plain) && /^[\p{Script=Han}\p{N}\p{P}\p{Z}\s]+$/u.test(plain.replace(/\bAI\b/g, ''))
    ? plain : fallback;
}
