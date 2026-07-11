import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { PriorityIcon } from './PriorityIcon';

describe('<PriorityIcon>', () => {
  it('renders a labelled priority glyph', () => {
    renderWithProviders(<PriorityIcon priority="urgent" />);
    const el = screen.getByRole('img', { name: 'Urgent' });
    expect(el).toBeInTheDocument();
  });

  it('supports all four priorities', () => {
    for (const p of ['low', 'medium', 'high', 'urgent'] as const) {
      const { unmount } = renderWithProviders(<PriorityIcon priority={p} />);
      expect(screen.getByRole('img')).toBeInTheDocument();
      unmount();
    }
  });
});
