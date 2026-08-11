import { useEffect, type RefObject } from 'react';

const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'textarea:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

/**
 * Keep keyboard Tab navigation inside `ref` while `active` is true.
 *
 * Written for the W3-4 developer panel's expanded pane: it is a persistent
 * overlay with no backdrop (the rest of the app stays clickable behind it,
 * devtools-style), so a full modal `Dialog` — which also traps *pointer*
 * interaction via its backdrop — is the wrong primitive. This hook does only
 * the keyboard half of dialog focus management (WAI-ARIA dialog pattern:
 * https://www.w3.org/WAI/ARIA/apg/patterns/dialog-modal/), leaving pointer
 * interaction with the rest of the page untouched.
 *
 * On activation: if focus isn't already somewhere inside `ref`, moves it to
 * the first focusable descendant. On deactivation: restores focus to
 * whatever was focused before activation (if it still exists in the DOM).
 */
export function useFocusTrap(ref: RefObject<HTMLElement | null>, active: boolean): void {
  useEffect(() => {
    if (!active) return;
    const container = ref.current;
    if (!container) return;

    const previouslyFocused = document.activeElement as HTMLElement | null;
    if (!container.contains(document.activeElement)) {
      container.querySelector<HTMLElement>(FOCUSABLE_SELECTOR)?.focus();
    }

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Tab') return;
      const focusable = Array.from(
        container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
      );
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const insideContainer = container.contains(document.activeElement);

      if (e.shiftKey) {
        if (!insideContainer || document.activeElement === first) {
          e.preventDefault();
          last.focus();
        }
      } else {
        if (!insideContainer || document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };

    container.addEventListener('keydown', onKeyDown);
    return () => {
      container.removeEventListener('keydown', onKeyDown);
      if (previouslyFocused && document.contains(previouslyFocused)) {
        previouslyFocused.focus();
      }
    };
  }, [active, ref]);
}
