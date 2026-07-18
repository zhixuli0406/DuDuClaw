import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Puzzle,
  Plug,
  Check,
  X,
  Clock,
  ShieldCheck,
  ShieldAlert,
  AlertTriangle,
  User,
  Loader2,
} from 'lucide-react';
import { api, type InstallRequestInfo } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { toast, formatError } from '@/lib/toast';
import { Card, CardContent, Badge, Empty, Button } from '@/components/mds';

const SEVERITY_COLOR: Record<string, string> = {
  critical: 'text-destructive',
  error: 'text-orange-500',
  high: 'text-orange-500',
  warning: 'text-warning',
  medium: 'text-warning',
  info: 'text-muted-foreground',
  low: 'text-muted-foreground',
};

type RiskBadge = { variant: 'secondary' | 'destructive'; className?: string };

const RISK_BADGE: Record<string, RiskBadge> = {
  Clean: { variant: 'secondary', className: 'text-success' },
  Low: { variant: 'secondary', className: 'text-success' },
  Medium: { variant: 'secondary', className: 'text-warning' },
  High: { variant: 'destructive' },
  Critical: { variant: 'destructive' },
};

/**
 * Actionable install requests (Skill / MCP) for a manager or admin. Shows the
 * item's function and its security-scan verdict; approve advances the
 * signature chain (and, on final approval, triggers the install server-side).
 */
export function InstallRequestsSection() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [items, setItems] = useState<InstallRequestInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [deciding, setDeciding] = useState<Record<string, boolean>>({});

  const load = useCallback(async () => {
    try {
      const res = await api.installRequests.list();
      setItems(res?.requests ?? []);
    } catch (e) {
      console.warn('[api]', e);
      // 403 for employees is expected — they never see this section's data.
      setItems([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    load();
  }, [connectionState, load]);

  const decide = useCallback(
    async (item: InstallRequestInfo, approve: boolean) => {
      setDeciding((prev) => ({ ...prev, [item.id]: true }));
      try {
        const res = await api.installRequests.decide(item.id, approve);
        if (res.warning) {
          toast.error(res.warning);
        } else if (res.status === 'approved') {
          toast.success(intl.formatMessage({ id: 'install.request.approvedInstalled' }, { title: item.title }));
        } else if (res.status === 'pending') {
          toast.success(intl.formatMessage({ id: 'install.request.advancedToAdmin' }, { title: item.title }));
        } else {
          toast.success(intl.formatMessage({ id: 'install.request.deniedToast' }, { title: item.title }));
        }
        await load();
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setDeciding((prev) => {
          const next = { ...prev };
          delete next[item.id];
          return next;
        });
      }
    },
    [intl, load],
  );

  // Hide the whole section for employees / when empty (keeps the page calm).
  if (!loading && items.length === 0) return null;

  return (
    <section className="space-y-3">
      <h2 className="text-base font-medium text-foreground">
        {intl.formatMessage({ id: 'install.request.section' })}
      </h2>
      {loading ? null : (
        <div className="space-y-3">
          {items.map((item) => {
            const busy = !!deciding[item.id];
            const KindIcon = item.kind === 'skill' ? Puzzle : Plug;
            const stageLabel = intl.formatMessage({ id: `install.request.stage.${item.stage}`, defaultMessage: item.stage });
            const riskBadge = RISK_BADGE[item.risk_level] ?? { variant: 'secondary' as const };
            const passed = item.risk_level === 'Clean' || item.risk_level === 'Low' || item.risk_level === 'Medium';
            return (
              <Card key={item.id}>
                <CardContent>
                <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
                  <div className="flex min-w-0 items-start gap-3">
                    <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand/10 text-brand">
                      <KindIcon className="h-[1.125rem] w-[1.125rem]" />
                    </span>
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <Badge>
                          {intl.formatMessage({ id: `install.request.kind.${item.kind}` })}
                        </Badge>
                        <Badge variant="secondary">{stageLabel}</Badge>
                        <span className="flex items-center gap-1 text-xs text-muted-foreground">
                          <User className="h-3 w-3" />
                          {item.requester_email || item.requester_id}
                          {' · '}
                          {intl.formatMessage({ id: `install.request.role.${item.requester_role}`, defaultMessage: item.requester_role })}
                          {item.requester_department && (
                            <span className="text-muted-foreground">
                              {' · '}{intl.formatMessage({ id: 'install.request.dept' })}{item.requester_department}
                            </span>
                          )}
                        </span>
                      </div>

                      <h3 className="mt-1.5 truncate font-semibold text-foreground">{item.title}</h3>
                      {item.description && (
                        <p className="mt-0.5 break-words text-sm text-muted-foreground">
                          {item.description}
                        </p>
                      )}

                      {/* Security scan verdict */}
                      <div className="mt-2 flex items-center gap-2 text-xs">
                        {passed ? (
                          <ShieldCheck className="h-3.5 w-3.5 text-success" />
                        ) : (
                          <ShieldAlert className="h-3.5 w-3.5 text-destructive" />
                        )}
                        <span className="text-muted-foreground">
                          {intl.formatMessage({ id: 'install.request.riskLabel' })}:
                        </span>
                        <Badge variant={riskBadge.variant} className={riskBadge.className}>{item.risk_level}</Badge>
                        <span className="text-muted-foreground">
                          {intl.formatMessage({ id: 'install.request.findingsCount' }, { count: item.scan?.length ?? 0 })}
                        </span>
                      </div>
                      {item.scan && item.scan.length > 0 && (
                        <ul className="mt-1.5 space-y-1">
                          {item.scan.slice(0, 5).map((f, i) => (
                            <li key={i} className="flex items-start gap-1.5 text-xs">
                              <AlertTriangle className={`mt-0.5 h-3 w-3 shrink-0 ${SEVERITY_COLOR[f.severity] ?? SEVERITY_COLOR.info}`} />
                              <span className="text-muted-foreground">
                                <span className={`font-semibold uppercase ${SEVERITY_COLOR[f.severity] ?? SEVERITY_COLOR.info}`}>{f.severity}</span>
                                {' · '}{f.description}
                              </span>
                            </li>
                          ))}
                        </ul>
                      )}

                      <div className="mt-2 flex items-center gap-1 text-xs text-muted-foreground">
                        <Clock className="h-3 w-3" />
                        {new Date(item.created_at).toLocaleString('zh-TW', {
                          month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
                        })}
                        {item.manager_by && (
                          <span className="ml-2">
                            {intl.formatMessage({ id: 'install.request.managerSigned' })}
                          </span>
                        )}
                      </div>
                    </div>
                  </div>

                  <div className="flex shrink-0 items-center gap-2">
                    <Button size="sm" variant="default" disabled={busy} onClick={() => decide(item, true)}>
                      {busy ? <Loader2 className="animate-spin" /> : <Check />}
                      {intl.formatMessage({ id: 'install.request.approve' })}
                    </Button>
                    <Button size="sm" variant="destructive" disabled={busy} onClick={() => decide(item, false)}>
                      <X />
                      {intl.formatMessage({ id: 'install.request.deny' })}
                    </Button>
                  </div>
                </div>
                </CardContent>
              </Card>
            );
          })}
          {items.length === 0 && (
            <Card>
              <CardContent>
                <Empty icon={ShieldCheck} title={intl.formatMessage({ id: 'install.request.empty' })} />
              </CardContent>
            </Card>
          )}
        </div>
      )}
    </section>
  );
}
