import { useIntl } from 'react-intl';
import type { GoogleServiceProbe } from '@/lib/api';

function enableUrl(value?: string | null): string | undefined {
  try {
    const url = new URL(value || '');
    return url.protocol === 'https:' && ['console.cloud.google.com', 'console.developers.google.com'].includes(url.hostname) && !url.username && !url.password ? url.href : undefined;
  } catch { return undefined; }
}

/** Scope evidence is displayed separately from a live service read. */
export function GoogleServiceResults({ probes }: { probes: GoogleServiceProbe[] }) {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `google.wizard.${key}` });
  return <ul className="space-y-2 text-sm">{probes.map(probe => <li key={probe.service} className="rounded-lg border border-surface-border p-3">
    <span>{probe.service}: {t(probe.ok ? 'ok' : 'failed')}</span>
    <p className="text-muted-foreground">{intl.formatMessage({ id: `google.probe.${probe.reason}`, defaultMessage: t(probe.ok ? 'scopeGranted' : 'verifyFailed') })}</p>
    {!probe.ok && <p className="text-muted-foreground">{t('reasonCode')}: <code>{probe.reason}</code></p>}
    {!probe.verified && <p className="text-warning">{t('notVerified')}</p>}
    {enableUrl(probe.enable_url) && <a href={enableUrl(probe.enable_url)} target="_blank" rel="noreferrer" className="text-brand underline">{t('enableApi')}</a>}
  </li>)}</ul>;
}
