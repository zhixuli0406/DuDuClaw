import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { CoinChip } from './CoinChip';

describe('<CoinChip>', () => {
  it('formats cents as USD by default', () => {
    renderWithProviders(<CoinChip cents={1234} />);
    expect(screen.getByText('$12.34')).toBeInTheDocument();
  });

  it('rounds TWD and fires onClick', () => {
    const onClick = vi.fn();
    renderWithProviders(<CoinChip cents={123_400} currency="TWD" onClick={onClick} />);
    const el = screen.getByText('NT$1,234');
    expect(el).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button'));
    expect(onClick).toHaveBeenCalled();
  });
});
