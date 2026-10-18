/**
 * Removed CLI runtimes can remain in an old saved agent configuration. Show
 * their names for migration, without offering them as new choices.
 */
export const REMOVED_RUNTIMES: ReadonlyArray<string> = ['gemini'];

export function isRemovedRuntime(id: string | null | undefined): boolean {
  return typeof id === 'string' && REMOVED_RUNTIMES.includes(id);
}
