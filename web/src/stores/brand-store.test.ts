import { afterEach, describe, expect, it, vi } from 'vitest';
import { parseBrand, useBrandStore, loadPublicBrand } from './brand-store';
import { effectiveName } from '@/lib/product-identity';
import { brandedMessages, messages } from '@/i18n';
import { createIntl } from 'react-intl';
import { applyDocumentBrand } from '@/lib/brand-document';

const logo = 'data:image/png;base64,iVBORw0KGgo=';
const brand = { product_name: '合作夥伴', logo_light: logo };

afterEach(() => { useBrandStore.getState().setBrand(null); vi.unstubAllGlobals(); });

describe('verified distributor display data', () => {
  it('defaults byte-for-byte to the original identity and catalogues', () => {
    expect(effectiveName()).toBe('DuDuClaw');
    for (const locale of Object.keys(messages)) expect(brandedMessages(locale)).toBe(messages[locale]);
  });
  it('accepts only the narrow name and raster logo contract', () => {
    expect(parseBrand({ ...brand, accent_color: 'red', vendor: 'fake' })).toEqual(brand);
    for (const product_name of ['', 'a'.repeat(41), 'bad\nname', '<script>', 'https://evil.test', ' ', '\u2028', 'a\u2029b', 'mailto:a@b.com'])
      expect(parseBrand({ ...brand, product_name })).toBeNull();
    for (const logo_light of ['data:image/svg+xml;base64,AAAA', 'data:image/png;base64,AAAA', 'https://evil.test/logo.png', `data:image/png;base64,${btoa('\x89PNG\r\n\x1a\n' + 'a'.repeat(128 * 1024))}`])
      expect(parseBrand({ ...brand, logo_light })).toBeNull();
  });
  it('loads public data and returns to original identity on failure', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true, json: async () => brand }).mockRejectedValueOnce(new Error('offline')));
    await loadPublicBrand();
    expect(effectiveName()).toBe('合作夥伴');
    await loadPublicBrand();
    expect(effectiveName()).toBe('DuDuClaw');
  });
  it.each(['zh-TW', 'en', 'ja-JP'])('substitutes %s once while preserving legal and privacy disclosures', (locale) => {
    const translated = brandedMessages(locale, brand.product_name);
    expect(translated['app.name']).toBe(brand.product_name);
    for (const id of ['googleDataDisclosure.access', 'googleDataDisclosure.training', 'googleDataDisclosure.privacy', 'license.distributorNotice', 'subscriptionRisk.conflicts', 'subscriptionRisk.vendor.openai.policy'])
      expect(translated[id]).toBe(messages[locale][id]);
    expect(messages[locale]['app.name']).toBe('DuDuClaw');
    expect(brandedMessages(locale)['app.name']).toBe('DuDuClaw');
  });
  it('ignores an older public response after a newer about.get brand or invalidation', async () => {
    let resolve!: (response: { ok: boolean; json: () => Promise<unknown> }) => void;
    vi.stubGlobal('fetch', vi.fn(() => new Promise((done) => { resolve = done; })));
    const pending = loadPublicBrand();
    useBrandStore.getState().setBrand({ ...brand, product_name: 'Newer About Brand' });
    resolve({ ok: true, json: async () => brand });
    await pending;
    expect(effectiveName()).toBe('Newer About Brand');
    const second = loadPublicBrand();
    useBrandStore.getState().setBrand(null);
    resolve({ ok: true, json: async () => brand });
    await second;
    expect(effectiveName()).toBe('DuDuClaw');
  });
  it('ignores older public failures and data after the latest public refresh', async () => {
    const resolvers: Array<(value: unknown) => void> = [];
    vi.stubGlobal('fetch', vi.fn(() => new Promise((resolve) => { resolvers.push(resolve); })));
    const older = loadPublicBrand();
    const newer = loadPublicBrand();
    resolvers[1]({ ok: true, json: async () => ({ ...brand, product_name: 'Latest' }) });
    await newer;
    resolvers[0]({ ok: false });
    await older;
    expect(effectiveName()).toBe('Latest');
  });
  it('treats brand names as literal ICU text', () => {
    const name = "Partner's {office}";
    const intl = createIntl({ locale: 'en', messages: brandedMessages('en', name) });
    expect(intl.formatMessage({ id: 'app.name' })).toBe(name);
  });
  it('sets and restores runtime title and favicon without changing defaults', () => {
    document.title = 'DuDuClaw Dashboard';
    document.head.innerHTML += '<link rel="icon" href="/default.svg" type="image/svg+xml">';
    const restore = applyDocumentBrand({ ...brand, logo_dark: 'data:image/webp;base64,UklGRgAAAABXRUJQ' }, true);
    expect(document.title).toBe(brand.product_name);
    expect(document.querySelector('link[rel="icon"]')?.getAttribute('href')).toBe('data:image/webp;base64,UklGRgAAAABXRUJQ');
    restore();
    expect(document.title).toBe('DuDuClaw Dashboard');
    expect(document.querySelector('link[rel="icon"]')?.getAttribute('href')).toBe('/default.svg');
  });
});
