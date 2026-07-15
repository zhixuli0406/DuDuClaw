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
import { Page, PageHeader, Card, Section, Badge, EmptyState, Button, SkeletonList } from '@/components/ui';
import { InstallRequestsSection } from '@/components/approvals/InstallRequestsSection';

/** Per-kind icon + Badge tone. Unknown kinds fall back to a neutral question mark. */
const KIND_META: Record<
  string,
  { icon: React.ComponentType<{ className?: string }>; tone: 'neutral' | 'info' | 'warning' | 'accent' | 'danger' }
> = {
  browser_action: { icon: Globe, tone: 'info' },
  tool_call: { icon: Wrench, tone: 'neutral' },
  skill_activation: { icon: Sparkles, tone: 'accent' },
  strategic_plan: { icon: Target, tone: 'warning' },
  agent_hire: { icon: UserPlus, tone: 'accent' },
  wiki_ingest: { icon: BookOpen, tone: 'info' },
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
    <Page>
      <PageHeader
        icon={ClipboardCheck}
        title={intl.formatMessage({ id: 'approvals.title' })}
        subtitle={intl.formatMessage({ id: 'approvals.subtitle' })}
      />

      {/* Install approval requests (Skill / MCP) — two-stage signature chain */}
      <InstallRequestsSection />

      <Section title={intl.formatMessage({ id: 'approvals.section.other' })} className="mt-6">
      {loading ? (
        <Card padded={false}>
          <div className="p-5">
            <SkeletonList rows={3} rowClassName="h-16" />
          </div>
        </Card>
      ) : items.length === 0 ? (
        <Card>
          <EmptyState
            icon={ClipboardCheck}
            title={intl.formatMessage({ id: 'approvals.empty' })}
            hint={intl.formatMessage({ id: 'approvals.emptyHint' })}
          />
        </Card>
      ) : (
        <div className="space-y-3">
          {items.map((item) => {
            const meta = KIND_META[item.kind] ?? { icon: ShieldQuestion, tone: 'neutral' as const };
            const Icon = meta.icon;
            const busy = !!deciding[item.id];
            return (
              <Card key={item.id}>
                <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
                  <div className="flex min-w-0 items-start gap-3">
                    <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-stone-500/10 text-stone-500 dark:bg-white/5 dark:text-stone-400">
                      <Icon className="h-[1.125rem] w-[1.125rem]" />
                    </span>
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <Badge tone={meta.tone}>{kindLabel(intl, item.kind)}</Badge>
                        <span className="text-xs text-stone-500 dark:text-stone-400">
                          {intl.formatMessage({ id: 'approvals.requester' })}: {item.agent_id}
                        </span>
                      </div>
                      <p className="mt-1.5 break-words text-sm text-stone-800 dark:text-stone-100">
                        {item.summary}
                      </p>
                      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-stone-400 dark:text-stone-500">
                        <span className="flex items-center gap-1">
                          <Clock className="h-3 w-3" />
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
                      size="sm"
                      variant="primary"
                      icon={Check}
                      pending={busy}
                      disabled={busy}
                      onClick={() => decide(item, true)}
                    >
                      {intl.formatMessage({ id: 'approvals.approve' })}
                    </Button>
                    <Button
                      size="sm"
                      variant="danger"
                      icon={X}
                      disabled={busy}
                      onClick={() => decide(item, false)}
                    >
                      {intl.formatMessage({ id: 'approvals.deny' })}
                    </Button>
                  </div>
                </div>
              </Card>
            );
          })}
        </div>
      )}
      </Section>
    </Page>
  );
}
