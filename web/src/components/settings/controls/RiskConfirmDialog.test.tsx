import { expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { RiskConfirmDialog } from './RiskConfirmDialog';

const props = { open: true, title: 'Allow more access', body: <ul><li>More people can join.</li></ul>, onClose: vi.fn(), onConfirm: vi.fn() };
it('requires a labelled acknowledgement before confirming', async () => {
  const onConfirm = vi.fn();
  renderWithProviders(<RiskConfirmDialog {...props} onConfirm={onConfirm} />);
  expect(screen.getByRole('dialog', { name: props.title })).toBeInTheDocument();
  const confirm = screen.getByRole('button', { name: 'Enable' });
  expect(confirm).toBeDisabled();
  await userEvent.setup().click(screen.getByRole('checkbox', { name: 'I understand the effects of enabling this' }));
  expect(confirm).toBeEnabled();
  await userEvent.setup().click(confirm);
  expect(onConfirm).toHaveBeenCalledOnce();
});
it('resets the checkbox on reopen', async () => {
  const { rerender } = renderWithProviders(<RiskConfirmDialog {...props} />);
  await userEvent.setup().click(screen.getByRole('checkbox'));
  rerender(<RiskConfirmDialog {...props} open={false} />);
  rerender(<RiskConfirmDialog {...props} />);
  expect(screen.getByRole('checkbox')).not.toBeChecked();
  expect(screen.getByRole('button', { name: 'Enable' })).toBeDisabled();
});
it('shows the audit note by default and hides it with null', () => {
  const { rerender } = renderWithProviders(<RiskConfirmDialog {...props} />);
  expect(screen.getByText('Enabling this will be recorded in the audit log.')).toBeInTheDocument();
  rerender(<RiskConfirmDialog {...props} auditNote={null} />);
  expect(screen.queryByText('Enabling this will be recorded in the audit log.')).toBeNull();
});
it('blocks confirmation while busy and shows the supplied error', async () => {
  const { rerender } = renderWithProviders(<RiskConfirmDialog {...props} />);
  await userEvent.setup().click(screen.getByRole('checkbox'));
  rerender(<RiskConfirmDialog {...props} busy error="Please try again." />);
  expect(screen.getByRole('button', { name: 'Enable' })).toBeDisabled();
  expect(screen.getByRole('alert')).toHaveTextContent('Please try again.');
});
