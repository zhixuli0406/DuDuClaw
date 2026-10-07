/**
 * Where an operator goes to buy, upgrade or renew a commercial licence.
 *
 * The vendor sells only through an exclusive general distributor, so the
 * dashboard never links to a vendor-side pricing page. Resolution order:
 *   1. the white-label distributor's own `branding.website` (and
 *      `branding.company_name`), when white-label is active and the website is a
 *      valid http(s) URL;
 *   2. the authorised general distributor, 未來企業股份有限公司 (Future Corp).
 */
import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import type { BrandingConfig } from '@/lib/api';
import { useBrandingStore } from '@/lib/branding';

/** Authorised general distributor (fallback when no white-label contact is set). */
export const COMMERCIAL_CONTACT_URL = 'https://www.futurecorp.tw/';

/** Short display names used inside copy, per UI language. */
export const COMMERCIAL_CONTACT_NAME = {
  zh: '未來企業',
  en: 'Future Corp',
} as const;

export interface CommercialContact {
  readonly url: string;
  readonly name: string;
}

function httpUrlOrNull(value: string | null | undefined): string | null {
  const t = value?.trim();
  if (!t) return null;
  try {
    const u = new URL(t);
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.toString() : null;
  } catch {
    return null;
  }
}

/** Pure resolver (exported for tests). `lang` selects the fallback display name. */
export function resolveCommercialContact(
  branding: BrandingConfig | null | undefined,
  whiteLabelActive: boolean,
  lang: 'zh' | 'en' = 'zh',
): CommercialContact {
  const brandUrl = whiteLabelActive ? httpUrlOrNull(branding?.website) : null;
  if (brandUrl) {
    const company = branding?.company_name?.trim();
    return { url: brandUrl, name: company || new URL(brandUrl).hostname };
  }
  return { url: COMMERCIAL_CONTACT_URL, name: COMMERCIAL_CONTACT_NAME[lang] };
}

/** Reactive contact for components: `url` for the link, `name` for the `{distributor}` copy value. */
export function useCommercialContact(): CommercialContact {
  const intl = useIntl();
  const branding = useBrandingStore((s) => s.branding);
  const whiteLabelActive = useBrandingStore((s) => s.whiteLabelActive);
  const lang = intl.locale.toLowerCase().startsWith('en') ? 'en' : 'zh';
  return useMemo(
    () => resolveCommercialContact(branding, whiteLabelActive, lang),
    [branding, whiteLabelActive, lang],
  );
}
