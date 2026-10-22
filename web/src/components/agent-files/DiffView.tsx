import { useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import type { AgentFileDiffHunk } from '@/lib/agent-files-types';

/** Unchanged runs longer than this collapse to a "N unchanged lines" line,
 *  keeping CONTEXT_LINES of context on each side. */
const COLLAPSE_AFTER = 6;
const CONTEXT_LINES = 2;

export type DiffRow =
  | { kind: 'add' | 'remove' | 'equal'; text: string }
  | { kind: 'collapsed'; count: number };

type IndexedRow = Exclude<DiffRow, { kind: 'collapsed' }> | { kind: 'collapsed'; count: number; hunk: number };

function rowsWithIndex(hunks: ReadonlyArray<AgentFileDiffHunk>, expanded: ReadonlySet<number>): IndexedRow[] {
  const rows: IndexedRow[] = [];
  hunks.forEach((h, i) => {
    if (h.op !== 'equal' || h.lines.length <= COLLAPSE_AFTER || expanded.has(i)) {
      for (const line of h.lines) rows.push({ kind: h.op, text: line });
      return;
    }
    const lines = h.lines;
    const head = i === 0 ? [] : lines.slice(0, CONTEXT_LINES);
    const tail = i === hunks.length - 1 ? [] : lines.slice(lines.length - CONTEXT_LINES);
    for (const line of head) rows.push({ kind: 'equal', text: line });
    rows.push({ kind: 'collapsed', count: lines.length - head.length - tail.length, hunk: i });
    for (const line of tail) rows.push({ kind: 'equal', text: line });
  });
  return rows;
}

/** Flatten the server's hunks into display rows, collapsing long unchanged runs. */
export function diffRows(hunks: ReadonlyArray<AgentFileDiffHunk>): DiffRow[] {
  return rowsWithIndex(hunks, new Set()).map((r) => (r.kind === 'collapsed' ? { kind: 'collapsed', count: r.count } : r));
}

export interface DiffViewProps {
  hunks: ReadonlyArray<AgentFileDiffHunk>;
  className?: string;
}

/**
 * DiffView — a line diff computed by the gateway (`agent_files.diff`, the
 * approval card), shown as one column with +/- markers. Long unchanged runs
 * collapse to a line that expands on click. Every line is plain text.
 */
export function DiffView({ hunks, className }: DiffViewProps) {
  const intl = useIntl();
  const [expanded, setExpanded] = useState<ReadonlySet<number>>(() => new Set());
  const added = intl.formatMessage({ id: 'agentFiles.approval.addedLine' });
  const removed = intl.formatMessage({ id: 'agentFiles.approval.removedLine' });
  const rows = rowsWithIndex(hunks, expanded);
  if (rows.length === 0) {
    return <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'agentFiles.diff.noChanges' })}</p>;
  }
  return (
    <ol
      className={cn('max-h-80 overflow-auto rounded-lg bg-muted py-1 font-mono text-[11px] leading-relaxed', className)}
      data-testid="agent-file-diff"
    >
      {rows.map((r, i) =>
        r.kind === 'collapsed' ? (
          <li key={i} className="px-2 py-0.5 text-center font-sans">
            <button
              type="button"
              className="text-muted-foreground underline-offset-2 hover:text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              onClick={() => setExpanded((prev) => new Set(prev).add(r.hunk))}
            >
              {intl.formatMessage({ id: 'agentFiles.approval.unchangedLines' }, { count: r.count })}
            </button>
          </li>
        ) : (
          <li
            key={i}
            data-diff={r.kind}
            className={cn(
              'flex gap-2 whitespace-pre-wrap break-all px-2',
              r.kind === 'add' && 'bg-success/10 text-foreground',
              r.kind === 'remove' && 'bg-destructive/10 text-foreground',
              r.kind === 'equal' && 'text-muted-foreground',
            )}
          >
            <span
              aria-hidden="true"
              className={cn('w-3 shrink-0 select-none', r.kind === 'add' && 'text-success', r.kind === 'remove' && 'text-destructive')}
            >
              {r.kind === 'add' ? '+' : r.kind === 'remove' ? '-' : ' '}
            </span>
            {r.kind !== 'equal' && <span className="sr-only">{r.kind === 'add' ? added : removed}: </span>}
            <span className="min-w-0">{r.text}</span>
          </li>
        ),
      )}
    </ol>
  );
}
