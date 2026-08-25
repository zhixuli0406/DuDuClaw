import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { DevicePage } from './DevicePage';
import { api } from '@/lib/api';
import { toast } from '@/lib/toast';
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

  it('checking for updates calls device.update_status and surfaces the classified log behind a disclosure', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const updateStatus = vi
      .spyOn(api.device, 'updateStatus')
      .mockResolvedValue({ success: true, stdout: '{"available":false}', stderr: '' });
    // H3d §11.5 item 1: `runUpdateCheck` now ALSO calls `device.update_check`
    // (the real-source answer) in parallel — stub it so this test exercises
    // only the raw-log-disclosure behavior it was written for.
    vi.spyOn(api.device, 'updateCheck').mockResolvedValue({
      available: false,
      current_version: '0.1.0',
      latest_version: '0.1.0',
    });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Check for updates' }));

    await waitFor(() => expect(updateStatus).toHaveBeenCalledTimes(1));
    // The disclosure now renders `formatDeviceOpDetail(updateLog)`
    // (no-linux-surface item 7), not raw `stdout`/`stderr` — this short,
    // secret-free, single-line JSON blob happens to sanitize to itself, so
    // the visible text is unchanged; `a_long_or_sensitive_dump_never_shows_up_verbatim`
    // and `a_failed_operation_leads_with_a_classified_human_clause` below
    // are what actually exercise the masking/classification behavior.
    expect(await screen.findByText('{"available":false}')).toBeInTheDocument();
    // It can still be collapsed again.
    await user.click(screen.getByRole('button', { name: 'Hide details' }));
    expect(screen.queryByText('{"available":false}')).not.toBeInTheDocument();
  });

  it('a long/multi-line stdout dump never shows up verbatim in the details panel (no-linux-surface item 7)', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const longFirstLine = `Transferring, please wait… ${'x'.repeat(200)}`;
    vi.spyOn(api.device, 'updateStatus').mockResolvedValue({
      success: true,
      stdout: `${longFirstLine}\nsecond raw journal line should never reach the screen`,
      stderr: '',
    });
    vi.spyOn(api.device, 'updateCheck').mockResolvedValue({
      available: false,
      current_version: '0.1.0',
      latest_version: '0.1.0',
    });

    const { container } = renderWithProviders(<DevicePage />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Check for updates' }));

    await screen.findByRole('button', { name: 'Hide details' });
    const pre = container.querySelector('pre');
    expect(pre).not.toBeNull();
    // Collapsed to one classified/capped line, not the raw multi-line dump:
    // the second line is dropped entirely and the first is length-capped.
    expect(pre!.textContent).not.toContain('second raw journal line');
    expect(pre!.textContent!.length).toBeLessThan(longFirstLine.length);
  });

  it('a failed operation leads with a classified human clause instead of the bare backend string (no-linux-surface item 7)', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    vi.spyOn(api.device, 'updateStatus').mockResolvedValue({ success: true, stdout: '', stderr: '' });
    vi.spyOn(api.device, 'updateCheck').mockResolvedValue({
      available: true,
      current_version: '0.1.0',
      latest_version: '0.2.0',
    });
    // The exact shape `handlers.rs`'s own test fixture uses for a real
    // systemd D-Bus refusal (see DRAFT-no-linux-surface-2026-08.md item 7).
    vi.spyOn(api.device, 'updateApply').mockResolvedValue({
      success: false,
      stdout: '',
      stderr: 'Call to Reboot failed: Access denied',
    });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Check for updates' }));
    await user.click(await screen.findByRole('button', { name: 'Update now' }));

    // A classified zh/en clause leads ("no permission…"); the raw D-Bus
    // string only ever appears as the bounded, secondary detail behind it —
    // never as the sole content of the details panel.
    expect(
      await screen.findByText('no permission, or the session expired (Call to Reboot failed: Access denied)'),
    ).toBeInTheDocument();
  });

  it('device.update_check reports a real newer version from the configured source', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    vi.spyOn(api.device, 'updateStatus').mockResolvedValue({ success: true, stdout: '', stderr: '' });
    const updateCheck = vi.spyOn(api.device, 'updateCheck').mockResolvedValue({
      available: true,
      current_version: '0.1.0',
      latest_version: '0.2.0',
    });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Check for updates' }));

    await waitFor(() => expect(updateCheck).toHaveBeenCalledTimes(1));
    expect(await screen.findByText('Update available')).toBeInTheDocument();
    expect(screen.getByText('Current 0.1.0 → latest 0.2.0')).toBeInTheDocument();
  });

  it('device.update_check honestly refuses instead of claiming "up to date" when no source is configured', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    vi.spyOn(api.device, 'updateStatus').mockResolvedValue({ success: true, stdout: '', stderr: '' });
    vi.spyOn(api.device, 'updateCheck').mockRejectedValue({
      code: 'not_configured',
      message: '尚未設定更新來源，請先在設定中填入更新來源網址。',
    });

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Check for updates' }));

    expect(
      await screen.findByText('No update source is configured yet — set one in Settings first.'),
    ).toBeInTheDocument();
    // Must never render an "up to date" badge on a failed check.
    expect(screen.queryByText('Up to date')).not.toBeInTheDocument();
    expect(screen.queryByText('Update available')).not.toBeInTheDocument();
  });

  it('the "roll back to previous version" button opens a confirm dialog, then calls device.update_rollback on confirm', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const rollback = vi
      .spyOn(api.device, 'updateRollback')
      .mockResolvedValue({ success: true, stdout: '', stderr: '' });
    const successToast = vi.spyOn(toast, 'success');

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    const btn = await screen.findByRole('button', { name: 'Roll back to previous version' });
    expect(btn).toBeEnabled();
    await user.click(btn);
    // Nothing destructive happens until the dialog is explicitly confirmed.
    expect(rollback).not.toHaveBeenCalled();
    expect(await screen.findByText('Roll back to the previous version?')).toBeInTheDocument();

    // The confirm dialog renders through a portal — query document-scoped.
    // Trigger button + dialog confirm button share the same label (same
    // convention as the "restart" test above), so click the last one.
    const dialogButtons = screen.getAllByRole('button', { name: 'Roll back to previous version' });
    await user.click(dialogButtons[dialogButtons.length - 1]);

    await waitFor(() => expect(rollback).toHaveBeenCalledTimes(1));
    // The gateway is about to reboot the box — the toast says "scheduled",
    // never "done".
    await waitFor(() =>
      expect(successToast).toHaveBeenCalledWith('Rollback scheduled — the device is restarting…'),
    );
    // Dialog closes once the rollback is scheduled.
    expect(screen.queryByText('Roll back to the previous version?')).not.toBeInTheDocument();
  });

  it('shows the backend\'s own reason verbatim when device.update_rollback answers unsupported — not a generic error', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    const backendMessage = '這台機器的自動回退機制未啟用。';
    vi.spyOn(api.device, 'updateRollback').mockRejectedValue({ code: 'unsupported', message: backendMessage });
    const infoToast = vi.spyOn(toast, 'info');
    const errorToast = vi.spyOn(toast, 'error');

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Roll back to previous version' }));
    const dialogButtons = screen.getAllByRole('button', { name: 'Roll back to previous version' });
    await user.click(dialogButtons[dialogButtons.length - 1]);

    // An honest refusal is not a bug — it renders via the "info" toast with
    // the gateway's own zh-TW reason passed through untouched, never
    // replaced by a generic classified error clause.
    await waitFor(() => expect(infoToast).toHaveBeenCalledWith(backendMessage));
    expect(errorToast).not.toHaveBeenCalled();
  });

  it('surfaces a generic failure toast when device.update_rollback rejects for a reason other than unsupported', async () => {
    vi.spyOn(api.device, 'status').mockResolvedValue(FULL_STATUS as never);
    vi.spyOn(api.device, 'updateRollback').mockRejectedValue(new Error('network error'));
    const errorToast = vi.spyOn(toast, 'error');
    const infoToast = vi.spyOn(toast, 'info');

    renderWithProviders(<DevicePage />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Roll back to previous version' }));
    const dialogButtons = screen.getAllByRole('button', { name: 'Roll back to previous version' });
    await user.click(dialogButtons[dialogButtons.length - 1]);

    await waitFor(() => expect(errorToast).toHaveBeenCalledTimes(1));
    expect(infoToast).not.toHaveBeenCalled();
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
    await waitFor(() => expect(factoryReset).toHaveBeenCalledWith(false));
  });

  it('factory reset defaults to keeping saved Wi-Fi credentials, and honors the opt-in checkbox', async () => {
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
    await user.type(screen.getByPlaceholderText('RESET'), 'RESET');

    // D4a-8: opt into "also clear saved Wi-Fi credentials" before confirming.
    await user.click(screen.getByRole('checkbox'));
    await user.click(confirmButton);

    await waitFor(() => expect(factoryReset).toHaveBeenCalledWith(true));
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
