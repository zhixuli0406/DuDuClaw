import { useIntl } from 'react-intl';
import { Check, Circle, X } from 'lucide-react';
import { Spinner } from '@/components/mds';
import type { ExpertGenerationJob, ExpertStepStatus } from '@/lib/api';

function StepIcon({ status }: { status: ExpertStepStatus }) {
  switch (status) {
    case 'done':
      return <Check className="size-4 text-success" aria-hidden />;
    case 'failed':
      return <X className="size-4 text-destructive" aria-hidden />;
    case 'running':
      return <Spinner className="size-4" />;
    default:
      return <Circle className="size-4 text-muted-foreground/50" aria-hidden />;
  }
}

function Step({
  status,
  label,
  children,
}: {
  status: ExpertStepStatus;
  label: string;
  children?: React.ReactNode;
}) {
  const intl = useIntl();
  return (
    <li className="space-y-1.5">
      <div className="flex items-center gap-2 text-sm">
        <StepIcon status={status} />
        <span className={status === 'pending' ? 'text-muted-foreground' : undefined}>{label}</span>
        <span className="sr-only">
          {intl.formatMessage({ id: `experts.generate.step.${status}` })}
        </span>
      </div>
      {children}
    </li>
  );
}

/** One status for the "every member" step: running while any member is
 *  in flight or still queued after others started; failed only when every
 *  member failed (some failures still yield a usable, partial team). */
export function aggregateStatus(statuses: ExpertStepStatus[]): ExpertStepStatus {
  if (statuses.length === 0) return 'pending';
  const finished = statuses.filter((s) => s === 'done' || s === 'failed').length;
  if (finished === statuses.length) {
    return statuses.every((s) => s === 'failed') ? 'failed' : 'done';
  }
  return statuses.some((s) => s !== 'pending') ? 'running' : 'pending';
}

/**
 * Per-stage progress of a background team generation: roster → each
 * member's role description and skills → knowledge base → assemble.
 */
export function GenerationProgress({ job }: { job: ExpertGenerationJob | null }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);
  const progress = job?.progress;
  const members = progress?.members ?? [];
  const done = members.filter((m) => m.status === 'done').length;
  const membersStatus = aggregateStatus(members.map((m) => m.status));

  return (
    <ol className="w-full max-w-md space-y-3" aria-live="polite">
      <Step status={progress?.roster ?? 'running'} label={t('experts.generate.stage.roster')} />
      <Step
        status={membersStatus}
        label={t('experts.generate.stage.members', { done, total: members.length })}
      >
        {members.length > 0 && (
          <ul className="ml-6 space-y-1">
            {members.map((m) => (
              <li key={m.name} className="flex items-center gap-2 text-xs text-muted-foreground">
                <StepIcon status={m.status} />
                <span>{m.display_name || m.name}</span>
                {m.status === 'failed' && (
                  <span className="text-destructive">{t('experts.generate.memberFallback')}</span>
                )}
              </li>
            ))}
          </ul>
        )}
      </Step>
      <Step status={progress?.wiki ?? 'pending'} label={t('experts.generate.stage.wiki')} />
      <Step status={progress?.assemble ?? 'pending'} label={t('experts.generate.stage.assemble')} />
    </ol>
  );
}
