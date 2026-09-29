import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { GitBranch } from 'lucide-react';
import { Card, CardContent, CollectionPageHeader, Button, Input, Textarea } from '@/components/mds';
import { DemoModeNotice } from '@/components/DemoModeNotice';
import { causalApi, type CausalAlias, type CausalClaim, type CausalScope, type ClaimDetail, type ClaimModality, type ModelReview, type ModelSummary, type ParentAdjustment } from '@/lib/causal-api';
import { CausalEffectPanel } from './CausalEffectPanel';
import { CausalDagView } from './CausalDagView';
import { CausalExtractionEvalPanel } from './CausalExtractionEvalPanel';

type Section = 'claims' | 'aliases' | 'models' | 'eval';

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function hasCycle(edges: CausalClaim[]): boolean {
  const next = new Map<string, string[]>();
  for (const edge of edges) next.set(edge.cause_variable, [...(next.get(edge.cause_variable) ?? []), edge.effect_variable]);
  const visiting = new Set<string>();
  const visited = new Set<string>();
  function visit(name: string): boolean {
    if (visiting.has(name)) return true;
    if (visited.has(name)) return false;
    visiting.add(name);
    for (const child of next.get(name) ?? []) if (visit(child)) return true;
    visiting.delete(name); visited.add(name);
    return false;
  }
  return [...next.keys()].some(visit);
}

const MODALITY_KINDS: ClaimModality[] = ['asserted', 'speculated', 'negated', 'questioned'];

