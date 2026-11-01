import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { MfaEnrollment, RecoveryCodes } from './MfaEnrollment';
import { useAuthStore } from '@/stores/auth-store';

const secret = 'JBSWY3DPEHPK3PXP';
const recovery = Array.from({ length: 10 }, (_, i) => `recovery-${i}`);
beforeEach(() => { vi.restoreAllMocks(); sessionStorage.clear(); localStorage.clear(); });

function fill(label: string, value: string) { fireEvent.change(screen.getByLabelText(label), { target: { value } }); }

describe('MFA enrollment', () => {
  it('asks for password, handles independent proof, renders QR/manual secret and shows codes once', async () => {
    const saved = vi.fn();
    const begin = vi.fn().mockResolvedValueOnce({ enrollment_token: 'step-up', challenge_ref: 'reference', proof_required: true })
      .mockResolvedValueOnce({ enrollment_token: 'step-up', secret, otpauth_uri: `otpauth://totp/test?secret=${secret}` });
    const confirm = vi.fn().mockResolvedValue({ recovery_codes: recovery });
    const local = vi.spyOn(Storage.prototype, 'setItem');
    const { unmount } = renderWithProviders(<MfaEnrollment begin={begin} confirm={confirm} onSaved={saved} onCancel={vi.fn()} />);
    fill('Current password', 'current-pass');
    fireEvent.click(screen.getByRole('button', { name: 'Continue setup' }));
    expect(await screen.findByText('reference', { exact: true })).toBeInTheDocument();
    fill('Channel or Admin enrollment code', 'admin-proof');
    fireEvent.click(screen.getByRole('button', { name: 'Continue setup' }));
    expect(await screen.findByText(secret)).toBeInTheDocument();
    expect(screen.getByRole('img', { name: 'Authenticator setup QR code' }).querySelector('svg')).toBeTruthy();
    expect(begin).toHaveBeenLastCalledWith({ password: 'current-pass', enrollment_token: 'step-up', proof_code: 'admin-proof' });
    fill('Authenticator code', '123456');
    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(await screen.findByLabelText('Recovery codes')).toHaveTextContent(recovery.join(' '));
    expect(confirm).toHaveBeenCalledWith({ password: 'current-pass', enrollment_token: 'step-up', proof_code: 'admin-proof', code: '123456' });
    expect(screen.queryByText(secret)).toBeNull();
    expect(screen.getByRole('button', { name: 'Continue' })).toBeDisabled();
    fireEvent.click(screen.getByLabelText('I saved my recovery codes'));
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await waitFor(() => expect(saved).toHaveBeenCalledOnce());
    expect(local).not.toHaveBeenCalled();
    expect(JSON.stringify(useAuthStore.getState())).not.toContain(secret);
    expect(JSON.stringify(useAuthStore.getState())).not.toContain('recovery-');
    unmount();
    renderWithProviders(<MfaEnrollment begin={begin} confirm={confirm} onSaved={saved} onCancel={vi.fn()} />);
    expect(screen.queryByLabelText('Recovery codes')).toBeNull();
    expect(screen.queryByText(secret)).toBeNull();
    expect(screen.getByLabelText('Current password')).toHaveValue('');
  });

  it('keeps recovery codes available if final session establishment fails', async () => {
    const saved = vi.fn().mockRejectedValue(new Error('network'));
    renderWithProviders(<RecoveryCodes codes={recovery} onSaved={saved} />);
    fireEvent.click(screen.getByLabelText('I saved my recovery codes'));
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not complete');
    expect(screen.getByLabelText('Recovery codes')).toBeInTheDocument();
  });

  it('copies and downloads only on explicit user actions', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    const create = vi.fn().mockReturnValue('blob:codes');
    const revoke = vi.fn();
    Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: create });
    Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: revoke });
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
    renderWithProviders(<RecoveryCodes codes={recovery} onSaved={vi.fn()} />);
    expect(writeText).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Copy codes' }));
    await screen.findByText('Codes copied.');
    expect(writeText).toHaveBeenCalledWith(recovery.join('\n'));
    fireEvent.click(screen.getByRole('button', { name: 'Download codes' }));
    expect(click).toHaveBeenCalledOnce(); expect(revoke).toHaveBeenCalledWith('blob:codes');
  });
});

it('handles unavailable clipboard without losing recovery codes', async () => {
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined });
  renderWithProviders(<RecoveryCodes codes={recovery} onSaved={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Copy codes' }));
  expect(await screen.findByText(/Could not copy/)).toBeInTheDocument();
  expect(screen.getByLabelText('Recovery codes')).toBeInTheDocument();
});
