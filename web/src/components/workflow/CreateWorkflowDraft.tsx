import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Button } from '@/components/mds';
import { errorText } from './workflow-text';
import type { DraftService, DraftView, ReviewSnapshot } from './types';

/** Proposal content is a typed candidate. Identity and authority are host-owned. */
export function CreateWorkflowDraft({ snapshot, service, onCreated }: {
  snapshot: ReviewSnapshot; service: DraftService; onCreated: (view: DraftView) => void;
}) {
  const intl = useIntl();
  const text = (id: string) => intl.formatMessage({ id });
  const [proposal, setProposal] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = snapshot.artifacts.length > 0 && snapshot.artifacts.every((a) => a.integrity === 'current');
  async function create() {
    if (busy || !current) return;
    setError(null);
    let parsed: unknown;
    try { parsed = JSON.parse(proposal); }
    catch { setError(text('workflow.draft.invalidJson')); return; }
    setBusy(true);
    try {
      const result = await service.create(snapshot.task_id, snapshot.snapshot_id, snapshot.snapshot_hash, parsed);
      onCreated(result);
    } catch (e) { setError(errorText(intl, e)); }
    finally { setBusy(false); }
  }
  return <section className="space-y-3 rounded-xl border border-border bg-surface p-4">
    <h3 className="text-sm font-semibold">{text('workflow.draft.create')}</h3>
    <p className="text-sm text-muted-foreground">{text('workflow.draft.proposalHint')}</p>
    <label htmlFor="workflow-proposal" className="block text-sm">{text('workflow.draft.proposal')}</label>
    <textarea id="workflow-proposal" value={proposal} onChange={(e) => setProposal(e.target.value)} rows={12} spellCheck={false} className="w-full rounded-lg border border-input bg-background p-3 font-mono text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring" disabled={busy} />
    {!current && <p role="alert" className="text-sm text-amber-700 dark:text-amber-400">{text('workflow.draft.refreshSnapshot')}</p>}
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    <Button disabled={busy || !current || proposal.trim().length === 0} onClick={() => void create()}>{busy ? text('workflow.draft.saving') : text('workflow.draft.save')}</Button>
  </section>;
}
