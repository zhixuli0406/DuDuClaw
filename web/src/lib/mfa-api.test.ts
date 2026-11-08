import { afterEach, expect, it, vi } from 'vitest';
import { ApiError } from '@/stores/auth-store';
import { mfaApi, mfaErrorId } from './mfa-api';

afterEach(() => vi.unstubAllGlobals());
it('shows a delivery-specific limit message without exposing the HTTP error body', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 429, json: async () => ({ error: 'private recipient detail' }) }));
  const error = await mfaApi.emailSend({ challenge_id: 'challenge' }).catch(error => error);
  expect(mfaErrorId(error)).toBe('mfa.email.rateLimited');
  expect(error.message).not.toContain('private recipient detail');
});
it('maps the relay RPC rate limit while keeping verification attempt limits distinct', () => {
  expect(mfaErrorId(new Error('寄送太頻繁，請稍後再試。'))).toBe('mfa.email.rateLimited');
  expect(mfaErrorId(new ApiError(429, 'Too many attempts'))).toBe('mfa.error.attempts');
});

it('uses the browser reason for a WebAuthn availability error', () => {
  vi.stubGlobal('isSecureContext', false);
  expect(mfaErrorId(new Error('PASSKEY_UNAVAILABLE'))).toBe('mfa.passkey.insecure_context');
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('PublicKeyCredential', undefined);
  expect(mfaErrorId(new Error('PASSKEY_UNAVAILABLE'))).toBe('mfa.passkey.unsupported_browser');
});
