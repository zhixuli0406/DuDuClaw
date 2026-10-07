import { describe, it, expect } from 'vitest';
import {
  COMMERCIAL_CONTACT_URL,
  COMMERCIAL_CONTACT_NAME,
  resolveCommercialContact,
} from './commercial-contact';

describe('resolveCommercialContact', () => {
  it('falls back to the authorized distributor with no branding', () => {
    expect(resolveCommercialContact(null, false)).toEqual({
      url: 'https://www.futurecorp.tw/',
      name: COMMERCIAL_CONTACT_NAME.zh,
    });
    expect(COMMERCIAL_CONTACT_URL).toBe('https://www.futurecorp.tw/');
  });

  it('uses the English display name for English UIs', () => {
    expect(resolveCommercialContact(null, false, 'en').name).toBe('Future Corp');
  });

  it('ignores branding.website unless white-label is active', () => {
    const c = resolveCommercialContact({ website: 'https://reseller.example/' }, false);
    expect(c.url).toBe(COMMERCIAL_CONTACT_URL);
  });

  it('prefers the white-label distributor website and company name', () => {
    const c = resolveCommercialContact(
      { website: 'https://reseller.example/shop', company_name: ' Reseller Co ' },
      true,
    );
    expect(c).toEqual({ url: 'https://reseller.example/shop', name: 'Reseller Co' });
  });

  it('names a white-label site by host when no company name is set', () => {
    expect(resolveCommercialContact({ website: 'https://reseller.example/' }, true).name).toBe(
      'reseller.example',
    );
  });

  it('rejects non-http(s) and malformed websites', () => {
    for (const website of ['javascript:alert(1)', 'not a url', '', '   ', null]) {
      expect(resolveCommercialContact({ website }, true).url).toBe(COMMERCIAL_CONTACT_URL);
    }
  });
});
