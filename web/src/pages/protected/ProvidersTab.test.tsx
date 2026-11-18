import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import '@/test/mocks';
import en from '@/i18n/en.json';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import { toastBus } from '@/lib/toast';
import type { ProtectedProviderStatus } from '@/lib/api';
import { ProvidersTab } from './ProvidersTab';
import { CSRF, NOT_VERIFIED, VERIFIED, calledWith, employeeList, renderTab, routeCalls } from './test-helpers';

const EMPLOYEE = {
  employee_id: 'a'.repeat(32), label: 'alice-helper', kind: 'personal' as const,
  provisioned: true, providers: [{ name: 'Existing provider', protocol: 'responses' as const, origin: 'https://models.example.com' }],
  source: 'dashboard' as const, updated_unix: 1795000000,
};
const list = (overrides: Partial<ProtectedProviderStatus> = {}): ProtectedProviderStatus => ({
  ...VERIFIED, employees: [EMPLOYEE], next: null, hidden_personal: 1, personal_revealed: false,
  visibility: 'dashboard_changes_only', ...overrides,
});
const RESULT = { provisioned: true, updated_unix: 1795000001, result: 'recorded' };
const KEY = 'secret-only-for-this-test';
const originalFetchStatus = useSystemStore.getState().fetchStatus;

beforeEach(() => {
  vi.clearAllMocks();
  useSystemStore.setState({ fetchStatus: async () => {} });
  useConnectionStore.setState({ state: 'authenticated' });
});
afterEach(() => {
  vi.restoreAllMocks();
  useSystemStore.setState({ status: null, fetchStatus: originalFetchStatus });
});

async function openSet() {
  fireEvent.click(await screen.findByRole('button', { name: 'Set model connections for alice-helper' }));
  return screen.findByRole('dialog');
}
function fill(dialog: HTMLElement, key = KEY) {
  fireEvent.change(within(dialog).getByLabelText('Provider name'), { target: { value: 'Entered provider' } });
  fireEvent.change(within(dialog).getByLabelText('Endpoint URL'), { target: { value: 'https://api.example.com/v1' } });
  fireEvent.change(within(dialog).getByLabelText('API key'), { target: { value: key } });
}
function verify(dialog: HTMLElement) {
  fireEvent.change(within(dialog).getByLabelText('Current password'), { target: { value: 'pw' } });
  fireEvent.change(within(dialog).getByLabelText('Six-digit authenticator code'), { target: { value: '123456' } });
}

// Independent expectations: all documented RPC codes, no server-message passthrough.
const ERRORS = [
  ['invalid_provider', 'invalid_provider'], ['not_allocated', 'not_allocated'], ['busy', 'busy'],
  ['home_entry_unsafe', 'home_entry_unsafe'], ['storage_unavailable', 'storage_unavailable'],
  ['unconfirmed', 'unconfirmed'], ['unreachable', 'unreachable'], ['not_allowed', 'generic'],
  ['admin_required', 'generic'], ['not_available', 'not_available'], ['users_unavailable', 'generic'],
  ['session_expired', 'step_up_required'], ['step_up_required', 'step_up_required'],
  ['request_invalid', 'generic'], ['clock_unavailable', 'generic'], ['employees_unavailable', 'generic'],
  ['records_unavailable', 'generic'], ['audit_unavailable', 'generic'], ['service_unavailable', 'unreachable'],
] as const;

