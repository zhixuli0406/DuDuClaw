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
} from 'lucide-react';
import { api, type InstallRequestInfo } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { toast, formatError } from '@/lib/toast';
import { Card, Section, Badge, EmptyState, Button } from '@/components/ui';

const SEVERITY_COLOR: Record<string, string> = {
  critical: 'text-rose-500',
  error: 'text-orange-500',
  high: 'text-orange-500',
  warning: 'text-amber-500',
  medium: 'text-amber-500',
  info: 'text-stone-400',
  low: 'text-stone-400',
};

const RISK_TONE: Record<string, 'success' | 'warning' | 'danger' | 'neutral'> = {
  Clean: 'success',
  Low: 'success',
  Medium: 'warning',
  High: 'danger',
  Critical: 'danger',
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
    <Section title={intl.formatMessage({ id: 'install.request.section' })}>
      {loading ? null : (
        <div className="space-y-3">
          {items.map((item) => {
            const busy = !!deciding[item.id];
            const KindIcon = item.kind === 'skill' ? Puzzle : Plug;
            const stageLabel = intl.formatMessage({ id: `install.request.stage.${item.stage}`, defaultMessage: item.stage });
            const riskTone = RISK_TONE[item.risk_level] ?? 'neutral';
            const passed = item.risk_level === 'Clean' || item.risk_level === 'Low' || item.risk_level === 'Medium';
            return (
              <Card key={item.id}>
                <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
                  <div className="flex min-w-0 items-start gap-3">
                    <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-amber-500/10 text-amber-600 dark:bg-amber-400/10 dark:text-amber-400">
                      <KindIcon className="h-[1.125rem] w-[1.125rem]" />
                    </span>
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <Badge tone="accent">
                          {intl.formatMessage({ id: `install.request.kind.${item.kind}` })}
                        </Badge>
                        <Badge tone="neutral">{stageLabel}</Badge>
                        <span className="flex items-center gap-1 text-xs text-stone-500 dark:text-stone-400">
                          <User className="h-3 w-3" />
                          {item.requester_email || item.requester_id}
                          {' · '}
                          {intl.formatMessage({ id: `install.request.role.${item.requester_role}`, defaultMessage: item.requester_role })}
                          {item.requester_department && (
                            <span className="text-stone-400 dark:text-stone-500">
                              {' · '}{intl.formatMessage({ id: 'install.request.dept' })}{item.requester_department}
                            </span>
                          )}
                        </span>
                      </div>

                      <h3 className="mt-1.5 truncate font-semibold text-stone-900 dark:text-stone-50">{item.title}</h3>
                      {item.description && (
                        <p className="mt-0.5 break-words text-sm text-stone-600 dark:text-stone-300">
                          {item.description}
                        </p>
                      )}

                      {/* Security scan verdict */}
                      <div className="mt-2 flex items-center gap-2 text-xs">
                        {passed ? (
                          <ShieldCheck className="h-3.5 w-3.5 text-emerald-500" />
                        ) : (
                          <ShieldAlert className="h-3.5 w-3.5 text-rose-500" />
                        )}
                        <span className="text-stone-500 dark:text-stone-400">
                          {intl.formatMessage({ id: 'install.request.riskLabel' })}:
                        </span>
                        <Badge tone={riskTone}>{item.risk_level}</Badge>
                        <span className="text-stone-400 dark:text-stone-500">
                          {intl.formatMessage({ id: 'install.request.findingsCount' }, { count: item.scan?.length ?? 0 })}
                        </span>
                      </div>
                      {item.scan && item.scan.length > 0 && (
                        <ul className="mt-1.5 space-y-1">
                          {item.scan.slice(0, 5).map((f, i) => (
                            <li key={i} className="flex items-start gap-1.5 text-xs">
                              <AlertTriangle className={`mt-0.5 h-3 w-3 shrink-0 ${SEVERITY_COLOR[f.severity] ?? SEVERITY_COLOR.info}`} />
                              <span className="text-stone-600 dark:text-stone-400">
                                <span className={`font-semibold uppercase ${SEVERITY_COLOR[f.severity] ?? SEVERITY_COLOR.info}`}>{f.severity}</span>
                                {' · '}{f.description}
                              </span>
                            </li>
                          ))}
                        </ul>
                      )}

                      <div className="mt-2 flex items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
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
                    <Button size="sm" variant="primary" icon={Check} pending={busy} disabled={busy} onClick={() => decide(item, true)}>
                      {intl.formatMessage({ id: 'install.request.approve' })}
                    </Button>
                    <Button size="sm" variant="danger" icon={X} disabled={busy} onClick={() => decide(item, false)}>
                      {intl.formatMessage({ id: 'install.request.deny' })}
                    </Button>
                  </div>
                </div>
              </Card>
            );
          })}
          {items.length === 0 && (
            <Card>
              <EmptyState icon={ShieldCheck} title={intl.formatMessage({ id: 'install.request.empty' })} />
            </Card>
          )}
        </div>
      )}
    </Section>
  );
}
