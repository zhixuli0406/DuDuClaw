import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Badge, Button } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { activationStateText, errorText, outcomeText, policyCategoryText, reasonText, runStatusText } from './workflow-text';
import type { DraftService, DraftView, FixtureKind } from './types';

const KINDS: FixtureKind[] = ['normal', 'empty', 'expired', 'injection', 'missing_permission'];
type Pending = { kind: 'run'; fixtureId: string; fixtureKind: FixtureKind } | { kind: 'request' | 'commit' | 'revoke' };

/** Server records are authoritative after every action and reconnect. */
export function WorkflowDraftPanel({ draftId, revision, service }: {
  draftId: string; revision: number; service: DraftService;
}) {
  const intl = useIntl();
  const text = (id: string, values?: Record<string, string>) => intl.formatMessage({ id }, values);
  const [view, setView] = useState<DraftView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [approvalId, setApprovalId] = useState<string | null>(null);
  const [requestedRun, setRequestedRun] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const generation = useRef(0);
  // Reads can overlap (poll + post-action refresh); only a newer one may land.
  const requestSeq = useRef(0);
  const appliedSeq = useRef(0);
  const refresh = useCallback(async (expectedGeneration: number) => {
    const seq = ++requestSeq.current;
    const next = await service.get(draftId, revision);
    if (generation.current === expectedGeneration && seq > appliedSeq.current) { appliedSeq.current = seq; setView(next); }
  }, [draftId, revision, service]);

  useEffect(() => {
    const token = ++generation.current;
    requestSeq.current = 0; appliedSeq.current = 0;
    setView(null); setError(null); setApprovalId(null); setRequestedRun(null); setBusy(null); setPending(null);
    const report = (e: unknown) => { if (generation.current === token) setError(errorText(intl, e)); };
    refresh(token).catch(report);
    const interval = window.setInterval(() => { refresh(token).catch(report); }, 3000);
    return () => { generation.current++; window.clearInterval(interval); };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intl only formats errors
  }, [refresh]);

  async function act(key: string, operation: () => Promise<void>) {
    if (busy) return;
    const token = generation.current;
    setBusy(key); setError(null);
    try { await operation(); await refresh(token); }
    catch (e) { if (generation.current === token) setError(errorText(intl, e)); }
    finally { if (generation.current === token) setBusy(null); }
  }
  if (!view) {
    return <div className="space-y-2">
      <p role={error ? 'alert' : 'status'} className={error ? 'text-sm text-destructive' : 'text-sm text-muted-foreground'}>{error ?? text('workflow.loading')}</p>
      {error && <Button size="sm" variant="outline" onClick={() => { setError(null); refresh(generation.current).catch((e) => setError(errorText(intl, e))); }}>{text('workflow.reload')}</Button>}
    </div>;
  }
  const { draft } = view;
  const complete = KINDS.every((kind) => {
    const fixture = draft.fixtures.find((f) => f.kind === kind);
    const result = view.fixtures.find((f) => f.evidence.fixture_id === fixture?.fixture_id);
    return result && Date.parse(result.evidence.expires_at) > Date.now()
      && result.assertions.length > 0 && result.assertions.every((a) => a.outcome === 'matched' && a.run_id === result.evidence.run_id);
  });
  const activationPending = Boolean(approvalId || view.activation);

  function confirmed() {
    const action = pending; setPending(null);
    if (!action) return;
    if (action.kind === 'run') {
      void act(action.fixtureId, async () => {
        const token = generation.current;
        const response = await service.runFixture(draft.draft_id, draft.revision, action.fixtureId, draft.draft_hash);
        if (generation.current === token) setRequestedRun(response.run_id);
      });
    } else if (action.kind === 'request') {
      void act('activation', async () => {
        const token = generation.current;
        const response = await service.requestActivation(draft.draft_id, draft.revision, draft.draft_hash);
        if (generation.current === token) setApprovalId(response.approval_id);
      });
    } else if (action.kind === 'commit') {
      void act('commit', async () => { await service.commitActivation!(draft.draft_id, draft.revision, draft.draft_hash); });
    } else {
      void act('revoke', async () => { await service.revokeActivation!(draft.draft_id, draft.revision, draft.draft_hash); });
    }
  }
  const confirmKey = pending?.kind ?? 'request';
  const confirmValues = pending?.kind === 'run' ? { kind: text(`workflow.fixture.${pending.fixtureKind}`) } : undefined;

  return (
    <section aria-label={text('workflow.draft.title')} className="space-y-4 rounded-xl border border-border bg-surface p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{text('workflow.draft.title')}</h3>
        <Badge variant="outline">{text('workflow.draft.revision')} {draft.revision}</Badge>
        <Badge variant="secondary">{view.activation?.state === 'active' ? text('workflow.activation.active') : text('workflow.draft.disabled')}</Badge>
      </div>
      <dl className="grid grid-cols-1 gap-2 text-xs sm:grid-cols-2">
        <div><dt className="text-muted-foreground">{text('workflow.skillHash')}</dt><dd className="break-all font-mono">{draft.definition.skill_revision_hash}</dd></div>
        <div><dt className="text-muted-foreground">{text('workflow.draftHash')}</dt><dd className="break-all font-mono">{draft.draft_hash}</dd></div>
        <div><dt className="text-muted-foreground">{text('workflow.capabilities')}</dt><dd className="break-words">{draft.definition.required_capabilities.join(', ') || '—'}</dd><dd className="text-xs text-muted-foreground">{text('workflow.capabilitiesHint')}</dd></div>
        <div><dt className="text-muted-foreground">{text('workflow.timezone')}</dt><dd className="break-words">{draft.timezone}</dd></div>
        {view.pricing?.money_limits_effective === false
          ? <div><dt className="text-muted-foreground">{text('workflow.countLimits')}</dt><dd>{text('workflow.countLimitsValue', Object.fromEntries(Object.entries(view.pricing.limits).map(([k, v]) => [k, String(v)])))}</dd><dd className="text-xs text-muted-foreground">{text('workflow.moneyLimitsInactive')}</dd></div>
          : <div><dt className="text-muted-foreground">{text('workflow.budget')}</dt><dd>{draft.budget.per_run_micros} / {draft.budget.monthly_micros}</dd></div>}
        <div><dt className="text-muted-foreground">{text('workflow.freshness')}</dt><dd>{draft.input_max_age_seconds}</dd></div>
        <div><dt className="text-muted-foreground">{text('workflow.failureLimit')}</dt><dd>{draft.budget.max_consecutive_failures}</dd></div>
      </dl>
      <details className="rounded-lg border border-border p-3 text-xs"><summary className="cursor-pointer">{text('workflow.schemas')}</summary>
        <p className="mt-2 text-muted-foreground">{text('workflow.sourceData')}</p>
        <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-all">{JSON.stringify({ input: draft.definition.input_schema, output: draft.definition.output_schema, DATA: draft.source_data }, null, 2)}</pre>
      </details>
      <p className="rounded-lg bg-amber-500/10 p-3 text-sm">{text('workflow.fixture.realOperation')}</p>
      <ul className="space-y-2">
        {KINDS.map((kind) => {
          const f = draft.fixtures.find((f) => f.kind === kind);
          const result = view.fixtures.find((r) => r.evidence.fixture_id === f?.fixture_id);
          return <li key={kind} className="rounded-lg bg-background p-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className="text-sm font-medium">{text(`workflow.fixture.${kind}`)}</span>
              <Button size="sm" variant="outline" disabled={Boolean(busy) || !f || Boolean(view.activation)} onClick={() => f && setPending({ kind: 'run', fixtureId: f.fixture_id, fixtureKind: kind })}>{text('workflow.fixture.run')}</Button>
            </div>
            {result ? <div className="mt-2 space-y-1 text-xs">
              <p>{text('workflow.fixture.actualStatus')}: {runStatusText(intl, result.evidence.status)}</p>
              <p>{text('workflow.fixture.assertions')}: {intl.formatList(result.assertions.map((a) => outcomeText(intl, a.outcome)), { type: 'conjunction' })}</p>
              <p className="break-all font-mono">{text('workflow.run')} {result.evidence.run_id}</p>
              {result.evidence.result_hash && <p className="break-all font-mono">{result.evidence.result_hash}</p>}
              <p>{text('workflow.fixture.expires')}: {result.evidence.expires_at}</p>
              {result.evidence.steps.map((s) => <p key={s.step_id} className="break-all">{s.step_id}: {runStatusText(intl, s.status)}{s.output_hash ? <span className="font-mono"> {s.output_hash}</span> : null}{s.error_code ? ` · ${errorText(intl, s.error_code)}` : ''}</p>)}
            </div> : <p className="mt-2 text-xs text-muted-foreground">{text('workflow.fixture.unverified')}</p>}
          </li>;
        })}
      </ul>
      {requestedRun && <p role="status" className="break-all text-xs">{text('workflow.fixture.requested')}: {requestedRun}</p>}
      <p className="text-xs text-muted-foreground">{text('workflow.fixture.limited')}</p>
      {view.issues.length > 0 && <ul className="list-inside list-disc text-sm text-amber-700 dark:text-amber-400">{[...new Set(view.issues.map((issue) => reasonText(intl, issue)))].map((issue) => <li key={issue}>{issue}</li>)}</ul>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className="flex flex-wrap gap-2">
        <Button disabled={Boolean(busy) || !view.activation_eligible || !complete || activationPending} onClick={() => setPending({ kind: 'request' })}>{text('workflow.activation.request')}</Button>
        {view.activation?.state === 'prepared' && service.commitActivation && <Button disabled={Boolean(busy)} onClick={() => setPending({ kind: 'commit' })}>{text('workflow.activation.commit')}</Button>}
        {view.activation && !['revoked', 'revoking'].includes(view.activation.state) && service.revokeActivation && <Button variant="outline" disabled={Boolean(busy)} onClick={() => setPending({ kind: 'revoke' })}>{text('workflow.activation.revoke')}</Button>}
      </div>
      {(approvalId || view.activation) && <div className="space-y-1 rounded-lg border border-border p-3 text-xs" role="status">
        <p>{text('workflow.activation.status')}: {view.activation ? activationStateText(intl, view.activation.state) : text('workflow.activation.pending')}</p>
        <p className="break-all font-mono">{view.activation?.acceptance_id || approvalId}</p>
        {view.activation?.expires_at && ['prepared', 'active'].includes(view.activation.state) && <p>{text('workflow.activation.expiresAt', { at: view.activation.expires_at })}</p>}
        {view.activation?.error_code && !['suspended', 'expired'].includes(view.activation.state) && <p className="text-destructive">{errorText(intl, view.activation.error_code)}</p>}
        {view.activation?.state === 'suspended' && <div className="space-y-1 text-amber-700 dark:text-amber-400">
          {view.activation.suspension?.reason.startsWith('limit:')
            ? <p>{text('workflow.activation.suspendedLimit')}</p>
            : <>
              <p>{text('workflow.activation.suspendedBecause')}</p>
              {(view.activation.suspension?.changed_categories.length ?? 0) > 0
                ? <ul className="list-inside list-disc">{view.activation.suspension!.changed_categories.map((c) => <li key={c}>{policyCategoryText(intl, c)}</li>)}</ul>
                : <p>{text('workflow.policyCategory.other')}</p>}
            </>}
          <p>{text('workflow.activation.suspendedRecover')}</p>
        </div>}
        {view.activation?.state === 'expired' && <div className="space-y-1 text-amber-700 dark:text-amber-400">
          <p>{text('workflow.activation.expiredBecause', { at: view.activation.expires_at ?? '' })}</p>
          <p>{text('workflow.activation.suspendedRecover')}</p>
        </div>}
      </div>}
      <ConfirmDialog
        open={pending !== null}
        onClose={() => setPending(null)}
        onConfirm={confirmed}
        title={text(`workflow.confirm.${confirmKey}.title`, confirmValues)}
        message={text(`workflow.confirm.${confirmKey}.message`, confirmValues)}
        confirmLabel={text(`workflow.confirm.${confirmKey}.confirm`)}
        busy={Boolean(busy)}
      />
    </section>
  );
}
