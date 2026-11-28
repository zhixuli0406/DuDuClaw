import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { GoogleIntegrationPage } from './GoogleIntegrationPage';
import { api, type GoogleCredentialsStatus } from '@/lib/api';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';

const BASE: GoogleCredentialsStatus = {
  integration_enabled: true,
  effective: 'none',
  service_account: { configured: false, key_file: '', subject: '', error: '' },
  apps_script: { configured: false, url: '', error: '' },
  required_scopes: [],
};

function mockGet(overrides: Partial<GoogleCredentialsStatus> = {}) {
  return vi.spyOn(api.googleCredentials, 'get').mockResolvedValue({ ...BASE, ...overrides });
}

beforeEach(() => {
  vi.restoreAllMocks();
  useAuthStore.setState({user:null});
});

describe('GoogleIntegrationPage — master gate (G6)', () => {
  it('shows an enable prompt, not a silent dead end, when the backend gate is off', async () => {
    // The regression this pins: the tab was unconditionally visible while
    // `[integrations] google_workspace` defaulted to false, so the page looked
    // ready while every agent tool call 403'd.
    mockGet({ integration_enabled: false });
    renderWithProviders(<GoogleIntegrationPage />);

    await waitFor(() => expect(screen.getByTestId('google-gate-off')).toBeInTheDocument());
    expect(screen.queryByTestId('google-gate-on')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /啟用|enable/i })).toBeInTheDocument();
  });

  it('enables the integration through the backend and flips the banner', async () => {
    mockGet({ integration_enabled: false });
    const set = vi
      .spyOn(api.googleCredentials, 'setIntegration')
      .mockResolvedValue({ enabled: true, changed: true });

    renderWithProviders(<GoogleIntegrationPage />);
    await waitFor(() => expect(screen.getByTestId('google-gate-off')).toBeInTheDocument());

    await userEvent.click(screen.getByRole('button', { name: /啟用|enable/i }));

    await waitFor(() => expect(set).toHaveBeenCalledWith(true));
    await waitFor(() => expect(screen.getByTestId('google-gate-on')).toBeInTheDocument());
    expect(screen.queryByTestId('google-gate-off')).not.toBeInTheDocument();
  });

  it('shows the enabled state with a way back off', async () => {
    mockGet({ integration_enabled: true });
    renderWithProviders(<GoogleIntegrationPage />);

    await waitFor(() => expect(screen.getByTestId('google-gate-on')).toBeInTheDocument());
    expect(screen.queryByTestId('google-gate-off')).not.toBeInTheDocument();
  });

  it('shows neither banner while the status is unknown', async () => {
    // A failed status read must not claim the integration is off — that would
    // push the operator to "enable" something that is already on.
    vi.spyOn(api.googleCredentials, 'get').mockRejectedValue(new Error('offline'));
    renderWithProviders(<GoogleIntegrationPage />);

    await waitFor(() => expect(api.googleCredentials.get).toHaveBeenCalled());
    expect(screen.queryByTestId('google-gate-off')).not.toBeInTheDocument();
    expect(screen.queryByTestId('google-gate-on')).not.toBeInTheDocument();
  });
});

it('lets an administrator opt into Chat without changing the Google integration gate', async () => {
  useAuthStore.setState({user:{id:'admin',email:'admin@example.test',display_name:'Admin',role:'admin',status:'active'}});
  mockGet({chat_read:false});
  const set = vi.spyOn(api.googleCredentials, 'setChatRead').mockResolvedValue({chat_read:true,changed:true});
  const integration = vi.spyOn(api.googleCredentials, 'setIntegration');
  renderWithProviders(<GoogleIntegrationPage/>);
  const toggle = await screen.findByRole('checkbox', {name:'Read Google Chat'});
  expect(toggle).not.toBeChecked();
  await userEvent.click(toggle);
  await waitFor(()=>expect(toggle).toBeChecked());
  expect(set).toHaveBeenCalledWith(true);
  expect(integration).not.toHaveBeenCalled();
  expect(screen.getByText(/Enable the Google Chat API/)).toBeInTheDocument();
});

