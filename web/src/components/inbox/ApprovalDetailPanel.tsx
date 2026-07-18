import { useState, type ComponentType, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import {
  ShieldCheck,
  ShieldAlert,
  CheckCircle2,
  XCircle,
  ChevronRight,
  ChevronDown,
  AlertTriangle,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  ActorAvatar,
  Badge,
  Button,
  Textarea,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
} from '@/components/mds';
import { timeAgo } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import type { ApprovalItem } from '@/lib/api';
import {
  approvalRisk,
  riskNeedsConfirm,
  extractPlanFacts,
  hasPlanFacts,
  type RiskLevel,
} from '@/lib/approval-risk';
import { decideApproval } from '@/lib/api-custom-skills';
import { parseSkillCreatePayload, type SkillCreatePayload } from '@/components/skills/skill-create-payload';
import { formatTimeSaved } from '@/components/skills/status-meta';

// ── Local mds-token property primitives (replace the Calm Glass PropertyRow) ──

function Section({ title, children }: { title: ReactNode; children: ReactNode }) {
  return (
    <section className="space-y-1">
      <h3 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">{title}</h3>
      <div className="space-y-0.5">{children}</div>
    </section>
  );
}

function Row({ label, icon: Icon, children }: { label: ReactNode; icon?: ComponentType<{ className?: string }>; children: ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-3 py-1 text-sm">
      <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
        {Icon && <Icon className="size-3.5 shrink-0" />}
        {label}
      </span>
      <span className="ml-auto flex min-w-0 items-center justify-end gap-1.5 text-right">{children}</span>
    </div>
  );
}

/** Inline monospace value. */
function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn('font-mono text-xs text-muted-foreground', className)}>{children}</span>;
}

/** Action kinds with a hand-written plain-language description key. */
const DESCRIBED_KINDS = new Set([
  'browser_action',
  'tool_call',
  'skill_activation',
  'skill_create',
  'strategic_plan',
  'agent_hire',
  'wiki_ingest',
]);

function riskBadgeVariant(level: RiskLevel): 'destructive' | 'secondary' | 'outline' {
  return level === 'high' ? 'destructive' : level === 'medium' ? 'secondary' : 'outline';
}

/** Small risk pill shown at the plan-summary header (whole-action level only —
 *  never per-token, per arXiv:2605.28571 which found over-granular uncertainty
 *  breeds over-trust). */
function RiskBadge({ level, label }: { level: RiskLevel; label: string }) {
  const Icon = level === 'high' ? ShieldAlert : ShieldCheck;
  return (
    <Badge variant={riskBadgeVariant(level)} className="shrink-0">
      <Icon aria-hidden="true" />
      {label}
    </Badge>
  );
}

/**
 * ApprovalDetailPanel — the right-pane body shown when an approval row is opened
 * in the Inbox split (§5.6). Renders the request metadata + the raw payload so
 * the operator reviews the artifact that actually takes effect (security
 * convention 4), then approves / rejects inline.
 *
 * `skill_create` approvals get a dedicated view that surfaces the SKILL.md that
 * installs on approve + the safety report + the human fields, and decides
 * in-place (with a mandatory rejection reason). That path is self-contained — it
 * calls `approvals.decide` itself to carry the reason and read the `side_effect`
 * — so it does NOT delegate to `onApprove`/`onReject` (which would decide a
 * second time and hit the terminal-state guard). Instead it signals the parent
 * via `onDecided` so the row leaves the queue immediately.
 */
export function ApprovalDetailPanel({
  approval,
  agentName,
  onApprove,
  onReject,
  onDecided,
}: {
  approval: ApprovalItem;
  agentName?: string;
  onApprove: () => void;
  onReject: () => void;
  /** Called after a self-contained (skill_create) decision succeeds, so the
   *  parent can remove the decided row without issuing a second decide. */
  onDecided?: () => void;
}) {
  // ── skill_create specialization ──
  if (approval.kind === 'skill_create') {
    const parsed = parseSkillCreatePayload(approval.payload);
    if (parsed) {
      return (
        <SkillCreateApprovalView
          approval={approval}
          agentName={agentName}
          payload={parsed}
          onDecided={onDecided}
        />
      );
    }
    // Missing artifact → fall through to the generic view.
  }

  return (
    <GenericApprovalView approval={approval} agentName={agentName} onApprove={onApprove} onReject={onReject} />
  );
}

