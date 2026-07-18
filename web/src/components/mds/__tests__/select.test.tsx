import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '../select';

function Fixture({
  onValueChange,
}: {
  onValueChange?: (v: string | null) => void;
}) {
  return (
    <Select onValueChange={onValueChange}>
      <SelectTrigger aria-label="fruit">
        <SelectValue placeholder="Pick one" />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="apple">Apple</SelectItem>
        <SelectItem value="banana">Banana</SelectItem>
      </SelectContent>
    </Select>
  );
}

describe('<Select>', () => {
  it('renders a styled trigger with placeholder', () => {
    renderWithProviders(<Fixture />);
    const trigger = screen.getByRole('combobox', { name: 'fruit' });
    expect(trigger).toHaveClass('rounded-lg', 'border-input');
    expect(trigger).toHaveTextContent('Pick one');
  });

  it('opens the listbox and reports the chosen value', async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    renderWithProviders(<Fixture onValueChange={onValueChange} />);

    await user.click(screen.getByRole('combobox', { name: 'fruit' }));
    await screen.findByRole('listbox');
    await user.click(screen.getByRole('option', { name: 'Banana' }));

    expect(onValueChange).toHaveBeenCalledWith('banana', expect.anything());
  });
});
