import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Wrench, ShieldAlert } from 'lucide-react';
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
import { useMaintenanceStore } from '@/stores/maintenance-store';

/**
 * Maintenance Mode — Entry A (`DESIGN-maintenance-mode-2026-08.md` §2).
 *
 * A dashboard software switch, Admin-only + appliance-only server-side, that
 * temporarily unlocks two sub-capabilities beyond the appliance's normal
 * "no Linux surface" posture: remote technical-support access (SSH) and
 * full raw diagnostic detail for `device.*` operation results. Always
 * bounded by a hard TTL (max 24h, no extension) and force-closes on its own
 * if the gateway process restarts — there is no way to leave it open by
 * accident past those two backstops.
 *
 * Copy is deliberately end-user-facing (no "systemd"/"service"/"Linux" —
 * `technical-terms.test.ts` enforces this mechanically): "遠端連線
 * （SSH）技術支援" rather than naming the unit being started, "完整診斷紀錄"
 * rather than "raw stdout/stderr".
 */
const TTL_OPTIONS = [1, 4, 12, 24] as const;

export function MaintenanceModeCard() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const status = useMaintenanceStore((s) => s.status);
  const fetchStatus = useMaintenanceStore((s) => s.fetchStatus);

  const [enableDialogOpen, setEnableDialogOpen] = useState(false);
  const [disableDialogOpen, setDisableDialogOpen] = useState(false);
  const [ttlHours, setTtlHours] = useState<string>('4');
  const [wantSsh, setWantSsh] = useState(false);
  const [wantShowDetails, setWantShowDetails] = useState(true);
  const [reauthPassword, setReauthPassword] = useState('');
  const [disableReason, setDisableReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    void fetchStatus();
    // §2.7 fallback poll: TTL expiry / gateway-restart force-close don't
    // currently push a `maintenance.status_changed` event (see
    // `maintenance-store.ts`'s doc comment) — a 30s poll is the honest
    // backstop until that gap is closed.
    const id = window.setInterval(() => void fetchStatus(), 30_000);
    return () => window.clearInterval(id);
  }, [fetchStatus]);

  // Live remaining-time countdown between polls — purely cosmetic (the
  // server, not this timer, is the source of truth for actual expiry).
  useEffect(() => {
    if (!status?.active) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [status?.active]);

  const remainingLabel = useMemo(() => {
    const win = status?.window;
    if (!win) return null;
    const enabledAtMs = Date.parse(win.enabled_at);
    const elapsedSecs = Number.isFinite(enabledAtMs) ? Math.floor((now - enabledAtMs) / 1000) : 0;
    const remaining = Math.max(0, win.ttl_seconds - elapsedSecs);
    const h = Math.floor(remaining / 3600);
    const m = Math.floor((remaining % 3600) / 60);
    return t('device.maintenance.remaining', { h, m });
  }, [status, now, t]);

  const sshMismatch = status != null && !status.active && status.ssh_observed_active === true;

  function resetEnableForm() {
    setTtlHours('4');
    setWantSsh(false);
    setWantShowDetails(true);
    setReauthPassword('');
  }

  async function handleEnable() {
    setBusy(true);
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
      });
      toast.success(t('device.maintenance.enabled'));
      setEnableDialogOpen(false);
      resetEnableForm();
      await fetchStatus();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleDisable() {
    setBusy(true);
    try {
      await api.maintenance.disable(disableReason.trim() || undefined);
      toast.success(t('device.maintenance.disabled'));
      setDisableDialogOpen(false);
      setDisableReason('');
      await fetchStatus();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  const active = status?.active ?? false;
  const subCaps = status?.window?.sub_capabilities ?? [];

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-3 space-y-0">
        <div className="min-w-0 space-y-1">
          <CardTitle className="flex items-center gap-2 text-sm font-medium">
            <Wrench className="size-4 text-muted-foreground" />
            {t('device.maintenance.title')}
            <Badge
              variant="secondary"
              className={active ? 'bg-amber-500/15 text-amber-600 dark:text-amber-400' : undefined}
            >
              {active ? t('device.maintenance.status.active') : t('device.maintenance.status.inactive')}
            </Badge>
          </CardTitle>
          <CardDescription>{t('device.maintenance.desc')}</CardDescription>
        </div>
        {active ? (
          <Button variant="outline" size="sm" onClick={() => setDisableDialogOpen(true)}>
            {t('device.maintenance.disableCta')}
          </Button>
        ) : (
          <Button variant="brand" size="sm" onClick={() => setEnableDialogOpen(true)}>
            {t('device.maintenance.enableCta')}
          </Button>
        )}
      </CardHeader>
      <CardContent className="space-y-3">
        {sshMismatch && (
          <div className="flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2.5 text-sm text-destructive">
            <ShieldAlert className="mt-0.5 size-4 shrink-0" />
            <span>{t('device.maintenance.sshMismatchWarning')}</span>
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
        {!active && (
          <p className="text-xs text-muted-foreground">{t('device.maintenance.inactiveHint')}</p>
        )}
      </CardContent>

      {/* 開啟維修模式：二次確認（輸入固定字串）+ 重新輸入密碼 + 期限 + 子能力 */}
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
      >
        <div className="space-y-3">
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
              <Checkbox className="mt-0.5" checked={wantSsh} onCheckedChange={(v) => setWantSsh(v === true)} />
              <span>
                <span className="block">{t('device.maintenance.capability.ssh')}</span>
                <span className="block text-xs text-muted-foreground">{t('device.maintenance.capability.sshHint')}</span>
              </span>
            </label>
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
