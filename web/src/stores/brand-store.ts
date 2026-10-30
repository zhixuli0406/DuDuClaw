import { create } from 'zustand';
import { client } from '@/lib/ws-client';

export interface DistributorBrand {
  product_name: string;
  logo_light: string;
  logo_dark?: string | null;
}

/** Public display data only; never persist a brand or trust the old local cache. */
export function parseBrand(value: unknown): DistributorBrand | null {
  if (!value || typeof value !== 'object') return null;
  const brand = value as Record<string, unknown>;
  const name = brand.product_name;
  if (typeof name !== 'string' || [...name].length < 1 || [...name].length > 40
    || !name.trim() || /[\p{Cc}<>\u2028\u2029]|:\/\/|www\./iu.test(name)
    || name.split(/\s+/u).some((part) => {
      try { new URL(part); return true; } catch { return false; }
    })) return null;
  const validLogo = (logo: unknown): logo is string => {
    if (typeof logo !== 'string') return false;
    const match = /^data:image\/(png|jpeg|webp);base64,([A-Za-z0-9+/]*={0,2})$/.exec(logo);
    if (!match || match[2].length > 174764) return false;
    try {
      const bytes = atob(match[2]);
      if (!bytes.length || bytes.length > 128 * 1024) return false;
      return match[1] === 'png' ? bytes.startsWith('\x89PNG\r\n\x1a\n')
        : match[1] === 'jpeg' ? bytes.startsWith('\xff\xd8\xff')
          : bytes.startsWith('RIFF') && bytes.substring(8, 12) === 'WEBP';
    } catch { return false; }
  };
  if (!validLogo(brand.logo_light) || (brand.logo_dark != null && !validLogo(brand.logo_dark))) return null;
  return { product_name: name, logo_light: brand.logo_light, logo_dark: brand.logo_dark as string | null | undefined };
}

// Public and about requests share an epoch from request start, so a late
// response cannot replace newer data or a license-change invalidation.
let brandRequestEpoch = 0;
let brandRevision: string | undefined;

export const useBrandStore = create<{ brand: DistributorBrand | null; setBrand: (value: unknown, revision?: string) => void }>((set) => ({
  brand: null,
  setBrand: (value, revision) => {
    brandRequestEpoch += 1;
    brandRevision = revision;
    set({ brand: parseBrand(value) });
  },
}));

/** Capture before starting I/O; only the newest, still-valid request may apply. */
export function beginBrandRequest(): (value: unknown, revision?: string) => void {
  const request = ++brandRequestEpoch;
  return (value, revision) => {
    if (request === brandRequestEpoch) useBrandStore.getState().setBrand(value, revision);
  };
}

export async function loadPublicBrand(): Promise<void> {
  const apply = beginBrandRequest();
  try {
    const response = await fetch('/api/brand', { cache: 'no-cache' });
    const value: unknown = response.ok ? await response.json() : null;
    apply(value, response.ok ? response.headers?.get('X-Brand-Revision') ?? undefined : undefined);
  } catch {
    apply(null);
  }
}

/** Poll only the small revision; download logos again when the identity changes. */
export async function refreshBrandRevision(): Promise<void> {
  // Metadata probes do not supersede a full response already in flight.
  // Only a changed revision starting loadPublicBrand may invalidate it.
  const request = brandRequestEpoch;
  try {
    const value = await client.call('about.get', { brand_revision_only: true }) as { brand_revision?: string };
    if (request !== brandRequestEpoch) return;
    if (typeof value?.brand_revision !== 'string' || value.brand_revision !== brandRevision) {
      await loadPublicBrand();
    }
  } catch {
    // A transient metadata failure leaves the last verified display intact.
  }
}
