import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { ThinkingOrbIndicator, type ThinkingState } from './ThinkingOrbIndicator';

/**
 * Minimal MediaQueryList mock so we can drive `prefers-reduced-motion` from
 * the test without a jsdom polyfill for the real API. Supports exactly the
 * subset ThinkingOrbIndicator's own gate (and thinking-orbs' internal one)
 * relies on: `.matches` plus `addEventListener('change', …)`.
 */
function mockMatchMedia(initialMatches: boolean) {
  let matches = initialMatches;
  const listeners = new Set<(e: { matches: boolean }) => void>();
  const mql = {
    get matches() {
      return matches;
    },
    media: '(prefers-reduced-motion: reduce)',
    addEventListener: (_type: string, cb: (e: { matches: boolean }) => void) => {
      listeners.add(cb);
    },
    removeEventListener: (_type: string, cb: (e: { matches: boolean }) => void) => {
      listeners.delete(cb);
    },
  };
  const trigger = (next: boolean) => {
    matches = next;
    for (const cb of listeners) cb({ matches: next });
  };
  return { mql, trigger };
}

const ALL_STATES: ThinkingState[] = ['waiting', 'inline', 'searching', 'solving', 'listening', 'shaping'];

describe('<ThinkingOrbIndicator>', () => {
  const originalMatchMedia = window.matchMedia;

  afterEach(() => {
    window.matchMedia = originalMatchMedia;
    vi.restoreAllMocks();
  });

  describe('without a reduced-motion preference', () => {
    beforeEach(() => {
      const { mql } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;
    });

    it.each(ALL_STATES)('renders the tuned canvas orb for state "%s"', (state) => {
      renderWithProviders(<ThinkingOrbIndicator state={state} />);
      const el = screen.getByRole('img');
      expect(el.tagName).toBe('CANVAS');
    });

    it('uses a localized default label per state', () => {
      renderWithProviders(<ThinkingOrbIndicator state="searching" />);
      expect(screen.getByRole('img', { name: 'Searching…' })).toBeInTheDocument();
    });

    it('accepts a label override', () => {
      renderWithProviders(<ThinkingOrbIndicator state="waiting" label="Custom label" />);
      expect(screen.getByRole('img', { name: 'Custom label' })).toBeInTheDocument();
    });
  });

  describe('reduced-motion gate', () => {
    it('renders a static, motion-free glyph instead of a canvas when the OS prefers reduced motion', () => {
      const { mql } = mockMatchMedia(true);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(<ThinkingOrbIndicator state="inline" />);

      expect(container.querySelector('canvas')).toBeNull();
      const el = screen.getByRole('img');
      expect(el.tagName).not.toBe('CANVAS');
      expect(el).toHaveAttribute('data-thinking-state', 'static');
    });

    it('switches live from canvas to static glyph when the media query flips (JS gate, not CSS)', () => {
      const { mql, trigger } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(<ThinkingOrbIndicator state="inline" />);
      expect(container.querySelector('canvas')).not.toBeNull();

      act(() => trigger(true));

      expect(container.querySelector('canvas')).toBeNull();
      expect(screen.getByRole('img')).toHaveAttribute('data-thinking-state', 'static');
    });

    it('switches back to the canvas orb when reduced motion is turned off again', () => {
      const { mql, trigger } = mockMatchMedia(true);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(<ThinkingOrbIndicator state="listening" />);
      expect(container.querySelector('canvas')).toBeNull();

      act(() => trigger(false));

      expect(container.querySelector('canvas')).not.toBeNull();
    });
  });

  describe('when matchMedia is unavailable (older WebView)', () => {
    it('falls back to rendering the animated orb rather than throwing', () => {
      // @ts-expect-error -- simulate an environment without matchMedia support
      window.matchMedia = undefined;
      expect(() => renderWithProviders(<ThinkingOrbIndicator state="shaping" />)).not.toThrow();
      expect(screen.getByRole('img').tagName).toBe('CANVAS');
    });
  });

  describe('decorative mode (ancestor already announces the state as text)', () => {
    it('hides the canvas orb from the accessibility tree', () => {
      const { mql } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(<ThinkingOrbIndicator state="listening" decorative />);

      // Still visually renders the canvas…
      const canvas = container.querySelector('canvas');
      expect(canvas).not.toBeNull();
      expect(canvas).toHaveAttribute('aria-hidden', 'true');
      // …but is not exposed as an accessible "img" (would double-announce
      // "Listening… Listening…" alongside an ancestor role="status" pill).
      expect(screen.queryByRole('img')).toBeNull();
    });

    it('hides the reduced-motion static glyph from the accessibility tree too', () => {
      const { mql } = mockMatchMedia(true);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      renderWithProviders(<ThinkingOrbIndicator state="listening" decorative />);

      expect(screen.queryByRole('img')).toBeNull();
      const el = document.querySelector('[data-thinking-state="static"]');
      expect(el).toHaveAttribute('aria-hidden', 'true');
      expect(el).not.toHaveAttribute('aria-label');
    });

    it('still exposes the accessible label when not decorative (default)', () => {
      const { mql } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      renderWithProviders(<ThinkingOrbIndicator state="listening" />);
      expect(screen.getByRole('img', { name: 'Listening…' })).toBeInTheDocument();
    });
  });
});
