import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { CredentialProtectionPanel } from './CredentialProtectionPanel';
import type { CredentialProtectionStatus } from '@/lib/api';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';

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

function mockProtection(initial: CredentialProtectionStatus, enabled = READY) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'security.credential_protection') return Promise.resolve(initial);
    if (method === 'security.credential_protection_enable') return Promise.resolve(enabled);
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
      'Credential protection: Files (personal accounts unavailable here)',
    );
    expect(screen.queryByRole('button', { name: 'Move to system keychain' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Turn off' })).toBeNull();
    expect(screen.getByText(/verify the release and install from a root-owned location/)).toHaveTextContent(
      'check the dashboard for the gateway state',
    );
  });
});


const CONTAINER_READY = status({
  backend: 'container_root_key',
  platform_level: 'process_isolated',
  level: 'process_isolated',
  state: 'ready',
  person_identity_allowed: true,
});

describe('container credential protection', () => {
  it('enables the separate key without asking to move into a system keychain', async () => {
    mockProtection(status({ backend: 'container_root_key', platform_level: 'process_isolated' }), CONTAINER_READY);
    renderWithProviders(<CredentialProtectionPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Enable credential protection' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('security.credential_protection_enable', { reset: false });
    expect((await screen.findByTestId('credential-protection-line')).textContent).toBe(
      'Credential protection: Separate key (AI employees cannot read it)',
    );
    expect(screen.queryByText(/system keychain/i)).toBeNull();
    expect(screen.getByText(/only the protected service’s startup step/)).toHaveTextContent(
      'read the token issued for its turn, and make DuDuClaw stop',
    );
    expect(screen.getByText(/only the protected service’s startup step/)).toHaveTextContent(
      'Anyone who controls the Linux host, Docker host or root-key storage can read the key',
    );
  });

  it.each([
    ['missing', /cannot find the protected key saved for this data folder/],
    ['key_changed', /Losing or replacing the root key also causes this/],
  ] as const)('explains container %s without keychain instructions', async (state, text) => {
    mockProtection(status({ backend: 'container_root_key', platform_level: 'process_isolated', state }));
    renderWithProviders(<CredentialProtectionPanel />);
    expect(await screen.findByText(text)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Re-enable' })).toBeInTheDocument();
    expect(screen.queryByText(/system keychain/i)).toBeNull();
  });

  it.each([
    ['en', en],
    ['zh-TW', zhTW],
    ['ja-JP', jaJP],
  ] as const)('shows a translated startup refusal in %s', async (locale, messages) => {
    mockProtection(status({
      backend: 'container_root_key',
      platform_level: 'process_isolated',
      state: 'unavailable',
      refusal_reason: 'engine_socket_reachable',
    }));
    renderWithProviders(
      <IntlProvider locale={locale} messages={messages}>
        <CredentialProtectionPanel />
      </IntlProvider>,
    );
    expect((await screen.findByTestId('credential-protection-refusal')).textContent).toBe(
      messages['security.credentialProtection.refusal.engine_socket_reachable'],
    );
    expect(screen.getByTestId('credential-protection-line')).toHaveTextContent(
      messages['security.credentialProtection.state.unavailable_container'],
    );
  });

  for (const [locale, messages] of [['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const) {
    it.each(['user_namespace', 'container_environment', 'reattest_unavailable', 'key_state_poisoned'] as const)(
      `shows the specific protection refusal %s in ${locale}`,
      async (reason) => {
        mockProtection(status({
          backend: 'container_root_key', platform_level: 'process_isolated',
          state: 'unavailable', refusal_reason: reason,
        }));
        renderWithProviders(
          <IntlProvider locale={locale} messages={messages}><CredentialProtectionPanel /></IntlProvider>,
        );
        const refusal = await screen.findByTestId('credential-protection-refusal');
        expect(refusal.textContent).toBe(messages[`security.credentialProtection.refusal.${reason}`]);
        expect(refusal.textContent).not.toBe(messages['security.credentialProtection.refusal.unknown']);
        expect(screen.queryByRole('button', { name: messages['security.credentialProtection.enable_container'] })).toBeNull();
      },
    );
  }

  it('does not expose an unknown startup diagnostic as a locale ID or raw message', async () => {
    mockProtection(status({ backend: 'container_root_key', state: 'unavailable', refusal_reason: 'untrusted diagnostic' }));
    renderWithProviders(<CredentialProtectionPanel />);
    expect((await screen.findByTestId('credential-protection-refusal')).textContent).toBe(
      'The separate-key startup checks failed. Check the deployment settings.',
    );
    expect(screen.queryByText(/untrusted diagnostic/)).toBeNull();
  });

  it.each([
    ['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP],
  ] as const)('shows socket diagnostics and blocked recovery in %s', async (locale, messages) => {
    const detail = '@/containerd-shim/<img src=x onerror=alert(1)> (peer uid=0, root)';
    mockProtection(status({
      backend: 'container_root_key', state: 'unavailable', refusal_reason: 'engine_socket_reachable',
      root_key_recovery: {
        latest_revocation: { reason: 'engine_socket_reachable', detail, timestamp: '2026-11-05T01:02:03Z' },
        recovery: 'state_unavailable',
      },
    }));
    renderWithProviders(
      <IntlProvider locale={locale} messages={messages}><CredentialProtectionPanel /></IntlProvider>,
    );
    const diagnostics = await screen.findByTestId('credential-protection-recovery');
    expect(diagnostics).toHaveTextContent(detail);
    expect(diagnostics).toHaveTextContent('2026-11-05T01:02:03Z');
    expect(diagnostics).toHaveTextContent(messages['security.credentialProtection.recovery.state_unavailable']);
    expect(diagnostics.querySelector('img')).toBeNull();
  });

  it.each([
    ['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP],
  ] as const)('shows the root docker command hint and upgrade requirement in %s', async (locale, messages) => {
    mockProtection(status({
      backend: 'container_root_key', state: 'unavailable', refusal_reason: 'engine_socket_reachable',
      root_key_recovery: {
        latest_revocation: {
          reason: 'engine_socket_reachable',
          detail: '@docker_cli_3e708842d9322cef25983565c19d0866: peer uid=0 (root)',
          timestamp: '2026-11-05T01:02:03Z',
        },
        recovery: 'unit_upgrade_required',
      },
    }));
    renderWithProviders(
      <IntlProvider locale={locale} messages={messages}><CredentialProtectionPanel /></IntlProvider>,
    );
    const diagnostics = await screen.findByTestId('credential-protection-recovery');
    expect(diagnostics).toHaveTextContent(messages['security.credentialProtection.recovery.docker_cli_root']);
    expect(diagnostics).toHaveTextContent(messages['security.credentialProtection.recovery.unit_upgrade_required']);
  });

  it.each([
    '@docker_cli_3E708842d9322cef25983565c19d0866: peer uid=0 (root)',
    '@docker_cli_3e708842d9322cef25983565c19d0866: peer uid=1000 (root)',
    '@docker_cli_3e708842d9322cef25983565c19d0866/suffix: peer uid=0 (root)',
  ])('does not select the docker command hint for a nonmatching detail: %s', async (detail) => {
    mockProtection(status({
      backend: 'container_root_key', state: 'unavailable', refusal_reason: 'engine_socket_reachable',
      root_key_recovery: {
        latest_revocation: { reason: 'engine_socket_reachable', detail, timestamp: '2026-11-05T01:02:03Z' },
        recovery: 'waiting',
      },
    }));
    renderWithProviders(<CredentialProtectionPanel />);
    const diagnostics = await screen.findByTestId('credential-protection-recovery');
    expect(diagnostics).not.toHaveTextContent(en['security.credentialProtection.recovery.docker_cli_root']);
  });

  it('does not describe persisted revocation history as a current outage', async () => {
    mockProtection(status({
      ...CONTAINER_READY,
      root_key_recovery: {
        latest_revocation: { reason: 'engine_socket_reachable', detail: '@/containerd-shim/example (root)', timestamp: '2026-11-05T01:02:03Z' },
        recovery: 'waiting',
      },
    }));
    renderWithProviders(<CredentialProtectionPanel />);
    const diagnostics = await screen.findByTestId('credential-protection-recovery');
    expect(diagnostics).toHaveTextContent(en['security.credentialProtection.recovery.history']);
    expect(diagnostics).not.toHaveTextContent(en['security.credentialProtection.recovery.waiting']);
    expect(screen.getByTestId('credential-protection-line')).toHaveTextContent('Separate key');
  });

  it.each([
    ['en', en],
    ['zh-TW', zhTW],
    ['ja-JP', jaJP],
  ] as const)('warns about restoring old folders when replacing or resetting a container key in %s', async (locale, messages) => {
    mockProtection(CONTAINER_READY, CONTAINER_READY);
    renderWithProviders(
      <IntlProvider locale={locale} messages={messages}>
        <CredentialProtectionPanel />
      </IntlProvider>,
    );
    await userEvent.click(await screen.findByRole('button', {
      name: messages['security.credentialProtection.rotate'],
    }));
    const warning = messages['security.credentialProtection.confirm.reset.message_container'];
    expect(screen.getByText(warning)).toBeInTheDocument();
    expect(warning).not.toBe(messages['security.credentialProtection.confirm.reset.message']);
    const buttons = screen.getAllByRole('button', {
      name: messages['security.credentialProtection.reset'],
    });
    await userEvent.click(buttons[buttons.length - 1]);
    expect(mockWsClient.call).toHaveBeenCalledWith('security.credential_protection_enable', { reset: true });
  });

  it('uses the same container warning when re-enabling after a changed key', async () => {
    mockProtection(status({ ...CONTAINER_READY, state: 'key_changed' }), CONTAINER_READY);
    renderWithProviders(<CredentialProtectionPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Re-enable' }));
    expect(screen.getByText(en['security.credentialProtection.confirm.reset.message_container'])).toBeInTheDocument();
  });

  it('explains that disabling removes the wrapped key and retains the startup volume', async () => {
    mockProtection(CONTAINER_READY);
    renderWithProviders(<CredentialProtectionPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Turn off' }));
    expect(screen.getByText(/The encrypted copy of the protected key will be deleted/)).toHaveTextContent(
      'The startup root key remains',
    );
    expect(screen.queryByText(/system keychain/i)).toBeNull();
    const buttons = screen.getAllByRole('button', { name: 'Turn off' });
    expect(buttons[buttons.length - 1]).toBeDisabled();
  });
});
