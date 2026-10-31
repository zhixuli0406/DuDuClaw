import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { api } from '@/lib/api';
import { loadPublicBrand, refreshBrandRevision, useBrandStore } from './brand-store';
import { useConnectionStore } from './connection-store';
import { useAuthStore } from './auth-store';
import './system-store';

const brand = { product_name: 'Partner', logo_light: 'data:image/png;base64,iVBORw0KGgo=' };
const statusChanged = mockWsClient.subscribe.mock.calls.find(([event]) => event === 'system.status_changed')?.[1] as ((value: unknown) => void) | undefined;

beforeEach(() => {
  useBrandStore.getState().setBrand(brand);
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => null }));
});
afterEach(() => { vi.unstubAllGlobals(); mockWsClient.call.mockReset(); useBrandStore.getState().setBrand(null); });

describe('brand refresh after license changes', () => {
  it.each(['self_host_pro', 'solo', 'business', 'open_source'])('preserves the server-provided brand when license.status reports %s', async (tier) => {
    mockWsClient.call.mockResolvedValue({ tier });
    expect(await api.license.status()).toEqual({ tier });
    expect(useBrandStore.getState().brand).toEqual(brand);
    expect(fetch).not.toHaveBeenCalled();
  });
  it('refuses a late About brand after a newer public response clears it', async () => {
    let resolveAbout!: (value: unknown) => void;
    mockWsClient.call.mockImplementation((method: string) => method === 'about.get'
      ? new Promise((resolve) => { resolveAbout = resolve; })
      : Promise.resolve({ tier: 'open_source' }));
    const pending = api.about.get();
    await api.license.status();
    await loadPublicBrand();
    expect(useBrandStore.getState().brand).toBeNull();
    resolveAbout({ brand, tier: 'self_host_pro' });
    await pending;
    expect(useBrandStore.getState().brand).toBeNull();
  });
  it('refetches public branding after a successful activation', async () => {
    mockWsClient.call.mockResolvedValue({ success: true, status: { tier: 'self_host_pro' } });
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => ({ ...brand, product_name: 'New Partner' }) }));
    await api.license.activate('signed-key');
    expect(fetch).toHaveBeenCalledWith('/api/brand', { cache: 'no-cache' });
    expect(useBrandStore.getState().brand?.product_name).toBe('New Partner');
  });
  it('invalidates and reloads branding on system.status_changed', async () => {
    expect(statusChanged).toBeTypeOf('function');
    statusChanged!({ version: '1.78.0' });
    expect(useBrandStore.getState().brand).toBeNull();
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledWith('/api/brand', { cache: 'no-cache' }));
    expect(useBrandStore.getState().brand).toBeNull();
  });
  it('checks a small revision and skips unchanged logo downloads', async () => {
    useBrandStore.getState().setBrand(brand, 'v1');
    mockWsClient.call.mockResolvedValue({ brand_revision: 'v1' });
    await refreshBrandRevision();
    expect(mockWsClient.call).toHaveBeenCalledWith('about.get', { brand_revision_only: true });
    expect(fetch).not.toHaveBeenCalled();
    expect(useBrandStore.getState().brand).toEqual(brand);
  });
  it('remembers revisions from the public header and full About response', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({
      ok: true, headers: new Headers({ 'X-Brand-Revision': 'public-v1' }), json: async () => brand,
    }));
    await loadPublicBrand();
    mockWsClient.call.mockResolvedValue({ brand_revision: 'public-v1' });
    await refreshBrandRevision();
    expect(fetch).toHaveBeenCalledTimes(1);
    mockWsClient.call.mockResolvedValueOnce({ brand, brand_revision: 'about-v2' });
    await api.about.get();
    mockWsClient.call.mockResolvedValue({ brand_revision: 'about-v2' });
    await refreshBrandRevision();
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it('reloads changed branding and accepts null as the original brand', async () => {
    useBrandStore.getState().setBrand(brand, 'v1');
    mockWsClient.call.mockResolvedValue({ brand_revision: 'v2' });
    await refreshBrandRevision();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(useBrandStore.getState().brand).toBeNull();
  });
  it('ignores an old revision response after a newer brand update', async () => {
    let resolve!: (value: unknown) => void;
    mockWsClient.call.mockImplementation(() => new Promise((done) => { resolve = done; }));
    const pending = refreshBrandRevision();
    useBrandStore.getState().setBrand(brand, 'new');
    resolve({ brand_revision: 'old' });
    await pending;
    expect(fetch).not.toHaveBeenCalled();
    expect(useBrandStore.getState().brand).toEqual(brand);
  });
  it('keeps an in-flight public brand update when a metadata probe fails', async () => {
    let resolve!: (value: unknown) => void;
    vi.stubGlobal('fetch', vi.fn(() => new Promise((done) => { resolve = done; })));
    useBrandStore.getState().setBrand(null);
    const pending = loadPublicBrand();
    mockWsClient.call.mockRejectedValueOnce(new Error('timeout'));
    await refreshBrandRevision();
    resolve({ ok: true, headers: new Headers({ 'X-Brand-Revision': 'new' }), json: async () => brand });
    await pending;
    expect(useBrandStore.getState().brand).toEqual(brand);
  });
  it('rechecks on reconnect and when an authenticated tab gains focus', async () => {
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'about.get'
      ? { brand_revision: 'v1' } : {}));
    useConnectionStore.setState({ state: 'connecting' });
    useConnectionStore.setState({ state: 'authenticated' });
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    useBrandStore.getState().setBrand(brand, 'v1');
    mockWsClient.call.mockClear();
    window.dispatchEvent(new Event('focus'));
    await vi.waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('about.get', { brand_revision_only: true }));
    expect(fetch).toHaveBeenCalledTimes(1);
    useConnectionStore.setState({ state: 'disconnected' });
  });
  it('rechecks using the existing periodic session refresh timer', async () => {
    vi.useFakeTimers();
    try {
      const user = { id: 'u1', email: 'a@b.c', display_name: 'A', role: 'admin', status: 'active' };
      const fetchMock = vi.fn(async (url: string) => ({ ok: true, status: 200, json: async () =>
        url === '/api/me' ? { user, bindings: [] } : { access_token: 'at', refresh_token: 'rt', user } }));
      vi.stubGlobal('fetch', fetchMock);
      mockWsClient.call.mockResolvedValue({ brand_revision: 'v1' });
      useBrandStore.getState().setBrand(brand, 'v1');
      await useAuthStore.getState().login(user.email, 'password');
      await vi.advanceTimersByTimeAsync(25 * 60 * 1000);
      expect(mockWsClient.call).toHaveBeenCalledWith('about.get', { brand_revision_only: true });
      expect(fetchMock.mock.calls.some(([url]) => url === '/api/brand')).toBe(false);
    } finally {
      useAuthStore.getState().logout();
      vi.useRealTimers();
    }
  });
});
