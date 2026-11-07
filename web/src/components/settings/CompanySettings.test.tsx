import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useCompanyStore } from '@/stores/company-store';
import { useAuthStore } from '@/stores/auth-store';
import { CompanySettings, validCompanyLogo } from './CompanySettings';

const call = vi.hoisted(() => vi.fn());
const fetcher = vi.fn();
vi.mock('@/lib/ws-client', () => ({ client: { call } }));

beforeEach(() => {
  call.mockReset();
  fetcher.mockReset();
  vi.stubGlobal('fetch', fetcher);
  useAuthStore.setState({ jwt: 'access-token' });
  useCompanyStore.getState().setCompany(null);
  call.mockResolvedValue({ name: 'Acme', logo_url: null });
});
afterEach(() => {
  vi.unstubAllGlobals();
  useAuthStore.setState({ jwt: null });
});

describe('CompanySettings', () => {
  it('checks size and supported MIME types before reading a selected file', () => {
    expect(validCompanyLogo({ size: 1024 * 1024, type: 'image/png' })).toBe(true);
    expect(validCompanyLogo({ size: 1024 * 1024 + 1, type: 'image/png' })).toBe(false);
    expect(validCompanyLogo({ size: 1, type: 'image/svg+xml' })).toBe(false);
    expect(validCompanyLogo({ size: 0, type: 'image/jpeg' })).toBe(false);
  });
  it('saves the name without deleting an existing logo implicitly', async () => {
    const logo = `/api/company/logo/${'a'.repeat(64)}.png`;
    call.mockResolvedValueOnce({ name: 'Acme', logo_url: logo })
      .mockResolvedValueOnce({ name: 'New company', logo_url: logo });
    renderWithProviders(<CompanySettings />);
    const name = await screen.findByLabelText('Company name');
    fireEvent.change(name, { target: { value: 'New company' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save company information' }));
    await waitFor(() => expect(call).toHaveBeenCalledWith('company.update', { name: 'New company' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Company information saved.');
    expect(useCompanyStore.getState().company.logo_url).toBe(logo);
    expect(screen.getByRole('img', { name: 'New company' })).toHaveAttribute('src', logo);
  });
  it('cannot submit a blank form when the initial read fails', async () => {
    call.mockRejectedValue(new Error('Unavailable'));
    renderWithProviders(<CompanySettings />);
    expect(await screen.findByRole('button', { name: 'Retry' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Save company information' })).toBeNull();
    expect(call).toHaveBeenCalledTimes(1);
  });
  it('uploads the raw file with the stored bearer token and never through WebSocket', async () => {
    const logo = `/api/company/logo/${'b'.repeat(64)}.png`;
    fetcher.mockResolvedValue({ ok: true, json: async () => ({ name: 'Acme', logo_url: logo }) });
    renderWithProviders(<CompanySettings />);
    const file = new File(['png-bytes'], 'logo.png', { type: 'image/png' });
    fireEvent.change(await screen.findByLabelText('Company logo'), { target: { files: [file] } });
    fireEvent.click(screen.getByRole('button', { name: 'Save company information' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Company information saved.');
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/api/company/logo', {
      method: 'POST', headers: { Authorization: 'Bearer access-token', 'Content-Type': 'image/png' },
      body: file, credentials: 'omit', redirect: 'error',
    });
    expect(call.mock.calls).toEqual([['company.get', {}], ['company.update', { name: 'Acme' }]]);
    expect(useCompanyStore.getState().company.logo_url).toBe(logo);
  });
  it('keeps an unsuccessful upload selected for retry and reflects the persisted name', async () => {
    const oldLogo = `/api/company/logo/${'a'.repeat(64)}.png`;
    const newLogo = `/api/company/logo/${'b'.repeat(64)}.png`;
    call.mockResolvedValueOnce({ name: 'Acme', logo_url: oldLogo })
      .mockResolvedValue({ name: 'New company', logo_url: oldLogo });
    fetcher.mockResolvedValueOnce({ ok: false, status: 429, json: async () => ({ error: 'Try later' }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ name: 'New company', logo_url: newLogo }) });
    renderWithProviders(<CompanySettings />);
    fireEvent.change(await screen.findByLabelText('Company name'), { target: { value: 'New company' } });
    const file = new File(['png-bytes'], 'logo.png', { type: 'image/png' });
    fireEvent.change(screen.getByLabelText('Company logo'), { target: { files: [file] } });
    fireEvent.click(screen.getByRole('button', { name: 'Save company information' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Try later');
    expect(screen.queryByRole('status')).toBeNull();
    expect(useCompanyStore.getState().company).toEqual({ name: 'New company', logo_url: oldLogo });
    expect(screen.getByRole('img', { name: 'New company' })).toHaveAttribute('src', oldLogo);
    fireEvent.click(screen.getByRole('button', { name: 'Save company information' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Company information saved.');
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(fetcher.mock.calls[1][1].body).toBe(file);
    expect(useCompanyStore.getState().company.logo_url).toBe(newLogo);
  });
  it('refuses an upload without an access token before changing any settings', async () => {
    useAuthStore.setState({ jwt: null });
    renderWithProviders(<CompanySettings />);
    fireEvent.change(await screen.findByLabelText('Company logo'), {
      target: { files: [new File(['png'], 'logo.png', { type: 'image/png' })] },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save company information' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('HTTP 401');
    expect(fetcher).not.toHaveBeenCalled();
    expect(call.mock.calls).toEqual([['company.get', {}]]);
  });
  it('does not upload if the name update is refused', async () => {
    call.mockResolvedValueOnce({ name: 'Acme', logo_url: null }).mockRejectedValue(new Error('Forbidden'));
    renderWithProviders(<CompanySettings />);
    fireEvent.change(await screen.findByLabelText('Company logo'), {
      target: { files: [new File(['png'], 'logo.png', { type: 'image/png' })] },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save company information' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Forbidden');
    expect(fetcher).not.toHaveBeenCalled();
    expect(screen.queryByRole('status')).toBeNull();
  });
  it('removes a logo through the name update without an HTTP upload', async () => {
    call.mockResolvedValueOnce({ name: 'Acme', logo_url: `/api/company/logo/${'a'.repeat(64)}.png` })
      .mockResolvedValueOnce({ name: 'Acme', logo_url: null });
    renderWithProviders(<CompanySettings />);
    fireEvent.click(await screen.findByRole('button', { name: 'Remove logo' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save company information' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Company information saved.');
    expect(call).toHaveBeenLastCalledWith('company.update', { name: 'Acme', remove_logo: true });
    expect(fetcher).not.toHaveBeenCalled();
    expect(useCompanyStore.getState().company.logo_url).toBeNull();
  });
});
