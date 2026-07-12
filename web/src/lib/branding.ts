/**
 * Branding store — the distributor white-label surface (design-distributor-
 * white-label §4.1). A distributor with a `white_label` license can rename the
 * product, swap the logo, and change the About-page company block; every screen
 * reflects it live. Non-white-label instances see the default DuDuClaw / 🐾.
 *
 * Two-phase load so the LoginPage (rendered BEFORE auth, outside the app shell)
 * can already show the right brand:
 *   1. module init hydrates from `localStorage['duduclaw-branding-cache']`;
 *   2. `fetch()` after auth refreshes from `branding.get` and re-writes the cache.
 *
 * The vendor block ("嘟嘟數位科技有限公司") is authored by the backend const and
 * is NOT part of `BrandingConfig` — it can never be overwritten from here.
 */
import { create } from 'zustand';
import {
  api,
  type BrandingConfig,
  type BrandingVendor,
  type BrandingGetResponse,
} from '@/lib/api';
import { applyAccent } from '@/lib/accent';

const CACHE_KEY = 'duduclaw-branding-cache';

/** Product-name fallback when no white-label name is set. */
export const DEFAULT_BRAND_NAME = 'DuDuClaw';
/** Logo fallback glyph (the 🐾 paw) when no custom image logo is uploaded. */
export const DEFAULT_BRAND_LOGO = '🐾';

/** The subset of the `branding.get` response we cache for pre-auth rendering. */
interface BrandingCache {
  branding: BrandingConfig | null;
  vendor: BrandingVendor | null;
  white_label_active: boolean;
}

function readCache(): BrandingCache | null {
  if (typeof localStorage === 'undefined') return null;
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as BrandingCache;
    if (parsed && typeof parsed === 'object') return parsed;
  } catch {
    /* corrupt cache → ignore, fall back to defaults */
  }
  return null;
}

function writeCache(cache: BrandingCache): void {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(cache));
  } catch {
    /* quota / private mode → cache is best-effort only */
  }
}

// ── Pure fallback resolvers (design §4.1 effectiveName / effectiveLogo) ──────

function trimOrNull(v: string | null | undefined): string | null {
  const t = v?.trim();
  return t && t.length > 0 ? t : null;
}

/** Resolve the display name for a given branding config (pure). */
export function brandNameFrom(b: BrandingConfig | null | undefined): string {
  return trimOrNull(b?.product_name) ?? DEFAULT_BRAND_NAME;
}

/** True when the string is an inline image data URI (renderable as `<img>`). */
export function logoIsImage(value: string): boolean {
  return value.startsWith('data:image/');
}

/**
 * Resolve the logo for a branding config (pure). Returns an image data URI when
 * a custom logo was uploaded, otherwise the default 🐾 glyph. Callers use
 * `logoIsImage()` to decide between `<img>` and a text glyph.
 */
export function brandLogoFrom(b: BrandingConfig | null | undefined): string {
  const uri = b?.logo_data_uri;
  return uri && logoIsImage(uri) ? uri : DEFAULT_BRAND_LOGO;
}

interface BrandingStore {
  readonly branding: BrandingConfig | null;
  readonly vendor: BrandingVendor | null;
  readonly whiteLabelActive: boolean;
  /** True once a live `branding.get` has resolved (cache alone is not loaded). */
  readonly loaded: boolean;
  /** Fetch the authoritative branding after auth; refreshes the cache. */
  fetch: () => Promise<void>;
  /** Apply a fresh config locally (e.g. right after `branding.set` succeeds). */
  setBranding: (branding: BrandingConfig, whiteLabelActive?: boolean) => void;
}

const initial = readCache();

