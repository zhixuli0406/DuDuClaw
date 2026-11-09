import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { useConnectionStore } from '@/stores/connection-store';
import type { AgentGoogleIdentity, ProtectedTenantSourceView } from '@/lib/api';
import { TenantSourceTab, TENANT_SOURCE_SAVE_AVAILABLE } from './TenantSourceTab';
import { CSRF, calledWith, employeeList, renderTab, routeCalls } from './test-helpers';

const GMAIL_READ = 'https://www.googleapis.com/auth/gmail.readonly';
const GMAIL_COMPOSE = 'https://www.googleapis.com/auth/gmail.compose';
const view = (over: Partial<ProtectedTenantSourceView> = {}): ProtectedTenantSourceView => ({
  key: { client_email: 'tenant-bot@example-project.iam.gserviceaccount.com', client_id: '1234567890',
    imported_at: '2026-11-01T08:00:00Z', key_id: 'protected-key-01' },
  key_error: null,
  settings: { service_mailboxes: ['support@example.com'], tenant_scopes: [GMAIL_READ] },
  settings_error: null,
  home_subject: null,
  required_scopes: [GMAIL_READ, GMAIL_COMPOSE],
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' });
});

describe('protected mode — Google tenant source tab', () => {
  it('shows the current source without the key, plus mailboxes and allowed permissions', async () => {
    routeCalls({ 'google.delegation.get': () => view() });
    renderTab(<TenantSourceTab />);
    expect(await screen.findByText('Domain-wide delegation')).toBeInTheDocument();
    expect(screen.getByText('tenant-bot@example-project.iam.gserviceaccount.com')).toBeInTheDocument();
    expect(screen.getByText('protected-key-01')).toBeInTheDocument();
    expect(screen.getByText('Available')).toBeInTheDocument();
    expect(screen.getByText(/The key itself is never shown/)).toBeInTheDocument();
    expect(screen.getByText(/only for the colleague who asked, or for a service mailbox listed below/)).toBeInTheDocument();
    expect(screen.getByText('support@example.com')).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: /Read mail/ })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: /Write mail drafts/ })).not.toBeChecked();
  });

  it('accepts only non-person service mailboxes and keeps saving gated', async () => {
    routeCalls({ 'google.delegation.get': () => view() });
    renderTab(<TenantSourceTab />);
    const input = await screen.findByLabelText('Add service mailbox');
    const add = screen.getByRole('button', { name: 'Add' });

    fireEvent.change(input, { target: { value: 'Alice@Example.com' } });
    fireEvent.click(add);
    expect(screen.getByRole('alert')).toHaveTextContent('colleague’s account email');

    fireEvent.change(input, { target: { value: 'not-an-address' } });
    fireEvent.click(add);
    expect(screen.getByRole('alert')).toHaveTextContent('Enter a valid email address.');

    fireEvent.change(input, { target: { value: 'sales@example.com' } });
    fireEvent.click(add);
    expect(screen.getByText('sales@example.com')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Remove support@example.com' }));
    expect(screen.queryByText('support@example.com')).not.toBeInTheDocument();

    // No gateway RPC writes these settings yet: save stays off and says why.
    expect(TENANT_SOURCE_SAVE_AVAILABLE).toBe(false);
    expect(screen.getByRole('note')).toHaveTextContent('can’t be saved from here');
    expect(screen.getByRole('button', { name: /^(Save|Verify identity and save)$/ })).toBeDisabled();
    expect(screen.queryByLabelText('Current password')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }));
    expect(screen.getByText('support@example.com')).toBeInTheDocument();
    expect(screen.queryByText('sales@example.com')).not.toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.map((call: unknown[]) => call[0])).toEqual(['google.delegation.get']);
  });

  it('explains that no permission means status only', async () => {
    routeCalls({ 'google.delegation.get': () => view({ settings: { service_mailboxes: [], tenant_scopes: [] } }) });
    renderTab(<TenantSourceTab />);
    expect(await screen.findByText(/only the connection status can be viewed/)).toBeInTheDocument();
    expect(screen.getByText('No service mailboxes yet.')).toBeInTheDocument();
  });

  it('shows an unset source', async () => {
    routeCalls({ 'google.delegation.get': () => view({ key: null, home_subject: null }) });
    renderTab(<TenantSourceTab />);
    expect(await screen.findAllByText('Not set up')).toHaveLength(2);
  });

  it('shows the service-account source by the mailbox it acts as', async () => {
    routeCalls({ 'google.delegation.get': () => view({ key: null, home_subject: 'ops@example.com' }) });
    renderTab(<TenantSourceTab />);
    expect(await screen.findByText('Service account')).toBeInTheDocument();
    expect(screen.getByText('ops@example.com')).toBeInTheDocument();
  });

  it('shows why a sealed key cannot be used', async () => {
    routeCalls({ 'google.delegation.get': () => view({ key_error: '封存的服務帳號金鑰紀錄已損壞' }) });
    renderTab(<TenantSourceTab />);
    expect(await screen.findByText('Unavailable')).toBeInTheDocument();
    expect(screen.getByText('封存的服務帳號金鑰紀錄已損壞')).toBeInTheDocument();
  });

  it('shows the server’s refusal when the source cannot load', async () => {
    routeCalls({ 'google.delegation.get': () => Promise.reject('請以管理員帳號重新登入後再操作。') });
    renderTab(<TenantSourceTab />);
    expect(await screen.findByRole('alert')).toHaveTextContent('請以管理員帳號重新登入後再操作。');
  });

  // SR4 round 3 (N1): the Admin confirms the listed holder; a self-set
  // address needs a second, explicit checkbox; the request names the holder.
  it('sr4r3 confirms the listed holder and requires the self-set acknowledgement', async () => {
    const agents: AgentGoogleIdentity[] = [{
      agent_id: 'ceo-assistant', kind: 'delegate', email: 'ceo@example.com', explicit: true, fallback: 'none',
      requesters: 'bound_people', state: 'ready', message: '',
      tenant_subject: {
        state: 'attestation_required', message: '需要管理員確認', subject: 'ceo@example.com', kind: 'delegate',
        holder: { user_id: 'u-mallory', name: 'Mallory Wu', created_at: '2026-10-01T08:00:00Z', email_self_set: true },
      },
    }];
    routeCalls({
      'google.delegation.get': () => view({ agents }),
      'isolation.connector_step_up': () => ({ verified: true }),
      'isolation.employee_list': () => employeeList(),
      'google.delegation.attest_subject': () => ({ agent_id: 'ceo-assistant', subject: 'ceo@example.com',
        user_id: 'u-mallory', attested_at: '2026-11-09T00:00:00Z', email_self_set: true,
        acknowledged_self_set: true, attestation: 'ok' }),
    });
    renderTab(<TenantSourceTab />);
    const row = await screen.findByTestId('attest-row-ceo-assistant');
    expect(row).toHaveTextContent('Mallory Wu');
    expect(row).toHaveTextContent('u-mallory');
    expect(row).toHaveTextContent('changed their own sign-in email');

    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    const person = await screen.findByRole('checkbox', { name: /is the personal mailbox of Mallory Wu/ });
    const selfSet = screen.getByRole('checkbox', { name: /set this address themselves/ });
    fireEvent.change(screen.getByLabelText('Current password'), { target: { value: 'pw' } });
    fireEvent.change(screen.getByLabelText('Six-digit authenticator code'), { target: { value: '123456' } });
    const submit = screen.getByRole('button', { name: 'Verify identity and confirm' });
    expect(submit).toBeDisabled();
    fireEvent.click(person);
    expect(submit).toBeDisabled();
    fireEvent.click(selfSet);
    expect(submit).toBeEnabled();
    fireEvent.click(submit);

    await waitFor(() => expect(calledWith('google.delegation.attest_subject')).toHaveLength(1));
    expect(calledWith('google.delegation.attest_subject')[0]).toEqual({
      agent_id: 'ceo-assistant', subject: 'ceo@example.com', user_id: 'u-mallory', acknowledge_self_set: true,
      csrf_token: CSRF,
    });
  });
});
