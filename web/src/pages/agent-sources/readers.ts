import type { IntlShape } from 'react-intl';
import type { KbReader, KbReaderCounts } from '@/lib/knowledge-api';

/** One reader in plain words (你 / a name / a department / anyone on LINE). */
export function readerLabel(intl: IntlShape, r: KbReader): string {
  if (r.you) return intl.formatMessage({ id: 'agentSources.reader.you' });
  switch (r.kind) {
    case 'department':
      return intl.formatMessage({ id: 'agentSources.reader.department' }, { name: r.label });
    case 'agent':
      return intl.formatMessage({ id: 'agentSources.reader.agent' }, { name: r.label });
    case 'channel':
      return intl.formatMessage({ id: 'agentSources.reader.channel' }, { name: r.label });
    default:
      return r.label;
  }
}

/**
 * Who else can read a collection, as one sentence. Names when the
 * gateway gave them (the viewer owns the collection), else counts.
 * Admins can always read every collection, so they are always mentioned.
 */
export function readersSentence(
  intl: IntlShape,
  readers: KbReader[] | null,
  counts: KbReaderCounts,
  /** The employee whose tab this is: it reads its own collections too, so it is named first. */
  self?: { id: string; name: string },
): string {
  const selfLabel = self ? intl.formatMessage({ id: 'agentSources.reader.self' }, { name: self.name }) : null;
  if (readers) {
    const rest = self ? readers.filter((r) => !(r.kind === 'agent' && r.id === self.id)) : readers;
    const names = rest.map((r) => readerLabel(intl, r));
    if (names.length === 0) return intl.formatMessage({ id: 'agentSources.readers.adminsOnly' });
    return intl.formatMessage(
      { id: 'agentSources.readers.named' },
      { names: intl.formatList(selfLabel ? [selfLabel, ...names] : names, { type: 'conjunction' }) },
    );
  }
  const parts: string[] = [];
  if (counts.users > 0) parts.push(intl.formatMessage({ id: 'agentSources.readers.countUsers' }, { count: counts.users }));
  if (counts.departments > 0)
    parts.push(intl.formatMessage({ id: 'agentSources.readers.countDepartments' }, { count: counts.departments }));
  if (counts.agents > 0) parts.push(intl.formatMessage({ id: 'agentSources.readers.countAgents' }, { count: counts.agents }));
  if (counts.channels > 0)
    parts.push(intl.formatMessage({ id: 'agentSources.readers.countChannels' }, { count: counts.channels }));
  if (parts.length === 0) return intl.formatMessage({ id: 'agentSources.readers.adminsOnly' });
  return intl.formatMessage(
    { id: 'agentSources.readers.counted' },
    { parts: intl.formatList(selfLabel ? [selfLabel, ...parts] : parts, { type: 'conjunction' }) },
  );
}

/** A channel grant: anyone in that chat app can read it. */
export function hasChannelReader(readers: KbReader[] | null, counts: KbReaderCounts): boolean {
  return readers ? readers.some((r) => r.kind === 'channel') : counts.channels > 0;
}
