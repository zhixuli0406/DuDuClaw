import { useIntl } from 'react-intl';
import { AudioLines, Ear, Loader2, Volume2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { TalkModeHandle } from './useTalkMode';
import type { TalkStatus } from './talk-mode';

/**
 * Talk Mode toggle for the composer (G13) — distinct from the push-to-talk
 * `MicButton`: one click enters the continuous voice-conversation loop, one
 * click (or Esc) leaves it. Calm Glass styling: amber accent when engaged, a
 * soft pulse while listening (gated off under `prefers-reduced-motion`).
 */
export function TalkModeButton({
  handle,
  disabled = false,
  className,
}: {
  handle: TalkModeHandle;
  disabled?: boolean;
  className?: string;
}) {
  const intl = useIntl();
  const { active, supported, status, toggle } = handle;
  const isDisabled = disabled || !supported;

  const label = !supported
    ? intl.formatMessage({
        id: 'voice.mic.unsupported',
        defaultMessage: '此瀏覽器不支援麥克風錄音',
      })
    : active
      ? intl.formatMessage({ id: 'voice.talk.stop', defaultMessage: '結束對話模式（Esc）' })
      : intl.formatMessage({ id: 'voice.talk.start', defaultMessage: '開始對話模式' });

  return (
    <button
      type="button"
      disabled={isDisabled}
      aria-label={label}
      aria-pressed={active}
      title={label}
      onClick={() => {
        if (!isDisabled) toggle();
      }}
      className={cn(
        'grid size-8 shrink-0 place-items-center rounded-lg transition-colors focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50',
        active
          ? 'bg-brand text-brand-foreground hover:bg-brand/90'
          : 'text-muted-foreground hover:bg-muted hover:text-foreground',
        active &&
          status === 'listening' &&
          'animate-pulse motion-reduce:animate-none',
        isDisabled && 'cursor-not-allowed opacity-50 hover:bg-transparent',
        className,
      )}
    >
      <AudioLines className="size-4" />
    </button>
  );
}

/**
 * Live loop-state pill shown near the composer while Talk Mode is engaged:
 * listening pulse / transcribing spinner / waiting / speaking. Text is
 * end-user wording (「聆聽中…」style), never internal state names.
 */
export function TalkModeStatusPill({ status, className }: { status: TalkStatus; className?: string }) {
  const intl = useIntl();
  if (status === 'idle') return null;

  const text =
    status === 'listening'
      ? intl.formatMessage({ id: 'voice.talk.listening', defaultMessage: '聆聽中…' })
      : status === 'transcribing'
        ? intl.formatMessage({ id: 'voice.talk.transcribing', defaultMessage: '辨識中…' })
        : status === 'awaiting-reply'
          ? intl.formatMessage({ id: 'voice.talk.waiting', defaultMessage: '等待回覆…' })
          : intl.formatMessage({ id: 'voice.talk.speaking', defaultMessage: '朗讀回覆中…' });

  return (
    <span
      role="status"
      aria-live="polite"
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full border border-brand/30 bg-brand/10 px-2.5 py-0.5 text-xs text-brand',
        className,
      )}
    >
      {status === 'listening' && (
        <Ear className="h-3.5 w-3.5 animate-pulse motion-reduce:animate-none" />
      )}
      {status === 'transcribing' && (
        <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" />
      )}
      {status === 'awaiting-reply' && (
        <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" />
      )}
      {status === 'speaking' && (
        <Volume2 className="h-3.5 w-3.5 animate-pulse motion-reduce:animate-none" />
      )}
      {text}
    </span>
  );
}
