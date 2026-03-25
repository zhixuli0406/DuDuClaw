import { create } from 'zustand';
import zhTW from './zh-TW.json';
import en from './en.json';

export const messages: Record<string, Record<string, string>> = {
  'zh-TW': zhTW,
  'en': en,
};

export const defaultLocale = 'zh-TW';

export function getLocale(): string {
  const stored = localStorage.getItem('duduclaw-locale');
  if (stored && stored in messages) return stored;
  const browserLang = navigator.language;
  if (browserLang.startsWith('zh')) return 'zh-TW';
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
    localStorage.setItem('duduclaw-locale', locale);
    set({ locale });
  },
}));
