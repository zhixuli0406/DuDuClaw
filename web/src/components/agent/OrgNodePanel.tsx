import { useIntl } from 'react-intl';
import { ArrowRight, Pause, Play } from 'lucide-react';
import type { AgentDetail } from '@/lib/api';
import { Badge, Button, CharacterAvatar, agentPose, agentEmote } from '@/components/ui';
import type { AgentLifecycle } from '@/components/character';

/**
 * OrgNodePanel — the small staff card that opens in the right PropertiesPanel
 * when an org-chart node is clicked (§5.4 T6.3). Avatar + status + a link into
 * the full detail page and rest/resume.
 */
export function OrgNodePanel({
  agent,
  onOpenDetail,
  onPause,
  onResume,
}: {
  agent: AgentDetail;
  onOpenDetail: () => void;
  onPause: () => void;
  onResume: () => void;
}) {
  const intl = useIntl();
  const lifecycle = agent.status as AgentLifecycle;
  const title = agent.role
    ? intl.formatMessage({ id: `agents.role.${agent.role}` })
    : agent.name;

  return (
    <div className="space-y-4">
      <div className="flex flex-col items-center gap-2 text-center">
        <CharacterAvatar
          agentId={agent.name}
          name={agent.display_name}
          size={80}
          variant="bust"
          pose={agentPose(lifecycle)}
          emote={agentEmote(lifecycle)}
        />
        <div className="min-w-0">
          <h3 className="truncate text-sm font-semibold text-stone-900 dark:text-stone-50">
            {agent.display_name}
          </h3>
          <p className="truncate text-xs text-stone-500 dark:text-stone-400">{title}</p>
        </div>
        <Badge tone={lifecycle === 'active' ? 'success' : lifecycle === 'paused' ? 'warning' : 'danger'} dot>
          {intl.formatMessage({ id: `status.${agent.status}` })}
        </Badge>
      </div>

      <div className="space-y-2">
        <Button variant="primary" icon={ArrowRight} onClick={onOpenDetail} className="w-full justify-center">
          {intl.formatMessage({ id: 'orgchart.openDetail' })}
        </Button>
        {lifecycle === 'active' ? (
          <Button variant="secondary" icon={Pause} onClick={onPause} className="w-full justify-center">
            {intl.formatMessage({ id: 'agents.pause' })}
          </Button>
        ) : (
          <Button variant="secondary" icon={Play} onClick={onResume} className="w-full justify-center">
            {intl.formatMessage({ id: 'agents.resume' })}
          </Button>
        )}
      </div>
    </div>
  );
}