export function CausalCurationPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const listSep = t('causalCuration.listSeparator');
  const reasonSep = t('causalCuration.reasonSeparator');
  const [tenant, setTenant] = useState('');
  const [acl, setAcl] = useState('');
  const [scope, setScope] = useState<CausalScope | null>(null);
  const [section, setSection] = useState<Section>('claims');
  const [claims, setClaims] = useState<CausalClaim[]>([]);
  const [aliases, setAliases] = useState<CausalAlias[]>([]);
  const [models, setModels] = useState<ModelSummary[]>([]);
  const [claimId, setClaimId] = useState('');
  const [modelId, setModelId] = useState('');
  const [detail, setDetail] = useState<ClaimDetail | null>(null);
  const [model, setModel] = useState<ModelReview | null>(null);
  const [parentAdjustment, setParentAdjustment] = useState<ParentAdjustment | null>(null);
  const [comparisonIds, setComparisonIds] = useState<string[]>([]);
  const [comparison, setComparison] = useState<Record<string, { review: ModelReview; supportLineages: number; oppositionLineages: number }>>({});
  const [source, setSource] = useState<{ artifact: string; text: string; offset: number } | null>(null);
  const [pendingRemoval, setPendingRemoval] = useState<{ artifact: string; erase: boolean } | null>(null);
  const [selectedEvidence, setSelectedEvidence] = useState<string[]>([]);
  const [cause, setCause] = useState('');
  const [effect, setEffect] = useState('');
  const [lagMin, setLagMin] = useState(0);
  const [lagMax, setLagMax] = useState(0);
  const [modality, setModality] = useState<ClaimModality>('asserted');
  const [note, setNote] = useState('');
  const [alias, setAlias] = useState('');
  const [canonical, setCanonical] = useState('');
  const [aliasReviewId, setAliasReviewId] = useState<string | null>(null);
  const [memoryId, setMemoryId] = useState('');
  const [wikiPath, setWikiPath] = useState('');
  const [extractArtifactId, setExtractArtifactId] = useState('');
  const [extractQuestion, setExtractQuestion] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const sourceRequestSequence = useRef(0);
  const claimRequestSequence = useRef(0);
  const comparisonClaimGate = useRef<{ inFlight: number; waiters: Array<() => void> }>({ inFlight: 0, waiters: [] });

  const refresh = useCallback(async (target: CausalScope) => {
    const [c, a, m] = await Promise.all([
      causalApi.claims(target), causalApi.aliases(target), causalApi.models(target),
    ]);
    setClaims(c.claims);
    setAliases(a.aliases);
    setModels(m.models);
  }, []);

  async function openScope() {
    const target = { tenant_id: tenant.trim(), acl: acl.trim() };
    if (!target.tenant_id || !target.acl) { setError(t('causalCuration.scopeRequired')); return; }
    setBusy(true); setError(''); setNotice('');
    try {
      await refresh(target);
      setScope(target); setClaimId(''); setModelId(''); setDetail(null); setModel(null); setParentAdjustment(null); setSource(null); setPendingRemoval(null); setComparisonIds([]);
    } catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  }

  useEffect(() => {
    sourceRequestSequence.current += 1;
    setSource(null);
  }, [scope, claimId]);

  useEffect(() => {
    if (!scope || !claimId) { setDetail(null); return; }
    let active = true;
    const requestSequence = ++claimRequestSequence.current;
    causalApi.claim(scope, claimId).then((next) => {
      if (!active || requestSequence !== claimRequestSequence.current) return;
      setDetail(next); setSource(null);
      setCause(next.claim.cause_variable); setEffect(next.claim.effect_variable);
      setLagMin(next.claim.lag_min_seconds); setLagMax(next.claim.lag_max_seconds);
      setModality(next.claim.modality); setNote('');
      setSelectedEvidence(next.evidence.filter((item) => item.source_active && item.span.stance === 'supports').map((item) => item.span.id));
    }).catch((err) => { if (active && requestSequence === claimRequestSequence.current) setError(message(err)); });
    return () => { active = false; };
  }, [scope, claimId]);

  useEffect(() => {
    if (!scope || !modelId) { setModel(null); setParentAdjustment(null); return; }
    let active = true;
    setModel(null); setParentAdjustment(null);
    causalApi.model(scope, modelId).then(({ review, parent_adjustment }) => {
      if (active) { setModel(review); setParentAdjustment(parent_adjustment); }
    })
      .catch((err) => { if (active) setError(message(err)); });
    return () => { active = false; };
  }, [scope, modelId]);

  useEffect(() => {
    if (!scope || comparisonIds.length === 0) { setComparison({}); return; }
    const currentScope = scope;
    let active = true;
    setComparison({});
    async function loadClaimBounded(id: string): Promise<ClaimDetail | null> {
      const gate = comparisonClaimGate.current;
      while (active && gate.inFlight >= 6) {
        await new Promise<void>((resolve) => gate.waiters.push(resolve));
      }
      if (!active) return null;
      gate.inFlight += 1;
      try {
        return await causalApi.claim(currentScope, id);
      } finally {
        gate.inFlight -= 1;
        gate.waiters.splice(0).forEach((resolve) => resolve());
      }
    }
    void (async () => {
      const reviewEntries = await Promise.all(comparisonIds.map(async (id) => [id, (await causalApi.model(currentScope, id)).review] as const));
      if (!active) return;
      const reviews = new Map(reviewEntries);
      const claimIds = [...new Set(reviewEntries.flatMap(([, review]) => review.edges.map((edge) => edge.id)))];
      const claimDetails = new Map<string, ClaimDetail>();
      let nextIndex = 0;
      async function worker() {
        while (active) {
          const index = nextIndex;
          nextIndex += 1;
          const id = claimIds[index];
          if (!id) return;
          const item = await loadClaimBounded(id);
          if (!active) return;
          if (item) claimDetails.set(id, item);
        }
      }
      await Promise.all(Array.from({ length: Math.min(6, claimIds.length) }, () => worker()));
      if (!active) return;
      const entries = comparisonIds.map((id) => {
        const review = reviews.get(id)!;
        const supporting = new Set<string>();
        const opposing = new Set<string>();
        for (const edge of review.edges) {
          for (const evidence of claimDetails.get(edge.id)?.evidence ?? []) {
            if (!evidence.source_active) continue;
            (evidence.span.stance === 'supports' ? supporting : opposing).add(evidence.source_lineage_id);
          }
        }
        return [id, { review, supportLineages: supporting.size, oppositionLineages: opposing.size }] as const;
      });
      setComparison(Object.fromEntries(entries));
    })().catch((err) => {
      if (!active) return;
      active = false;
      setError(message(err));
    });
    return () => { active = false; };
  }, [scope, comparisonIds]);

  async function mutate(action: () => Promise<unknown>, success: string) {
    if (!scope) return;
    setBusy(true); setError(''); setNotice('');
    try {
      await action();
      await refresh(scope);
      if (claimId) setDetail(await causalApi.claim(scope, claimId));
      if (modelId) {
        const next = await causalApi.model(scope, modelId);
        setModel(next.review); setParentAdjustment(next.parent_adjustment);
      }
      setNotice(success);
    } catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  }

  async function showSource(artifact: string, spanStart: number) {
    if (!scope) return;
    const requestSequence = ++sourceRequestSequence.current;
    try {
      const chunk = await causalApi.source(scope, artifact, Math.max(0, spanStart - 512));
      if (requestSequence !== sourceRequestSequence.current) return;
      setSource({ artifact, text: chunk.text, offset: chunk.byte_offset });
      setError('');
    } catch (err) {
      if (requestSequence !== sourceRequestSequence.current) return;
      setSource(null); setError(message(err));
    }
  }

  async function removeSource(artifact: string, erase: boolean) {
    if (!scope || busy) return;
    if (erase && !window.confirm(t('causalCuration.source.eraseConfirm', { artifact }))) return;
    sourceRequestSequence.current += 1;
    claimRequestSequence.current += 1;
    setSource(null);
    // A cross-store failure may happen after the causal source has already
    // changed. Hide every cached excerpt before making the request.
    setClaimId(''); setDetail(null); setSelectedEvidence([]);
    setModelId(''); setModel(null); setParentAdjustment(null);
    setComparisonIds([]); setComparison({});
    setPendingRemoval({ artifact, erase });
    setBusy(true); setError(''); setNotice('');
    try {
      const { result } = erase
        ? await causalApi.eraseSource(scope, artifact)
        : await causalApi.invalidateSource(scope, artifact);
      await refresh(scope);
      setPendingRemoval(null);
      const statusText = erase ? t('causalCuration.source.erasedNotice') : t('causalCuration.source.revokedNotice');
      setNotice(statusText + t('causalCuration.source.cleanupSummary', {
        demoted: result.demoted_claims, snapshots: result.scrubbed_snapshots, ccr: result.scrubbed_ccr_originals,
      }));
    } catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  }

  async function submitRevision() {
    if (!scope || !detail) return;
    if (!note.trim() || !cause.trim() || !effect.trim() || cause.trim() === effect.trim()
      || !Number.isInteger(lagMin) || !Number.isInteger(lagMax) || lagMin < 0 || lagMax < lagMin
      || !selectedEvidence.length) {
      setError(t('causalCuration.revision.validationError'));
      return;
    }
    setBusy(true); setError(''); setNotice('');
    try {
      const response = await causalApi.reviseClaim(scope, detail.claim.id, {
        expected_state: detail.claim.review_state, cause_variable: cause.trim(), effect_variable: effect.trim(),
        lag_min_seconds: lagMin, lag_max_seconds: lagMax, modality, context: {},
        evidence_ids: selectedEvidence, note: note.trim(),
      });
      await refresh(scope);
      setClaimId(response.claim.id);
      setNotice(t('causalCuration.revision.success'));
    } catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  }

  const modalityLabels: Record<ClaimModality, string> = {
    asserted: t('causalCuration.modality.asserted'),
    speculated: t('causalCuration.modality.speculated'),
    negated: t('causalCuration.modality.negated'),
    questioned: t('causalCuration.modality.questioned'),
  };

  const currentAlias = aliases.find((item) => item.alias === alias);
  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader icon={GitBranch} title={t('causalCuration.title')} description={t('causalCuration.header.description')} />
      <div className="flex-1 space-y-4 overflow-y-auto p-5">
        {/* X1 方案 5 (2026-09-29 audit). The bundled evaluation fixtures are
            synthetic by construction. X1 方案 2 added one non-synthetic scope:
            `local` / `audit`, the claims transcribed from this machine's own
            `tool_calls.jsonl`. The scope is compared exactly — a substring
            check would label an unrelated `audit-archive` ACL as real. */}
        <DemoModeNotice
          pageKey="causal-curation"
          origin={scope?.tenant_id === 'local' && scope?.acl === 'audit' ? 'auditTrail' : 'synthetic'}
        />
        <Card><CardContent className="flex flex-wrap items-end gap-3">
          <label className="space-y-1 text-sm">{t('causalCuration.tenant')}<Input aria-label={t('causalCuration.tenant')} value={tenant} onChange={(e) => setTenant(e.target.value)} /></label>
          <label className="space-y-1 text-sm">{t('causalCuration.acl')}<Input aria-label={t('causalCuration.acl')} value={acl} onChange={(e) => setAcl(e.target.value)} /></label>
          <Button disabled={busy} onClick={openScope}>{t('causalCuration.loadScope')}</Button>
          <span className="text-xs text-muted-foreground">{t('causalCuration.scopeHint')}</span>
        </CardContent></Card>
        {error && <p role="alert" className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{error}</p>}
        {scope && pendingRemoval && !busy && <div className="flex flex-wrap items-center gap-2 rounded-md border p-3 text-sm">
          <span>{t('causalCuration.pendingRemoval.notice', { artifact: pendingRemoval.artifact })}</span>
          <Button variant="outline" size="sm" onClick={() => removeSource(pendingRemoval.artifact, pendingRemoval.erase)}>
            {pendingRemoval.erase ? t('causalCuration.pendingRemoval.retryDelete') : t('causalCuration.pendingRemoval.retryRevoke')}
          </Button>
        </div>}
        {notice && <p role="status" className="rounded-md bg-emerald-500/10 p-3 text-sm">{notice}</p>}
        {scope && <>
          <Card><CardContent className="flex flex-wrap items-end gap-2">
            <label className="space-y-1 text-sm">{t('causalCuration.extract.sourceIdLabel')}<Input aria-label={t('causalCuration.extract.sourceIdAria')} value={extractArtifactId} onChange={(e) => setExtractArtifactId(e.target.value)} /></label>
            <label className="min-w-64 flex-1 space-y-1 text-sm">{t('causalCuration.extract.questionLabel')}<Input aria-label={t('causalCuration.extract.questionAria')} value={extractQuestion} onChange={(e) => setExtractQuestion(e.target.value)} /></label>
            <Button disabled={busy || !extractArtifactId.trim() || !extractQuestion.trim()} onClick={async () => {
              setBusy(true); setError(''); setNotice('');
              try {
                const { run } = await causalApi.extract(scope, extractArtifactId.trim(), extractQuestion.trim());
                await refresh(scope);
                setNotice(t('causalCuration.extract.success', { count: run.candidates.length, provider: run.provider_id, model: run.model_used }));
                if (run.candidates.length > 0) setClaimId(run.candidates[0][0].id);
              } catch (err) { setError(message(err)); }
              finally { setBusy(false); }
            }}>{t('causalCuration.extract.button')}</Button>
            <span className="w-full text-xs text-muted-foreground">{t('causalCuration.extract.note')}</span>
          </CardContent></Card>
          {scope.acl === 'agent-private' && <Card><CardContent className="flex flex-wrap items-end gap-2">
            <label className="space-y-1 text-sm">{t('causalCuration.memory.idLabel')}<Input aria-label={t('causalCuration.memory.idLabel')} value={memoryId} onChange={(e) => setMemoryId(e.target.value)} /></label>
            <Button disabled={busy || !memoryId.trim()} onClick={async () => {
              setBusy(true); setError(''); setNotice('');
              try {
                const result = await causalApi.importMemory(scope.tenant_id, memoryId.trim());
                setNotice(t('causalCuration.memory.success', { id: result.source.id, version: result.source.version }));
              } catch (err) { setError(message(err)); }
              finally { setBusy(false); }
            }}>{t('causalCuration.memory.importButton')}</Button>
            <span className="text-xs text-muted-foreground">{t('causalCuration.memory.note')}</span>
            <label className="space-y-1 text-sm">{t('causalCuration.wiki.pathLabel')}<Input aria-label={t('causalCuration.wiki.pathLabel')} placeholder={t('causalCuration.wiki.pathPlaceholder')} value={wikiPath} onChange={(e) => setWikiPath(e.target.value)} /></label>
            <Button disabled={busy || !wikiPath.trim()} onClick={async () => {
              setBusy(true); setError(''); setNotice('');
              try {
                const result = await causalApi.importWiki(scope.tenant_id, wikiPath.trim());
                setNotice(t('causalCuration.wiki.success', { id: result.source.id, version: result.source.version }));
              } catch (err) { setError(message(err)); }
              finally { setBusy(false); }
            }}>{t('causalCuration.wiki.importButton')}</Button>
            <span className="text-xs text-muted-foreground">{t('causalCuration.wiki.note')}</span>
          </CardContent></Card>}
          {scope.tenant_id === 'workspace' && scope.acl === 'shared-wiki' && <Card><CardContent className="flex flex-wrap items-end gap-2">
            <label className="space-y-1 text-sm">{t('causalCuration.sharedWiki.pathLabel')}<Input aria-label={t('causalCuration.sharedWiki.pathLabel')} placeholder={t('causalCuration.wiki.pathPlaceholder')} value={wikiPath} onChange={(e) => setWikiPath(e.target.value)} /></label>
            <Button disabled={busy || !wikiPath.trim()} onClick={async () => {
              setBusy(true); setError(''); setNotice('');
              try {
                const result = await causalApi.importSharedWiki(wikiPath.trim());
                setNotice(t('causalCuration.sharedWiki.success', { id: result.source.id, version: result.source.version }));
              } catch (err) { setError(message(err)); }
              finally { setBusy(false); }
            }}>{t('causalCuration.sharedWiki.importButton')}</Button>
            <span className="text-xs text-muted-foreground">{t('causalCuration.sharedWiki.note')}</span>
          </CardContent></Card>}
          <nav aria-label={t('causalCuration.nav.ariaLabel')} className="flex gap-2">
            {(['claims', 'aliases', 'models', 'eval'] as const).map((key) =>
              <Button key={key} variant={section === key ? 'default' : 'outline'} onClick={() => setSection(key)}>
                {{
                  claims: t('causalCuration.nav.claims', { count: claims.length }),
                  aliases: t('causalCuration.nav.aliases', { count: aliases.length }),
                  models: t('causalCuration.nav.models', { count: models.length }),
                  eval: t('causalCuration.nav.eval'),
                }[key]}
              </Button>)}
          </nav>
          {section === 'eval' && <CausalExtractionEvalPanel key={`${scope.tenant_id}\0${scope.acl}`} scope={scope} />}
          {section === 'claims' && <div className="grid gap-4 lg:grid-cols-[minmax(220px,1fr)_minmax(0,2fr)]">
            <Card><CardContent className="space-y-2">
              {claims.length === 0 && <p className="text-sm text-muted-foreground">{t('causalCuration.claims.empty')}</p>}
              {claims.map((item) => <button key={item.id} onClick={() => setClaimId(item.id)}
                className={`block w-full rounded-md border p-3 text-left text-sm ${claimId === item.id ? 'border-brand bg-brand/5' : 'border-border'}`}>
                <span className="font-medium">{item.cause_variable} → {item.effect_variable}</span>
                <span className="ml-2 text-xs text-muted-foreground">{item.review_state}</span>
                <span className="block truncate text-xs text-muted-foreground">{item.id}</span>
              </button>)}
            </CardContent></Card>
            {detail && <Card><CardContent className="space-y-4">
              <div><h2 className="font-semibold">{detail.claim.cause_variable} → {detail.claim.effect_variable}</h2>
                <p className="text-xs text-muted-foreground">{t('causalCuration.claim.statusLine', {
                  state: detail.effective_state, modality: detail.claim.modality,
                  lagMin: detail.claim.lag_min_seconds, lagMax: detail.claim.lag_max_seconds,
                  reviewer: detail.claim.reviewer ?? t('causalCuration.claim.noReviewer'),
                })}</p>
                <p className="text-xs text-muted-foreground">{t('causalCuration.claim.lineageCounts', {
                  supporting: detail.lineages.independent_supporting_lineages, opposing: detail.lineages.independent_opposing_lineages,
                })}</p>
              </div>
              <section className="space-y-2"><h3 className="text-sm font-semibold">{t('causalCuration.evidence.title')}</h3>
                {detail.evidence.map((item) => <div key={item.span.id} className="rounded-md border p-2 text-sm">
                  <label className="flex items-start gap-2"><input type="checkbox" checked={selectedEvidence.includes(item.span.id)}
                    disabled={!item.source_active || busy} onChange={(e) => setSelectedEvidence((current) => e.target.checked ? [...current, item.span.id] : current.filter((id) => id !== item.span.id))} />
                    <span>{t('causalCuration.evidence.line', {
                      stance: item.span.stance, lineage: item.source_lineage_id,
                      start: item.span.span_start, end: item.span.span_end,
                    })}</span></label>
                  <blockquote className="my-2 whitespace-pre-wrap break-words border-l-2 pl-3">{item.source_active ? item.span.excerpt : t('causalCuration.evidence.revokedOrExpired')}</blockquote>
                  <div className="flex flex-wrap gap-2">
                    {item.source_active && <Button variant="outline" size="sm" disabled={busy} onClick={() => showSource(item.span.artifact_id, item.span.span_start)}>{t('causalCuration.evidence.viewSource')}</Button>}
                    <Button variant="outline" size="sm" disabled={busy} onClick={() => removeSource(item.span.artifact_id, true)}>{t('causalCuration.evidence.deleteForever')}</Button>
                  </div>
                </div>)}
                {source && <div className="space-y-2 rounded-md bg-muted p-3 text-xs">
                  <p>{t('causalCuration.source.offsetLine', { artifact: source.artifact, offset: source.offset })}</p>
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words">{source.text}</pre>
                  <div className="flex flex-wrap gap-2">
                    <Button variant="outline" size="sm" disabled={busy} onClick={() => removeSource(source.artifact, false)}>{t('causalCuration.source.revoke')}</Button>
                  </div>
                </div>}
              </section>
              {detail.revisions.map((rev) => <p key={`${rev.old_claim_id}-${rev.new_claim_id}`} className="text-xs text-muted-foreground">{t('causalCuration.revision.prefix', {
                old: rev.old_claim_id, new: rev.new_claim_id, reviewer: rev.reviewer, note: rev.note,
              })}</p>)}
              {detail.claim.review_state !== 'superseded' && <>
                <div className="flex gap-2"><Button disabled={busy} onClick={() => mutate(() => causalApi.reviewClaim(scope, detail.claim.id, detail.claim.review_state, true), t('causalCuration.review.acceptSuccess'))}>{t('causalCuration.review.accept')}</Button>
                  <Button variant="outline" disabled={busy} onClick={() => mutate(() => causalApi.reviewClaim(scope, detail.claim.id, detail.claim.review_state, false), t('causalCuration.review.rejectSuccess'))}>{t('causalCuration.review.reject')}</Button></div>
                <section className="space-y-2 border-t pt-3"><h3 className="text-sm font-semibold">{t('causalCuration.revision.sectionTitle')}</h3>
                  <div className="grid grid-cols-2 gap-2"><label>{t('causalCuration.revision.causeLabel')}<Input value={cause} onChange={(e) => setCause(e.target.value)} /></label><label>{t('causalCuration.revision.effectLabel')}<Input value={effect} onChange={(e) => setEffect(e.target.value)} /></label>
                    <label>{t('causalCuration.revision.lagMinLabel')}<Input type="number" min={0} value={lagMin} onChange={(e) => setLagMin(Number(e.target.value))} /></label>
                    <label>{t('causalCuration.revision.lagMaxLabel')}<Input type="number" min={0} value={lagMax} onChange={(e) => setLagMax(Number(e.target.value))} /></label></div>
                  <label className="block">{t('causalCuration.revision.modalityLabel')}<select className="ml-2 rounded-md border bg-background p-2" value={modality} onChange={(e) => setModality(e.target.value as ClaimModality)}>
                    {MODALITY_KINDS.map((kind) => <option key={kind} value={kind}>{modalityLabels[kind]}</option>)}
                  </select></label>
                  <Textarea aria-label={t('causalCuration.revision.reasonAria')} placeholder={t('causalCuration.revision.reasonPlaceholder')} value={note} onChange={(e) => setNote(e.target.value)} />
                  <Button disabled={busy} onClick={submitRevision}>{t('causalCuration.revision.submit')}</Button>
                </section>
              </>}
            </CardContent></Card>}
          </div>}
          {section === 'aliases' && <Card><CardContent className="space-y-3">
            <p className="text-sm text-muted-foreground">{t('causalCuration.alias.note')}</p>
            <div className="flex flex-wrap gap-2"><Input aria-label={t('causalCuration.alias.aliasLabel')} placeholder={t('causalCuration.alias.aliasLabel')} value={alias} onChange={(e) => { setAlias(e.target.value); setAliasReviewId(null); }} />
              <Input aria-label={t('causalCuration.alias.canonicalLabel')} placeholder={t('causalCuration.alias.canonicalLabel')} value={canonical} onChange={(e) => setCanonical(e.target.value)} />
              <Button disabled={busy} onClick={() => mutate(() => causalApi.setAlias(scope, alias.trim(), canonical.trim(), aliasReviewId ?? currentAlias?.review_id ?? null), t('causalCuration.alias.saveSuccess'))}>{t('causalCuration.alias.save')}</Button></div>
            {aliases.map((item) => <div key={item.alias} className="flex flex-wrap items-center gap-2 rounded-md border p-2 text-sm">
              <span className="flex-1">{item.alias} → {item.canonical_name}</span>
              <Button variant="outline" size="sm" onClick={() => { setAlias(item.alias); setCanonical(item.canonical_name); setAliasReviewId(item.review_id); }}>{t('causalCuration.alias.edit')}</Button>
              <Button variant="outline" size="sm" disabled={busy} onClick={() => mutate(() => causalApi.revokeAlias(scope, item.alias, item.review_id), t('causalCuration.alias.revokeSuccess'))}>{t('causalCuration.alias.revoke')}</Button>
            </div>)}
          </CardContent></Card>}
          {section === 'models' && <div className="space-y-4">
            <Card><CardContent className="space-y-3">
              <section className="space-y-2">
                <h2 className="text-sm font-semibold">{t('causalCuration.models.compareTitle')}</h2>
                <p className="text-xs text-muted-foreground">{t('causalCuration.models.compareNote')}</p>
                {models.map((item) => <label key={`compare-${item.model.id}`} className="flex items-start gap-2 text-xs">
                  <input type="checkbox" checked={comparisonIds.includes(item.model.id)} disabled={!comparisonIds.includes(item.model.id) && comparisonIds.length >= 3}
                    onChange={(event) => setComparisonIds((current) => event.target.checked
                      ? [...current, item.model.id] : current.filter((id) => id !== item.model.id))} />
                  <span>{item.model.name} · {item.model.version}</span>
                </label>)}
                {comparisonIds.length > 0 && <div className="grid gap-2 pt-2 sm:grid-cols-2 xl:grid-cols-3">
                  {comparisonIds.map((id) => {
                    const item = models.find((candidate) => candidate.model.id === id);
                    const data = comparison[id];
                    return <article key={id} className="min-w-0 rounded-md border p-3 text-xs" aria-label={t('causalCuration.models.compareAria', { name: item?.model.name ?? id })}>
                      <h3 className="truncate font-semibold">{item?.model.name ?? id} · {item?.model.version}</h3>
                      {data ? <>
                        <p className="mt-2">{t('causalCuration.models.reviewState', { state: data.review.model.review_state })}</p>
                        <p>{t('causalCuration.models.effectiveState', { state: data.review.effective_state })}</p>
                        <p>{t('causalCuration.models.activeOpposition', { count: data.review.active_opposition_count })}</p>
                        <p>{t('causalCuration.models.edgesVariables', { edges: data.review.edges.length, variables: data.review.variables.length })}</p>
                        <p>{t('causalCuration.models.supportLineage', { count: data.supportLineages })}</p>
                        <p>{t('causalCuration.models.oppositionLineage', { count: data.oppositionLineages })}</p>
                      </> : <p className="mt-2 text-muted-foreground">{t('causalCuration.models.loading')}</p>}
                    </article>;
                  })}
                </div>}
              </section>
            </CardContent></Card>
            <div className="grid gap-4 lg:grid-cols-[minmax(220px,1fr)_minmax(0,2fr)]">
              <Card><CardContent className="space-y-2">{models.map((item) => <button key={item.model.id} onClick={() => setModelId(item.model.id)}
                className={`block w-full rounded-md border p-3 text-left text-sm ${modelId === item.model.id ? 'border-brand bg-brand/5' : 'border-border'}`}>
                {item.model.name} · {item.model.version}<span className="block text-xs text-muted-foreground">{item.effective_state}</span>
              </button>)}{models.length === 0 && <p className="text-sm text-muted-foreground">{t('causalCuration.models.empty')}</p>}</CardContent></Card>
            {model && <Card><CardContent className="space-y-3 text-sm"><h2 className="font-semibold">{model.model.name} · {model.model.version}</h2>
              <p>{t('causalCuration.model.storedStatus', { state: model.model.review_state, effective: model.effective_state, count: model.active_opposition_count })}</p>
              <p className="text-xs text-muted-foreground">{t('causalCuration.model.variableVersions', { list: model.variables.map((v) => `${v.name}@${v.version}`).join(listSep) })}</p>
              <h3 className="font-medium">{t('causalCuration.model.dagTitle')}</h3>
              <CausalDagView model={model} onSelectEdge={(edge) => { setSection('claims'); setClaimId(edge.id); }} />
              <h3 className="font-medium">{t('causalCuration.model.edgesTitle')}</h3>{model.edges.map((edge) => <button key={edge.id} className="block w-full rounded-md border p-2 text-left" onClick={() => { setSection('claims'); setClaimId(edge.id); }}>
                {t('causalCuration.model.edgeLine', {
                  cause: edge.cause_variable, effect: edge.effect_variable,
                  lagMin: edge.lag_min_seconds, lagMax: edge.lag_max_seconds,
                  modality: edge.modality, state: edge.review_state,
                })}
              </button>)}
              <p className={hasCycle(model.edges) ? 'text-destructive' : 'text-muted-foreground'}>
                {hasCycle(model.edges) ? t('causalCuration.model.cycleDetected') : t('causalCuration.model.noCycle')}
              </p>
              {parentAdjustment?.status === 'graphically_admissible' && <p className="rounded-md border p-2 text-xs">
                {t('causalCuration.model.adjustmentAdmissible', {
                  vars: parentAdjustment.adjustment_variable_ids.length
                    ? parentAdjustment.adjustment_variable_ids.map((id) => model.variables.find((v) => v.id === id)?.name ?? id).join(listSep)
                    : t('causalCuration.model.adjustmentEmptySet'),
                })}
              </p>}
              {parentAdjustment?.status === 'unknown' && <p className="text-xs text-muted-foreground">
                {t('causalCuration.model.adjustmentUnknown', { reasons: parentAdjustment.reasons.join(reasonSep) })}
              </p>}
              <p className="text-xs text-muted-foreground">{t('causalCuration.model.edgeNote')}</p>
              <div className="flex flex-wrap gap-2"><Button disabled={busy || model.active_opposition_count > 0 || hasCycle(model.edges)} onClick={() => mutate(() => causalApi.reviewModel(scope, model, true, false), t('causalCuration.model.approveSuccess'))}>{t('causalCuration.model.approve')}</Button>
                {model.active_opposition_count > 0 && <Button disabled={busy || hasCycle(model.edges)} variant="outline" onClick={() => mutate(() => causalApi.reviewModel(scope, model, true, true), t('causalCuration.model.approveOppositionSuccess'))}>{t('causalCuration.model.approveWithOpposition')}</Button>}
                <Button disabled={busy} variant="outline" onClick={() => mutate(() => causalApi.reviewModel(scope, model, false, false), t('causalCuration.model.rejectSuccess'))}>{t('causalCuration.model.reject')}</Button></div>
              <CausalEffectPanel key={`${scope.tenant_id}:${scope.acl}:${model.model.id}`} scope={scope} model={model} />
              </CardContent></Card>}
            </div>
          </div>}
        </>}
      </div>
    </div>
  );
}
