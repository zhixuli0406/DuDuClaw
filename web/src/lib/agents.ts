/**
 * Shared agent-derived helpers.
 */

/**
 * The sorted, de-duplicated set of non-empty department names in use across the
 * given agents. Used to populate the `department:<dept>` scope option and the
 * agent-editor datalist. Output is identical to the inline expression it
 * replaces: `Array.from(new Set(...filter(non-empty))).sort()`.
 */
export function departmentsOf(
  agents: ReadonlyArray<{ department?: string | null }>,
): string[] {
  return Array.from(
    new Set(agents.map((a) => a.department).filter((d): d is string => !!d && d.length > 0)),
  ).sort();
}
