import { useEffect, useState } from 'react';
import { Link } from 'react-router';
import { useIntl } from 'react-intl';
import { Button, Card, CardContent, CardHeader, CardTitle } from '@/components/mds';
import { useAuthStore } from '@/stores/auth-store';
import { useSetupChecklist } from '@/hooks/useSetupChecklist';
import { setupProgress } from '@/lib/setup-checklist';

export function SetupChecklistCard() {
  const intl = useIntl();
  const userId = useAuthStore(s => s.user?.id);
  const storageKey = `duduclaw.setup.dismissed.${userId ?? ''}`;
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);
  useEffect(() => {
    try { setDismissedKey(localStorage.getItem(storageKey) === '1' ? storageKey : null); }
    catch { setDismissedKey(null); }
  }, [storageKey]);
  const dismissed = dismissedKey === storageKey;
  const { allowed, data } = useSetupChecklist(!dismissed);
  const progress = data ? setupProgress(data) : null;
  if (!allowed || dismissed || !progress || progress.done === progress.total) return null;
  const dismiss = () => {
    setDismissedKey(storageKey);
    try { localStorage.setItem(storageKey, '1'); } catch { /* Keep dismissal for this session. */ }
  };
  return <Card>
    <CardHeader className="flex-row items-center justify-between gap-3">
      <CardTitle>{intl.formatMessage({ id: 'setup.title' })}</CardTitle>
      <Button variant="ghost" size="sm" onClick={dismiss}>{intl.formatMessage({ id: 'setup.dismiss' })}</Button>
    </CardHeader>
    <CardContent className="flex flex-wrap items-center justify-between gap-3">
      <span className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'setup.progress' }, progress)}</span>
      <Link className="text-sm underline underline-offset-4 focus-visible:ring-3 focus-visible:ring-ring/50" to="/setup">{intl.formatMessage({ id: 'setup.open' })}</Link>
    </CardContent>
  </Card>;
}
