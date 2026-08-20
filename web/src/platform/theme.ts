/**
 * Platform layer — theme (N-1, `DESIGN-agent-os-native-apps-2026-08.md`
 * §2 L2). Single import path for theme state + the `.dark` class applier, so
 * a future app can't drift from the shared light/dark tokens (C4).
 * See `connection.ts` for why this is a re-export barrel rather than a move.
 */
export { applyTheme, useThemeStore, type Theme } from '@/stores/theme-store';
