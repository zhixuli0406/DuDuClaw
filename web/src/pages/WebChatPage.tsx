import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useChatStore, type PendingAttachment } from '@/stores/chat-store';
import { cn } from '@/lib/utils';
import { Send, RotateCcw, Loader2, Paperclip, Eye, EyeOff } from 'lucide-react';
import { toast } from '@/lib/toast';
import { Button, Badge } from '@/components/ui';
import { AttachmentChip, MessageBubble, TypingIndicator } from '@/components/chat';
import { isImageMime, readAttachment } from '@/lib/attachments';

export function WebChatPage() {
  const intl = useIntl();
  const {
    messages,
    isStreaming,
    agentName,
    agentIcon,
    supportsVision,
    model,
    connectionState,
    connect,
    send,
    reset,
  } = useChatStore();

  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const hasImageAttachment = attachments.some((a) => isImageMime(a.mime));
  const showVisionWarning = hasImageAttachment && !supportsVision;

  // Connect on mount
  useEffect(() => {
    if (connectionState === 'disconnected') {
      connect();
    }
    return () => {
      // Don't disconnect on unmount — keep session alive
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Auto-scroll to bottom
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isStreaming]);

  const handleSend = () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || isStreaming) return;
    send(text, attachments);
    setInput('');
    setAttachments([]);
    inputRef.current?.focus();
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
    // Reset so selecting the same file again re-triggers onChange.
    if (fileInputRef.current) fileInputRef.current.value = '';
  };

  const removeAttachment = (index: number) => {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  };

  const canSend =
    (input.trim().length > 0 || attachments.length > 0) &&
    !isStreaming &&
    connectionState === 'connected';

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-[var(--panel-border)] px-6 py-4">
        <div className="flex items-center gap-3">
          <span className="text-2xl">{agentIcon}</span>
          <div>
            <h2 className="text-lg font-semibold tracking-tight text-stone-900 dark:text-stone-50">
              {agentName}
            </h2>
            <div className="mt-0.5">
              <Badge
                dot
                tone={
                  connectionState === 'connected'
                    ? 'success'
                    : connectionState === 'connecting'
                      ? 'warning'
                      : 'neutral'
                }
              >
                {connectionState === 'connected'
                  ? intl.formatMessage({ id: 'webchat.connected', defaultMessage: 'Connected' })
                  : connectionState === 'connecting'
                    ? intl.formatMessage({ id: 'webchat.connecting', defaultMessage: 'Connecting...' })
                    : intl.formatMessage({ id: 'webchat.disconnected', defaultMessage: 'Disconnected' })}
              </Badge>
            </div>
          </div>
        </div>

        <Button
          variant="ghost"
          icon={RotateCcw}
          onClick={reset}
          title={intl.formatMessage({ id: 'webchat.reset', defaultMessage: 'New conversation' })}
        />
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-6 py-4">
        <div className="mx-auto max-w-2xl space-y-3">
          {messages.length === 0 && (
            <div className="flex flex-col items-center justify-center py-20 text-center">
              <span className="text-5xl">{agentIcon}</span>
              <h3 className="mt-4 text-lg font-medium text-stone-700 dark:text-stone-300">
                {intl.formatMessage({ id: 'webchat.welcome', defaultMessage: 'Hello! How can I help you?' })}
              </h3>
              <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'webchat.hint', defaultMessage: 'Type a message or use /help for commands' })}
              </p>
            </div>
          )}

          {messages.map((msg) => (
            <MessageBubble key={msg.id} message={msg} />
          ))}

          {isStreaming && <TypingIndicator />}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {/* Input */}
      <div className="border-t border-[var(--panel-border)] px-6 py-4">
        <div className="mx-auto max-w-2xl space-y-2">
          {/* Capability badge */}
          <div className="flex items-center gap-2 text-xs text-stone-400 dark:text-stone-500">
            {supportsVision ? (
              <span
                title={intl.formatMessage({
                  id: 'webchat.visionOnTip',
                  defaultMessage: '此模型可理解上傳的圖片內容',
                })}
              >
                <Badge tone="success">
                  <Eye className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'webchat.visionOn', defaultMessage: '支援圖片' })}
                </Badge>
              </span>
            ) : (
              <span
                title={intl.formatMessage({
                  id: 'webchat.visionOffTip',
                  defaultMessage: '此模型不支援圖片理解,僅會讀取文件的文字內容',
                })}
              >
                <Badge tone="neutral">
                  <EyeOff className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'webchat.visionOff', defaultMessage: '僅文件' })}
                </Badge>
              </span>
            )}
            {model && <span className="opacity-70 tabular-nums">· {model}</span>}
          </div>

          {/* Vision warning when an image is queued for a text-only model */}
          {showVisionWarning && (
            <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
              {intl.formatMessage({
                id: 'webchat.visionWarning',
                defaultMessage: '⚠️ 目前模型不支援圖片理解 — 圖片仍會送出,但模型可能無法看見內容。',
              })}
            </div>
          )}

          {/* Pending attachment chips */}
          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {attachments.map((a, i) => (
                <AttachmentChip key={i} name={a.name} mime={a.mime} onRemove={() => removeAttachment(i)} />
              ))}
            </div>
          )}

          <div className="flex items-end gap-3">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="hidden"
              onChange={(e) => handleFilesSelected(e.target.files)}
            />
            <Button
              variant="secondary"
              icon={Paperclip}
              onClick={() => fileInputRef.current?.click()}
              disabled={connectionState !== 'connected'}
              title={intl.formatMessage({ id: 'webchat.attach', defaultMessage: '上傳檔案' })}
              className="h-11 w-11 shrink-0 rounded-xl"
            />
            <textarea
              ref={inputRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={intl.formatMessage({
                id: 'webchat.placeholder',
                defaultMessage: 'Type a message...',
              })}
              rows={1}
              className={cn(
                'flex-1 resize-none rounded-xl border border-[var(--panel-border)] bg-[var(--panel-fill)] px-4 py-3 text-sm',
                'text-stone-800 placeholder:text-stone-400 focus-visible:border-amber-500/50 focus-visible:outline-none',
                'focus-visible:ring-2 focus-visible:ring-amber-500/30 dark:text-stone-100 dark:placeholder:text-stone-500'
              )}
              disabled={connectionState !== 'connected'}
            />
            <Button
              variant="primary"
              onClick={handleSend}
              disabled={!canSend}
              className="h-11 w-11 shrink-0 rounded-xl px-0"
            >
              {isStreaming ? (
                <Loader2 className="h-5 w-5 animate-spin" />
              ) : (
                <Send className="h-5 w-5" />
              )}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
