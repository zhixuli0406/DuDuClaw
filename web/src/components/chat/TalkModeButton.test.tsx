import { describe, it, expect, vi, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { TalkModeStatusPill } from './TalkModeButton';

describe('<TalkModeStatusPill>', () => {
  const originalMatchMedia = window.matchMedia;

  afterEach(() => {
    window.matchMedia = originalMatchMedia;
    vi.restoreAllMocks();
  });

  it('does not render for the idle status', () => {
    const { container } = renderWithProviders(<TalkModeStatusPill status="idle" />);
    expect(container).toBeEmptyDOMElement();
  });

  it('announces the listening state once via the pill text, not the decorative orb', () => {
    renderWithProviders(<TalkModeStatusPill status="listening" />);

    // The pill itself is the single accessible announcement…
    const pill = screen.getByRole('status');
    expect(pill).toHaveTextContent('Listening…');

    // …the orb inside it must not add a second "img" accessible node, or a
    // screen reader would read "Listening… 聆聽中…" back to back.
    expect(screen.queryByRole('img')).toBeNull();
  });

  it('clamps the listening orb to the same 14px footprint as the other status icons', () => {
    const { container } = renderWithProviders(<TalkModeStatusPill status="listening" />);
    const clip = container.querySelector('.size-3\\.5.overflow-hidden');
    expect(clip).not.toBeNull();
    const canvas = clip?.querySelector('canvas');
    expect(canvas).not.toBeNull();
    expect(canvas?.className).toContain('scale-[0.7]');
  });

  it('still shows the decorative orb (hidden a11y-wise) under reduced motion as a static glyph', () => {
    window.matchMedia = vi.fn().mockReturnValue({
      matches: true,
      media: '(prefers-reduced-motion: reduce)',
      addEventListener: () => {},
      removeEventListener: () => {},
    }) as unknown as typeof window.matchMedia;

    const { container } = renderWithProviders(<TalkModeStatusPill status="listening" />);
    expect(container.querySelector('canvas')).toBeNull();
    const glyph = container.querySelector('[data-thinking-state="static"]');
    expect(glyph).not.toBeNull();
    expect(glyph).toHaveAttribute('aria-hidden', 'true');
    expect(screen.queryByRole('img')).toBeNull();
  });
});
