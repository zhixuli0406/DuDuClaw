import { describe, it, expect } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Input } from '../input';

describe('<Input>', () => {
  it('renders with base classes and forwards placeholder', () => {
    renderWithProviders(<Input placeholder="Search" />);
    const el = screen.getByPlaceholderText('Search');
    expect(el).toHaveClass('h-8', 'rounded-lg', 'border-input');
    expect(el).toHaveAttribute('type', 'text');
  });

  it('accepts typing', () => {
    renderWithProviders(<Input aria-label="name" />);
    const el = screen.getByLabelText('name') as HTMLInputElement;
    fireEvent.change(el, { target: { value: 'abc' } });
    expect(el.value).toBe('abc');
  });
});
