import { afterEach, describe, expect, it, vi } from 'vitest';
import { loadPublicCompany, parseCompany, useCompanyStore } from './company-store';

const logo = `/api/company/logo/${'a'.repeat(64)}.png`;
afterEach(() => { vi.unstubAllGlobals(); useCompanyStore.getState().setCompany(null); });

describe('public company identity', () => {
  it('accepts only same-origin content-addressed logo paths', () => {
    expect(parseCompany({ name: '公司', logo_url: logo })).toEqual({ name: '公司', logo_url: logo });
    for (const url of ['https://other.example/logo.png', '//other.example/logo.png', '/api/company/logo/../x.png', 'data:image/svg+xml,x']) {
      expect(parseCompany({ name: '公司', logo_url: url }).logo_url).toBeNull();
    }
    expect(parseCompany({ name: 'x\nInjected', logo_url: logo }).name).toBe('');
  });
  it('does not let a late public read overwrite a successful save', async () => {
    let resolve!: (value: unknown) => void;
    vi.stubGlobal('fetch', vi.fn(() => new Promise((done) => { resolve = done; })));
    const pending = loadPublicCompany();
    useCompanyStore.getState().setCompany({ name: 'New company', logo_url: logo });
    resolve({ ok: true, json: async () => ({ name: 'Old company', logo_url: null }) });
    await pending;
    expect(useCompanyStore.getState().company.name).toBe('New company');
  });
  it('rejects bidi overrides, isolates and other format controls in company names', () => {
    const controls = [0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069,
      0xad, 0x61c, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x2060, 0xfeff, 0xe0001, 0xe0061];
    for (const codepoint of controls) {
      expect(parseCompany({ name: `Company${String.fromCodePoint(codepoint)}name`, logo_url: logo }))
        .toEqual({ name: '', logo_url: null });
    }
    expect(parseCompany({ name: '範例公司・株式会社 Acme', logo_url: logo }).name).toBe('範例公司・株式会社 Acme');
  });
  it('clears public identity on a failed read', async () => {
    useCompanyStore.getState().setCompany({ name: 'Old company', logo_url: logo });
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('network')));
    await loadPublicCompany();
    expect(useCompanyStore.getState().company).toEqual({ name: '', logo_url: null });
  });
});