export const useBrandingStore = create<BrandingStore>((set, get) => ({
  branding: initial?.branding ?? null,
  vendor: initial?.vendor ?? null,
  whiteLabelActive: initial?.white_label_active ?? false,
  loaded: false,

  fetch: async () => {
    try {
      const res: BrandingGetResponse = await api.branding.get();
      set({
        branding: res.branding ?? null,
        vendor: res.vendor ?? null,
        whiteLabelActive: res.white_label_active,
        loaded: true,
      });
      writeCache({
        branding: res.branding ?? null,
        vendor: res.vendor ?? null,
        white_label_active: res.white_label_active,
      });
    } catch {
      // Fail-soft: keep whatever the cache gave us. A branding fetch failure
      // must never block the dashboard — it just means defaults / stale brand.
      set({ loaded: true });
    }
  },

  setBranding: (branding, whiteLabelActive) => {
    const wl = whiteLabelActive ?? get().whiteLabelActive;
    set({ branding, whiteLabelActive: wl, loaded: true });
    writeCache({ branding, vendor: get().vendor, white_label_active: wl });
  },
}));

// ── Non-reactive resolvers (for module-scope / d3 / chat-store init) ─────────

/** Current effective product name (reads store state; non-reactive). */
export function effectiveName(): string {
  return brandNameFrom(useBrandingStore.getState().branding);
}

/** Current effective logo — image data URI or 🐾 (reads store state). */
export function effectiveLogo(): string {
  return brandLogoFrom(useBrandingStore.getState().branding);
}

/**
 * Logo restricted to a text glyph — always the 🐾 fallback, since a custom
 * (image) logo cannot render inside a text slot. Use where only a glyph fits
 * (agent-picker icon fallback, d3 org node).
 */
export function effectiveLogoGlyph(): string {
  const logo = effectiveLogo();
  return logoIsImage(logo) ? DEFAULT_BRAND_LOGO : logo;
}

// ── Reactive hooks (for React components) ────────────────────────────────────

/** Reactive effective product name. */
export function useEffectiveName(): string {
  return useBrandingStore((s) => brandNameFrom(s.branding));
}

/** Reactive effective logo + whether it is an image. The zustand selector must
 *  return a stable primitive (the logo string); the object is derived outside it
 *  so we never trip the "getSnapshot should be cached" infinite-render guard. */
export function useEffectiveLogo(): { value: string; isImage: boolean } {
  const value = useBrandingStore((s) => brandLogoFrom(s.branding));
  return { value, isImage: logoIsImage(value) };
}

/** Reactive logo restricted to a text glyph (image logos collapse to 🐾). */
export function useEffectiveLogoGlyph(): string {
  return useBrandingStore((s) => {
    const value = brandLogoFrom(s.branding);
    return logoIsImage(value) ? DEFAULT_BRAND_LOGO : value;
  });
}

// ── Document chrome: title + favicon (design §4.1) ───────────────────────────

// Capture the original favicon href once so we can restore it when a custom
// logo is cleared (the index.html static value is the default fallback).
let originalFavicon: string | null | undefined;

function faviconLink(): HTMLLinkElement | null {
  if (typeof document === 'undefined') return null;
  let link = document.querySelector<HTMLLinkElement>('link[rel~="icon"]');
  if (!link) {
    link = document.createElement('link');
    link.rel = 'icon';
    document.head.appendChild(link);
  }
  return link;
}

function applyChrome(branding: BrandingConfig | null): void {
  if (typeof document === 'undefined') return;
  document.title = `${brandNameFrom(branding)} Dashboard`;

  const link = faviconLink();
  if (link) {
    if (originalFavicon === undefined) originalFavicon = link.getAttribute('href');

    const uri = branding?.logo_data_uri;
    if (uri && logoIsImage(uri)) {
      link.setAttribute('href', uri);
    } else if (originalFavicon != null) {
      link.setAttribute('href', originalFavicon);
    }
  }

  // Brand accent (design §10.4): inject / remove the token override. Invalid or
  // absent accent leaves the default amber untouched.
  applyAccent(branding?.accent_color ?? null);
}

// Apply once from the hydrated cache, then on every branding change.
applyChrome(useBrandingStore.getState().branding);
useBrandingStore.subscribe((s) => applyChrome(s.branding));
