import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { MyConnectionsPage } from './MyConnectionsPage';

const status: PersonalGoogleStatus = {
  drive_write: { connected: false, status: null, account_email: null, granted_scopes: [] }, enabled_by_admin: false, connection: { owner_id: 'alice', account_email: 'other@gmail.com', granted_scopes: ['https://www.googleapis.com/auth/gmail.readonly'], local_purposes: ['base'], status: 'connected', connected_at: '2026-10-23T00:00:00Z' }, login_email: 'alice@example.com', client_configured: true, integration_enabled: false, requires_login: false,
  requests: [{ agent_id: 'helper', allowed: null, requesters: 'bound_people', bound_people: [{ user_id:'bob', name:'Bob', email:'bob@example.com' }], channels: ['line'], public_visitors_allowed:false }], delegated: [], recent_usage: [] };

beforeEach(() => { useAuthStore.setState({ user: null }); vi.restoreAllMocks(); vi.spyOn(api.personalGoogle,'status').mockResolvedValue(status); vi.spyOn(api.personalGoogle, 'writeConfirmations').mockResolvedValue({ requests: [] }); });

describe('MyConnectionsPage', () => {
  it('renders only the escaped Chat confirmation display text', async () => {
    const raw = '私人\u202e\u2066\u200b\u0001\n文字';
    const textDisplay = '私人\\u{202e}\\u{2066}\\u{200b}\\u{1}\\u{a}文字';
    // An unexpected legacy raw field must never become a rendering fallback.
    const request = {
      id: 'chat-confirmation', employee: 'helper', account: 'alice@example.com',
      space: 'spaces/A\\u{200b}', thread: 'spaces/A/threads/T\\u{2066}',
      text_display: textDisplay, text: raw, expires_at: '2099-11-25T00:00:00Z',
    };
    vi.spyOn(api.personalGoogle, 'chatConfirmations').mockResolvedValue({ requests: [request] });
    const { container } = renderWithProviders(<MyConnectionsPage />);
    const preview = await screen.findByText(textDisplay);
    expect(preview.tagName).toBe('PRE');
    expect(preview.textContent).toBe(textDisplay);
    expect(screen.getByText('spaces/A\\u{200b} spaces/A/threads/T\\u{2066}')).toBeInTheDocument();
    for (const control of ['\u202e', '\u2066', '\u200b', '\u0001']) {
      expect(container.textContent).not.toContain(control);
    }
  });

  it('shows persisted administrator test notices even without a connected account', async () => {
    vi.mocked(api.personalGoogle.status).mockResolvedValue({ ...status, connection: null, notices: [
      { id: 1, agent_id: 'helper', actor_id: 'admin', kind: 'google_identity_test', timestamp: '2026-11-25T00:00:00Z' },
    ] });
    renderWithProviders(<MyConnectionsPage />);
    const notice = await screen.findByRole('status');
    expect(notice).toHaveTextContent('administrator requested a Google connection and permission check through helper');
    expect(notice).toHaveTextContent('does not read mail, files or calendars');
  });

  it('asks a connected member missing Chat scopes to reconnect', async () => {
    vi.mocked(api.personalGoogle.status).mockResolvedValue({ ...status, chat_read: true });
    renderWithProviders(<MyConnectionsPage />);
    expect(await screen.findByText('Reconnect to add Google Chat.')).toBeInTheDocument();
  });

  it('does not ask for Chat again when both scopes and the purpose are present', async () => {
    vi.mocked(api.personalGoogle.status).mockResolvedValue({ ...status, chat_read: true, connection: {
      ...status.connection!, local_purposes: ['base', 'chat_read'], granted_scopes: [
        'https://www.googleapis.com/auth/chat.spaces.readonly',
        'https://www.googleapis.com/auth/chat.messages.readonly',
      ],
    } });
    renderWithProviders(<MyConnectionsPage />);
    await screen.findByText('other@gmail.com');
    expect(screen.queryByText('Reconnect to add Google Chat.')).not.toBeInTheDocument();
  });

  it('does not advertise employee Drive access from extra provider scopes alone', async () => {
    vi.mocked(api.personalGoogle.status).mockResolvedValue({...status, connection: {...status.connection!,
      local_purposes: ['base', 'meeting_recordings'],
      granted_scopes: ['https://www.googleapis.com/auth/gmail.readonly', 'https://www.googleapis.com/auth/drive.readonly'],
    }});
    renderWithProviders(<MyConnectionsPage/>);
    const services = await screen.findByText(/^Available services:/);
    expect(services).toHaveTextContent('Gmail');
    expect(services).not.toHaveTextContent('Drive');
  });

  it('requires an in-product Google data disclosure acknowledgement before OAuth starts', async () => {
    vi.mocked(api.personalGoogle.status).mockResolvedValue({...status, connection: null});
    const start = vi.spyOn(api.personalGoogle,'start').mockResolvedValue({authorization_url:'https://accounts.google.com/fixture',state:'fixture',completion:'paste',redirect_uri:'http://127.0.0.1:18889/api/me/google/callback',expires_in:600});
    vi.spyOn(window,'open').mockReturnValue({opener:null,location:{replace:vi.fn()},close:vi.fn()} as never);
    const user = userEvent.setup();
    renderWithProviders(<MyConnectionsPage/>);

    expect(await screen.findByText(/Google data may be stored in local conversation/i)).toBeInTheDocument();
    expect(screen.getByText(/must not use Google data to train or improve a generalized AI/i)).toBeInTheDocument();
    expect(screen.getByRole('link',{name:/privacy policy/i})).toHaveAttribute('href','https://dudustudio.monster/duduclaw/privacy/');
    const connect = screen.getByRole('button',{name:'Connect my Google'});
    expect(connect).toBeDisabled();
    expect(start).not.toHaveBeenCalled();

    await user.click(screen.getByRole('checkbox',{name:/understand how my Google data is accessed/i}));
    expect(connect).toBeEnabled();
    await user.click(connect);
    await waitFor(()=>expect(start).toHaveBeenCalledTimes(1));
    expect(start).toHaveBeenCalledWith(true, false, false, false);
  });

  it('requires a separate consent before requesting recording scopes', async () => {
    const start = vi.spyOn(api.personalGoogle, 'start').mockResolvedValue({ authorization_url: 'https://accounts.google.com/fixture', state: 'fixture', completion: 'paste', redirect_uri: 'http://127.0.0.1:18889/api/me/google/callback', expires_in: 600 });
    vi.spyOn(window, 'open').mockReturnValue({ opener: null, location: { replace: vi.fn() }, close: vi.fn() } as never);
    const user = userEvent.setup(); renderWithProviders(<MyConnectionsPage />);
    const authorize = await screen.findByRole('button', { name: 'Authorize recording imports separately' });
    expect(authorize).toBeDisabled();
    expect(screen.getByText('https://www.googleapis.com/auth/drive.file')).toBeInTheDocument();
    expect(screen.getByText('https://www.googleapis.com/auth/meetings.space.readonly')).toBeInTheDocument();
    await user.click(screen.getByRole('checkbox', { name: /understand how my Google data is accessed/i }));
    expect(authorize).toBeDisabled();
    await user.click(screen.getByRole('checkbox', { name: /agree to these additional recording import permissions/i }));
    await user.click(authorize);
    await waitFor(() => expect(start).toHaveBeenCalledWith(true, true, false, false));
  });

  it('requires an independent broad Drive consent and does not request recording scopes with it', async () => {
    const start = vi.spyOn(api.personalGoogle, 'start').mockResolvedValue({ authorization_url: 'https://accounts.google.com/fixture', state: 'fixture', completion: 'paste', redirect_uri: 'http://127.0.0.1:18889/api/me/google/callback', expires_in: 600 });
    vi.spyOn(window, 'open').mockReturnValue({ opener: null, location: { replace: vi.fn() }, close: vi.fn() } as never);
    const user = userEvent.setup(); renderWithProviders(<MyConnectionsPage />);
    const authorize = await screen.findByRole('button', { name: 'Enable employee Drive tools' });
    expect(authorize).toBeDisabled();
    expect(screen.getByText('https://www.googleapis.com/auth/drive.readonly')).toBeInTheDocument();
    await user.click(screen.getByRole('checkbox', { name: /understand how my Google data is accessed/i }));
    await user.click(screen.getByRole('checkbox', { name: /agree to these additional recording import permissions/i }));
    expect(authorize).toBeDisabled();
    await user.click(screen.getByRole('checkbox', { name: /allow read-only access to all Drive files my Google account can access/i }));
    await user.click(authorize);
    await waitFor(() => expect(start).toHaveBeenCalledWith(true, false, true, false));
  });

  it('shows that Anyone includes external tools while public visitors remain excluded',async()=>{
    vi.mocked(api.personalGoogle.status).mockResolvedValue({...status,requests:status.requests.map(request=>({...request,requesters:'anyone'}))});
    renderWithProviders(<MyConnectionsPage/>);
    expect(await screen.findByText(/including authorized external tools/i)).toBeInTheDocument();
    expect(screen.getByText(/Public visitors can never use your account/i)).toBeInTheDocument();
  });
  it('keeps the exact registered return URI visible and copyable after starting',async()=>{
    const user=userEvent.setup();
    const write=vi.spyOn(navigator.clipboard,'writeText').mockResolvedValue();
    vi.spyOn(window,'open').mockReturnValue({opener:null,location:{replace:vi.fn()},close:vi.fn()} as never);
    vi.spyOn(api.personalGoogle,'start').mockResolvedValue({authorization_url:'https://accounts.google.com/fixture',state:'fixture',completion:'paste',redirect_uri:'http://127.0.0.1:18889/api/me/google/callback',expires_in:600});
    renderWithProviders(<MyConnectionsPage/>);
    await user.click(await screen.findByRole('checkbox',{name:/understand how my Google data is accessed/i}));
    await user.click(await screen.findByRole('button',{name:'Reconnect'}));
    expect(await screen.findByDisplayValue('http://127.0.0.1:18889/api/me/google/callback')).toBeInTheDocument();
    await user.click(screen.getByRole('button',{name:'Copy return address'}));
    expect(write).toHaveBeenCalledWith('http://127.0.0.1:18889/api/me/google/callback');
  });
  it('explains different account, disabled integration and shared access before consent', async () => {
    const consent = vi.spyOn(api.personalGoogle,'consent').mockResolvedValue({});
    renderWithProviders(<MyConnectionsPage />);
    await screen.findByText('other@gmail.com');
    expect(screen.getByText(/different from your sign-in email/i)).toBeInTheDocument();
    expect(screen.getByText(/administrator has not enabled/i)).toBeInTheDocument();
    expect(screen.getByText(/Bob/)).toBeInTheDocument();
    expect(consent).not.toHaveBeenCalled();
    const user = userEvent.setup();
    await user.click(screen.getByRole('checkbox', { name: /understand who can use/i }));
    await user.click(screen.getByRole('button', { name: /^Allow$/ }));
    await waitFor(() => expect(consent).toHaveBeenCalledWith('helper',true));
  });
});

