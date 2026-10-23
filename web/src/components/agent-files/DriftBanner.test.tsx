import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import en from '@/i18n/en.json';
import { renderWithProviders } from '@/test/render';
import { DriftBanner } from './DriftBanner';

describe('<DriftBanner>', () => {
  it('offers restore and accept to someone who may edit', async () => {
    const onRestore = vi.fn();
    const onAccept = vi.fn();
    renderWithProviders(<DriftBanner acceptedVersionId="1-0123456789ab" canEdit onRestore={onRestore} onAccept={onAccept} />);
    expect(screen.getByText(en['agentFiles.drift.title'])).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.drift.restore'] }));
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.drift.accept'] }));
    expect(onRestore).toHaveBeenCalledTimes(1);
    expect(onAccept).toHaveBeenCalledTimes(1);
  });

  it('without a kept approved version only offers accept', () => {
    renderWithProviders(<DriftBanner acceptedVersionId={null} canEdit onRestore={vi.fn()} onAccept={vi.fn()} />);
    expect(screen.queryByRole('button', { name: en['agentFiles.drift.restore'] })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: en['agentFiles.drift.accept'] })).toBeInTheDocument();
    expect(screen.getByText(en['agentFiles.drift.noApprovedVersion'])).toBeInTheDocument();
  });

  it('shows no buttons to someone who can only view', () => {
    renderWithProviders(<DriftBanner acceptedVersionId="1-0123456789ab" canEdit={false} onRestore={vi.fn()} onAccept={vi.fn()} />);
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
    expect(screen.getByText(en['agentFiles.drift.ownerOnly'])).toBeInTheDocument();
  });

  it('disables the buttons while a check runs', () => {
    renderWithProviders(<DriftBanner acceptedVersionId="1-0123456789ab" canEdit busy onRestore={vi.fn()} onAccept={vi.fn()} />);
    for (const b of screen.getAllByRole('button')) expect(b).toBeDisabled();
  });
});
