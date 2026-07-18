import type { ComponentType } from 'react';
import { ClipboardCheck, Package, Lightbulb, Ban, Wallet, AlertTriangle } from 'lucide-react';
import type { InboxItemType } from '@/lib/inbox-model';

export type BadgeTone = 'neutral' | 'info' | 'warning' | 'accent' | 'danger' | 'success';

export interface InboxTypeMeta {
  icon: ComponentType<{ className?: string }>;
  tone: BadgeTone;
  /** i18n key for the type label. */
  labelKey: string;
}

/** One place mapping each source type to its icon / tone / label key. */
export const TYPE_META: Record<InboxItemType, InboxTypeMeta> = {
  approval: { icon: ClipboardCheck, tone: 'accent', labelKey: 'inbox.type.approval' },
  install: { icon: Package, tone: 'accent', labelKey: 'inbox.type.install' },
  decision: { icon: Lightbulb, tone: 'info', labelKey: 'inbox.type.decision' },
  blocked: { icon: Ban, tone: 'danger', labelKey: 'inbox.type.blocked' },
  budget: { icon: Wallet, tone: 'warning', labelKey: 'inbox.type.budget' },
  failed_run: { icon: AlertTriangle, tone: 'danger', labelKey: 'inbox.type.failed_run' },
};
