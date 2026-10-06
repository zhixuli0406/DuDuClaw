import { useIntl } from 'react-intl';
import { Badge, Button } from '@/components/mds';
import { reasonList } from './workflow-text';
import type { ReviewSnapshot } from './types';

/** The snapshot never follows mutable task/files; status is a server projection. */
export function ReviewEvidencePanel({ snapshot, onCreateDraft, busy = false }: {
  snapshot: ReviewSnapshot; onCreateDraft?: () => void; busy?: boolean;
}) {
  const intl = useIntl();
  const text = (id: string) => intl.formatMessage({ id });
  const stale = snapshot.artifacts.some((a) => a.integrity !== 'current');
  const gaps = reasonList(intl, snapshot.gaps);
  return (
    <section aria-label={text('workflow.review.title')} className="space-y-3 rounded-xl border border-border bg-surface p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{text('workflow.review.title')}</h3>
        <Badge variant="outline">{text('workflow.review.revision')} {snapshot.authority_revision}</Badge>
      </div>
      <p className="break-all text-xs text-muted-foreground">
        {text('workflow.review.fingerprint')}: <span className="font-mono">{snapshot.snapshot_hash}</span>
      </p>
      {snapshot.criteria_ledger != null && (
        <details>
          <summary className="cursor-pointer text-xs">{text('workflow.review.criteria')}</summary>
          <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(snapshot.criteria_ledger, null, 2)}</pre>
        </details>
      )}
      {snapshot.artifacts.length === 0 && <p className="text-sm text-muted-foreground">{text('workflow.review.noArtifacts')}</p>}
      <ul className="space-y-2">
        {snapshot.artifacts.map((a) => (
          <li key={a.artifact_id} className="space-y-1 rounded-lg bg-background p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="min-w-0 break-all text-sm font-medium">{a.name}</span>
              <Badge variant="secondary">{text(`workflow.evidence.${a.evidence_kind}`)}</Badge>
              <Badge variant="outline">{text(`workflow.integrity.${a.integrity}`)}</Badge>
              {a.audience.length > 0 && <Badge variant="outline">{text('workflow.review.private')}</Badge>}
            </div>
            {(a.archived_hash || a.source_hash) && <p className="break-all font-mono text-xs text-muted-foreground">{a.archived_hash || a.source_hash}</p>}
            {a.run_id && <p className="break-all font-mono text-xs">{text('workflow.run')} {a.run_id}</p>}
            {a.reasons.length > 0 && (
              <ul className="list-inside list-disc text-xs text-amber-700 dark:text-amber-400">
                {reasonList(intl, a.reasons).map((r) => <li key={r}>{r}</li>)}
              </ul>
            )}
          </li>
        ))}
      </ul>
      {gaps.length > 0 && <ul aria-label={text('workflow.review.gaps')} className="list-inside list-disc text-sm text-muted-foreground">{gaps.map((g) => <li key={g}>{g}</li>)}</ul>}
      {snapshot.waiting_for && <p className="break-words text-sm">{text('workflow.review.waiting')}: {snapshot.waiting_for}</p>}
      {snapshot.next_check_at && <p className="text-xs">{text('workflow.review.nextCheck')}: {snapshot.next_check_at}</p>}
      {onCreateDraft && <Button onClick={onCreateDraft} disabled={busy || stale || snapshot.artifacts.length === 0}>{text('workflow.draft.create')}</Button>}
    </section>
  );
}
