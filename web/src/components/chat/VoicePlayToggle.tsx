import { useIntl } from 'react-intl';
import { Volume2, VolumeX } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useChatStore } from '@/stores/chat-store';

/**
 * Reply-playback toggle for the chat header (openhuman-parity B-P2). When on,
 * each completed assistant reply is spoken via `POST /api/tts` (the actual
 * playback lives in `WebChatPage`, which watches for finished replies). The
 * preference is persisted in `localStorage` by the store.
 */
export function VoicePlayToggle({ className }: { className?: string }) {
  const intl = useIntl();
  const ttsEnabled = useChatStore((s) => s.ttsEnabled);
  const setTtsEnabled = useChatStore((s) => s.setTtsEnabled);

  const label = ttsEnabled
    ? intl.formatMessage({ id: 'voice.play.on', defaultMessage: '語音朗讀：開' })
    : intl.formatMessage({ id: 'voice.play.off', defaultMessage: '語音朗讀：關' });

  return (
    <button
      type="button"
      onClick={() => setTtsEnabled(!ttsEnabled)}
      aria-label={label}
      aria-pressed={ttsEnabled}
      title={label}
      className={cn(
        'grid h-9 w-9 shrink-0 place-items-center rounded-lg transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
        ttsEnabled
          ? 'text-amber-600 hover:bg-amber-500/10 dark:text-amber-400'
          : 'text-stone-400 hover:bg-stone-500/10 hover:text-stone-600 dark:text-stone-500 dark:hover:text-stone-300',
        className,
      )}
    >
      {ttsEnabled ? <Volume2 className="h-5 w-5" /> : <VolumeX className="h-5 w-5" />}
    </button>
  );
}
