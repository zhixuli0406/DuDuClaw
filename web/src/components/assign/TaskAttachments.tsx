import { useCallback, useId, useRef, useState, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { Paperclip, X } from 'lucide-react';

import { MAX_ATTACHMENT_BYTES, readAttachment } from '@/lib/attachments';
import type { TaskAttachmentInput } from '@/lib/task-rpc';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import type { PendingAttachment } from '@/stores/chat-store';

/** Most files one task may carry — the gateway's `MAX_TASK_ATTACHMENTS`. */
export const MAX_TASK_FILES = 10;
/** All files of one task together — the gateway's total cap. */
export const MAX_TASK_TOTAL_BYTES = 50 * 1024 * 1024;

function bytesOf(a: PendingAttachment): number {
  // base64 → bytes, close enough for a client-side precheck.
  return Math.floor((a.dataBase64.length * 3) / 4);
}

/** The files picked for one task, with the gateway's limits checked here
 *  first so the person hears about a problem before pressing 交辦. */
export function useTaskAttachments() {
  const intl = useIntl();
  const [files, setFiles] = useState<PendingAttachment[]>([]);
  // Mirrors `files`; every change goes through the three callbacks below,
  // which keep both in step (an async add must count what is already there).
  const filesRef = useRef<PendingAttachment[]>([]);

  const addFiles = useCallback(
    async (list: FileList | readonly File[] | null) => {
      const picked = list ? Array.from(list) : [];
      if (picked.length === 0) return;
      const next = [...filesRef.current];
      let total = next.reduce((n, f) => n + bytesOf(f), 0);
      for (const file of picked) {
        if (next.length >= MAX_TASK_FILES) {
          toast.error(intl.formatMessage({ id: 'assign.attach.tooMany' }, { max: MAX_TASK_FILES }));
          break;
        }
        if (file.size > MAX_ATTACHMENT_BYTES) {
          toast.error(intl.formatMessage({ id: 'assign.attach.tooLarge' }, { name: file.name }));
          continue;
        }
        if (total + file.size > MAX_TASK_TOTAL_BYTES) {
          toast.error(
            intl.formatMessage(
              { id: 'assign.attach.totalTooLarge' },
              { max: MAX_TASK_TOTAL_BYTES / (1024 * 1024) },
            ),
          );
          continue;
        }
        const result = await readAttachment(file);
        if (result.ok) {
          next.push(result.attachment);
          total += file.size;
        } else if (result.reason === 'too-large') {
          toast.error(intl.formatMessage({ id: 'assign.attach.tooLarge' }, { name: result.name }));
        } else {
          toast.error(intl.formatMessage({ id: 'assign.attach.readFailed' }, { name: result.name }));
        }
      }
      filesRef.current = next;
      setFiles(next);
    },
    [intl],
  );

  const remove = useCallback((index: number) => {
    setFiles((prev) => {
      const next = prev.filter((_, i) => i !== index);
      filesRef.current = next;
      return next;
    });
  }, []);

  const reset = useCallback(() => {
    filesRef.current = [];
    setFiles([]);
  }, []);

  return { files, addFiles, remove, reset };
}

/** What the gateway expects for `attachments`; `undefined` when none. */
export function toAttachmentInput(files: readonly PendingAttachment[]): TaskAttachmentInput[] | undefined {
  if (files.length === 0) return undefined;
  return files.map((f) => ({ name: f.name, mime: f.mime, data_base64: f.dataBase64 }));
}

function sizeLabel(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

/** Removable chips for the picked files. */
export function AttachmentChips({
  files,
  onRemove,
  disabled = false,
}: {
  files: readonly PendingAttachment[];
  onRemove: (index: number) => void;
  disabled?: boolean;
}) {
  const intl = useIntl();
  if (files.length === 0) return null;
  return (
    <ul className="flex flex-wrap gap-2" aria-label={intl.formatMessage({ id: 'assign.attach.list' })}>
      {files.map((f, i) => (
        <li
          key={`${f.name}-${i}`}
          className="flex max-w-full items-center gap-1.5 rounded-lg border border-surface-border bg-muted/50 py-1 pr-1 pl-2.5 text-xs text-foreground"
        >
          <Paperclip className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
          <span className="truncate">{f.name}</span>
          <span className="shrink-0 text-muted-foreground">{sizeLabel(bytesOf(f))}</span>
          <button
            type="button"
            onClick={() => onRemove(i)}
            disabled={disabled}
            aria-label={intl.formatMessage({ id: 'assign.attach.remove' }, { name: f.name })}
            className="grid size-7 shrink-0 place-items-center rounded-md text-muted-foreground outline-none transition-colors hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50"
          >
            <X className="size-3.5" />
          </button>
        </li>
      ))}
    </ul>
  );
}

/** Hidden file input + the button or drop zone that opens it. */
function useFilePicker(onFiles: (files: FileList | null) => void) {
  const inputRef = useRef<HTMLInputElement>(null);
  const input = (
    <input
      ref={inputRef}
      type="file"
      multiple
      className="sr-only"
      tabIndex={-1}
      aria-hidden
      onChange={(e) => {
        onFiles(e.target.files);
        e.target.value = '';
      }}
    />
  );
  return { input, open: () => inputRef.current?.click() };
}

/** 「附件」 button for inside the prompt box (複雜任務 tab). */
export function AttachButton({
  onFiles,
  disabled = false,
}: {
  onFiles: (files: FileList | null) => void;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const { input, open } = useFilePicker(onFiles);
  return (
    <>
      {input}
      <button
        type="button"
        onClick={open}
        disabled={disabled}
        className="flex h-9 min-h-[44px] items-center gap-1.5 rounded-lg border border-surface-border px-3 text-sm text-foreground outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 sm:min-h-0"
      >
        <Paperclip className="size-4" aria-hidden />
        {intl.formatMessage({ id: 'assign.attach.button' })}
      </button>
    </>
  );
}

/** Dashed drop zone (一般 tab). */
export function AttachmentDropZone({
  onFiles,
  hint,
  disabled = false,
}: {
  onFiles: (files: FileList | null) => void;
  hint: string;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const hintId = useId();
  const { input, open } = useFilePicker(onFiles);
  const [over, setOver] = useState(false);
  const onDrop = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setOver(false);
    if (!disabled) onFiles(e.dataTransfer.files);
  };
  return (
    <div
      onDragOver={(e) => {
        e.preventDefault();
        if (!disabled) setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={onDrop}
      className={cn(
        'rounded-xl border border-dashed px-4 py-4 text-center transition-colors motion-reduce:transition-none',
        over ? 'border-brand bg-brand/5' : 'border-surface-border',
      )}
    >
      {input}
      <p className="text-sm text-foreground">
        {intl.formatMessage({ id: 'assign.attach.drop' })}
        <button
          type="button"
          onClick={open}
          disabled={disabled}
          aria-describedby={hintId}
          className="inline-flex min-h-[44px] items-center rounded-md px-1 font-medium text-brand underline-offset-2 outline-none hover:underline focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 sm:min-h-0"
        >
          {intl.formatMessage({ id: 'assign.attach.choose' })}
        </button>
      </p>
      <p id={hintId} className="mt-1 text-xs text-muted-foreground">
        {hint}
      </p>
    </div>
  );
}
