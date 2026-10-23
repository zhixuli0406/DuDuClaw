import { useIntl, type IntlShape } from 'react-intl';
import type { TeamHistoryPart } from '@/lib/api';

export function historyCategoryLabel(intl: IntlShape, category: string): string {
  const id = `teamHistory.category.${category}`;
  return id in intl.messages ? intl.formatMessage({ id }) : category;
}

/** Plain text only. Restricted external views never expose attachment names. */
export function HistoryPart({ part, showAttachmentNames = true }: { part: TeamHistoryPart; showAttachmentNames?: boolean }) {
  const intl = useIntl();
  if (part.t === 'text') return <>{part.v}</>;
  if (part.t === 'attachment') return <span className="rounded bg-muted px-2 py-0.5 text-muted-foreground">{intl.formatMessage({ id: part.kind === 'image' ? 'teamHistory.image' : 'teamHistory.attachment' })}{showAttachmentNames && part.name ? ` · ${part.name}` : ''}</span>;
  if (part.t !== 'mask') return null;
  const label = historyCategoryLabel(intl, part.cat);
  return <span className="mx-0.5 inline-block rounded-md bg-brand/10 px-1.5 py-0.5 text-xs text-brand" title={intl.formatMessage({ id: 'teamHistory.maskHint' }, { category: label })}>{label} {part.n}</span>;
}
