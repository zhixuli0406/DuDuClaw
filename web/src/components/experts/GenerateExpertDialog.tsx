import { useProductPolicy } from '@/lib/product-policy';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  api,
  type ExpertDraftPreview,
  type ExpertDraftStatus,
  type ExpertGenerationJob,
  type ExpertInstallPlacement,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  Button,
  Input,
  Textarea,
  Checkbox,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Spinner,
} from '@/components/mds';
import { Sparkles, RefreshCw } from 'lucide-react';
import { AttachUnderSelect } from './AttachUnderSelect';
import { ThinkingOrbIndicator } from '@/components/chat/ThinkingOrbIndicator';
import { GenerationProgress } from './GenerationProgress';
import { DraftPreview } from './DraftPreview';
import { InstallResult } from './InstallResult';

/** Channels a generated pack may suggest (mirrors the gateway allowlist). */
const CHANNEL_OPTIONS = [
  { value: 'line', label: 'LINE' },
  { value: 'telegram', label: 'Telegram' },
  { value: 'discord', label: 'Discord' },
  { value: 'slack', label: 'Slack' },
  { value: 'whatsapp', label: 'WhatsApp' },
  { value: 'feishu', label: 'Feishu' },
  { value: 'googlechat', label: 'Google Chat' },
  { value: 'teams', label: 'Teams' },
  { value: 'webchat', label: 'WebChat' },
];

const TEAM_SIZES = [1, 2, 3, 4, 5, 6, 7, 8];

/** How often the dialog polls a running generation job. */
export const POLL_INTERVAL_MS = 2000;
/** Consecutive status-poll failures tolerated before giving up. */
const MAX_POLL_ERRORS = 3;

type Step = 'form' | 'generating' | 'preview' | 'installing' | 'installed';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * GenerateExpertDialog — the LLM-guided "build your own AI team" flow:
 * ① industry + description + team size + channels → ② staged background
 * generation with per-stage progress (cancelable) → ③ preview (members with
 * their skills and red lines, knowledge-base pages, predicted install skips)
 * + feedback loop (max 5 rounds) → ④ install through the full
 * security-scanned pipeline, then a summary of anything that was not placed.
 */
