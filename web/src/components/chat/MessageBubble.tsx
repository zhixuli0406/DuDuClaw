import type { ChatMessage } from '@/stores/chat-store';
import { cn } from '@/lib/utils';
import { AttachmentChip } from './AttachmentChip';

/** A single chat turn rendered as a left/right-aligned bubble. */
export function MessageBubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === 'user';
  const isSystem = message.role === 'system';

  return (
    <div className={cn('flex w-full', isUser ? 'justify-end' : 'justify-start')}>
      <div
        className={cn(
          'max-w-[80%] rounded-2xl px-4 py-2.5 text-sm leading-relaxed',
          isUser
            ? 'bg-amber-500 text-white'
            : isSystem
              ? 'bg-rose-500/10 text-rose-700 ring-1 ring-inset ring-rose-500/20 dark:text-rose-400'
              : 'border border-[var(--panel-border)] bg-[var(--panel-fill)] text-stone-800 dark:text-stone-200'
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
          <div className="mt-1 text-xs opacity-50 tabular-nums">{message.tokens} tokens</div>
        )}
      </div>
    </div>
  );
}
