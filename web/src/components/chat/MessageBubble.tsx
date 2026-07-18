import type { ReactNode } from 'react';
import type { ChatMessage } from '@/stores/chat-store';
import { cn } from '@/lib/utils';
import { AttachmentChip } from './AttachmentChip';

/** A single chat turn rendered as a left/right-aligned bubble (Multica plain
 *  style, spec §5.6): the user's turn sits right on `bg-secondary`, the
 *  assistant's sits left on `bg-surface`, a system notice is destructive-toned.
 *  `leading` renders a small avatar to the left of an assistant/system bubble
 *  (the conversation partner's identity). */
export function MessageBubble({ message, leading }: { message: ChatMessage; leading?: ReactNode }) {
  const isUser = message.role === 'user';
  const isSystem = message.role === 'system';

  return (
    <div className={cn('flex w-full items-end gap-2', isUser ? 'justify-end' : 'justify-start')}>
      {!isUser && leading && <div className="mb-0.5 shrink-0">{leading}</div>}
      <div
        className={cn(
          'max-w-[80%] rounded-xl px-3.5 py-2 text-sm leading-relaxed',
          isUser
            ? 'bg-secondary text-secondary-foreground'
            : isSystem
              ? 'bg-destructive/10 text-destructive ring-1 ring-inset ring-destructive/20'
              : 'bg-surface text-surface-foreground ring-1 ring-surface-border',
        )}
      >
        {message.attachments && message.attachments.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-1.5">
            {message.attachments.map((a, i) => (
              <AttachmentChip key={i} name={a.name} mime={a.mime} />
            ))}
          </div>
        )}
        {message.content && <div className="whitespace-pre-wrap break-words">{message.content}</div>}
        {message.tokens != null && message.tokens > 0 && (
          <div className="mt-1 font-mono text-xs tabular-nums text-muted-foreground">{message.tokens} tokens</div>
        )}
      </div>
    </div>
  );
}
