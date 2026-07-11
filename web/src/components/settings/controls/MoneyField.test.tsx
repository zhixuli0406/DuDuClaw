import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { MoneyField, centsToDisplay, displayToCents } from './MoneyField';

describe('cents ↔ major-unit conversion', () => {
  it('cents → display', () => {
    expect(centsToDisplay(1250)).toBe('12.50');
    expect(centsToDisplay(0)).toBe('0.00');
    expect(centsToDisplay(5)).toBe('0.05');
    expect(centsToDisplay(100000)).toBe('1000.00');
  });

  it('display → cents', () => {
    expect(displayToCents('12.50')).toBe(1250);
    expect(displayToCents('12.5')).toBe(1250);
    expect(displayToCents('0.05')).toBe(5);
    expect(displayToCents('')).toBe(0);
    expect(displayToCents('-3')).toBe(0);
    expect(displayToCents('abc')).toBe(0);
  });
});

describe('<MoneyField>', () => {
  it('shows cents as major units and emits cents on edit', () => {
    const onChange = vi.fn();
    renderWithProviders(<MoneyField cents={1250} onChange={onChange} />);
    const input = screen.getByRole('spinbutton') as HTMLInputElement;
    expect(input.value).toBe('12.50');
    fireEvent.change(input, { target: { value: '20' } });
    expect(onChange).toHaveBeenLastCalledWith(2000);
  });
});
