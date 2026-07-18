/**
 * Class strings for raw native `<input>` / `<select>` elements styled to match
 * the MDS `Input` primitive. Used by the few shared controls that need a native
 * element (e.g. `<select>` with `<optgroup>`, which the base-ui Select cannot
 * express). Prefer the MDS `Input` / `Select` components where a native element
 * is not required.
 */
export const inputClass =
  'h-8 w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 text-sm outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 dark:bg-input/30';

export const selectClass = inputClass;
