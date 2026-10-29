import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { ChevronRight } from 'lucide-react';

import { api } from '@/lib/api';
import type { AssignMode, AssignPrefill } from '@/lib/assign-entry';
import { WHEN_NOW, scheduleErrorText, toScheduleInput, viewerTimezone, type WhenValue } from '@/lib/assign-schedule';
import { stashChatAttachments } from '@/lib/chat-handoff';
import { toast } from '@/lib/toast';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import {
  Checkbox,
  Input,
  Segmented,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Textarea,
} from '@/components/mds';
import { PromptBar } from '@/components/workspace/PromptBar';
import { AgentModelPicker } from '@/components/workspace/AgentModelPicker';
import { ConnectorChips } from '@/components/workspace/ConnectorChips';
import { ScheduleField } from '@/components/workspace/ScheduleField';

import { AssignFooter } from './AssignFooter';
import { AttachButton, AttachmentChips, toAttachmentInput, useTaskAttachments } from './TaskAttachments';
import { announceGoalCreated, assignErrorText, createdGoalPath } from './announce';

/**
 * 複雜任務 — the full 交辦 form (formerly the AssignSheet modal, UX plan
 * I-1a/I-1b), rendered as page sections of `/assign`.
 *
 * Three modes:
 * - 問一問 (`ask`)  → `/chat` with the draft (and any files) carried over. A
 *   conversation; no task is filed and no loop starts.
 * - 交辦   (`assign`) → `tasks.goal_create`, i.e. the autonomous goal loop with
 *   the MAV acceptance judge. This is the default.
 * - 想一想 (`plan`, I-1c) → `tasks.goal_create` with `plan_first: true`: the
 *   backend generates a plan and parks the task `needs_human` for approval.
 *
 * The four-element guide (目標／輸入／輸出格式／約束含完成標準) is a collapsed
 * hint block plus placeholders, never a required form.
 *
 * **`output_format` has no backend field and this deliberately does not add
 * one.** The chosen format rides along as one extra acceptance-criteria line,
 * which reaches the judge through the frozen criteria baseline.
 *
 * Files picked with 「附件」 go with the goal (saved into the chosen
 * employee's file area by the gateway) or, for 問一問, to `/chat` as pending
 * attachments.
 */

/** Human-facing output shapes. Values are i18n key suffixes only. */
const OUTPUT_FORMATS = ['unspecified', 'reply', 'list', 'table', 'document', 'slides'] as const;
type OutputFormat = (typeof OUTPUT_FORMATS)[number];

