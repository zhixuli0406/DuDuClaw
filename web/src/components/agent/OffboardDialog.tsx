import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Archive, Trash2, ArrowRightLeft, AlertTriangle } from 'lucide-react';
import type { AgentDetail, AgentInfo } from '@/lib/api';
import { Dialog, FormField, inputClass } from '@/components/shared/Dialog';
import { Button } from '@/components/ui';
import { Switch } from '@/components/settings/controls';
import { useAgentsStore } from '@/stores/agents-store';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';

type OffboardMode = 'archive' | 'handoff' | 'remove';

/** A labelled toggle row — the Switch itself only carries an aria-label. */
function SwitchRow({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="flex items-center justify-between gap-3 text-sm text-stone-700 dark:text-stone-200">
      <span>{label}</span>
      <Switch checked={checked} onChange={onChange} label={label} />
    </label>
  );
}

/**
 * OffboardDialog (WP4) — the single "讓 AI 員工離職" flow, phrased for an
 * end-user audience. Three explicit choices instead of one destructive button:
 *
 *  • 封存 (archive)  — keep everything, restore any time.
 *  • 移除 (remove)    — hide from view; data still retained (type-to-confirm).
 *  • 交接後封存 (handoff) — move memory / knowledge / tasks to a colleague, then
 *                          archive. A PARTIAL result surfaces every error as-is.
 */
