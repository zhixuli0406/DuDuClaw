import { useEffect, useState } from 'react';

/**
 * RAF-driven elapsed-time clock in seconds since mount, used to drive DuDu's
 * idle bob, blink, and wave. Returns a frozen `0` (never starting a RAF loop)
 * when `active` is false OR the user prefers reduced motion — so reduced-motion
 * DuDu renders a completely static resting pose (§7.1 "reduced-motion 全靜態").
 */
export function useDuduClock(active = true): number {
  const [seconds, setSeconds] = useState(0);

  useEffect(() => {
    if (!active) return;
    if (
      typeof window !== 'undefined' &&
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches
    ) {
      return; // reduced motion → stay at the resting frame
    }
    let raf = 0;
    const start = window.performance.now();
    const tick = (now: number) => {
      setSeconds((now - start) / 1000);
      raf = window.requestAnimationFrame(tick);
    };
    raf = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(raf);
  }, [active]);

  return seconds;
}
