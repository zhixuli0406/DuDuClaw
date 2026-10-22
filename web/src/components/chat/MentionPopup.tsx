import { useIntl } from 'react-intl';
import { CharacterAvatar } from '@/components/character';
import { cn } from '@/lib/utils';
import { groupMentionCandidates, type MentionAgent, type MentionGroup } from '@/lib/mention';

/** Element id of the listbox, referenced by the composer's `aria-controls`. */
export const MENTION_LISTBOX_ID = 'webchat-mention-listbox';

/** Element id of option `i`, referenced by `aria-activedescendant`. */
export function mentionOptionId(i: number): string {
  return `webchat-mention-option-${i}`;
}

/**
 * The `@` popup above the chat composer: the AI staff the signed-in account
 * may call, filtered by what follows the `@`. When the list spans more than
 * one source it is split into sections — my AI staff, each report's AI staff
 * (a manager's team), public AI staff. `candidates` must already be in that
 * order (`orderMentionCandidates`) so `activeIndex` matches what is shown.
 * Keyboard handling lives in the composer (focus never leaves the textarea);
 * this component only renders the ARIA listbox and handles pointer picks.
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
  const sections = groupMentionCandidates(candidates);
  const showHeadings = sections.length > 1;
  const indexOf = new Map<MentionAgent, number>(candidates.map((a, i) => [a, i]));
  const groupHeading = (group: MentionGroup): string => {
    if (group.kind === 'team') {
      return intl.formatMessage(
        { id: 'webchat.mention.groupTeam', defaultMessage: '我的團隊：{name}的 AI 員工' },
        { name: group.owner ?? '' },
      );
    }
    if (group.kind === 'public') {
      return intl.formatMessage({ id: 'webchat.mention.groupPublic', defaultMessage: '公開的 AI 員工' });
    }
    return intl.formatMessage({ id: 'webchat.mention.groupMine', defaultMessage: '我的 AI 員工' });
  };
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
          {sections.map((group) => (
            <li key={`${group.kind}:${group.owner ?? ''}`} role="presentation">
              {showHeadings && (
                <p
                  className="px-2.5 pb-0.5 pt-1.5 text-xs font-medium text-muted-foreground"
                  role="presentation"
                  data-testid="mention-group"
                >
                  {groupHeading(group)}
                </p>
              )}
              <ul role="group" aria-label={showHeadings ? groupHeading(group) : undefined}>
                {group.agents.map((a) => {
                  const i = indexOf.get(a) ?? 0;
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
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
