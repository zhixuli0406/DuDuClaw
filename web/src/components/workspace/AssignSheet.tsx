import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { ChevronRight } from 'lucide-react';

import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { useAgentsStore } from '@/stores/agents-store';
import { useAssignStore, type AssignMode } from '@/stores/assign-store';
import {
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Segmented,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Textarea,
} from '@/components/mds';

import { PromptBar } from './PromptBar';
import { AgentModelPicker } from './AgentModelPicker';
import { ConnectorChips } from './ConnectorChips';

/**
 * AssignSheet — the one 交辦 panel (UX plan I-1a/I-1b).
 *
 * Every "give an AI employee something to do" entry point in the app opens
 * this: the sidebar quick action (Personal included — the `!isPersonal` gate
 * that hid it is gone), the mobile centre ＋, the task board's primary action,
 * `/goals` 交辦任務, and the agent card menu. Mounted once in `MainLayout` and
 * driven by `assign-store`.
 *
 * Two modes, both existing paths — nothing new was invented:
 * - 問一問 (`ask`)  → `/chat` with the draft carried over. A conversation; no
 *   task is filed and no loop starts.
 * - 交辦   (`assign`) → `tasks.goal_create`, i.e. the autonomous goal loop with
 *   the MAV acceptance judge. This is the default.
 *
 * The four-element guide (目標／輸入／輸出格式／約束含完成標準) is a collapsed
 * hint block plus placeholders, never a required form — a non-technical user
 * can type one sentence and press 交辦.
 *
 * **`output_format` has no backend field and this deliberately does not add
 * one.** `tasks.goal_create` takes `acceptance_criteria`, freezes it into
 * `acceptance_criteria_baseline` and hands that to the judge, so the chosen
 * format rides along as one extra criteria line. That reaches the judge's
 * completeness aspect through the existing contract. (`outcome` is a different
 * thing — a machine-checkable JSON-schema / file-glob spec, not a human
 * "what shape do you want it in".)
 */

/** Human-facing output shapes. Values are i18n key suffixes only. */
const OUTPUT_FORMATS = ['unspecified', 'reply', 'list', 'table', 'document', 'slides'] as const;
type OutputFormat = (typeof OUTPUT_FORMATS)[number];

