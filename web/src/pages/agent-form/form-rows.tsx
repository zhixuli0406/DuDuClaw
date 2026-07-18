import { type ReactNode } from 'react';
import { cn } from '@/lib/utils';
import {
  Input,
  Switch,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  SettingsRow,
  type SettingsRowTier,
} from '@/components/mds';
import type { SelectOption } from '@/components/settings/controls';

/**
 * Agent-form row helpers (WP2.3) — thin wrappers that place a labelled control
 * inside an mds `SettingsRow` (label left, control right). They swap the legacy
 * Calm-Glass inputs for mds primitives (Input / Switch / Select) without
 * touching the shared `components/settings/*` controls that the manage system
 * pages still depend on.
 */

/** Single-line text field. */
export function RowText({
  label,
  description,
  value,
  onChange,
  placeholder,
  type = 'text',
  autoComplete,
  tier = 'text',
}: {
  label: ReactNode;
  description?: ReactNode;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: 'text' | 'password';
  autoComplete?: string;
  tier?: SettingsRowTier;
}) {
  return (
    <SettingsRow label={label} description={description} tier={tier}>
      <Input
        type={type}
        value={value}
        placeholder={placeholder}
        autoComplete={autoComplete}
        onChange={(e) => onChange(e.target.value)}
      />
    </SettingsRow>
  );
}

/** Numeric field with optional bounds. */
export function RowNumber({
  label,
  description,
  value,
  onChange,
  min,
  max,
  step,
  tier = 'select',
}: {
  label: ReactNode;
  description?: ReactNode;
  value: number;
  onChange: (v: number) => void;
  min?: number;
  max?: number;
  step?: number;
  tier?: SettingsRowTier;
}) {
  return (
    <SettingsRow label={label} description={description} tier={tier}>
      <Input
        type="number"
        value={value}
        min={min}
        max={max}
        step={step}
        onChange={(e) => onChange(Number(e.target.value))}
      />
    </SettingsRow>
  );
}

/** On/off toggle. */
export function RowSwitch({
  label,
  description,
  checked,
  onChange,
  disabled,
}: {
  label: ReactNode;
  description?: ReactNode;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <SettingsRow label={label} description={description}>
      <Switch
        checked={checked}
        onCheckedChange={(v) => onChange(Boolean(v))}
        disabled={disabled}
        aria-label={typeof label === 'string' ? label : undefined}
      />
    </SettingsRow>
  );
}

/** Enum dropdown driven by `SelectOption[]`. */
export function RowSelect({
  label,
  description,
  value,
  onChange,
  options,
  disabled,
  tier = 'select-wide',
}: {
  label: ReactNode;
  description?: ReactNode;
  value: string;
  onChange: (v: string) => void;
  options: readonly SelectOption[];
  disabled?: boolean;
  tier?: SettingsRowTier;
}) {
  const current = options.find((o) => o.value === value);
  return (
    <SettingsRow label={label} description={description} tier={tier}>
      <Select
        value={value}
        onValueChange={(v) => onChange(String(v))}
        disabled={disabled}
      >
        <SelectTrigger
          className="w-full"
          aria-label={typeof label === 'string' ? label : undefined}
        >
          <SelectValue>{current?.label}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {options.map((o) => (
            <SelectItem key={o.value} value={o.value}>
              {o.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </SettingsRow>
  );
}

/**
 * FieldBlock — a stacked label + full-width control, for wide/complex editors
 * (chip lists, tables, policy builder, textarea) that don't fit the side-by-side
 * SettingsRow. Lives inside a SettingsSection, not a SettingsCard.
 */
export function FieldBlock({
  label,
  description,
  children,
  className,
}: {
  label?: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('space-y-1.5', className)}>
      {(label || description) && (
        <div className="space-y-0.5">
          {label && <div className="text-sm font-medium">{label}</div>}
          {description && (
            <div className="text-xs text-muted-foreground">{description}</div>
          )}
        </div>
      )}
      {children}
    </div>
  );
}
