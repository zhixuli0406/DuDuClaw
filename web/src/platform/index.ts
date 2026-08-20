/**
 * Platform layer barrel (N-1, `DESIGN-agent-os-native-apps-2026-08.md` §2 L2).
 *
 * The single entry point for the cross-cutting concerns every app shares:
 * connection (WS client + connection store), auth/session, i18n, and theme.
 * `src/apps/**` and any future per-app directory MUST import these from
 * `@/platform` (or one of its named submodules), never by reaching directly
 * into `@/lib/ws-client`, `@/stores/connection-store`, `@/stores/auth-store`,
 * `@/i18n`, or `@/stores/theme-store` — that direct-import path is what let
 * per-app copies of connection/auth/i18n/theme state drift apart in the first
 * place (design doc §3 C4). `src/apps/import-boundary.test.ts` enforces this
 * mechanically pending a real ESLint `no-restricted-imports` rule (see that
 * file's header for why it's a Vitest guard instead of an ESLint rule today).
 *
 * `src/pages/**` and other existing call sites are NOT required to migrate to
 * this barrel in this pass — this is a boundary for new app code, not a
 * repo-wide import rewrite (§2 L2: "重點是建立邊界不是大搬家").
 */
export * from './connection';
export * from './auth';
export * from './i18n';
export * from './theme';
