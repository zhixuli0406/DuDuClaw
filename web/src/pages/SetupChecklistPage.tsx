import { Link, Navigate } from 'react-router';
import { useIntl } from 'react-intl';
import { PageHeader, Card, CardContent, CardHeader, CardTitle, Badge } from '@/components/mds';
import { useSetupChecklist } from '@/hooks/useSetupChecklist';
import { setupItems, setupProgress, setupSections } from '@/lib/setup-checklist';

export function SetupChecklistPage() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const { allowed, data } = useSetupChecklist();
  if (!allowed) return <Navigate to="/" replace />;
  const hasProgress = data !== null && Object.values(data).some(value => value !== undefined);
  const progress = hasProgress ? setupProgress(data!) : null;
  return <div className="flex h-full flex-col">
    <PageHeader><h1 className="text-lg font-semibold">{t('setup.title')}</h1></PageHeader>
    <main className="mx-auto w-full max-w-3xl space-y-5 overflow-auto p-6">
      <p role="status" className="text-muted-foreground">{progress ? intl.formatMessage({ id: 'setup.progress' }, progress) : t('setup.loading')}</p>
      {setupSections.map(section => <Card key={section}>
        <CardHeader><CardTitle>{t(`setup.section.${section}`)}</CardTitle></CardHeader>
        <CardContent><ul className="divide-y divide-surface-border">
          {setupItems.filter(item => item.section === section).map(item => {
            const value = item.id === 'permissions' ? data?.users : data?.[item.id as keyof typeof data];
            const done = data && value !== undefined ? item.check(data) : null;
            return <li key={item.id} className="flex items-center justify-between gap-3 py-3">
              <Link className="min-w-0 text-sm underline underline-offset-4 focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50" to={item.to}>{t(item.label)}{item.optional && <span className="text-muted-foreground"> {t('setup.optional')}</span>}</Link>
              <Badge variant="secondary">{t(value === undefined ? 'setup.loading' : done === null ? 'setup.unknown' : done ? 'setup.done' : 'setup.pending')}</Badge>
            </li>;
          })}
        </ul></CardContent>
      </Card>)}
    </main>
  </div>;
}
