import { useState } from 'react';
import { useIntl } from 'react-intl';
import { ShieldCheck, ShieldAlert, CheckCircle2, XCircle } from 'lucide-react';
import { Button, PropertyRow, PropertySection, CharacterAvatar, Mono, Badge } from '@/components/ui';
import { cn } from '@/lib/utils';
import { timeAgo } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import type { ApprovalItem } from '@/lib/api';
import { decideApproval } from '@/lib/api-custom-skills';
import { parseSkillCreatePayload, type SkillCreatePayload } from '@/components/skills/skill-create-payload';
import { formatTimeSaved } from '@/components/skills/status-meta';

/**
 * ApprovalDetailPanel — right-panel body shown when `Enter` opens an approval
 * row (§5.2 T4.3). Renders the request metadata + the raw payload so the
 * operator审 the artifact that actually takes effect (security convention 4),
 * then approve / reject inline.
 *
 * `skill_create` approvals (V13-T13.3) get a dedicated view that surfaces the
 * SKILL.md that installs on approve + the safety report + the human fields, and
 * decides in-place (with a mandatory rejection reason). That path is
 * self-contained — it calls `approvals.decide` itself to carry the reason and
 * read the `side_effect` — so it does NOT delegate to `onApprove`/`onReject`
 * (which would decide a second time and hit the terminal-state guard). Instead
 * it signals the parent via `onDecided` (A1) so the row leaves the queue
 * immediately, with the decision made exactly once.
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
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  // ── skill_create specialization (T13.3) ──
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
    // Missing artifact → fall through to the generic raw-payload view.
  }

  const ttlAt = approval.created_at
    ? new Date(Date.parse(approval.created_at) + approval.ttl_seconds * 1000).toISOString()
    : undefined;
  const payload =
    typeof approval.payload === 'string' ? approval.payload : JSON.stringify(approval.payload, null, 2);

  return (
    <div className="space-y-4">
      <p className="text-sm text-stone-800 dark:text-stone-100">{approval.summary}</p>

      <PropertySection title={t('inbox.approval.panelTitle')}>
        <PropertyRow label={t('inbox.approval.kind')}>
          <Mono>{approval.kind}</Mono>
        </PropertyRow>
        {approval.agent_id && (
          <PropertyRow label={t('inbox.approval.agent')}>
            <span className="flex items-center gap-1.5">
              <CharacterAvatar agentId={approval.agent_id} name={agentName ?? approval.agent_id} size={18} />
              <span className="truncate">{agentName ?? approval.agent_id}</span>
            </span>
          </PropertyRow>
        )}
        <PropertyRow label={t('inbox.approval.created')}>{timeAgo(approval.created_at)}</PropertyRow>
        {ttlAt && <PropertyRow label={t('inbox.approval.ttl')}>{timeAgo(ttlAt)}</PropertyRow>}
      </PropertySection>

      <PropertySection title={t('inbox.approval.payload')}>
        <pre className="max-h-64 overflow-auto rounded-control bg-stone-500/8 p-2 text-[11px] leading-relaxed text-stone-700 dark:bg-white/5 dark:text-stone-300">
          {payload}
        </pre>
      </PropertySection>

      <div className="flex items-center gap-2">
        <Button size="sm" variant="primary" onClick={onApprove} className="flex-1">
          {t('inbox.approval.approve')}
        </Button>
        <Button size="sm" variant="danger" onClick={onReject} className="flex-1">
          {t('inbox.approval.reject')}
        </Button>
      </div>
    </div>
  );
}

// ── skill_create approval view (T13.3) ──────────────────────

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
      // Signal the queue to drop this row (no second decide — A1).
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
      // Signal the queue to drop this row (no second decide — A1).
      onDecided?.();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      <p className="text-sm text-stone-800 dark:text-stone-100">{approval.summary}</p>

      {/* Human-facing fields (given for context; the review target is the SKILL.md) */}
      <PropertySection title={t('inbox.skillCreate.fields')}>
        <PropertyRow label={t('skills.new.form.displayName')}>{payload.display_name}</PropertyRow>
        <PropertyRow label={t('skills.custom.slug')}>
          <Mono>{payload.slug}</Mono>
        </PropertyRow>
        {payload.description_human && (
          <PropertyRow label={t('skills.new.form.description')}>
            <span className="whitespace-pre-wrap">{payload.description_human}</span>
          </PropertyRow>
        )}
        <PropertyRow label={t('skills.new.form.timeSaved')}>
          {formatTimeSaved(intl, payload.time_saved_value, payload.time_saved_unit)}
        </PropertyRow>
        <PropertyRow label={t('skills.custom.builtBy')}>
          <span className="flex items-center gap-1.5">
            <CharacterAvatar
              agentId={payload.built_by_agent || approval.agent_id}
              name={agentName ?? payload.built_by_agent}
              size={18}
            />
            <span className="truncate">{payload.built_by_agent || agentName || approval.agent_id}</span>
          </span>
        </PropertyRow>
        <PropertyRow label={t('skills.custom.createdBy')}>{payload.created_by_user || '—'}</PropertyRow>
      </PropertySection>

      {/* Safety report */}
      <PropertySection title={t('inbox.skillCreate.safety')}>
        <div className="space-y-2">
          <div className="flex items-center gap-2 px-1.5">
            {sr?.passed ? (
              <ShieldCheck className="h-4 w-4 text-emerald-500" />
            ) : (
              <ShieldAlert className="h-4 w-4 text-amber-500" />
            )}
            <span className="text-sm text-stone-700 dark:text-stone-200">{t('inbox.skillCreate.risk')}</span>
            <Badge tone={sr?.passed ? 'success' : 'warning'} className="ml-auto">
              {sr?.risk_level ?? '—'}
            </Badge>
          </div>
          {sr && sr.findings.length > 0 ? (
            <ul className="space-y-1 px-1.5">
              {sr.findings.map((f, i) => (
                <li key={i} className="text-xs text-stone-600 dark:text-stone-400">
                  <span className="font-semibold uppercase">{f.severity}</span>
                  <span className="mx-1 text-stone-300 dark:text-stone-600">|</span>
                  {f.category}
                  {f.line_number != null && <span className="text-stone-400"> (L{f.line_number})</span>}
                  <span className="block text-stone-500 dark:text-stone-500">{f.description}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="px-1.5 text-xs text-stone-400 dark:text-stone-500">{t('inbox.skillCreate.noFindings')}</p>
          )}
          {sr?.sandbox_trial && !sr.sandbox_trial.ran && (
            <p className="px-1.5 text-[11px] text-stone-400 dark:text-stone-500">
              {t('inbox.skillCreate.sandboxSkipped')}
            </p>
          )}
        </div>
      </PropertySection>

      {/* The artifact that installs on approve — reviewed verbatim */}
      <PropertySection title={t('inbox.skillCreate.skillMd')}>
        <pre className="max-h-72 overflow-auto rounded-control bg-stone-500/8 p-2 text-[11px] leading-relaxed text-stone-700 dark:bg-white/5 dark:text-stone-300">
          {payload.skill_md}
        </pre>
      </PropertySection>

      {/* Decision */}
      {decided ? (
        <div
          className={cn(
            'flex items-start gap-2 rounded-lg border p-3 text-sm',
            decided.kind === 'approved'
              ? 'border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-800 dark:bg-emerald-900/20 dark:text-emerald-400'
              : 'border-rose-200 bg-rose-50 text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-400',
          )}
        >
          {decided.kind === 'approved' ? (
            <>
              <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" />
              <span>
                {intl.formatMessage({ id: 'inbox.skillCreate.approvedDone' }, { name: decided.installed })}
                {decided.selfApproved && ` · ${t('inbox.skillCreate.selfApproved')}`}
              </span>
            </>
          ) : (
            <>
              <XCircle className="mt-0.5 h-4 w-4 shrink-0" />
              <span>{t('inbox.skillCreate.rejectedDone')}</span>
            </>
          )}
        </div>
      ) : rejecting ? (
        <div className="space-y-2">
          <textarea
            className="h-20 w-full resize-y rounded-lg border border-[var(--panel-border)] bg-[var(--panel-fill)] p-2 text-sm text-stone-800 placeholder:text-stone-400 focus-visible:border-rose-400/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-rose-500/25 dark:text-stone-100"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder={t('inbox.skillCreate.reasonPlaceholder')}
            autoFocus
          />
          <div className="flex items-center gap-2">
            <Button size="sm" variant="danger" onClick={handleReject} disabled={busy || !reason.trim()} className="flex-1">
              {t('inbox.skillCreate.confirmReject')}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setRejecting(false)} disabled={busy}>
              {t('common.cancel')}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex items-center gap-2">
          <Button size="sm" variant="primary" onClick={handleApprove} disabled={busy} className="flex-1">
            {t('inbox.skillCreate.approve')}
          </Button>
          <Button size="sm" variant="danger" onClick={() => setRejecting(true)} disabled={busy} className="flex-1">
            {t('inbox.skillCreate.reject')}
          </Button>
        </div>
      )}
    </div>
  );
}
