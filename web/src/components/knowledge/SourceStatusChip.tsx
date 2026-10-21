import { useIntl } from 'react-intl';
import { AlertCircle, CheckCircle2, Clock, Loader2, ShieldQuestion, Trash2 } from 'lucide-react';
import { Badge } from '@/components/mds';
import { cn } from '@/lib/utils';
import type { KbSource } from '@/lib/knowledge-api';

/** Tone classes (text colours chosen for AA contrast on both themes). */
const TONE = {
  ok: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-400',
  busy: 'bg-sky-500/12 text-sky-700 dark:text-sky-300',
  wait: 'bg-amber-500/15 text-amber-700 dark:text-amber-400',
  bad: 'bg-destructive/10 text-destructive',
  muted: 'bg-muted text-muted-foreground',
} as const;

type Tone = keyof typeof TONE;

interface ChipModel {
  tone: Tone;
  labelId: string;
  values?: Record<string, number>;
  icon: typeof Clock;
  spin?: boolean;
}

/** The chip for a source's current state. Exported for tests. */
export function chipModel(source: Pick<KbSource, 'status' | 'status_detail' | 'job'>): ChipModel {
  switch (source.status) {
    case 'ready':
      return source.status_detail === 'keyword_only'
        ? { tone: 'ok', labelId: 'kb.status.readyKeywordOnly', icon: CheckCircle2 }
        : { tone: 'ok', labelId: 'kb.status.ready', icon: CheckCircle2 };
    case 'failed':
      return { tone: 'bad', labelId: 'kb.status.failed', icon: AlertCircle };
    case 'awaiting_approval':
      return { tone: 'wait', labelId: 'kb.status.awaitingApproval', icon: ShieldQuestion };
    case 'queued':
      return { tone: 'busy', labelId: 'kb.status.queued', icon: Clock };
    case 'extracting':
      return { tone: 'busy', labelId: 'kb.status.extracting', icon: Loader2, spin: true };
    case 'indexing':
      return { tone: 'busy', labelId: 'kb.status.indexing', icon: Loader2, spin: true };
    case 'embedding': {
      const total = source.job?.progress_total ?? 0;
      const pct = total > 0 ? Math.min(100, Math.round(((source.job?.progress_done ?? 0) / total) * 100)) : 0;
      return { tone: 'busy', labelId: 'kb.status.embedding', values: { pct }, icon: Loader2, spin: true };
    }
    case 'deleting':
      return { tone: 'muted', labelId: 'kb.status.deleting', icon: Trash2 };
    default:
      return { tone: 'muted', labelId: 'kb.status.unknown', icon: Clock };
  }
}

/**
 * SourceStatusChip — one source's ingest state (排隊中 / 讀取中 / 建立索引中 /
 * 完成 / 失敗 …). A failure reason is shown next to the chip by the caller,
 * since it is a sentence, not a label.
 */
export function SourceStatusChip({ source, className }: { source: Pick<KbSource, 'status' | 'status_detail' | 'job'>; className?: string }) {
  const intl = useIntl();
  const m = chipModel(source);
  const Icon = m.icon;
  return (
    <Badge variant="ghost" className={cn(TONE[m.tone], className)} data-status={source.status}>
      <Icon aria-hidden className={cn(m.spin && 'motion-safe:animate-spin')} />
      {intl.formatMessage({ id: m.labelId }, m.values)}
    </Badge>
  );
}