export function OffboardDialog({
  open,
  agent,
  candidates,
  busy,
  onClose,
  onDone,
}: {
  open: boolean;
  agent: AgentDetail;
  /** Possible handoff targets (exclude self + archived). */
  candidates: ReadonlyArray<AgentInfo>;
  busy?: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const intl = useIntl();
  const { archiveAgent, removeAgent, handoffAgent } = useAgentsStore();

  const [mode, setMode] = useState<OffboardMode>('archive');
  const [target, setTarget] = useState('');
  const [moveMemory, setMoveMemory] = useState(true);
  const [moveWiki, setMoveWiki] = useState(true);
  const [moveTasks, setMoveTasks] = useState(true);
  const [typed, setTyped] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [errors, setErrors] = useState<string[] | null>(null);

  // Reset when (re)opened.
  useEffect(() => {
    if (open) {
      setMode('archive');
      setTarget(candidates[0]?.name ?? '');
      setMoveMemory(true);
      setMoveWiki(true);
      setMoveTasks(true);
      setTyped('');
      setErrors(null);
      setSubmitting(false);
    }
  }, [open, candidates]);

  const busyAll = busy || submitting;
  const removeReady = mode !== 'remove' || typed.trim() === agent.display_name.trim();
  const handoffReady = mode !== 'handoff' || target !== '';
  const canSubmit = !busyAll && removeReady && handoffReady;

  const handleSubmit = async () => {
    setErrors(null);
    setSubmitting(true);
    try {
      if (mode === 'archive') {
        await archiveAgent(agent.name);
        toast.success(intl.formatMessage({ id: 'agents.archive.done' }, { name: agent.display_name }));
        onDone();
      } else if (mode === 'remove') {
        await removeAgent(agent.name);
        toast.success(intl.formatMessage({ id: 'agents.remove.done' }, { name: agent.display_name }));
        onDone();
      } else {
        const res = await handoffAgent({
          from_agent: agent.name,
          to_agent: target,
          memory: moveMemory,
          wiki: moveWiki,
          tasks: moveTasks,
          auto_archive: true,
        });
        if (res.status === 'PARTIAL') {
          // Surface every failure verbatim — never report a partial as success.
          setErrors(res.errors ?? [intl.formatMessage({ id: 'agents.handoff.partialGeneric' })]);
          return;
        }
        const toName = candidates.find((c) => c.name === target)?.display_name ?? target;
        toast.success(intl.formatMessage({ id: 'agents.handoff.done' }, { name: agent.display_name, target: toName }));
        onDone();
      }
    } catch (e) {
      setErrors([formatError(e)]);
    } finally {
      setSubmitting(false);
    }
  };

  const options: ReadonlyArray<{ id: OffboardMode; icon: typeof Archive; disabled?: boolean }> = [
    { id: 'archive', icon: Archive },
    { id: 'remove', icon: Trash2 },
    { id: 'handoff', icon: ArrowRightLeft, disabled: candidates.length === 0 },
  ];

  const confirmLabel =
    mode === 'archive'
      ? intl.formatMessage({ id: 'agents.offboard.archive.cta' })
      : mode === 'remove'
        ? intl.formatMessage({ id: 'agents.offboard.remove.cta' })
        : intl.formatMessage({ id: 'agents.offboard.handoff.cta' });

  return (
    <Dialog open={open} onClose={onClose} title={intl.formatMessage({ id: 'agents.offboard.title' }, { name: agent.display_name })}>
      <div className="space-y-4">
        <p className="text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'agents.offboard.intro' }, { name: agent.display_name })}
        </p>

        {/* Mode picker */}
        <div className="space-y-2" role="radiogroup" aria-label={intl.formatMessage({ id: 'agents.offboard.title' }, { name: agent.display_name })}>
          {options.map((o) => {
            const Icon = o.icon;
            const active = mode === o.id;
            return (
              <button
                key={o.id}
                type="button"
                role="radio"
                aria-checked={active}
                disabled={o.disabled}
                onClick={() => { setMode(o.id); setErrors(null); }}
                className={cn(
                  'flex w-full items-start gap-3 rounded-control border p-3 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50',
                  active
                    ? 'border-amber-500/60 bg-amber-500/10'
                    : 'border-[var(--panel-border)] hover:bg-stone-500/5 dark:hover:bg-white/5',
                  o.disabled && 'cursor-not-allowed opacity-50',
                )}
              >
                <span className={cn('mt-0.5 grid h-8 w-8 shrink-0 place-items-center rounded-lg', active ? 'bg-amber-500/15 text-amber-600 dark:text-amber-400' : 'bg-stone-500/10 text-stone-500')}>
                  <Icon className="h-4 w-4" />
                </span>
                <span className="min-w-0">
                  <span className="block text-sm font-medium text-stone-800 dark:text-stone-100">
                    {intl.formatMessage({ id: `agents.offboard.${o.id}.title` })}
                  </span>
                  <span className="mt-0.5 block text-xs text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: `agents.offboard.${o.id}.desc` })}
                  </span>
                </span>
              </button>
            );
          })}
        </div>

        {/* Handoff options */}
        {mode === 'handoff' && candidates.length > 0 && (
          <div className="space-y-3 rounded-control bg-stone-500/5 p-3 dark:bg-white/5">
            <FormField label={intl.formatMessage({ id: 'agents.offboard.handoff.target' })}>
              <select value={target} onChange={(e) => setTarget(e.target.value)} className={inputClass}>
                {candidates.map((c) => (
                  <option key={c.name} value={c.name}>{c.display_name}</option>
                ))}
              </select>
            </FormField>
            <div className="space-y-2">
              <SwitchRow label={intl.formatMessage({ id: 'agents.offboard.handoff.memory' })} checked={moveMemory} onChange={setMoveMemory} />
              <SwitchRow label={intl.formatMessage({ id: 'agents.offboard.handoff.wiki' })} checked={moveWiki} onChange={setMoveWiki} />
              <SwitchRow label={intl.formatMessage({ id: 'agents.offboard.handoff.tasks' })} checked={moveTasks} onChange={setMoveTasks} />
            </div>
          </div>
        )}

        {/* Remove type-to-confirm */}
        {mode === 'remove' && (
          <FormField
            label={intl.formatMessage({ id: 'agents.offboard.remove.confirmLabel' })}
            hint={intl.formatMessage({ id: 'agents.offboard.remove.confirmHint' }, { name: agent.display_name })}
          >
            <input
              type="text"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              placeholder={agent.display_name}
              className={inputClass}
            />
          </FormField>
        )}

        {/* PARTIAL / error surface — honest, verbatim */}
        {errors && errors.length > 0 && (
          <div className="space-y-1.5 rounded-lg border border-rose-300 bg-rose-50 p-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300">
            <p className="flex items-center gap-1.5 font-medium">
              <AlertTriangle className="h-4 w-4 shrink-0" />
              {intl.formatMessage({ id: 'agents.handoff.partialTitle' })}
            </p>
            <ul className="ml-1 list-disc space-y-0.5 pl-4 text-xs">
              {errors.map((err, i) => (
                <li key={i} className="break-words">{err}</li>
              ))}
            </ul>
          </div>
        )}

        <div className="flex justify-end gap-2 pt-1">
          <Button variant="secondary" onClick={onClose} disabled={submitting}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant={mode === 'archive' ? 'primary' : 'danger'} onClick={handleSubmit} disabled={!canSubmit}>
            {submitting ? intl.formatMessage({ id: 'common.saving' }) : confirmLabel}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
