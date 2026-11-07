import { afterEach, expect, it, vi } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import ja from '@/i18n/ja-JP.json';
import { DisabledFactorExplanation } from './DisabledFactorExplanation';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
const catalogues = { en, 'zh-TW': zh, 'ja-JP': ja };
function explanation(locale: keyof typeof catalogues, factor: 'email' | 'passkey', reason?: string, fingerprint?: string) {
  render(<IntlProvider locale={locale} messages={catalogues[locale]}>
    <DisabledFactorExplanation factor={factor} reason={reason} fingerprint={fingerprint} />
  </IntlProvider>);
  return screen.getByRole('status');
}
it.each(['en', 'zh-TW', 'ja-JP'] as const)('explains missing SMTP with setup guidance in %s', locale => {
  const message = explanation(locale, 'email', 'no_smtp');
  expect(message).toHaveTextContent(catalogues[locale]['mfa.email.unavailable']);
  expect(message).toHaveTextContent('SMTP');
  expect(message).toHaveTextContent('[channels.email]');
});
it.each([
  ['en', 'relay_nfr', 'mfa.email.relayNfr'],
  ['zh-TW', 'relay_nfr', 'mfa.email.relayNfr'],
  ['ja-JP', 'relay_nfr', 'mfa.email.relayNfr'],
  ['en', 'relay_unbound', 'mfa.email.relayUnbound'],
  ['zh-TW', 'relay_unbound', 'mfa.email.relayUnbound'],
  ['ja-JP', 'relay_unbound', 'mfa.email.relayUnbound'],
] as const)('explains excluded license types in %s for %s', (locale, reason, id) => {
  const message = explanation(locale, 'email', reason);
  expect(message).toHaveTextContent(catalogues[locale][id]);
  expect(message).toHaveTextContent('SMTP');
  expect(message).not.toHaveTextContent(reason);
});
it.each([
  ['en', 'relay_key_unbound', 'mfa.email.relayKeyUnbound', 'license console'],
  ['zh-TW', 'relay_key_unbound', 'mfa.email.relayKeyUnbound', '授權控制台'],
  ['ja-JP', 'relay_key_unbound', 'mfa.email.relayKeyUnbound', 'ライセンスコンソール'],
  ['en', 'relay_key_mismatch', 'mfa.email.relayKeyMismatch', 'license console'],
  ['zh-TW', 'relay_key_mismatch', 'mfa.email.relayKeyMismatch', '授權控制台'],
  ['ja-JP', 'relay_key_mismatch', 'mfa.email.relayKeyMismatch', 'ライセンスコンソール'],
] as const)('asks the owner to approve the installation key in %s for %s', (locale, reason, id, place) => {
  const message = explanation(locale, 'email', reason, '1a2b-3c4d-5e6f-7a8b');
  expect(message).toHaveTextContent(catalogues[locale][id]);
  expect(message).toHaveTextContent(place);
  expect(message).toHaveTextContent('SMTP');
  expect(message).toHaveTextContent('1a2b-3c4d-5e6f-7a8b');
  expect(screen.getByTestId('relay-key-fingerprint')).toHaveTextContent(
    catalogues[locale]['mfa.email.relayKeyFingerprint'].replace('{fingerprint}', '1a2b-3c4d-5e6f-7a8b'));
  expect(message).not.toHaveTextContent(reason);
});
it('shows only a well-formed fingerprint, and only for installation key reasons', () => {
  explanation('en', 'email', 'relay_key_mismatch');
  expect(screen.queryByTestId('relay-key-fingerprint')).toBeNull();
  cleanup();
  explanation('en', 'email', 'relay_key_unbound', '<script>raw</script>');
  expect(screen.queryByTestId('relay-key-fingerprint')).toBeNull();
  expect(screen.getByRole('status')).not.toHaveTextContent('raw');
  cleanup();
  explanation('en', 'email', 'no_smtp', '1a2b-3c4d-5e6f-7a8b');
  expect(screen.queryByTestId('relay-key-fingerprint')).toBeNull();
  cleanup();
  explanation('en', 'passkey', 'relay_key_unbound', '1a2b-3c4d-5e6f-7a8b');
  expect(screen.queryByTestId('relay-key-fingerprint')).toBeNull();
});
it('explains an explicitly disabled relay and does not render unknown reason text', () => {
  expect(explanation('en', 'email', 'relay_disabled')).toHaveTextContent('administrator has disabled');
  cleanup();
  expect(explanation('en', 'email', 'raw private detail')).toHaveTextContent(en['mfa.email.unavailable']);
  expect(screen.getByRole('status')).not.toHaveTextContent('raw private detail');
  cleanup();
  // Prototype keys are not reason codes.
  expect(explanation('en', 'email', 'constructor')).toHaveTextContent(en['mfa.email.unavailable']);
});
it.each(['en', 'zh-TW', 'ja-JP'] as const)('explains insecure context and HTTPS guidance in %s', locale => {
  vi.stubGlobal('isSecureContext', false);
  vi.stubGlobal('PublicKeyCredential', undefined);
  const message = explanation(locale, 'passkey');
  expect(message).toHaveTextContent(catalogues[locale]['mfa.passkey.insecure_context']);
  expect(message).toHaveTextContent('HTTPS');
  expect(message).toHaveTextContent('localhost');
});
it.each(['en', 'zh-TW', 'ja-JP'] as const)('distinguishes unsupported browsers from server configuration in %s', locale => {
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('PublicKeyCredential', undefined);
  expect(explanation(locale, 'passkey')).toHaveTextContent(catalogues[locale]['mfa.passkey.unsupported_browser']);
  cleanup();
  vi.stubGlobal('PublicKeyCredential', class {});
  vi.stubGlobal('navigator', { credentials: {} });
  expect(explanation(locale, 'passkey')).toHaveTextContent(catalogues[locale]['mfa.passkey.unavailable']);
});
