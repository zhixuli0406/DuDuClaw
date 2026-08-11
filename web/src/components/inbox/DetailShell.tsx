import type { ComponentType, ReactNode } from 'react';
import { ActorAvatar, Badge } from '@/components/mds';

/**
 * DetailShell — shared header scaffold for Inbox detail-pane bodies that
 * aren't a full custom layout (approvals and installs build their own):
 * leading avatar (or a type glyph when there's no owning AI staff), a type
 * badge + optional staff name, the title, then free-form children.
 *
 * Decoupled from `InboxItem` on purpose (W1-2) — `NeedsHumanTaskPanel` needs
 * this exact shell for a `TaskInfo`, which isn't an `InboxItem`.
 */
export function DetailShell({
  icon: Icon,
  title,
  typeLabel,
  agentId,
  agentName,
  children,
}: {
  icon: ComponentType<{ className?: string }>;
  title: string;
  typeLabel: string;
  agentId?: string;
  agentName?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-4">
      <div className="flex items-start gap-3">
        {agentId ? (
          <ActorAvatar actorType="agent" size="lg" name={agentName ?? agentId} />
        ) : (
          <span className="grid size-8 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground ring-1 ring-surface-border">
            <Icon className="size-4" />
          </span>
        )}
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex items-center gap-2">
            <Badge variant="secondary">{typeLabel}</Badge>
            {agentName && <span className="truncate text-xs text-muted-foreground">{agentName}</span>}
          </div>
          <h2 className="text-lg font-medium text-foreground">{title}</h2>
        </div>
      </div>
      {children}
    </div>
  );
}
