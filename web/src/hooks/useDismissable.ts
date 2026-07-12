import { useEffect, type RefObject } from 'react';

/**
 * Dismiss an open overlay (popover / menu / dropdown) on an outside mousedown or
 * an Escape keypress. Behaviour is identical to the hand-rolled effect it
 * replaces: listeners are attached only while `open` is true, an outside
 * `mousedown` (target not contained by `ref`) or `Escape` calls `onClose`, and
 * both listeners are removed on cleanup.
 *
 * @param ref    Root element of the overlay; a mousedown inside it is ignored.
 * @param open   Whether the overlay is currently open.
 * @param onClose Called to request dismissal.
 */
export function useDismissable(
  ref: RefObject<HTMLElement | null>,
  open: boolean,
  onClose: () => void,
): void {
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('mousedown', onDown);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('mousedown', onDown);
      window.removeEventListener('keydown', onKey);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
}