it('does not offer the Chat switch to a non-administrator', async () => {
  mockGet({chat_read:true});
  renderWithProviders(<GoogleIntegrationPage/>);
  await screen.findByTestId('google-gate-on');
  expect(screen.queryByRole('checkbox', {name:'Read Google Chat'})).not.toBeInTheDocument();
  expect(screen.queryByTestId('google-chat-identity')).not.toBeInTheDocument();
});

it('Chat sender recognition by Google account is off by default, admin-only and carries the risk note', async () => {
  useAuthStore.setState({user:{id:'admin',email:'admin@example.test',display_name:'Admin',role:'admin',status:'active'}});
  // An older gateway that does not report the key reads as off.
  mockGet({});
  const set = vi.spyOn(api.googleCredentials, 'setChatIdentityFromAccount')
    .mockResolvedValue({chat_identity_from_account:true,changed:true});
  const chatRead = vi.spyOn(api.googleCredentials, 'setChatRead');
  renderWithProviders(<GoogleIntegrationPage/>);
  const toggle = await screen.findByRole('checkbox', {name:'Recognise Google Chat senders by their connected Google account'});
  expect(toggle).not.toBeChecked();
  expect(screen.getByText(/Not yet tested with a real Google Workspace/)).toBeInTheDocument();
  await userEvent.click(toggle);
  await waitFor(()=>expect(toggle).toBeChecked());
  expect(set).toHaveBeenCalledWith(true);
  expect(chatRead).not.toHaveBeenCalled();
});

