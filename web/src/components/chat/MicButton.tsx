import { useIntl } from 'react-intl';
import { Mic, Loader2 } from 'lucide-react';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { useChatStore } from '@/stores/chat-store';
import { useVoiceRecorder } from './useVoiceRecorder';
import { isCapturing, isTranscribing } from './voice-recorder';

/**
 * Push-to-talk microphone button (openhuman-parity B-P2). Hold to record,
 * release to transcribe; the recognized text is handed to `onTranscript` (the
 * composer fills it — we never auto-send). When the browser can't capture audio
 * the button is disabled with an explanatory tooltip.
 */
export function MicButton({
  onTranscript,
  disabled = false,
  className,
}: {
  onTranscript: (text: string) => void;
  disabled?: boolean;
  className?: string;
}) {
  const intl = useIntl();
  const setTtsEnabled = useChatStore((s) => s.setTtsEnabled);

  const { state, supported, start, stop } = useVoiceRecorder({
    onTranscript,
    onNotConfigured: (msg) => {
      // Server says STT isn't configured — surface it and don't keep trying.
      toast.error(
        msg ||
          intl.formatMessage({
            id: 'voice.stt.notConfigured',
            defaultMessage: '尚未設定語音轉文字，請至設定 → 語音',
          }),
      );
      // Turn any reply-playback off too, since voice is unconfigured.
      setTtsEnabled(false);
    },
    onError: (msg) => {
      if (msg === 'unsupported') return; // button is already disabled
      toast.error(
        intl.formatMessage(
          { id: 'voice.stt.failed', defaultMessage: '語音辨識失敗：{message}' },
          { message: msg },
        ),
      );
    },
  });

  const capturing = isCapturing(state);
  const busy = isTranscribing(state);
  const isDisabled = disabled || !supported || busy;

  const label = !supported
    ? intl.formatMessage({
        id: 'voice.mic.unsupported',
        defaultMessage: '此瀏覽器不支援麥克風錄音',
      })
    : capturing
      ? intl.formatMessage({ id: 'voice.mic.release', defaultMessage: '放開以送出語音' })
      : intl.formatMessage({ id: 'voice.mic.hold', defaultMessage: '按住說話' });

  // Release should always stop, even if the pointer left the button first.
  const handleStop = () => {
    if (capturing) stop();
  };

  return (
    <button
      type="button"
      disabled={isDisabled}
      aria-label={label}
      aria-pressed={capturing}
      title={label}
      onPointerDown={(e) => {
        if (isDisabled) return;
        e.preventDefault();
        e.currentTarget.setPointerCapture?.(e.pointerId);
        void start();
      }}
      onPointerUp={handleStop}
      onPointerCancel={handleStop}
      onPointerLeave={handleStop}
      className={cn(
        'grid h-11 w-11 shrink-0 place-items-center rounded-control transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
        capturing
          ? 'animate-pulse bg-rose-500 text-white'
          : 'text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200',
        isDisabled && 'cursor-not-allowed opacity-50 hover:bg-transparent',
        className,
      )}
    >
      {busy ? <Loader2 className="h-5 w-5 animate-spin" /> : <Mic className="h-5 w-5" />}
    </button>
  );
}
