/**
 * Agent runtimes the dashboard no longer OFFERS. A value already saved on an
 * agent / credential stays visible and is labelled "deprecated"; nothing is
 * silently substituted. Mirrors the gateway's `runtime.detect` `deprecated`
 * flag (the Gemini API *provider* is a different thing and is not listed here).
 */
export const DEPRECATED_RUNTIMES: ReadonlyArray<string> = ['gemini'];

export function isDeprecatedRuntime(id: string | null | undefined): boolean {
  return typeof id === 'string' && DEPRECATED_RUNTIMES.includes(id);
}
