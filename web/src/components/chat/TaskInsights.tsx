import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Check,
  ChevronDown,
  FileText,
  Pencil,
  Search,
  Terminal,
  ListChecks,
  Wrench,
  Globe,
  BookOpen,
  Users,
  Loader2,
  type LucideIcon,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import type { ChatStep, StepNode } from '@/stores/chat-store';

/** Tool name → icon. Keeps the tree scannable at a glance. */
function iconForTool(tool: string): LucideIcon {
  switch (tool.toLowerCase()) {
    case 'read':
      return FileText;
    case 'write':
    case 'edit':
    case 'multiedit':
      return Pencil;
    case 'grep':
    case 'glob':
    case 'search':
      return Search;
    case 'bash':
      return Terminal;
    case 'webfetch':
    case 'websearch':
      return Globe;
    case 'notebookedit':
    case 'notebookread':
      return BookOpen;
    case 'task':
      return Users;
    default:
      return Wrench;
  }
}

/** Strip the leading status emoji the gateway prepends for a cleaner row. */
function clean(content: string): string {
  return content.replace(/^[⏳✅🔧📋]\s*/u, '').trim();
}

/** One tool step, indented by depth, with a running spinner or a done check. */
function StepRow({ node, runningLabel, doneLabel }: { node: StepNode; runningLabel: string; doneLabel: string }) {
  const Icon = iconForTool(node.tool);
  return (
    <li
      className="flex items-start gap-2 text-xs text-muted-foreground"
      style={{ paddingLeft: `${node.depth * 14}px` }}
    >
      {node.running ? (
        <Loader2 className="mt-0.5 h-3.5 w-3.5 shrink-0 animate-spin text-brand" aria-label={runningLabel} />
      ) : (
        <Check className="mt-0.5 h-3.5 w-3.5 shrink-0 text-success" aria-label={doneLabel} />
      )}
      <Icon className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground/60" aria-hidden="true" />
      <span className="min-w-0 break-words">
        <span className="font-medium text-foreground">{node.tool}</span>
        {node.summary && <span className="text-muted-foreground"> · {node.summary}</span>}
      </span>
    </li>
  );
}

/**
 * TaskInsights — the live agentic step tree (V7 / T7.3). Folds the gateway's
 * structured `step` frames into a collapsible tree (start opens a node with a
 * spinner, its `end` flips it to a check; `depth` indents nested sub-agent
 * tools), and lists any task-board (`todo`) progress below it. Sits above the
 * in-progress assistant reply while streaming; once the reply lands it collapses
 * but stays expandable.
 */
export function TaskInsights({
  tree,
  todos,
  streaming,
}: {
  tree: readonly StepNode[];
  todos: readonly ChatStep[];
  streaming: boolean;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(true);
  const manual = useRef(false);
  const prevStreaming = useRef(streaming);

  // Auto-collapse when the turn finishes, unless the user has toggled it by hand.
  useEffect(() => {
    if (prevStreaming.current && !streaming && !manual.current) {
      setOpen(false);
    }
    prevStreaming.current = streaming;
  }, [streaming]);

  const total = tree.length + todos.length;
  if (total === 0) return null;

  const runningLabel = intl.formatMessage({ id: 'chat.insights.running', defaultMessage: 'In progress' });
  const doneLabel = intl.formatMessage({ id: 'chat.insights.done', defaultMessage: 'Done' });

  return (
    <div className="rounded-xl border border-surface-border bg-surface p-3">
      <button
        type="button"
        onClick={() => {
          manual.current = true;
          setOpen((o) => !o);
        }}
        aria-expanded={open}
        className="flex w-full items-center gap-2 text-sm font-medium text-foreground"
      >
        <ChevronDown className={cn('h-4 w-4 shrink-0 transition-transform', !open && '-rotate-90')} />
        <span>{intl.formatMessage({ id: 'chat.insights.title', defaultMessage: 'Task insights' })}</span>
        <span className="rounded-full bg-muted px-1.5 font-mono text-[11px] tabular-nums text-muted-foreground">
          {total}
        </span>
        {streaming && <Loader2 className="h-3.5 w-3.5 animate-spin text-brand" />}
      </button>

      {open && (
        <ol className="mt-2 space-y-1.5 border-l-2 border-border pl-3 text-xs text-muted-foreground">
          {tree.map((node) => (
            <StepRow key={node.id} node={node} runningLabel={runningLabel} doneLabel={doneLabel} />
          ))}
          {todos.map((s) => (
            <li key={s.id} className="flex items-start gap-2 text-xs text-muted-foreground">
              <ListChecks className="mt-0.5 h-3.5 w-3.5 shrink-0 text-brand" aria-hidden="true" />
              <span className="min-w-0 whitespace-pre-wrap break-words">{clean(s.content)}</span>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
