import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { ChatArtifactCard } from './ChatArtifactCard';
import {
  FIXTURE_DEVICE_STATUS,
  FIXTURE_UPDATE_STATUS,
  FIXTURE_BACKUP_CREATED,
  FIXTURE_BACKUP_LIST,
  FIXTURE_NETWORK_INFO,
  FIXTURE_CONFIRM_RESTART,
  FIXTURE_UPDATE_CONFIRM_DEVICE,
  FIXTURE_UPDATE_CONFIRM_SYSTEM,
  FIXTURE_APPROVAL_REQUEST,
  ALL_FIXTURES,
} from './fixtures';

/**
 * O-3's "以 mock/fixture 資料展示" proof: every artifact type renders a
 * sensible card off nothing but its fixture payload — this is the contract
 * O-1/O-4 need to hit when they start attaching real `device.*`/`approvals.*`
 * results (see the WIRING TODO in `artifact-types.ts`).
 */
describe('<ChatArtifactCard> — dispatch + fixture showcase', () => {
  it('renders every fixture without throwing', () => {
    for (const artifact of ALL_FIXTURES) {
      const { unmount } = renderWithProviders(<ChatArtifactCard artifact={artifact} />);
      unmount();
    }
  });

  it('device_status: shows CPU cores, RAM usage, and a link to the full 裝置 page', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_DEVICE_STATUS} />);
    expect(screen.getByText('Device status')).toBeInTheDocument();
    expect(screen.getByText('4')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /View full page/ })).toHaveAttribute('href', '/device');
  });

  it('update_status: shows the check-vs-apply heading from the payload', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_UPDATE_STATUS} />);
    expect(screen.getByText('Update check complete')).toBeInTheDocument();
    expect(screen.getByText('Update complete')).toBeInTheDocument(); // success badge
  });

  it('backup_result (created): renders a download button for the fresh archive', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_BACKUP_CREATED} />);
    expect(screen.getByText('Backup created')).toBeInTheDocument();
    expect(screen.getByText('duduclaw-backup-20260818.tar.gz')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Download' })).toBeInTheDocument();
  });

  it('backup_result (created): the download button does not throw when clicked', async () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_BACKUP_CREATED} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Download' }));
  });

  it('backup_result (list): renders every stored file with a download affordance', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_BACKUP_LIST} />);
    expect(screen.getByText('Stored backups')).toBeInTheDocument();
    expect(screen.getByText('duduclaw-backup-20260818.tar.gz')).toBeInTheDocument();
    expect(screen.getByText('duduclaw-backup-20260817.tar.gz')).toBeInTheDocument();
  });

  it('network_info: shows up/down badges per interface', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_NETWORK_INFO} />);
    expect(screen.getByText('eth0')).toBeInTheDocument();
    expect(screen.getByText('Up')).toBeInTheDocument();
    expect(screen.getByText('wlan0')).toBeInTheDocument();
    expect(screen.getByText('Down')).toBeInTheDocument();
  });

  it('confirm_action: renders the DevicePage-matching danger-zone copy inline (no modal)', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_CONFIRM_RESTART} />);
    // "Restart" appears twice — the card title and the (disabled) confirm
    // button — both are part of the same inline card, neither is a modal.
    expect(screen.getAllByText('Restart').length).toBeGreaterThanOrEqual(2);
    expect(
      screen.getByText('Restarts this device. Every service on it will be briefly unavailable.'),
    ).toBeInTheDocument();
    // Not a dialog/modal — the confirm affordance is part of the message flow.
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('update_confirm (device): renders the pending update prompt inline (no modal)', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_UPDATE_CONFIRM_DEVICE} />);
    expect(screen.getByText('Apply device update?')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Apply update' })).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('update_confirm (system): renders the app-update-flavored prompt', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_UPDATE_CONFIRM_SYSTEM} />);
    expect(screen.getByText('Apply app update?')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Apply update' })).toBeInTheDocument();
  });

  it('approval_request: renders approve/reject wired to the ApprovalBroker RPC contract', () => {
    renderWithProviders(<ChatArtifactCard artifact={FIXTURE_APPROVAL_REQUEST} />);
    expect(screen.getByText('Needs your approval')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Approve' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Reject' })).toBeInTheDocument();
  });
});
