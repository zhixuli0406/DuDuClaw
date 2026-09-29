import { useEffect, useRef, useState } from 'react';
import { useLocation, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { FlaskConical, Play, RefreshCw, ShieldAlert, Trash2 } from 'lucide-react';
import { Badge, Button, Card, CardContent, CollectionPageHeader, Input } from '@/components/mds';
import { decisionApi, type DecisionArtifact, type DecisionBrief, type DecisionCatalog, type DecisionEmpiricalInput, type DecisionEmpiricalReport, type DecisionEngineeringValidation, type DecisionEventComparison, type DecisionForecastValidation, type DecisionOverview, type DecisionPilotUploadReceipt, type DecisionShadowMonitor, type PilotReviewStatus, type PolicySweepReport, type SensitivityReport, type ShadowMonitorAssessment, type SimulationResult, type SyntheticLifecycleKind } from '@/lib/decision-api';
import { parsePilotReviewDeepLink } from '@/lib/decision-review-link';
import { DemoModeNotice } from '@/components/DemoModeNotice';
import { useDecisionEnabled } from '@/hooks/useDecisionEnabled';
import { DecisionPilotUploadPanel } from './DecisionPilotUploadPanel';
import { DecisionEventComparePanel } from './DecisionEventComparePanel';
import { DecisionForecastValidationPanel } from './DecisionForecastValidationPanel';
import { DecisionCandidateModelPanel } from './DecisionCandidateModelPanel';
import { DecisionOutcomeReviewPanel } from './DecisionOutcomeReviewPanel';
import { DecisionShadowWorkflowPanel } from './DecisionShadowWorkflowPanel';
import { DecisionSlaShadowWorkflowPanel } from './DecisionSlaShadowWorkflowPanel';

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

type DecisionTranslator = (id: string, values?: Record<string, string | number>) => string;

function dateTime(value: string | number, locale: string): string {
  const date = typeof value === 'number' ? new Date(value * 1000) : new Date(value);
  return Number.isNaN(date.getTime()) ? String(value) : new Intl.DateTimeFormat(locale, {
    dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC',
  }).format(date);
}

function syntheticPilotIdentity(snapshotId: string): { seed: number; days: number } | null {
  const match = /^synthetic-support-(\d+)-(\d+)$/.exec(snapshotId);
  if (!match) return null;
  const seed = Number(match[1]);
  const days = Number(match[2]);
  return Number.isSafeInteger(seed) && seed >= 0 && Number.isInteger(days) && days >= 21 && days <= 90
    ? { seed, days } : null;
}

export function DecisionLabPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const location = useLocation();
  // `config.toml [decision] enabled`, fail-open (see `useDecisionEnabled`).
  const decisionEnabled = useDecisionEnabled();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [tenant, setTenant] = useState('');
  const [acl, setAcl] = useState('');
  const [overview, setOverview] = useState<DecisionOverview | null>(null);
  const [catalog, setCatalog] = useState<DecisionCatalog | null>(null);
  const [seed, setSeed] = useState(23);
  const [days, setDays] = useState(30);
  const [lifecycleKind, setLifecycleKind] = useState<SyntheticLifecycleKind>('version_changed');
  const [snapshotId, setSnapshotId] = useState('');
  const [modelVersion, setModelVersion] = useState('');
  const [baselineScenarioId, setBaselineScenarioId] = useState('');
  const [alternativeScenarioId, setAlternativeScenarioId] = useState('');
  const [pilotSourceId, setPilotSourceId] = useState('');
  const [brief, setBrief] = useState<DecisionBrief | null>(null);
  const [baselineReplay, setBaselineReplay] = useState<SimulationResult | null>(null);
  const [alternativeReplay, setAlternativeReplay] = useState<SimulationResult | null>(null);
  const [policySweep, setPolicySweep] = useState<PolicySweepReport | null>(null);
  const [sensitivity, setSensitivity] = useState<SensitivityReport | null>(null);
  const [engineeringValidation, setEngineeringValidation] = useState<DecisionEngineeringValidation | null>(null);
  const [shadowPolicyId, setShadowPolicyId] = useState('');
  const [shadowMonitor, setShadowMonitor] = useState<DecisionShadowMonitor | null>(null);
  const [reviewScenario, setReviewScenario] = useState<'baseline' | 'alternative'>('baseline');
  const [reviewSummary, setReviewSummary] = useState('');
  const [approvalId, setApprovalId] = useState('');
  const [pilotReview, setPilotReview] = useState<PilotReviewStatus | null>(null);
  const [reviewCheckedAt, setReviewCheckedAt] = useState('');
  const [agentsPerDay, setAgentsPerDay] = useState(4);
  const [maxAddedAgents, setMaxAddedAgents] = useState(1);
  const [maxAgentDays, setMaxAgentDays] = useState(270);
  const [maxStaffCost, setMaxStaffCost] = useState(2_700_000);
  const [maxFinalBacklog, setMaxFinalBacklog] = useState(100);
  const [capacityMin, setCapacityMin] = useState(2);
  const [capacityMax, setCapacityMax] = useState(12);
  const [sensitivityRuns, setSensitivityRuns] = useState(100);
  const [arrivalDelta, setArrivalDelta] = useState(2);
  const [sensitivityCapacityMin, setSensitivityCapacityMin] = useState(2);
  const [sensitivityCapacityMax, setSensitivityCapacityMax] = useState(12);
  const [sensitivityMaxBacklog, setSensitivityMaxBacklog] = useState(100);
  const [sensitivityMaxCost, setSensitivityMaxCost] = useState(2_700_000);
  const [confirmScrub, setConfirmScrub] = useState(false);
  const [candidateScreenPrefill, setCandidateScreenPrefill] = useState({ scopeKey: '', hash: '', serial: 0 });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const hadReviewQuery = useRef(false);
  const importEpoch = useRef(0);

  // The Inbox link contains selectors only. Re-fetch the scoped catalog,
  // comparison, both replays, and broker status before showing any trajectory.
  useEffect(() => {
    if (!new URLSearchParams(location.search).has('approval_id')) {
      if (hadReviewQuery.current) {
        hadReviewQuery.current = false;
        setBusy(false); setError(''); setNotice('');
        setOverview(null); setCatalog(null); setBrief(null);
        setBaselineReplay(null); setAlternativeReplay(null);
        setShadowMonitor(null);
        setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
      }
      return;
    }
    hadReviewQuery.current = true;
    const link = parsePilotReviewDeepLink(location.search);
    if (!link) {
      setBusy(false); setNotice('');
      setOverview(null); setCatalog(null); setBrief(null);
      setBaselineReplay(null); setAlternativeReplay(null); setPilotReview(null);
      setShadowMonitor(null);
      setApprovalId(''); setReviewCheckedAt('');
      setError(t('decisionLab.deepLinkMismatch'));
      return;
    }
    let cancelled = false;
    setBusy(true); setError(''); setNotice('');
    setOverview(null); setCatalog(null); setBrief(null);
    setBaselineReplay(null); setAlternativeReplay(null); setPilotReview(null);
    setShadowMonitor(null);
    setApprovalId(''); setReviewCheckedAt('');
    async function openReviewLink() {
      try {
        const scope = { tenant_id: link!.tenant_id, acl: link!.acl };
        const [nextOverview, nextCatalog] = await Promise.all([decisionApi.overview(scope), decisionApi.catalog(scope)]);
        const pilot = [...(nextCatalog.uploaded_pilots ?? []), ...nextCatalog.synthetic_pilots].find((item) =>
          item.snapshot_id === link!.snapshot_id && item.model_version === link!.model_version
          && (item.baseline_scenario_id === link!.scenario_id || item.alternative_scenario_id === link!.scenario_id));
        if (!pilot) throw new Error(t('decisionLab.deepLinkMismatch'));
        const { brief: nextBrief } = await decisionApi.compare({ ...scope,
          snapshot_id: pilot.snapshot_id, model_version: pilot.model_version,
          baseline_scenario_id: pilot.baseline_scenario_id, alternative_scenario_id: pilot.alternative_scenario_id,
        });
        const selectedHash = pilot.baseline_scenario_id === link!.scenario_id
          ? nextBrief.replay.baseline_replay_hash : nextBrief.replay.alternative_replay_hash;
        if (selectedHash !== link!.expected_hash) throw new Error(t('decisionLab.deepLinkMismatch'));
        const [baseline, alternative] = await Promise.all([
          decisionApi.replay({ ...scope, snapshot_id: pilot.snapshot_id, model_version: pilot.model_version,
            scenario_id: pilot.baseline_scenario_id, expected_hash: nextBrief.replay.baseline_replay_hash }),
          decisionApi.replay({ ...scope, snapshot_id: pilot.snapshot_id, model_version: pilot.model_version,
            scenario_id: pilot.alternative_scenario_id, expected_hash: nextBrief.replay.alternative_replay_hash }),
        ]);
        if (baseline.result.replay_hash !== nextBrief.replay.baseline_replay_hash
          || alternative.result.replay_hash !== nextBrief.replay.alternative_replay_hash
          || baseline.result.scenario_id !== pilot.baseline_scenario_id
          || alternative.result.scenario_id !== pilot.alternative_scenario_id) {
          throw new Error(t('decisionLab.deepLinkMismatch'));
        }
        const { review } = await decisionApi.pilotReviewStatus(link!);
        if (review.link.approval_id !== link!.approval_id || review.link.replay_hash !== selectedHash
          || review.link.snapshot_id !== pilot.snapshot_id || review.link.scenario_id !== link!.scenario_id) {
          throw new Error(t('decisionLab.deepLinkMismatch'));
        }
        if (cancelled) return;
        setTenant(scope.tenant_id); setAcl(scope.acl);
        setOverview(nextOverview); setCatalog(nextCatalog);
        setSnapshotId(pilot.snapshot_id); setModelVersion(pilot.model_version);
        setBaselineScenarioId(pilot.baseline_scenario_id); setAlternativeScenarioId(pilot.alternative_scenario_id);
        setBrief(nextBrief); setBaselineReplay(baseline.result); setAlternativeReplay(alternative.result);
        setReviewScenario(pilot.baseline_scenario_id === link!.scenario_id ? 'baseline' : 'alternative');
        setApprovalId(link!.approval_id); setPilotReview(review); setReviewCheckedAt(new Date().toISOString());
      } catch (cause) { if (!cancelled) setError(errorMessage(cause)); }
      finally { if (!cancelled) setBusy(false); }
    }
    void openReviewLink();
    return () => { cancelled = true; };
  // URL changes are the only trigger. Fetched results are checked before commit.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.search]);

  function changeScope(field: 'tenant' | 'acl', value: string) {
    importEpoch.current += 1;
    if (field === 'tenant') setTenant(value);
    else setAcl(value);
    setOverview(null);
    setCatalog(null);
    setSnapshotId(''); setModelVersion(''); setBaselineScenarioId(''); setAlternativeScenarioId('');
    setPilotSourceId(''); setBrief(null); setBaselineReplay(null); setAlternativeReplay(null);
    setPolicySweep(null); setSensitivity(null);
    setEngineeringValidation(null);
    setShadowMonitor(null);
    setShadowPolicyId('');
    setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
    setConfirmScrub(false);
    setError(''); setNotice('');
  }

  async function load() {
    importEpoch.current += 1;
    const scope = { tenant_id: tenant.trim(), acl: acl.trim() };
    if (!scope.tenant_id || !scope.acl) { setError(t('decisionLab.scopeRequired')); return; }
    setBusy(true); setError(''); setNotice(''); setConfirmScrub(false);
    setPilotReview(null); setReviewCheckedAt('');
    try {
      const [nextOverview, nextCatalog] = await Promise.all([decisionApi.overview(scope), decisionApi.catalog(scope)]);
      setOverview(nextOverview); setCatalog(nextCatalog);
      const pilot = [...(nextCatalog.uploaded_pilots ?? []), ...nextCatalog.synthetic_pilots].find((item) =>
        nextCatalog.snapshots.some((snapshot) => snapshot.id === item.snapshot_id)
        && nextCatalog.models.some((model) => model.id === item.model_version)
        && nextCatalog.scenarios.some((scenario) => scenario.id === item.baseline_scenario_id)
        && nextCatalog.scenarios.some((scenario) => scenario.id === item.alternative_scenario_id));
      setSnapshotId(pilot?.snapshot_id ?? nextCatalog.snapshots[0]?.id ?? '');
      setModelVersion(pilot?.model_version ?? nextCatalog.models[0]?.id ?? '');
      setBaselineScenarioId(pilot?.baseline_scenario_id ?? '');
      setAlternativeScenarioId(pilot?.alternative_scenario_id ?? '');
      setBrief(null); setBaselineReplay(null); setAlternativeReplay(null); setPilotSourceId('');
      setPolicySweep(null); setSensitivity(null);
      setEngineeringValidation(null);
      setShadowMonitor(null);
      setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
    }
    catch (cause) { setError(errorMessage(cause)); setOverview(null); setCatalog(null); }
    finally { setBusy(false); }
  }

  async function finishUploadedPilot(receipt: DecisionPilotUploadReceipt, current: () => boolean) {
    if (!overview) throw new Error('scope unavailable');
    const epoch = importEpoch.current;
    const scope = { tenant_id: overview.tenant_id, acl: overview.acl };
    const [nextCatalog, nextOverview] = await Promise.all([decisionApi.catalog(scope), decisionApi.overview(scope)]);
    if (!current() || importEpoch.current !== epoch) return;
    const pair = nextCatalog.uploaded_pilots?.find((item) =>
      item.snapshot_id === receipt.snapshot_id && item.model_version === receipt.model_version
      && item.baseline_scenario_id === receipt.baseline_scenario_id
      && item.alternative_scenario_id === receipt.alternative_scenario_id);
    if (!pair || nextOverview.tenant_id !== scope.tenant_id || nextOverview.acl !== scope.acl
      || !nextCatalog.snapshots.some((item) => item.id === receipt.snapshot_id && item.queue_id === receipt.queue_id
        && item.source_kinds.includes('local_support_pilot_export'))
      || !nextCatalog.models.some((item) => item.id === receipt.model_version)
      || !nextCatalog.scenarios.some((item) => item.id === receipt.baseline_scenario_id)
      || !nextCatalog.scenarios.some((item) => item.id === receipt.alternative_scenario_id)) {
      throw new Error('imported pair unavailable');
    }
    setCatalog(nextCatalog); setOverview(nextOverview);
    setSnapshotId(receipt.snapshot_id); setModelVersion(receipt.model_version);
    setBaselineScenarioId(receipt.baseline_scenario_id); setAlternativeScenarioId(receipt.alternative_scenario_id);
    setPilotSourceId(''); setBrief(null); setBaselineReplay(null); setAlternativeReplay(null);
    setPolicySweep(null); setSensitivity(null); setEngineeringValidation(null);
    setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
    setError(''); setNotice('');
  }

  async function createPilot() {
    if (!overview || !Number.isSafeInteger(seed) || seed < 0 || !Number.isInteger(days) || days < 21 || days > 90) {
      setError(t('decisionLab.pilotInvalid')); return;
    }
    const scope = { tenant_id: overview.tenant_id, acl: overview.acl };
    setBusy(true); setError(''); setNotice(''); setBrief(null); setBaselineReplay(null); setAlternativeReplay(null); setPolicySweep(null); setSensitivity(null);
    setEngineeringValidation(null);
    setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
    try {
      const created = await decisionApi.createSyntheticPilot(scope, { seed, days });
      const [nextCatalog, nextOverview] = await Promise.all([decisionApi.catalog(scope), decisionApi.overview(scope)]);
      setCatalog(nextCatalog); setOverview(nextOverview);
      setSnapshotId(created.snapshot_id); setModelVersion(created.model_version);
      setBaselineScenarioId(created.baseline_scenario_id); setAlternativeScenarioId(created.alternative_scenario_id);
      setPilotSourceId(created.source_artifact_id);
      setMaxAgentDays(days * 3); setMaxStaffCost(days * 30_000); setMaxFinalBacklog(days * 3);
      setSensitivityMaxCost(days * 30_000); setSensitivityMaxBacklog(days * 3);
      setNotice(t('decisionLab.pilotCreated'));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function stageSyntheticLifecycle() {
    const selected = catalog?.synthetic_pilots.find((pilot) =>
      pilot.snapshot_id === snapshotId && pilot.model_version === modelVersion
      && pilot.baseline_scenario_id === baselineScenarioId && pilot.alternative_scenario_id === alternativeScenarioId);
    const identity = selected ? syntheticPilotIdentity(selected.snapshot_id) : null;
    if (!overview || !identity) { setError(t('decisionLab.lifecycleSelectPilot')); return; }
    const scope = { tenant_id: overview.tenant_id, acl: overview.acl };
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await decisionApi.stageSyntheticLifecycle(scope, { ...identity, kind: lifecycleKind });
      setBrief(null); setBaselineReplay(null); setAlternativeReplay(null);
      setPolicySweep(null); setSensitivity(null); setEngineeringValidation(null);
      setShadowMonitor(null);
      setApprovalId(''); setPilotReview(null); setReviewCheckedAt(''); setPilotSourceId('');
      const [nextOverview, nextCatalog] = await Promise.all([decisionApi.overview(scope), decisionApi.catalog(scope)]);
      setOverview(nextOverview); setCatalog(nextCatalog);
      setSnapshotId(''); setModelVersion(''); setBaselineScenarioId(''); setAlternativeScenarioId('');
      setNotice(t('decisionLab.lifecycleStaged', { outcome: t(`decisionLab.lifecycleOutcome.${result.stage_outcome}`) }));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function loadShadowMonitor() {
    if (!overview || !shadowPolicyId.trim()) { setError(t('decisionLab.shadowPolicyRequired')); return; }
    setBusy(true); setError(''); setNotice(''); setShadowMonitor(null);
    try {
      const report = await decisionApi.shadowMonitor(
        { tenant_id: overview.tenant_id, acl: overview.acl }, shadowPolicyId.trim(),
      );
      if (report.policy.id !== shadowPolicyId.trim()) throw new Error(t('decisionLab.shadowPolicyMismatch'));
      setShadowMonitor(report);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function compareScenarios() {
    if (!overview || !snapshotId || !modelVersion || !baselineScenarioId || !alternativeScenarioId
      || baselineScenarioId === alternativeScenarioId) {
      setError(t('decisionLab.selectValidScenarios')); return;
    }
    setBusy(true); setError(''); setNotice(''); setBrief(null); setBaselineReplay(null); setAlternativeReplay(null); setPolicySweep(null); setSensitivity(null);
    setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
    try {
      const { brief: nextBrief } = await decisionApi.compare({
        tenant_id: overview.tenant_id, acl: overview.acl, snapshot_id: snapshotId,
        model_version: modelVersion, baseline_scenario_id: baselineScenarioId,
        alternative_scenario_id: alternativeScenarioId,
      });
      setBrief(nextBrief);
      setNotice(t('decisionLab.comparisonReady'));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  function changeSelection(setter: (value: string) => void, value: string) {
    setter(value);
    setBrief(null); setBaselineReplay(null); setAlternativeReplay(null); setPolicySweep(null); setSensitivity(null);
    setEngineeringValidation(null);
    setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
    setError(''); setNotice('');
  }

  async function runEngineeringValidation() {
    if (!overview || !selectedReviewPilot) return;
    setBusy(true); setError(''); setNotice(''); setEngineeringValidation(null);
    try {
      const { report } = await decisionApi.engineeringValidation({
        tenant_id: overview.tenant_id, acl: overview.acl,
        snapshot_id: selectedReviewPilot.snapshot_id, model_version: selectedReviewPilot.model_version,
      });
      setEngineeringValidation(report);
      setNotice(t('decisionLab.engineeringComplete'));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function verifyReplays() {
    if (!overview || !brief) return;
    setBusy(true); setError(''); setNotice(''); setBaselineReplay(null); setAlternativeReplay(null);
    setPilotReview(null); setReviewCheckedAt('');
    try {
      const scope = { tenant_id: overview.tenant_id, acl: overview.acl, snapshot_id: snapshotId, model_version: modelVersion };
      const [baseline, alternative] = await Promise.all([
        decisionApi.replay({ ...scope, scenario_id: baselineScenarioId, expected_hash: brief.replay.baseline_replay_hash }),
        decisionApi.replay({ ...scope, scenario_id: alternativeScenarioId, expected_hash: brief.replay.alternative_replay_hash }),
      ]);
      if (baseline.result.replay_hash !== brief.replay.baseline_replay_hash
        || alternative.result.replay_hash !== brief.replay.alternative_replay_hash) {
        throw new Error(t('decisionLab.replayMismatch'));
      }
      setBaselineReplay(baseline.result); setAlternativeReplay(alternative.result);
      setNotice(t('decisionLab.replayVerified'));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  function reviewSelection() {
    if (!overview || !selectedReviewPilot || !brief || !baselineReplay || !alternativeReplay) return null;
    const scenario_id = reviewScenario === 'baseline' ? baselineScenarioId : alternativeScenarioId;
    const expected_hash = reviewScenario === 'baseline'
      ? brief.replay.baseline_replay_hash : brief.replay.alternative_replay_hash;
    const verified = reviewScenario === 'baseline' ? baselineReplay : alternativeReplay;
    if (verified.scenario_id !== scenario_id || verified.replay_hash !== expected_hash) return null;
    return { tenant_id: overview.tenant_id, acl: overview.acl, snapshot_id: snapshotId,
      model_version: modelVersion, scenario_id, expected_hash };
  }

  async function requestReview() {
    const selection = reviewSelection();
    if (!selection || !reviewSummary.trim()) { setError(t('decisionLab.reviewInvalid')); return; }
    setBusy(true); setError(''); setNotice(''); setPilotReview(null); setApprovalId(''); setReviewCheckedAt('');
    try {
      const { review } = await decisionApi.requestPilotReview({ ...selection, summary: reviewSummary.trim(), ttl_seconds: 3600 });
      if (review.link.replay_hash !== selection.expected_hash || review.link.scenario_id !== selection.scenario_id) {
        throw new Error(t('decisionLab.replayMismatch'));
      }
      setApprovalId(review.link.approval_id); setPilotReview(review); setReviewCheckedAt(new Date().toISOString());
      setNotice(t('decisionLab.reviewRequested'));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function checkReview() {
    const selection = reviewSelection();
    if (!selection || !approvalId.trim()) { setError(t('decisionLab.reviewInvalid')); return; }
    setBusy(true); setError(''); setNotice(''); setPilotReview(null); setReviewCheckedAt('');
    try {
      const { review } = await decisionApi.pilotReviewStatus({ ...selection, approval_id: approvalId.trim() });
      if (review.link.replay_hash !== selection.expected_hash || review.link.scenario_id !== selection.scenario_id
        || review.link.approval_id !== approvalId.trim()) throw new Error(t('decisionLab.replayMismatch'));
      setPilotReview(review); setReviewCheckedAt(new Date().toISOString());
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function runPolicySweep() {
    if (!overview || !brief) return;
    const snapshot = catalog?.snapshots.find((item) => item.id === snapshotId);
    if (!snapshot || !isPositiveInt(snapshot.horizon_days) || !isNonNegativeInt(agentsPerDay) || agentsPerDay > 4_294_967_295
      || !isNonNegativeInt(maxAddedAgents) || maxAddedAgents > 4_294_967_295 || !isNonNegativeInt(maxAgentDays)
      || !isNonNegativeInt(maxStaffCost) || !isNonNegativeInt(maxFinalBacklog)
      || !isPositiveInt(capacityMin) || !isPositiveInt(capacityMax) || capacityMin > capacityMax
      || capacityMax > 512 || capacityMax - capacityMin > 511) {
      setError(t('decisionLab.sweepInvalid')); return;
    }
    setBusy(true); setError(''); setNotice(''); setPolicySweep(null);
    try {
      const { report } = await decisionApi.policySweep({
        tenant_id: overview.tenant_id, acl: overview.acl, snapshot_id: snapshotId, model_version: modelVersion,
        baseline_scenario_id: baselineScenarioId, alternative_scenario_id: alternativeScenarioId,
        resource_plan: {
          available_agents_by_day: Array(snapshot.horizon_days).fill(agentsPerDay),
          max_added_agents_per_day: maxAddedAgents, max_total_agent_days: maxAgentDays,
          max_staff_cost_cents: maxStaffCost, max_final_backlog: maxFinalBacklog,
          service_capacity_band: { min: capacityMin, max: capacityMax },
        },
      });
      setPolicySweep(report); setNotice(t('decisionLab.sweepComplete'));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function runSensitivity() {
    if (!overview || !brief || !isPositiveInt(sensitivityRuns) || sensitivityRuns > 1000
      || !isNonNegativeInt(arrivalDelta) || arrivalDelta > 1000
      || !isPositiveInt(sensitivityCapacityMin) || !isPositiveInt(sensitivityCapacityMax)
      || sensitivityCapacityMin > sensitivityCapacityMax || sensitivityCapacityMax > 512 || !isNonNegativeInt(sensitivityMaxBacklog)
      || !isNonNegativeInt(sensitivityMaxCost)) {
      setError(t('decisionLab.sensitivityInvalid')); return;
    }
    setBusy(true); setError(''); setNotice(''); setSensitivity(null);
    try {
      const { report } = await decisionApi.sensitivity({
        tenant_id: overview.tenant_id, acl: overview.acl, snapshot_id: snapshotId, model_version: modelVersion,
        baseline_scenario_id: baselineScenarioId, alternative_scenario_id: alternativeScenarioId,
        runs: sensitivityRuns, arrival_delta: arrivalDelta, capacity_min: sensitivityCapacityMin,
        capacity_max: sensitivityCapacityMax, max_final_backlog: sensitivityMaxBacklog,
        max_staff_cost_cents: sensitivityMaxCost,
      });
      setSensitivity(report); setNotice(t('decisionLab.sensitivityComplete'));
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function scrub() {
    if (!overview) return;
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await decisionApi.scrubTicketSources({ tenant_id: overview.tenant_id, acl: overview.acl });
      setOverview(result.overview);
      setCatalog(null); setSnapshotId(''); setModelVersion(''); setBaselineScenarioId(''); setAlternativeScenarioId('');
      setPilotSourceId(''); setBrief(null); setBaselineReplay(null); setAlternativeReplay(null);
      setPolicySweep(null); setSensitivity(null);
      setEngineeringValidation(null);
      setApprovalId(''); setPilotReview(null); setReviewCheckedAt('');
      setNotice(intl.formatMessage({ id: 'decisionLab.scrubbed' }, { count: result.scrubbed }));
      setConfirmScrub(false);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  const counts = overview ? Object.entries(overview.counts).sort(([a], [b]) => a.localeCompare(b)) : [];
  const selectedSyntheticPilot = catalog?.synthetic_pilots.find((pilot) =>
    pilot.snapshot_id === snapshotId && pilot.model_version === modelVersion
    && pilot.baseline_scenario_id === baselineScenarioId && pilot.alternative_scenario_id === alternativeScenarioId);
  const selectedUploadedPilot = catalog?.uploaded_pilots?.find((pilot) =>
    pilot.snapshot_id === snapshotId && pilot.model_version === modelVersion
    && pilot.baseline_scenario_id === baselineScenarioId && pilot.alternative_scenario_id === alternativeScenarioId);
  const selectedReviewPilot = selectedSyntheticPilot ?? selectedUploadedPilot;
  const outcomeScopeKey = overview ? JSON.stringify([overview.tenant_id, overview.acl,
    overview.generated_at_utc, overview.invalidated_inputs, overview.ticket_sources, overview.artifacts]) : '';
  const uploadedHoldout = selectedUploadedPilot?.sla_holdout_id
    && selectedUploadedPilot.sla_holdout_sha256 && selectedUploadedPilot.source_sha256
    && [selectedUploadedPilot.sla_holdout_sha256, selectedUploadedPilot.source_sha256]
      .every((hash) => /^[a-f0-9]{64}$/.test(hash))
    ? { id: selectedUploadedPilot.sla_holdout_id,
      digest: selectedUploadedPilot.sla_holdout_sha256, sourceSha256: selectedUploadedPilot.source_sha256 }
    : null;
  const lifecycleTarget = selectedSyntheticPilot ? syntheticPilotIdentity(selectedSyntheticPilot.snapshot_id) : null;
  // X1 落日條款 (2026-09-29 audit): `config.toml [decision] enabled = false`
  // answers every `/api/decision/*` request with 404, so every control below
  // could only fail on contact. The route deliberately stays reachable — a
  // bookmark lands on this honest statement instead of a silent redirect that
  // reads as a bug — and names the key, because turning the line back on is an
  // operator action on the host, not something this page can do.
  if (!decisionEnabled) {
    return (
      <div className="flex h-full flex-col">
        <CollectionPageHeader icon={FlaskConical} title={t('decisionLab.title')} description={t('decisionLab.description')} />
        <div className="flex-1 overflow-y-auto p-5">
          <Card data-testid="decision-lab-disabled"><CardContent className="space-y-2 py-4">
            <p className="text-sm font-medium">{t('decisionLab.disabledTitle')}</p>
            <p className="text-sm text-muted-foreground">{t('decisionLab.disabledHint')}</p>
          </CardContent></Card>
        </div>
      </div>
    );
  }
  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader icon={FlaskConical} title={t('decisionLab.title')} description={t('decisionLab.description')} />
      <div className="flex-1 space-y-4 overflow-y-auto p-5">
        {/* X1 方案 5 (2026-09-29 audit): say up front that nothing here has
            seen real operator data. `overview.status` is the backend's own
            word for it — an operator-supplied pilot flips the wording. */}
        <DemoModeNotice
          pageKey="decision-lab"
          origin={decisionDataOrigin(catalog, overview?.status)}
        />
        <Card><CardContent className="flex flex-wrap items-end gap-3">
          <label className="space-y-1 text-sm">{t('decisionLab.tenant')}<Input aria-label={t('decisionLab.tenant')} value={tenant} disabled={busy} onChange={(event) => changeScope('tenant', event.target.value)} /></label>
          <label className="space-y-1 text-sm">{t('decisionLab.acl')}<Input aria-label={t('decisionLab.acl')} value={acl} disabled={busy} onChange={(event) => changeScope('acl', event.target.value)} /></label>
          <Button disabled={busy} onClick={load}><RefreshCw className="mr-2 size-4" />{t('decisionLab.load')}</Button>
          <span className="text-xs text-muted-foreground">{t('decisionLab.scopeHint')}</span>
        </CardContent></Card>

        {error && <p role="alert" className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{error}</p>}
        {notice && <p role="status" className="rounded-md bg-emerald-500/10 p-3 text-sm">{notice}</p>}

        <Card className="border-amber-500/40 bg-amber-500/5"><CardContent className="flex gap-3">
          <ShieldAlert className="mt-0.5 size-5 shrink-0 text-amber-700" aria-hidden="true" />
          <div className="space-y-1">
            <p className="font-medium">{t('decisionLab.exploratory')}</p>
            <p className="text-sm text-muted-foreground">{t('decisionLab.noPromotion')}</p>
            <p className="text-sm text-muted-foreground">{t('decisionLab.noAction')}</p>
          </div>
        </CardContent></Card>

        {overview && <>
          <p className="rounded-md border px-3 py-2 text-sm" role="status">{intl.formatMessage({ id: 'decisionLab.loadedScope' }, { tenant: overview.tenant_id, acl: overview.acl })}</p>
          <section aria-label={t('decisionLab.overview')} className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <MetricCard label={t('decisionLab.status')} value={overview.status} detail={intl.formatMessage({ id: 'decisionLab.generatedAt' }, { time: dateTime(overview.generated_at_utc, intl.locale) })} />
            <MetricCard label={t('decisionLab.invalidatedInputs')} value={String(overview.invalidated_inputs)} />
            <MetricCard label={t('decisionLab.ticketActive')} value={String(overview.ticket_sources.active)} detail={intl.formatMessage({ id: 'decisionLab.nextExpiry' }, { time: overview.ticket_sources.next_expiry_utc ? dateTime(overview.ticket_sources.next_expiry_utc, intl.locale) : t('decisionLab.none') })} />
            <MetricCard label={t('decisionLab.ticketOther')} value={`${overview.ticket_sources.expired} / ${overview.ticket_sources.revoked}`} detail={t('decisionLab.expiredRevoked')} />
          </section>

          {counts.length > 0 && <Card><CardContent>
            <h2 className="mb-3 font-semibold">{t('decisionLab.counts')}</h2>
            <div className="flex flex-wrap gap-2">{counts.map(([kind, count]) => <Badge key={kind} variant="secondary">{kind}: {count}</Badge>)}</div>
          </CardContent></Card>}

          <DecisionPilotUploadPanel key={JSON.stringify([overview.tenant_id, overview.acl])}
            scope={{ tenant_id: overview.tenant_id, acl: overview.acl }} t={t} onImported={finishUploadedPilot}
            blocked={busy} onBusyChange={setBusy} />

          <Card><CardContent className="space-y-4">
            <div><h2 className="font-semibold">{t('decisionLab.syntheticPilot')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.syntheticHint')}</p></div>
            <div className="flex flex-wrap items-end gap-3">
              <label className="space-y-1 text-sm">{t('decisionLab.seed')}<Input aria-label={t('decisionLab.seed')} type="number" min={0} step={1} disabled={busy} value={seed} onChange={(event) => setSeed(Number(event.target.value))} /></label>
      <label className="space-y-1 text-sm">{t('decisionLab.days')}<Input aria-label={t('decisionLab.days')} type="number" min={21} max={90} step={1} disabled={busy} value={days} onChange={(event) => setDays(Number(event.target.value))} /></label>
      <Button disabled={busy || !Number.isSafeInteger(seed) || seed < 0 || !Number.isInteger(days) || days < 21 || days > 90} onClick={createPilot}><Play className="mr-2 size-4" />{t('decisionLab.createPilot')}</Button>
            </div>
            {pilotSourceId && <p className="break-all text-xs text-muted-foreground">{t('decisionLab.pilotSource')}: {pilotSourceId}</p>}
          </CardContent></Card>

          <Card><CardContent className="space-y-3">
            <div><h2 className="font-semibold">{t('decisionLab.lifecycleTitle')}</h2>
              <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.lifecycleHint')}</p></div>
            <p className="break-all text-xs text-muted-foreground">{lifecycleTarget ? selectedSyntheticPilot?.snapshot_id : t('decisionLab.lifecycleSelectPilot')}</p>
            <div className="flex flex-wrap items-end gap-3">
              <label className="space-y-1 text-sm">{t('decisionLab.lifecycleKind')}
                <select aria-label={t('decisionLab.lifecycleKind')} className="block rounded-md border bg-background px-3 py-2"
                  disabled={busy || !lifecycleTarget} value={lifecycleKind}
                  onChange={(event) => setLifecycleKind(event.target.value as SyntheticLifecycleKind)}>
                  {(['version_changed', 'acl_lost', 'quarantined', 'deleted'] as const).map((kind) =>
                    <option key={kind} value={kind}>{t(`decisionLab.lifecycleKind.${kind}`)}</option>)}
                </select>
              </label>
              <Button disabled={busy || !lifecycleTarget} onClick={stageSyntheticLifecycle}>{t('decisionLab.lifecycleStage')}</Button>
              <Button variant="outline" disabled={busy}
                onClick={() => navigate(`/app/system/ccr?tenant_id=${encodeURIComponent(overview.tenant_id)}`)}>
                {t('decisionLab.lifecycleOpenDashboard')}
              </Button>
            </div>
          </CardContent></Card>

          <Card><CardContent className="space-y-3">
            <div><h2 className="font-semibold">{t('decisionLab.shadowTitle')}</h2>
              <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.shadowHint')}</p></div>
            <div className="flex flex-wrap items-end gap-3">
              <label className="min-w-64 flex-1 space-y-1 text-sm">{t('decisionLab.shadowPolicyId')}
                <Input aria-label={t('decisionLab.shadowPolicyId')} value={shadowPolicyId} disabled={busy}
                  onChange={(event) => { setShadowPolicyId(event.target.value); setShadowMonitor(null); }} />
              </label>
              <Button disabled={busy || !shadowPolicyId.trim()} onClick={loadShadowMonitor}>{t('decisionLab.shadowLoad')}</Button>
            </div>
            {shadowMonitor && <div className="space-y-3" aria-label={t('decisionLab.shadowTitle')}>
              <p className="text-xs text-muted-foreground">{t('decisionLab.shadowAssessedAt')}: {dateTime(shadowMonitor.assessed_at_utc, intl.locale)} · {shadowMonitor.policy.queue_id ?? t('decisionLab.sourceUnmarked')}</p>
              <p className="text-xs text-muted-foreground">{t('decisionLab.shadowWindow')}: {dateTime(shadowMonitor.policy.effective_from_utc, intl.locale)} – {dateTime(shadowMonitor.policy.effective_until_utc, intl.locale)}</p>
              <div className="grid gap-3 lg:grid-cols-2">
                <ShadowAssessmentCard title={t('decisionLab.shadowAggregate')} assessment={shadowMonitor.aggregate} t={t} />
                <ShadowAssessmentCard title={t('decisionLab.shadowSla')} assessment={shadowMonitor.sla} t={t} />
              </div>
              <p className="text-xs text-muted-foreground">{t('decisionLab.shadowLimit')}</p>
            </div>}
          </CardContent></Card>

          <DecisionShadowWorkflowPanel key={`shadow:${outcomeScopeKey}`}
            scope={{ tenant_id: overview.tenant_id, acl: overview.acl }} blocked={busy}
            onBusyChange={setBusy} locale={intl.locale} t={t} />

          <DecisionSlaShadowWorkflowPanel key={`sla-shadow:${outcomeScopeKey}`}
            scope={{ tenant_id: overview.tenant_id, acl: overview.acl }} blocked={busy}
            onBusyChange={setBusy} locale={intl.locale} t={t} />

          <DecisionOutcomeReviewPanel key={outcomeScopeKey}
            scope={{ tenant_id: overview.tenant_id, acl: overview.acl }} blocked={busy}
            onBusyChange={setBusy} locale={intl.locale} t={t}
            onUseScreen={(hash) => setCandidateScreenPrefill((current) => ({
              scopeKey: outcomeScopeKey, hash, serial: current.serial + 1,
            }))} />

          <DecisionCandidateModelPanel key={`${outcomeScopeKey}:${candidateScreenPrefill.scopeKey === outcomeScopeKey ? candidateScreenPrefill.serial : 0}`}
            scope={{ tenant_id: overview.tenant_id, acl: overview.acl }} blocked={busy}
            onBusyChange={setBusy} locale={intl.locale} t={t}
            screenPrefill={candidateScreenPrefill.scopeKey === outcomeScopeKey ? candidateScreenPrefill.hash : ''} />

          <Card><CardContent className="space-y-4">
            <div><h2 className="font-semibold">{t('decisionLab.compareTitle')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.compareHint')}</p></div>
            {catalog && <div className="grid gap-3 sm:grid-cols-2">
              <CatalogSelect label={t('decisionLab.snapshot')} value={snapshotId} disabled={busy} onChange={(value) => changeSelection(setSnapshotId, value)}
                items={catalog.snapshots.map((item) => ({ id: item.id, label: `${item.source_kinds.includes('synthetic_support_export') ? `[${t('decisionLab.syntheticBadge')}] ` : item.source_kinds.includes('local_support_pilot_export') ? `[${t('decisionLab.uploadBadge')}] ` : ''}${item.id} · ${item.queue_id ?? t('decisionLab.sourceUnmarked')} · ${item.horizon_days}d · ${item.data_cutoff_utc}` }))} />
              <CatalogSelect label={t('decisionLab.model')} value={modelVersion} disabled={busy} onChange={(value) => changeSelection(setModelVersion, value)}
                items={catalog.models.map((item) => ({ id: item.id, label: item.id }))} />
              <CatalogSelect label={t('decisionLab.baseline')} value={baselineScenarioId} disabled={busy} onChange={(value) => changeSelection(setBaselineScenarioId, value)}
                items={catalog.scenarios.map((item) => ({ id: item.id, label: `${item.id} · ${item.horizon_days}d` }))} />
              <CatalogSelect label={t('decisionLab.alternative')} value={alternativeScenarioId} disabled={busy} onChange={(value) => changeSelection(setAlternativeScenarioId, value)}
                items={catalog.scenarios.map((item) => ({ id: item.id, label: `${item.id} · ${item.horizon_days}d` }))} />
            </div>}
            <Button disabled={busy || !snapshotId || !modelVersion || !baselineScenarioId || !alternativeScenarioId || baselineScenarioId === alternativeScenarioId} onClick={compareScenarios}>{t('decisionLab.compare')}</Button>
          </CardContent></Card>

          {selectedReviewPilot && <EngineeringValidationPanel busy={busy} report={engineeringValidation}
            origin={selectedUploadedPilot ? 'uploaded' : 'synthetic'}
            locale={intl.locale} t={t} onRun={runEngineeringValidation} />}

          {brief && <>
            <Card><CardContent className="space-y-3">
              <div className="flex flex-wrap items-center gap-2"><h2 className="font-semibold">{t('decisionLab.comparison')}</h2><Badge variant="secondary">{brief.status}</Badge></div>
              <KpiTable brief={brief} locale={intl.locale} t={t} />
              {(() => {
                const sourceKinds = catalog?.snapshots.find((item) => item.id === snapshotId)?.source_kinds ?? [];
                const label = sourceKinds.includes('synthetic_support_export') ? t('decisionLab.syntheticBadge')
                  : sourceKinds.includes('local_support_pilot_export') ? t('decisionLab.uploadBadge')
                    : sourceKinds.length > 0 ? t('decisionLab.sourceLinked') : t('decisionLab.sourceUnmarked');
                return <Badge variant={sourceKinds.includes('synthetic_support_export') ? 'secondary' : 'outline'}>
                  {label}
                </Badge>;
              })()}
              <div className="flex flex-wrap items-center gap-2"><Button disabled={busy} onClick={verifyReplays}>{t('decisionLab.verifyReplays')}</Button>
                <span className="break-all font-mono text-xs text-muted-foreground">{t('decisionLab.baselineHash')}: {brief.replay.baseline_replay_hash}</span>
                <span className="break-all font-mono text-xs text-muted-foreground">{t('decisionLab.alternativeHash')}: {brief.replay.alternative_replay_hash}</span>
              </div>
              {brief.uncertainty_interval === null && <p className="text-sm text-muted-foreground">{t('decisionLab.noInterval')}</p>}
            </CardContent></Card>

            <Card><CardContent className="space-y-3"><h2 className="font-semibold">{t('decisionLab.provenance')}</h2>
              {brief.source_artifact_links.length === 0 ? <p className="text-sm text-muted-foreground">{t('decisionLab.noSources')}</p> : brief.source_artifact_links.map((source) => <div key={`${source.artifact_id}:${source.source_version_sha256}`} className="break-all rounded border p-2 text-xs">
                <p>{t('decisionLab.sourceArtifact')}: <span className="font-mono">{source.artifact_id}</span></p><p>{t('decisionLab.sourceVersion')}: <span className="font-mono">{source.source_version_sha256}</span></p>
              </div>)}
            </CardContent></Card>

            <TextList title={t('decisionLab.assumptions')} items={brief.assumptions} empty={t('decisionLab.noAssumptions')} />
            <TextList title={t('decisionLab.limitations')} items={brief.limitations} empty={t('decisionLab.noLimitations')} />
            {(baselineReplay || alternativeReplay) && <TrajectoryTable baseline={baselineReplay} alternative={alternativeReplay} locale={intl.locale} t={t} />}

            {selectedReviewPilot && <Card><CardContent className="space-y-3">
              <div><h2 className="font-semibold">{t(selectedUploadedPilot ? 'decisionLab.reviewTitleOperator' : 'decisionLab.reviewTitle')}</h2>
                <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.reviewHint')}</p></div>
              <label className="block space-y-1 text-sm">{t('decisionLab.reviewScenario')}
                <select aria-label={t('decisionLab.reviewScenario')} disabled={busy} value={reviewScenario}
                  onChange={(event) => { setReviewScenario(event.target.value as 'baseline' | 'alternative'); setApprovalId(''); setPilotReview(null); setReviewCheckedAt(''); }}
                  className="block w-full rounded-md border bg-background px-3 py-2 text-sm">
                  <option value="baseline">{t('decisionLab.baseline')}</option><option value="alternative">{t('decisionLab.alternative')}</option>
                </select>
              </label>
              <p className="break-all rounded border p-2 font-mono text-xs">{t('decisionLab.replayHash')}: {reviewScenario === 'baseline' ? brief.replay.baseline_replay_hash : brief.replay.alternative_replay_hash}</p>
              <label className="block space-y-1 text-sm">{t('decisionLab.reviewSummary')}
                <Input aria-label={t('decisionLab.reviewSummary')} maxLength={240} disabled={busy} value={reviewSummary}
                  onChange={(event) => setReviewSummary(event.target.value)} placeholder={t('decisionLab.reviewSummaryHint')} />
              </label>
              <div className="flex flex-wrap gap-2"><Button disabled={busy || !reviewSelection() || !reviewSummary.trim()} onClick={requestReview}>{t('decisionLab.requestReview')}</Button>
                <Button variant="outline" disabled={busy} onClick={() => navigate('/inbox')}>{t('decisionLab.openApprovalInbox')}</Button></div>
              <label className="block space-y-1 text-sm">{t('decisionLab.approvalId')}
                <Input aria-label={t('decisionLab.approvalId')} disabled={busy || !reviewSelection()} value={approvalId}
                  onChange={(event) => { setApprovalId(event.target.value); setPilotReview(null); setReviewCheckedAt(''); }} />
              </label>
              <Button variant="outline" disabled={busy || !reviewSelection() || !approvalId.trim()} onClick={checkReview}>{t('decisionLab.checkReview')}</Button>
              {pilotReview && <div className="space-y-1 rounded border p-3 text-sm" role="status">
                <p><Badge variant={pilotReview.status === 'approved' ? 'secondary' : 'outline'}>{t(`decisionLab.reviewStatus.${pilotReview.status}`)}</Badge></p>
                <p>{t('decisionLab.approvalId')}: <span className="break-all font-mono">{pilotReview.link.approval_id}</span></p>
                <p>{t('decisionLab.reviewExpires')}: {dateTime(pilotReview.expires_at_utc, intl.locale)}</p>
                {pilotReview.decided_by && <p>{t('decisionLab.reviewDecider')}: {pilotReview.decided_by}</p>}
                {reviewCheckedAt && <p className="text-xs text-muted-foreground">{t('decisionLab.reviewCheckedAt')}: {dateTime(reviewCheckedAt, intl.locale)}</p>}
                <p className="text-xs text-muted-foreground">{t('decisionLab.reviewReceiptOnly')}</p>
              </div>}
            </CardContent></Card>}

            <PolicySweepControls busy={busy} horizonDays={catalog?.snapshots.find((item) => item.id === snapshotId)?.horizon_days ?? 0}
              values={{ agentsPerDay, maxAddedAgents, maxAgentDays, maxStaffCost, maxFinalBacklog, capacityMin, capacityMax }}
              setters={{ agentsPerDay: setAgentsPerDay, maxAddedAgents: setMaxAddedAgents, maxAgentDays: setMaxAgentDays,
                maxStaffCost: setMaxStaffCost, maxFinalBacklog: setMaxFinalBacklog, capacityMin: setCapacityMin, capacityMax: setCapacityMax }}
              report={policySweep} locale={intl.locale} t={t} onRun={runPolicySweep} onInputsChanged={() => setPolicySweep(null)} />
            <SensitivityControls busy={busy}
              values={{ sensitivityRuns, arrivalDelta, sensitivityCapacityMin, sensitivityCapacityMax, sensitivityMaxBacklog, sensitivityMaxCost }}
              setters={{ sensitivityRuns: setSensitivityRuns, arrivalDelta: setArrivalDelta,
                sensitivityCapacityMin: setSensitivityCapacityMin, sensitivityCapacityMax: setSensitivityCapacityMax,
                sensitivityMaxBacklog: setSensitivityMaxBacklog, sensitivityMaxCost: setSensitivityMaxCost }}
              report={sensitivity} locale={intl.locale} t={t} onRun={runSensitivity} onInputsChanged={() => setSensitivity(null)} />
            {selectedReviewPilot && <DecisionPilotEvidenceSection key={`${overview.tenant_id}:${overview.acl}:${snapshotId}:${modelVersion}:${baselineScenarioId}:${alternativeScenarioId}:${brief.replay.baseline_replay_hash}:${brief.replay.alternative_replay_hash}`}
              busy={busy} scope={{ tenant_id: overview.tenant_id, acl: overview.acl }}
              selection={{ snapshot_id: snapshotId, model_version: modelVersion,
                baseline_scenario_id: baselineScenarioId, alternative_scenario_id: alternativeScenarioId }}
              origin={selectedUploadedPilot ? 'uploaded' : 'synthetic'}
              slaHoldout={uploadedHoldout}
              minHorizonDays={selectedUploadedPilot ? 10 : 21}
              horizonDays={catalog?.snapshots.find((item) => item.id === snapshotId)?.horizon_days ?? 0}
              locale={intl.locale} t={t} />}
          </>}

          <Card><CardContent className="space-y-3">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div><h2 className="font-semibold">{t('decisionLab.retention')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.retentionHint')}</p></div>
              <Button variant="destructive" disabled={busy || !confirmScrub} onClick={scrub}><Trash2 className="mr-2 size-4" />{t('decisionLab.scrub')}</Button>
            </div>
            <label className="flex items-start gap-2 text-sm"><input type="checkbox" disabled={busy} checked={confirmScrub} onChange={(event) => setConfirmScrub(event.target.checked)} />{t('decisionLab.confirmScrub')}</label>
          </CardContent></Card>

          <Card><CardContent className="space-y-3">
            <h2 className="font-semibold">{t('decisionLab.recentArtifacts')}</h2>
            {overview.artifacts.length === 0 ? <p className="text-sm text-muted-foreground">{t('decisionLab.noArtifacts')}</p> : (
              <div className="divide-y">{overview.artifacts.map((artifact) => <ArtifactRow key={`${artifact.kind}:${artifact.id}`} artifact={artifact} locale={intl.locale} t={t} />)}</div>
            )}
          </CardContent></Card>

          {overview.limitations.length > 0 && <Card><CardContent>
            <h2 className="mb-2 font-semibold">{t('decisionLab.limitations')}</h2>
            <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">{overview.limitations.map((limitation, index) => <li key={`${index}:${limitation}`}>{limitation}</li>)}</ul>
          </CardContent></Card>}

          <div className="flex flex-wrap items-center justify-between gap-3 rounded-md border p-4">
            <p className="text-sm">{t('decisionLab.causalPrompt')}</p>
            <Button variant="outline" onClick={() => navigate('/app/system/causal')}>{t('decisionLab.openCausal')}</Button>
          </div>
        </>}
      </div>
    </div>
  );
}

function MetricCard({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return <Card><CardContent className="space-y-1"><p className="text-xs text-muted-foreground">{label}</p><p className="break-words text-xl font-semibold">{value}</p>{detail && <p className="text-xs text-muted-foreground">{detail}</p>}</CardContent></Card>;
}

function ShadowAssessmentCard({ title, assessment, t }: {
  title: string; assessment: ShadowMonitorAssessment; t: DecisionTranslator;
}) {
  const count = (...statuses: string[]) => statuses.reduce((total, status) => total + (assessment.status_counts[status] ?? 0), 0);
  const missing = count('missing_forecast', 'missing_sla_forecast', 'unscored');
  const invalid = count('forecast_invalid', 'sla_forecast_invalid', 'score_invalid', 'score_stale',
    'cross_day_identity_mismatch', 'aggregate_unavailable');
  const revoked = count('forecast_revoked', 'sla_forecast_revoked', 'score_revoked');
  const verdict = (value: boolean | null) => value === null ? t('decisionLab.shadowUnavailable')
    : value ? t('decisionLab.yes') : t('decisionLab.no');
  return <section className="space-y-2 rounded-md border p-3" aria-label={title}>
    <div className="flex flex-wrap items-center gap-2"><h3 className="font-medium">{title}</h3>
      <Badge variant={assessment.complete ? 'secondary' : 'outline'}>{assessment.due_days === 0
        ? t('decisionLab.shadowPending') : assessment.complete ? t('decisionLab.shadowComplete') : t('decisionLab.shadowIncomplete')}</Badge></div>
    <p className="text-sm">{t('decisionLab.shadowProgress', { scored: assessment.scored_days, due: assessment.due_days, corrected: assessment.corrected_days })}</p>
    <p className="text-sm">{t('decisionLab.shadowIssues', { missing, invalid, revoked })}</p>
    <p className="text-sm">{t('decisionLab.shadowWholeSkill')}: {verdict(assessment.whole_window_skill)} · {t('decisionLab.shadowRecentSkill')}: {verdict(assessment.recent_skill)}</p>
    <p className="text-sm">{t('decisionLab.shadowInterval')}: {assessment.fixed_interval_available ? t('decisionLab.yes') : t('decisionLab.no')} · {t('decisionLab.shadowDrift')}: {verdict(assessment.drift_signal)}</p>
  </section>;
}

/**
 * Which banner this page should show (X1 2026-09-29).
 *
 * `task-board:` is the queue-id prefix the local task-board exporter mints,
 * and the importer verifies that id against EVERY ticket and staffing row
 * before a snapshot is stored — so a snapshot carrying it really was imported
 * from rows that all declared it. It is still the export's own claim about its
 * upstream, which is exactly why the `taskBoard` message says "not synthetic"
 * and names the staffing proxy rather than calling the numbers validated.
 *
 * Matched with an anchored `startsWith` on the full prefix including the
 * colon, so a queue called `task-boardish:…` never trips it.
 */
export function decisionDataOrigin(
  catalog: DecisionCatalog | null,
  status: string | undefined,
): 'synthetic' | 'operator' | 'taskBoard' {
  if (catalog?.snapshots.some((s) => s.queue_id?.startsWith('task-board:'))) return 'taskBoard';
  if (status?.startsWith('operator') || status?.startsWith('exploratory_operator')) return 'operator';
  return 'synthetic';
}

function EngineeringValidationPanel({ busy, report, origin, locale, t, onRun }: {
  busy: boolean; report: DecisionEngineeringValidation | null; origin: 'synthetic' | 'uploaded';
  locale: string; t: DecisionTranslator; onRun: () => void;
}) {
  const backtest = report?.one_day_backtest;
  const averages = backtest ? [
    [t('decisionLab.validationRollingFit'), backtest.model_abs_error_sum],
    [t('decisionLab.validationNoChange'), backtest.no_change_abs_error_sum],
    [t('decisionLab.validationSeasonal'), backtest.seasonal_naive_abs_error_sum],
    [t('decisionLab.validationMean'), backtest.mean_change_abs_error_sum],
  ] as const : [];
  // The sums are decimal strings (they can exceed the exact integer range of a
  // JSON number). The displayed mean is a rounded average either way.
  const meanError = (sum: string) => backtest
    ? formatNumber(Number(sum) / backtest.evaluation_days, locale) : '';
  return <Card><CardContent className="space-y-4">
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div><div className="flex flex-wrap items-center gap-2"><h2 className="font-semibold">{t('decisionLab.engineeringValidation')}</h2><Badge variant="secondary">{t(origin === 'uploaded' ? 'decisionLab.uploadBadge' : 'decisionLab.syntheticBadge')}</Badge></div>
        <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.engineeringValidationHint')}</p>
      </div>
      <Button disabled={busy} onClick={onRun}>{t('decisionLab.runEngineeringValidation')}</Button>
    </div>
    {report && <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2"><Badge variant="outline">{report.status}</Badge><span className="text-xs text-muted-foreground">{t('decisionLab.generatedAtUtcLabel')}: {dateTime(report.generated_at_utc, locale)}</span></div>
      <div className="break-all rounded border p-2 text-xs"><p>{t('decisionLab.snapshot')}: <span className="font-mono">{report.snapshot_id}</span></p>
        <p>{t('decisionLab.model')}: <span className="font-mono">{report.model_version}</span></p>
        <p>{t('decisionLab.sourceArtifact')}: <span className="font-mono">{report.source_artifact_id}</span></p>
        <p>{t('decisionLab.sourceVersion')}: <span className="font-mono">{report.source_version_sha256}</span></p>
        <p>{t('decisionLab.validationEngineHash')}: <span className="font-mono">{report.engineering_engine_sha256}</span></p>
        <p>{t('decisionLab.validationSimulationEngineHash')}: <span className="font-mono">{report.simulation_engine_sha256}</span></p>
        <p>{t('decisionLab.replayHash')}: <span className="font-mono">{report.replay_hash}</span></p></div>
      <div><h3 className="mb-2 font-medium">{t('decisionLab.validationCapacity')}</h3>
        <p className="text-sm">{t('decisionLab.validationProvidedCapacity')}: {formatNumber(report.service_capacity_per_agent_day, locale)}</p>
        {report.initial_prefix_fit ? <p className="text-sm">{t('decisionLab.validationPrefixFit')}: {formatNumber(report.initial_prefix_fit.service_per_agent_day, locale)} ({formatNumber(report.initial_prefix_fit.saturated_days, locale)} / {formatNumber(report.initial_prefix_fit.training_days, locale)} {t('decisionLab.validationSaturatedDays')}) · {t('decisionLab.validationModelMatchesFit')}: {report.provided_model_matches_initial_prefix_fit ? t('decisionLab.yes') : t('decisionLab.no')}</p>
          : <p className="text-sm text-muted-foreground">{t('decisionLab.validationPrefixFit')}: {t('decisionLab.validationUnavailable')} · {report.initial_prefix_unavailable_reason}</p>}
      </div>
      {backtest ? <section><div className="mb-2 flex flex-wrap items-center gap-2"><h3 className="font-medium">{t('decisionLab.validationBacktest')} · {formatNumber(backtest.evaluation_days, locale)} {t('decisionLab.validationDays')}</h3><Badge variant={backtest.model_beats_all_baselines ? 'secondary' : 'outline'}>{backtest.model_beats_all_baselines ? t('decisionLab.validationRollingFitBeatsBaselines') : t('decisionLab.validationRollingFitDoesNotBeatBaselines')}</Badge></div>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5"><MetricCard label={t('decisionLab.validationArrivalError')} value={formatNumber(Number(backtest.arrival_abs_error_sum) / backtest.evaluation_days, locale)} detail={t('decisionLab.validationMeanAbsoluteError')} />{averages.map(([label, sum]) => <MetricCard key={label} label={label} value={meanError(sum)} detail={t('decisionLab.validationMeanAbsoluteError')} />)}</div>
        <div className="mt-3 overflow-x-auto"><table className="w-full min-w-[42rem] text-sm"><thead><tr className="border-b text-left text-muted-foreground">
          {['decisionLab.validationDay', 'decisionLab.validationActual', 'decisionLab.validationRollingFit', 'decisionLab.validationNoChange', 'decisionLab.validationSeasonal', 'decisionLab.validationMean'].map((id) => <th key={id} className="px-2 py-2 first:pl-0">{t(id)}</th>)}
        </tr></thead><tbody>{backtest.points.map((point) => <tr key={point.day_index} className="border-b last:border-0">
          <td className="px-2 py-2">{formatNumber(point.day_index + 1, locale)}</td><td className="px-2 py-2">{formatNumber(point.actual_backlog_end, locale)}</td>
          <td className="px-2 py-2">{formatNumber(point.predicted_backlog_end, locale)}</td><td className="px-2 py-2">{formatNumber(point.no_change_backlog_end, locale)}</td>
          <td className="px-2 py-2">{formatNumber(point.seasonal_naive_backlog_end, locale)}</td><td className="px-2 py-2">{formatNumber(point.mean_change_backlog_end, locale)}</td>
        </tr>)}</tbody></table></div>
      </section> : <section><h3 className="font-medium">{t('decisionLab.validationBacktest')}</h3>
        <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.validationUnavailable')}: {report.one_day_backtest_unavailable_reason}</p></section>}
      <section className="rounded border p-3"><h3 className="font-medium">{t('decisionLab.validationIntervals')}</h3>
        {report.fixed_interval_diagnostic ? <p className="mt-1 text-sm">{t('decisionLab.validationTargetCoverage')}: {formatNumber(report.fixed_interval_diagnostic.target_coverage_basis_points / 100, locale)}% · {t('decisionLab.validationObservedCoverage')}: {formatNumber(report.fixed_interval_diagnostic.observed_coverage_basis_points / 100, locale)}% · {t('decisionLab.validationRecentMisses')}: {formatNumber(report.fixed_interval_diagnostic.recent_miss_count, locale)} / {formatNumber(report.fixed_interval_diagnostic.recent_window_points, locale)} · {t('decisionLab.validationDrift')}: {report.fixed_interval_diagnostic.drift_signal ? t('decisionLab.yes') : t('decisionLab.no')}</p>
          : <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.validationIntervalUnavailable')}: {report.fixed_interval_unavailable_reason}</p>}
      </section>
      <section className="rounded border p-3"><h3 className="font-medium">{t('decisionLab.validationTicketSla')}</h3>
        {report.ticket_sla_holdout ? <p className="mt-1 text-sm">{t('decisionLab.validationTicketSlaTotals')}: {formatNumber(report.ticket_sla_holdout.predicted_total_within_sla, locale)} / {formatNumber(report.ticket_sla_holdout.observed_total_within_sla, locale)} · {t('decisionLab.validationTicketSlaError')}: {new Intl.NumberFormat(locale).format(BigInt(report.ticket_sla_holdout.daily_abs_error_sum))} · {t('decisionLab.validationModelMatchesFit')}: {report.ticket_sla_holdout.provided_model_matches_fit ? t('decisionLab.yes') : t('decisionLab.no')}</p>
          : <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.validationTicketSlaUnavailable')}: {report.ticket_sla_unavailable_reason}</p>}
      </section>
      <TextList title={t('decisionLab.limitations')} items={report.limitations} empty={t('decisionLab.noLimitations')} />
    </div>}
  </CardContent></Card>;
}

function CatalogSelect({ label, value, disabled, onChange, items }: {
  label: string; value: string; disabled: boolean; onChange: (value: string) => void;
  items: Array<{ id: string; label: string }>;
}) {
  return <label className="space-y-1 text-sm">{label}<select aria-label={label} disabled={disabled} value={value}
    onChange={(event) => onChange(event.target.value)} className="block w-full rounded-md border bg-background px-3 py-2 text-sm">
    <option value="">—</option>
    {items.map((item) => <option key={item.id} value={item.id}>{item.label}</option>)}
  </select></label>;
}

const KPI_ROWS = [
  ['final_backlog', 'decisionLab.kpi.backlog'],
  ['total_resolved', 'decisionLab.kpi.resolved'],
  ['resolved_within_sla', 'decisionLab.kpi.sla'],
  ['total_staff_cost_cents', 'decisionLab.kpi.cost'],
] as const;

function formatNumber(value: number, locale: string, signed = false): string {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 0, signDisplay: signed ? 'always' : 'auto' }).format(value);
}

/** Exact formatting for a decimal-string integer. These sums and totals can
 *  pass Number.MAX_SAFE_INTEGER, where `Number` silently rounds; `BigInt`
 *  cannot. A value that is not a plain decimal is shown verbatim rather than
 *  thrown away — this is a display path, not a validation gate. */
function formatDecimal(value: string, locale: string, signed = false): string {
  return /^-?\d+$/.test(value)
    ? new Intl.NumberFormat(locale, { signDisplay: signed ? 'always' : 'auto' }).format(BigInt(value))
    : value;
}

function KpiTable({ brief, locale, t }: { brief: DecisionBrief; locale: string; t: DecisionTranslator }) {
  return <div className="overflow-x-auto"><table className="w-full min-w-[34rem] text-sm">
    <thead><tr className="border-b text-left text-muted-foreground"><th className="py-2 pr-3">{t('decisionLab.metric')}</th><th className="py-2 pr-3">{t('decisionLab.baseline')}</th><th className="py-2 pr-3">{t('decisionLab.alternative')}</th><th className="py-2">{t('decisionLab.delta')}</th></tr></thead>
    <tbody>{KPI_ROWS.map(([key, label]) => <tr key={key} className="border-b last:border-0"><th scope="row" className="py-2 pr-3 text-left font-medium">{t(label)}</th>
      <td className="py-2 pr-3 tabular-nums">{formatDecimal(brief.baseline[key], locale)}</td>
      <td className="py-2 pr-3 tabular-nums">{formatDecimal(brief.alternative[key], locale)}</td>
      <td className="py-2 tabular-nums">{formatDecimal(brief.delta[key], locale, true)}</td></tr>)}</tbody>
  </table></div>;
}

function TextList({ title, items, empty }: { title: string; items: string[]; empty: string }) {
  return <Card><CardContent><h2 className="mb-2 font-semibold">{title}</h2>{items.length === 0
    ? <p className="text-sm text-muted-foreground">{empty}</p>
    : <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">{items.map((item, index) => <li key={`${index}:${item}`}>{item}</li>)}</ul>}</CardContent></Card>;
}

function TrajectoryTable({ baseline, alternative, locale, t }: {
  baseline: SimulationResult | null; alternative: SimulationResult | null; locale: string; t: DecisionTranslator;
}) {
  const length = Math.max(baseline?.days.length ?? 0, alternative?.days.length ?? 0);
  const dayIds = Array.from({ length }, (_, index) => index);
  return <Card><CardContent className="space-y-3">
    <div><h2 className="font-semibold">{t('decisionLab.trajectories')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.trajectoryHint')}</p></div>
    <div className="overflow-x-auto"><table className="w-full min-w-[66rem] text-sm">
      <thead><tr className="border-b text-left text-muted-foreground">
        {[t('decisionLab.day'), t('decisionLab.baselineArrivals'), t('decisionLab.baselineResolved'), t('decisionLab.baselineBacklog'), t('decisionLab.baselineSla'), t('decisionLab.baselineOverdue'), t('decisionLab.baselineCost'), t('decisionLab.alternativeArrivals'), t('decisionLab.alternativeResolved'), t('decisionLab.alternativeBacklog'), t('decisionLab.alternativeSla'), t('decisionLab.alternativeOverdue'), t('decisionLab.alternativeCost')].map((heading) => <th key={heading} className="px-2 py-2 first:pl-0">{heading}</th>)}
      </tr></thead>
      <tbody>{dayIds.map((index) => {
        const base = baseline?.days[index]; const alt = alternative?.days[index];
        const number = (value?: number) => value === undefined ? '—' : new Intl.NumberFormat(locale).format(value);
        return <tr key={index} className="border-b last:border-0"><th scope="row" className="py-2 pr-2 text-left">{base?.day ?? alt?.day ?? index + 1}</th>
          <td className="px-2 py-2 tabular-nums">{number(base?.arrivals)}</td><td className="px-2 py-2 tabular-nums">{number(base?.resolved)}</td><td className="px-2 py-2 tabular-nums">{number(base?.backlog_end)}</td><td className="px-2 py-2 tabular-nums">{number(base?.resolved_within_sla)}</td><td className="px-2 py-2 tabular-nums">{number(base?.overdue_pending)}</td><td className="px-2 py-2 tabular-nums">{number(base?.staff_cost_cents)}</td>
          <td className="px-2 py-2 tabular-nums">{number(alt?.arrivals)}</td><td className="px-2 py-2 tabular-nums">{number(alt?.resolved)}</td><td className="px-2 py-2 tabular-nums">{number(alt?.backlog_end)}</td><td className="px-2 py-2 tabular-nums">{number(alt?.resolved_within_sla)}</td><td className="px-2 py-2 tabular-nums">{number(alt?.overdue_pending)}</td><td className="px-2 py-2 tabular-nums">{number(alt?.staff_cost_cents)}</td>
        </tr>;
      })}</tbody>
    </table></div>
  </CardContent></Card>;
}

type PolicyControls = { agentsPerDay: number; maxAddedAgents: number; maxAgentDays: number; maxStaffCost: number; maxFinalBacklog: number; capacityMin: number; capacityMax: number };
type PolicySetters = { [K in keyof PolicyControls]: (value: number) => void };
type SensitivityControlsValue = { sensitivityRuns: number; arrivalDelta: number; sensitivityCapacityMin: number; sensitivityCapacityMax: number; sensitivityMaxBacklog: number; sensitivityMaxCost: number };
type SensitivitySetters = { [K in keyof SensitivityControlsValue]: (value: number) => void };

function PolicySweepControls({ busy, horizonDays, values, setters, report, locale, t, onRun, onInputsChanged }: {
  busy: boolean; horizonDays: number; values: PolicyControls; setters: PolicySetters;
  report: PolicySweepReport | null; locale: string; t: DecisionTranslator; onRun: () => void; onInputsChanged: () => void;
}) {
  const field = (id: string, key: keyof PolicyControls, min = 0, max?: number) => <AssumptionNumber
    key={id} label={t(id)} value={values[key]} min={min} max={max} disabled={busy}
    onChange={(value) => { setters[key](value); onInputsChanged(); }} />;
  return <Card><CardContent className="space-y-4">
    <div><h2 className="font-semibold">{t('decisionLab.policySweep')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.operatorAssumptionNotice')}</p>
      <p className="mt-1 text-xs text-muted-foreground">{t('decisionLab.availableAgentsHint', { days: horizonDays })}</p></div>
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {field('decisionLab.availableAgentsPerDay', 'agentsPerDay', 0, 4_294_967_295)}
      {field('decisionLab.maxAddedAgents', 'maxAddedAgents', 0, 4_294_967_295)}
      {field('decisionLab.maxAgentDays', 'maxAgentDays')}
      {field('decisionLab.maxStaffCost', 'maxStaffCost')}
      {field('decisionLab.maxFinalBacklog', 'maxFinalBacklog')}
      {field('decisionLab.capacityMin', 'capacityMin', 1, 512)}
      {field('decisionLab.capacityMax', 'capacityMax', 1, 512)}
    </div>
    <Button disabled={busy || horizonDays < 1} onClick={onRun}>{t('decisionLab.runSweep')}</Button>
    {report && <div className="space-y-3 rounded-md border p-3">
      <div className="flex flex-wrap items-center gap-2"><Badge variant="secondary">{report.status}</Badge><span className="text-xs text-muted-foreground">{t('decisionLab.replayHash')}: <span className="break-all font-mono">{report.replay_hash}</span></span></div>
      <div><h3 className="font-medium">{t('decisionLab.policyFlip')}</h3>{report.flips.length === 0
        ? <p className="text-sm text-muted-foreground">{t('decisionLab.noPolicyFlip')}</p>
        : <ul className="list-disc pl-5 text-sm">{report.flips.map((flip, index) => <li key={`${flip.at_capacity_per_agent_day}:${index}`}>{t('decisionLab.flipAt', { capacity: flip.at_capacity_per_agent_day })}: {choiceLabel(flip.from, t)} → {choiceLabel(flip.to, t)}</li>)}</ul>}</div>
      <div className="overflow-x-auto"><table className="w-full min-w-[48rem] text-sm">
        <thead><tr className="border-b text-left text-muted-foreground">{['decisionLab.capacity', 'decisionLab.baselineBacklog', 'decisionLab.alternativeBacklog', 'decisionLab.baselineCost', 'decisionLab.alternativeCost', 'decisionLab.baselineFeasible', 'decisionLab.alternativeFeasible', 'decisionLab.choice'].map((id) => <th key={id} className="px-2 py-2 first:pl-0">{t(id)}</th>)}</tr></thead>
        <tbody>{report.points.map((point) => <tr key={point.service_capacity_per_agent_day} className="border-b last:border-0">
          <td className="px-2 py-2 tabular-nums">{formatNumber(point.service_capacity_per_agent_day, locale)}</td>
          <td className="px-2 py-2 tabular-nums">{formatNumber(point.baseline_final_backlog, locale)}</td>
          <td className="px-2 py-2 tabular-nums">{formatNumber(point.alternative_final_backlog, locale)}</td>
          <td className="px-2 py-2 tabular-nums">{formatNumber(point.baseline_staff_cost_cents, locale)}</td>
          <td className="px-2 py-2 tabular-nums">{formatNumber(point.alternative_staff_cost_cents, locale)}</td>
          <td className="px-2 py-2">{point.baseline_feasible ? t('decisionLab.feasible') : t('decisionLab.infeasible')}</td>
          <td className="px-2 py-2">{point.alternative_feasible ? t('decisionLab.feasible') : t('decisionLab.infeasible')}</td>
          <td className="px-2 py-2">{choiceLabel(point.choice, t)}</td>
        </tr>)}</tbody>
      </table></div>
      <p className="text-sm"><strong>{t('decisionLab.decisionRule')}:</strong> {report.decision_rule}</p>
      <TextList title={t('decisionLab.limitations')} items={report.limitations} empty={t('decisionLab.noLimitations')} />
    </div>}
  </CardContent></Card>;
}

function SensitivityControls({ busy, values, setters, report, locale, t, onRun, onInputsChanged }: {
  busy: boolean; values: SensitivityControlsValue; setters: SensitivitySetters;
  report: SensitivityReport | null; locale: string; t: DecisionTranslator; onRun: () => void; onInputsChanged: () => void;
}) {
  const field = (id: string, key: keyof SensitivityControlsValue, min = 0, max?: number) => <AssumptionNumber
    key={id} label={t(id)} value={values[key]} min={min} max={max} disabled={busy}
    onChange={(value) => { setters[key](value); onInputsChanged(); }} />;
  return <Card><CardContent className="space-y-4">
    <div><h2 className="font-semibold">{t('decisionLab.sensitivity')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.sensitivityHint')}</p><p className="mt-1 text-xs text-muted-foreground">{t('decisionLab.operatorAssumptionNotice')}</p></div>
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {field('decisionLab.runs', 'sensitivityRuns', 1, 1000)}
      {field('decisionLab.arrivalDelta', 'arrivalDelta')}
      {field('decisionLab.capacityMin', 'sensitivityCapacityMin', 1, 512)}
      {field('decisionLab.capacityMax', 'sensitivityCapacityMax', 1, 512)}
      {field('decisionLab.maxFinalBacklog', 'sensitivityMaxBacklog')}
      {field('decisionLab.maxStaffCost', 'sensitivityMaxCost')}
    </div>
    <Button disabled={busy} onClick={onRun}>{t('decisionLab.runSensitivity')}</Button>
    {report && <div className="space-y-3 rounded-md border p-3">
      <div className="flex flex-wrap items-center gap-2"><Badge variant="secondary">{report.status}</Badge><span className="text-xs text-muted-foreground">{t('decisionLab.runs')}: {formatNumber(report.runs, locale)} · {t('decisionLab.replayHash')}: <span className="break-all font-mono">{report.replay_hash}</span></span></div>
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
        <MetricCard label={t('decisionLab.deltaP05')} value={formatNumber(report.delta_backlog_p05, locale, true)} />
        <MetricCard label={t('decisionLab.deltaP50')} value={formatNumber(report.delta_backlog_p50, locale, true)} />
        <MetricCard label={t('decisionLab.deltaP95')} value={formatNumber(report.delta_backlog_p95, locale, true)} />
        <MetricCard label={t('decisionLab.altLowerBacklog')} value={formatBps(report.alternative_lower_backlog_bps, locale)} />
        <MetricCard label={t('decisionLab.worstAlternativeBacklog')} value={formatNumber(report.worst_alternative_backlog, locale)} />
      </div>
      <div className="overflow-x-auto"><table className="w-full min-w-[36rem] text-sm">
        <thead><tr className="border-b text-left text-muted-foreground"><th className="py-2 pr-3">{t('decisionLab.violationRate')}</th><th className="py-2 pr-3">{t('decisionLab.baseline')}</th><th className="py-2">{t('decisionLab.alternative')}</th></tr></thead>
        <tbody>{[
          [t('decisionLab.backlogViolation'), report.baseline_backlog_violation_bps, report.alternative_backlog_violation_bps],
          [t('decisionLab.costViolation'), report.baseline_cost_violation_bps, report.alternative_cost_violation_bps],
        ].map(([label, base, alt]) => <tr key={String(label)} className="border-b last:border-0"><th scope="row" className="py-2 pr-3 text-left font-medium">{label}</th>
          <td className="py-2 pr-3 tabular-nums">{formatBps(Number(base), locale)}</td><td className="py-2 tabular-nums">{formatBps(Number(alt), locale)}</td></tr>)}</tbody>
      </table></div>
      <TextList title={t('decisionLab.limitations')} items={report.limitations} empty={t('decisionLab.noLimitations')} />
    </div>}
  </CardContent></Card>;
}

type EmpiricalValues = {
  trainingDays: number; minSaturatedDays: number; runs: number; arrivalBlockDays: number;
  maxFinalBacklog: number; maxStaffCost: number; minSlaResolved: number;
  fallbackMin: number; fallbackMax: number;
  availableAgents: number; maxAddedAgents: number; maxAgentDays: number;
  screenCapacityMin: number; screenCapacityMax: number;
  maxJointViolationBps: number; minJointRecoveryBps: number; minSlaImprovementBps: number;
};

type StoredSlaHoldoutSelection = { id: string; digest: string; sourceSha256: string };

function DecisionPilotEvidenceSection({ busy, scope, selection, origin, slaHoldout, horizonDays, minHorizonDays, locale, t }: {
  busy: boolean;
  scope: Pick<DecisionEmpiricalInput, 'tenant_id' | 'acl'>;
  selection: Pick<DecisionEmpiricalInput, 'snapshot_id' | 'model_version' | 'baseline_scenario_id' | 'alternative_scenario_id'>;
  origin: 'synthetic' | 'uploaded';
  slaHoldout: StoredSlaHoldoutSelection | null;
  horizonDays: number; minHorizonDays: number; locale: string; t: DecisionTranslator;
}) {
  const [eventComparison, setEventComparison] = useState<DecisionEventComparison | null>(null);
  const [forecastValidation, setForecastValidation] = useState<DecisionForecastValidation | null>(null);
  const [eventBusy, setEventBusy] = useState(false);
  const [forecastBusy, setForecastBusy] = useState(false);
  const [empiricalBusy, setEmpiricalBusy] = useState(false);
  return <>
    <DecisionEventComparePanel blocked={busy || empiricalBusy || forecastBusy} scope={scope} selection={selection} origin={origin}
      locale={locale} t={t} onComparison={setEventComparison} onBusyChange={setEventBusy} />
    <DecisionForecastValidationPanel blocked={busy || empiricalBusy || eventBusy} scope={scope}
      selection={selection} origin={origin} locale={locale} t={t}
      onValidation={setForecastValidation} onBusyChange={setForecastBusy} />
    <EmpiricalResamplingControls busy={busy || eventBusy || forecastBusy} scope={scope} selection={selection} origin={origin}
      horizonDays={horizonDays} minHorizonDays={minHorizonDays} locale={locale} t={t}
      eventComparison={eventComparison} forecastValidation={forecastValidation}
      slaHoldout={slaHoldout} onBusyChange={setEmpiricalBusy} />
  </>;
}

function EmpiricalResamplingControls({ busy, scope, selection, origin, horizonDays, minHorizonDays, locale, t, eventComparison, forecastValidation, slaHoldout, onBusyChange }: {
  busy: boolean;
  scope: Pick<DecisionEmpiricalInput, 'tenant_id' | 'acl'>;
  selection: Pick<DecisionEmpiricalInput, 'snapshot_id' | 'model_version' | 'baseline_scenario_id' | 'alternative_scenario_id'>;
  origin: 'synthetic' | 'uploaded';
  horizonDays: number; minHorizonDays: number; locale: string; t: DecisionTranslator;
  eventComparison: DecisionEventComparison | null;
  forecastValidation: DecisionForecastValidation | null;
  slaHoldout: StoredSlaHoldoutSelection | null;
  onBusyChange: (busy: boolean) => void;
}) {
  const [values, setValues] = useState<EmpiricalValues>({
    trainingDays: Math.min(14, Math.max(7, horizonDays - 3)), minSaturatedDays: 7, runs: 100, arrivalBlockDays: 7,
    maxFinalBacklog: horizonDays * 3, maxStaffCost: horizonDays * 30_000,
    minSlaResolved: horizonDays * 15, fallbackMin: 6, fallbackMax: 10,
    availableAgents: 4, maxAddedAgents: 2, maxAgentDays: horizonDays * 4,
    screenCapacityMin: 8, screenCapacityMax: 8,
    maxJointViolationBps: 5_000, minJointRecoveryBps: 1_000, minSlaImprovementBps: 1_000,
  });
  const [samplingMode, setSamplingMode] = useState<'independent' | 'paired_saturated_days'>('independent');
  const [useFallback, setUseFallback] = useState(false);
  const [withScreen, setWithScreen] = useState(false);
  const [report, setReport] = useState<DecisionEmpiricalReport | null>(null);
  const [evidenceRunId, setEvidenceRunId] = useState('');
  const [evidenceScreenHash, setEvidenceScreenHash] = useState('');
  const [effectIdsText, setEffectIdsText] = useState('');
  const [evidenceBrief, setEvidenceBrief] = useState<DecisionBrief | null>(null);
  const [includeEventEvidence, setIncludeEventEvidence] = useState(false);
  const [includeSlaHoldout, setIncludeSlaHoldout] = useState(false);
  const [includeForecastValidation, setIncludeForecastValidation] = useState(false);
  const [localBusy, setLocalBusy] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    setIncludeEventEvidence(false);
    setEvidenceBrief(null);
  }, [eventComparison]);
  useEffect(() => {
    setIncludeForecastValidation(false);
    setEvidenceBrief(null);
  }, [forecastValidation]);
  const disabled = busy || localBusy;
  const clearDerived = () => {
    setReport(null); setEvidenceRunId(''); setEvidenceScreenHash(''); setEvidenceBrief(null); setError('');
  };
  const setField = (key: keyof EmpiricalValues, value: number) => {
    setValues((current) => ({ ...current, [key]: value })); clearDerived();
  };
  const field = (id: string, key: keyof EmpiricalValues, min = 0, max?: number) => <AssumptionNumber
    key={id} label={t(id)} value={values[key]} min={min} max={max} disabled={disabled}
    onChange={(value) => setField(key, value)} />;
  async function run() {
    const numeric = Object.values(values);
    if (horizonDays < minHorizonDays || numeric.some((value) => !isNonNegativeInt(value))
      || values.trainingDays < 7 || values.trainingDays > horizonDays - 3
      || values.minSaturatedDays < 1 || values.minSaturatedDays > Math.min(values.trainingDays, 512)
      || values.runs < 1 || values.runs > 1_000
      || values.arrivalBlockDays < 1 || values.arrivalBlockDays > values.trainingDays
      || values.maxFinalBacklog > 1_000_000 || values.maxStaffCost > 1_000_000_000_000
      || values.minSlaResolved > 1_000_000
      || (useFallback && (values.fallbackMin < 1 || values.fallbackMin >= values.fallbackMax || values.fallbackMax > 512))
      || (useFallback && samplingMode !== 'independent')
      || (withScreen && (values.screenCapacityMin < 1 || values.screenCapacityMin > values.screenCapacityMax
        || values.screenCapacityMax > 512 || values.availableAgents > 512
        || values.maxAddedAgents > 512 || values.maxAgentDays > 512 * 366
        || values.maxJointViolationBps > 10_000
        || values.minJointRecoveryBps > 10_000 || values.minSlaImprovementBps > 10_000))) {
      setError(t('decisionLab.empiricalInvalid')); return;
    }
    const input: DecisionEmpiricalInput = {
      ...scope, ...selection, training_days: values.trainingDays, min_saturated_days: values.minSaturatedDays,
      runs: values.runs, arrival_block_days: values.arrivalBlockDays, sampling_mode: samplingMode,
      max_final_backlog: values.maxFinalBacklog, max_staff_cost_cents: values.maxStaffCost,
      min_sla_resolved: values.minSlaResolved,
      ...(useFallback ? { capacity_fallback_range: { min: values.fallbackMin, max: values.fallbackMax } } : {}),
      ...(withScreen ? { screen: {
        resource_plan: {
          available_agents_by_day: Array.from({ length: horizonDays }, () => values.availableAgents),
          max_added_agents_per_day: values.maxAddedAgents, max_total_agent_days: values.maxAgentDays,
          max_staff_cost_cents: values.maxStaffCost, max_final_backlog: values.maxFinalBacklog,
          service_capacity_band: { min: values.screenCapacityMin, max: values.screenCapacityMax },
        },
        criteria: { max_joint_violation_bps: values.maxJointViolationBps,
          min_joint_recovery_bps: values.minJointRecoveryBps,
          min_sla_improvement_bps: values.minSlaImprovementBps },
      } } : {}),
    };
    setLocalBusy(true); onBusyChange(true); setError(''); setReport(null); setEvidenceBrief(null);
    try {
      const next = await decisionApi.empiricalResampling(input);
      setReport(next);
      setEvidenceRunId(next.run.id);
      setEvidenceScreenHash(next.screen?.replay_hash ?? '');
    }
    catch (cause) { setError(errorMessage(cause)); }
    finally { setLocalBusy(false); onBusyChange(false); }
  }
  async function composeEvidenceBrief() {
    const runId = evidenceRunId.trim();
    const screenHash = evidenceScreenHash.trim();
    const effectIds = effectIdsText.split(',').map((item) => item.trim()).filter(Boolean);
    const eventEvidence = includeEventEvidence ? eventComparison : null;
    const holdoutEvidence = includeSlaHoldout ? slaHoldout : null;
    const forecastEvidence = includeForecastValidation ? forecastValidation : null;
    if ((!runId && effectIds.length === 0 && !holdoutEvidence && !forecastEvidence) || (screenHash && !runId)
      || (includeEventEvidence && (!runId || !eventEvidence)) || (includeSlaHoldout && !holdoutEvidence)
      || (includeForecastValidation && !forecastEvidence?.record_id)
      || effectIds.length > 8 || new Set(effectIds).size !== effectIds.length) {
      setError(t('decisionLab.evidenceRunRequired')); return;
    }
    setLocalBusy(true); onBusyChange(true); setError(''); setEvidenceBrief(null);
    try {
      const result = await decisionApi.compare({
        ...scope, ...selection,
        ...(runId ? { empirical_run_id: runId } : {}),
        ...(screenHash ? { policy_screen_hash: screenHash } : {}),
        ...(effectIds.length > 0 ? { effect_ids: effectIds } : {}),
        ...(eventEvidence ? { baseline_event_replay_hash: eventEvidence.baseline.replay_hash,
          alternative_event_replay_hash: eventEvidence.alternative.replay_hash } : {}),
        ...(holdoutEvidence ? { sla_holdout_id: holdoutEvidence.id } : {}),
        ...(forecastEvidence?.record_id ? { forecast_validation_id: forecastEvidence.record_id } : {}),
      });
      if ((runId && result.brief.exploratory_empirical?.run_id !== runId)
        || (screenHash && result.brief.exploratory_policy_screen?.screen_hash !== screenHash)
        || (eventEvidence && (result.brief.exploratory_event?.baseline_run_hash !== eventEvidence.baseline.replay_hash
          || result.brief.exploratory_event?.alternative_run_hash !== eventEvidence.alternative.replay_hash
          || result.brief.exploratory_event?.source_sha256 !== eventEvidence.source_sha256
          || result.brief.exploratory_event?.event_engine_sha256 !== eventEvidence.event_engine_sha256))
        || (holdoutEvidence && (result.brief.exploratory_sla_holdout?.record_id !== holdoutEvidence.id
          || result.brief.exploratory_sla_holdout?.record_sha256 !== holdoutEvidence.digest
          || result.brief.exploratory_sla_holdout?.source_sha256 !== holdoutEvidence.sourceSha256))
        || (forecastEvidence && (result.brief.exploratory_forecast?.record_id !== forecastEvidence.record_id
          || result.brief.exploratory_forecast?.record_sha256 !== forecastEvidence.record_sha256
          || result.brief.exploratory_forecast?.source_sha256 !== forecastEvidence.source_sha256))
        || (effectIds.length > 0 && (result.brief.observational_effects?.length !== effectIds.length
          || effectIds.some((id, index) => result.brief.observational_effects?.[index]?.estimate_id !== id)))) {
        throw new Error(t('decisionLab.evidenceMismatch'));
      }
      setEvidenceBrief(result.brief);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setLocalBusy(false); onBusyChange(false); }
  }
  return <Card><CardContent className="space-y-4">
    <div><h2 className="font-semibold">{t('decisionLab.empiricalTitle')}</h2>
      <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.empiricalHint')}</p></div>
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {field('decisionLab.empiricalTrainingDays', 'trainingDays', 7, Math.max(7, horizonDays - 3))}
      {field('decisionLab.empiricalMinSaturated', 'minSaturatedDays', 1, values.trainingDays)}
      {field('decisionLab.runs', 'runs', 1, 1_000)}
      {field('decisionLab.empiricalBlockDays', 'arrivalBlockDays', 1, values.trainingDays)}
      {field('decisionLab.maxFinalBacklog', 'maxFinalBacklog')}
      {field('decisionLab.maxStaffCost', 'maxStaffCost')}
      {field('decisionLab.empiricalMinSla', 'minSlaResolved')}
      <label className="space-y-1 text-sm">{t('decisionLab.empiricalSamplingMode')}
        <select aria-label={t('decisionLab.empiricalSamplingMode')} value={samplingMode} disabled={disabled}
          className="block w-full rounded-md border bg-background px-3 py-2"
          onChange={(event) => { setSamplingMode(event.target.value as typeof samplingMode); clearDerived(); }}>
          <option value="independent">{t('decisionLab.empiricalIndependent')}</option>
          <option value="paired_saturated_days">{t('decisionLab.empiricalPaired')}</option>
        </select>
      </label>
    </div>
    <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={useFallback} disabled={disabled}
      onChange={(event) => { setUseFallback(event.target.checked); clearDerived(); }} />{t('decisionLab.empiricalFallback')}</label>
    {useFallback && <div className="grid gap-3 sm:grid-cols-2">
      {field('decisionLab.capacityMin', 'fallbackMin', 1, 512)}{field('decisionLab.capacityMax', 'fallbackMax', 1, 512)}
      <p className="text-xs text-muted-foreground sm:col-span-2">{t('decisionLab.empiricalFallbackHint')}</p>
    </div>}
    <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={withScreen} disabled={disabled}
      onChange={(event) => { setWithScreen(event.target.checked); clearDerived(); }} />{t('decisionLab.empiricalScreen')}</label>
    {withScreen && <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {field('decisionLab.availableAgentsPerDay', 'availableAgents')}
      {field('decisionLab.maxAddedAgents', 'maxAddedAgents')}
      {field('decisionLab.maxAgentDays', 'maxAgentDays')}
      {field('decisionLab.capacityMin', 'screenCapacityMin', 1, 512)}
      {field('decisionLab.capacityMax', 'screenCapacityMax', 1, 512)}
      {field('decisionLab.empiricalMaxJointViolation', 'maxJointViolationBps', 0, 10_000)}
      {field('decisionLab.empiricalMinJointRecovery', 'minJointRecoveryBps', 0, 10_000)}
      {field('decisionLab.empiricalMinSlaImprovement', 'minSlaImprovementBps', 0, 10_000)}
      <p className="text-xs text-muted-foreground sm:col-span-2 lg:col-span-4">{t('decisionLab.empiricalScreenHint')}</p>
    </div>}
    <Button disabled={disabled || horizonDays < minHorizonDays} onClick={run}>{t('decisionLab.empiricalRun')}</Button>
    {horizonDays < minHorizonDays && <p className="text-xs text-muted-foreground">{t('decisionLab.empiricalMinimumDays', { days: minHorizonDays })}</p>}
    <div className="space-y-3 rounded-md border p-3">
      <p className="text-sm font-medium">{t('decisionLab.evidenceBriefTitle')}</p>
      <p className="text-xs text-muted-foreground">{t('decisionLab.evidenceBriefHint')}</p>
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="space-y-1 text-sm">{t('decisionLab.evidenceRunId')}
          <Input aria-label={t('decisionLab.evidenceRunId')} value={evidenceRunId} maxLength={192}
            disabled={disabled} onChange={(event) => { setEvidenceRunId(event.target.value); setEvidenceBrief(null); setError(''); }} />
        </label>
        <label className="space-y-1 text-sm">{t('decisionLab.evidenceScreenHash')}
          <Input aria-label={t('decisionLab.evidenceScreenHash')} value={evidenceScreenHash} maxLength={128}
            disabled={disabled} onChange={(event) => { setEvidenceScreenHash(event.target.value); setEvidenceBrief(null); setError(''); }} />
        </label>
      </div>
      <label className="block space-y-1 text-sm">{t('decisionLab.evidenceEffectIds')}
        <Input aria-label={t('decisionLab.evidenceEffectIds')} value={effectIdsText} maxLength={1024}
          disabled={disabled} onChange={(event) => { setEffectIdsText(event.target.value); setEvidenceBrief(null); setError(''); }} />
      </label>
      <p className="text-xs text-muted-foreground">{t('decisionLab.evidenceEffectHint')}</p>
      {eventComparison && <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={includeEventEvidence} disabled={disabled}
          onChange={(event) => { setIncludeEventEvidence(event.target.checked); setEvidenceBrief(null); setError(''); }} />
        {t('decisionLab.evidenceIncludeEvent')}
      </label>}
      {eventComparison && <p className="break-all text-xs text-muted-foreground">
        {t('decisionLab.baselineHash')}: {eventComparison.baseline.replay_hash} · {t('decisionLab.alternativeHash')}: {eventComparison.alternative.replay_hash}
      </p>}
      {slaHoldout && <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={includeSlaHoldout} disabled={disabled}
          onChange={(event) => { setIncludeSlaHoldout(event.target.checked); setEvidenceBrief(null); setError(''); }} />
        {t('decisionLab.evidenceIncludeSlaHoldout')}
      </label>}
      {slaHoldout && <p className="break-all text-xs text-muted-foreground">
        {t('decisionLab.uploadHoldout')}: {slaHoldout.id} · {t('decisionLab.uploadHoldoutSha')}: {slaHoldout.digest}
      </p>}
      {forecastValidation?.record_id && <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={includeForecastValidation} disabled={disabled}
          onChange={(event) => { setIncludeForecastValidation(event.target.checked); setEvidenceBrief(null); setError(''); }} />
        {t('decisionLab.evidenceIncludeForecast')}
      </label>}
      {forecastValidation?.record_id && <p className="break-all text-xs text-muted-foreground">
        {t('decisionLab.forecastRecordId')}: {forecastValidation.record_id} · {t('decisionLab.forecastRecordSha')}: {forecastValidation.record_sha256}
      </p>}
      <Button variant="outline" disabled={disabled || (!evidenceRunId.trim() && !effectIdsText.trim() && !includeSlaHoldout && !includeForecastValidation)} onClick={composeEvidenceBrief}>
        {t('decisionLab.composeEvidenceBrief')}
      </Button>
      {evidenceBrief && <EvidenceBriefReport brief={evidenceBrief} origin={origin} locale={locale} t={t} />}
    </div>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {report && <EmpiricalResamplingReport report={report} locale={locale} t={t} />}
  </CardContent></Card>;
}

function EmpiricalResamplingReport({ report, locale, t }: {
  report: DecisionEmpiricalReport; locale: string; t: DecisionTranslator;
}) {
  const run = report.run;
  return <div className="space-y-3 rounded-md border p-3" aria-label={t('decisionLab.empiricalTitle')}>
    <div className="flex flex-wrap items-center gap-2"><Badge variant="secondary">{t(report.status === 'exploratory_operator_upload'
      ? 'decisionLab.uploadBadge' : report.status === 'synthetic_only_exploratory'
        ? 'decisionLab.syntheticBadge' : 'decisionLab.empiricalBadge')}</Badge>
      <span className="text-xs text-muted-foreground">{t('decisionLab.empiricalRunId')}: <span className="font-mono">{run.id}</span></span></div>
    <p className="break-all text-xs text-muted-foreground">{t('decisionLab.replayHash')}: {run.replay_hash} · {t('decisionLab.empiricalCommonDraws')}: {run.common_draws_sha256}</p>
    <p className="text-sm">{t('decisionLab.empiricalFitSummary', { training: report.fit.training_days,
      arrivals: report.fit.arrival_sample_count, capacities: report.fit.capacity_sample_count,
      source: t(`decisionLab.empiricalCapacitySource.${run.capacity_source}`) })}</p>
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <MetricCard label={t('decisionLab.empiricalBacklogDelta')} value={`${formatNumber(run.delta_backlog_p05, locale, true)} / ${formatNumber(run.delta_backlog_p50, locale, true)} / ${formatNumber(run.delta_backlog_p95, locale, true)}`} detail={t('decisionLab.empiricalQuantiles')} />
      <MetricCard label={t('decisionLab.empiricalSlaDelta')} value={`${formatNumber(run.delta_sla_resolved_p05, locale, true)} / ${formatNumber(run.delta_sla_resolved_p50, locale, true)} / ${formatNumber(run.delta_sla_resolved_p95, locale, true)}`} detail={t('decisionLab.empiricalQuantiles')} />
      <MetricCard label={t('decisionLab.empiricalWorstBacklog')} value={formatNumber(run.worst_alternative_backlog, locale)} detail={t('decisionLab.empiricalWorstSla', { value: formatNumber(run.worst_alternative_sla_resolved, locale) })} />
    </div>
    <p className="text-sm">{t('decisionLab.empiricalJointRisk')}: {formatBps(run.baseline_joint_violation_bps, locale)} / {formatBps(run.alternative_joint_violation_bps, locale)} · {t('decisionLab.empiricalJointRecovery')}: {formatBps(run.alternative_joint_recovery_bps, locale)} · {t('decisionLab.empiricalSlaImprovement')}: {formatBps(run.alternative_sla_improvement_bps, locale)}</p>
    {report.screen && <div className="space-y-1 rounded border p-2 text-sm"><p>{t('decisionLab.empiricalScreenChoice')}: <Badge variant="outline">{t(`decisionLab.empiricalChoice.${report.screen.choice}`)}</Badge></p>
      <p>{t('decisionLab.empiricalResourceFeasibility')}: {report.screen.baseline_deterministic_feasible ? t('decisionLab.yes') : t('decisionLab.no')} / {report.screen.alternative_deterministic_feasible ? t('decisionLab.yes') : t('decisionLab.no')}</p>
      {report.screen.failed_alternative_checks.length > 0 && <p className="text-muted-foreground">{t('decisionLab.empiricalFailedChecks')}: {report.screen.failed_alternative_checks.map((check) => t(`decisionLab.empiricalFailedCheck.${check}`)).join(', ')}</p>}
    </div>}
    <TextList title={t('decisionLab.limitations')} items={report.limitations} empty={t('decisionLab.noLimitations')} />
  </div>;
}

/** Minutes from a decimal-string wait in seconds. The division is the one
 *  place a float is correct (minutes are shown to one decimal), and a wait in
 *  seconds cannot approach 2^53 in any real queue — but a value that is not a
 *  plain decimal is shown verbatim rather than rendered as NaN. */
function formatEventWait(value: string | null, locale: string, t: DecisionTranslator): string {
  if (value === null) return t('decisionLab.eventUnavailable');
  if (!/^-?\d+$/.test(value)) return value;
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(Number(value) / 60);
}

function EvidenceBriefReport({ brief, origin, locale, t }: {
  brief: DecisionBrief; origin: 'synthetic' | 'uploaded'; locale: string; t: DecisionTranslator;
}) {
  const empirical = brief.exploratory_empirical;
  const eventEvidence = brief.exploratory_event;
  const slaHoldout = brief.exploratory_sla_holdout;
  const forecast = brief.exploratory_forecast;
  const observation = empirical?.report;
  const screen = brief.exploratory_policy_screen;
  const effects = brief.observational_effects ?? [];
  return <div className="space-y-3 rounded-md border p-3" aria-label={t('decisionLab.evidenceBriefTitle')}>
    <div className="flex flex-wrap items-center gap-2">
      {empirical && <Badge variant="secondary">{t(empirical.source_origin === 'operator_upload'
        ? 'decisionLab.uploadBadge' : empirical.source_origin === 'synthetic_fixture'
          ? 'decisionLab.syntheticBadge' : 'decisionLab.empiricalBadge')}</Badge>}
      {!empirical && (forecast || slaHoldout || eventEvidence) && <Badge variant="secondary">{t(origin === 'uploaded'
        ? 'decisionLab.uploadBadge' : 'decisionLab.syntheticBadge')}</Badge>}
      {effects.length > 0 && <Badge variant="outline">{t('decisionLab.evidenceObservationalBadge')}</Badge>}
      <span className="text-sm font-medium">{t('decisionLab.evidenceVerified')}</span>
    </div>
    {empirical && <p className="break-all font-mono text-xs">{t('decisionLab.evidenceRunId')}: {empirical.run_id}</p>}
    {empirical && <p className="break-all font-mono text-xs">{t('decisionLab.evidenceRunDigest')}: {empirical.run_sha256}</p>}
    {empirical && <p className="break-all font-mono text-xs">{t('decisionLab.evidenceFitDigest')}: {empirical.fit_id} · {empirical.fit_sha256}</p>}
    {observation && <p className="break-all font-mono text-xs">{t('decisionLab.replayHash')}: {observation.replay_hash}</p>}
    {observation && <div className="grid gap-3 sm:grid-cols-2">
      <MetricCard label={t('decisionLab.empiricalBacklogDelta')}
        value={`${formatNumber(observation.delta_backlog_p05, locale, true)} / ${formatNumber(observation.delta_backlog_p50, locale, true)} / ${formatNumber(observation.delta_backlog_p95, locale, true)}`}
        detail={t('decisionLab.empiricalQuantiles')} />
      <MetricCard label={t('decisionLab.empiricalSlaDelta')}
        value={`${formatNumber(observation.delta_sla_resolved_p05, locale, true)} / ${formatNumber(observation.delta_sla_resolved_p50, locale, true)} / ${formatNumber(observation.delta_sla_resolved_p95, locale, true)}`}
        detail={t('decisionLab.empiricalQuantiles')} />
    </div>}
    {observation && <p className="text-sm">{t('decisionLab.empiricalJointRisk')}: {formatBps(observation.baseline_joint_constraint_violation_bps, locale)} / {formatBps(observation.alternative_joint_constraint_violation_bps, locale)}</p>}
    {eventEvidence && <div className="space-y-2 rounded border p-2" aria-label={t('decisionLab.evidenceEventTitle')}>
      <p className="text-sm font-medium">{t('decisionLab.evidenceEventTitle')}</p>
      <p className="text-xs text-muted-foreground">{t('decisionLab.eventInterpretation')}</p>
      <div className="grid gap-2 sm:grid-cols-3">
        <MetricCard label={t('decisionLab.eventWithinSla')}
          value={`${formatDecimal(eventEvidence.baseline.resolved_within_sla, locale)} / ${formatDecimal(eventEvidence.alternative.resolved_within_sla, locale)}`}
          detail={`${t('decisionLab.baseline')} / ${t('decisionLab.alternative')}`} />
        <MetricCard label={t('decisionLab.eventP50')}
          value={`${formatEventWait(eventEvidence.baseline.resolved_wait_seconds_p50, locale, t)} / ${formatEventWait(eventEvidence.alternative.resolved_wait_seconds_p50, locale, t)}`}
          detail={`${t('decisionLab.baseline')} / ${t('decisionLab.alternative')}`} />
        <MetricCard label={t('decisionLab.eventP95')}
          value={`${formatEventWait(eventEvidence.baseline.resolved_wait_seconds_p95, locale, t)} / ${formatEventWait(eventEvidence.alternative.resolved_wait_seconds_p95, locale, t)}`}
          detail={`${t('decisionLab.baseline')} / ${t('decisionLab.alternative')}`} />
      </div>
      <p className="break-all font-mono text-xs">{t('decisionLab.uploadSourceSha')}: {eventEvidence.source_sha256} · {t('decisionLab.eventEngineHash')}: {eventEvidence.event_engine_sha256}</p>
      <p className="break-all font-mono text-xs">{t('decisionLab.baselineHash')}: {eventEvidence.baseline_run_hash} · {t('decisionLab.alternativeHash')}: {eventEvidence.alternative_run_hash}</p>
      <p className="text-xs text-muted-foreground">{t('decisionLab.evidenceEventShift', {
        start: `${String(Math.floor(eventEvidence.config.shift_start_seconds / 3_600)).padStart(2, '0')}:${String(Math.floor(eventEvidence.config.shift_start_seconds % 3_600 / 60)).padStart(2, '0')}`,
        duration: eventEvidence.config.shift_seconds / 3_600,
      })}</p>
    </div>}
    {slaHoldout && <div className="space-y-2 rounded border p-2" aria-label={t('decisionLab.evidenceSlaHoldoutTitle')}>
      <p className="text-sm font-medium">{t('decisionLab.evidenceSlaHoldoutTitle')}</p>
      <p className="text-xs text-muted-foreground">{t('decisionLab.evidenceSlaHoldoutHint')}</p>
      <div className="grid gap-2 sm:grid-cols-3">
        <MetricCard label={t('decisionLab.evidenceSlaHoldoutTotals')}
          value={`${formatNumber(slaHoldout.diagnostic.observed_total_within_sla, locale)} / ${formatNumber(slaHoldout.diagnostic.predicted_total_within_sla, locale)}`}
          detail={`${t('decisionLab.evidenceSlaObserved')} / ${t('decisionLab.evidenceSlaPredicted')}`} />
        <MetricCard label={t('decisionLab.evidenceSlaHoldoutError')}
          value={formatDecimal(slaHoldout.diagnostic.daily_abs_error_sum, locale)} />
        <MetricCard label={t('decisionLab.evidenceSlaHoldoutBaselines')}
          value={t(slaHoldout.diagnostic.conditional_model_beats_all_baselines ? 'decisionLab.yes' : 'decisionLab.no')}
          detail={t('decisionLab.evidenceSlaHoldoutDays', { training: slaHoldout.diagnostic.training_days,
            holdout: slaHoldout.diagnostic.holdout_days })} />
      </div>
      <p className="break-all font-mono text-xs">{t('decisionLab.uploadHoldout')}: {slaHoldout.record_id} · {t('decisionLab.uploadHoldoutSha')}: {slaHoldout.record_sha256}</p>
      <p className="break-all font-mono text-xs">{t('decisionLab.uploadSourceSha')}: {slaHoldout.source_sha256} · {t('decisionLab.evidenceSlaEngineHash')}: {slaHoldout.engine_sha256}</p>
      {slaHoldout.diagnostic.limitations.length > 0 && <ul className="list-disc pl-5 text-xs text-muted-foreground">
        {slaHoldout.diagnostic.limitations.map((limit, index) => <li key={index}>{limit}</li>)}
      </ul>}
    </div>}
    {forecast && <div className="space-y-2 rounded border p-2" aria-label={t('decisionLab.evidenceForecastTitle')}>
      <p className="text-sm font-medium">{t('decisionLab.evidenceForecastTitle')}</p>
      <p className="text-xs text-muted-foreground">{t('decisionLab.evidenceForecastHint')}</p>
      <div className="grid gap-2 sm:grid-cols-3">
        <MetricCard label={t('decisionLab.forecastEvaluationDays')}
          value={formatNumber(forecast.forecast_points, locale)}
          detail={t('decisionLab.evidenceForecastCalibration', { points: forecast.calibration_points })} />
        <MetricCard label={t('decisionLab.forecastRollingCoverage')}
          value={forecast.rolling_observed_coverage_basis_points === null ? t('decisionLab.eventUnavailable')
            : formatBps(forecast.rolling_observed_coverage_basis_points, locale)}
          detail={t('decisionLab.forecastEvaluatedPoints') + ': ' + (forecast.interval_evaluated_points ?? t('decisionLab.eventUnavailable'))} />
        <MetricCard label={t('decisionLab.forecastFixedCoverage')}
          value={forecast.fixed_observed_coverage_basis_points === null ? t('decisionLab.eventUnavailable')
            : formatBps(forecast.fixed_observed_coverage_basis_points, locale)}
          detail={t('decisionLab.forecastEvaluatedPoints') + ': ' + (forecast.interval_evaluated_points ?? t('decisionLab.eventUnavailable'))} />
      </div>
      <p className="text-xs">{t('decisionLab.forecastArrivalError')}: {formatDecimal(forecast.arrival_abs_error_sum, locale)}</p>
      <div className="overflow-x-auto"><table className="w-full min-w-[28rem] text-left text-xs">
        <caption className="pb-1 text-left font-medium">{t('decisionLab.forecastBacklogErrors')}</caption>
        <thead><tr><th scope="col">{t('decisionLab.forecastModel')}</th><th scope="col">{t('decisionLab.forecastNoChange')}</th>
          <th scope="col">{t('decisionLab.forecastSeasonal')}</th><th scope="col">{t('decisionLab.forecastMeanChange')}</th></tr></thead>
        <tbody><tr>{[forecast.model_abs_error_sum, forecast.no_change_abs_error_sum,
          forecast.seasonal_naive_abs_error_sum, forecast.mean_change_abs_error_sum].map((value, index) =>
          <td key={index} className="tabular-nums">{formatDecimal(value, locale)}</td>)}</tr></tbody>
      </table></div>
      <p className="text-xs">{t('decisionLab.forecastBeatsBaselines')}: {t(forecast.model_beats_all_baselines
        ? 'decisionLab.yes' : 'decisionLab.no')}</p>
      <p className="break-all font-mono text-xs">{t('decisionLab.forecastRecordId')}: {forecast.record_id} · {t('decisionLab.forecastRecordSha')}: {forecast.record_sha256}</p>
      <p className="break-all font-mono text-xs">{t('decisionLab.uploadSourceSha')}: {forecast.source_sha256} · {t('decisionLab.evidenceForecastEngineHash')}: {forecast.calibration_engine_sha256}</p>
      <p className="text-xs text-muted-foreground">{t('decisionLab.evidenceForecastWindow')}: {dateTime(forecast.window_start_utc, locale)}</p>
    </div>}
    {screen && <p className="break-all text-sm">{t('decisionLab.evidenceScreenHash')}: {screen.screen_hash} · {t('decisionLab.empiricalScreenChoice')}: {t(`decisionLab.empiricalChoice.${screen.report.choice}`)}</p>}
    {effects.length > 0 && <div className="space-y-2">
      <p className="text-sm font-medium">{t('decisionLab.evidenceObservationalTitle')}</p>
      <p className="text-xs text-muted-foreground">{t('decisionLab.evidenceObservationalHint')}</p>
      {effects.map((effect) => <ObservationalEvidenceCard key={effect.estimate_id} effect={effect}
        dataCutoffUtc={brief.data_cutoff_utc} locale={locale} t={t} />)}
    </div>}
    <TextList title={t('decisionLab.evidenceSourceVersions')} items={brief.source_version_hashes ?? []} empty={t('decisionLab.noSources')} />
    {brief.replay.baseline_command && <p className="break-all font-mono text-xs">{t('decisionLab.evidenceBaselineReplay')}: {JSON.stringify([brief.replay.baseline_command.program, ...brief.replay.baseline_command.args])}</p>}
    {brief.replay.alternative_command && <p className="break-all font-mono text-xs">{t('decisionLab.evidenceAlternativeReplay')}: {JSON.stringify([brief.replay.alternative_command.program, ...brief.replay.alternative_command.args])}</p>}
    <TextList title={t('decisionLab.limitations')} items={brief.limitations} empty={t('decisionLab.noLimitations')} />
  </div>;
}

function ObservationalEvidenceCard({ effect, dataCutoffUtc, locale, t }: {
  effect: NonNullable<DecisionBrief['observational_effects']>[number];
  dataCutoffUtc?: string; locale: string; t: DecisionTranslator;
}) {
  const number = (value: number) => new Intl.NumberFormat(locale, { maximumSignificantDigits: 5 }).format(value);
  const status = (flag: boolean | null | undefined) => flag === null || flag === undefined
    ? t('decisionLab.evidenceCheckUnavailable')
    : flag ? t('decisionLab.evidenceCheckFlagged') : t('decisionLab.evidenceCheckNotFlagged');
  const flagged = effect.negative_control?.imbalance_flag || effect.pre_treatment_placebo?.imbalance_flag
    || effect.temporal_holdout?.sign_flip || effect.leave_one_unit_out.sign_flip
    || effect.leave_one_stratum_out_sign_flip;
  const cutoffMs = dataCutoffUtc ? Date.parse(dataCutoffUtc) : NaN;
  const retrospectivelyIngested = Number.isFinite(cutoffMs) && effect.data_ingested_at * 1000 > cutoffMs;
  return <div className="space-y-1 rounded border p-2 text-xs">
    <div className="flex flex-wrap items-center gap-2">
      <p className="font-medium">{effect.treatment_name} → {effect.outcome_name} · {effect.population}</p>
      {flagged && <Badge variant="destructive">{t('decisionLab.evidenceDiagnosticsFlagged')}</Badge>}
      {retrospectivelyIngested && <Badge variant="outline">{t('decisionLab.evidenceRetrospective')}</Badge>}
    </div>
    <p>{t('decisionLab.evidenceEffectEstimate')}: {number(effect.estimate)}
      {' '}[{number(effect.lower_bound)}, {number(effect.upper_bound)}] ({effect.interval_kind})</p>
    <p className="break-all">{t('decisionLab.evidenceVariables', {
      treatment: effect.treatment_variable_id, treatmentUnit: effect.treatment_unit,
      outcome: effect.outcome_variable_id, outcomeUnit: effect.outcome_unit })}</p>
    <p>{t('decisionLab.evidenceEffectWindow', { start: dateTime(effect.window_start, locale),
      end: dateTime(effect.window_end, locale) })}</p>
    <p>{t('decisionLab.evidenceIngestedAt')}: {dateTime(effect.data_ingested_at, locale)}</p>
    <p>{t('decisionLab.evidenceSampleSize', { units: effect.unit_count, strata: effect.strata_count })}</p>
    <p>{t('decisionLab.evidenceNegativeControl')}: {status(effect.negative_control?.imbalance_flag)}
      {' · '}{t('decisionLab.evidencePreTreatment')}: {status(effect.pre_treatment_placebo?.imbalance_flag)}</p>
    {effect.negative_control && <p>{t('decisionLab.evidenceNegativeControlDetail', {
      contrast: number(effect.negative_control.contrast), lower: number(effect.negative_control.lower_bound),
      upper: number(effect.negative_control.upper_bound), variable: effect.negative_control.variable_id,
      review: effect.negative_control.review_id })}</p>}
    {effect.pre_treatment_placebo && <p>{t('decisionLab.evidencePreTreatmentDetail', {
      contrast: number(effect.pre_treatment_placebo.contrast), lower: number(effect.pre_treatment_placebo.lower_bound),
      upper: number(effect.pre_treatment_placebo.upper_bound) })}</p>}
    <p>{t('decisionLab.evidenceTemporalFlip')}: {status(effect.temporal_holdout?.sign_flip)}
      {' · '}{t('decisionLab.evidenceUnitFlip')}: {status(effect.leave_one_unit_out.sign_flip)}
      {' · '}{t('decisionLab.evidenceStratumFlip')}: {status(effect.leave_one_stratum_out_sign_flip)}</p>
    <p>{t('decisionLab.evidencePermutation', { hits: effect.permutation_diagnostic.abs_at_least_observed,
      runs: effect.permutation_diagnostic.runs })}; {t('decisionLab.evidencePositivitySkipped', {
      skipped: effect.leave_one_unit_out.skipped_due_positivity })}</p>
    <p>{t('decisionLab.evidenceLeaveOneUnitDetail', {
      evaluated: effect.leave_one_unit_out.evaluated_units,
      shift: effect.leave_one_unit_out.max_abs_effect_shift === null
        ? t('decisionLab.evidenceCheckUnavailable') : number(effect.leave_one_unit_out.max_abs_effect_shift) })}</p>
    {effect.temporal_holdout && <p>{t('decisionLab.evidenceTemporalDetail', {
      cutoff: dateTime(effect.temporal_holdout.cutoff, locale),
      training: number(effect.temporal_holdout.training_estimate),
      heldout: number(effect.temporal_holdout.holdout_estimate),
      gap: number(effect.temporal_holdout.abs_gap),
      trainingUnits: effect.temporal_holdout.training_units,
      heldoutUnits: effect.temporal_holdout.holdout_units })}</p>}
    <p>{t('decisionLab.evidencePermutationDetail', {
      method: effect.permutation_diagnostic.method,
      median: number(effect.permutation_diagnostic.median_abs_placebo),
      max: number(effect.permutation_diagnostic.max_abs_placebo) })}</p>
    <p className="break-all font-mono">{effect.estimate_id} · {effect.model_id} · {effect.data_artifact_id} · {effect.data_sha256}</p>
    <p>{effect.method} · {effect.identification_state}</p>
  </div>;
}

function AssumptionNumber({ label, value, min, max, disabled, onChange }: {
  label: string; value: number; min: number; max?: number; disabled: boolean; onChange: (value: number) => void;
}) {
  return <label className="space-y-1 text-sm">{label}<Input aria-label={label} type="number" min={min} max={max} step={1} disabled={disabled}
    value={value} onChange={(event) => onChange(Number(event.target.value))} /></label>;
}

function choiceLabel(choice: string, t: DecisionTranslator): string {
  const key = `decisionLab.choice.${choice}`;
  return ['baseline', 'alternative', 'neither_feasible'].includes(choice) ? t(key) : choice;
}

function formatBps(value: number, locale: string): string {
  return new Intl.NumberFormat(locale, { style: 'percent', minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(value / 10_000);
}

function isNonNegativeInt(value: number): boolean { return Number.isSafeInteger(value) && value >= 0; }
function isPositiveInt(value: number): boolean { return Number.isSafeInteger(value) && value > 0; }

function ArtifactRow({ artifact, locale, t }: { artifact: DecisionArtifact; locale: string; t: DecisionTranslator }) {
  const refs = [artifact.candidate_id, artifact.target_snapshot_id, artifact.scenario_id, artifact.outcome_id]
    .filter((value): value is string => Boolean(value));
  return <div className="space-y-1 py-3 first:pt-0 last:pb-0">
    <div className="flex flex-wrap items-center gap-2"><Badge variant="outline">{artifact.kind}</Badge><span className="break-all font-mono text-xs">{artifact.id}</span>{artifact.status && <Badge variant="secondary">{artifact.status}</Badge>}</div>
    <p className="text-xs text-muted-foreground">{dateTime(artifact.created_at_unix, locale)} · {artifact.ticket_backed ? t('decisionLab.ticketBacked') : t('decisionLab.notTicketBacked')}</p>
    {refs.length > 0 && <p className="break-words text-xs text-muted-foreground">{refs.join(' · ')}</p>}
    {artifact.replay_hash && <p className="break-all font-mono text-xs text-muted-foreground">{t('decisionLab.replayHash')}: {artifact.replay_hash}</p>}
  </div>;
}
