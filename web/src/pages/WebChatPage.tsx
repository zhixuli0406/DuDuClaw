import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useChatStore, type ChatMessage, type PendingAttachment } from '@/stores/chat-store';
import { cn } from '@/lib/utils';
import { Send, RotateCcw, Loader2, Paperclip, X, Eye, EyeOff, FileText, Image as ImageIcon } from 'lucide-react';
import { toast } from '@/lib/toast';

/** 20 MB — must match the backend `media::MAX_FILE_SIZE` guard. */
const MAX_ATTACHMENT_BYTES = 20 * 1024 * 1024;

function isImageMime(mime: string): boolean {
  return mime.startsWith('image/');
}

/** Read a File into a base64 string (without the data: URI prefix). */
function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const comma = result.indexOf(',');
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.onerror = () => reject(reader.error ?? new Error('read failed'));
    reader.readAsDataURL(file);
  });
}

function AttachmentChip({
  name,
  mime,
  onRemove,
}: {
  name: string;
  mime: string;
  onRemove?: () => void;
}) {
  const Icon = isImageMime(mime) ? ImageIcon : FileText;
  return (
    <span className="inline-flex max-w-[12rem] items-center gap-1.5 rounded-lg bg-stone-200/70 px-2 py-1 text-xs text-stone-700 dark:bg-stone-700/70 dark:text-stone-200">
      <Icon className="h-3.5 w-3.5 flex-shrink-0" />
      <span className="truncate">{name}</span>
      {onRemove && (
        <button
          onClick={onRemove}
          className="flex-shrink-0 rounded p-0.5 hover:bg-stone-300 dark:hover:bg-stone-600"
          aria-label="Remove attachment"
        >
          <X className="h-3 w-3" />
        </button>
      )}
    </span>
  );
}

function MessageBubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === 'user';
  const isSystem = message.role === 'system';

  return (
    <div
      className={cn(
        'flex w-full',
        isUser ? 'justify-end' : 'justify-start'
      )}
    >
      <div
        className={cn(
          'max-w-[80%] rounded-2xl px-4 py-2.5 text-sm leading-relaxed',
          isUser
            ? 'bg-amber-500 text-white'
            : isSystem
              ? 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400'
              : 'bg-stone-100 text-stone-800 dark:bg-stone-800 dark:text-stone-200'
        )}
      >
        {message.attachments && message.attachments.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-1.5">
            {message.attachments.map((a, i) => (
              <AttachmentChip key={i} name={a.name} mime={a.mime} />
            ))}
          </div>
        )}
        {message.content && (
          <div className="whitespace-pre-wrap break-words">{message.content}</div>
        )}
        {message.tokens != null && message.tokens > 0 && (
          <div className="mt-1 text-xs opacity-50">{message.tokens} tokens</div>
        )}
      </div>
    </div>
  );
}

