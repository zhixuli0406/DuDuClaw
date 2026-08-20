/**
 * Platform layer — i18n init (N-1, `DESIGN-agent-os-native-apps-2026-08.md`
 * §2 L2). Single import path for the message catalogues + locale store, so a
 * future app can't accidentally ship a second copy of the locale list (C4).
 * See `connection.ts` for why this is a re-export barrel rather than a move.
 */
export { messages, localeNames, defaultLocale, getLocale, useLocaleStore } from '@/i18n';
