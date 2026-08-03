import { ThinkingOrbIndicator } from './ThinkingOrbIndicator';

/**
 * Inline "composing a reply" glyph shown while the assistant is streaming a
 * reply but hasn't produced any partial text yet. Backed by `ThinkingOrbIndicator`
 * (state="inline" → thinking-orbs' `composing` animation, size 20) with a
 * built-in static fallback under `prefers-reduced-motion: reduce`.
 */
export function TypingIndicator() {
  return (
    <div className="flex justify-start">
      <div className="flex items-center gap-2 rounded-2xl border border-surface-border bg-surface px-4 py-3">
        <ThinkingOrbIndicator state="inline" />
      </div>
    </div>
  );
}