function TypingIndicator() {
  return (
    <div className="flex justify-start">
      <div className="flex items-center gap-1 rounded-2xl bg-stone-100 px-4 py-3 dark:bg-stone-800">
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:0ms]" />
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:150ms]" />
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:300ms]" />
      </div>
    </div>
  );
}

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
      if (file.size > MAX_ATTACHMENT_BYTES) {
        toast.error(
          intl.formatMessage(
            { id: 'webchat.attachTooLarge', defaultMessage: '{name} 超過 20MB 上限' },
            { name: file.name },
          ),
        );
        continue;
      }
      try {
        const dataBase64 = await readFileAsBase64(file);
        next.push({ name: file.name, mime: file.type || 'application/octet-stream', dataBase64 });
      } catch {
        toast.error(
          intl.formatMessage(
            { id: 'webchat.attachReadFailed', defaultMessage: '無法讀取 {name}' },
            { name: file.name },
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
      <div className="flex items-center justify-between border-b border-stone-200 px-6 py-4 dark:border-stone-800">
        <div className="flex items-center gap-3">
          <span className="text-2xl">{agentIcon}</span>
          <div>
            <h2 className="text-lg font-semibold text-stone-900 dark:text-stone-50">
              {agentName}
            </h2>
            <div className="flex items-center gap-1.5">
              <span
                className={cn(
                  'h-2 w-2 rounded-full',
                  connectionState === 'connected'
                    ? 'bg-emerald-500'
                    : connectionState === 'connecting'
                      ? 'bg-amber-500'
                      : 'bg-stone-400'
                )}
              />
              <span className="text-xs text-stone-500 dark:text-stone-400">
                {connectionState === 'connected'
                  ? intl.formatMessage({ id: 'webchat.connected', defaultMessage: 'Connected' })
                  : connectionState === 'connecting'
                    ? intl.formatMessage({ id: 'webchat.connecting', defaultMessage: 'Connecting...' })
                    : intl.formatMessage({ id: 'webchat.disconnected', defaultMessage: 'Disconnected' })}
              </span>
            </div>
          </div>
        </div>

        <button
          onClick={reset}
          className="rounded-lg p-2 text-stone-500 transition-colors hover:bg-stone-100 hover:text-stone-700 dark:hover:bg-stone-800 dark:hover:text-stone-300"
          title={intl.formatMessage({ id: 'webchat.reset', defaultMessage: 'New conversation' })}
        >
          <RotateCcw className="h-4 w-4" />
        </button>
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
      <div className="border-t border-stone-200 px-6 py-4 dark:border-stone-800">
        <div className="mx-auto max-w-2xl space-y-2">
          {/* Capability badge */}
          <div className="flex items-center gap-2 text-xs text-stone-400 dark:text-stone-500">
            {supportsVision ? (
              <span
                className="inline-flex items-center gap-1 text-emerald-600 dark:text-emerald-400"
                title={intl.formatMessage({
                  id: 'webchat.visionOnTip',
                  defaultMessage: '此模型可理解上傳的圖片內容',
                })}
              >
                <Eye className="h-3.5 w-3.5" />
                {intl.formatMessage({ id: 'webchat.visionOn', defaultMessage: '支援圖片' })}
              </span>
            ) : (
              <span
                className="inline-flex items-center gap-1"
                title={intl.formatMessage({
                  id: 'webchat.visionOffTip',
                  defaultMessage: '此模型不支援圖片理解,僅會讀取文件的文字內容',
                })}
              >
                <EyeOff className="h-3.5 w-3.5" />
                {intl.formatMessage({ id: 'webchat.visionOff', defaultMessage: '僅文件' })}
              </span>
            )}
            {model && <span className="opacity-70">· {model}</span>}
          </div>

          {/* Vision warning when an image is queued for a text-only model */}
          {showVisionWarning && (
            <div className="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-700 dark:border-amber-800 dark:bg-amber-900/20 dark:text-amber-400">
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
            <button
              onClick={() => fileInputRef.current?.click()}
              disabled={connectionState !== 'connected'}
              title={intl.formatMessage({ id: 'webchat.attach', defaultMessage: '上傳檔案' })}
              className={cn(
                'flex h-11 w-11 flex-shrink-0 items-center justify-center rounded-xl border transition-colors',
                'border-stone-200 text-stone-500 hover:bg-stone-100 disabled:opacity-50',
                'dark:border-stone-700 dark:text-stone-400 dark:hover:bg-stone-800'
              )}
            >
              <Paperclip className="h-5 w-5" />
            </button>
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
                'flex-1 resize-none rounded-xl border border-stone-200 bg-white px-4 py-3 text-sm',
                'placeholder:text-stone-400 focus:border-amber-400 focus:outline-none focus:ring-2 focus:ring-amber-400/20',
                'dark:border-stone-700 dark:bg-stone-800 dark:text-stone-200 dark:placeholder:text-stone-500 dark:focus:border-amber-500'
              )}
              disabled={connectionState !== 'connected'}
            />
            <button
              onClick={handleSend}
              disabled={!canSend}
              className={cn(
                'flex h-11 w-11 flex-shrink-0 items-center justify-center rounded-xl transition-colors',
                canSend
                  ? 'bg-amber-500 text-white hover:bg-amber-600'
                  : 'bg-stone-100 text-stone-400 dark:bg-stone-800'
              )}
            >
              {isStreaming ? (
                <Loader2 className="h-5 w-5 animate-spin" />
              ) : (
                <Send className="h-5 w-5" />
              )}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
