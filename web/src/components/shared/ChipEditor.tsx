import { useState, type KeyboardEvent } from 'react';
import { X, Plus } from 'lucide-react';
import { cn } from '@/lib/utils';
import { inputClass } from './Dialog';

interface ChipEditorProps {
  /** Current list of values (immutable — parent owns state). */
  values: ReadonlyArray<string>;
  /** Called with a brand-new array whenever the list changes. */
  onChange: (next: string[]) => void;
  placeholder?: string;
  /** Add-button label (i18n string), defaults to a `+` icon only. */
  addLabel?: string;
}

/**
 * A small tag/chip editor: shows existing values as removable chips and an
 * input + add button to append new ones. Trims input, ignores empties and
 * duplicates. Always produces a fresh array (immutability convention).
 */
export function ChipEditor({ values, onChange, placeholder, addLabel }: ChipEditorProps) {
  const [draft, setDraft] = useState('');

  const add = () => {
    const v = draft.trim();
    if (!v || values.includes(v)) {
      setDraft('');
      return;
    }
    onChange([...values, v]);
    setDraft('');
  };

  const remove = (idx: number) => {
    onChange(values.filter((_, i) => i !== idx));
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      add();
    }
  };

  return (
    <div className="space-y-2">
      {values.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {values.map((v, idx) => (
            <span
              key={`${v}-${idx}`}
              className="inline-flex items-center gap-1 rounded-full bg-amber-100 px-2.5 py-0.5 text-xs font-medium text-amber-700 dark:bg-amber-900/30 dark:text-amber-400"
            >
              {v}
              <button
                type="button"
                onClick={() => remove(idx)}
                className="rounded-full p-0.5 hover:bg-amber-200/60 dark:hover:bg-amber-800/40"
                aria-label={`remove ${v}`}
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        <input
          type="text"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          className={cn(inputClass, 'flex-1')}
        />
        <button
          type="button"
          onClick={add}
          className="inline-flex shrink-0 items-center gap-1 rounded-lg border border-stone-300/70 bg-white/50 px-3 py-2 text-sm font-medium text-stone-700 backdrop-blur transition-colors hover:bg-white/80 dark:border-white/10 dark:bg-white/5 dark:text-stone-300 dark:hover:bg-white/10"
        >
          <Plus className="h-4 w-4" />
          {addLabel}
        </button>
      </div>
    </div>
  );
}
