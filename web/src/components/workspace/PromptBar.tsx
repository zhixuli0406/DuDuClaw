import { useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Send, Loader2, Plus, AudioLines } from 'lucide-react';
import { useChatStore, type PendingAttachment } from '@/stores/chat-store';
import { isImageMime, readAttachment } from '@/lib/attachments';
import { AttachmentChip } from '@/components/chat';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { AgentModelPicker } from './AgentModelPicker';
import { ConnectorChips } from './ConnectorChips';

/**
 * The Genspark-style central composer for the workspace landing
 * (TODO-genspark-workspace-shell §P1.2 / §P2). One large rounded input with a
 * control row underneath: attach · agent/model · connectors · voice · send.
 * Reuses the existing `/ws/chat` pipeline via `useChatStore` — no new socket.
 */
export function PromptBar({ onSent }: { onSent?: () => void }) {
  const intl = useIntl();
  const { isStreaming, connectionState, supportsVision, send } = useChatStore();

  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const hasImageAttachment = attachments.some((a) => isImageMime(a.mime));
  const showVisionWarning = hasImageAttachment && !supportsVision;

  const canSend =
    (input.trim().length > 0 || attachments.length > 0) &&
    !isStreaming &&
    connectionState === 'connected';

  const handleSend = () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || isStreaming) return;
    if (connectionState !== 'connected') return;
    send(text, attachments);
    setInput('');
    setAttachments([]);
    onSent?.();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleFilesSelected = async (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    const next: PendingAttachment[] = [];
    for (const file of Array.from(fileList)) {
      const result = await readAttachment(file);
      if (result.ok) {
        next.push(result.attachment);
      } else if (result.reason === 'too-large') {
        toast.error(
          intl.formatMessage(
            { id: 'webchat.attachTooLarge', defaultMessage: '{name} 超過 20MB 上限' },
            { name: result.name },
          ),
        );
      } else {
        toast.error(
          intl.formatMessage(
            { id: 'webchat.attachReadFailed', defaultMessage: '無法讀取 {name}' },
            { name: result.name },
          ),
        );
      }
    }
    if (next.length > 0) setAttachments((prev) => [...prev, ...next]);
    if (fileInputRef.current) fileInputRef.current.value = '';
  };

  const removeAttachment = (index: number) => {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  };

  return (
    <div
      className={cn(
        'rounded-2xl border border-surface-border bg-surface px-4 pb-3 pt-4 shadow-[var(--surface-shadow)]',
        connectionState !== 'connected' && 'opacity-70'
      )}
    >
      {/* Pending attachment chips */}
      {attachments.length > 0 && (
        <div className="mb-2 flex flex-wrap gap-1.5">
          {attachments.map((a, i) => (
            <AttachmentChip key={i} name={a.name} mime={a.mime} onRemove={() => removeAttachment(i)} />
          ))}
        </div>
      )}

      <label htmlFor="workspace-prompt" className="sr-only">
        {intl.formatMessage({ id: 'workspace.promptLabel', defaultMessage: '輸入指令' })}
      </label>
      <textarea
        id="workspace-prompt"
        ref={inputRef}
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={handleKeyDown}
        rows={2}
        placeholder={intl.formatMessage({
          id: 'workspace.promptPlaceholder',
          defaultMessage: '問任何問題,交辦任何任務…',
        })}
        className="w-full resize-none bg-transparent px-1 text-base text-foreground placeholder:text-muted-foreground focus-visible:outline-none"
        disabled={connectionState !== 'connected'}
      />

      {/* Vision warning */}
      {showVisionWarning && (
        <div className="mb-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
          {intl.formatMessage({ id: 'webchat.visionWarning' })}
        </div>
      )}

      {/* Control row */}
      <div className="mt-2 flex items-center gap-2">
        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          onChange={(e) => handleFilesSelected(e.target.files)}
        />
        <button
          type="button"
          onClick={() => fileInputRef.current?.click()}
          disabled={connectionState !== 'connected'}
          title={intl.formatMessage({ id: 'webchat.attach' })}
          aria-label={intl.formatMessage({ id: 'webchat.attach' })}
          className="grid h-9 w-9 shrink-0 place-items-center rounded-lg text-muted-foreground transition-colors outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50"
        >
          <Plus className="h-5 w-5" />
        </button>

        <AgentModelPicker />
        <ConnectorChips />

        <div className="flex-1" />

        <VoiceButton />

        <button
          type="button"
          onClick={handleSend}
          disabled={!canSend}
          aria-label={intl.formatMessage({ id: 'workspace.send', defaultMessage: '送出' })}
          className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand text-brand-foreground transition-colors outline-none hover:bg-brand/90 focus-visible:ring-3 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50"
        >
          {isStreaming ? <Loader2 className="h-5 w-5 animate-spin" /> : <Send className="h-5 w-5" />}
        </button>
      </div>
    </div>
  );
}

/**
 * Voice entry (TODO §P2.3). The voice pipeline (feature 14) is not yet wired to
 * the dashboard WS, so this renders as a disabled "coming soon" affordance with
 * a tooltip rather than a dead button. Flip to active once the backend lands.
 */
function VoiceButton() {
  const intl = useIntl();
  return (
    <button
      type="button"
      disabled
      title={intl.formatMessage({ id: 'workspace.voiceComingSoon', defaultMessage: '語音功能即將推出' })}
      aria-label={intl.formatMessage({ id: 'workspace.voiceComingSoon', defaultMessage: '語音功能即將推出' })}
      className="grid h-9 w-9 shrink-0 cursor-not-allowed place-items-center rounded-lg text-muted-foreground/50"
    >
      <AudioLines className="h-5 w-5" />
    </button>
  );
}
