import { useIntl } from 'react-intl';
import type { MaintenanceStatus } from '@/lib/api';
import { MAINT_LOGIN_USER, MAINT_MDNS_HOST } from '@/lib/maintenance-ssh';
import { CopyField } from './CopyField';
import { Notice } from './Notice';

/**
 * What an Admin needs while a keyed SSH window is open (design §7.1): how to
 * sign in, the technician's key fingerprint, this device's host key
 * fingerprints with the right verification instruction (with or without a
 * notification channel, R2-L10), expiry, live sessions, failed attempts, the
 * accepted source range and what ends the window early.
 */
export function MaintenanceSshPanel({
  status,
  addresses,
}: {
  status: MaintenanceStatus;
  addresses: readonly string[];
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const win = status.window;
  if (!win) return null;
  const ssh = status.ssh;

  if (win.state === 'opening') {
    return (
      <Notice tone="info">
        <p>{t('device.maintenance.ssh.opening')}</p>
      </Notice>
    );
  }

  const keyFingerprint = win.ssh_key_fingerprint ?? ssh?.key_fingerprint ?? null;
  const hostFingerprints =
    win.host_key_fingerprints && win.host_key_fingerprints.length > 0
      ? win.host_key_fingerprints
      : (ssh?.host_key_fingerprints ?? []);
  const sessions = ssh?.sessions ?? [];
  const expiresMs = Date.parse(win.expires_at);
  const expiresLabel = Number.isFinite(expiresMs)
    ? intl.formatDate(expiresMs, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
    : win.expires_at;

  return (
    <section className="space-y-3 rounded-lg border border-border p-3" aria-labelledby="maintenance-ssh-heading">
      <h3 id="maintenance-ssh-heading" className="text-sm font-medium text-foreground">
        {t('device.maintenance.capability.ssh')}
      </h3>

      <div className="space-y-1.5">
        <p className="text-xs font-medium text-muted-foreground">{t('device.maintenance.ssh.signIn')}</p>
        <CopyField value={`ssh ${MAINT_LOGIN_USER}@${MAINT_MDNS_HOST}`} label={t('device.maintenance.ssh.signIn')} />
        {addresses.length > 0 && (
          <>
            <p className="text-xs text-muted-foreground">{t('device.maintenance.ssh.signInByAddress')}</p>
            {addresses.map((addr) => (
              <CopyField key={addr} value={`ssh ${MAINT_LOGIN_USER}@${addr}`} label={addr} />
            ))}
          </>
        )}
        <p className="text-xs text-muted-foreground">{t('device.maintenance.ssh.sourceRange')}</p>
      </div>

      {keyFingerprint && (
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">
            {t('device.maintenance.ssh.keyFingerprint')}
            {win.ssh_key_label ? ` · ${win.ssh_key_label}` : ''}
          </p>
          <CopyField value={keyFingerprint} label={t('device.maintenance.ssh.keyFingerprint')} />
        </div>
      )}

      {hostFingerprints.length > 0 && (
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">{t('device.maintenance.ssh.hostFingerprint')}</p>
          {hostFingerprints.map((fp) => (
            <CopyField key={fp} value={fp} label={t('device.maintenance.ssh.hostFingerprint')} />
          ))}
        </div>
      )}

      <Notice tone={status.push_channel_available === true ? 'info' : 'warning'}>
        <p>
          {status.push_channel_available === true
            ? t('device.maintenance.ssh.verifyPush')
            : t('device.maintenance.ssh.verifyNoPush')}
        </p>
      </Notice>

      <div className="space-y-1 text-sm text-muted-foreground" role="status" aria-live="polite">
        <p>{t('device.maintenance.ssh.expires', { time: expiresLabel })}</p>
        <p>{t('device.maintenance.ssh.sessions', { n: sessions.length })}</p>
        {typeof ssh?.auth_failures === 'number' && ssh.auth_failures > 0 && (
          <p>{t('device.maintenance.ssh.authFailures', { n: ssh.auth_failures })}</p>
        )}
      </div>

      {sessions.length > 0 && (
        <ul className="space-y-1 text-xs text-muted-foreground" aria-label={t('device.maintenance.ssh.sessionsLabel')}>
          {sessions.map((s, i) => (
            <li key={`${s.source ?? 'unknown'}-${s.since_unix}-${i}`} className="font-mono">
              {t('device.maintenance.ssh.sessionRow', {
                source: s.source ?? t('device.maintenance.ssh.sourceUnknown'),
                since: intl.formatTime(s.since_unix * 1000, { hour: '2-digit', minute: '2-digit' }),
              })}
            </li>
          ))}
        </ul>
      )}

      <p className="text-xs text-muted-foreground">{t('device.maintenance.ssh.menuScope')}</p>
      <p className="text-xs text-muted-foreground">{t('device.maintenance.ssh.restartGatewayNote')}</p>

      {ssh && ssh.problems.length > 0 && (
        <Notice tone="warning">
          <p>{t('device.maintenance.ssh.problems')}</p>
        </Notice>
      )}
      {ssh?.pending_audit && (
        <Notice tone="info">
          <p>{t('device.maintenance.ssh.pendingAudit')}</p>
        </Notice>
      )}
    </section>
  );
}
