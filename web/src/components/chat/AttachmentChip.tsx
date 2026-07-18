import { X, FileText, Image as ImageIcon } from 'lucide-react';
import { isImageMime } from '@/lib/attachments';

/**
 * A compact chip representing one attached file — used both in the composer
 * (with a remove button) and inside message bubbles (read-only).
 */
export function AttachmentChip({
  name,
  mime,
  onRemove,
}: {
  name: string;
  mime: string;
  onRemove?: () => void;
}) {
  const Icon = isImageMime(mime) ? ImageIcon : FileText;
  return (
    <span className="inline-flex max-w-[12rem] items-center gap-1.5 rounded-lg border border-surface-border bg-surface px-2 py-1 text-xs text-foreground">
      <Icon className="h-3.5 w-3.5 flex-shrink-0" />
      <span className="truncate">{name}</span>
      {onRemove && (
        <button
          onClick={onRemove}
          className="flex-shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          aria-label="Remove attachment"
        >
          <X className="h-3 w-3" />
        </button>
      )}
    </span>
  );
}
