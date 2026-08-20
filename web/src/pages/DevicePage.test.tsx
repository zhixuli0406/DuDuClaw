import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { DevicePage } from './DevicePage';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';

const FULL_STATUS = {
  cpu_cores: 4,
  load_average: { load1: 0.5, load5: 0.4, load15: 0.3 },
  ram: { total_mb: 8192, available_mb: 4096, used_mb: 4096 },
  disk: { total_mb: 102400, used_mb: 51200, available_mb: 51200 },
  temperature_c: 42.5,
  uptime_secs: 90000,
  network_interfaces: [
    { name: 'eth0', is_up: true, addresses: ['192.168.1.10'] },
  ],
};

beforeEach(() => {
  vi.restoreAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never });
  useAuthStore.setState({ jwt: 'jwt-123' } as never);
  vi.spyOn(api.device, 'network').mockResolvedValue({ interfaces: FULL_STATUS.network_interfaces } as never);
  // WP-G1: the backup card unconditionally loads the schedule + backup list
  // on mount — give every test a quiet default so pre-existing assertions
  // (written before WP-G1) don't have to know about either.
  vi.spyOn(api.device, 'backupScheduleGet').mockResolvedValue({
    schedule_enabled: false,
    interval_hours: 24,
    retention_count: 7,
  });
  vi.spyOn(api.device, 'backupList').mockResolvedValue({ files: [] });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('<DevicePage> — appliance device management', () => {
  it('renders the status card with every field when all sensors are readable', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);

    renderWithProviders(<DevicePage />);

    expect(await screen.findByText('Device')).toBeInTheDocument();
    expect(screen.getByText('4')).toBeInTheDocument(); // cpu_cores
    expect(screen.getByText('42.5 °C')).toBeInTheDocument();
    expect(screen.getByText('1d 1h')).toBeInTheDocument(); // 90000s uptime
  });

  it('omits a status row whose sensor reading is null instead of showing a blank/zero', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue({
      cpu_cores: 2,
      load_average: null,
      ram: null,
      disk: null,
      temperature_c: null,
      uptime_secs: null,
      network_interfaces: [],
    } as never);

    renderWithProviders(<DevicePage />);

    expect(await screen.findByText('2')).toBeInTheDocument();
    expect(screen.queryByText('Temperature')).not.toBeInTheDocument();
    expect(screen.queryByText('Uptime')).not.toBeInTheDocument();
    expect(screen.queryByText('Memory')).not.toBeInTheDocument();
  });

  it('shows a plain-language refusal instead of crashing on a non-appliance install', async () => {
    vi.spyOn(api.device, 'status').mockRejectedValue({
      code: 'not_appliance',
      message: '此功能僅限 DuDuClaw 裝置版（appliance image）使用。',
    });

    renderWithProviders(<DevicePage />);

    expect(await screen.findByText('This page is only for the DuDuClaw appliance')).toBeInTheDocument();
    // The rest of the page (update/network/backup/danger cards) never renders.
    expect(screen.queryByText('Danger zone')).not.toBeInTheDocument();
  });

  it('checking for updates calls device.update_status and surfaces the raw log behind a disclosure', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const updateStatus = vi
      .spyOn(api.device, 'updateStatus')
      .mockResolvedValue({ success: true, stdout: '{"available":false}', stderr: '' });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Check for updates' }));

    await waitFor(() => expect(updateStatus).toHaveBeenCalledTimes(1));
    // The raw log auto-opens right after a check/apply — no extra click needed.
    expect(await screen.findByText('{"available":false}')).toBeInTheDocument();
    // It can still be collapsed again.
    await user.click(screen.getByRole('button', { name: 'Hide details' }));
    expect(screen.queryByText('{"available":false}')).not.toBeInTheDocument();
  });

  it('the "roll back to previous version" button stays disabled — the RPC always answers unsupported', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const rollback = vi.spyOn(api.device, 'updateRollback');

    renderWithProviders(<DevicePage />);

    const btn = await screen.findByRole('button', { name: 'Roll back to previous version' });
    expect(btn).toBeDisabled();
    expect(rollback).not.toHaveBeenCalled();
  });

  it('creates a backup and triggers the download once device.backup_create resolves', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const backupCreate = vi
      .spyOn(api.device, 'backupCreate')
      .mockResolvedValue({ filename: 'device-backup-20260818.tar.gz', stdout: '', stderr: '' });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Create backup' }));

    await waitFor(() => expect(backupCreate).toHaveBeenCalledTimes(1));
  });

  it('restart requires an explicit confirm before calling device.power', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const power = vi.spyOn(api.device, 'power').mockResolvedValue({ success: true, stdout: '', stderr: '' });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Restart' }));
    expect(power).not.toHaveBeenCalled();

    // The confirm dialog renders through a portal — query document-scoped.
    await user.click(await screen.findByRole('button', { name: 'Restart', hidden: false }));
    // Two "Restart" buttons now exist (danger row + dialog confirm) — click
    // the dialog's, which is the one inside the alertdialog/dialog role.
    const dialogButtons = screen.getAllByRole('button', { name: 'Restart' });
    await user.click(dialogButtons[dialogButtons.length - 1]);

    await waitFor(() => expect(power).toHaveBeenCalledWith('restart'));
  });

  it('factory reset stays blocked until the user types the exact confirm word', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const factoryReset = vi.spyOn(api.device, 'factoryReset').mockResolvedValue({
      success: true,
      stdout: '',
      stderr: '',
    });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Factory reset' }));

    const dialogButtons = screen.getAllByRole('button', { name: 'Factory reset' });
    const confirmButton = dialogButtons[dialogButtons.length - 1];
    expect(confirmButton).toBeDisabled();

    await user.type(screen.getByPlaceholderText('RESET'), 'RESET');
    expect(confirmButton).toBeEnabled();

    await user.click(confirmButton);
    await waitFor(() => expect(factoryReset).toHaveBeenCalledTimes(1));
  });
});

