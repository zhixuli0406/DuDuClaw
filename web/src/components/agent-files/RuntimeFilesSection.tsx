import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import type { AgentRuntimeFile } from '@/lib/agent-files-types';
import { Skeleton } from '@/components/mds';

export interface RuntimeFilesSectionProps {
  agentId: string;
}

type State = { kind: 'idle' } | { kind: 'loading' } | { kind: 'failed' } | { kind: 'ready'; files: AgentRuntimeFile[] };

/**
 * The context files the system generates for the employee's runtime
 * (CLAUDE.md / AGENTS.md), view only and collapsed. They are fetched the first
 * time the section is opened; people who want to change the rules are sent to
 * the work rules above. File text stays DATA (rendered as a text node).
 */
export function RuntimeFilesSection({ agentId }: RuntimeFilesSectionProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [state, setState] = useState<State>({ kind: 'idle' });

  const load = async () => {
    setState({ kind: 'loading' });
    try {
      const r = await api.agentFiles.runtimeFiles(agentId);
      setState({ kind: 'ready', files: Array.isArray(r?.files) ? r.files : [] });
    } catch {
      setState({ kind: 'failed' });
    }
  };

  return (
    <details
      className="rounded-lg border border-surface-border px-3 py-2"
      data-testid="runtime-files-section"
      onToggle={(e) => {
        if ((e.currentTarget as HTMLDetailsElement).open && state.kind === 'idle') void load();
      }}
    >
      <summary className="cursor-pointer text-sm font-medium text-foreground">{t('agentFiles.runtime.title')}</summary>
      <div className="mt-2 space-y-3">
        <p className="text-sm text-muted-foreground">{t('agentFiles.runtime.note')}</p>
        {state.kind === 'loading' && <Skeleton className="h-16" />}
        {state.kind === 'failed' && (
          <p className="text-sm text-muted-foreground" role="status">
            {t('agentFiles.runtime.failed')}
          </p>
        )}
        {state.kind === 'ready' && state.files.length === 0 && (
          <p className="text-sm text-muted-foreground">{t('agentFiles.runtime.none')}</p>
        )}
        {state.kind === 'ready' &&
          state.files.map((f) => (
            <div key={f.name} className="space-y-1" data-testid={`runtime-file-${f.name}`}>
              <p className="font-mono text-[11px] text-muted-foreground">{f.name}</p>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-surface-border bg-muted p-3 font-mono text-xs leading-relaxed">
                {f.content}
              </pre>
              {f.truncated && <p className="text-xs text-muted-foreground">{t('agentFiles.runtime.truncated')}</p>}
            </div>
          ))}
      </div>
    </details>
  );
}
