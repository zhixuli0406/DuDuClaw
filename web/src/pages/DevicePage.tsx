import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import {
  HardDrive,
  Cpu,
  Gauge,
  MemoryStick,
  Thermometer,
  Timer,
  Wifi,
  Activity,
  Download,
  Archive,
  AlertTriangle,
  RefreshCw,
  RotateCcw,
  Power,
  Trash2,
  UploadCloud,
} from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import {
  api,
  DEVICE_NOT_APPLIANCE_ERROR_CODE,
  type DeviceStatus,
  type DeviceNetworkInterface,
  type DeviceOpResult,
  type DeviceUpdateCheckResult,
  type DeviceBackupScheduleConfig,
  type DeviceBackupFileEntry,
} from '@/lib/api';
import { toast, formatError, formatDeviceOpDetail } from '@/lib/toast';
import { timeAgo } from '@/lib/format';
import { cn } from '@/lib/utils';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
  ErrorState,
  Skeleton,
  Switch,
  Checkbox,
  Input,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/mds';
import { DangerZone, ConfirmDialog } from '@/components/settings/controls';
import { MaintenanceModeCard } from '@/components/settings/MaintenanceModeCard';
import { useMaintenanceStore, showDetailsUnlocked } from '@/stores/maintenance-store';

/**
 * DevicePage (WP-C, 2026-08) — "裝置": the appliance hardware-management
 * console. Every RPC behind this page (`device.*`) is admin + appliance-only
 * server-side (`require_admin!()` + `require_appliance!()` in
 * `handlers.rs`) — the nav entry additionally hides itself on every
 * non-appliance install via `useIsAppliance` (progressive disclosure), but
 * the route itself stays reachable by URL (same convention as `/forks`/
 * `/org`), so this page still renders a plain-language refusal rather than
 * crashing when reached on a non-appliance install.
 *
 * User-facing copy deliberately avoids internal terms ("A/B 槽",
 * "sysupdate", "systemd-bless-boot", "RPC") — 更新中心 says "系統更新", the
 * rollback button says "回到上一版", and its confirm dialog only ever
 * promises "重新開機＋切回上一版＋資料不受影響".
 *
 * `device.update_rollback` is now backed by a real appliance action (see the
 * gateway's own doc comment on `SystemDeviceOps::update_rollback`) — the
 * button is no longer disabled. Its response still needs THREE-way
 * handling, not a plain success/failure binary:
 *   1. Resolves `{ success: true }` → the box is about to reboot into the
 *      previous slot. The toast says "已排定" (scheduled), never "完成"
 *      (done) — the WebSocket connection is about to drop, so this tab will
 *      never observe a real completion event.
 *   2. Rejects with `error.code === 'unsupported'` → an HONEST refusal, not
 *      a bug (e.g. this box's A/B rollback mechanism isn't enabled, or
 *      there is no previous slot to fall back to). The gateway already
 *      writes a specific zh-TW reason into `error.message` — this UI must
 *      surface that text verbatim rather than overwrite it with a generic
 *      clause.
 *   3. Anything else (network / timeout / other RPC error) → the ordinary
 *      `formatError()` toast.
 */

// ── Small structured-error helper (RPC rejects with `{ code, message }`) ──

function errorCode(err: unknown): string | undefined {
  if (err && typeof err === 'object' && 'code' in err) {
    const c = (err as { code?: unknown }).code;
    return typeof c === 'string' ? c : undefined;
  }
  return undefined;
}

/** Extract `.message` from a `{ code, message }` RPC rejection — the
 *  companion to `errorCode` above. Used only for the `unsupported` rollback
 *  refusal, where the gateway's own zh-TW reason must reach the user
 *  verbatim instead of being replaced by a generic classified clause. */
function errorMessage(err: unknown): string | undefined {
  if (err && typeof err === 'object' && 'message' in err) {
    const m = (err as { message?: unknown }).message;
    return typeof m === 'string' ? m : undefined;
  }
  return undefined;
}

const STATUS_POLL_MS = 10_000;

/** MB → a compact "1.2 GB" / "512 MB" token. Locale-neutral, matches the
 *  rest of the dashboard's machine-value formatters (`lib/format.ts`). */
function formatMb(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  return `${Math.round(mb)} MB`;
}

/** Whole-seconds uptime → "3d 4h" / "2h 15m" / "45m". */
function formatUptime(secs: number): string {
  const days = Math.floor(secs / 86400);
  const hours = Math.floor((secs % 86400) / 3600);
  const mins = Math.floor((secs % 3600) / 60);
  if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  return `${mins}m`;
}

// ── Report-panel shell (mirrors OSPage's local `Panel`) ────────────────────

