import { afterEach, expect, it, vi } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import ja from '@/i18n/ja-JP.json';
import { DisabledFactorExplanation } from './DisabledFactorExplanation';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
const catalogues = { en, 'zh-TW': zh, 'ja-JP': ja };
function explanation(locale: keyof typeof catalogues, factor: 'email' | 'passkey', reason?: string) {
  render(<IntlProvider locale={locale} messages={catalogues[locale]}>
    <DisabledFactorExplanation factor={factor} reason={reason} />
  </IntlProvider>);
  return screen.getByRole('status');
}
it.each(['en', 'zh-TW', 'ja-JP'] as const)('explains missing SMTP and relay, with SMTP guidance in %s', locale => {
  const message = explanation(locale, 'email', 'no_smtp_no_relay');
  expect(message).toHaveTextContent(catalogues[locale]['mfa.email.unavailable']);
  expect(message).toHaveTextContent('SMTP');
});
it('explains an explicitly disabled relay and does not render unknown reason text', () => {
  expect(explanation('en', 'email', 'relay_disabled')).toHaveTextContent('administrator has disabled');
  cleanup();
  expect(explanation('en', 'email', 'raw private detail')).toHaveTextContent(en['mfa.email.unavailable']);
  expect(screen.getByRole('status')).not.toHaveTextContent('raw private detail');
});
it.each(['en', 'zh-TW', 'ja-JP'] as const)('explains insecure context and HTTPS guidance in %s', locale => {
  vi.stubGlobal('isSecureContext', false);
  vi.stubGlobal('PublicKeyCredential', undefined);
  const message = explanation(locale, 'passkey');
  expect(message).toHaveTextContent(catalogues[locale]['mfa.passkey.insecure_context']);
  expect(message).toHaveTextContent('HTTPS');
  expect(message).toHaveTextContent('localhost');
});
it('distinguishes unsupported browsers on a secure connection from server configuration', () => {
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('PublicKeyCredential', undefined);
  expect(explanation('en', 'passkey')).toHaveTextContent(en['mfa.passkey.unsupported_browser']);
  cleanup();
  vi.stubGlobal('PublicKeyCredential', class {});
  vi.stubGlobal('navigator', { credentials: {} });
  expect(explanation('en', 'passkey')).toHaveTextContent(en['mfa.passkey.unavailable']);
});
