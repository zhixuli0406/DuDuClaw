import { useState, type KeyboardEvent } from 'react';
import { X, Trash2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button, Input } from '@/components/mds';
import { addToken, removeToken, type KvRow } from './redactionFieldRules';

/**
 * RedactionTokenInput — small multi-value token field shared by the field
 * rules and data sources cards (field tokens / exclude_keys / tool names /
 * allowed_tables). Canvas screens 3/4/8 all use the same `.tokens` input
 * shape: chips + a trailing free-text caret, Enter commits a token.
 *
 * `*` renders in the warning tone (canvas: the wildcard "all fields, keep
 * id" shorthand) — every caller that allows `*` should also render
 * `wildcardHint` below when the wildcard is present.
 */
export function RedactionTokenInput({
  tokens,
  onChange,
  placeholder,
  ariaLabel,
  className,
  invalid,
  isTokenValid,
  mono = true,
}: {
  tokens: readonly string[];
  onChange: (next: string[]) => void;
  placeholder?: string;
  ariaLabel?: string;
  className?: string;
  invalid?: boolean;
  /** Optional per-token validity check — an invalid token renders destructive. */
  isTokenValid?: (token: string) => boolean;
  /** Chips render `font-mono` by default (identifiers/tool names). Pass
   *  `false` for a `free_form_names` (§14.3) field — `--font-mono` has no
   *  CJK fallback by design (DESIGN.md §1.5), so a free-form chip (`地址`,
   *  `客戶清單.xlsx`) should render in the default sans stack instead. */
  mono?: boolean;
}) {
  const [draft, setDraft] = useState('');

  const commit = () => {
    if (!draft.trim()) return;
    onChange(addToken(tokens, draft));
    setDraft('');
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commit();
    } else if (e.key === 'Backspace' && draft === '' && tokens.length > 0) {
      onChange(tokens.slice(0, -1));
    }
  };

  return (
    <div
      className={cn(
        'flex min-h-8 flex-wrap items-center gap-1.5 rounded-lg border bg-transparent px-2 py-1.5',
        invalid ? 'border-destructive' : 'border-input',
        'focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50',
        className,
      )}
    >
      {tokens.map((t) => {
        const wild = t === '*';
        const bad = isTokenValid ? !isTokenValid(t) : false;
        return (
          <span
            key={t}
            className={cn(
              'inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-xs',
              mono && 'font-mono',
              bad
                ? 'bg-destructive/10 text-destructive'
                : wild
                  ? 'bg-warning/10 text-warning'
                  : 'bg-brand/10 text-brand',
            )}
          >
            {t}
            <button
              type="button"
              onClick={() => onChange(removeToken(tokens, t))}
              aria-label={ariaLabel ? `${ariaLabel}: ${t}` : t}
              className="opacity-60 outline-none hover:opacity-100 focus-visible:opacity-100"
            >
              <X className="size-3" />
            </button>
          </span>
        );
      })}
      <input
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={handleKeyDown}
        onBlur={commit}
        placeholder={tokens.length === 0 ? placeholder : undefined}
        aria-label={ariaLabel}
        className="min-w-24 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
      />
    </div>
  );
}

/** One `key = value` row with a remove button — shared by the field-rule
 *  form's match_args, the dry-run panel's args, and the data-sources card's
 *  key_alias rows. */
export function KvRowEditor({
  row,
  onChange,
  onRemove,
  keyPlaceholder,
  valuePlaceholder,
}: {
  row: KvRow;
  onChange: (patch: Partial<KvRow>) => void;
  onRemove: () => void;
  keyPlaceholder: string;
  valuePlaceholder: string;
}) {
  return (
    <div className="flex items-center gap-1.5">
      <Input value={row.key} onChange={(e) => onChange({ key: e.target.value })} placeholder={keyPlaceholder} className="font-mono" />
      <span className="text-xs text-muted-foreground">=</span>
      <Input value={row.value} onChange={(e) => onChange({ value: e.target.value })} placeholder={valuePlaceholder} className="font-mono" />
      <Button variant="ghost" size="icon-xs" onClick={onRemove}><Trash2 /></Button>
    </div>
  );
}