function Panel({
  icon: Icon,
  title,
  description,
  action,
  children,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-3 space-y-0">
        <div className="min-w-0 space-y-1">
          <CardTitle className="flex items-center gap-2 text-sm font-medium">
            <Icon className="size-4 text-muted-foreground" />
            {title}
          </CardTitle>
          {description && <CardDescription>{description}</CardDescription>}
        </div>
        {action}
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

/** One label/value tile in the status grid. Omits itself (via the caller
 *  never rendering it) when the underlying reading is `null` — a missing
 *  sensor is an honest gap, not a zero. */
function StatTile({
  icon: Icon,
  label,
  value,
}: {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: ReactNode;
}) {
  return (
    <div className="flex items-start gap-3 rounded-lg border border-surface-border px-3 py-2.5">
      <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 space-y-0.5">
        <p className="text-xs text-muted-foreground">{label}</p>
        <p className="text-sm font-medium tabular-nums text-foreground">{value}</p>
      </div>
    </div>
  );
}

/** Compact usage bar shared by the RAM/磁碟 tiles. */
function UsageBar({ usedMb, totalMb }: { usedMb: number; totalMb: number }) {
  const pct = totalMb > 0 ? Math.min(100, Math.round((usedMb / totalMb) * 100)) : 0;
  return (
    <div className="mt-1.5 h-1.5 w-full overflow-hidden rounded-full bg-muted">
      <div className="h-full rounded-full bg-brand transition-all duration-700" style={{ width: `${pct}%` }} />
    </div>
  );
}

// ── Danger-zone action identity ────────────────────────────────────────────

type DangerAction = 'factoryReset' | 'restart' | 'shutdown';

export function DevicePage() {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );
  const jwt = useAuthStore((s) => s.jwt);
  const connectionState = useConnectionStore((s) => s.state);

  // ── Status card: 10s polling, silent after the first load (no skeleton
  //    flash, no layout shift on refresh — the same field set renders every
  //    tick since the hardware doesn't gain/lose sensors at runtime). ──
  const [status, setStatus] = useState<DeviceStatus | null>(null);
  const [statusLoading, setStatusLoading] = useState(true);
  const [statusError, setStatusError] = useState<unknown>(null);

  const fetchStatus = useCallback(async (silent = false) => {
    if (!silent) setStatusLoading(true);
    try {
      const res = await api.device.status();
      setStatus(res);
      setStatusError(null);
    } catch (e) {
      console.warn('[device.status]', e);
      setStatusError(e);
    } finally {
      setStatusLoading(false);
    }
  }, []);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchStatus();
    const id = setInterval(() => fetchStatus(true), STATUS_POLL_MS);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionState]);

  const notAppliance = errorCode(statusError) === DEVICE_NOT_APPLIANCE_ERROR_CODE;

  // ── Network card (dedicated `device.network` read — the detailed twin of
  //    the status card's compact interface summary). ──
  const [interfaces, setInterfaces] = useState<DeviceNetworkInterface[]>([]);
  const [netLoading, setNetLoading] = useState(true);
  const [netError, setNetError] = useState<unknown>(null);

  const fetchNetwork = useCallback(async () => {
    setNetLoading(true);
    try {
      const res = await api.device.network();
      setInterfaces(res.interfaces ?? []);
      setNetError(null);
    } catch (e) {
      console.warn('[device.network]', e);
      setNetError(e);
    } finally {
      setNetLoading(false);
    }
  }, []);

  useEffect(() => {
    if (connectionState !== 'authenticated' || notAppliance) return;
    fetchNetwork();
  }, [connectionState, notAppliance, fetchNetwork]);

  // ── Maintenance Mode — Entry A: drives the "顯示詳情" full-disclosure
  // bypass below (§2.6). Store is shared/live (see `maintenance-store.ts`),
  // so this page doesn't need its own fetch effect — `MaintenanceModeCard`
  // (rendered further down) owns the initial fetch + 30s poll.
  const maintenanceStatus = useMaintenanceStore((s) => s.status);
  const maintenanceShowDetailsUnlocked = showDetailsUnlocked(maintenanceStatus);

  // ── Update center ──
  const [checking, setChecking] = useState(false);
  const [applying, setApplying] = useState(false);
  const [updateLog, setUpdateLog] = useState<DeviceOpResult | null>(null);
  const [showUpdateLog, setShowUpdateLog] = useState(false);
  const [rollbackConfirmOpen, setRollbackConfirmOpen] = useState(false);
  const [rollingBack, setRollingBack] = useState(false);
  // H3d §11.5 item 1: `device.update_check`'s answer — the REAL update
  // source, not `device.update_status`'s local-staging-only view (which
  // still feeds `updateLog`/`showUpdateLog` below for the raw detail
  // toggle). `checkError` carries the gateway's own zh-TW reason
  // (`not_configured` renders its own copy; everything else falls back to
  // `formatError`) rather than a generic failure clause.
  const [checkResult, setCheckResult] = useState<DeviceUpdateCheckResult | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);

  const runUpdateCheck = async () => {
    setChecking(true);
    try {
      const [statusOutcome, checkOutcome] = await Promise.allSettled([
        api.device.updateStatus(),
        api.device.updateCheck(),
      ]);
      if (statusOutcome.status === 'fulfilled') {
        setUpdateLog(statusOutcome.value);
        setShowUpdateLog(true);
      } else {
        console.warn('[device.update_status]', statusOutcome.reason);
      }

      // `fulfilled` alone is not enough to trust — a malformed/empty
      // response (e.g. the test double's default `null`) must fall through
      // to the honest-failure branch below rather than dereferencing a
      // field that isn't there.
      if (checkOutcome.status === 'fulfilled' && checkOutcome.value) {
        const result = checkOutcome.value;
        setCheckResult(result);
        setCheckError(null);
        toast.success(
          result.available
            ? t('device.update.checkAvailable', { version: result.latest_version })
            : t('device.update.checkUpToDate', { version: result.current_version }),
        );
      } else {
        const reason = checkOutcome.status === 'rejected' ? checkOutcome.reason : undefined;
        console.warn('[device.update_check]', reason ?? 'empty response');
        setCheckResult(null);
        const msg =
          errorCode(reason) === 'not_configured'
            ? t('device.update.check.notConfigured')
            : (errorMessage(reason) ?? t('device.update.check.failed'));
        setCheckError(msg);
        toast.error(msg);
      }
    } finally {
      setChecking(false);
    }
  };

  const runUpdateApply = async () => {
    setApplying(true);
    try {
      const res = await api.device.updateApply();
      setUpdateLog(res);
      setShowUpdateLog(true);
      toast[res.success ? 'success' : 'error'](
        t(res.success ? 'device.update.applyDone' : 'device.update.applyFailed'),
      );
    } catch (e) {
      console.warn('[device.update_apply]', e);
      toast.error(formatError(e));
    } finally {
      setApplying(false);
    }
  };

  // Three-way response handling — see the module doc comment above for why
  // this is deliberately not a plain success/failure binary.
  const runUpdateRollback = async () => {
    setRollingBack(true);
    try {
      const res = await api.device.updateRollback();
      setUpdateLog(res);
      setShowUpdateLog(true);
      if (res.success) {
        // The gateway is about to reboot the box — "scheduled", not "done".
        toast.success(t('device.update.rollback.scheduled'));
        setRollbackConfirmOpen(false);
      } else {
        toast.error(t('device.update.rollback.failed'));
      }
    } catch (e) {
      console.warn('[device.update_rollback]', e);
      if (errorCode(e) === 'unsupported') {
        // Honest refusal, not a bug — show the gateway's own reason
        // verbatim instead of a generic error clause.
        toast.info(errorMessage(e) ?? t('device.update.rollback.unsupportedFallback'));
      } else {
        toast.error(formatError(e));
      }
    } finally {
      setRollingBack(false);
    }
  };

  // ── Backup card ──
  const [backingUp, setBackingUp] = useState(false);

  const runBackup = async () => {
    setBackingUp(true);
    try {
      const res = await api.device.backupCreate();
      toast.success(t('device.backup.done'));
      const params = new URLSearchParams({ name: res.filename });
      if (jwt) params.set('token', jwt);
      const a = document.createElement('a');
      a.href = `/api/files/download?${params.toString()}`;
      a.download = res.filename;
      document.body.appendChild(a);
      a.click();
      a.remove();
    } catch (e) {
      console.warn('[device.backup_create]', e);
      toast.error(formatError(e));
    } finally {
      setBackingUp(false);
    }
  };

  // ── Scheduled backups (WP-G1) ──
  const [schedule, setSchedule] = useState<DeviceBackupScheduleConfig | null>(null);
  const [scheduleLoading, setScheduleLoading] = useState(true);
  const [scheduleSaving, setScheduleSaving] = useState(false);
  // Local draft fields — only committed to the gateway on "儲存".
  const [draftEnabled, setDraftEnabled] = useState(false);
  const [draftInterval, setDraftInterval] = useState('24');
  const [draftRetention, setDraftRetention] = useState('7');

  const fetchSchedule = useCallback(async () => {
    setScheduleLoading(true);
    try {
      const res = await api.device.backupScheduleGet();
      setSchedule(res);
      setDraftEnabled(res.schedule_enabled);
      setDraftInterval(String(res.interval_hours));
      setDraftRetention(String(res.retention_count));
    } catch (e) {
      console.warn('[device.backup_schedule_get]', e);
    } finally {
      setScheduleLoading(false);
    }
  }, []);

  const scheduleDirty =
    schedule != null &&
    (draftEnabled !== schedule.schedule_enabled ||
      draftInterval !== String(schedule.interval_hours) ||
      draftRetention !== String(schedule.retention_count));

  const saveSchedule = async () => {
    setScheduleSaving(true);
    try {
      const res = await api.device.backupScheduleSet({
        schedule_enabled: draftEnabled,
        interval_hours: Math.max(1, Number(draftInterval) || 24),
        retention_count: Math.max(1, Number(draftRetention) || 7),
      });
      setSchedule(res);
      setDraftInterval(String(res.interval_hours));
      setDraftRetention(String(res.retention_count));
      toast.success(t('device.backup.schedule.saved'));
    } catch (e) {
      console.warn('[device.backup_schedule_set]', e);
      toast.error(formatError(e));
    } finally {
      setScheduleSaving(false);
    }
  };

  // ── Backup list (WP-G1) ──
  const [backupFiles, setBackupFiles] = useState<DeviceBackupFileEntry[]>([]);
  const [backupListLoading, setBackupListLoading] = useState(true);
  const [backupListError, setBackupListError] = useState<unknown>(null);
  const [deletingName, setDeletingName] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<DeviceBackupFileEntry | null>(null);

  const fetchBackupList = useCallback(async () => {
    setBackupListLoading(true);
    try {
      const res = await api.device.backupList();
      setBackupFiles(res.files ?? []);
      setBackupListError(null);
    } catch (e) {
      console.warn('[device.backup_list]', e);
      setBackupListError(e);
    } finally {
      setBackupListLoading(false);
    }
  }, []);

  useEffect(() => {
    if (connectionState !== 'authenticated' || notAppliance) return;
    fetchSchedule();
    fetchBackupList();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionState, notAppliance]);

  const downloadBackup = (name: string) => {
    const params = new URLSearchParams({ name });
    if (jwt) params.set('token', jwt);
    const a = document.createElement('a');
    a.href = `/api/device/backups/download?${params.toString()}`;
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
  };

  const confirmDeleteBackup = async () => {
    if (!deleteTarget) return;
    setDeletingName(deleteTarget.name);
    try {
      await api.device.backupDelete(deleteTarget.name);
      toast.success(t('device.backup.list.deleted'));
      setDeleteTarget(null);
      await fetchBackupList();
    } catch (e) {
      console.warn('[device.backup_delete]', e);
      toast.error(formatError(e));
    } finally {
      setDeletingName(null);
    }
  };

  // ── "從舊機匯入" restore wizard (WP-G1) ──
  // Two steps: upload (progress via XHR — fetch has no upload progress),
  // then an explicit confirm modal before the actual `device.backup_restore`
  // call — nothing destructive happens until the confirm modal is accepted,
  // and even then the gateway only STAGES the restore; it takes effect on
  // the next restart.
  const importInputRef = useRef<HTMLInputElement | null>(null);
  const [uploading, setUploading] = useState(false);
  const [uploadPct, setUploadPct] = useState<number | null>(null);
  const [restoring, setRestoring] = useState(false);
  const [restoreCandidate, setRestoreCandidate] = useState<{ path: string; name: string } | null>(null);
  const [restoreDone, setRestoreDone] = useState(false);

  const uploadBackup = (file: File): Promise<string> =>
    new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      xhr.open('POST', '/api/device/backup-upload');
      if (jwt) xhr.setRequestHeader('Authorization', `Bearer ${jwt}`);
      xhr.upload.onprogress = (ev) => {
        if (ev.lengthComputable) setUploadPct(Math.round((ev.loaded / ev.total) * 100));
      };
      xhr.onload = () => {
        try {
          const body = JSON.parse(xhr.responseText || '{}');
          if (xhr.status >= 200 && xhr.status < 300 && typeof body.path === 'string') {
            resolve(body.path);
          } else {
            reject(new Error(typeof body.error === 'string' ? body.error : `HTTP ${xhr.status}`));
          }
        } catch {
          reject(new Error(`HTTP ${xhr.status}`));
        }
      };
      xhr.onerror = () => reject(new Error('network error'));
      const form = new FormData();
      form.append('file', file, file.name);
      xhr.send(form);
    });

  const handleImportFilePicked = async (file: File | null) => {
    if (!file) return;
    const lower = file.name.toLowerCase();
    if (!lower.endsWith('.tar.gz') && !lower.endsWith('.tgz')) {
      toast.error(t('device.restore.notTarGz'));
      return;
    }
    setUploading(true);
    setUploadPct(0);
    try {
      const path = await uploadBackup(file);
      setRestoreCandidate({ path, name: file.name });
    } catch (e) {
      console.warn('[device.backup-upload]', e);
      toast.error(intl.formatMessage({ id: 'device.restore.uploadFailed' }, { message: String(e instanceof Error ? e.message : e) }));
    } finally {
      setUploading(false);
      setUploadPct(null);
      if (importInputRef.current) importInputRef.current.value = '';
    }
  };

  const confirmRestore = async () => {
    if (!restoreCandidate) return;
    setRestoring(true);
    try {
      await api.device.backupRestore(restoreCandidate.path);
      setRestoreCandidate(null);
      setRestoreDone(true);
    } catch (e) {
      console.warn('[device.backup_restore]', e);
      toast.error(formatError(e));
    } finally {
      setRestoring(false);
    }
  };

  const runRestartAfterRestore = async () => {
    try {
      await api.device.power('restart');
      toast.success(t('device.danger.restart.done'));
      setRestoreDone(false);
    } catch (e) {
      console.warn('[device.power]', e);
      toast.error(formatError(e));
    }
  };

  // ── Danger zone: every action goes through a type-to-confirm or plain
  //    confirm modal — `confirm: true` is only ever sent once the user has
  //    gone through it, never inferred client-side ahead of time. ──
  //
  // `runDangerAction`/`runRestartAfterRestore` below are DESIGN.md §5.6's
  // reference implementation for error display: `toast.error(formatError(e))`
  // only, never a branch that renders `e.message` or a raw field. Any new
  // page's `catch` block should look like this one, not reinvent it.
  const [confirmAction, setConfirmAction] = useState<DangerAction | null>(null);
  const [dangerBusy, setDangerBusy] = useState(false);
  // D4a-8: "一併清除網路設定" — defaults to false (keep saved Wi-Fi
  // credentials) on every open, same "reset each time the dialog opens"
  // discipline ConfirmDialog's own `requireText` input follows.
  const [clearNetworkOnReset, setClearNetworkOnReset] = useState(false);

  const openConfirm = (action: DangerAction) => {
    setClearNetworkOnReset(false);
    setConfirmAction(action);
  };

  const runDangerAction = async () => {
    if (!confirmAction) return;
    setDangerBusy(true);
    try {
      if (confirmAction === 'factoryReset') {
        await api.device.factoryReset(clearNetworkOnReset);
      } else {
        await api.device.power(confirmAction === 'restart' ? 'restart' : 'shutdown');
      }
      toast.success(t(`device.danger.${confirmAction}.done`));
      setConfirmAction(null);
    } catch (e) {
      console.warn('[device.danger]', confirmAction, e);
      toast.error(formatError(e));
    } finally {
      setDangerBusy(false);
    }
  };

  const upInterfaces = status?.network_interfaces.filter((i) => i.is_up).length ?? 0;
  const totalInterfaces = status?.network_interfaces.length ?? 0;

  return (
    <div className="mx-auto w-full max-w-4xl space-y-6">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <HardDrive className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{t('nav.device')}</h1>
            <p className="text-sm text-muted-foreground">{t('device.subtitle')}</p>
          </div>
        </div>
        {!notAppliance && (
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => fetchStatus(true)}
            disabled={statusLoading}
            aria-label={t('common.refresh')}
            title={t('common.refresh')}
          >
            <RefreshCw className={cn(statusLoading && 'animate-spin')} />
          </Button>
        )}
      </div>

      {notAppliance ? (
        <ErrorState
          icon={HardDrive}
          title={t('device.notAppliance.title')}
          description={t('device.notAppliance.desc')}
        />
      ) : (
        <>
          {/* ① 狀態卡 */}
          <Panel icon={Activity} title={t('device.section.status')} description={t('device.section.status.desc')}>
            {statusLoading ? (
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                {Array.from({ length: 4 }).map((_, i) => (
                  <Skeleton key={i} className="h-14 w-full rounded-lg" />
                ))}
              </div>
            ) : statusError ? (
              <ErrorState variant="inline" icon={Activity} error={statusError} onRetry={() => fetchStatus()} />
            ) : status ? (
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <StatTile icon={Cpu} label={t('device.status.cpuCores')} value={status.cpu_cores} />
                {status.load_average && (
                  <StatTile
                    icon={Gauge}
                    label={t('device.status.load')}
                    value={`${status.load_average.load1.toFixed(2)} / ${status.load_average.load5.toFixed(2)} / ${status.load_average.load15.toFixed(2)}`}
                  />
                )}
                {status.ram && (
                  <StatTile
                    icon={MemoryStick}
                    label={t('device.status.ram')}
                    value={
                      <>
                        {t('device.status.usedOfTotal', {
                          used: formatMb(status.ram.used_mb),
                          total: formatMb(status.ram.total_mb),
                        })}
                        <UsageBar usedMb={status.ram.used_mb} totalMb={status.ram.total_mb} />
                      </>
                    }
                  />
                )}
                {status.disk && (
                  <StatTile
                    icon={HardDrive}
                    label={t('device.status.disk')}
                    value={
                      <>
                        {t('device.status.usedOfTotal', {
                          used: formatMb(status.disk.used_mb),
                          total: formatMb(status.disk.total_mb),
                        })}
                        <UsageBar usedMb={status.disk.used_mb} totalMb={status.disk.total_mb} />
                      </>
                    }
                  />
                )}
                {status.temperature_c != null && (
                  <StatTile
                    icon={Thermometer}
                    label={t('device.status.temperature')}
                    value={`${status.temperature_c.toFixed(1)} °C`}
                  />
                )}
                {status.uptime_secs != null && (
                  <StatTile icon={Timer} label={t('device.status.uptime')} value={formatUptime(status.uptime_secs)} />
                )}
                {totalInterfaces > 0 && (
                  <StatTile
                    icon={Wifi}
                    label={t('device.status.network')}
                    value={t('device.status.network.summary', { up: upInterfaces, total: totalInterfaces })}
                  />
                )}
              </div>
            ) : null}
          </Panel>

          {/* ② 更新中心 */}
          <Panel icon={Download} title={t('device.section.update')} description={t('device.section.update.desc')}>
            <div className="space-y-3">
              {/* H3d §11.5 item 1: the REAL update source's answer
                  (device.update_check), not the local-staging-only view
                  the raw detail toggle below still shows. Renders nothing
                  until a check has actually run. */}
              {checkResult && (
                <div className="flex items-center gap-2 rounded-lg border border-surface-border bg-muted/30 p-2.5">
                  <Badge
                    variant="secondary"
                    className={checkResult.available ? 'bg-warning/15 text-warning' : 'bg-success/15 text-success'}
                  >
                    {t(checkResult.available ? 'device.update.checkAvailableBadge' : 'device.update.checkUpToDateBadge')}
                  </Badge>
                  <span className="text-xs text-muted-foreground">
                    {t('device.update.checkVersions', {
                      current: checkResult.current_version,
                      latest: checkResult.latest_version,
                    })}
                  </span>
                </div>
              )}
              {checkError && !checkResult && (
                <p className="text-xs text-muted-foreground">{checkError}</p>
              )}
              <div className="flex flex-wrap gap-2">
                <Button variant="outline" size="sm" onClick={runUpdateCheck} disabled={checking || applying || rollingBack}>
                  <Download className={cn(checking && 'animate-pulse')} />
                  {checking ? t('device.update.checking') : t('device.update.check')}
                </Button>
                <Button variant="brand" size="sm" onClick={runUpdateApply} disabled={checking || applying || rollingBack}>
                  <RefreshCw className={cn(applying && 'animate-spin')} />
                  {applying ? t('device.update.applying') : t('device.update.apply')}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setRollbackConfirmOpen(true)}
                  disabled={checking || applying || rollingBack}
                >
                  <RotateCcw />
                  {t('device.update.rollback')}
                </Button>
              </div>

              {updateLog && (
                <div className="space-y-1.5">
                  <button
                    type="button"
                    onClick={() => {
                      const next = !showUpdateLog;
                      setShowUpdateLog(next);
                      // §2.3: every "顯示詳情" reveal under an unlocked
                      // maintenance window is its own audit line — fire on
                      // expand only (not on collapse), best-effort (a failed
                      // audit call must never block the UI toggle itself).
                      if (next && maintenanceShowDetailsUnlocked) {
                        void api.maintenance.logAccess('device.update_apply').catch((e) => {
                          console.warn('[maintenance] logAccess failed', e);
                        });
                      }
                    }}
                    className="text-xs font-medium text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
                  >
                    {showUpdateLog ? t('device.update.details.hide') : t('device.update.details')}
                  </button>
                  {showUpdateLog && (
                    <div className="space-y-1">
                      {maintenanceShowDetailsUnlocked && (
                        <Badge variant="secondary" className="bg-amber-500/15 text-amber-600 dark:text-amber-400">
                          {t('device.maintenance.infoLabel')}
                        </Badge>
                      )}
                      <pre className="max-h-48 overflow-auto rounded-lg border border-surface-border bg-muted/40 p-2.5 text-xs whitespace-pre-wrap text-muted-foreground">
                        {/* no-linux-surface item 7: classified+masked by
                            default, never the raw systemctl/systemd-sysupdate
                            stdout/stderr — UNLESS maintenance mode's
                            show_details sub-capability is unlocked, in which
                            case the full (still secret-redacted) text is
                            shown, per DESIGN-maintenance-mode-2026-08.md §2.6. */}
                        {formatDeviceOpDetail(updateLog, maintenanceShowDetailsUnlocked) || '—'}
                      </pre>
                    </div>
                  )}
                </div>
              )}
            </div>
          </Panel>

          {/* ③ 網路卡 */}
          <Panel icon={Wifi} title={t('device.section.network')} description={t('device.section.network.desc')}>
            {netLoading ? (
              <div className="space-y-2">
                <Skeleton className="h-9 w-full" />
                <Skeleton className="h-9 w-full" />
              </div>
            ) : netError ? (
              <ErrorState variant="inline" icon={Wifi} error={netError} onRetry={fetchNetwork} />
            ) : interfaces.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('device.network.empty')}</p>
            ) : (
              <div className="space-y-3">
                <div className="overflow-x-auto">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>{t('device.network.col.name')}</TableHead>
                        <TableHead>{t('device.network.col.status')}</TableHead>
                        <TableHead>{t('device.network.col.addresses')}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {interfaces.map((iface) => (
                        <TableRow key={iface.name}>
                          <TableCell className="font-mono text-xs text-foreground">{iface.name}</TableCell>
                          <TableCell>
                            <Badge
                              variant="secondary"
                              className={iface.is_up ? 'bg-success/15 text-success' : undefined}
                            >
                              {t(iface.is_up ? 'device.network.status.up' : 'device.network.status.down')}
                            </Badge>
                          </TableCell>
                          <TableCell className="font-mono text-xs text-muted-foreground">
                            {iface.addresses.length > 0 ? iface.addresses.join(', ') : '—'}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
                <div className="flex items-center justify-between gap-3 rounded-lg border border-surface-border px-3 py-2">
                  <span className="text-sm text-foreground">{t('device.network.staticIp')}</span>
                  <span className="text-xs text-muted-foreground">{t('device.network.staticIp.comingSoon')}</span>
                </div>
              </div>
            )}
          </Panel>

          {/* ④ 備份卡 */}
          <Panel icon={Archive} title={t('device.section.backup')} description={t('device.section.backup.desc')}>
            <div className="space-y-5">
              <div className="space-y-2">
                <Button variant="brand" size="sm" onClick={runBackup} disabled={backingUp}>
                  <Archive className={cn(backingUp && 'animate-pulse')} />
                  {backingUp ? t('device.backup.creating') : t('device.backup.create')}
                </Button>
                {backingUp && <p className="text-xs text-muted-foreground">{t('device.backup.creating')}</p>}
              </div>

              {/* 排程備份 */}
              <div className="space-y-3 border-t border-surface-border pt-4">
                <div className="flex items-center justify-between gap-3 rounded-lg border border-surface-border px-3 py-2.5">
                  <div className="min-w-0 space-y-0.5">
                    <p className="text-sm font-medium text-foreground">{t('device.backup.schedule.title')}</p>
                    <p className="text-xs text-muted-foreground">{t('device.backup.schedule.desc')}</p>
                  </div>
                  <Switch
                    checked={draftEnabled}
                    onCheckedChange={(v) => setDraftEnabled(Boolean(v))}
                    disabled={scheduleLoading}
                    aria-label={t('device.backup.schedule.title')}
                  />
                </div>
                {draftEnabled && (
                  <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                    <div className="space-y-1">
                      <label htmlFor="backup-schedule-interval" className="text-xs text-muted-foreground">
                        {t('device.backup.schedule.interval')}
                      </label>
                      <Input
                        id="backup-schedule-interval"
                        type="number"
                        min={1}
                        max={8760}
                        value={draftInterval}
                        onChange={(e) => setDraftInterval(e.target.value)}
                        disabled={scheduleLoading}
                      />
                    </div>
                    <div className="space-y-1">
                      <label htmlFor="backup-schedule-retention" className="text-xs text-muted-foreground">
                        {t('device.backup.schedule.retention')}
                      </label>
                      <Input
                        id="backup-schedule-retention"
                        type="number"
                        min={1}
                        max={1000}
                        value={draftRetention}
                        onChange={(e) => setDraftRetention(e.target.value)}
                        disabled={scheduleLoading}
                      />
                    </div>
                  </div>
                )}
                <Button
                  variant="outline"
                  size="sm"
                  onClick={saveSchedule}
                  disabled={scheduleLoading || scheduleSaving || !scheduleDirty}
                >
                  {scheduleSaving ? t('device.backup.schedule.saving') : t('device.backup.schedule.save')}
                </Button>
              </div>

              {/* 備份清單 */}
              <div className="space-y-2 border-t border-surface-border pt-4">
                <p className="text-sm font-medium text-foreground">{t('device.backup.list.title')}</p>
                {backupListLoading ? (
                  <Skeleton className="h-16 w-full rounded-lg" />
                ) : backupListError ? (
                  <ErrorState variant="inline" icon={Archive} error={backupListError} onRetry={fetchBackupList} />
                ) : backupFiles.length === 0 ? (
                  <p className="text-sm text-muted-foreground">{t('device.backup.list.empty')}</p>
                ) : (
                  <div className="overflow-x-auto">
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>{t('device.backup.list.col.name')}</TableHead>
                          <TableHead>{t('device.backup.list.col.size')}</TableHead>
                          <TableHead>{t('device.backup.list.col.time')}</TableHead>
                          <TableHead />
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {backupFiles.map((f) => (
                          <TableRow key={f.name}>
                            <TableCell className="font-mono text-xs text-foreground">{f.name}</TableCell>
                            <TableCell className="text-xs text-muted-foreground">{formatMb(f.size / (1024 * 1024))}</TableCell>
                            <TableCell className="text-xs text-muted-foreground">{timeAgo(f.mtime)}</TableCell>
                            <TableCell className="text-right">
                              <div className="flex justify-end gap-1">
                                <Button
                                  variant="ghost"
                                  size="icon-sm"
                                  onClick={() => downloadBackup(f.name)}
                                  aria-label={t('files.download')}
                                  title={t('files.download')}
                                >
                                  <Download />
                                </Button>
                                <Button
                                  variant="ghost"
                                  size="icon-sm"
                                  onClick={() => setDeleteTarget(f)}
                                  disabled={deletingName === f.name}
                                  aria-label={t('common.delete')}
                                  title={t('common.delete')}
                                >
                                  <Trash2 />
                                </Button>
                              </div>
                            </TableCell>
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </div>
                )}
              </div>
            </div>
          </Panel>

          {/* ⑤ 汰機搬家：從舊機匯入 */}
          <Panel
            icon={UploadCloud}
            title={t('device.restore.title')}
            description={t('device.restore.desc')}
          >
            <div className="space-y-2">
              <input
                ref={importInputRef}
                type="file"
                accept=".tar.gz,.tgz,application/gzip"
                className="hidden"
                aria-label={t('device.restore.pickFile')}
                onChange={(e) => void handleImportFilePicked(e.target.files?.[0] ?? null)}
              />
              <Button
                variant="outline"
                size="sm"
                onClick={() => importInputRef.current?.click()}
                disabled={uploading || restoring}
              >
                <UploadCloud className={cn(uploading && 'animate-pulse')} />
                {uploading
                  ? t('device.restore.uploading', { pct: uploadPct ?? 0 })
                  : t('device.restore.pickFile')}
              </Button>
              <p className="text-xs text-muted-foreground">{t('device.restore.hint')}</p>
            </div>
          </Panel>

          {/* ⑥ 維修模式（Entry A，DESIGN-maintenance-mode-2026-08.md §2） */}
          <MaintenanceModeCard />

          {/* ⑦ 危險區 */}
          <DangerZone title={t('device.section.danger')} description={t('device.section.danger.desc')}>
            <div className="space-y-3">
              {(['factoryReset', 'restart', 'shutdown'] as const).map((action) => (
                <div
                  key={action}
                  className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-destructive/20 bg-background/60 px-3 py-2.5"
                >
                  <div className="min-w-0 space-y-0.5">
                    <p className="text-sm font-medium text-foreground">{t(`device.danger.${action}.title`)}</p>
                    <p className="text-xs text-muted-foreground">{t(`device.danger.${action}.desc`)}</p>
                  </div>
                  <Button variant="destructive" size="sm" onClick={() => openConfirm(action)}>
                    {action === 'factoryReset' ? <AlertTriangle /> : action === 'restart' ? <RefreshCw /> : <Power />}
                    {t(`device.danger.${action}.cta`)}
                  </Button>
                </div>
              ))}
            </div>
          </DangerZone>
        </>
      )}

      <ConfirmDialog
        open={confirmAction != null}
        onClose={() => setConfirmAction(null)}
        onConfirm={() => void runDangerAction()}
        title={confirmAction ? t(`device.danger.${confirmAction}.confirmTitle`) : ''}
        message={confirmAction ? t(`device.danger.${confirmAction}.confirmMessage`) : ''}
        confirmLabel={confirmAction ? t(`device.danger.${confirmAction}.cta`) : undefined}
        requireText={confirmAction === 'factoryReset' ? 'RESET' : undefined}
        requireTextHint={confirmAction === 'factoryReset' ? t('device.danger.factoryReset.confirmHint') : undefined}
        busy={dangerBusy}
      >
        {confirmAction === 'factoryReset' && (
          <label className="flex items-start gap-2 text-sm text-foreground">
            <Checkbox
              className="mt-0.5"
              checked={clearNetworkOnReset}
              onCheckedChange={(v) => setClearNetworkOnReset(v === true)}
            />
            <span>
              <span className="block">{t('device.danger.factoryReset.clearNetwork')}</span>
              <span className="block text-xs text-muted-foreground">
                {t('device.danger.factoryReset.clearNetworkHint')}
              </span>
            </span>
          </label>
        )}
      </ConfirmDialog>

      {/* 回到上一版：重新開機並切回上一版本，資料不受影響 */}
      <ConfirmDialog
        open={rollbackConfirmOpen}
        onClose={() => setRollbackConfirmOpen(false)}
        onConfirm={() => void runUpdateRollback()}
        title={t('device.update.rollback.confirmTitle')}
        message={t('device.update.rollback.confirmMessage')}
        confirmLabel={t('device.update.rollback')}
        busy={rollingBack}
      />

      {/* 刪除單一備份檔 */}
      <ConfirmDialog
        open={deleteTarget != null}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => void confirmDeleteBackup()}
        title={t('device.backup.list.deleteConfirmTitle')}
        message={deleteTarget ? t('device.backup.list.deleteConfirmMessage', { name: deleteTarget.name }) : ''}
        confirmLabel={t('common.delete')}
        busy={deletingName != null}
      />

      {/* 汰機搬家：上傳完成後的還原確認 — 說明會蓋掉現有資料，舊資料保留備份 */}
      <Dialog open={restoreCandidate != null} onOpenChange={(open) => !open && setRestoreCandidate(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('device.restore.confirmTitle')}</DialogTitle>
            <DialogDescription>
              {restoreCandidate
                ? t('device.restore.confirmMessage', { name: restoreCandidate.name })
                : ''}
            </DialogDescription>
          </DialogHeader>
          <p className="text-xs text-muted-foreground">{t('device.restore.confirmPreserved')}</p>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setRestoreCandidate(null)} disabled={restoring}>
              {t('common.cancel')}
            </Button>
            <Button variant="destructive" size="sm" onClick={() => void confirmRestore()} disabled={restoring}>
              {restoring ? t('device.restore.staging') : t('device.restore.confirmCta')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 還原已排入佇列 — 需要重新開機才會生效 */}
      <Dialog open={restoreDone} onOpenChange={(open) => !open && setRestoreDone(false)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('device.restore.doneTitle')}</DialogTitle>
            <DialogDescription>{t('device.restore.doneMessage')}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setRestoreDone(false)}>
              {t('device.restore.doneLater')}
            </Button>
            <Button variant="brand" size="sm" onClick={() => void runRestartAfterRestore()}>
              <RefreshCw />
              {t('device.danger.restart.cta')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
