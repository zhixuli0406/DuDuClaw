import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { UpdateStatusCard } from './UpdateStatusCard';
import {
  FIXTURE_UPDATE_STATUS,
  FIXTURE_UPDATE_STATUS_WITH_SYSTEM,
  FIXTURE_UPDATE_STATUS_SYSTEM_ONLY,
} from './fixtures';
import type { UpdateStatusArtifact } from './artifact-types';

describe('<UpdateStatusCard> — system (self-update) half', () => {
  it('renders nothing extra when payload.system is absent (backward compatible)', () => {
    renderWithProviders(<UpdateStatusCard payload={FIXTURE_UPDATE_STATUS.payload as UpdateStatusArtifact['payload']} />);
    expect(screen.queryByText('DuDuClaw version')).not.toBeInTheDocument();
  });

  it('renders the DuDuClaw version section with an "update available" badge when system.available is true', () => {
    renderWithProviders(
      <UpdateStatusCard payload={FIXTURE_UPDATE_STATUS_WITH_SYSTEM.payload as UpdateStatusArtifact['payload']} />,
    );
    expect(screen.getByText('DuDuClaw version')).toBeInTheDocument();
    expect(screen.getByText('Update available')).toBeInTheDocument();
    expect(screen.getByText('v1.61.2 → v1.62.0')).toBeInTheDocument();
  });

  it('renders an "up to date" badge when system.available is false', () => {
    renderWithProviders(
      <UpdateStatusCard
        payload={{
          action: 'check',
          result: { success: true, stdout: '', stderr: '' },
          system: {
            available: false,
            current_version: '1.62.0',
            latest_version: '1.62.0',
          },
        }}
      />,
    );
    expect(screen.getByText('Up to date')).toBeInTheDocument();
    expect(screen.getByText('v1.62.0 → v1.62.0')).toBeInTheDocument();
  });

  it('renders a check-failed note (not the version card) when system is the error shape', () => {
    renderWithProviders(
      <UpdateStatusCard
        payload={{
          action: 'check',
          result: { success: true, stdout: '', stderr: '' },
          system: { error: 'GitHub API rate limited' },
        }}
      />,
    );
    expect(screen.getByText('Version check failed: GitHub API rate limited')).toBeInTheDocument();
    expect(screen.queryByText('DuDuClaw version')).not.toBeInTheDocument();
  });
});

describe('<UpdateStatusCard> — P5: optional `result` (device) half', () => {
  it('renders only the system (version) section when `result` is absent (non-appliance install), without throwing', () => {
    renderWithProviders(
      <UpdateStatusCard payload={FIXTURE_UPDATE_STATUS_SYSTEM_ONLY.payload as UpdateStatusArtifact['payload']} />,
    );
    // The version section IS present, sourced from `system` alone.
    expect(screen.getByText('DuDuClaw version')).toBeInTheDocument();
    expect(screen.getByText('Update available')).toBeInTheDocument();
    // The device-result section (success/fail badge, rollback button, details
    // toggle) must be entirely absent — never a fabricated `DeviceOpResult`.
    expect(screen.queryByText('Update complete')).not.toBeInTheDocument();
    expect(screen.queryByText("The update didn't finish — see the details below")).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Roll back to previous version/ })).not.toBeInTheDocument();
    expect(screen.queryByText('Show details')).not.toBeInTheDocument();
  });

  it('renders both sections when both `result` and `system` are present (appliance install, unchanged)', () => {
    renderWithProviders(
      <UpdateStatusCard payload={FIXTURE_UPDATE_STATUS_WITH_SYSTEM.payload as UpdateStatusArtifact['payload']} />,
    );
    expect(screen.getByText('DuDuClaw version')).toBeInTheDocument();
    expect(screen.getByText('Update complete')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Roll back to previous version/ })).toBeInTheDocument();
  });
});
