import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { api } from '@/lib/api';
import { loadPublicBrand, useBrandStore } from './brand-store';
import './system-store';

const brand = { product_name: 'Partner', logo_light: 'data:image/png;base64,iVBORw0KGgo=' };
const statusChanged = mockWsClient.subscribe.mock.calls.find(([event]) => event === 'system.status_changed')?.[1] as ((value: unknown) => void) | undefined;

beforeEach(() => {
  useBrandStore.getState().setBrand(brand);
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => null }));
});
afterEach(() => { vi.unstubAllGlobals(); mockWsClient.call.mockReset(); useBrandStore.getState().setBrand(null); });

describe('brand refresh after license changes', () => {
  it('clears an active brand when license.status is no longer Oem', async () => {
    mockWsClient.call.mockResolvedValue({ tier: 'open_source' });
    await api.license.status();
    expect(useBrandStore.getState().brand).toBeNull();
  });
  it('refuses a late About brand after license invalidation and a newer public response', async () => {
    let resolveAbout!: (value: unknown) => void;
    mockWsClient.call.mockImplementation((method: string) => method === 'about.get'
      ? new Promise((resolve) => { resolveAbout = resolve; })
      : Promise.resolve({ tier: 'open_source' }));
    const pending = api.about.get();
    await api.license.status();
    await loadPublicBrand();
    expect(useBrandStore.getState().brand).toBeNull();
    resolveAbout({ brand, tier: 'oem' });
    await pending;
    expect(useBrandStore.getState().brand).toBeNull();
  });
  it('refetches public branding after a successful activation', async () => {
    mockWsClient.call.mockResolvedValue({ success: true, status: { tier: 'oem' } });
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => ({ ...brand, product_name: 'New Partner' }) }));
    await api.license.activate('signed-key');
    expect(fetch).toHaveBeenCalledWith('/api/brand', { cache: 'no-store' });
    expect(useBrandStore.getState().brand?.product_name).toBe('New Partner');
  });
  it('invalidates and reloads branding on system.status_changed', async () => {
    expect(statusChanged).toBeTypeOf('function');
    statusChanged!({ version: '1.78.0' });
    expect(useBrandStore.getState().brand).toBeNull();
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledWith('/api/brand', { cache: 'no-store' }));
    expect(useBrandStore.getState().brand).toBeNull();
  });
});
