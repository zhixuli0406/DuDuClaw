import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import { MyConnectionsPage } from './MyConnectionsPage';

const status: PersonalGoogleStatus = { connection: { owner_id: 'alice', account_email: 'other@gmail.com', granted_scopes: ['https://www.googleapis.com/auth/gmail.readonly'], local_purposes: ['base'], status: 'connected', connected_at: '2026-10-23T00:00:00Z' }, login_email: 'alice@example.com', client_configured: true, integration_enabled: false, requires_login: false,
  requests: [{ agent_id: 'helper', allowed: null, requesters: 'bound_people', bound_people: [{ user_id:'bob', name:'Bob', email:'bob@example.com' }], channels: ['line'], public_visitors_allowed:false }], delegated: [], recent_usage: [] };

beforeEach(() => { vi.restoreAllMocks(); vi.spyOn(api.personalGoogle,'status').mockResolvedValue(status); });

describe('MyConnectionsPage', () => {
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
    expect(start).toHaveBeenCalledWith(true, false, false);
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
    await waitFor(() => expect(start).toHaveBeenCalledWith(true, true, false));
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
    await waitFor(() => expect(start).toHaveBeenCalledWith(true, false, true));
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
