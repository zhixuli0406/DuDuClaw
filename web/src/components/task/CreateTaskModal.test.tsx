import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { CreateTaskModal } from './CreateTaskModal';

const AGENTS = [{ name: 'nova', display_name: 'Nova' }] as never;

function setup(onCreate = vi.fn().mockResolvedValue({ id: 't1' })) {
  const onClose = vi.fn();
  renderWithProviders(
    <CreateTaskModal open onClose={onClose} agents={AGENTS} onCreate={onCreate} />,
  );
  fireEvent.change(screen.getByPlaceholderText('Task title'), { target: { value: 'Write report' } });
  return { onCreate, onClose };
}

const submit = () =>
  fireEvent.click(screen.getAllByRole('button', { name: 'New Task' }).at(-1)!);

describe('<CreateTaskModal> assignee is required (the gateway refuses tasks without one)', () => {
  it('blocks submit without an assignee and says why', async () => {
    const { onCreate, onClose } = setup();
    submit();
    expect(await screen.findByText('Choose the AI employee to assign this to.')).toBeInTheDocument();
    expect(onCreate).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('no longer offers an "unassigned" choice', () => {
    setup();
    fireEvent.click(screen.getByRole('button', { name: 'Assign' }));
    expect(screen.queryByRole('menuitemradio', { name: /Unassigned/i })).toBeNull();
  });

  it('sends assigned_to once an assignee is picked', async () => {
    const { onCreate, onClose } = setup();
    fireEvent.click(screen.getByRole('button', { name: 'Assign' }));
    fireEvent.click(screen.getByRole('menuitemradio', { name: /Nova/ }));
    submit();
    await waitFor(() => expect(onCreate).toHaveBeenCalledTimes(1));
    expect(onCreate.mock.calls[0][0]).toMatchObject({ title: 'Write report', assigned_to: 'nova' });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it('keeps the dialog open and says so when the create is refused', async () => {
    const { onClose } = setup(vi.fn().mockResolvedValue(null));
    fireEvent.click(screen.getByRole('button', { name: 'Assign' }));
    fireEvent.click(screen.getByRole('menuitemradio', { name: /Nova/ }));
    submit();
    expect(await screen.findByText('The task was not created. Please try again.')).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });
});