/**
 * GenericApprovalView — evidence-based default approval card. Leads with a
 * plain-language plan summary ("what does this AI employee intend to do") + a
 * whole-action risk badge, so the operator reviews the plan before the decision
 * buttons become the visual focus (arXiv:2604.04918). Full payload is a
 * one-click, opt-in spot-check — not force-read (arXiv:2606.05391). High-risk
 * actions gate approve behind a confirmation dialog.
 */
function GenericApprovalView({
  approval,
  agentName,
  onApprove,
  onReject,
}: {
  approval: ApprovalItem;
  agentName?: string;
  onApprove: () => void;
  onReject: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const [spotCheck, setSpotCheck] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);

  const risk = approvalRisk(approval.kind, approval.payload);
  const facts = extractPlanFacts(approval.payload);
  const described = DESCRIBED_KINDS.has(approval.kind);
  const kindDesc = described ? t(`approval.plan.kind.${approval.kind}`) : t('approval.plan.kind.unknown');

  const ttlAt = approval.created_at
    ? new Date(Date.parse(approval.created_at) + approval.ttl_seconds * 1000).toISOString()
    : undefined;
  const payload =
    typeof approval.payload === 'string' ? approval.payload : JSON.stringify(approval.payload, null, 2);

  const requestApprove = () => {
    if (riskNeedsConfirm(risk)) setConfirmOpen(true);
    else onApprove();
  };

  return (
    <div className="space-y-4">
      {/* ── Plan summary first (arXiv:2604.04918) ── */}
      <Section title={t('approval.plan.title')}>
        <div className="space-y-2 rounded-lg bg-muted p-3">
          <div className="flex items-start gap-2">
            <p className="min-w-0 flex-1 text-sm font-medium text-foreground">{kindDesc}</p>
            <RiskBadge level={risk} label={t(`approval.risk.${risk}`)} />
          </div>
          <p className="text-sm text-muted-foreground">{approval.summary}</p>
          {approval.agent_id && (
            <div className="flex items-center gap-1.5 pt-0.5 text-xs text-muted-foreground">
              <ActorAvatar actorType="agent" size="xs" name={agentName ?? approval.agent_id} />
              <span className="truncate">{agentName ?? approval.agent_id}</span>
              {!described && <Mono className="text-[11px]">{approval.kind}</Mono>}
            </div>
          )}
        </div>
      </Section>

      {/* ── Scope of impact (heuristic verification aid, arXiv:2606.05391) ── */}
      {hasPlanFacts(facts) && (
        <Section title={t('approval.plan.scope')}>
          {facts.scope && (
            <Row label={t('approval.plan.scopeLabel')}>
              <Mono>{facts.scope}</Mono>
            </Row>
          )}
          {facts.tools.length > 0 && (
            <Row label={t('approval.plan.tools')}>
              <span className="flex flex-wrap justify-end gap-1">
                {facts.tools.map((tool) => (
                  <Mono key={tool} className="rounded bg-muted px-1 text-[11px]">
                    {tool}
                  </Mono>
                ))}
              </span>
            </Row>
          )}
          {facts.targets.length > 0 && (
            <Row label={t('approval.plan.targets')}>
              <span className="flex flex-col items-end gap-0.5">
                {facts.targets.map((target) => (
                  <span key={target} className="max-w-full truncate text-[11px]" title={target}>
                    {target}
                  </span>
                ))}
              </span>
            </Row>
          )}
        </Section>
      )}

      {/* ── One-click spot-check — opt-in, not force-read (arXiv:2606.05391) ── */}
      <div className="space-y-2">
        <button
          type="button"
          onClick={() => setSpotCheck((v) => !v)}
          aria-expanded={spotCheck}
          className="flex w-full items-center gap-1.5 rounded-md px-1.5 py-1.5 text-left text-xs font-medium uppercase tracking-wide text-muted-foreground transition-colors hover:bg-surface-hover focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          {spotCheck ? (
            <ChevronDown className="size-3.5 shrink-0" aria-hidden="true" />
          ) : (
            <ChevronRight className="size-3.5 shrink-0" aria-hidden="true" />
          )}
          {t('approval.spotCheck.toggle')}
        </button>
        {spotCheck && (
          <div className="space-y-3 pl-1.5">
            <div className="grid grid-cols-1 gap-0.5">
              <Row label={t('inbox.approval.kind')}>
                <Mono>{approval.kind}</Mono>
              </Row>
              <Row label={t('inbox.approval.created')}>{timeAgo(approval.created_at)}</Row>
              {ttlAt && <Row label={t('inbox.approval.ttl')}>{timeAgo(ttlAt)}</Row>}
            </div>
            <pre className="max-h-64 overflow-auto rounded-lg bg-muted p-2 text-[11px] leading-relaxed text-muted-foreground">
              {payload}
            </pre>
          </div>
        )}
      </div>

      {/* ── Decision (after the summary is read) ── */}
      <div className="flex items-center gap-2">
        <Button variant="brand" onClick={requestApprove} className="flex-1">
          {t('inbox.approval.approve')}
        </Button>
        <Button variant="destructive" onClick={onReject} className="flex-1">
          {t('inbox.approval.reject')}
        </Button>
      </div>

      {/* High-risk second confirmation. */}
      <Dialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>{t('approval.confirm.title')}</DialogTitle>
            <DialogDescription>
              {intl.formatMessage({ id: 'approval.confirm.message' }, { summary: approval.summary })}
            </DialogDescription>
          </DialogHeader>
          <div className="flex items-start gap-2.5 px-0.5 text-sm text-muted-foreground">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden="true" />
            <span>{approval.summary}</span>
          </div>
          <DialogFooter>
            <DialogClose render={<Button variant="outline">{t('common.cancel')}</Button>} />
            <Button
              variant="brand"
              onClick={() => {
                setConfirmOpen(false);
                onApprove();
              }}
            >
              {t('approval.confirm.approve')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// ── skill_create approval view ──────────────────────────────

type DecidedState = { kind: 'approved'; installed: string; selfApproved: boolean } | { kind: 'rejected' } | null;

function SkillCreateApprovalView({
  approval,
  agentName,
  payload,
  onDecided,
}: {
  approval: ApprovalItem;
  agentName?: string;
  payload: SkillCreatePayload;
  onDecided?: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [decided, setDecided] = useState<DecidedState>(null);

  const sr = payload.safety_report;
  const risk = approvalRisk(approval.kind, approval.payload);

  const handleApprove = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const res = await decideApproval(approval.id, true);
      const se = res.side_effect ?? {};
      const installed = se.installed_skill ?? payload.slug;
      const selfApproved = se.self_approved === true;
      setDecided({ kind: 'approved', installed, selfApproved });
      toast.success(intl.formatMessage({ id: 'inbox.skillCreate.installedToast' }, { name: installed }));
      onDecided?.();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleReject = async () => {
    if (busy || !reason.trim()) return;
    setBusy(true);
    try {
      const res = await decideApproval(approval.id, false, reason.trim());
      const rejectedId = res.side_effect?.custom_skill_rejected ?? payload.custom_skill_id;
      setDecided({ kind: 'rejected' });
      toast.success(intl.formatMessage({ id: 'inbox.skillCreate.rejectedToast' }, { id: rejectedId }));
      onDecided?.();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2">
        <p className="min-w-0 flex-1 text-sm text-foreground">{approval.summary}</p>
        <RiskBadge level={risk} label={t(`approval.risk.${risk}`)} />
      </div>

      {/* Human-facing fields (given for context; the review target is the SKILL.md) */}
      <Section title={t('inbox.skillCreate.fields')}>
        <Row label={t('skills.new.form.displayName')}>{payload.display_name}</Row>
        <Row label={t('skills.custom.slug')}>
          <Mono>{payload.slug}</Mono>
        </Row>
        {payload.description_human && (
          <Row label={t('skills.new.form.description')}>
            <span className="whitespace-pre-wrap text-sm">{payload.description_human}</span>
          </Row>
        )}
        <Row label={t('skills.new.form.timeSaved')}>
          {formatTimeSaved(intl, payload.time_saved_value, payload.time_saved_unit)}
        </Row>
        <Row label={t('skills.custom.builtBy')}>
          <span className="flex items-center gap-1.5">
            <ActorAvatar actorType="agent" size="xs" name={agentName ?? payload.built_by_agent} />
            <span className="truncate">{payload.built_by_agent || agentName || approval.agent_id}</span>
          </span>
        </Row>
        <Row label={t('skills.custom.createdBy')}>{payload.created_by_user || '—'}</Row>
      </Section>

      {/* Safety report */}
      <Section title={t('inbox.skillCreate.safety')}>
        <div className="space-y-2">
          <div className="flex items-center gap-2 px-0.5">
            {sr?.passed ? (
              <ShieldCheck className="size-4 text-success" />
            ) : (
              <ShieldAlert className="size-4 text-warning" />
            )}
            <span className="text-sm text-foreground">{t('inbox.skillCreate.risk')}</span>
            <Badge variant={sr?.passed ? 'secondary' : 'outline'} className="ml-auto">
              {sr?.risk_level ?? '—'}
            </Badge>
          </div>
          {sr && sr.findings.length > 0 ? (
            <ul className="space-y-1 px-0.5">
              {sr.findings.map((f, i) => (
                <li key={i} className="text-xs text-muted-foreground">
                  <span className="font-medium uppercase text-foreground">{f.severity}</span>
                  <span className="mx-1 text-muted-foreground/40">|</span>
                  {f.category}
                  {f.line_number != null && <span className="text-muted-foreground/70"> (L{f.line_number})</span>}
                  <span className="block text-muted-foreground/80">{f.description}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="px-0.5 text-xs text-muted-foreground">{t('inbox.skillCreate.noFindings')}</p>
          )}
          {sr?.sandbox_trial && !sr.sandbox_trial.ran && (
            <p className="px-0.5 text-[11px] text-muted-foreground/70">{t('inbox.skillCreate.sandboxSkipped')}</p>
          )}
        </div>
      </Section>

      {/* The artifact that installs on approve — reviewed verbatim */}
      <Section title={t('inbox.skillCreate.skillMd')}>
        <pre className="max-h-72 overflow-auto rounded-lg bg-muted p-2 text-[11px] leading-relaxed text-muted-foreground">
          {payload.skill_md}
        </pre>
      </Section>

      {/* Decision */}
      {decided ? (
        <div
          className={cn(
            'flex items-start gap-2 rounded-lg border p-3 text-sm',
            decided.kind === 'approved'
              ? 'border-success/40 bg-success/10 text-success'
              : 'border-destructive/40 bg-destructive/10 text-destructive',
          )}
        >
          {decided.kind === 'approved' ? (
            <>
              <CheckCircle2 className="mt-0.5 size-4 shrink-0" />
              <span>
                {intl.formatMessage({ id: 'inbox.skillCreate.approvedDone' }, { name: decided.installed })}
                {decided.selfApproved && ` · ${t('inbox.skillCreate.selfApproved')}`}
              </span>
            </>
          ) : (
            <>
              <XCircle className="mt-0.5 size-4 shrink-0" />
              <span>{t('inbox.skillCreate.rejectedDone')}</span>
            </>
          )}
        </div>
      ) : rejecting ? (
        <div className="space-y-2">
          <Textarea
            className="h-20 resize-y"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder={t('inbox.skillCreate.reasonPlaceholder')}
            autoFocus
          />
          <div className="flex items-center gap-2">
            <Button variant="destructive" onClick={handleReject} disabled={busy || !reason.trim()} className="flex-1">
              {t('inbox.skillCreate.confirmReject')}
            </Button>
            <Button variant="ghost" onClick={() => setRejecting(false)} disabled={busy}>
              {t('common.cancel')}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex items-center gap-2">
          <Button variant="brand" onClick={handleApprove} disabled={busy} className="flex-1">
            {t('inbox.skillCreate.approve')}
          </Button>
          <Button variant="destructive" onClick={() => setRejecting(true)} disabled={busy} className="flex-1">
            {t('inbox.skillCreate.reject')}
          </Button>
        </div>
      )}
    </div>
  );
}
