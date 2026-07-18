import { useState, useEffect, useRef, useCallback, type ReactNode } from 'react';
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
  BreadcrumbHeader,
  CollectionPageState,
  Card,
  CardContent,
  Button,
  Badge,
  Input,
  Textarea,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  ActorAvatar,
  type BreadcrumbSegment,
} from '@/components/mds';
import { DuDu } from '@/components/mascot';
import { celebrate } from '@/components/ui/CelebrationLayer';
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
import { statusMeta, statusToneBadge, formatTimeSaved } from './status-meta';

const TIME_UNITS: TimeSavedUnit[] = ['minutes_per_use', 'hours_per_month'];
const POLL_MS = 4000;

function splitTags(tags: string): string[] {
  return tags.split(',').map((s) => s.trim()).filter(Boolean);
}

/** Local field wrapper (label + optional help), MDS-styled. */
function Field({
  label,
  help,
  required,
  children,
}: {
  label: ReactNode;
  help?: ReactNode;
  required?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="flex items-center gap-1 text-xs font-medium text-muted-foreground">
        {label}
        {required && <span className="text-destructive">*</span>}
      </label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

/** Local property row (label left, value right), MDS-styled (spec §5.3). */
function PropertyRow({ label, children }: { label: ReactNode; children: ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-4 py-2.5 first:pt-0 last:pb-0">
      <span className="shrink-0 text-sm text-muted-foreground">{label}</span>
      <span className="min-w-0 text-right text-sm text-foreground">{children}</span>
    </div>
  );
}

function SectionTitle({ children }: { children: ReactNode }) {
  return <h2 className="text-sm font-medium text-foreground">{children}</h2>;
}

/**
 * CustomSkillDetail — `/skills/custom/:id` (T13.2), re-skinned onto MDS (spec
 * §5.3 detail-page shell: BreadcrumbHeader + max-w-4xl container). All fields +
 * status + approval history + status-appropriate actions. Draft/rejected are
 * editable and (re)submittable; pending shows a waiting state; rejected shows
 * the reason. Polls for a transition to `approved` to fire the on-launch
 * celebration (T13.4).
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

  const segments = useCallback(
    (leaf: ReactNode): BreadcrumbSegment[] => [
      { label: t('nav.skills'), onClick: () => navigate('/skills') },
      { label: leaf },
    ],
    [navigate, t],
  );

  if (loading) {
    return (
      <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
        <BreadcrumbHeader segments={segments(t('skills.custom.title'))} />
        <CollectionPageState state="loading" />
      </div>
    );
  }

  if (notFound || !record) {
    return (
      <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
        <BreadcrumbHeader segments={segments(t('skills.custom.title'))} />
        <CollectionPageState
          state="empty"
          icon={Puzzle}
          title={t('skills.custom.notFound')}
          action={
            <Button variant="outline" size="sm" onClick={() => navigate('/skills')}>
              {t('skills.custom.backToList')}
            </Button>
          }
        />
      </div>
    );
  }

  const meta = statusMeta(record.status);
  const badge = statusToneBadge(meta.tone);
  const isApproved = record.status === 'approved';

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <BreadcrumbHeader
        segments={segments(record.display_name)}
        actions={
          <Badge variant={badge.variant} className={badge.className}>
            {t(meta.labelKey)}
          </Badge>
        }
      />

      <div className="mx-auto w-full max-w-4xl space-y-6 px-5 py-6 md:px-8 md:py-8">
        {/* Approved banner + DuDu (T13.4) */}
        {isApproved && (
          <Card>
            <CardContent className="flex items-center gap-4">
              <DuDu face="proud" size="sm" />
              <div>
                <p className="text-sm font-medium text-success">{t('skills.custom.liveTitle')}</p>
                <p className="text-xs text-muted-foreground">{t('skills.custom.liveHint')}</p>
              </div>
            </CardContent>
          </Card>
        )}

        {/* Rejection reason (T13.2) */}
        {record.status === 'rejected' && record.rejection_reason && (
          <div className="flex items-start gap-2 rounded-xl bg-destructive/10 p-4 text-sm text-destructive">
            <XCircle className="mt-0.5 size-4 shrink-0" />
            <div>
              <p className="font-medium">{t('skills.custom.rejectedTitle')}</p>
              <p className="mt-0.5 text-xs">{record.rejection_reason}</p>
            </div>
          </div>
        )}

        {/* Pending waiting state */}
        {record.status === 'pending_approval' && (
          <div className="flex items-center gap-3 rounded-xl bg-warning/10 p-4 text-sm text-warning">
            <Clock className="size-4 shrink-0" />
            {t('skills.custom.waiting')}
          </div>
        )}

        {/* Generating */}
        {record.status === 'generating' && (
          <div className="flex items-center gap-3 rounded-xl bg-info/10 p-4 text-sm text-info">
            <Loader2 className="size-4 shrink-0 animate-spin" />
            {t('skills.custom.generatingHint')}
          </div>
        )}

        {/* Human fields — editable when draft/rejected, else read-only */}
        {editable ? (
          <section className="space-y-3">
            <SectionTitle>{t('skills.custom.fields')}</SectionTitle>
            <Card>
              <CardContent className="space-y-4">
                <Field label={t('skills.new.form.displayName')} required>
                  <Input value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
                </Field>
                <Field label={t('skills.new.form.description')}>
                  <Textarea value={descriptionHuman} onChange={(e) => setDescriptionHuman(e.target.value)} rows={3} />
                </Field>
                <Field label={t('skills.new.form.timeSaved')} help={t('skills.new.form.timeSavedHelp')}>
                  <div className="flex gap-2">
                    <Input
                      type="number"
                      min={0}
                      className="w-28"
                      value={timeSavedValue}
                      onChange={(e) => setTimeSavedValue(e.target.value)}
                    />
                    <Select value={timeSavedUnit} onValueChange={(v) => setTimeSavedUnit(String(v) as TimeSavedUnit)}>
                      <SelectTrigger className="w-48">
                        <SelectValue>{t(`skills.custom.unit.${timeSavedUnit}`)}</SelectValue>
                      </SelectTrigger>
                      <SelectContent>
                        {TIME_UNITS.map((u) => (
                          <SelectItem key={u} value={u}>
                            {t(`skills.custom.unit.${u}`)}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                </Field>
                <Field label={t('skills.new.form.tags')}>
                  <ChipEditor values={tags} onChange={setTags} />
                </Field>
                <div>
                  <Button variant="outline" onClick={handleSave} disabled={saving || !displayName.trim()}>
                    {saving ? <Loader2 className="animate-spin" /> : <Save />}
                    {saving ? t('common.saving') : t('common.save')}
                  </Button>
                </div>
              </CardContent>
            </Card>
          </section>
        ) : (
          <section className="space-y-3">
            <SectionTitle>{t('skills.custom.fields')}</SectionTitle>
            <Card>
              <CardContent className="divide-y divide-surface-border">
                <PropertyRow label={t('skills.new.form.displayName')}>{record.display_name}</PropertyRow>
                <PropertyRow label={t('skills.custom.slug')}>
                  <span className="font-mono">{record.slug}</span>
                </PropertyRow>
                <PropertyRow label={t('skills.new.form.description')}>
                  <span className="whitespace-pre-wrap">{record.description_human || '—'}</span>
                </PropertyRow>
                <PropertyRow label={t('skills.new.form.timeSaved')}>
                  {formatTimeSaved(intl, record.time_saved_value, record.time_saved_unit)}
                </PropertyRow>
                {record.tags && (
                  <PropertyRow label={t('skills.new.form.tags')}>
                    <span className="flex flex-wrap justify-end gap-1">
                      {splitTags(record.tags).map((tg) => (
                        <Badge key={tg} variant="secondary">
                          {tg}
                        </Badge>
                      ))}
                    </span>
                  </PropertyRow>
                )}
              </CardContent>
            </Card>
          </section>
        )}

        {/* Cumulative time saved — real figure computed by the backend, still
            labeled as the user's own estimate basis. */}
        <section className="space-y-3">
          <SectionTitle>{t('skills.custom.savings')}</SectionTitle>
          <Card>
            <CardContent>
              <p className="text-2xl font-semibold tabular-nums text-foreground">
                {intl.formatMessage(
                  { id: 'skills.custom.savings.hoursValue', defaultMessage: '{value} h' },
                  { value: (record.saved_hours_estimate ?? 0).toFixed(1) },
                )}
              </p>
              <p className="mt-1 text-xs text-muted-foreground">{t('skills.custom.savings.estimateNote')}</p>
              <p className="mt-2 text-xs text-muted-foreground">
                {intl.formatMessage(
                  { id: 'skills.custom.savings.usedTimes', defaultMessage: 'Used {count} times' },
                  { count: record.usage_count ?? 0 },
                )}
              </p>
            </CardContent>
          </Card>
        </section>

        {/* Metadata + approval history */}
        <section className="space-y-3">
          <SectionTitle>{t('skills.custom.history')}</SectionTitle>
          <Card>
            <CardContent className="divide-y divide-surface-border">
              <PropertyRow label={t('skills.custom.builtBy')}>
                {record.built_by_agent ? (
                  <span className="flex items-center justify-end gap-1.5">
                    <ActorAvatar actorType="agent" size="xs" name={record.built_by_agent} />
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
                  <span className="font-mono">{record.approval_id}</span>
                </PropertyRow>
              )}
              {record.approved_at && (
                <PropertyRow label={t('skills.custom.approvedAt')}>
                  <span className="flex items-center justify-end gap-1 text-success">
                    <CheckCircle2 className="size-3.5" />
                    {timeAgo(record.approved_at)}
                  </span>
                </PropertyRow>
              )}
              {record.rejection_reason && (
                <PropertyRow label={t('skills.custom.rejectionReason')}>{record.rejection_reason}</PropertyRow>
              )}
            </CardContent>
          </Card>
        </section>

        {/* Fail-closed submit error */}
        {submitError && (
          <div className="flex items-start gap-2 rounded-xl bg-destructive/10 p-3 text-sm text-destructive">
            <ShieldAlert className="mt-0.5 size-4 shrink-0" />
            <p className="min-w-0 break-words text-xs">{submitError}</p>
          </div>
        )}

        {/* Actions */}
        <div className="flex items-center gap-2 border-t border-surface-border pt-4">
          {canSubmit && (
            <Button variant="brand" onClick={handleSubmit} disabled={submitting}>
              {submitting ? <Loader2 className="animate-spin" /> : <Send />}
              {submitting
                ? t('skills.new.review.submitting')
                : record.status === 'rejected'
                  ? t('skills.custom.resubmit')
                  : t('skills.custom.submit')}
            </Button>
          )}
          {canRetire && (
            <Button variant="ghost" onClick={handleRetire} disabled={retiring}>
              {retiring ? <Loader2 className={cn('animate-spin')} /> : <Archive />}
              {t('skills.custom.retire')}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