describe('protected mode — model connections tab', () => {
  it('renders recorded employee metadata, visibility, hidden count and paged rows', async () => {
    routeCalls({ 'isolation.provider_status': (params) => params.after
      ? list({ employees: [{ ...EMPLOYEE, employee_id: 'b'.repeat(32), label: 'shared-helper', kind: 'shared', provisioned: false, providers: [], updated_unix: undefined }], next: null })
      : list({ next: 'alice-helper' }) });
    renderTab(<ProvidersTab />);
    expect(await screen.findByText('alice-helper')).toBeInTheDocument();
    expect(screen.getByText('Personal')).toBeInTheDocument();
    expect(screen.getByText('Yes')).toBeInTheDocument();
    expect(screen.getByText(/Existing provider · Responses · https:\/\/models.example.com/)).toBeInTheDocument();
    expect(screen.getByText(/duduclaw service provider set/)).toBeInTheDocument();
    expect(screen.getByText('1 personal employees are hidden.')).toBeInTheDocument();
    expect(document.body).not.toHaveTextContent(EMPLOYEE.employee_id);
    expect(screen.getByText(new Date(EMPLOYEE.updated_unix * 1000).toLocaleString('en', { dateStyle: 'medium', timeStyle: 'short' }))).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
    expect(await screen.findByText('shared-helper')).toBeInTheDocument();
    expect(screen.getByText('No')).toBeInTheDocument();
    expect(calledWith('isolation.provider_status')).toEqual([{}, { after: 'alice-helper' }]);
  });

  it('shows an empty state while keeping the dashboard-only notice visible', async () => {
    routeCalls({ 'isolation.provider_status': () => list({ employees: [], hidden_personal: 0 }) });
    renderTab(<ProvidersTab />);
    expect(await screen.findByText('No allocated AI employees')).toBeInTheDocument();
    expect(screen.getByText(/duduclaw service provider set/)).toBeInTheDocument();
  });

  it('uses the existing step-up dialog before opening the set form', async () => {
    let verified = false;
    routeCalls({
      'isolation.provider_status': () => list(verified ? VERIFIED : NOT_VERIFIED),
      'isolation.employee_list': () => employeeList(verified ? VERIFIED : NOT_VERIFIED),
      'isolation.connector_step_up': () => { verified = true; return { verified: true }; },
    });
    renderTab(<ProvidersTab />);
    const first = await openSet();
    expect(within(first).queryByLabelText('API key')).not.toBeInTheDocument();
    const confirm = within(first).getByRole('button', { name: 'Verify identity and continue' });
    expect(confirm).toBeDisabled();
    verify(first);
    fireEvent.click(confirm);
    expect(await screen.findByLabelText('API key')).toHaveValue('');
    expect(calledWith('isolation.connector_step_up')).toEqual([{ password: 'pw', code: '123456' }]);
    expect(calledWith('isolation.provider_status')).toContainEqual({ employee_id: EMPLOYEE.employee_id, step_up_csrf: CSRF });
  });

  it('reveals personal employees only after an explicit step-up action', async () => {
    let verified = false;
    routeCalls({
      'isolation.provider_status': (params) => list({ ...(verified ? VERIFIED : NOT_VERIFIED), personal_revealed: !!params.step_up_csrf, hidden_personal: params.step_up_csrf ? 0 : 1 }),
      'isolation.employee_list': () => employeeList(verified ? VERIFIED : NOT_VERIFIED),
      'isolation.connector_step_up': () => { verified = true; return { verified: true }; },
    });
    renderTab(<ProvidersTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Verify identity to reveal' }));
    const dialog = await screen.findByRole('dialog');
    verify(dialog);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Verify identity and continue' }));
    expect(await screen.findByText('0 personal employees are hidden.')).toBeInTheDocument();
    expect(calledWith('isolation.provider_status')).toEqual([{}, { step_up_csrf: CSRF }]);
  });

  it('submits exactly the entered provider list and CSRF, clears keys immediately, and never writes storage', async () => {
    let submitted: unknown;
    let finish!: (value: typeof RESULT) => void;
    routeCalls({
      'isolation.provider_status': () => list(),
      'isolation.provider_set': (params) => {
        submitted = structuredClone(params);
        return new Promise((resolve) => { finish = resolve; });
      },
    });
    const view = renderTab(<ProvidersTab />);
    const dialog = await openSet();
    // Both localStorage and sessionStorage use this prototype in jsdom.
    const storage = vi.spyOn(Storage.prototype, 'setItem');
    const emit = vi.spyOn(toastBus, 'emit');
    expect(within(dialog).getByText('Existing provider · https://models.example.com')).toBeInTheDocument();
    expect(within(dialog).getByText(/entire provider list/)).toBeInTheDocument();
    const keyInput = within(dialog).getByLabelText('API key');
    expect(keyInput).toHaveAttribute('type', 'password');
    expect(keyInput).toHaveAttribute('autocomplete', 'off');
    expect(keyInput).toHaveValue('');
    fill(dialog);
    fireEvent.change(within(dialog).getByLabelText('Protocol'), { target: { value: 'anthropic' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(submitted).toEqual({ employee_id: EMPLOYEE.employee_id, step_up_csrf: CSRF,
      providers: [{ name: 'Entered provider', base_url: 'https://api.example.com/v1', api_key: KEY, protocol: 'anthropic' }] }));
    expect(keyInput).toHaveValue('');
    expect(document.body).not.toHaveTextContent(KEY);
    finish(RESULT);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(emit).toHaveBeenCalledWith(expect.objectContaining({ variant: 'success', message: 'Model connections updated' }));
    expect(calledWith('isolation.provider_status')).toHaveLength(2);
    const reopened = await openSet();
    expect(within(reopened).getByLabelText('API key')).toHaveValue('');
    fill(reopened, 'discard-on-close');
    fireEvent.click(within(reopened).getByRole('button', { name: 'Cancel' }));
    const again = await openSet();
    expect(within(again).getByLabelText('API key')).toHaveValue('');
    fill(again, 'discard-on-unmount');
    view.unmount();
    expect(document.body).not.toHaveTextContent(KEY);
    expect(document.querySelector('input[type="password"]')).toBeNull();
    expect(storage).not.toHaveBeenCalled();
  });

  it('submits multiple provider rows with the default chat completions protocol', async () => {
    let submitted: unknown;
    routeCalls({ 'isolation.provider_status': () => list(), 'isolation.provider_set': (params) => { submitted = structuredClone(params); return RESULT; } });
    renderTab(<ProvidersTab />);
    const dialog = await openSet();
    fill(dialog);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Add provider' }));
    const second = within(dialog).getByRole('group', { name: 'Provider 2' });
    fireEvent.change(within(second).getByLabelText('Provider name'), { target: { value: 'Other' } });
    fireEvent.change(within(second).getByLabelText('Endpoint URL'), { target: { value: 'https://other.example.com' } });
    fireEvent.change(within(second).getByLabelText('API key'), { target: { value: 'other-secret' } });
    fireEvent.change(within(second).getByLabelText('Protocol'), { target: { value: 'gemini' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(submitted).toEqual({ employee_id: EMPLOYEE.employee_id, step_up_csrf: CSRF, providers: [
      { name: 'Entered provider', base_url: 'https://api.example.com/v1', api_key: KEY, protocol: 'chat_completions' },
      { name: 'Other', base_url: 'https://other.example.com', api_key: 'other-secret', protocol: 'gemini' },
    ] }));
    expect(document.body).not.toHaveTextContent('other-secret');
  });

  it.each(ERRORS)('maps %s to the %s message key without displaying server text', async (code, suffix) => {
    routeCalls({ 'isolation.provider_status': () => Promise.reject({ code, message: 'connectors-server-message-with-secret' }) });
    renderTab(<ProvidersTab />);
    const template = en[`protectedMode.providers.error.${suffix}` as keyof typeof en];
    expect(await screen.findByRole('alert')).toHaveTextContent(template.replace('{code}', code));
    expect(document.body).not.toHaveTextContent('connectors-server-message-with-secret');
  });

  it.each(['step_up_required', 'session_expired'])('reopens verification after %s and clears the submitted key', async (code) => {
    routeCalls({
      'isolation.provider_status': () => list(),
      'isolation.provider_set': () => Promise.reject({ code, message: KEY }),
      'isolation.connector_step_up': () => Promise.reject({ code: 'request_invalid', message: KEY }),
      'isolation.employee_list': () => employeeList(NOT_VERIFIED),
    });
    renderTab(<ProvidersTab />);
    const dialog = await openSet();
    fill(dialog);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    expect(await within(dialog).findByLabelText('Current password')).toHaveValue('');
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Verify your identity again');
    expect(within(dialog).getByLabelText('API key')).toHaveValue('');
    expect(document.body).not.toHaveTextContent(KEY);
    // Even a failed identity-verification attempt discards newly entered keys.
    fill(dialog);
    verify(dialog);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Verify identity and save' }));
    await waitFor(() => expect(within(dialog).getByLabelText('API key')).toHaveValue(''));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Operation failed (request_invalid)');
    expect(document.body).not.toHaveTextContent(KEY);
  });

  it('shows code-based mutation refusals and allows safe re-entry after an unconfirmed result', async () => {
    routeCalls({
      'isolation.provider_status': () => list(),
      'isolation.provider_set': () => Promise.reject({ code: 'unconfirmed', message: KEY }),
      'isolation.employee_list': () => employeeList(),
    });
    renderTab(<ProvidersTab />);
    const dialog = await openSet();
    fill(dialog);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Result unconfirmed: it is safe to submit the same content again.');
    expect(within(dialog).getByLabelText('API key')).toHaveValue('');
    expect(document.body).not.toHaveTextContent(KEY);
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled();
  });

  it('warns when the write succeeds but the status was not recorded', async () => {
    routeCalls({ 'isolation.provider_status': () => list(), 'isolation.provider_set': () => ({ ...RESULT, result: 'not_recorded' }) });
    renderTab(<ProvidersTab />);
    const dialog = await openSet();
    fill(dialog);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Written, but the status record was not updated')).toBeInTheDocument();
    expect(calledWith('isolation.provider_status')).toHaveLength(2);
    expect(document.body).not.toHaveTextContent(KEY);
  });

  it('requires confirmation and step-up before clearing, and cancel changes nothing', async () => {
    let verified = false;
    routeCalls({
      'isolation.provider_status': () => list(verified ? VERIFIED : NOT_VERIFIED),
      'isolation.employee_list': () => employeeList(verified ? VERIFIED : NOT_VERIFIED),
      'isolation.connector_step_up': () => { verified = true; return { verified: true }; },
      'isolation.provider_clear': () => ({ ...RESULT, provisioned: false }),
    });
    renderTab(<ProvidersTab />);
    const clear = await screen.findByRole('button', { name: 'Clear model connections for alice-helper' });
    fireEvent.click(clear);
    let dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/unable to call models until you set them up again/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(calledWith('isolation.provider_clear')).toEqual([]);
    fireEvent.click(clear);
    dialog = await screen.findByRole('dialog');
    verify(dialog);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Verify identity and clear' }));
    await waitFor(() => expect(calledWith('isolation.provider_clear')).toEqual([{ employee_id: EMPLOYEE.employee_id, step_up_csrf: CSRF }]));
  });
});
