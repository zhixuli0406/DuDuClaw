import { create } from 'zustand';
import zhTW from './zh-TW.json';
import en from './en.json';
import jaJP from './ja-JP.json';

export const messages: Record<string, Record<string, string>> = {
  'zh-TW': zhTW,
  'en': en,
  // English fills any key missing from the Japanese catalogue so react-intl
  // never falls back to raw message ids.
  'ja-JP': { ...en, ...jaJP },
};

/** Legal notices and privacy disclosures keep the software vendor's identity.
 * Google privacy links are defined outside the catalogues and remain untouched.
 */
export const BRAND_EXCLUDED_PREFIXES = ['googleDataDisclosure.', 'subscriptionRisk.', 'license.nfr.', 'license.mode.', 'about.'];
export const BRAND_EXCLUDED_IDS: string[] = [];

export function brandedMessages(locale: string, productName?: string): Record<string, string> {
  const catalogue = messages[locale] ?? messages[defaultLocale];
  if (!productName || productName === 'DuDuClaw') return catalogue;
  // Brand names are plain text, never ICU placeholders or rich-text syntax.
  const literal = productName.replace(/'/g, "''").replace(/[{}]/g, (char) => "'" + char + "'");
  return Object.fromEntries(Object.entries(catalogue).map(([id, value]) => [id,
    BRAND_EXCLUDED_PREFIXES.some((prefix) => id.startsWith(prefix)) || BRAND_EXCLUDED_IDS.includes(id)
      || /(?:copyright|trademark|著作權|商標|著作権|https?:\/\/)/iu.test(value)
      ? value : value.replaceAll('DuDuClaw', () => literal),
  ]));
}

/** Native-language labels for the language switcher. */
export const localeNames: Record<string, string> = {
  'zh-TW': '繁體中文',
  'en': 'English',
  'ja-JP': '日本語',
};

export const defaultLocale = 'zh-TW';

export function getLocale(): string {
  const stored = localStorage.getItem('duduclaw-locale');
  if (stored && stored in messages) return stored;
  const browserLang = navigator.language;
  if (browserLang.startsWith('zh')) return 'zh-TW';
  if (browserLang.startsWith('ja')) return 'ja-JP';
  return 'en';
}

/** Locale store — triggers React re-render when locale changes (FE-M1). */
interface LocaleStore {
  locale: string;
  setLocale: (locale: string) => void;
}

export const useLocaleStore = create<LocaleStore>((set) => ({
  locale: getLocale(),
  setLocale: (locale: string) => {
    if (!(locale in messages)) return;
    localStorage.setItem('duduclaw-locale', locale);
    set({ locale });
  },
}));
