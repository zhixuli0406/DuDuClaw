import { useEffect, useState } from 'react';
import { Link } from 'react-router';
import { useIntl } from 'react-intl';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { Button, Card, CardContent, CardHeader, CardTitle } from '@/components/mds';

export function missingConnections(status: PersonalGoogleStatus): string[] {
  const items: string[] = [];
  if (!status.connection || status.connection.status !== 'connected') items.push('google');
  if (status.requests.some(request => request.allowed !== true)) items.push('consent');
  return items;
}

export function ConnectionsChecklist({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  const user = useAuthStore(s => s.user);
  const [items, setItems] = useState<string[]>([]);
  const [dismissed, setDismissed] = useState(false);
  const storageKey = `duduclaw.connectionsChecklist.dismissed.${user?.id ?? ''}`;
  useEffect(() => {
    let active = true;
    let hidden = false;
    try { hidden = localStorage.getItem(storageKey) === '1'; } catch { /* Storage can be blocked by the browser. */ }
    setDismissed(hidden); setItems([]);
    if (!enabled || user?.role !== 'employee' || hidden) return;
    void api.personalGoogle.status().then(status => { if (active) setItems(missingConnections(status)); }).catch(() => { if (active) setItems([]); });
    return () => { active = false; };
  }, [enabled, user?.role, storageKey]);
  if (user?.role !== 'employee' || dismissed || !items.length) return null;
  const dismiss = () => {
    setDismissed(true);
    try { localStorage.setItem(storageKey, '1'); } catch { /* Dismiss for the current session even when persistence is unavailable. */ }
  };
  return <Card><CardHeader className="flex-row items-center justify-between gap-3"><CardTitle>{intl.formatMessage({ id: 'home.connections.title' })}</CardTitle><Button variant="ghost" size="sm" onClick={dismiss}>{intl.formatMessage({ id: 'home.connections.dismiss' })}</Button></CardHeader>
    <CardContent><ul className="space-y-2">{items.map(item => <li key={item}><Link className="text-sm underline" to="/me/connections">{intl.formatMessage({ id: `home.connections.${item}` })}</Link></li>)}</ul></CardContent>
  </Card>;
}
