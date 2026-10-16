import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Wrench } from 'lucide-react';
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  Badge,
  Button,
  Checkbox,
  Input,
} from '@/components/mds';
import { ConfirmDialog, OptionSelect } from '@/components/settings/controls';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  closeTriggerMessageId,
  enableErrorMessage,
  asMaintenanceError,
  looksLikePrivateKey,
  minutesUntil,
  precheckPublicKey,
  sshAvailability,
  unsupportedMessageId,
} from '@/lib/maintenance-ssh';
import { useMaintenanceStore } from '@/stores/maintenance-store';
import { MaintenanceSshPanel } from '@/components/settings/maintenance/MaintenanceSshPanel';
import { MaintenanceHistory } from '@/components/settings/maintenance/MaintenanceHistory';
import { SshKeyField } from '@/components/settings/maintenance/SshKeyField';
import { Notice } from '@/components/settings/maintenance/Notice';

/**
 * Maintenance Mode — Entry A (`DESIGN-maintenance-mode-2026-08.md` §2), with
 * the keyed SSH window of `DESIGN-maintenance-ssh-account-2026-10.md` §7.
 *
 * A dashboard software switch, Admin-only + appliance-only server-side, that
 * temporarily unlocks two sub-capabilities beyond the appliance's normal
 * "no Linux surface" posture: remote technical-support access (SSH, with the
 * technician's one public key) and full raw diagnostic detail for `device.*`
 * operation results. Always bounded by a hard TTL (max 24h, no extension)
 * and force-closes on its own if the gateway process restarts.
 *
 * The key pre-check here is a hint; the gateway and the device's system
 * helper parse the key again and are the only authority. Copy is end-user
 * facing (no unit names or paths; `technical-terms.test.ts` enforces it).
 */
const TTL_OPTIONS = [1, 4, 12, 24] as const;

type Msg = { id: string; values?: Record<string, string | number> } | { text: string };

