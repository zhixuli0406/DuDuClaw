import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Segmented } from '../segmented';

const options = [
  { value: 'day', label: 'Day' },
  { value: 'week', label: 'Week' },
] as const;

describe('<Segmented>', () => {
  it('marks the selected option and styles the track', () => {
    renderWithProviders(
      <Segmented
        value="day"
        onValueChange={() => {}}
        options={options}
        aria-label="range"
      />
    );
    expect(screen.getByRole('radiogroup', { name: 'range' })).toHaveClass(
      'bg-muted',
      'rounded-md'
    );
    const day = screen.getByRole('radio', { name: 'Day' });
    expect(day).toHaveAttribute('aria-checked', 'true');
    expect(day).toHaveClass('bg-background', 'shadow-sm');
    expect(screen.getByRole('radio', { name: 'Week' })).toHaveAttribute(
      'aria-checked',
      'false'
    );
  });

  it('reports the chosen value', () => {
    const onValueChange = vi.fn();
    renderWithProviders(
      <Segmented value="day" onValueChange={onValueChange} options={options} />
    );
    fireEvent.click(screen.getByRole('radio', { name: 'Week' }));
    expect(onValueChange).toHaveBeenCalledWith('week');
  });
});
