import { useCallback, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type ExpertDraftPreview } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  Button,
  Badge,
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
import { Sparkles, Users, Puzzle, BookOpen, RefreshCw } from 'lucide-react';
import { AttachUnderSelect } from './AttachUnderSelect';
import { ThinkingOrbIndicator } from '@/components/chat/ThinkingOrbIndicator';

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

type Step = 'form' | 'generating' | 'preview' | 'installing';

/**
 * GenerateExpertDialog — the LLM-guided "build your own expert pack" flow:
 * ① industry + description + team size + channels → ② generate (cancelable)
 * → ③ preview (roster / skills / SOPs) + feedback loop (max 5 rounds)
 * → ④ install through the full security-scanned pipeline.
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
  const [draftId, setDraftId] = useState<string | null>(null);
  const [preview, setPreview] = useState<ExpertDraftPreview | null>(null);
  const [roundsLeft, setRoundsLeft] = useState(0);
  const [feedback, setFeedback] = useState('');
  const [revising, setRevising] = useState(false);
  // WP-ORG — org placement for the installed pack root (empty = independent).
  const [attachUnder, setAttachUnder] = useState('');
  /** Bumped on cancel so an in-flight generation's late result is ignored. */
  const generationToken = useRef(0);

  const reset = () => {
    setStep('form');
    setDraftId(null);
    setPreview(null);
    setFeedback('');
    setRevising(false);
    setAttachUnder('');
  };

  const close = (next: boolean, eventDetails?: { reason?: string }) => {
    // Never lose the flow to an accidental dismissal: outside presses are
    // disabled at the Dialog level (`disablePointerDismissal`), and Escape is
    // ignored while a generation/installation is running or a draft preview
    // is on screen. Closing then requires an explicit control (X / 取消).
    if (
      !next &&
      eventDetails?.reason === 'escape-key' &&
      step !== 'form'
    ) {
      return;
    }
    if (!next) {
      generationToken.current += 1; // drop any in-flight result
      reset();
    }
    onOpenChange(next);
  };

  const toggleChannel = (value: string) => {
    setChannels((prev) =>
      prev.includes(value) ? prev.filter((c) => c !== value) : [...prev, value],
    );
  };

  const handleGenerate = async () => {
    if (!description.trim()) {
      toast.error(t('experts.generate.needDescription'));
      return;
    }
    const token = ++generationToken.current;
    setStep('generating');
    try {
      const res = await api.experts.generate({
        industry_hint: industryHint.trim(),
        description: description.trim(),
        team_size: teamSize,
        channels,
      });
      if (generationToken.current !== token) return; // cancelled
      setDraftId(res.draft_id);
      setPreview(res.preview);
      setRoundsLeft(res.rounds_left);
      setFeedback('');
      setStep('preview');
    } catch (e) {
      if (generationToken.current !== token) return;
      toast.error(intl.formatMessage({ id: 'experts.generate.failed' }, { message: formatError(e) }));
      setStep('form');
    }
  };

  const handleRevise = async () => {
    if (!draftId) return;
    if (!feedback.trim()) {
      toast.error(t('experts.generate.needFeedback'));
      return;
    }
    const token = ++generationToken.current;
    setRevising(true);
    try {
      const res = await api.experts.generateRevise(draftId, feedback.trim());
      if (generationToken.current !== token) return;
      setPreview(res.preview);
      setRoundsLeft(res.rounds_left);
      setFeedback('');
    } catch (e) {
      if (generationToken.current !== token) return;
      toast.error(intl.formatMessage({ id: 'experts.generate.failed' }, { message: formatError(e) }));
    } finally {
      if (generationToken.current === token) setRevising(false);
    }
  };

  const handleInstall = async () => {
    if (!draftId) return;
    setStep('installing');
    try {
      await api.experts.installDraft(draftId, attachUnder || undefined);
      toast.success(t('experts.install.success'));
      reset();
      onOpenChange(false);
      onInstalled();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'experts.install.failed' }, { message: formatError(e) }));
      setStep('preview');
    }
  };

  const cancelGeneration = () => {
    generationToken.current += 1;
    setStep(draftId ? 'preview' : 'form');
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
                {CHANNEL_OPTIONS.map((ch) => (
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
          </div>
        )}

        {(step === 'generating' || step === 'installing') && (
          <div className="flex flex-col items-center gap-3 py-10">
            {step === 'generating' ? (
              // AI is generating the expert team spec — big center-stage
              // "thinking" moment; `decorative` since the paragraph below
              // already announces the same state as text.
              <ThinkingOrbIndicator state="waiting" decorative />
            ) : (
              <Spinner className="size-6" />
            )}
            <p className="text-sm text-muted-foreground">
              {step === 'generating'
                ? t('experts.generate.generating')
                : t('experts.installing')}
            </p>
            {step === 'generating' && (
              <Button variant="ghost" size="sm" onClick={cancelGeneration}>
                {t('common.cancel')}
              </Button>
            )}
          </div>
        )}

        {step === 'preview' && preview && (
          <div className="space-y-4">
            <div className="rounded-lg border border-surface-border bg-muted/40 p-3">
              <div className="flex items-center gap-2">
                <h3 className="text-sm font-medium">{preview.display_name || preview.slug}</h3>
                <span className="font-mono text-xs text-muted-foreground">{preview.slug}</span>
              </div>
              {preview.description && (
                <p className="mt-1 text-xs text-muted-foreground">{preview.description}</p>
              )}
              <div className="mt-2 flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
                <span className="inline-flex items-center gap-1">
                  <Users className="size-3.5" />
                  {intl.formatMessage({ id: 'experts.card.agents' }, { count: preview.agents.length })}
                </span>
                <span className="inline-flex items-center gap-1">
                  <Puzzle className="size-3.5" />
                  {intl.formatMessage({ id: 'experts.card.skills' }, { count: preview.skills.length })}
                </span>
                <span className="inline-flex items-center gap-1">
                  <BookOpen className="size-3.5" />
                  {intl.formatMessage({ id: 'experts.card.wiki' }, { count: preview.wiki_titles.length })}
                </span>
              </div>
            </div>

            <div className="space-y-2">
              <span className="text-sm font-medium">{t('experts.generate.roster')}</span>
              <ul className="space-y-2">
                {preview.agents.map((a) => (
                  <li key={a.name} className="rounded-lg border border-surface-border p-2.5">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium">{a.display_name || a.name}</span>
                      <Badge variant={a.role === 'front_desk' ? 'secondary' : 'ghost'}>
                        {a.role === 'front_desk'
                          ? t('experts.generate.roleFrontDesk')
                          : t('experts.generate.roleWorker')}
                      </Badge>
                    </div>
                    {a.soul_excerpt && (
                      <p className="mt-1 line-clamp-2 text-xs text-muted-foreground">
                        {a.soul_excerpt}
                      </p>
                    )}
                  </li>
                ))}
              </ul>
            </div>

            {(preview.skills.length > 0 || preview.wiki_titles.length > 0) && (
              <div className="flex flex-wrap gap-1.5 text-xs">
                {preview.skills.map((s) => (
                  <Badge key={`s-${s}`} variant="secondary">
                    <Puzzle />
                    {s}
                  </Badge>
                ))}
                {preview.wiki_titles.map((w) => (
                  <Badge key={`w-${w}`} variant="ghost">
                    <BookOpen />
                    {w}
                  </Badge>
                ))}
              </div>
            )}

            <AttachUnderSelect value={attachUnder} onChange={setAttachUnder} disabled={revising} />

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
                  disabled={revising || roundsLeft <= 0 || !feedback.trim()}
                  onClick={handleRevise}
                >
                  {revising ? <Spinner className="size-3.5" /> : <RefreshCw />}
                  {t('experts.generate.revise')}
                </Button>
                <span className="text-xs text-muted-foreground">
                  {roundsLeft > 0
                    ? intl.formatMessage({ id: 'experts.generate.roundsLeft' }, { count: roundsLeft })
                    : t('experts.generate.roundsExhausted')}
                </span>
              </div>
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
              <Button variant="outline" size="sm" onClick={() => close(false)} disabled={revising}>
                {t('common.cancel')}
              </Button>
              <Button variant="brand" size="sm" onClick={handleInstall} disabled={revising}>
                {t('experts.generate.install')}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
