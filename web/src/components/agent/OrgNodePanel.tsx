import { useIntl } from 'react-intl';
import { ArrowRight, Pause, Play } from 'lucide-react';
import type { AgentDetail } from '@/lib/api';
import { ActorAvatar, Badge, Button, CrossLink, type ActorStatus } from '@/components/mds';

/** Lifecycle → ActorAvatar availability dot. */
function actorStatus(status: string): ActorStatus {
  if (status === 'active') return 'online';
  if (status === 'paused') return 'busy';
  if (status === 'terminated') return 'error';
  return 'offline';
}

/**
 * OrgNodePanel — the small staff card that opens in the right PropertiesPanel
 * when an org-chart node is clicked (§5.4 T6.3). Avatar + status + a link into
 * the full detail page and rest/resume.
 *
 * X03 (UX audit §3.3): the only exit used to be "查看員工詳情" into
 * AgentDetailPage — actually editing the staff member took 3 hops (node →
 * detail page → kebab menu → edit). `onEdit` adds a direct CrossLink straight
 * into EditAgentPage, bringing it back under the ≤2-hop bar.
 */
export function OrgNodePanel({
  agent,
  onOpenDetail,
  onEdit,
  onPause,
  onResume,
}: {
  agent: AgentDetail;
  onOpenDetail: () => void;
  onEdit: () => void;
  onPause: () => void;
  onResume: () => void;
}) {
  const intl = useIntl();
  const active = agent.status === 'active';
  const title = agent.role
    ? intl.formatMessage({ id: `agents.role.${agent.role}` })
    : agent.name;

  return (
    <div className="space-y-4">
      <div className="flex flex-col items-center gap-2 text-center">
        <ActorAvatar
          actorType="agent"
          size="2xl"
          name={agent.display_name}
          src={agent.avatar ?? undefined}
          showStatusDot
          status={actorStatus(agent.status)}
          className="ring-1"
        />
        <div className="min-w-0">
          <h3 className="truncate text-sm font-medium text-foreground">
            {agent.display_name}
          </h3>
          <p className="truncate text-xs text-muted-foreground">{title}</p>
        </div>
        <Badge variant={active ? 'default' : 'secondary'}>
          {intl.formatMessage({ id: `status.${agent.status}` })}
        </Badge>
      </div>

      {/* Property rows (Card-style divide list). */}
      <div className="divide-y divide-surface-border rounded-lg border border-surface-border">
        <PropertyRow
          label={intl.formatMessage({ id: 'orgchart.detail.role' })}
          value={intl.formatMessage({ id: `agents.role.${agent.role}` })}
        />
        {agent.trigger && (
          <PropertyRow
            label={intl.formatMessage({ id: 'orgchart.detail.trigger' })}
            value={agent.trigger}
          />
        )}
        {agent.reports_to && (
          <PropertyRow
            label={intl.formatMessage({ id: 'orgchart.detail.reportsTo' })}
            value={agent.reports_to}
          />
        )}
        {agent.model?.preferred && (
          <PropertyRow
            label={intl.formatMessage({ id: 'orgchart.detail.model' })}
            value={agent.model.preferred}
          />
        )}
      </div>

      <div className="space-y-2">
        <Button variant="brand" size="sm" onClick={onOpenDetail} className="w-full">
          <ArrowRight />
          {intl.formatMessage({ id: 'orgchart.openDetail' })}
        </Button>
        <div className="flex justify-center">
          <CrossLink label={intl.formatMessage({ id: 'crosslink.orgChart.editAgent' })} onClick={onEdit} />
        </div>
        {active ? (
          <Button variant="outline" size="sm" onClick={onPause} className="w-full">
            <Pause />
            {intl.formatMessage({ id: 'agents.pause' })}
          </Button>
        ) : (
          <Button variant="outline" size="sm" onClick={onResume} className="w-full">
            <Play />
            {intl.formatMessage({ id: 'agents.resume' })}
          </Button>
        )}
      </div>
    </div>
  );
}

/** One label/value row, Multica PropertyRow convention (label muted, value foreground). */
function PropertyRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between gap-2 px-3 py-2 text-sm">
      <span className="shrink-0 text-muted-foreground">{label}</span>
      <span className="truncate font-medium text-foreground">{value}</span>
    </div>
  );
}