export function ComplexAssignTab({
  prefill,
  onCancel,
}: {
  /** Seed from the entry point; the parent remounts this per navigation. */
  prefill: AssignPrefill | null;
  onCancel: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();

  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const loaded = useAgentsStore((s) => s.loaded);

  const [mode, setMode] = useState<AssignMode>(prefill?.mode ?? 'assign');
  const [agentId, setAgentId] = useState(prefill?.agentId ?? '');
  const [description, setDescription] = useState(prefill?.description ?? '');
  const [criteria, setCriteria] = useState(prefill?.acceptanceCriteria ?? '');
  const [outputFormat, setOutputFormat] = useState<OutputFormat>('unspecified');
  const [priority, setPriority] = useState('medium');
  const [durationHours, setDurationHours] = useState('');
  const [costCap, setCostCap] = useState('');
  // Per-goal run limit, in minutes on the form (1-60), sent as seconds.
  const [runLimitMinutes, setRunLimitMinutes] = useState('');
  const [riskBoundary, setRiskBoundary] = useState('');
  // A per-goal risk boundary replaces the deployment baseline: Admin only
  // (the gateway refuses it from anyone else).
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [requireBeliefs, setRequireBeliefs] = useState(false);
  const [showMore, setShowMore] = useState(false);
  const [showGuide, setShowGuide] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  // When it runs: now (default), once later, or on a repeat.
  const [when, setWhen] = useState<WhenValue>(WHEN_NOW);
  const attach = useTaskAttachments();

  useEffect(() => {
    if (!loaded) fetchAgents();
  }, [loaded, fetchAgents]);

  const defaultAgentId = useMemo(
    () => agents.find((a) => a.role === 'main')?.name ?? agents[0]?.name ?? '',
    [agents],
  );

  // The roster can arrive after the page opened; adopt the default then,
  // never over an employee the entry point preselected.
  useEffect(() => {
    if (!defaultAgentId) return;
    setAgentId((prev) => prev || defaultAgentId);
  }, [defaultAgentId]);

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
    // (never the URL) and the files through a one-time key (`chat-handoff`).
    const to = agentId ? `/chat?agent=${encodeURIComponent(agentId)}` : '/chat';
    const state =
      attach.files.length > 0
        ? { draft: goalText, attachmentsKey: stashChatAttachments(attach.files) }
        : { draft: goalText };
    navigate(to, { state });
  };

  /** Backs both 交辦 (`assign`) and 想一想 (`plan`) — the only difference is
   *  `plan_first` (I-1c). Same request shape, same landing page. */
  const submitAssign = async (planFirst: boolean) => {
    const trimmedDuration = durationHours.trim();
    const parsedDuration = trimmedDuration ? Number(trimmedDuration) : undefined;
    if (
      parsedDuration !== undefined &&
      (!Number.isFinite(parsedDuration) || parsedDuration < 1 || parsedDuration > 720)
    ) {
      toast.error(intl.formatMessage({ id: 'goals.create.durationInvalid' }));
      return;
    }
    const parsedCap = costCap.trim() ? Number(costCap) : undefined;
    if (parsedCap !== undefined && (!Number.isSafeInteger(parsedCap) || parsedCap <= 0)) {
      toast.error(intl.formatMessage({ id: 'goals.create.costCapInvalid' }));
      return;
    }
    const parsedRunLimit = runLimitMinutes.trim() ? Number(runLimitMinutes) : undefined;
    if (
      parsedRunLimit !== undefined &&
      (!Number.isSafeInteger(parsedRunLimit) || parsedRunLimit < 1 || parsedRunLimit > 60)
    ) {
      toast.error(intl.formatMessage({ id: 'goals.create.runLimitInvalid' }));
      return;
    }
    let schedule;
    try {
      schedule = toScheduleInput(when);
    } catch {
      toast.error(intl.formatMessage({ id: 'assign.when.incomplete' }));
      return;
    }
    const common = {
      agent_id: agentId,
      description: goalText,
      acceptance_criteria: composeCriteria(),
      priority: priority as 'low' | 'medium' | 'high' | 'urgent',
      duration_hours: parsedDuration,
      cost_cap_cents: parsedCap,
      timeout_secs: parsedRunLimit !== undefined ? parsedRunLimit * 60 : undefined,
      risk_boundary: (isAdmin && riskBoundary.trim()) || undefined,
      require_beliefs: requireBeliefs || undefined,
      plan_first: planFirst || undefined,
      attachments: toAttachmentInput(attach.files),
    };
    setSubmitting(true);
    if (schedule) {
      try {
        await api.tasks.goalSchedule({ ...common, schedule, schedule_timezone: viewerTimezone() });
        toast.success(intl.formatMessage({ id: 'assign.when.scheduled' }));
        // The schedule is listed under 我交辦的.
        navigate('/goals?assigned=me');
      } catch (e) {
        toast.error(scheduleErrorText(intl, e));
      } finally {
        setSubmitting(false);
      }
      return;
    }
    try {
      const result = await api.tasks.goalCreate(common);
      announceGoalCreated(intl, result);
      // Land on the task that was just created, not on a generic list.
      navigate(createdGoalPath(result));
    } catch (e) {
      toast.error(assignErrorText(intl, e));
    } finally {
      setSubmitting(false);
    }
  };

  const handleSubmit = () => {
    if (!canSubmit || submitting) return;
    if (mode === 'ask') submitAsk();
    else void submitAssign(mode === 'plan');
  };

  const submitLabel = intl.formatMessage({
    id:
      mode === 'ask'
        ? 'assign.submitAsk'
        : when.kind !== 'now'
          ? 'assign.when.submit'
          : mode === 'plan'
            ? 'assign.submitPlan'
            : 'assign.submit',
  });

  return (
    <div>
      <div className="space-y-5">
        {/* Mode — the three execution paths, named from the user's side. */}
        <section className="space-y-1.5">
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
                { value: 'plan', label: intl.formatMessage({ id: 'assign.mode.plan' }) },
              ]}
            />
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: `assign.mode.${mode}.hint` })}
            </p>
          </div>
        </section>

        {/* 目標 — free text first, everything else optional. */}
        <section className="space-y-2">
          <PromptBar
            id="assign-goal"
            value={description}
            onChange={setDescription}
            onSubmit={handleSubmit}
            showSubmit={false}
            label={intl.formatMessage({ id: 'assign.goal' })}
            placeholder={intl.formatMessage({ id: 'assign.goalPlaceholder' })}
            disabled={submitting}
            attachments={
              attach.files.length > 0 ? (
                <AttachmentChips files={attach.files} onRemove={attach.remove} disabled={submitting} />
              ) : undefined
            }
            controls={
              <>
                <AgentModelPicker value={agentId} onChange={setAgentId} />
                <ConnectorChips labelId="assign.dataScope" onNavigate={(to) => navigate(to)} />
                <AttachButton onFiles={(f) => void attach.addFiles(f)} disabled={submitting} />
              </>
            }
          />
          <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'assign.attach.complexHint' })}
          </p>
        </section>

        {/* 四要素引導 — a hint block, never a required form. */}
        <section>
          <button
            type="button"
            onClick={() => setShowGuide((v) => !v)}
            aria-expanded={showGuide}
            className="flex min-h-[44px] items-center gap-1 text-xs text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 sm:min-h-0"
          >
            <ChevronRight className={`size-3.5 transition-transform motion-reduce:transition-none ${showGuide ? 'rotate-90' : ''}`} />
            {intl.formatMessage({ id: 'assign.guide' })}
          </button>
          {showGuide && (
            <ul className="mt-2 space-y-1 rounded-lg bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
              {(['goal', 'input', 'format', 'constraint'] as const).map((k) => (
                <li key={k}>{intl.formatMessage({ id: `assign.guide.${k}` })}</li>
              ))}
            </ul>
          )}
        </section>

        {/* 想一想 still creates a goal task — only 問一問 skips these fields. */}
        {mode !== 'ask' && (
          <>
            <ScheduleField value={when} onChange={setWhen} isAdmin={isAdmin} disabled={submitting} />

            {/* 約束／完成標準 */}
            <section className="space-y-1.5">
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
            </section>

            {/* 輸出格式 */}
            <section className="space-y-1.5">
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
            </section>

            {/* 更多設定 — everything a first-timer should not have to meet. */}
            <section>
              <button
                type="button"
                onClick={() => setShowMore((v) => !v)}
                aria-expanded={showMore}
                className="flex min-h-[44px] items-center gap-1 text-xs text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 sm:min-h-0"
              >
                <ChevronRight className={`size-3.5 transition-transform motion-reduce:transition-none ${showMore ? 'rotate-90' : ''}`} />
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
                    <label htmlFor="assign-cost-cap" className="text-xs font-medium text-muted-foreground">
                      {intl.formatMessage({ id: 'goals.create.costCap' })}
                    </label>
                    <Input
                      id="assign-cost-cap"
                      type="number"
                      min={1}
                      step={1}
                      value={costCap}
                      onChange={(e) => setCostCap(e.target.value)}
                    />
                    <p className="text-xs text-muted-foreground">
                      {intl.formatMessage({ id: 'goals.create.costCapHint' })}
                    </p>
                  </div>

                  <div className="space-y-1.5">
                    <label htmlFor="assign-run-limit" className="text-xs font-medium text-muted-foreground">
                      {intl.formatMessage({ id: 'goals.create.runLimit' })}
                    </label>
                    <Input
                      id="assign-run-limit"
                      type="number"
                      min={1}
                      max={60}
                      step={1}
                      value={runLimitMinutes}
                      placeholder={intl.formatMessage({ id: 'goals.create.runLimitPlaceholder' })}
                      onChange={(e) => setRunLimitMinutes(e.target.value)}
                    />
                    <p className="text-xs text-muted-foreground">
                      {intl.formatMessage({ id: isAdmin ? 'goals.create.runLimitHint' : 'goals.create.runLimitHintNonAdmin' })}
                    </p>
                  </div>

                  {isAdmin && (
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
                  )}

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
            </section>
          </>
        )}
      </div>

      <AssignFooter
        submitLabel={submitLabel}
        onCancel={onCancel}
        onSubmit={handleSubmit}
        canSubmit={canSubmit}
        submitting={submitting}
      />
    </div>
  );
}