export function AssignSheet() {
  const intl = useIntl();
  const navigate = useNavigate();
  const open = useAssignStore((s) => s.open);
  const prefill = useAssignStore((s) => s.prefill);
  const closeAssign = useAssignStore((s) => s.closeAssign);

  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const loaded = useAgentsStore((s) => s.loaded);

  const [mode, setMode] = useState<AssignMode>('assign');
  const [agentId, setAgentId] = useState('');
  const [description, setDescription] = useState('');
  const [criteria, setCriteria] = useState('');
  const [outputFormat, setOutputFormat] = useState<OutputFormat>('unspecified');
  const [priority, setPriority] = useState('medium');
  const [durationHours, setDurationHours] = useState('');
  const [riskBoundary, setRiskBoundary] = useState('');
  const [requireBeliefs, setRequireBeliefs] = useState(false);
  const [showMore, setShowMore] = useState(false);
  const [showGuide, setShowGuide] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (open && !loaded) fetchAgents();
  }, [open, loaded, fetchAgents]);

  const defaultAgentId = useMemo(
    () => agents.find((a) => a.role === 'main')?.name ?? agents[0]?.name ?? '',
    [agents],
  );

  // Reset to the seeds each time the panel opens — a stale draft from a
  // previous, abandoned assignment must never be submitted by accident.
  useEffect(() => {
    if (!open) return;
    setMode(prefill?.mode ?? 'assign');
    setAgentId(prefill?.agentId ?? defaultAgentId);
    setDescription(prefill?.description ?? '');
    setCriteria(prefill?.acceptanceCriteria ?? '');
    setOutputFormat('unspecified');
    setPriority('medium');
    setDurationHours('');
    setRiskBoundary('');
    setRequireBeliefs(false);
    setShowMore(false);
    setShowGuide(false);
  }, [open, prefill, defaultAgentId]);

  // The roster can arrive after the panel opened; adopt the default then.
  // Functional update on purpose: this effect and the reset above run in the
  // same commit, so a plain `if (!agentId)` would read the pre-reset value and
  // stomp a prefilled employee with the default.
  useEffect(() => {
    if (!open || !defaultAgentId) return;
    setAgentId((prev) => prev || defaultAgentId);
  }, [open, defaultAgentId]);

  const goalText = description.trim();
  const canSubmit = goalText.length > 0 && (mode === 'ask' || agentId.length > 0);

  /**
   * Acceptance criteria actually sent. The output format is appended as one
   * extra line rather than a new backend field (see the module doc). When the
   * user picked a format but wrote no criteria we still have to send something,
   * so we send the goal text — which is exactly what the gateway would have
   * defaulted to anyway — plus the format line.
   */
  const composeCriteria = (): string | undefined => {
    const base = criteria.trim();
    if (outputFormat === 'unspecified') return base || undefined;
    const formatLabel = intl.formatMessage({ id: `assign.format.${outputFormat}` });
    const line = intl.formatMessage({ id: 'assign.outputFormatLine' }, { format: formatLabel });
    return [base || goalText, line].join('\n');
  };

  const submitAsk = () => {
    // `/chat` owns the conversation; hand it the draft through router state
    // (never the URL — a goal paragraph does not belong in a query string).
    const to = agentId ? `/chat?agent=${encodeURIComponent(agentId)}` : '/chat';
    closeAssign();
    navigate(to, { state: { draft: goalText } });
  };

  const submitAssign = async () => {
    const trimmedDuration = durationHours.trim();
    const parsedDuration = trimmedDuration ? Number(trimmedDuration) : undefined;
    if (
      parsedDuration !== undefined &&
      (!Number.isFinite(parsedDuration) || parsedDuration < 1 || parsedDuration > 720)
    ) {
      toast.error(intl.formatMessage({ id: 'goals.create.durationInvalid' }));
      return;
    }
    setSubmitting(true);
    try {
      const result = await api.tasks.goalCreate({
        agent_id: agentId,
        description: goalText,
        acceptance_criteria: composeCriteria(),
        priority: priority as 'low' | 'medium' | 'high' | 'urgent',
        duration_hours: parsedDuration,
        risk_boundary: riskBoundary.trim() || undefined,
        require_beliefs: requireBeliefs || undefined,
      });
      toast.success(
        intl.formatMessage({ id: 'goals.create.success' }, { cap: result.iteration_cap }),
      );
      if (!result.dispatch_enabled) {
        toast.error(intl.formatMessage({ id: 'goals.create.dispatchOff' }));
      }
      closeAssign();
      // Land on the task that was just created, not on a generic list.
      navigate(`/goals?task=${encodeURIComponent(result.task.id)}`);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setSubmitting(false);
    }
  };

  const handleSubmit = () => {
    if (!canSubmit || submitting) return;
    if (mode === 'ask') submitAsk();
    else void submitAssign();
  };

  const submitLabel = intl.formatMessage({
    id: mode === 'ask' ? 'assign.submitAsk' : 'assign.submit',
  });

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) closeAssign(); }}>
      <DialogContent className="max-h-[88vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'assign.title' })}</DialogTitle>
          <DialogDescription>{intl.formatMessage({ id: 'assign.subtitle' })}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          {/* Mode — the two existing execution paths, named from the user's side. */}
          <div className="space-y-1.5">
            <span className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'assign.mode' })}
            </span>
            <div className="flex flex-wrap items-center gap-2">
              <Segmented
                value={mode}
                onValueChange={(v) => setMode(v)}
                aria-label={intl.formatMessage({ id: 'assign.mode' })}
                options={[
                  { value: 'ask', label: intl.formatMessage({ id: 'assign.mode.ask' }) },
                  { value: 'assign', label: intl.formatMessage({ id: 'assign.mode.assign' }) },
                ]}
              />
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: `assign.mode.${mode}.hint` })}
              </p>
            </div>
          </div>

          {/* 目標 — free text first, everything else optional. */}
          <PromptBar
            id="assign-goal"
            value={description}
            onChange={setDescription}
            onSubmit={handleSubmit}
            showSubmit={false}
            label={intl.formatMessage({ id: 'assign.goal' })}
            placeholder={intl.formatMessage({ id: 'assign.goalPlaceholder' })}
            disabled={submitting}
            controls={
              <>
                <AgentModelPicker value={agentId} onChange={setAgentId} />
                <ConnectorChips
                  labelId="assign.dataScope"
                  onNavigate={(to) => {
                    closeAssign();
                    navigate(to);
                  }}
                />
              </>
            }
          />

          {/* 四要素引導 — a hint block, never a required form (WorkBuddy B5). */}
          <div>
            <button
              type="button"
              onClick={() => setShowGuide((v) => !v)}
              aria-expanded={showGuide}
              className="flex items-center gap-1 text-xs text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
            >
              <ChevronRight className={`size-3.5 transition-transform ${showGuide ? 'rotate-90' : ''}`} />
              {intl.formatMessage({ id: 'assign.guide' })}
            </button>
            {showGuide && (
              <ul className="mt-2 space-y-1 rounded-lg bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
                {(['goal', 'input', 'format', 'constraint'] as const).map((k) => (
                  <li key={k}>{intl.formatMessage({ id: `assign.guide.${k}` })}</li>
                ))}
              </ul>
            )}
          </div>

          {mode === 'assign' && (
            <>
              {/* 約束／完成標準 */}
              <div className="space-y-1.5">
                <label htmlFor="assign-criteria" className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage({ id: 'assign.criteria' })}
                </label>
                <Textarea
                  id="assign-criteria"
                  value={criteria}
                  rows={2}
                  placeholder={intl.formatMessage({ id: 'assign.criteriaPlaceholder' })}
                  onChange={(e) => setCriteria(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'assign.criteriaHint' })}
                </p>
              </div>

              {/* 輸出格式 */}
              <div className="space-y-1.5">
                <span className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage({ id: 'assign.outputFormat' })}
                </span>
                <Select value={outputFormat} onValueChange={(v) => setOutputFormat(v as OutputFormat)}>
                  <SelectTrigger
                    className="w-full sm:w-56"
                    aria-label={intl.formatMessage({ id: 'assign.outputFormat' })}
                  >
                    <SelectValue>
                      {intl.formatMessage({ id: `assign.format.${outputFormat}` })}
                    </SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {OUTPUT_FORMATS.map((f) => (
                      <SelectItem key={f} value={f}>
                        {intl.formatMessage({ id: `assign.format.${f}` })}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              {/* 更多設定 — everything a first-timer should not have to meet. */}
              <div>
                <button
                  type="button"
                  onClick={() => setShowMore((v) => !v)}
                  aria-expanded={showMore}
                  className="flex items-center gap-1 text-xs text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
                >
                  <ChevronRight className={`size-3.5 transition-transform ${showMore ? 'rotate-90' : ''}`} />
                  {intl.formatMessage({ id: 'assign.more' })}
                </button>

                {showMore && (
                  <div className="mt-3 space-y-4 rounded-lg border border-surface-border px-3 py-3">
                    <div className="space-y-1.5">
                      <span className="text-xs font-medium text-muted-foreground">
                        {intl.formatMessage({ id: 'goals.create.priority' })}
                      </span>
                      <Select value={priority} onValueChange={(v) => setPriority(String(v))}>
                        <SelectTrigger
                          className="w-full sm:w-56"
                          aria-label={intl.formatMessage({ id: 'goals.create.priority' })}
                        >
                          <SelectValue>
                            {intl.formatMessage({ id: `tasks.priority.${priority}` })}
                          </SelectValue>
                        </SelectTrigger>
                        <SelectContent>
                          {['low', 'medium', 'high', 'urgent'].map((p) => (
                            <SelectItem key={p} value={p}>
                              {intl.formatMessage({ id: `tasks.priority.${p}` })}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>

                    <div className="space-y-1.5">
                      <label htmlFor="assign-duration" className="text-xs font-medium text-muted-foreground">
                        {intl.formatMessage({ id: 'goals.create.duration' })}
                      </label>
                      <Input
                        id="assign-duration"
                        type="number"
                        min={1}
                        max={720}
                        value={durationHours}
                        placeholder={intl.formatMessage({ id: 'goals.create.durationPlaceholder' })}
                        onChange={(e) => setDurationHours(e.target.value)}
                      />
                    </div>

                    <div className="space-y-1.5">
                      <label htmlFor="assign-risk" className="text-xs font-medium text-muted-foreground">
                        {intl.formatMessage({ id: 'goals.create.riskBoundary' })}
                      </label>
                      <Textarea
                        id="assign-risk"
                        value={riskBoundary}
                        rows={3}
                        placeholder={intl.formatMessage({ id: 'goals.create.riskBoundaryPlaceholder' })}
                        onChange={(e) => setRiskBoundary(e.target.value)}
                      />
                      <p className="text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'goals.create.riskBoundaryHint' })}
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <label className="flex items-center gap-2 text-sm font-medium text-foreground">
                        <Checkbox
                          checked={requireBeliefs}
                          onCheckedChange={(v) => setRequireBeliefs(v === true)}
                        />
                        {intl.formatMessage({ id: 'goals.create.requireBeliefs' })}
                      </label>
                      <p className="text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'goals.create.requireBeliefsHint' })}
                      </p>
                    </div>
                  </div>
                )}
              </div>
            </>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={closeAssign}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="brand" onClick={handleSubmit} disabled={!canSubmit || submitting}>
            {submitting ? intl.formatMessage({ id: 'assign.submitting' }) : submitLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
