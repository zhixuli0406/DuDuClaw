import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import type { SystemStatus } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { FOLDER_ACCESS_DOCS_URL, FolderAccessBanner } from './FolderAccessBanner';

function setStatus(folderAccess?: SystemStatus['folder_access']) {
  useSystemStore.setState({
    status: { version: '1.81.0', edition_profile: 'personal', folder_access: folderAccess } as SystemStatus,
  });
}

beforeEach(() => {
  useAuthStore.setState({ user: {
    id: 'admin', email: 'admin@test.invalid', display_name: 'Admin', role: 'admin', status: 'active',
  } });
});

afterEach(() => {
  useAuthStore.setState({ user: null });
  useSystemStore.setState({ status: null } as never);
});

describe('FolderAccessBanner', () => {
  it('is hidden while folder access is healthy (field absent)', () => {
    setStatus(undefined);
    const { container } = renderWithProviders(<FolderAccessBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('is hidden for non-admins', () => {
    useAuthStore.setState({ user: { ...useAuthStore.getState().user!, role: 'manager' } });
    setStatus({ state: 'pending_prompt', paths: [{ path_label: '~/Documents', state: 'pending_prompt' }] });
    const { container } = renderWithProviders(<FolderAccessBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('asks the admin to click Allow while a prompt is pending, with the paths and the guide', () => {
    setStatus({
      state: 'pending_prompt',
      paths: [
        { path_label: '~/Documents/clients', state: 'pending_prompt' },
        { path_label: '/Volumes/NAS', state: 'posix_denied' },
      ],
    });
    renderWithProviders(<FolderAccessBanner />);
    expect(screen.getByText(/waiting for someone on this Mac/)).toBeInTheDocument();
    expect(screen.getByText(/Privacy & Security › Files and Folders/)).toBeInTheDocument();
    expect(screen.getByText('~/Documents/clients')).toBeInTheDocument();
    expect(screen.queryByText('/Volumes/NAS')).not.toBeInTheDocument();
    expect(screen.getByRole('link')).toHaveAttribute('href', FOLDER_ACCESS_DOCS_URL);
  });

  it('uses the denied title when macOS refused access', () => {
    setStatus({
      state: 'denied',
      paths: [{ path_label: '~/Desktop', state: 'denied' }],
      docs_url: 'https://example.invalid/guide',
    });
    renderWithProviders(<FolderAccessBanner />);
    expect(screen.getByText(/not allowing DuDuClaw/)).toBeInTheDocument();
    expect(screen.getByRole('link')).toHaveAttribute('href', 'https://example.invalid/guide');
  });
});
