import { useEffect, useState } from 'react';
import { Link } from 'react-router';
import { ActorAvatar, Badge, Card, CardContent, CardDescription, CardHeader, CardTitle, Skeleton } from '@/components/mds';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import type { PersonalEmployeeRequest } from '@/lib/isolation-restore-api';
import { useRestoreT } from '@/pages/protected/restore/restore-ui';

/**
 * 我的個人 AI 員工 — each of the caller's working Personal employees, with
 * the accounts it uses: the caller's own Google connection and any
 * company-shared account an Admin set up for it. Connecting is done on
 * 我的連線 (the single place that writes it).
 */
export function MyPersonalEmployeeCard({ requests }: { requests: PersonalEmployeeRequest[] | null }) {
  const t = useRestoreT();
  const [google, setGoogle] = useState<PersonalGoogleStatus | null>(null);
  const [googleFailed, setGoogleFailed] = useState(false);

  useEffect(() => {
    let active = true;
    api.personalGoogle
      .status()
      .then((s) => { if (active) setGoogle(s); })
      .catch(() => { if (active) setGoogleFailed(true); });
    return () => { active = false; };
  }, []);

  const mine = requests?.filter((r) => r.status === 'applied') ?? null;
  const connection = google?.connection ?? null;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('myPersonal.mine.title')}</CardTitle>
        <CardDescription>{t('myPersonal.mine.desc')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {mine === null && <Skeleton className="h-16 w-full" />}
        {mine !== null && mine.length === 0 && <p className="text-sm text-muted-foreground">{t('myPersonal.mine.none')}</p>}
        {mine?.map((employee) => {
          const shared = google?.delegated.filter((d) => d.agent_id === employee.label) ?? [];
          return (
            <section key={employee.request_id} aria-label={employee.display_name} className="space-y-3 rounded-lg border border-surface-border p-3">
              <div className="flex flex-wrap items-center gap-3">
                <ActorAvatar actorType="agent" name={employee.display_name} size="lg" />
                <div className="min-w-0 flex-1">
                  <Link to={`/agents/${encodeURIComponent(employee.label)}`} className="block truncate text-sm font-medium underline-offset-2 hover:underline" title={employee.display_name}>
                    {employee.display_name}
                  </Link>
                  <span className="font-mono text-xs text-muted-foreground">{employee.label}</span>
                </div>
                <Badge variant="secondary">{t('myPersonal.mine.active')}</Badge>
              </div>
              <div className="space-y-1.5">
                <h3 className="text-xs font-medium text-muted-foreground">{t('myPersonal.mine.connections')}</h3>
                {/* Board "Mine": one tile per account — the account on the left, whose it is on the right. */}
                <ul className="grid gap-2 text-sm sm:grid-cols-2">
                  <li className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-muted/40 px-3 py-2.5">
                    <span className="flex min-w-0 flex-wrap items-center gap-1">
                      <span>{t('myPersonal.mine.googlePersonal')}</span>
                      {connection && !googleFailed && (
                        <>
                          <span aria-hidden="true">·</span>
                          <span className="break-all">{connection.account_email}</span>
                        </>
                      )}
                    </span>
                    {googleFailed ? (
                      <span className="text-muted-foreground">{t('myPersonal.mine.googleUnknown')}</span>
                    ) : connection ? (
                      <Badge variant={connection.status === 'connected' ? 'secondary' : 'destructive'}>
                        {t(`myPersonal.mine.google.${connection.status}`)}
                      </Badge>
                    ) : (
                      <span className="text-muted-foreground">{t('myPersonal.mine.googleNone')}</span>
                    )}
                  </li>
                  {shared.map((row) => (
                    <li key={row.email} className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-muted/40 px-3 py-2.5">
                      <span className="break-all">{row.email}</span>
                      <span className="text-muted-foreground">{t('myPersonal.mine.companyShared')}</span>
                    </li>
                  ))}
                </ul>
                <Link
                  to="/me/connections"
                  className="inline-flex h-9 items-center rounded-lg border border-input px-3 text-sm font-medium hover:bg-muted"
                >
                  {t('myPersonal.mine.relinkGoogle')}
                </Link>
              </div>
            </section>
          );
        })}
      </CardContent>
    </Card>
  );
}
