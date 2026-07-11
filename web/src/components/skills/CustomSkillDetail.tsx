import { useState, useEffect, useRef, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import {
  Puzzle,
  Send,
  Archive,
  Save,
  Loader2,
  ShieldAlert,
  Clock,
  CheckCircle2,
  XCircle,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Button,
  Badge,
  Field,
  controlClass,
  EmptyState,
  Mono,
  PropertyRow,
  PropertySection,
  CharacterAvatar,
  DuDu,
  celebrate,
} from '@/components/ui';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import { timeAgo } from '@/lib/format';
import {
  listCustomSkills,
  updateCustomSkill,
  submitCustomSkill,
  retireCustomSkill,
  type CustomSkillRecord,
  type TimeSavedUnit,
} from '@/lib/api-custom-skills';
import { statusMeta, formatTimeSaved } from './status-meta';

const textareaClass = cn(controlClass, 'h-auto min-h-[80px] resize-y py-2 leading-relaxed');
const TIME_UNITS: TimeSavedUnit[] = ['minutes_per_use', 'hours_per_month'];
const POLL_MS = 4000;

function splitTags(tags: string): string[] {
  return tags.split(',').map((s) => s.trim()).filter(Boolean);
}

/**
 * CustomSkillDetail — `/skills/custom/:id` (T13.2). All fields + status +
 * approval history + status-appropriate actions. Draft/rejected are editable
 * and (re)submittable; pending shows a waiting state; rejected shows the
 * reason. Polls for a transition to `approved` to fire the on-launch
 * celebration (T13.4). No per-skill call counter exists on the backend, so the
 * time-saving figure is shown as an estimate, honestly labeled.
 */
export function CustomSkillDetail({ id }: { id: string }) {
  const intl = useIntl();
  const t = (msgId: string) => intl.formatMessage({ id: msgId });
  const navigate = useNavigate();

  const [record, setRecord] = useState<CustomSkillRecord | null>(null);
  const [loading, setLoading] = useState(true);
  const [notFound, setNotFound] = useState(false);

  // Editable human fields (draft / rejected).
  const [displayName, setDisplayName] = useState('');
  const [descriptionHuman, setDescriptionHuman] = useState('');
  const [timeSavedValue, setTimeSavedValue] = useState('0');
  const [timeSavedUnit, setTimeSavedUnit] = useState<TimeSavedUnit>('minutes_per_use');
  const [tags, setTags] = useState<string[]>([]);

  const [saving, setSaving] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [retiring, setRetiring] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const prevStatusRef = useRef<string | null>(null);

  const hydrateForm = useCallback((rec: CustomSkillRecord) => {
    setDisplayName(rec.display_name);
    setDescriptionHuman(rec.description_human);
    setTimeSavedValue(String(rec.time_saved_value));
    setTimeSavedUnit(rec.time_saved_unit);
    setTags(splitTags(rec.tags));
  }, []);

  const load = useCallback(
    async (opts?: { hydrate?: boolean }) => {
      try {
        const res = await listCustomSkills();
        const found = res.custom_skills.find((s) => s.id === id) ?? null;
        if (!found) {
          setNotFound(true);
          setRecord(null);
          return;
        }
        // T13.4 — celebrate the transition into `approved`.
        if (prevStatusRef.current && prevStatusRef.current !== 'approved' && found.status === 'approved') {
          celebrate('badge', { message: t('skills.custom.celebrate') });
          toast.success(t('skills.custom.celebrate'));
        }
        prevStatusRef.current = found.status;
        setRecord(found);
        setNotFound(false);
        if (opts?.hydrate) hydrateForm(found);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      } finally {
        setLoading(false);
      }
    },
    [id, intl, t, hydrateForm],
  );

  useEffect(() => {
    setLoading(true);
    load({ hydrate: true });
  }, [load]);

  // Poll while pending/generating so an approval reflects (and celebrates) live.
  useEffect(() => {
    const status = record?.status;
    if (status !== 'pending_approval' && status !== 'generating') return;
    const timer = setInterval(() => load(), POLL_MS);
    return () => clearInterval(timer);
  }, [record?.status, load]);

  const editable = record?.status === 'draft' || record?.status === 'rejected';
  const canSubmit = editable;
  // Retire is a valid transition from every status except `retired` itself.
  const canRetire = record != null && record.status !== 'retired';

  const handleSave = useCallback(async () => {
    if (!record || saving) return;
    setSaving(true);
    try {
      const updated = await updateCustomSkill({
        id: record.id,
        display_name: displayName.trim(),
        description_human: descriptionHuman.trim(),
        time_saved_value: Number(timeSavedValue) || 0,
        time_saved_unit: timeSavedUnit,
        tags: tags.join(','),
      });
      setRecord(updated);
      toast.success(t('common.saved'));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  }, [record, saving, displayName, descriptionHuman, timeSavedValue, timeSavedUnit, tags, intl, t]);

  const handleSubmit = useCallback(async () => {
    if (!record || submitting) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      await submitCustomSkill(record.id);
      toast.success(t('skills.custom.submitted'));
      await load();
    } catch (e) {
      setSubmitError(formatError(e));
    } finally {
      setSubmitting(false);
    }
  }, [record, submitting, load, t]);

  const handleRetire = useCallback(async () => {
    if (!record || retiring) return;
    setRetiring(true);
    try {
      await retireCustomSkill(record.id);
      toast.success(t('skills.custom.retired'));
      await load();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setRetiring(false);
    }
  }, [record, retiring, load, t, intl]);

  if (loading) {
    return (
      <Page>
        <div className="py-16 text-center text-stone-400">{t('common.loading')}</div>
      </Page>
    );
  }

  if (notFound || !record) {
    return (
      <Page>
        <PageHeader icon={Puzzle} title={t('skills.custom.title')} />
        <Card>
          <EmptyState
            icon={Puzzle}
            title={t('skills.custom.notFound')}
            action={<Button onClick={() => navigate('/skills')}>{t('skills.custom.backToList')}</Button>}
          />
        </Card>
      </Page>
    );
  }

  const meta = statusMeta(record.status);
  const isApproved = record.status === 'approved';

  return (
    <Page>
      <PageHeader
        icon={Puzzle}
        title={record.display_name}
        subtitle={record.slug}
        actions={<Badge tone={meta.tone}>{t(meta.labelKey)}</Badge>}
      />

      <div className="space-y-6">
        {/* Approved banner + DuDu (T13.4) */}
        {isApproved && (
          <Card className="flex items-center gap-4 border-emerald-300/60 bg-emerald-50/60 dark:border-emerald-800/60 dark:bg-emerald-900/15">
            <DuDu face="proud" size="sm" />
            <div>
              <p className="text-sm font-semibold text-emerald-700 dark:text-emerald-400">
                {t('skills.custom.liveTitle')}
              </p>
              <p className="text-xs text-emerald-600/80 dark:text-emerald-500/80">{t('skills.custom.liveHint')}</p>
            </div>
          </Card>
        )}

        {/* Rejection reason (T13.2) */}
        {record.status === 'rejected' && record.rejection_reason && (
          <Card className="border-rose-200 bg-rose-50/60 dark:border-rose-800 dark:bg-rose-900/15">
            <div className="flex items-start gap-2 text-sm text-rose-700 dark:text-rose-400">
              <XCircle className="mt-0.5 h-4 w-4 shrink-0" />
              <div>
                <p className="font-medium">{t('skills.custom.rejectedTitle')}</p>
                <p className="mt-0.5 text-xs">{record.rejection_reason}</p>
              </div>
            </div>
          </Card>
        )}

        {/* Pending waiting state */}
        {record.status === 'pending_approval' && (
          <Card className="flex items-center gap-3 text-sm text-amber-700 dark:text-amber-400">
            <Clock className="h-4 w-4 shrink-0" />
            {t('skills.custom.waiting')}
          </Card>
        )}

        {/* Generating */}
        {record.status === 'generating' && (
          <Card className="flex items-center gap-3 text-sm text-sky-700 dark:text-sky-400">
            <Loader2 className="h-4 w-4 shrink-0 animate-spin" />
            {t('skills.custom.generatingHint')}
          </Card>
        )}

        {/* Human fields — editable when draft/rejected, else read-only */}
        {editable ? (
          <Section title={t('skills.custom.fields')}>
            <div className="space-y-4">
              <Field label={t('skills.new.form.displayName')} required>
                <input className={controlClass} value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
              </Field>
              <Field label={t('skills.new.form.description')}>
                <textarea
                  className={textareaClass}
                  value={descriptionHuman}
                  onChange={(e) => setDescriptionHuman(e.target.value)}
                  rows={3}
                />
              </Field>
              <Field label={t('skills.new.form.timeSaved')} help={t('skills.new.form.timeSavedHelp')}>
                <div className="flex gap-2">
                  <input
                    type="number"
                    min={0}
                    className={cn(controlClass, 'w-28')}
                    value={timeSavedValue}
                    onChange={(e) => setTimeSavedValue(e.target.value)}
                  />
                  <select
                    className={cn(controlClass, 'w-auto flex-1')}
                    value={timeSavedUnit}
                    onChange={(e) => setTimeSavedUnit(e.target.value as TimeSavedUnit)}
                  >
                    {TIME_UNITS.map((u) => (
                      <option key={u} value={u}>
                        {t(`skills.custom.unit.${u}`)}
                      </option>
                    ))}
                  </select>
                </div>
              </Field>
              <Field label={t('skills.new.form.tags')}>
                <ChipEditor values={tags} onChange={setTags} />
              </Field>
              <div>
                <Button
                  variant="secondary"
                  icon={saving ? Loader2 : Save}
                  onClick={handleSave}
                  disabled={saving || !displayName.trim()}
                  className={cn(saving && '[&>svg]:animate-spin')}
                >
                  {saving ? t('common.saving') : t('common.save')}
                </Button>
              </div>
            </div>
          </Section>
        ) : (
          <Card>
            <PropertySection title={t('skills.custom.fields')}>
              <PropertyRow label={t('skills.new.form.displayName')}>{record.display_name}</PropertyRow>
              <PropertyRow label={t('skills.custom.slug')}>
                <Mono>{record.slug}</Mono>
              </PropertyRow>
              <PropertyRow label={t('skills.new.form.description')}>
                <span className="whitespace-pre-wrap">{record.description_human || '—'}</span>
              </PropertyRow>
              <PropertyRow label={t('skills.new.form.timeSaved')}>
                {formatTimeSaved(intl, record.time_saved_value, record.time_saved_unit)}
              </PropertyRow>
              {record.tags && (
                <PropertyRow label={t('skills.new.form.tags')}>
                  <span className="flex flex-wrap gap-1">
                    {splitTags(record.tags).map((tg) => (
                      <Badge key={tg} tone="neutral">{tg}</Badge>
                    ))}
                  </span>
                </PropertyRow>
              )}
            </PropertySection>
          </Card>
        )}

        {/* Cumulative time saved — now a real figure: usage_count × per-use
            estimate (or months-since-approval × per-month estimate), computed by
            the backend. Still labeled as the user's own estimate basis. */}
        <Section title={t('skills.custom.savings')}>
          <Card>
            <p className="text-2xl font-semibold tabular-nums text-stone-800 dark:text-stone-100">
              {intl.formatMessage(
                { id: 'skills.custom.savings.hoursValue', defaultMessage: '{value} h' },
                { value: (record.saved_hours_estimate ?? 0).toFixed(1) },
              )}
            </p>
            <p className="mt-1 text-xs text-stone-500 dark:text-stone-400">{t('skills.custom.savings.estimateNote')}</p>
            <p className="mt-2 text-xs text-stone-400 dark:text-stone-500">
              {intl.formatMessage(
                { id: 'skills.custom.savings.usedTimes', defaultMessage: 'Used {count} times' },
                { count: record.usage_count ?? 0 },
              )}
            </p>
          </Card>
        </Section>

        {/* Metadata + approval history */}
        <Card>
          <PropertySection title={t('skills.custom.history')}>
            <PropertyRow label={t('skills.custom.builtBy')}>
              {record.built_by_agent ? (
                <span className="flex items-center gap-1.5">
                  <CharacterAvatar agentId={record.built_by_agent} name={record.built_by_agent} size={18} />
                  <span className="truncate">{record.built_by_agent}</span>
                </span>
              ) : (
                '—'
              )}
            </PropertyRow>
            <PropertyRow label={t('skills.custom.createdBy')}>{record.created_by_user || '—'}</PropertyRow>
            <PropertyRow label={t('skills.custom.created')}>{timeAgo(record.created_at)}</PropertyRow>
            {record.approval_id && (
              <PropertyRow label={t('skills.custom.approvalId')}>
                <Mono>{record.approval_id}</Mono>
              </PropertyRow>
            )}
            {record.approved_at && (
              <PropertyRow label={t('skills.custom.approvedAt')}>
                <span className="flex items-center gap-1 text-emerald-600 dark:text-emerald-400">
                  <CheckCircle2 className="h-3.5 w-3.5" />
                  {timeAgo(record.approved_at)}
                </span>
              </PropertyRow>
            )}
            {record.rejection_reason && (
              <PropertyRow label={t('skills.custom.rejectionReason')}>{record.rejection_reason}</PropertyRow>
            )}
          </PropertySection>
        </Card>

        {/* Fail-closed submit error */}
        {submitError && (
          <div className="flex items-start gap-2 rounded-lg border border-rose-200 bg-rose-50 p-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-400">
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
            <p className="break-words text-xs">{submitError}</p>
          </div>
        )}

        {/* Actions */}
        <div className="flex items-center gap-2 border-t border-[var(--panel-border)] pt-4">
          {canSubmit && (
            <Button
              variant="primary"
              icon={submitting ? Loader2 : Send}
              onClick={handleSubmit}
              disabled={submitting}
              className={cn(submitting && '[&>svg]:animate-spin')}
            >
              {submitting
                ? t('skills.new.review.submitting')
                : record.status === 'rejected'
                  ? t('skills.custom.resubmit')
                  : t('skills.custom.submit')}
            </Button>
          )}
          {canRetire && (
            <Button
              variant="ghost"
              icon={retiring ? Loader2 : Archive}
              onClick={handleRetire}
              disabled={retiring}
              className={cn(retiring && '[&>svg]:animate-spin')}
            >
              {t('skills.custom.retire')}
            </Button>
          )}
        </div>
      </div>
    </Page>
  );
}
