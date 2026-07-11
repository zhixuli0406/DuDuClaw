import { useRef, useState, type ReactNode, type PointerEvent } from 'react';
import { Archive } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * SwipeToArchive — touch left-swipe to archive an inbox row (paperclip P3
 * mobile). Drag the row left past `threshold` px and release to fire
 * `onArchive`; release short and it springs back. A red "archive" backing is
 * revealed under the row as it moves. Pointer events cover touch + mouse.
 *
 * Not the only way to archive (the `a` keyboard shortcut / row menu remain) —
 * this is the mobile affordance, so no extra a11y burden beyond a labelled
 * backing.
 */
export function SwipeToArchive({
  children,
  onArchive,
  threshold = 96,
  className,
}: {
  children: ReactNode;
  onArchive: () => void;
  threshold?: number;
  className?: string;
}) {
  const [dx, setDx] = useState(0);
  const [dragging, setDragging] = useState(false);
  const startX = useRef(0);
  const active = useRef(false);

  const onPointerDown = (e: PointerEvent) => {
    // Ignore secondary buttons; only start on primary press.
    if (e.button !== 0 && e.pointerType === 'mouse') return;
    active.current = true;
    startX.current = e.clientX;
    setDragging(true);
    // Not implemented in jsdom / very old engines — best-effort.
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent) => {
    if (!active.current) return;
    // Only track leftward movement (archive gesture); clamp rightward to 0.
    const delta = Math.min(0, e.clientX - startX.current);
    setDx(delta);
  };
  const finish = () => {
    if (!active.current) return;
    active.current = false;
    setDragging(false);
    if (-dx >= threshold) {
      onArchive();
      setDx(0);
    } else {
      setDx(0); // spring back
    }
  };

  const revealed = -dx > 8;

  return (
    <div className={cn('relative overflow-hidden', className)}>
      {/* Archive backing, revealed as the row slides left. */}
      <div
        aria-hidden={!revealed}
        className={cn(
          'absolute inset-y-0 right-0 flex items-center gap-1.5 rounded-card bg-rose-500/90 px-4 text-white transition-opacity',
          revealed ? 'opacity-100' : 'opacity-0',
        )}
      >
        <Archive className="h-4 w-4" />
      </div>
      <div
        role="presentation"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={finish}
        onPointerCancel={finish}
        style={{ transform: `translateX(${dx}px)`, touchAction: 'pan-y' }}
        className={cn('relative', !dragging && 'transition-transform duration-200')}
      >
        {children}
      </div>
    </div>
  );
}
