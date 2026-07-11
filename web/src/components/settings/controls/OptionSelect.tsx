import { cn } from '@/lib/utils';
import { controlClass } from '@/components/ui';

export interface SelectOption {
  value: string;
  /** Plain-language label shown to the user. */
  label: string;
  /** Raw technical value shown in grey after the label. Defaults to `value`. */
  raw?: string;
}

/**
 * OptionSelect — a <select> that shows a plain-language label plus the raw
 * technical value (e.g. "優先序（依序使用） · priority"), so non-technical users
 * understand the choice while power users still see what actually gets written
 * to config. Native <option> can't style two colours, so the raw value is
 * appended after a middle dot unless `showRaw={false}`.
 */
export function OptionSelect({
  value,
  onChange,
  options,
  showRaw = true,
  disabled,
  className,
  id,
}: {
  value: string;
  onChange: (value: string) => void;
  options: readonly SelectOption[];
  showRaw?: boolean;
  disabled?: boolean;
  className?: string;
  id?: string;
}) {
  return (
    <select
      id={id}
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value)}
      className={cn(controlClass, className)}
    >
      {options.map((opt) => {
        const raw = opt.raw ?? opt.value;
        const text = showRaw && raw && raw !== opt.label ? `${opt.label} · ${raw}` : opt.label;
        return (
          <option key={opt.value} value={opt.value}>
            {text}
          </option>
        );
      })}
    </select>
  );
}