it('shows Chat reconnect guidance after the administrator opts in', async () => {
  vi.mocked(api.personalGoogle.status).mockResolvedValue({...status,chat_read:true});
  renderWithProviders(<MyConnectionsPage/>);
  expect(await screen.findByText(/users reconnect Google in My Connections/)).toBeInTheDocument();
});

it('keeps the admin chat link card visible when Google fails and retries Google', async () => {
  useAuthStore.setState({ user: { id: 'admin', email: 'a@example.com', display_name: 'Admin', role: 'admin', status: 'active' } });
  vi.spyOn(api.chatLink, 'list').mockResolvedValue({ identities: [], handshakes: [] });
  vi.mocked(api.personalGoogle.status).mockRejectedValueOnce(new Error('Google unavailable'));
  renderWithProviders(<MyConnectionsPage />);
  expect(await screen.findByText('Chat account links (administrators)')).toBeInTheDocument();
  expect(await screen.findByRole('alert')).toBeInTheDocument();
  await userEvent.setup().click(screen.getByRole('button', { name: 'Try again' }));
  expect(await screen.findByText('other@gmail.com')).toBeInTheDocument();
  expect(api.personalGoogle.status).toHaveBeenCalledTimes(2);
});
it('hides chat link management for non-admins', async () => {
  useAuthStore.setState({ user: { id: 'member', email: 'm@example.com', display_name: 'Member', role: 'employee', status: 'active' } });
  const list = vi.spyOn(api.chatLink, 'list');
  renderWithProviders(<MyConnectionsPage />);
  await screen.findByText('other@gmail.com');
  expect(screen.queryByText('Chat account links (administrators)')).not.toBeInTheDocument();
  expect(list).not.toHaveBeenCalled();
});


it('shows member Drive write access on the connections page', async () => {
  vi.mocked(api.personalGoogle.status).mockResolvedValue({ ...status, enabled_by_admin: true });
  renderWithProviders(<MyConnectionsPage />);
  expect(await screen.findByRole('button', { name: 'Link Drive write access' })).toBeDisabled();
  expect(api.personalGoogle.writeConfirmations).toHaveBeenCalledOnce();
});

it('reuses the existing Google return path for a Drive write connection', async () => {
  const finish = vi.spyOn(api.personalGoogle, 'finish').mockResolvedValue({});
  const { MemoryRouter } = await import('react-router');
  const { IntlProvider } = await import('react-intl');
  const { render } = await import('@testing-library/react');
  const { default: messages } = await import('@/i18n/en.json');
  render(<IntlProvider locale="en" messages={messages}><MemoryRouter initialEntries={['/me/connections?google_state=drive-state']}><MyConnectionsPage /></MemoryRouter></IntlProvider>);
  await waitFor(() => expect(finish).toHaveBeenCalledWith('drive-state'));
  expect(finish).toHaveBeenCalledOnce();
});
