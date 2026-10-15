import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { BrandMark } from './BrandMark';

describe('BrandMark', () => {
  it('exposes an accessible name when titled', () => {
    const { getByRole } = render(<BrandMark title="DuDuClaw" />);
    expect(getByRole('img').getAttribute('aria-label')).toBe('DuDuClaw');
  });

  it('is decorative (aria-hidden) without a title', () => {
    const { container } = render(<BrandMark />);
    expect(container.querySelector('svg')?.getAttribute('aria-hidden')).toBe('true');
  });

  it('paints the ink from --logo-ink and the tail from --logo-accent', () => {
    const { container } = render(<BrandMark size={40} />);
    const svg = container.querySelector('svg')!;
    expect(svg.getAttribute('width')).toBe('40');
    const groups = svg.querySelectorAll('g');
    expect(groups[0].getAttribute('style')).toContain('var(--logo-ink)');
    expect(groups[1].getAttribute('style')).toContain('var(--logo-accent)');
    expect(svg.querySelectorAll('path')).toHaveLength(3);
  });

  it('uses currentColor for both layers in mono mode', () => {
    const { container } = render(<BrandMark mono />);
    for (const g of container.querySelectorAll('g')) {
      expect(g.getAttribute('style')).toContain('currentcolor');
    }
  });
});