export function GenerateExpertDialog({
  open,
  onOpenChange,
  onInstalled,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onInstalled: () => void;
}) {
  const policy = useProductPolicy();
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );

  const [step, setStep] = useState<Step>('form');
  const [industryHint, setIndustryHint] = useState('');
  const [description, setDescription] = useState('');
  const [teamSize, setTeamSize] = useState(3);
  const [channels, setChannels] = useState<string[]>(['line']);
  const [research, setResearch] = useState(false);
  const [draftId, setDraftId] = useState<string | null>(null);
  const [preview, setPreview] = useState<ExpertDraftPreview | null>(null);
  const [job, setJob] = useState<ExpertGenerationJob | null>(null);
  const [roundsLeft, setRoundsLeft] = useState(0);
  const [feedback, setFeedback] = useState('');
  const [placement, setPlacement] = useState<ExpertInstallPlacement | null>(null);
  /** The admin confirmed reading the pages written with web search. */
  const [reviewedWeb, setReviewedWeb] = useState(false);
  // WP-ORG — org placement for the installed pack root (empty = independent).
  const [attachUnder, setAttachUnder] = useState('');
  /** Bumped on cancel/close so an in-flight poll loop stops and its late
   *  result is ignored. */
  const generationToken = useRef(0);
  /** Draft whose job is running right now (for cancel-on-close). */
  const runningDraft = useRef<string | null>(null);

  useEffect(
    () => () => {
      generationToken.current += 1;
      // Leaving the page must not leave a model job running unseen.
      const id = runningDraft.current;
      runningDraft.current = null;
      if (id) api.experts.generateCancel(id).catch(() => undefined);
    },
    [],
  );

  const reset = () => {
    setStep('form');
    setDraftId(null);
    setPreview(null);
    setJob(null);
    setFeedback('');
    setPlacement(null);
    setReviewedWeb(false);
    setAttachUnder('');
  };

  const stopRunningJob = () => {
    const id = runningDraft.current;
    runningDraft.current = null;
    if (id) {
      api.experts.generateCancel(id).catch(() => {
        // Best effort: the gateway also stops a job at its own deadline.
      });
    }
  };

  const close = (next: boolean, eventDetails?: { reason?: string }) => {
    // Never lose the flow to an accidental dismissal: outside presses are
    // disabled at the Dialog level (`disablePointerDismissal`), and Escape is
    // ignored while a generation/installation is running or a draft preview
    // is on screen. Closing then requires an explicit control (X / 取消).
    if (!next && eventDetails?.reason === 'escape-key' && step !== 'form') {
      return;
    }
    if (!next) {
      generationToken.current += 1; // stop polling, drop any in-flight result
      stopRunningJob();
      reset();
    }
    onOpenChange(next);
  };

  const toggleChannel = (value: string) => {
    setChannels((prev) =>
      prev.includes(value) ? prev.filter((c) => c !== value) : [...prev, value],
    );
  };

  /** Poll a draft's job until it finishes; returns the terminal status, or
   *  null when the dialog cancelled the wait. */
  const waitForJob = async (id: string, token: number): Promise<ExpertDraftStatus | null> => {
    let errors = 0;
    for (;;) {
      await sleep(POLL_INTERVAL_MS);
      if (generationToken.current !== token) return null;
      let res: ExpertDraftStatus;
      try {
        res = await api.experts.generateStatus(id);
        errors = 0;
      } catch (e) {
        // A transient disconnect should not abandon a running job.
        errors += 1;
        if (errors >= MAX_POLL_ERRORS) throw e;
        continue;
      }
      if (generationToken.current !== token) return null;
      setJob(res.job ?? null);
      if (res.status !== 'running') return res;
    }
  };

  /** Shared tail of generate + revise: wait, then show the outcome. */
  const runJob = async (start: () => Promise<ExpertDraftStatus>, backTo: Step) => {
    const token = ++generationToken.current;
    setJob(null);
    setStep('generating');
    try {
      const started = await start();
      if (generationToken.current !== token) {
        // Cancelled or closed while the start call was in flight: the job
        // exists on the gateway now, so stop it instead of orphaning it.
        api.experts.generateCancel(started.draft_id).catch(() => undefined);
        return;
      }
      setDraftId(started.draft_id);
      setJob(started.job ?? null);
      runningDraft.current = started.draft_id;
      const res = await waitForJob(started.draft_id, token);
      if (!res) return; // cancelled or closed
      runningDraft.current = null;
      if ((res.status === 'done' || res.status === 'partial') && res.preview) {
        setPreview(res.preview);
        setReviewedWeb(false);
        setRoundsLeft(res.rounds_left ?? 0);
        setFeedback('');
        setStep('preview');
        if (res.status === 'partial') toast.info(t('experts.generate.partialToast'));
        return;
      }
      const message =
        res.status === 'cancelled' ? t('experts.generate.cancelled') : res.job?.error || '';
      toast.error(intl.formatMessage({ id: 'experts.generate.failed' }, { message }));
      setStep(backTo);
    } catch (e) {
      if (generationToken.current !== token) return;
      stopRunningJob(); // we can no longer follow it, so stop it
      toast.error(intl.formatMessage({ id: 'experts.generate.failed' }, { message: formatError(e) }));
      setStep(backTo);
    }
  };

  const handleGenerate = async () => {
    if (!description.trim()) {
      toast.error(t('experts.generate.needDescription'));
      return;
    }
    await runJob(
      () =>
        api.experts.generate({
          industry_hint: industryHint.trim(),
          description: description.trim(),
          team_size: teamSize,
          channels,
          research,
        }),
      'form',
    );
  };

  const handleRevise = async () => {
    if (!draftId) return;
    if (!feedback.trim()) {
      toast.error(t('experts.generate.needFeedback'));
      return;
    }
    await runJob(() => api.experts.generateRevise(draftId, feedback.trim()), 'preview');
  };

  const handleInstall = async () => {
    if (!draftId) return;
    setStep('installing');
    try {
      const res = await api.experts.installDraft(draftId, attachUnder || undefined, reviewedWeb);
      onInstalled();
      if (
        res.placement &&
        (!res.placement.complete || res.placement.agents.some((a) => a.renamed))
      ) {
        setPlacement(res.placement);
        setStep('installed');
        return;
      }
      toast.success(t('experts.install.success'));
      reset();
      onOpenChange(false);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'experts.install.failed' }, { message: formatError(e) }));
      setStep('preview');
    }
  };

  const needsWebReview = preview?.wiki_pages?.some((p) => p.web_research) ?? false;
  // Every revise rewrites the whole team: roster + one call per member +
  // knowledge base (retries can add up to the same again).
  const reviseCalls = (preview?.agents.length ?? 0) + 2;

  const cancelGeneration = () => {
    generationToken.current += 1;
    stopRunningJob();
    setStep(preview ? 'preview' : 'form');
  };

  return (
    <Dialog open={open} onOpenChange={close} disablePointerDismissal>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Sparkles className="size-4 text-brand" />
            {t('experts.generate.title')}
          </DialogTitle>
          <DialogDescription>{t('experts.generate.desc')}</DialogDescription>
        </DialogHeader>

        {step === 'form' && (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <label className="text-sm font-medium" htmlFor="expert-gen-industry">
                {t('experts.generate.industry')}
              </label>
              <Input
                id="expert-gen-industry"
                value={industryHint}
                maxLength={100}
                placeholder={t('experts.generate.industryPh')}
                onChange={(e) => setIndustryHint(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-sm font-medium" htmlFor="expert-gen-desc">
                {t('experts.generate.description')}
              </label>
              <Textarea
                id="expert-gen-desc"
                value={description}
                rows={4}
                maxLength={2000}
                placeholder={t('experts.generate.descriptionPh')}
                onChange={(e) => setDescription(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-sm font-medium" htmlFor="expert-gen-size">
                {t('experts.generate.teamSize')}
              </label>
              <Select value={String(teamSize)} onValueChange={(v) => setTeamSize(Number(v))}>
                <SelectTrigger id="expert-gen-size" className="w-full">
                  <SelectValue>
                    {intl.formatMessage({ id: 'experts.generate.teamSizeValue' }, { count: teamSize })}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {TEAM_SIZES.map((n) => (
                    <SelectItem key={n} value={String(n)}>
                      {intl.formatMessage({ id: 'experts.generate.teamSizeValue' }, { count: n })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <span className="text-sm font-medium">{t('experts.generate.channels')}</span>
              <div className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3">
                {CHANNEL_OPTIONS.filter((ch) => policy.allows('channels', ch.value)).map((ch) => (
                  <label
                    key={ch.value}
                    className="flex items-center gap-2 text-sm text-muted-foreground"
                  >
                    <Checkbox
                      checked={channels.includes(ch.value)}
                      onCheckedChange={() => toggleChannel(ch.value)}
                    />
                    {ch.label}
                  </label>
                ))}
              </div>
            </div>
            <label className="flex items-start gap-2 text-sm">
              <Checkbox
                checked={research}
                onCheckedChange={(v) => setResearch(v === true)}
                className="mt-0.5"
              />
              <span>
                {t('experts.generate.research')}
                <span className="block text-xs text-muted-foreground">
                  {t('experts.generate.researchHint')}
                </span>
              </span>
            </label>
          </div>
        )}

        {step === 'generating' && (
          <div className="flex flex-col items-center gap-4 py-6">
            {/* `decorative`: the progress list below announces the state. */}
            <ThinkingOrbIndicator state="waiting" decorative />
            <p className="text-sm text-muted-foreground">{t('experts.generate.generating')}</p>
            <GenerationProgress job={job} />
            <Button variant="ghost" size="sm" onClick={cancelGeneration}>
              {t('common.cancel')}
            </Button>
          </div>
        )}

        {step === 'installing' && (
          <div className="flex flex-col items-center gap-3 py-10">
            <Spinner className="size-6" />
            <p className="text-sm text-muted-foreground">{t('experts.installing')}</p>
          </div>
        )}

        {step === 'installed' && placement && (
          <InstallResult placement={placement} preview={preview} />
        )}

        {step === 'preview' && preview && (
          <div className="space-y-4">
            <DraftPreview preview={preview} />

            {needsWebReview && (
              <label className="flex items-start gap-2 text-sm">
                <Checkbox
                  checked={reviewedWeb}
                  onCheckedChange={(v) => setReviewedWeb(v === true)}
                  className="mt-0.5"
                />
                <span>{t('experts.generate.webPagesConfirm')}</span>
              </label>
            )}

            <AttachUnderSelect value={attachUnder} onChange={setAttachUnder} />

            <div className="space-y-1.5">
              <label className="text-sm font-medium" htmlFor="expert-gen-feedback">
                {t('experts.generate.feedback')}
              </label>
              <Textarea
                id="expert-gen-feedback"
                value={feedback}
                rows={2}
                maxLength={2000}
                placeholder={t('experts.generate.feedbackPh')}
                onChange={(e) => setFeedback(e.target.value)}
              />
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={roundsLeft <= 0 || !feedback.trim()}
                  onClick={handleRevise}
                >
                  <RefreshCw />
                  {t('experts.generate.revise')}
                </Button>
                <span className="text-xs text-muted-foreground">
                  {roundsLeft > 0
                    ? intl.formatMessage({ id: 'experts.generate.roundsLeft' }, { count: roundsLeft })
                    : t('experts.generate.roundsExhausted')}
                </span>
              </div>
              <p className="text-xs text-muted-foreground">
                {t('experts.generate.reviseCost', { count: reviseCalls })}
              </p>
            </div>
          </div>
        )}

        <DialogFooter>
          {step === 'form' && (
            <>
              <Button variant="outline" size="sm" onClick={() => close(false)}>
                {t('common.cancel')}
              </Button>
              <Button variant="brand" size="sm" disabled={!description.trim()} onClick={handleGenerate}>
                <Sparkles />
                {t('experts.generate.start')}
              </Button>
            </>
          )}
          {step === 'preview' && (
            <>
              <Button variant="outline" size="sm" onClick={() => close(false)}>
                {t('common.cancel')}
              </Button>
              <Button
                variant="brand"
                size="sm"
                onClick={handleInstall}
                disabled={needsWebReview && !reviewedWeb}
              >
                {t('experts.generate.install')}
              </Button>
            </>
          )}
          {step === 'installed' && (
            <Button variant="brand" size="sm" onClick={() => close(false)}>
              {t('experts.install.result.done')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
