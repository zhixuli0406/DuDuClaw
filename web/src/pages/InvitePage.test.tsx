import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { render } from '@/test/render';
import { StrictMode } from 'react';
import { InvitePage, invitePasswordError } from './InvitePage';

const request = vi.fn();
const response = (body: unknown, ok = true) => ({ ok, json: async () => body });
function mount(strict = false) {
  const page = <IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={['/invite']}><Routes><Route path="/invite" element={<InvitePage />} /></Routes></MemoryRouter></IntlProvider>;
  return render(strict ? <StrictMode>{page}</StrictMode> : page);
}
beforeEach(() => { window.history.replaceState({}, '', '/invite#test-token'); request.mockReset(); vi.stubGlobal('fetch', request); });
afterEach(() => vi.unstubAllGlobals());

describe('InvitePage', () => {
  it('keeps the captured token through StrictMode effect replay', async () => {
    request.mockResolvedValue(response({ valid: true })); mount(true);
    expect(await screen.findByLabelText('New password')).toBeInTheDocument();
    expect(window.location.href).not.toContain('test-token');
    expect(request.mock.calls.every(([url, options]) => url === '/api/invite/check' && JSON.parse(options.body).token === 'test-token')).toBe(true);
  });
  it('does not check an invitation without a fragment, including after reload', async () => {
    window.history.replaceState({}, '', '/invite'); mount();
    expect(await screen.findByRole('alert')).toHaveTextContent('This invitation is unavailable.');
    expect(request).not.toHaveBeenCalled();
  });
  it('checks the link without credentials and displays only a masked email', async () => {
    request.mockImplementation(async () => {
      expect(window.location.hash).toBe('');
      return response({ valid: true, email_masked: 'a***@example.com' });
    });
    const view = mount();
    expect(await screen.findByText('a***@example.com')).toBeInTheDocument();
    expect(request).toHaveBeenCalledWith('/api/invite/check', expect.objectContaining({ method: 'POST', body: JSON.stringify({ token: 'test-token' }), headers: { 'Content-Type': 'application/json' }, credentials: 'omit', cache: 'no-store', referrerPolicy: 'no-referrer' }));
    expect(document.querySelector('meta[name="referrer"]')).toHaveAttribute('content', 'no-referrer');
    view.unmount();
    expect(document.querySelector('meta[name="referrer"]')).toBeNull();
  });
  it('shows one unavailable state and no password form for an invalid invitation', async () => {
    request.mockResolvedValue(response({ valid: false })); mount();
    expect(await screen.findByRole('alert')).toHaveTextContent('This invitation is unavailable.');
    expect(screen.queryByLabelText('New password')).toBeNull();
  });
  it('shows a safe error when checking fails, without raw server text', async () => {
    request.mockRejectedValue(new Error('database secret')); mount();
    expect(await screen.findByRole('alert')).toHaveTextContent('Unable to complete this request.');
    expect(screen.queryByText(/database secret/)).toBeNull();
  });
  it('validates matching passwords before submitting, sets the password, and offers normal login', async () => {
    request.mockResolvedValueOnce(response({ valid: true })).mockResolvedValueOnce(response({ valid: true })); mount();
    const password = await screen.findByLabelText('New password');
    const confirmation = screen.getByLabelText('Confirm password');
    fireEvent.change(password, { target: { value: '12345678' } });
    fireEvent.change(confirmation, { target: { value: 'different' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password' }));
    expect(screen.getByRole('alert')).toHaveTextContent('The passwords do not match.');
    expect(request).toHaveBeenCalledTimes(1);
    fireEvent.change(confirmation, { target: { value: '12345678' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password' }));
    expect(await screen.findByText('Your password is set. Sign in to continue.')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Go to sign in' })).toHaveAttribute('href', '/login');
    expect(window.location.hash).toBe('');
    expect(request.mock.calls[1][0]).toBe('/api/invite/redeem');
    expect(request.mock.calls[1][1]).toEqual(expect.objectContaining({ body: JSON.stringify({ token: 'test-token', password: '12345678' }), credentials: 'omit' }));
  });
  it('handles an invite consumed between checking and submitting', async () => {
    request.mockResolvedValueOnce(response({ valid: true })).mockResolvedValueOnce(response({ valid: false })); mount();
    fireEvent.change(await screen.findByLabelText('New password'), { target: { value: '12345678' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: '12345678' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password' }));
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('This invitation is unavailable.'));
  });
  it('translates password policy errors without displaying raw error details', async () => {
    request.mockResolvedValueOnce(response({ valid: true })).mockResolvedValueOnce(response({ code: 'password_policy', error: 'private details' }, false)); mount();
    fireEvent.change(await screen.findByLabelText('New password'), { target: { value: '12345678' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: '12345678' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Use at least 8 characters');
    expect(screen.queryByText(/private details/)).toBeNull();
  });
});

describe('invitePasswordError', () => {
  it('uses Unicode character minimum and UTF-8 byte maximum', () => {
    expect(invitePasswordError('密碼很短', '密碼很短')).toBe('policy');
    expect(invitePasswordError('八個字元密碼足夠', '八個字元密碼足夠')).toBeNull();
    expect(invitePasswordError('a'.repeat(1024), 'a'.repeat(1024))).toBeNull();
    expect(invitePasswordError('密'.repeat(342), '密'.repeat(342))).toBe('policy');
  });
});
