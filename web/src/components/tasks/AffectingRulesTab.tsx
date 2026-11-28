import { Fragment, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { api, type AffectingAutopilotRule } from '@/lib/api';
import { type AutopilotCondition, CONDITION_OPS, EVENT_FIELD_LABELS, eventFieldLabel } from '@/lib/autopilot-rules';
import { describeHistoryResult, runReasonMessageId } from '@/lib/autopilot-history';
import { formatDateTime, useCompanyTimeZone } from '@/lib/company-time';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { Badge, Button, CollectionPageState } from '@/components/mds';
import { useActionLabel, useTriggerLabel } from '@/components/settings/sections/AutopilotRuleDialog';

/** Render the condition tree without serializing arbitrary server objects or identifiers. */
function conditionSummary(condition: AutopilotCondition, t: (id: string) => string, agentName: (id: string) => string): string {
  if (condition == null || Object.keys(condition).length === 0) return '—';
  const tree = condition as Record<string, unknown>;
  for (const group of ['all', 'any'] as const) {
    if (Array.isArray(tree[group])) {
      const parts = (tree[group] as AutopilotCondition[]).map(c => conditionSummary(c, t, agentName));
      return parts.length ? `(${parts.join(` ${t(`routines.rules.${group}`)} `)})` : '—';
    }
  }
  const { field, op, value } = tree;
  if (typeof field !== 'string' || !EVENT_FIELD_LABELS[field] || typeof op !== 'string' || !CONDITION_OPS.some(o => o === op)) return t('routines.rules.customCondition');
  const fieldKind = EVENT_FIELD_LABELS[field];
  // Identifier and channel conditions are described without disclosing their values.
  if (['taskId', 'creator', 'recordId', 'eventId', 'channel', 'server'].includes(fieldKind)) return t('routines.rules.customCondition');
  const display = (v: unknown): string => {
    if (fieldKind === 'agent' && typeof v === 'string') return agentName(v);
    if (typeof v === 'string' || typeof v === 'number') return String(v);
    if (typeof v === 'boolean') return t(`routines.rules.${v ? 'yes' : 'no'}`);
    return t('routines.rules.customCondition');
  };
  return `${eventFieldLabel(field, t)} ${t(`autopilot.op.${op}`)} ${Array.isArray(value) ? value.map(display).join('、') : display(value)}`;
}

export function AffectingRulesTab() {
  const intl = useIntl();
  useCompanyTimeZone();
  const t = (id: string) => intl.formatMessage({ id });
  const triggerLabel = useTriggerLabel();
  const actionLabel = useActionLabel();
  const agents = useAgentsStore(s => s.agents);
  const fetchAgents = useAgentsStore(s => s.fetchAgents);
  const connection = useConnectionStore(s => s.state);
  const [rules, setRules] = useState<AffectingAutopilotRule[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [retry, setRetry] = useState(0);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  useEffect(() => {
    let active = true;
    setRules([]);
    setLoading(true);
    setFailed(false);
    if (connection !== 'authenticated') return;
    void fetchAgents();
    void api.autopilot.affectingMe().then(result => {
      if (active) setRules(result.rules ?? []);
    }).catch(() => {
      if (active) setFailed(true);
    }).finally(() => {
      if (active) setLoading(false);
    });
    return () => { active = false; };
  }, [connection, retry, fetchAgents]);
  const agentName = (id: string) => agents.find(a => a.name === id)?.display_name || t('routines.rules.unavailableAgent');
  const historyText = (entry: AffectingAutopilotRule['history'][number]) => {
    const result = describeHistoryResult(entry.result);
    return `${formatDateTime(entry.triggered_at, { locale: intl.locale })} · ${t(result.messageId ?? runReasonMessageId(entry.result))}`;
  };
  if (loading) return <CollectionPageState state="loading" />;
  if (failed) return <div role="alert" className="space-y-3 text-sm">
    <p>{t('routines.rules.loadFailed')}</p>
    <Button variant="outline" size="sm" onClick={() => setRetry(n => n + 1)}>{t('routines.rules.retry')}</Button>
  </div>;
  if (!rules.length) return <CollectionPageState state="empty" title={t('routines.rules.empty')} />;
  const columns = ['name', 'enabled', 'trigger', 'conditions', 'actions', 'targets', 'approval', 'department', 'latest'];
  return <div className="overflow-x-auto rounded-xl border border-surface-border">
    <table className="w-full text-left text-sm">
      <thead className="bg-surface"><tr>{columns.map(c => <th key={c} scope="col" className="whitespace-nowrap px-4 py-3 font-medium text-muted-foreground">{t(`routines.rules.col.${c}`)}</th>)}</tr></thead>
      <tbody>{rules.map(rule => {
        const open = expanded.has(rule.id);
        const history = [...rule.history].sort((a, b) => Date.parse(b.triggered_at) - Date.parse(a.triggered_at)).slice(0, 20);
        return <Fragment key={rule.id}>
          <tr className="border-t border-surface-border [&>td]:px-4 [&>td]:py-3 [&>td]:align-top">
            <td><Button variant="ghost" size="sm" aria-expanded={open} onClick={() => setExpanded(previous => {
              const next = new Set(previous);
              if (open) next.delete(rule.id); else next.add(rule.id);
              return next;
            })}>{open ? <ChevronDown /> : <ChevronRight />}{rule.name}</Button></td>
            <td><Badge variant="secondary">{t(`routines.rules.${rule.enabled ? 'enabled' : 'disabled'}`)}</Badge></td>
            <td>{intl.messages[`autopilot.trigger.${rule.trigger_event}`] ? triggerLabel(rule.trigger_event) : t('routines.rules.otherTrigger')}</td>
            <td className="min-w-48 break-words">{conditionSummary(rule.conditions, t, agentName)}</td>
            <td>{[...new Set(rule.action_kinds)].map(kind => intl.messages[`autopilot.action.${kind}`] ? actionLabel(kind) : t('routines.rules.otherAction')).join('、')}</td>
            <td>{rule.target_agents.map(agentName).join('、')}</td>
            <td>{t(`routines.rules.${rule.require_approval ? 'yes' : 'no'}`)}</td>
            <td>{rule.owner_department ?? t('routines.rules.company')}</td>
            <td className="min-w-48">{history[0] ? historyText(history[0]) : '—'}</td>
          </tr>
          {open && <tr className="border-t border-surface-border bg-surface"><td colSpan={9} className="space-y-2 px-6 py-4">
            <h3 className="text-sm font-medium">{t('routines.rules.history')}</h3>
            {history.length ? <ul className="space-y-1 text-sm text-muted-foreground">{history.map((entry, i) => <li key={i}>{historyText(entry)}</li>)}</ul> : <p className="text-sm text-muted-foreground">{t('autopilot.history.empty')}</p>}
          </td></tr>}
        </Fragment>;
      })}</tbody>
    </table>
  </div>;
}
