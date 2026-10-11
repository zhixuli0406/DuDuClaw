/** Commercial licenses are sold and renewed by the authorized general distributor. */
import { useIntl } from 'react-intl';
export const COMMERCIAL_CONTACT_URL = 'https://www.futurecorp.tw/';
export const COMMERCIAL_CONTACT_NAME = { zh: '未來企業', en: 'Future Corp' } as const;
export interface CommercialContact { readonly url: string; readonly name: string; }
export function resolveCommercialContact(
  lang: 'zh' | 'en' = 'zh',
): CommercialContact {
  return { url: COMMERCIAL_CONTACT_URL, name: COMMERCIAL_CONTACT_NAME[lang] };
}
export function useCommercialContact(): CommercialContact {
  const intl = useIntl();
  return resolveCommercialContact(intl.locale.toLowerCase().startsWith('en') ? 'en' : 'zh');
}