export function MaintenanceModeCard({ addresses = [] }: { addresses?: readonly string[] } = {}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const status = useMaintenanceStore((s) => s.status);
  const fetchStatus = useMaintenanceStore((s) => s.fetchStatus);

  const [enableDialogOpen, setEnableDialogOpen] = useState(false);
  const [disableDialogOpen, setDisableDialogOpen] = useState(false);
  const [ttlHours, setTtlHours] = useState<string>('4');
  const [wantSsh, setWantSsh] = useState(false);
  const [wantShowDetails, setWantShowDetails] = useState(true);
  const [sshKey, setSshKey] = useState('');
  const [keyTouched, setKeyTouched] = useState(false);
  const [privateKeyCleared, setPrivateKeyCleared] = useState(false);
  const [reauthPassword, setReauthPassword] = useState('');
  const [disableReason, setDisableReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [dialogError, setDialogError] = useState<Msg | null>(null);
  const [lockUntilMs, setLockUntilMs] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    void fetchStatus();
    // Fallback poll: a TTL expiry or a close by the device itself is only
    // seen on the next status call.
    const id = window.setInterval(() => void fetchStatus(), 30_000);
    return () => window.clearInterval(id);
  }, [fetchStatus]);

  // The re-auth lock comes back as "seconds from now"; pin it to a clock time.
  useEffect(() => {
    const r = status?.reauth;
    if (r?.locked && typeof r.retry_after_seconds === 'number') {
      const observedAt = Date.now();
      setNow(observedAt);
      setLockUntilMs(observedAt + r.retry_after_seconds * 1000);
    } else if (r && !r.locked) {
      setLockUntilMs(null);
    }
  }, [status?.reauth]);

  const lockRemainingS = lockUntilMs !== null ? Math.max(0, Math.ceil((lockUntilMs - now) / 1000)) : 0;
  const lockUnknown = status?.reauth?.locked === true && status.reauth.retry_after_seconds === null;
  const locked = lockRemainingS > 0 || lockUnknown;

  // Cosmetic one-second clock for the countdowns; the server decides expiry.
  const ticking = Boolean(status?.active) || lockUntilMs !== null;
  useEffect(() => {
    if (!ticking) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [ticking]);

  useEffect(() => {
    if (lockUntilMs !== null && lockRemainingS === 0) {
      setLockUntilMs(null);
      void fetchStatus();
    }
  }, [lockUntilMs, lockRemainingS, fetchStatus]);

  const remainingLabel = useMemo(() => {
    const win = status?.window;
    if (!win) return null;
    const expiresMs = Date.parse(win.expires_at);
    const enabledAtMs = Date.parse(win.enabled_at);
    const remaining = Number.isFinite(expiresMs)
      ? Math.max(0, Math.floor((expiresMs - now) / 1000))
      : Math.max(0, win.ttl_seconds - (Number.isFinite(enabledAtMs) ? Math.floor((now - enabledAtMs) / 1000) : 0));
    const h = Math.floor(remaining / 3600);
    const m = Math.floor((remaining % 3600) / 60);
    return intl.formatMessage({ id: 'device.maintenance.remaining' }, { h, m });
  }, [status, now, intl]);

  const availability = sshAvailability(status?.ssh);
  const sshAllowed = availability === 'available';
  const keyHint = privateKeyCleared ? 'private_key' : precheckPublicKey(sshKey);
  const closeUnconfirmed = status?.close_unconfirmed === true;
  const sshMismatch = !closeUnconfirmed && status != null && !status.active && status.ssh_observed_active === true;

  function resetEnableForm() {
    setTtlHours('4');
    setWantSsh(false);
    setWantShowDetails(true);
    setSshKey('');
    setKeyTouched(false);
    setPrivateKeyCleared(false);
    setReauthPassword('');
    setDialogError(null);
  }

  function handleKeyChange(next: string) {
    setKeyTouched(true);
    setDialogError(null);
    if (looksLikePrivateKey(next)) {
      // Never keep (or send) a private key: clear it on the spot (§7.1).
      setSshKey('');
      setPrivateKeyCleared(true);
      return;
    }
    setPrivateKeyCleared(false);
    setSshKey(next);
  }

  function showEnableError(e: unknown) {
    const err = asMaintenanceError(e);
    if (err?.code === 'reauth_failed') setReauthPassword('');
    if (err?.code === 'reauth_locked') {
      setReauthPassword('');
      setLockUntilMs(Date.now() + minutesUntil(err.retry_after_seconds) * 60_000);
      if (typeof err.retry_after_seconds === 'number') {
        setLockUntilMs(Date.now() + err.retry_after_seconds * 1000);
      }
    }
    if (err?.code === 'ssh_key_invalid' && err.reason === 'private_key') {
      setSshKey('');
      setPrivateKeyCleared(true);
    }
    const ref = enableErrorMessage(e);
    setDialogError(ref ?? { text: formatError(e) });
  }

  async function handleEnable() {
    setBusy(true);
    setDialogError(null);
    try {
      const subCapabilities = [
        ...(wantSsh ? ['ssh'] : []),
        ...(wantShowDetails ? ['show_details'] : []),
      ];
      await api.maintenance.enable({
        ttlHours: Number(ttlHours),
        subCapabilities,
        confirmText: 'MAINTENANCE',
        reauthPassword,
        ...(wantSsh ? { sshPublicKey: sshKey.trim() } : {}),
      });
      toast.success(t('device.maintenance.enabled'));
      setEnableDialogOpen(false);
      resetEnableForm();
      await fetchStatus();
    } catch (e) {
      showEnableError(e);
      void fetchStatus();
    } finally {
      setBusy(false);
    }
  }

  async function handleDisable() {
    setBusy(true);
    try {
      const res = await api.maintenance.disable(disableReason.trim() || undefined);
      if (res.window?.close_error) {
        toast.error(t('device.maintenance.ssh.closeUnconfirmed'));
      } else {
        toast.success(t('device.maintenance.disabled'));
      }
      setDisableDialogOpen(false);
      setDisableReason('');
      await fetchStatus();
    } catch (e) {
      const ref = enableErrorMessage(e);
      toast.error(ref ? t(ref.id, ref.values) : formatError(e));
    } finally {
      setBusy(false);
    }
  }

  const active = status?.active ?? false;
  const opening = status?.window?.state === 'opening';
  const subCaps = status?.window?.sub_capabilities ?? [];
  const lastClose = status?.ssh?.last_close ?? null;
  const lockMessage = lockUnknown
    ? t('device.maintenance.ssh.lockedUnknown')
    : t('device.maintenance.ssh.locked', { m: minutesUntil(lockRemainingS) });
  const confirmDisabled =
    locked ||
    (!wantSsh && !wantShowDetails) ||
    (wantSsh && (!sshAllowed || keyHint !== null));
  const sshUnavailableHint =
    availability === 'unsupported'
      ? t(unsupportedMessageId(status?.ssh?.reason))
      : availability === 'unknown'
        ? t('device.maintenance.ssh.unknownSupport')
        : null;

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-3 space-y-0">
        <div className="min-w-0 space-y-1">
          <CardTitle className="flex items-center gap-2 text-sm font-medium">
            <Wrench className="size-4 text-muted-foreground" aria-hidden="true" />
            {t('device.maintenance.title')}
            <Badge
              variant="secondary"
              className={active || opening ? 'bg-amber-500/15 text-amber-700 dark:text-amber-400' : undefined}
            >
              {active || opening ? t('device.maintenance.status.active') : t('device.maintenance.status.inactive')}
            </Badge>
          </CardTitle>
          <CardDescription>{t('device.maintenance.desc')}</CardDescription>
        </div>
        {active ? (
          <Button variant="outline" size="sm" onClick={() => setDisableDialogOpen(true)}>
            {t('device.maintenance.disableCta')}
          </Button>
        ) : (
          <Button
            variant="brand"
            size="sm"
            onClick={() => setEnableDialogOpen(true)}
            disabled={locked || opening}
            aria-describedby={locked ? 'maintenance-lock-notice' : undefined}
          >
            {t('device.maintenance.enableCta')}
          </Button>
        )}
      </CardHeader>
      <CardContent className="space-y-3">
        {closeUnconfirmed && (
          <Notice tone="danger">
            <p>{t('device.maintenance.ssh.closeUnconfirmed')}</p>
          </Notice>
        )}
        {sshMismatch && (
          <Notice tone="danger">
            <p>{t('device.maintenance.sshMismatchWarning')}</p>
          </Notice>
        )}
        {locked && !active && (
          <div id="maintenance-lock-notice">
            <Notice tone="warning">
              <p>{lockMessage}</p>
            </Notice>
          </div>
        )}
        {active && status?.window && (
          <div className="space-y-1.5 text-sm">
            <p className="text-muted-foreground">{remainingLabel}</p>
            <p className="text-muted-foreground">
              {t('device.maintenance.enabledBy', { email: status.window.enabled_by_email })}
            </p>
            <div className="flex flex-wrap gap-1.5">
              {subCaps.includes('ssh') && (
                <Badge variant="secondary">{t('device.maintenance.capability.ssh')}</Badge>
              )}
              {subCaps.includes('show_details') && (
                <Badge variant="secondary">{t('device.maintenance.capability.showDetails')}</Badge>
              )}
            </div>
          </div>
        )}
        {(active || opening) && status?.window && subCaps.includes('ssh') && (
          <MaintenanceSshPanel status={status} addresses={addresses} />
        )}
        {!active && !opening && (
          <>
            {lastClose && (
              <Notice tone="info">
                <p>{t('device.maintenance.ssh.lastClose', { reason: t(closeTriggerMessageId(lastClose.trigger)) })}</p>
                {lastClose.events_lost && <p>{t('device.maintenance.ssh.eventsLost')}</p>}
              </Notice>
            )}
            {status?.ssh?.pending_cleanup && (
              <Notice tone="info">
                <p>{t('device.maintenance.ssh.pendingCleanup')}</p>
              </Notice>
            )}
            {status?.ssh?.pending_audit && (
              <Notice tone="info">
                <p>{t('device.maintenance.ssh.pendingAudit')}</p>
              </Notice>
            )}
            <p className="text-xs text-muted-foreground">{t('device.maintenance.inactiveHint')}</p>
          </>
        )}
        <MaintenanceHistory />
      </CardContent>

      {/* 開啟維修模式：二次確認（輸入固定字串）+ 重新輸入密碼 + 期限 + 子能力 + 公開金鑰 */}
      <ConfirmDialog
        open={enableDialogOpen}
        onClose={() => {
          setEnableDialogOpen(false);
          resetEnableForm();
        }}
        onConfirm={() => void handleEnable()}
        title={t('device.maintenance.enableCta')}
        message={t('device.maintenance.confirmMessage')}
        confirmLabel={t('device.maintenance.enableCta')}
        requireText="MAINTENANCE"
        requireTextHint={t('device.maintenance.confirmHint')}
        busy={busy}
        confirmDisabled={confirmDisabled}
      >
        <div className="max-h-[50vh] space-y-3 overflow-y-auto pr-1">
          {locked && (
            <Notice tone="warning">
              <p>{lockMessage}</p>
            </Notice>
          )}
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground" htmlFor="maintenance-ttl">
              {t('device.maintenance.ttlLabel')}
            </label>
            <OptionSelect
              id="maintenance-ttl"
              value={ttlHours}
              onChange={setTtlHours}
              showRaw={false}
              options={TTL_OPTIONS.map((h) => ({
                value: String(h),
                label: t('device.maintenance.ttlOption', { h }),
              }))}
            />
          </div>
          <div className="space-y-2">
            <label className="flex items-start gap-2 text-sm text-foreground">
              <Checkbox
                className="mt-0.5"
                checked={wantSsh}
                disabled={!sshAllowed && !wantSsh}
                aria-describedby="maintenance-ssh-option-hint"
                onCheckedChange={(v) => {
                  setWantSsh(v === true);
                  setDialogError(null);
                }}
              />
              <span>
                <span className="block">{t('device.maintenance.capability.ssh')}</span>
                <span id="maintenance-ssh-option-hint" className="block text-xs text-muted-foreground">
                  {sshUnavailableHint ?? t('device.maintenance.capability.sshHint')}
                </span>
              </span>
            </label>
            {wantSsh && sshAllowed && (
              <SshKeyField value={sshKey} onChange={handleKeyChange} hint={keyHint} showEmptyHint={keyTouched} />
            )}
            <label className="flex items-start gap-2 text-sm text-foreground">
              <Checkbox
                className="mt-0.5"
                checked={wantShowDetails}
                onCheckedChange={(v) => setWantShowDetails(v === true)}
              />
              <span>
                <span className="block">{t('device.maintenance.capability.showDetails')}</span>
                <span className="block text-xs text-muted-foreground">
                  {t('device.maintenance.capability.showDetailsHint')}
                </span>
              </span>
            </label>
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground" htmlFor="maintenance-reauth">
              {t('device.maintenance.reauthLabel')}
            </label>
            <Input
              id="maintenance-reauth"
              type="password"
              value={reauthPassword}
              onChange={(e) => setReauthPassword(e.target.value)}
              autoComplete="current-password"
            />
          </div>
          {dialogError && (
            <Notice tone="danger">
              <p>{'text' in dialogError ? dialogError.text : t(dialogError.id, dialogError.values)}</p>
            </Notice>
          )}
        </div>
      </ConfirmDialog>

      {/* 關閉維修模式：不需二次確認（關閉是安全方向） */}
      <ConfirmDialog
        open={disableDialogOpen}
        onClose={() => {
          setDisableDialogOpen(false);
          setDisableReason('');
        }}
        onConfirm={() => void handleDisable()}
        title={t('device.maintenance.disableCta')}
        message={t('device.maintenance.disableConfirmMessage')}
        confirmLabel={t('device.maintenance.disableCta')}
        busy={busy}
      >
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground" htmlFor="maintenance-disable-reason">
            {t('device.maintenance.reasonLabel')}
          </label>
          <Input
            id="maintenance-disable-reason"
            value={disableReason}
            onChange={(e) => setDisableReason(e.target.value)}
          />
        </div>
      </ConfirmDialog>
    </Card>
  );
}
