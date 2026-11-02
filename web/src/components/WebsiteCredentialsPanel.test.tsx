import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { WebsiteCredentialsPanel } from './WebsiteCredentialsPanel';

const RECORD = {
  id: 'credential-1', label: 'Work account', login_url: 'https://example.com/login',
  origin: 'https://example.com', login_path: '/login', allowed_paths: [],
  username: 'alice@example.com', allowed_agents: ['alice'],
};

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  useConnectionStore.setState({ state: 'authenticated' });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'website_credentials.list') return { credentials: [RECORD] };
    if (method === 'agents.list') return { agents: [{ name: 'alice', display_name: 'Alice' }] };
    return {};
  });
});

it('does not load or show credential controls to non-admin users', () => {
  useAuthStore.setState({ user: { id: 'viewer', role: 'viewer' } as never });
  renderWithProviders(<WebsiteCredentialsPanel />);
  expect(screen.queryByText('Website login credentials')).not.toBeInTheDocument();
  expect(mockWsClient.call).not.toHaveBeenCalled();
});

it('never reads a password back when editing, and clears it before save resolves', async () => {
  let finish!: () => void;
  const pending = new Promise<void>((resolve) => { finish = resolve; });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'website_credentials.list') return { credentials: [{ ...RECORD, password: 'must-not-be-read' }] };
    if (method === 'agents.list') return { agents: [{ name: 'alice', display_name: 'Alice' }] };
    if (method === 'website_credentials.upsert') return pending;
    return {};
  });
  renderWithProviders(<WebsiteCredentialsPanel />);
  fireEvent.click(await screen.findByRole('button', { name: 'Edit website login' }));
  const password = screen.getByLabelText('Password');
  expect(password).toHaveAttribute('type', 'password');
  expect(password).toHaveAttribute('autocomplete', 'off');
  expect(password).toHaveValue('');
  fireEvent.change(password, { target: { value: 'entered-secret' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save website login' }));
  expect(mockWsClient.call).toHaveBeenCalledWith('website_credentials.upsert', {
    id: RECORD.id, label: RECORD.label, login_url: RECORD.login_url,
    username: RECORD.username, allowed_agents: RECORD.allowed_agents, allowed_paths: [],
    password: 'entered-secret',
  });
  expect(password).toHaveValue('');
  finish();
  await waitFor(() => expect(screen.queryByLabelText('Password')).not.toBeInTheDocument());
  expect(screen.queryByText('must-not-be-read')).not.toBeInTheDocument();
});

it('clears a failed save and never displays a backend error containing the password', async () => {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'website_credentials.list') return { credentials: [RECORD] };
    if (method === 'agents.list') return { agents: [{ name: 'alice', display_name: 'Alice' }] };
    throw new Error('echoed-secret');
  });
  renderWithProviders(<WebsiteCredentialsPanel />);
  fireEvent.click(await screen.findByRole('button', { name: 'Edit website login' }));
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'echoed-secret' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save website login' }));
  expect(await screen.findByRole('alert')).not.toHaveTextContent('echoed-secret');
  expect(screen.getByLabelText('Password')).toHaveValue('');
});

it('requires an allowed employee and confirms deletion', async () => {
  renderWithProviders(<WebsiteCredentialsPanel />);
  fireEvent.click(await screen.findByRole('button', { name: 'Edit website login' }));
  fireEvent.click(screen.getByRole('checkbox', { name: 'Alice (alice)' }));
  expect(screen.getByRole('button', { name: 'Save website login' })).toBeDisabled();
  fireEvent.click(screen.getByRole('button', { name: 'Delete website login' }));
  expect(mockWsClient.call).not.toHaveBeenCalledWith('website_credentials.delete', expect.anything());
  const buttons = screen.getAllByRole('button', { name: 'Delete website login' });
  fireEvent.click(buttons[1]);
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('website_credentials.delete', { id: RECORD.id }));
});

it('edits only explicit second-step paths and sends no derived URL fields', async () => {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'website_credentials.list') return { credentials: [{ ...RECORD, allowed_paths: ['/password'] }] };
    if (method === 'agents.list') return { agents: [{ name: 'alice', display_name: 'Alice' }] };
    return {};
  });
  renderWithProviders(<WebsiteCredentialsPanel />);
  fireEvent.click(await screen.findByRole('button', { name: 'Edit website login' }));
  const loginUrl = screen.getByLabelText('Login URL (https://example.com/login)');
  expect(loginUrl).toHaveValue(RECORD.login_url);
  expect(loginUrl).toHaveAttribute('type', 'url');
  expect(loginUrl).toHaveAttribute('pattern', 'https://.+');
  const paths = screen.getByLabelText('Additional second-step paths (optional)');
  expect(paths).toHaveValue('/password');
  fireEvent.change(paths, { target: { value: '/password\n /signin/password \n\n' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'entered-secret' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save website login' }));
  expect(mockWsClient.call).toHaveBeenCalledWith('website_credentials.upsert', {
    id: RECORD.id, label: RECORD.label, login_url: RECORD.login_url,
    username: RECORD.username, allowed_agents: RECORD.allowed_agents,
    allowed_paths: ['/password', '/signin/password'], password: 'entered-secret',
  });
});

it('creates logins with no additional second-step paths by default', async () => {
  renderWithProviders(<WebsiteCredentialsPanel />);
  fireEvent.click(await screen.findByRole('button', { name: 'Add website login' }));
  fireEvent.change(screen.getByLabelText('Label'), { target: { value: 'New account' } });
  fireEvent.change(screen.getByLabelText('Login URL (https://example.com/login)'), { target: { value: 'https://example.com/signin' } });
  fireEvent.change(screen.getByLabelText('Username or email'), { target: { value: 'user' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'new-secret' } });
  fireEvent.click(screen.getByRole('checkbox', { name: 'Alice (alice)' }));
  fireEvent.click(screen.getByRole('button', { name: 'Save website login' }));
  expect(mockWsClient.call).toHaveBeenCalledWith('website_credentials.upsert', {
    id: undefined, label: 'New account', login_url: 'https://example.com/signin',
    username: 'user', allowed_agents: ['alice'], allowed_paths: [], password: 'new-secret',
  });
});

it('rejects an HTTP login URL before submitting the form', async () => {
  renderWithProviders(<WebsiteCredentialsPanel />);
  fireEvent.click(await screen.findByRole('button', { name: 'Edit website login' }));
  const loginUrl = screen.getByLabelText('Login URL (https://example.com/login)');
  fireEvent.change(loginUrl, { target: { value: 'http://localhost/login' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'entered-secret' } });
  expect(loginUrl).toBeInvalid();
  fireEvent.click(screen.getByRole('button', { name: 'Save website login' }));
  expect(mockWsClient.call).not.toHaveBeenCalledWith('website_credentials.upsert', expect.anything());
});
