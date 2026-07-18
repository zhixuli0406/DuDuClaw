import { describe, it, expect } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Textarea } from '../textarea';

describe('<Textarea>', () => {
  it('renders with base classes', () => {
    renderWithProviders(<Textarea aria-label="notes" placeholder="Notes" />);
    const el = screen.getByLabelText('notes');
    expect(el).toHaveClass('rounded-lg', 'border-input', 'min-h-16');
  });

  it('accepts typing', () => {
    renderWithProviders(<Textarea aria-label="notes" />);
    const el = screen.getByLabelText('notes') as HTMLTextAreaElement;
    fireEvent.change(el, { target: { value: 'hello' } });
    expect(el.value).toBe('hello');
  });
});
