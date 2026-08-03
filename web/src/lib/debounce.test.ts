import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { debounceTrailing, REFRESH_DEBOUNCE_MS } from './debounce';

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('debounceTrailing', () => {
  it('collapses a burst into a single trailing call', () => {
    const fn = vi.fn();
    const d = debounceTrailing(fn, 400);

    // A distill pass raising one event per persisted fact.
    for (let i = 0; i < 8; i++) d();
    expect(fn).not.toHaveBeenCalled();

    vi.advanceTimersByTime(400);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('restarts the window on each call rather than firing mid-burst', () => {
    const fn = vi.fn();
    const d = debounceTrailing(fn, 400);

    d();
    vi.advanceTimersByTime(300);
    d(); // still inside the window — pushes the deadline out
    vi.advanceTimersByTime(300);
    expect(fn).not.toHaveBeenCalled();

    vi.advanceTimersByTime(100);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('fires again for a genuinely separate burst', () => {
    const fn = vi.fn();
    const d = debounceTrailing(fn, 400);

    d();
    vi.advanceTimersByTime(400);
    d();
    vi.advanceTimersByTime(400);
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it('passes the latest arguments through', () => {
    const fn = vi.fn();
    const d = debounceTrailing(fn, 400);

    d('first');
    d('last');
    vi.advanceTimersByTime(400);
    expect(fn).toHaveBeenCalledWith('last');
  });

  it('cancel drops a pending call so an unmounted view never refetches', () => {
    const fn = vi.fn();
    const d = debounceTrailing(fn, 400);

    d();
    d.cancel();
    vi.advanceTimersByTime(1000);
    expect(fn).not.toHaveBeenCalled();
  });

  it('defaults to a burst window inside the "feels instant" budget', () => {
    expect(REFRESH_DEBOUNCE_MS).toBeGreaterThanOrEqual(300);
    expect(REFRESH_DEBOUNCE_MS).toBeLessThanOrEqual(800);
  });
});
