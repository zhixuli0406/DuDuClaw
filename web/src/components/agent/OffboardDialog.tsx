import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Archive, Trash2, ArrowRightLeft, AlertTriangle } from 'lucide-react';
import type { AgentDetail, AgentInfo } from '@/lib/api';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter, Input, Button } from '@/components/mds';
import { Switch } from '@/components/settings/controls';
import { useAgentsStore } from '@/stores/agents-store';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';

type OffboardMode = 'archive' | 'handoff' | 'remove';

/** A labelled toggle row — the Switch itself only carries an aria-label. */
function SwitchRow({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="flex items-center justify-between gap-3 text-sm text-foreground">
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
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'agents.offboard.title' }, { name: agent.display_name })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
        <p className="text-sm text-muted-foreground">
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
                  'flex w-full items-start gap-3 rounded-xl border p-3 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
                  active
                    ? 'border-brand bg-brand/10'
                    : 'border-surface-border hover:bg-muted',
                  o.disabled && 'cursor-not-allowed opacity-50',
                )}
              >
                <span className={cn('mt-0.5 grid h-8 w-8 shrink-0 place-items-center rounded-lg', active ? 'bg-brand/15 text-brand' : 'bg-muted text-muted-foreground')}>
                  <Icon className="h-4 w-4" />
                </span>
                <span className="min-w-0">
                  <span className="block text-sm font-medium text-foreground">
                    {intl.formatMessage({ id: `agents.offboard.${o.id}.title` })}
                  </span>
                  <span className="mt-0.5 block text-xs text-muted-foreground">
                    {intl.formatMessage({ id: `agents.offboard.${o.id}.desc` })}
                  </span>
                </span>
              </button>
            );
          })}
        </div>

        {/* Handoff options */}
        {mode === 'handoff' && candidates.length > 0 && (
          <div className="space-y-3 rounded-xl bg-muted/50 p-3">
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-foreground">
                {intl.formatMessage({ id: 'agents.offboard.handoff.target' })}
              </label>
              <select
                value={target}
                onChange={(e) => setTarget(e.target.value)}
                className="h-8 w-full rounded-lg border border-input bg-transparent px-2.5 text-sm focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
              >
                {candidates.map((c) => (
                  <option key={c.name} value={c.name}>{c.display_name}</option>
                ))}
              </select>
            </div>
            <div className="space-y-2">
              <SwitchRow label={intl.formatMessage({ id: 'agents.offboard.handoff.memory' })} checked={moveMemory} onChange={setMoveMemory} />
              <SwitchRow label={intl.formatMessage({ id: 'agents.offboard.handoff.wiki' })} checked={moveWiki} onChange={setMoveWiki} />
              <SwitchRow label={intl.formatMessage({ id: 'agents.offboard.handoff.tasks' })} checked={moveTasks} onChange={setMoveTasks} />
            </div>
          </div>
        )}

        {/* Remove type-to-confirm */}
        {mode === 'remove' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'agents.offboard.remove.confirmLabel' })}
            </label>
            <Input
              type="text"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              placeholder={agent.display_name}
            />
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'agents.offboard.remove.confirmHint' }, { name: agent.display_name })}
            </p>
          </div>
        )}

        {/* PARTIAL / error surface — honest, verbatim */}
        {errors && errors.length > 0 && (
          <div className="space-y-1.5 rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">
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

        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={submitting}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant={mode === 'archive' ? 'brand' : 'destructive'} onClick={handleSubmit} disabled={!canSubmit}>
            {submitting ? intl.formatMessage({ id: 'common.saving' }) : confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
