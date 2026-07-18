import { useState, type KeyboardEvent } from 'react';
import { X, Plus } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/mds';
import { inputClass } from './controlClass';

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
              className="inline-flex items-center gap-1 rounded-full bg-brand/12 px-2.5 py-0.5 text-xs font-medium text-brand"
            >
              {v}
              <button
                type="button"
                onClick={() => remove(idx)}
                className="rounded-full p-0.5 hover:bg-brand/20"
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
        <Button type="button" variant="outline" onClick={add} className="shrink-0">
          <Plus className="h-4 w-4" />
          {addLabel}
        </Button>
      </div>
    </div>
  );
}
