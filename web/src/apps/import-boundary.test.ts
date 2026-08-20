import { describe, it, expect } from 'vitest';

/**
 * Import-boundary guard (N-1, `DESIGN-agent-os-native-apps-2026-08.md` §3 C4:
 * "platform 單一來源＋CI 檢查：app 目錄禁 import 非 platform 的共用層路徑
 * （eslint rule）").
 *
 * This project has NO ESLint installed anywhere in the repo today (verified:
 * no `eslint.config.*`/`.eslintrc*` under `web/`, no `eslint` in
 * `package.json` dependencies, no `lint` script) — there is no "既有 lint 設定
 * 慣例" to extend. Bootstrapping ESLint + typescript-eslint from scratch is a
 * separate, heavier decision (new devDependencies, a config, a lint script,
 * likely CI wiring) than this pass's scope, so this test stands in as the
 * mechanical enforcement for C4 instead: it runs on every `vitest run` (the
 * same command this task's verification step already requires), needs no new
 * dependency (uses Vite's own `import.meta.glob`, not Node's `fs` — this repo
 * has no `@types/node` either), and fails loudly the moment `src/apps/**`
 * reaches around the `@/platform` barrel for connection/auth/i18n/theme
 * state. Swap this for a real `no-restricted-imports` ESLint rule if/when
 * ESLint is bootstrapped for `web/` — the check it performs should transfer
 * directly.
 */

/** The exact modules `@/platform` re-exports (see `platform/*.ts`). Importing
 *  these directly from `src/apps/**` bypasses the single entry point C4 is
 *  guarding — importing them via `@/platform` (or a `@/platform/<submodule>`)
 *  is fine and expected. */
const GUARDED_SPECIFIERS = [
  '@/lib/ws-client',
  '@/stores/connection-store',
  '@/stores/auth-store',
  '@/i18n',
  '@/i18n/index',
  '@/stores/theme-store',
];

const IMPORT_FROM_RE = /\bfrom\s+['"]([^'"]+)['"]/g;

function violatingImports(src: string): string[] {
  const hits: string[] = [];
  for (const match of src.matchAll(IMPORT_FROM_RE)) {
    if (GUARDED_SPECIFIERS.includes(match[1])) hits.push(match[1]);
  }
  return hits;
}

// Every non-test source file under src/apps/ (and, once they exist, any
// future `src/apps/<id>/**` subdirectory — the glob is recursive), as raw
// text via Vite's own module graph. Eager + `?raw` so this is plain data at
// test time, no dynamic import juggling.
const appFiles = import.meta.glob('./**/*.{ts,tsx}', { eager: true, query: '?raw', import: 'default' }) as Record<
  string,
  string
>;

describe('src/apps/** import boundary — no deep-import around @/platform (§3 C4)', () => {
  it('no source file under src/apps/ imports ws-client/connection-store/auth-store/i18n/theme-store directly', () => {
    const nonTestFiles = Object.entries(appFiles).filter(
      ([path]) => !path.endsWith('.test.ts') && !path.endsWith('.test.tsx'),
    );
    expect(nonTestFiles.length).toBeGreaterThan(0); // sanity: the glob actually found files

    const offenders = nonTestFiles
      .map(([path, src]) => ({ path, hits: violatingImports(src) }))
      .filter((r) => r.hits.length > 0);

    expect(offenders).toEqual([]);
  });

  it('sanity: the guard itself would catch a violation (meta-test on a synthetic sample)', () => {
    const sample = `import { client } from '@/lib/ws-client';\nimport { useAuthStore } from '@/stores/auth-store';\n`;
    expect(violatingImports(sample)).toEqual(['@/lib/ws-client', '@/stores/auth-store']);
  });
});