// RM-131: immediate saves must preserve the loaded form and fail closed.
import { within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { useAgentsStore } from '@/stores/agents-store';

const DRIVE = {
  configured: { create: false, modify: false, move: false, trash: false },
  effective: { create: false, modify: false, move: false, trash: false },
  approval: { create: 'member', modify: 'member', move: 'member', trash: 'member' },
  folders: [] as { id: string; name: string; kind: string }[], ack: null,
  integration_enabled: true, folder_list_empty_note: true,
  write_connections: { connected: 3 }, write_store_unreadable: false,
  home_write: { route_oauth: true, connected: false, status: 'disconnected', account_email: null },
  identity_routes: [],
};
function driveSetup(overrides: Partial<typeof DRIVE> = {}) {
  const state = { ...DRIVE, ...overrides };
  useAuthStore.setState({ user: { id: 'admin', email: 'admin@example.test', display_name: 'Admin', role: 'admin', status: 'active' } });
  useAgentsStore.setState({ agents: [], fetchAgents: vi.fn().mockResolvedValue(undefined) });
  mockGet();
  mockWsClient.call.mockImplementation(async (method: string, params: Record<string, unknown>) => {
    if (method === 'google.drive_write.get') return state;
    if (method === 'google.drive_write.set') return { ...state, configured: params, effective: params, approval: params.approval,
      folders: (params.folders as string[]).map(id => ({ id, name: 'Team folder', kind: 'folder' })) };
    return {};
  });
  return state;
}
async function driveCard() {
  renderWithProviders(<GoogleIntegrationPage />);
  return within(await screen.findByTestId('google-drive-write'));
}
async function acknowledge() {
  const dialog = within(await screen.findByRole('dialog'));
  await userEvent.click(dialog.getByRole('checkbox', { name: 'I understand the effects of enabling this' }));
  await userEvent.click(dialog.getByRole('button', { name: 'Enable' }));
}
function sets() { return mockWsClient.call.mock.calls.filter(c => c[0] === 'google.drive_write.set'); }

describe('S-08 Drive write administration', () => {
  beforeEach(() => { mockWsClient.call.mockReset(); });
  it('defaults to all off and is absent for members', async () => {
    driveSetup();
    const card = await driveCard();
    expect(await card.findByText('All actions are off')).toBeInTheDocument();
    expect(card.getAllByRole('switch')).toHaveLength(4);
    card.getAllByRole('switch').forEach(toggle => expect(toggle).not.toBeChecked());
    expect(card.getByRole('link', { name: 'View AI write history' })).toHaveAttribute('href', '/logs?tab=history&tool_name_filter=google_drive_write');
  });
  it('requires risk acknowledgement before enabling and sends the full form', async () => {
    driveSetup(); const card = await driveCard();
    await userEvent.click(await card.findByRole('switch', { name: 'Create' }));
    expect(sets()).toHaveLength(0);
    expect(await screen.findByText('Existing document and spreadsheet editing tools are not controlled by these switches.')).toBeInTheDocument();
    await acknowledge();
    await waitFor(() => expect(sets()).toHaveLength(1));
    expect(sets()[0][1]).toEqual({ ...DRIVE.configured, create: true, approval: DRIVE.approval, folders: [], acknowledge_risk: true });
  });
  it('turns off without acknowledgement', async () => {
    driveSetup({ configured: { ...DRIVE.configured, create: true }, effective: { ...DRIVE.effective, create: true } });
    const card = await driveCard(); await userEvent.click(await card.findByRole('switch', { name: 'Create' }));
    await waitFor(() => expect(sets()).toHaveLength(1));
    expect(sets()[0][1]).not.toHaveProperty('acknowledge_risk');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
  it.each([['none', true], ['member_and_admin', false]])('approval %s needs risk: %s', async (mode, risk) => {
    driveSetup(); const card = await driveCard();
    await userEvent.selectOptions(await card.findByRole('combobox', { name: 'Create · Confirmation before execution' }), mode);
    if (risk) { expect(sets()).toHaveLength(0); await acknowledge(); }
    await waitFor(() => expect(sets()).toHaveLength(1));
    expect(sets()[0][1]).toEqual(expect.objectContaining({ approval: { ...DRIVE.approval, create: mode }, ...(risk ? { acknowledge_risk: true } : {}) }));
    if (!risk) expect(sets()[0][1]).not.toHaveProperty('acknowledge_risk');
  });
  it('adds with acknowledgement, hides folder ids and removes directly', async () => {
    driveSetup({ folders: [{ id: 'secret-folder-id', name: '', kind: 'unverified' }] });
    const card = await driveCard();
    expect(await card.findByText('(Name unavailable)')).toBeInTheDocument();
    expect(card.queryByText('secret-folder-id')).not.toBeInTheDocument();
    await userEvent.type(card.getByRole('textbox', { name: 'Paste a folder link' }), 'https://drive.google.com/drive/folders/new-folder');
    await userEvent.click(card.getByRole('button', { name: 'Add' })); await acknowledge();
    await waitFor(() => expect(sets()).toHaveLength(1));
    expect(sets()[0][1]).toEqual(expect.objectContaining({ folders: ['secret-folder-id', 'https://drive.google.com/drive/folders/new-folder'], acknowledge_risk: true }));
  });
  it('removes a folder without acknowledgement', async () => {
    driveSetup({ folders: [{ id: 'hidden-folder', name: 'Sales', kind: 'folder' }] });
    const card = await driveCard(); await userEvent.click(await card.findByRole('button', { name: 'Remove Sales' }));
    await waitFor(() => expect(sets()).toHaveLength(1)); expect(sets()[0][1]).not.toHaveProperty('acknowledge_risk');
  });
  it('requires risk when removing the last folder leaves create on', async () => {
    driveSetup({ configured: { ...DRIVE.configured, create: true }, folders: [{ id: 'hidden-folder', name: 'Sales', kind: 'folder' }] });
    const card = await driveCard(); await userEvent.click(await card.findByRole('button', { name: 'Remove Sales' }));
    expect(sets()).toHaveLength(0); await acknowledge();
    await waitFor(() => expect(sets()).toHaveLength(1)); expect(sets()[0][1]).toHaveProperty('acknowledge_risk', true);
  });
  it.each(['mismatch', 'failure'])('blocks saves after a stale re-read %s', async kind => {
    const state = driveSetup(); const card = await driveCard();
    await card.findByRole('switch', { name: 'Create' });
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'google.drive_write.get') { if (kind === 'failure') throw new Error('technical secret'); return { ...state, approval: { ...state.approval, trash: 'none' } }; }
      return {};
    });
    await userEvent.click(card.getByRole('switch', { name: 'Create' })); await acknowledge();
    await waitFor(() => expect(card.getByRole('alert')).toBeInTheDocument());
    expect(sets()).toHaveLength(0); expect(card.queryByText('technical secret')).not.toBeInTheDocument();
    if (kind === 'mismatch') expect(card.getByRole('button', { name: 'Reload' })).toBeInTheDocument();
  });
  it('disables the four rows when the integration is off but allows folders', async () => {
    driveSetup({ integration_enabled: false, configured: { ...DRIVE.configured, create: true } }); const card = await driveCard();
    expect(await card.findByText('Google integration is off. All four Drive write actions are inactive.')).toBeInTheDocument();
    card.getAllByRole('switch').forEach(toggle => { expect(toggle).toBeDisabled(); expect(toggle).not.toBeChecked(); });
    card.getAllByRole('combobox').filter(el => el.getAttribute('aria-label')?.includes('Confirmation')).forEach(el => expect(el).toBeDisabled());
    expect(card.getByRole('textbox', { name: 'Paste a folder link' })).toBeEnabled();
  });
  it('requires both consent acknowledgements and sends both to the server', async () => {
    driveSetup(); const popup = { opener: {}, location: { replace: vi.fn() }, close: vi.fn() };
    vi.spyOn(window, 'open').mockReturnValue(popup as unknown as Window);
    const card = await driveCard(); const button = await card.findByRole('button', { name: 'Consent to writing' });
    expect(button).toBeDisabled();
    await userEvent.click(card.getByRole('checkbox', { name: 'I understand how my Google data is accessed, used, shared, stored, and deleted.' }));
    expect(button).toBeDisabled();
    await userEvent.click(card.getByRole('checkbox', { name: 'I understand AI will use the company account to write to Drive.' }));
    mockWsClient.call.mockResolvedValueOnce({ authorization_url: 'https://accounts.google.com/o/oauth2/auth', completion: 'redirect' });
    await userEvent.click(button);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('google.drive_write.consent_home_start', { redirect_origin: window.location.origin, acknowledge_google_data: true, acknowledge_write: true }));
    expect(popup.opener).toBeNull(); expect(popup.location.replace).toHaveBeenCalledWith('https://accounts.google.com/o/oauth2/auth');
  });
  it('copies three scopes and offers employees by display name', async () => {
    driveSetup();
    useAgentsStore.setState({ agents: [{ name: 'hidden-agent', display_name: 'Sales assistant' }] as never });
    const copy = vi.fn().mockResolvedValue(undefined); Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: copy } });
    const card = await driveCard(); await userEvent.click(await card.findByRole('button', { name: 'Copy scope list' }));
    expect(copy).toHaveBeenCalledWith('https://www.googleapis.com/auth/drive\nhttps://www.googleapis.com/auth/documents\nhttps://www.googleapis.com/auth/spreadsheets');
    expect(card.getByRole('option', { name: 'Sales assistant' })).toBeInTheDocument();
    expect(card.queryByText('hidden-agent')).not.toBeInTheDocument();
    vi.spyOn(api.googleIdentity, 'test').mockResolvedValue({ agent_id: 'hidden-agent', email: null, ok: true, steps: [{ id: 'drive_write', label: 'Drive write check', ok: true, detail: 'Ready to write' }] });
    await userEvent.selectOptions(card.getByRole('combobox', { name: 'AI employee' }), 'hidden-agent');
    await userEvent.click(card.getByRole('button', { name: 'Test' }));
    expect(await card.findByText('Ready to write')).toBeInTheDocument(); expect(api.googleIdentity.test).toHaveBeenCalledWith('hidden-agent');
  });
});

