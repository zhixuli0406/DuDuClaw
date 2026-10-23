import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { api, type TeamHistorySettings } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { Button, Switch } from '@/components/mds';

export function OrgHistorySettings() {
  const intl = useIntl();
  const admin = useAuthStore(s => s.user?.role === 'admin');
  const [settings, setSettings] = useState<TeamHistorySettings | null>(null);
  const [error, setError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let active = true;
    setSettings(null);
    if (!admin) return;
    setError(false);
    api.users.orgSettings.get().then(data => {
      if (typeof data.manager_team_history !== 'boolean') throw new Error('invalid settings');
      if (active) setSettings(data);
    }).catch(() => { if (active) setError(true); });
    return () => { active = false; };
  }, [admin, revision]);
  if (!admin) return null;
  const text = (id: string) => intl.formatMessage({ id: `teamHistory.${id}` });
  async function update(enabled: boolean) {
    setBusy(true);
    setError(false);
    try {
      const confirmed = await api.users.orgSettings.update(enabled);
      if (typeof confirmed.manager_team_history !== 'boolean') throw new Error('invalid settings');
      setSettings(confirmed);
    } catch { setError(true); }
    finally { setBusy(false); }
  }
  return <section className="space-y-3 rounded-xl border border-surface-border bg-card p-4" aria-label={text('settings.title')}>
    <h2 className="text-sm font-medium">{text('settings.title')}</h2>
    <div className="flex items-start justify-between gap-4">
      <div className="space-y-1"><p id="team-history-setting-label" className="text-sm font-medium">{text('settings.label')}</p><p className="max-w-4xl text-sm text-muted-foreground">{text('settings.description')}</p></div>
      {settings && <Switch aria-labelledby="team-history-setting-label" checked={settings.manager_team_history} disabled={busy} onCheckedChange={enabled => void update(enabled)} />}
    </div>
    <p className="text-xs text-muted-foreground">{text('settings.notice')}</p>
    <Link className="text-sm text-brand underline underline-offset-2" to="/app/system/settings?tab=redaction">{text('settings.detection')}</Link>
    {error && <div role="alert" className="flex flex-wrap items-center gap-2 text-sm text-destructive">{text('unavailable')}<Button variant="outline" size="sm" onClick={() => setRevision(n => n + 1)}>{text('refresh')}</Button></div>}
  </section>;
}
