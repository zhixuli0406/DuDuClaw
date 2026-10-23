import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { CredentialProtectionPanel } from './CredentialProtectionPanel';
import type { CredentialProtectionStatus } from '@/lib/api';

beforeEach(() => {
  vi.clearAllMocks();
});

function status(over: Partial<CredentialProtectionStatus>): CredentialProtectionStatus {
  return {
    level: 'file',
    platform_level: 'app_bound',
    backend: 'macos_keychain',
    state: 'not_enabled',
    key_id: null,
    enabled_at: null,
    person_identity_allowed: false,
    summary: '憑證保護：檔案',
    detail: '',
    ...over,
  };
}

const READY = status({
  level: 'app_bound',
  state: 'ready',
  key_id: '9c1e0000aaaa1111',
  person_identity_allowed: true,
});

function mockProtection(initial: CredentialProtectionStatus) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'security.credential_protection') return Promise.resolve(initial);
    if (method === 'security.credential_protection_enable') return Promise.resolve(READY);
    if (method === 'security.credential_protection_disable') return Promise.resolve(status({}));
    return Promise.resolve({});
  });
}

describe('<CredentialProtectionPanel>', () => {
  it('shows the file level and offers to move into the keychain', async () => {
    mockProtection(status({}));
    renderWithProviders(<CredentialProtectionPanel />);
    const line = await screen.findByTestId('credential-protection-line');
    expect(line.textContent).toBe('Credential protection: Files');
    expect(screen.getByText('AI employees cannot use personal accounts yet')).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Move to system keychain' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('security.credential_protection_enable', { reset: false });
    expect((await screen.findByTestId('credential-protection-line')).textContent).toBe(
      'Credential protection: System keychain (only the DuDuClaw program can read it)',
    );
    expect(screen.getByText('AI employees may use personal accounts')).toBeInTheDocument();
  });

  it('on macOS without hardened runtime says code injection can read the key', async () => {
    mockProtection(status({ level: 'user_store', state: 'ready', person_identity_allowed: true }));
    renderWithProviders(<CredentialProtectionPanel />);
    expect((await screen.findByTestId('credential-protection-line')).textContent).toBe(
      'Credential protection: System keychain (readable by programs under the same user account)',
    );
    expect(screen.getByText(/not built with hardened runtime/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Replace key' })).toBeInTheDocument();
  });

  it('shows the authorize command when macOS needs a person', async () => {
    mockProtection(status({ state: 'needs_authorization', key_id: 'abcd' }));
    renderWithProviders(<CredentialProtectionPanel />);
    expect((await screen.findByTestId('credential-protection-line')).textContent).toBe(
      'Credential protection: Needs re-authorization',
    );
    expect(screen.getByText('duduclaw credentials authorize')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Move to system keychain' })).toBeNull();
  });

  it('a replaced key offers a confirmed re-enable that sends reset', async () => {
    mockProtection(status({ state: 'key_changed', key_id: 'abcd' }));
    renderWithProviders(<CredentialProtectionPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Re-enable' }));
    // The dialog's confirm button carries the same label.
    const buttons = await screen.findAllByRole('button', { name: 'Re-enable' });
    await userEvent.click(buttons[buttons.length - 1]);
    expect(mockWsClient.call).toHaveBeenCalledWith('security.credential_protection_enable', { reset: true });
  });

  it('turning off needs the typed confirmation', async () => {
    mockProtection(READY);
    renderWithProviders(<CredentialProtectionPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Turn off' }));
    const dialogButtons = await screen.findAllByRole('button', { name: 'Turn off' });
    const confirm = dialogButtons[dialogButtons.length - 1];
    expect(confirm).toBeDisabled();
    await userEvent.type(screen.getByRole('textbox'), 'turn off');
    await userEvent.click(confirm);
    expect(mockWsClient.call).toHaveBeenCalledWith('security.credential_protection_disable', { confirm: true });
  });

  it('an unsupported platform offers no actions', async () => {
    mockProtection(status({ state: 'unsupported', backend: 'none', platform_level: 'file' }));
    renderWithProviders(<CredentialProtectionPanel />);
    expect((await screen.findByTestId('credential-protection-line')).textContent).toBe(
      'Credential protection: Files (this platform has no supported system keychain)',
    );
    expect(screen.queryByRole('button', { name: 'Move to system keychain' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Turn off' })).toBeNull();
  });
});