// Home consent uses a separate credential and only Google may receive navigation.
describe('S-08 consent and refusal states', () => {
  beforeEach(() => { mockWsClient.call.mockReset(); });
  it('hides the card and never reads admin settings for members', async () => {
    mockGet(); mockWsClient.call.mockResolvedValue({});
    renderWithProviders(<GoogleIntegrationPage />); await screen.findByTestId('google-gate-on');
    expect(screen.queryByTestId('google-drive-write')).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('google.drive_write.get', {});
  });
  it('does not offer OAuth for other routes', async () => {
    driveSetup({ home_write: { ...DRIVE.home_write, route_oauth: false } }); const card = await driveCard();
    await card.findByText('All actions are off'); expect(card.queryByRole('button', { name: 'Consent to writing' })).not.toBeInTheDocument();
  });
  it('shows plain server refusals without modifying the saved form', async () => {
    const state = driveSetup(); const card = await driveCard();
    await card.findByRole('switch', { name: 'Create' });
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'google.drive_write.set') throw new Error('這個連結不是資料夾');
      return state;
    });
    await userEvent.type(card.getByRole('textbox', { name: 'Paste a folder link' }), 'not-a-folder');
    await userEvent.click(card.getByRole('button', { name: 'Add' })); await acknowledge();
    expect(await card.findByRole('alert')).toHaveTextContent('這個連結不是資料夾');
    expect(card.getByRole('switch', { name: 'Create' })).not.toBeChecked();
  });
  it('blocks foreign consent URLs and completes the paste flow', async () => {
    const state = driveSetup(); const replace = vi.fn(); const close = vi.fn();
    vi.spyOn(window, 'open').mockReturnValue({ opener: {}, location: { replace }, close } as unknown as Window);
    const card = await driveCard();
    async function start(url: string) {
      await userEvent.click(card.getByRole('checkbox', { name: 'I understand how my Google data is accessed, used, shared, stored, and deleted.' }));
      await userEvent.click(card.getByRole('checkbox', { name: 'I understand AI will use the company account to write to Drive.' }));
      mockWsClient.call.mockResolvedValueOnce({ authorization_url: url, completion: 'paste' });
      await userEvent.click(card.getByRole('button', { name: 'Consent to writing' }));
    }
    await card.findByRole('button', { name: 'Consent to writing' });
    await start('https://accounts.google.com.evil.test/');
    await waitFor(() => expect(close).toHaveBeenCalled()); expect(replace).not.toHaveBeenCalled();
    // Failed navigation preserves both acknowledgements so the admin can retry.
    mockWsClient.call.mockResolvedValueOnce({ authorization_url: 'https://accounts.google.com/auth', completion: 'paste' });
    await userEvent.click(card.getByRole('button', { name: 'Consent to writing' }));
    const input = await card.findByRole('textbox', { name: 'Paste the URL after Google consent' });
    await userEvent.type(input, 'https://localhost/callback?state=hidden');
    mockWsClient.call.mockImplementation(async (method: string) => method === 'google.drive_write.get' ? { ...state, home_write: { ...state.home_write, connected: true, account_email: 'office@example.test' } } : {});
    await userEvent.click(card.getByRole('button', { name: 'Finish connection' }));
    expect(await card.findByText('office@example.test')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('google.drive_write.consent_home_paste', { url: 'https://localhost/callback?state=hidden' });
  });
  it('disconnects only after confirmation and reports unreadable connection counts', async () => {
    const state = driveSetup({ home_write: { ...DRIVE.home_write, connected: true, account_email: 'office@example.test' } as never, write_store_unreadable: true });
    const card = await driveCard();
    expect(await card.findByText('Members’ connection status cannot be read right now.')).toBeInTheDocument();
    await userEvent.click(card.getByRole('button', { name: 'Disconnect company account writing' }));
    expect(mockWsClient.call.mock.calls.filter(call => call[0] === 'google.drive_write.consent_home_disconnect')).toHaveLength(0);
    mockWsClient.call.mockImplementation(async (method: string) => method === 'google.drive_write.get' ? { ...state, home_write: DRIVE.home_write } : {});
    await userEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Disconnect company account writing' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('google.drive_write.consent_home_disconnect', {}));
    expect(await card.findByRole('button', { name: 'Consent to writing' })).toBeInTheDocument();
  });
});


describe('S-11 sign-in card visibility', () => {
  beforeEach(() => { mockWsClient.call.mockReset(); });
  it.each(['personal', 'member'])('does not render or read sign-in settings for %s', async kind => {
    const previous = useSystemStore.getState().status;
    driveSetup();
    if (kind === 'personal') useSystemStore.setState({ status: { ...previous, edition_profile: 'personal' } as never });
    else useAuthStore.setState({ user: { id: 'member', email: 'member@example.test', display_name: 'Member', role: 'employee', status: 'active' } });
    const view = renderWithProviders(<GoogleIntegrationPage />);
    try {
      await screen.findByTestId('google-gate-on');
      expect(screen.queryByTestId('google-signin-admin')).not.toBeInTheDocument();
      expect(mockWsClient.call).not.toHaveBeenCalledWith('google.signin.get', {});
    } finally { view.unmount(); useSystemStore.setState({ status: previous }); }
  });
});
