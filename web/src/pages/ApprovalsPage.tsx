import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  ClipboardCheck,
  Globe,
  Wrench,
  Sparkles,
  Target,
  UserPlus,
  BookOpen,
  ShieldQuestion,
  Check,
  X,
  Clock,
} from 'lucide-react';
import { api, type ApprovalItem, type ApprovalKind } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useApprovalsStore } from '@/stores/approvals-store';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { Badge, Button, Card, CardContent, Empty, Skeleton, Spinner } from '@/components/mds';
import { InstallRequestsSection } from '@/components/approvals/InstallRequestsSection';

/** Per-kind icon + Badge tone-class. Unknown kinds fall back to a neutral mark. */
const KIND_META: Record<
  string,
  { icon: React.ComponentType<{ className?: string }>; badge: string }
> = {
  browser_action: { icon: Globe, badge: 'bg-info/15 text-info' },
  tool_call: { icon: Wrench, badge: '' },
  skill_activation: { icon: Sparkles, badge: 'bg-brand/15 text-brand' },
  strategic_plan: { icon: Target, badge: 'bg-warning/15 text-warning' },
  agent_hire: { icon: UserPlus, badge: 'bg-brand/15 text-brand' },
  wiki_ingest: { icon: BookOpen, badge: 'bg-info/15 text-info' },
};

function kindLabel(intl: ReturnType<typeof useIntl>, kind: ApprovalKind): string {
  const id = `approvals.kind.${kind}`;
  const fallback = intl.formatMessage({ id: 'approvals.kind.unknown' });
  // react-intl logs a missing-id warning and returns the id; guard by checking
  // against the message catalogue so unknown backend kinds show a clean label.
  const msg = intl.formatMessage({ id, defaultMessage: fallback });
  return msg === id ? fallback : msg;
}

function ttlLabel(intl: ReturnType<typeof useIntl>, seconds: number): string {
  if (seconds <= 0) return intl.formatMessage({ id: 'approvals.ttl.expired' });
  if (seconds >= 3600) {
    return intl.formatMessage({ id: 'approvals.ttl.hours' }, { hours: Math.floor(seconds / 3600) });
  }
  if (seconds >= 60) {
    return intl.formatMessage({ id: 'approvals.ttl.minutes' }, { minutes: Math.floor(seconds / 60) });
  }
  return intl.formatMessage({ id: 'approvals.ttl.seconds' }, { seconds });
}

export function ApprovalsPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const setPendingCount = useApprovalsStore((s) => s.setPendingCount);
  const [items, setItems] = useState<ApprovalItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [deciding, setDeciding] = useState<Record<string, boolean>>({});

  const load = useCallback(async () => {
    try {
      const res = await api.approvals.list();
      setItems(res?.approvals ?? []);
      setPendingCount(res?.count ?? res?.approvals?.length ?? 0);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setItems([]);
    } finally {
      setLoading(false);
    }
  }, [intl, setPendingCount]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    load();
  }, [connectionState, load]);

  const decide = useCallback(
    async (item: ApprovalItem, approve: boolean) => {
      setDeciding((prev) => ({ ...prev, [item.id]: true }));
      try {
        await api.approvals.decide(item.id, approve);
        toast.success(
          intl.formatMessage(
            { id: approve ? 'approvals.approvedToast' : 'approvals.deniedToast' },
            { summary: item.summary },
          ),
        );
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

  return (
    <div className="space-y-6">
      {/* Slim page header (spec §5.2). */}
      <div className="flex items-center gap-2">
        <ClipboardCheck className="size-5 text-muted-foreground" />
        <div>
          <h1 className="text-base font-medium">{intl.formatMessage({ id: 'approvals.title' })}</h1>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'approvals.subtitle' })}</p>
        </div>
      </div>

      {/* Install approval requests (Skill / MCP) — two-stage signature chain */}
      <InstallRequestsSection />

      <section className="space-y-3">
        <h2 className="text-base font-medium">{intl.formatMessage({ id: 'approvals.section.other' })}</h2>
        {loading ? (
          <div className="space-y-3">
            {Array.from({ length: 3 }).map((_, i) => (
              <Skeleton key={i} className="h-16 w-full rounded-xl" />
            ))}
          </div>
        ) : items.length === 0 ? (
          <Empty
            icon={ClipboardCheck}
            title={intl.formatMessage({ id: 'approvals.empty' })}
            description={intl.formatMessage({ id: 'approvals.emptyHint' })}
            variant="dashed"
          />
        ) : (
          <div className="space-y-3">
            {items.map((item) => {
              const meta = KIND_META[item.kind] ?? { icon: ShieldQuestion, badge: '' };
              const Icon = meta.icon;
              const busy = !!deciding[item.id];
              return (
                <Card key={item.id}>
                  <CardContent>
                    <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
                      <div className="flex min-w-0 items-start gap-3">
                        <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-muted text-muted-foreground">
                          <Icon className="size-[1.125rem]" />
                        </span>
                        <div className="min-w-0">
                          <div className="flex flex-wrap items-center gap-2">
                            <Badge variant="secondary" className={cn(meta.badge)}>
                              {kindLabel(intl, item.kind)}
                            </Badge>
                            <span className="text-xs text-muted-foreground">
                              {intl.formatMessage({ id: 'approvals.requester' })}: {item.agent_id}
                            </span>
                          </div>
                          <p className="mt-1.5 break-words text-sm text-foreground">{item.summary}</p>
                          <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                            <span className="flex items-center gap-1">
                              <Clock className="size-3" />
                              {new Date(item.created_at).toLocaleString('zh-TW', {
                                month: 'short',
                                day: 'numeric',
                                hour: '2-digit',
                                minute: '2-digit',
                              })}
                            </span>
                            <span>{ttlLabel(intl, item.ttl_seconds)}</span>
                          </div>
                        </div>
                      </div>

                      <div className="flex shrink-0 items-center gap-2">
                        <Button
                          variant="brand"
                          size="sm"
                          disabled={busy}
                          onClick={() => decide(item, true)}
                        >
                          {busy ? <Spinner className="size-3.5" /> : <Check />}
                          {intl.formatMessage({ id: 'approvals.approve' })}
                        </Button>
                        <Button
                          variant="destructive"
                          size="sm"
                          disabled={busy}
                          onClick={() => decide(item, false)}
                        >
                          <X />
                          {intl.formatMessage({ id: 'approvals.deny' })}
                        </Button>
                      </div>
                    </div>
                  </CardContent>
                </Card>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}