describe('<DevicePage> — WP-G1 scheduled backups + device migration restore', () => {
  it('loads the schedule settings and saves an edited draft', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const setSchedule = vi
      .spyOn(api.device, 'backupScheduleSet')
      .mockResolvedValue({ schedule_enabled: true, interval_hours: 12, retention_count: 5 });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    // beforeEach's default: off, 24h, keep 7.
    const toggle = await screen.findByRole('switch', { name: 'Scheduled backups' });
    expect(toggle).not.toBeChecked();

    await user.click(toggle);
    const interval = screen.getByLabelText('Every (hours)');
    await user.clear(interval);
    await user.type(interval, '12');
    const retention = screen.getByLabelText('Keep how many copies');
    await user.clear(retention);
    await user.type(retention, '5');

    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() =>
      expect(setSchedule).toHaveBeenCalledWith({
        schedule_enabled: true,
        interval_hours: 12,
        retention_count: 5,
      }),
    );
  });

  it('the save button stays disabled until a field is actually changed', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    renderWithProviders(<DevicePage />);

    expect(await screen.findByRole('switch', { name: 'Scheduled backups' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
  });

  it('lists stored backups and deletes one after confirmation', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const list = vi.spyOn(api.device, 'backupList').mockResolvedValue({
      files: [{ name: 'device-backup-20260101T000000Z.tar.gz', size: 2 * 1024 * 1024, mtime: Date.now() }],
    });
    const del = vi.spyOn(api.device, 'backupDelete').mockResolvedValue({ deleted: true });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    expect(await screen.findByText('device-backup-20260101T000000Z.tar.gz')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Delete' }));
    const dialogButtons = await screen.findAllByRole('button', { name: 'Delete' });
    await user.click(dialogButtons[dialogButtons.length - 1]);

    await waitFor(() => expect(del).toHaveBeenCalledWith('device-backup-20260101T000000Z.tar.gz'));
    await waitFor(() => expect(list).toHaveBeenCalledTimes(2)); // initial load + post-delete refresh
  });

  it('shows an empty state when there are no backups yet', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    renderWithProviders(<DevicePage />);
    expect(await screen.findByText('No backups yet')).toBeInTheDocument();
  });

  it('rejects a picked file that is not .tar.gz/.tgz before ever uploading', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    const input = screen.getByLabelText('Choose backup file (.tar.gz)');
    const file = new File(['dummy'], 'not-a-backup.zip', { type: 'application/zip' });
    await user.upload(input, file);

    expect(screen.queryByText('Import this backup?')).not.toBeInTheDocument();
  });

  it('imports a backup from an old device: upload → confirm → stage → restart prompt', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const restore = vi
      .spyOn(api.device, 'backupRestore')
      .mockResolvedValue({ staged: true, files_written: 12, restart_required: true });
    const power = vi.spyOn(api.device, 'power').mockResolvedValue({ success: true, stdout: '', stderr: '' });

    // `uploadBackup` drives a raw XHR (fetch has no upload-progress event) —
    // stub it with a minimal fake that succeeds synchronously on `send()`.
    class FakeXhr {
      upload: { onprogress: ((ev: ProgressEvent) => void) | null } = { onprogress: null };
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      status = 200;
      responseText = JSON.stringify({ path: '/home/x/tmp/backup-uploads/abc-old-device.tar.gz' });
      open = vi.fn();
      setRequestHeader = vi.fn();
      send = vi.fn(() => {
        this.onload?.();
      });
    }
    vi.stubGlobal('XMLHttpRequest', FakeXhr as unknown as typeof XMLHttpRequest);

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    const input = screen.getByLabelText('Choose backup file (.tar.gz)');
    const file = new File(['dummy'], 'old-device.tar.gz', { type: 'application/gzip' });
    await user.upload(input, file);

    // Upload finished ⇒ the explicit confirm modal appears (nothing
    // destructive has happened server-side yet).
    expect(await screen.findByText('Import this backup?')).toBeInTheDocument();
    expect(screen.getByText(/old-device\.tar\.gz/)).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Import and replace' }));

    await waitFor(() =>
      expect(restore).toHaveBeenCalledWith('/home/x/tmp/backup-uploads/abc-old-device.tar.gz'),
    );

    // Staged ⇒ restart prompt, restart button wires to the existing
    // device.power('restart') RPC.
    expect(await screen.findByText('Import ready — restart to finish')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Restart' }));
    await waitFor(() => expect(power).toHaveBeenCalledWith('restart'));
  });
});
