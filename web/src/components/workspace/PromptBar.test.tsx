import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { PromptBar } from './PromptBar';

const onSubmit = vi.fn();
const onChange = vi.fn();

function setup(props: Partial<Parameters<typeof PromptBar>[0]> = {}) {
  return renderWithProviders(
    <PromptBar value="" onChange={onChange} onSubmit={onSubmit} {...props} />,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe('PromptBar', () => {
  it('reports every keystroke to the owner (controlled)', async () => {
    const user = userEvent.setup();
    setup();
    await user.type(screen.getByLabelText(/enter a prompt/i), 'hi');
    expect(onChange).toHaveBeenCalled();
    expect(onChange.mock.calls.at(-1)?.[0]).toBe('i');
  });

  it('submits on Enter when there is text', async () => {
    const user = userEvent.setup();
    setup({ value: 'ship it' });
    screen.getByLabelText(/enter a prompt/i).focus();
    await user.keyboard('{Enter}');
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });

  it('does not submit an empty or whitespace-only prompt', async () => {
    const user = userEvent.setup();
    setup({ value: '   ' });
    screen.getByLabelText(/enter a prompt/i).focus();
    await user.keyboard('{Enter}');
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('Shift+Enter inserts a newline instead of submitting', async () => {
    const user = userEvent.setup();
    setup({ value: 'line1' });
    screen.getByLabelText(/enter a prompt/i).focus();
    await user.keyboard('{Shift>}{Enter}{/Shift}');
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('disables the composer while the caller is busy', () => {
    setup({ disabled: true });
    expect(screen.getByLabelText(/enter a prompt/i)).toBeDisabled();
  });

  it('renders the control row and hides the inline send when the caller owns the CTA', () => {
    setup({ showSubmit: false, controls: <button type="button">pick</button> });
    expect(screen.getByRole('button', { name: 'pick' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /send/i })).not.toBeInTheDocument();
  });
});
