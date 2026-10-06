import { useIntl } from 'react-intl';
import type { ApprovalItem } from '@/lib/api';

/**
 * What an Admin accepts on a workflow activation card: each effect and the
 * one record it may change, who submitted it, and that only an Admin in the
 * dashboard may decide it. Server fields are read defensively; anything
 * missing renders nothing rather than a guess.
 */
export function WorkflowActivationFacts({ approval }: { approval: ApprovalItem }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const facts = approval.workflow_activation ?? null;
  const targets = Array.isArray(facts?.effect_targets) ? facts.effect_targets : [];
  return (
    <section aria-label={t('approval.activation.title')} className="space-y-2 rounded-lg border border-border p-3 text-sm">
      <p className="font-medium">{t('approval.activation.title')}</p>
      <p className="text-xs text-muted-foreground">{t('approval.activation.adminOnly')}</p>
      {approval.submitter_is_viewer && (
        <p className="rounded-md bg-warning/10 px-2 py-1 text-xs" role="note">{t('approval.activation.selfApproval')}</p>
      )}
      {approval.may_decide === false && (
        <p className="text-xs text-muted-foreground">{t('approval.activation.cannotDecide')}</p>
      )}
      {typeof facts?.expires_at === 'string' && facts.expires_at && (
        <p className="text-xs">{intl.formatMessage({ id: 'approval.activation.expiresAt' }, { at: facts.expires_at })}</p>
      )}
      {facts?.pricing?.money_limits_effective === false && (
        <p className="text-xs text-muted-foreground" role="note">{t('approval.activation.countLimitsOnly')}</p>
      )}
      {targets.length > 0 && (
        <div className="space-y-1">
          <p className="text-xs text-muted-foreground">{t('approval.activation.targets')}</p>
          <ul className="space-y-1">
            {targets.map((target) => (
              <li key={`${target.step_id}:${target.tool}`} className="break-all text-xs">
                <span className="font-medium">{target.step_id}</span>
                {' · '}
                {target.target
                  ? <span className="font-mono">{target.target}</span>
                  : <span className="text-destructive">{t('approval.activation.targetMissing')}</span>}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
