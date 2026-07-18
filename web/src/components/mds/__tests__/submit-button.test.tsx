import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { SubmitButton } from '../submit-button';

describe('<SubmitButton>', () => {
  it('idle state is a clickable send button', () => {
    const onClick = vi.fn();
    renderWithProviders(<SubmitButton state="idle" onClick={onClick} />);
    const btn = screen.getByRole('button', { name: 'Send' });
    expect(btn).not.toBeDisabled();
    expect(btn).not.toHaveAttribute('aria-busy');
    fireEvent.click(btn);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it('submitting state is busy + disabled', () => {
    renderWithProviders(<SubmitButton state="submitting" />);
    const btn = screen.getByRole('button', { name: 'Sending' });
    expect(btn).toBeDisabled();
    expect(btn).toHaveAttribute('aria-busy', 'true');
    expect(btn).toHaveAttribute('data-state', 'submitting');
  });

  it('streaming state is a clickable stop button', () => {
    const onClick = vi.fn();
    renderWithProviders(<SubmitButton state="streaming" onClick={onClick} />);
    const btn = screen.getByRole('button', { name: 'Stop' });
    expect(btn).not.toBeDisabled();
    fireEvent.click(btn);
    expect(onClick).toHaveBeenCalledTimes(1);
  });
});
