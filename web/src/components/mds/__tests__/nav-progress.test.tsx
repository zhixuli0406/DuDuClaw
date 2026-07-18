import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { NavProgress } from '../nav-progress';

describe('<NavProgress>', () => {
  it('renders nothing when inactive', () => {
    const { container } = renderWithProviders(<NavProgress active={false} />);
    expect(container.querySelector('[data-slot="nav-progress"]')).toBeNull();
  });

  it('renders the sweeping brand bar when active', () => {
    const { container } = renderWithProviders(<NavProgress active />);
    const bar = screen.getByRole('progressbar');
    expect(bar).toHaveAttribute('aria-busy', 'true');
    expect(container.querySelector('.nav-progress-sweep')).toHaveClass('bg-brand');
  });
});
