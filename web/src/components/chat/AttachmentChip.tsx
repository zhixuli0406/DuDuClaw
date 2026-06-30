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
    <span className="inline-flex max-w-[12rem] items-center gap-1.5 rounded-lg border border-[var(--panel-border)] bg-[var(--panel-fill)] px-2 py-1 text-xs text-stone-700 dark:text-stone-200">
      <Icon className="h-3.5 w-3.5 flex-shrink-0" />
      <span className="truncate">{name}</span>
      {onRemove && (
        <button
          onClick={onRemove}
          className="flex-shrink-0 rounded p-0.5 text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-600 dark:hover:bg-white/5 dark:hover:text-stone-300"
          aria-label="Remove attachment"
        >
          <X className="h-3 w-3" />
        </button>
      )}
    </span>
  );
}
