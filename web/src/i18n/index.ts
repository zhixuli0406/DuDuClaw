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
