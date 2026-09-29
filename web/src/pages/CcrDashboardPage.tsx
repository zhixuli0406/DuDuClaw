import { useState } from 'react';
import { useLocation } from 'react-router';
import { useIntl } from 'react-intl';
import { Database, RefreshCw } from 'lucide-react';
import { Button, Card, CardContent, CollectionPageHeader, Input } from '@/components/mds';
import { DemoModeNotice } from '@/components/DemoModeNotice';
import { ccrApi, type CcrDashboardSnapshot } from '@/lib/ccr-api';
import { CcrReplayPanel } from './CcrReplayPanel';

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function CcrDashboardPage() {
  const location = useLocation();
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const number = (value: number) => new Intl.NumberFormat(intl.locale).format(value);
  const [tenant, setTenant] = useState(() => new URLSearchParams(location.search).get('tenant_id') || 'local');
  const [snapshot, setSnapshot] = useState<CcrDashboardSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  async function refresh() {
    const exactTenant = tenant.trim();
    if (!exactTenant) { setError(t('ccrDashboard.tenantRequired')); return; }
    setLoading(true);
    setError('');
    setSnapshot(null);
    try { setSnapshot(await ccrApi.dashboard(exactTenant)); }
    catch (cause) { setError(errorText(cause)); }
    finally { setLoading(false); }
  }

  return <div className="flex h-full flex-col">
    <CollectionPageHeader icon={Database} title={t('ccrDashboard.title')} description={t('ccrDashboard.description')} />
    <div className="flex-1 space-y-4 overflow-y-auto p-5">
      {/* X1 方案 5 (2026-09-29 audit): the four-arm comparison this page shows
          has only ever run on synthetic fixtures and recorded claims. */}
      <DemoModeNotice pageKey="ccr-dashboard" />
      <Card><CardContent className="flex flex-wrap items-end gap-3">
        <label className="space-y-1 text-sm">{t('ccrDashboard.tenant')}
          <Input aria-label={t('ccrDashboard.tenant')} value={tenant} disabled={loading}
            onChange={(event) => { setTenant(event.target.value); setSnapshot(null); setError(''); }} />
        </label>
        <Button disabled={loading || !tenant.trim()} onClick={refresh}>
          <RefreshCw className="mr-2 size-4" />{t('ccrDashboard.refresh')}
        </Button>
      </CardContent></Card>

      <CcrReplayPanel key={tenant} tenant={tenant} />

      {error && <p role="alert" className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{error}</p>}
      {snapshot && <>
        <p role="status" className="rounded-md border px-3 py-2 text-sm">
          {snapshot.status === 'absent' ? t('ccrDashboard.absent') : t('ccrDashboard.ready')}
        </p>
        <Card><CardContent className="space-y-2">
          <h2 className="font-semibold">{t('ccrDashboard.causalOutbox')}</h2>
          {snapshot.causal_revocation_outbox.available ? <>
            <div className="grid gap-3 sm:grid-cols-2">
              <Count label={t('ccrDashboard.pendingNotices')} value={number(snapshot.causal_revocation_outbox.pending_notices)} />
              <Count label={t('ccrDashboard.oldestPendingSeconds')} value={snapshot.causal_revocation_outbox.oldest_pending_seconds === null
                ? '—' : number(snapshot.causal_revocation_outbox.oldest_pending_seconds)} />
            </div>
            <p className="text-xs text-muted-foreground">{t('ccrDashboard.causalOutboxNote')}</p>
          </> : <p className="text-sm text-amber-700 dark:text-amber-300">
            {snapshot.causal_revocation_outbox.reason === 'legacy_schema'
              ? t('ccrDashboard.causalOutboxLegacy')
              : snapshot.causal_revocation_outbox.reason === 'source_db_absent'
                ? t('ccrDashboard.causalOutboxAbsent')
                : t('ccrDashboard.causalOutboxUnavailable')}
          </p>}
        </CardContent></Card>
        <Card><CardContent className="space-y-2">
          <h2 className="font-semibold">{t('ccrDashboard.causalDelivery')}</h2>
          {snapshot.causal_delivery.available ? <>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
              <Count label={t('ccrDashboard.activeLeases')} value={number(snapshot.causal_delivery.active_leases)} />
              <Count label={t('ccrDashboard.oldestLeaseSeconds')} value={snapshot.causal_delivery.oldest_lease_seconds === null
                ? '—' : number(snapshot.causal_delivery.oldest_lease_seconds)} />
              <Count label={t('ccrDashboard.revokingSources')} value={number(snapshot.causal_delivery.revoking_sources)} />
              <Count label={t('ccrDashboard.oldestRevokingSeconds')} value={snapshot.causal_delivery.oldest_revoking_seconds === null
                ? '—' : number(snapshot.causal_delivery.oldest_revoking_seconds)} />
            </div>
            <p className="text-xs text-muted-foreground">{t('ccrDashboard.causalDeliveryNote')}</p>
          </> : <p className="text-sm text-amber-700 dark:text-amber-300">
            {snapshot.causal_delivery.reason === 'legacy_schema'
              ? t('ccrDashboard.causalDeliveryLegacy')
              : snapshot.causal_delivery.reason === 'source_db_absent'
                ? t('ccrDashboard.causalDeliveryAbsent')
                : t('ccrDashboard.causalDeliveryUnavailable')}
          </p>}
        </CardContent></Card>
        <Card><CardContent className="space-y-2">
          <h2 className="font-semibold">{t('ccrDashboard.connectorLifecycle')}</h2>
          {snapshot.connector_lifecycle.available ? <>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <Count label={t('ccrDashboard.connectorPending')} value={number(snapshot.connector_lifecycle.pending_events)} />
              <Count label={t('ccrDashboard.connectorBlocked')} value={number(snapshot.connector_lifecycle.blocked_events)} />
              <Count label={t('ccrDashboard.connectorCompleted')} value={number(snapshot.connector_lifecycle.completed_events)} />
              <Count label={t('ccrDashboard.connectorOldest')} value={snapshot.connector_lifecycle.oldest_pending_seconds === null
                ? '—' : number(snapshot.connector_lifecycle.oldest_pending_seconds)} />
              <Count label={t('ccrDashboard.connectorLatestCompleted')} value={snapshot.connector_lifecycle.latest_completed_seconds === null
                ? '—' : number(snapshot.connector_lifecycle.latest_completed_seconds)} />
              <Count label={t('ccrDashboard.connectorFailure')} value={snapshot.connector_lifecycle.last_failure_code === null
                ? '—' : t(`ccrDashboard.connectorFailure.${snapshot.connector_lifecycle.last_failure_code}`)} />
            </div>
            <p className="text-xs text-muted-foreground">{t('ccrDashboard.connectorLifecycleNote')}</p>
          </> : <p className="text-sm text-amber-700 dark:text-amber-300">
            {snapshot.connector_lifecycle.reason === 'legacy_schema'
              ? t('ccrDashboard.connectorLifecycleLegacy')
              : snapshot.connector_lifecycle.reason === 'source_db_absent'
                ? t('ccrDashboard.connectorLifecycleAbsent')
                : t('ccrDashboard.connectorLifecycleUnavailable')}
          </p>}
        </CardContent></Card>
        {snapshot.status === 'ready' && <>
          <section aria-label={t('ccrDashboard.entries')} className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <CountCard label={t('ccrDashboard.eligibleRetained')} value={number(snapshot.entries.eligible_retained)} />
            <CountCard label={t('ccrDashboard.eligibleBytes')} value={number(snapshot.entries.declared_original_bytes_eligible)} />
            <CountCard label={t('ccrDashboard.expiredRetained')} value={number(snapshot.entries.expired_retained)} />
            <CountCard label={t('ccrDashboard.revokedRetained')} value={number(snapshot.entries.revoked_retained)} />
          </section>
          <p className="text-xs text-muted-foreground">{t('ccrDashboard.eligibilityNote')}</p>
          <Card><CardContent className="space-y-2">
            <h2 className="font-semibold">{t('ccrDashboard.revocations')}</h2>
            <div className="grid gap-3 sm:grid-cols-3">
              <Count label={t('ccrDashboard.sourceCalls')} value={number(snapshot.revocations.source_calls)} />
              <Count label={t('ccrDashboard.scopes')} value={number(snapshot.revocations.scopes)} />
              <Count label={t('ccrDashboard.artifactVersions')} value={number(snapshot.revocations.artifact_versions)} />
            </div>
          </CardContent></Card>
          <Card><CardContent className="space-y-2">
            <h2 className="font-semibold">{t('ccrDashboard.retrievalAudit')}</h2>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'ccrDashboard.auditWindow' }, { rows: number(snapshot.retrieval_audit.window_max_rows) })}</p>
            <div className="grid gap-3 sm:grid-cols-3">
              <Count label={t('ccrDashboard.grants')} value={number(snapshot.retrieval_audit.grants)} />
              <Count label={t('ccrDashboard.refusals')} value={number(snapshot.retrieval_audit.refusals)} />
              <Count label={t('ccrDashboard.returnedBytes')} value={number(snapshot.retrieval_audit.returned_bytes_granted)} />
            </div>
          </CardContent></Card>
          {!snapshot.compression_metrics.available ? <p className="rounded-md border border-amber-500/40 bg-amber-500/5 p-3 text-sm">
            {t('ccrDashboard.metricsUnavailable')}
          </p> : <Card><CardContent className="space-y-3">
            <h2 className="font-semibold">{t('ccrDashboard.loopMetrics')}</h2>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'ccrDashboard.loopWindow' }, { rows: number(snapshot.compression_metrics.window_max_rows) })}</p>
            <div className="grid gap-3 sm:grid-cols-3">
              <Count label={t('ccrDashboard.observedLoops')} value={number(snapshot.compression_metrics.observed_loops)} />
              <Count label={t('ccrDashboard.usageCoverage')} value={`${number(snapshot.compression_metrics.usage_reported_rounds)} / ${number(snapshot.compression_metrics.provider_rounds)}`} />
              <Count label={t('ccrDashboard.p95LoopMillis')} value={snapshot.compression_metrics.p95_elapsed_millis === null ? '—' : number(snapshot.compression_metrics.p95_elapsed_millis)} />
            </div>
            {snapshot.compression_metrics.observed_loops === 0 ? <p className="text-sm text-muted-foreground">{t('ccrDashboard.noObservations')}</p> : <>
              <h3 className="text-sm font-medium">{t('ccrDashboard.providerUsage')}</h3>
              <div className="grid gap-3 sm:grid-cols-3 lg:grid-cols-5">
                <Count label={t('ccrDashboard.inputTokens')} value={number(snapshot.compression_metrics.input_tokens)} />
                <Count label={t('ccrDashboard.outputTokens')} value={number(snapshot.compression_metrics.output_tokens)} />
                <Count label={t('ccrDashboard.cacheReadTokens')} value={number(snapshot.compression_metrics.cache_read_tokens)} />
                <Count label={t('ccrDashboard.cacheWriteTokens')} value={number(snapshot.compression_metrics.cache_write_tokens)} />
                <Count label={t('ccrDashboard.reasoningTokens')} value={number(snapshot.compression_metrics.reasoning_tokens)} />
              </div>
              <h3 className="text-sm font-medium">{t('ccrDashboard.compression')}</h3>
              <div className="grid gap-3 sm:grid-cols-3">
                <Count label={t('ccrDashboard.compressedResults')} value={number(snapshot.compression_metrics.ccr_compressed_results)} />
                <Count label={t('ccrDashboard.originalBytes')} value={number(snapshot.compression_metrics.ccr_original_bytes)} />
                <Count label={t('ccrDashboard.deliveredBytes')} value={number(snapshot.compression_metrics.ccr_delivered_bytes)} />
              </div>
              <h3 className="text-sm font-medium">{t('ccrDashboard.findRetrieve')}</h3>
              <div className="grid gap-3 sm:grid-cols-3 lg:grid-cols-4">
                <Count label={t('ccrDashboard.findAttempts')} value={number(snapshot.compression_metrics.ccr_find_attempts)} />
                <Count label={t('ccrDashboard.findHits')} value={number(snapshot.compression_metrics.ccr_find_hits)} />
                <Count label={t('ccrDashboard.findMisses')} value={number(snapshot.compression_metrics.ccr_find_misses)} />
                <Count label={t('ccrDashboard.retrieveAttempts')} value={number(snapshot.compression_metrics.ccr_retrieve_attempts)} />
                <Count label={t('ccrDashboard.retrieveSuccesses')} value={number(snapshot.compression_metrics.ccr_retrieve_successes)} />
                <Count label={t('ccrDashboard.retrieveMisses')} value={number(snapshot.compression_metrics.ccr_retrieve_misses)} />
                <Count label={t('ccrDashboard.retrievedBytes')} value={number(snapshot.compression_metrics.ccr_retrieved_bytes)} />
              </div>
            </>}
            <p className="text-xs text-muted-foreground">{t('ccrDashboard.metricsNote')}</p>
          </CardContent></Card>}
          <p className="text-xs text-muted-foreground">{t('ccrDashboard.privacyNote')}</p>
        </>}
      </>}
    </div>
  </div>;
}

function CountCard({ label, value }: { label: string; value: string }) {
  return <Card><CardContent><Count label={label} value={value} /></CardContent></Card>;
}

function Count({ label, value }: { label: string; value: string }) {
  return <div><p className="text-xs text-muted-foreground">{label}</p><p className="text-xl font-semibold tabular-nums">{value}</p></div>;
}
