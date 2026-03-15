import zhTW from './zh-TW.json';
import en from './en.json';

export const messages: Record<string, Record<string, string>> = {
  'zh-TW': zhTW,
  'en': en,
};

export const defaultLocale = 'zh-TW';

export function getLocale(): string {
  // Check localStorage first, then browser language
  const stored = localStorage.getItem('duduclaw-locale');
  if (stored && stored in messages) return stored;

  const browserLang = navigator.language;
  if (browserLang.startsWith('zh')) return 'zh-TW';
  return 'en';
}

export function setLocale(locale: string) {
  localStorage.setItem('duduclaw-locale', locale);
}
