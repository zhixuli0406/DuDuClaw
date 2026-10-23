import { useState, type ReactNode } from 'react';
import { AlertTriangle } from 'lucide-react';
import type { ChatMessage } from '@/stores/chat-store';
import { cn } from '@/lib/utils';
import { AttachmentChip } from './AttachmentChip';
import { AssignedTaskCard } from './AssignedTaskCard';
import { CitedText, CitationSourceList } from './CitationChips';
import { PassageDrawer } from '@/components/knowledge/PassageDrawer';

/** A single chat turn rendered as a left/right-aligned bubble (Multica plain
 *  style, spec §5.6): the user's turn sits right on `bg-secondary`, the
 *  assistant's sits left on `bg-surface`, a system notice is destructive-toned.
 *  `leading` renders a small avatar to the left of an assistant/system bubble
 *  (the conversation partner's identity). */
export function MessageBubble({
  message,
  leading,
  label,
}: {
  message: ChatMessage;
  leading?: ReactNode;
  /** Small caption above an assistant bubble, e.g. who answered when a
   *  message called another AI staff member by `@`. */
  label?: string;
}) {
  const isUser = message.role === 'user';
  const isSystem = message.role === 'system';
  const citations = !isUser && !isSystem ? message.citations : undefined;
  const hasCitations = !!citations && citations.length > 0;
  const [openChunk, setOpenChunk] = useState<number | null>(null);

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
        {label && !isUser && <div className="mb-1 text-xs font-medium text-muted-foreground">{label}</div>}
        {message.attachments && message.attachments.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-1.5">
            {message.attachments.map((a, i) => (
              <AttachmentChip key={i} name={a.name} mime={a.mime} />
            ))}
          </div>
        )}
        {message.content && (
          <div className={cn('whitespace-pre-wrap break-words', isSystem && 'flex items-start gap-1.5')}>
            {isSystem && <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />}
            <span>
              {hasCitations ? (
                <CitedText
                  text={message.content}
                  citations={citations}
                  onOpen={(chunkId) => setOpenChunk(chunkId)}
                />
              ) : (
                message.content
              )}
            </span>
          </div>
        )}
        {message.task && !isUser && <AssignedTaskCard task={message.task} />}
        {hasCitations && (
          <CitationSourceList
            citations={citations}
            onOpen={(chunkId) => setOpenChunk(chunkId)}
          />
        )}
        {message.tokens != null && message.tokens > 0 && (
          <div className="mt-1 font-mono text-xs tabular-nums text-muted-foreground">{message.tokens} tokens</div>
        )}
      </div>
      {/* The document library's passage drawer: permissions are re-checked
          by the gateway on every open; the sheet returns focus on close. */}
      {hasCitations && <PassageDrawer chunkId={openChunk} onClose={() => setOpenChunk(null)} />}
    </div>
  );
}
