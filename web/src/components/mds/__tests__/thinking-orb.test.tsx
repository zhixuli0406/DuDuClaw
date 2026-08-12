import { describe, it, expect, vi, afterEach } from 'vitest';
import { act, screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { ThinkingOrb, type ThinkingOrbState } from '../thinking-orb';

/**
 * Minimal MediaQueryList mock so we can drive `prefers-reduced-motion` from
 * the test without a jsdom polyfill for the real API — supports exactly the
 * subset `ThinkingOrb`'s reduced-motion gate relies on: `.matches` plus
 * `addEventListener('change', …)`.
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

const ALL_STATES: ThinkingOrbState[] = ['working', 'searching', 'solving', 'listening', 'composing', 'shaping'];

describe('<ThinkingOrb>', () => {
  const originalMatchMedia = window.matchMedia;

  afterEach(() => {
    window.matchMedia = originalMatchMedia;
    vi.restoreAllMocks();
  });

  describe('without a reduced-motion preference', () => {
    it.each(ALL_STATES)('renders an accessible canvas for the "%s" state', (state) => {
      const { mql } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      renderWithProviders(<ThinkingOrb state={state} size={64} label={`${state}…`} />);
      const el = screen.getByRole('img', { name: `${state}…` });
      expect(el.tagName).toBe('CANVAS');
      expect(el).toHaveAttribute('data-thinking-state', 'canvas');
    });

    it('sizes the canvas element from the `size` prop', () => {
      const { mql } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      renderWithProviders(<ThinkingOrb state="searching" size={20} label="Searching…" />);
      const el = screen.getByRole('img', { name: 'Searching…' });
      expect(el).toHaveStyle({ width: '20px', height: '20px' });
    });
  });

  describe('reduced-motion gate (JS-driven, per web/DESIGN.md §1.7)', () => {
    it('renders a static, motion-free glyph instead of a canvas when the OS prefers reduced motion', () => {
      const { mql } = mockMatchMedia(true);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(<ThinkingOrb state="working" size={64} label="Thinking…" />);

      expect(container.querySelector('canvas')).toBeNull();
      const el = screen.getByRole('img', { name: 'Thinking…' });
      expect(el.tagName).not.toBe('CANVAS');
      expect(el).toHaveAttribute('data-thinking-state', 'static');
    });

    it('switches live from canvas to static glyph when the media query flips', () => {
      const { mql, trigger } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(<ThinkingOrb state="composing" size={20} label="Composing…" />);
      expect(container.querySelector('canvas')).not.toBeNull();

      act(() => trigger(true));

      expect(container.querySelector('canvas')).toBeNull();
      expect(screen.getByRole('img')).toHaveAttribute('data-thinking-state', 'static');
    });

    it('switches back to the canvas orb when reduced motion is turned off again', () => {
      const { mql, trigger } = mockMatchMedia(true);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(<ThinkingOrb state="solving" size={20} label="Reviewing…" />);
      expect(container.querySelector('canvas')).toBeNull();

      act(() => trigger(false));

      expect(container.querySelector('canvas')).not.toBeNull();
    });

    it('falls back to rendering the animated canvas rather than throwing when matchMedia is unavailable', () => {
      // @ts-expect-error -- simulate an environment without matchMedia support (older WebView)
      window.matchMedia = undefined;
      expect(() =>
        renderWithProviders(<ThinkingOrb state="shaping" size={20} label="Synthesizing…" />),
      ).not.toThrow();
      expect(screen.getByRole('img').tagName).toBe('CANVAS');
    });
  });

  describe('decorative mode', () => {
    it('hides the canvas orb from the accessibility tree', () => {
      const { mql } = mockMatchMedia(false);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      const { container } = renderWithProviders(
        <ThinkingOrb state="listening" size={20} label="Listening…" decorative />,
      );

      const canvas = container.querySelector('canvas');
      expect(canvas).not.toBeNull();
      expect(canvas).toHaveAttribute('aria-hidden', 'true');
      expect(screen.queryByRole('img')).toBeNull();
    });

    it('hides the reduced-motion static glyph from the accessibility tree too', () => {
      const { mql } = mockMatchMedia(true);
      window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;

      renderWithProviders(<ThinkingOrb state="listening" size={20} label="Listening…" decorative />);

      expect(screen.queryByRole('img')).toBeNull();
      const el = document.querySelector('[data-thinking-state="static"]');
      expect(el).toHaveAttribute('aria-hidden', 'true');
      expect(el).not.toHaveAttribute('aria-label');
    });
  });
});
