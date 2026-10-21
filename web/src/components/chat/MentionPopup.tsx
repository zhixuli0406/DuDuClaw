import { useIntl } from 'react-intl';
import { CharacterAvatar } from '@/components/character';
import { cn } from '@/lib/utils';
import type { MentionAgent } from '@/lib/mention';

/** Element id of the listbox, referenced by the composer's `aria-controls`. */
export const MENTION_LISTBOX_ID = 'webchat-mention-listbox';

/** Element id of option `i`, referenced by `aria-activedescendant`. */
export function mentionOptionId(i: number): string {
  return `webchat-mention-option-${i}`;
}

/**
 * The `@` popup above the chat composer: the AI staff the signed-in account
 * may call, filtered by what follows the `@`. Keyboard handling lives in the
 * composer (focus never leaves the textarea); this component only renders the
 * ARIA listbox and handles pointer picks.
 */
export function MentionPopup({
  candidates,
  activeIndex,
  onPick,
  onHover,
}: {
  candidates: readonly MentionAgent[];
  activeIndex: number;
  onPick: (agent: MentionAgent) => void;
  onHover: (index: number) => void;
}) {
  const intl = useIntl();
  return (
    <div
      className="absolute bottom-full left-0 right-0 z-20 mb-1.5 max-h-64 overflow-y-auto rounded-xl border border-surface-border bg-surface p-1 shadow-lg"
      data-testid="mention-popup"
    >
      {candidates.length === 0 ? (
        <p className="px-3 py-2 text-sm text-muted-foreground" role="status">
          {intl.formatMessage({ id: 'webchat.mention.empty', defaultMessage: '沒有符合的 AI 員工' })}
        </p>
      ) : (
        <ul
          id={MENTION_LISTBOX_ID}
          role="listbox"
          aria-label={intl.formatMessage({ id: 'webchat.mention.label', defaultMessage: '可以呼叫的 AI 員工' })}
        >
          {candidates.map((a, i) => {
            const subtitle = a.department || (a.role === 'main'
              ? intl.formatMessage({ id: 'webchat.mention.main', defaultMessage: '預設 AI 員工' })
              : '');
            return (
              <li
                key={a.name}
                id={mentionOptionId(i)}
                role="option"
                aria-selected={i === activeIndex}
                className={cn(
                  'flex cursor-pointer items-center gap-2 rounded-lg px-2.5 py-1.5 text-sm',
                  i === activeIndex ? 'bg-muted text-foreground' : 'text-surface-foreground',
                )}
                // mousedown, not click: keep focus in the textarea.
                onMouseDown={(e) => {
                  e.preventDefault();
                  onPick(a);
                }}
                onMouseEnter={() => onHover(i)}
              >
                <CharacterAvatar agentId={a.name} name={a.display_name} size={22} variant="avatar" animated={false} />
                <span className="font-medium">{a.display_name}</span>
                {subtitle && <span className="truncate text-xs text-muted-foreground">{subtitle}</span>}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
