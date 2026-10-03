import { useIntl } from 'react-intl';
import { Input } from '@/components/mds';

/**
 * v1.68 (W2): `mcp.list` answers env values as `set` / `not_set` /
 * `reference`, never the value. Those words must never be sent back as if
 * they were values (an install would store the literal `not_set` as an API
 * key), and every name in an item's `required_env` is typed by the operator
 * at install time.
 */
export const MASKED_ENV_WORDS: readonly string[] = ['set', 'not_set', 'reference'];

export function isMaskedEnvWord(value: string): boolean {
  return MASKED_ENV_WORDS.includes(value);
}

/** Env entries safe to echo back: everything except the masked status words. */
export function unmaskedEnv(env: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(env).filter(([, v]) => !isMaskedEnvWord(v)));
}

/** Required names the operator has not filled in yet. */
export function missingRequiredEnv(required: readonly string[], values: Record<string, string>): string[] {
  return required.filter((name) => (values[name] ?? '').trim() === '');
}

/** `{NAME: value}` for the required names, trimmed. */
export function requiredEnvPayload(required: readonly string[], values: Record<string, string>): Record<string, string> {
  return Object.fromEntries(required.map((name) => [name, (values[name] ?? '').trim()]));
}

/** One password input per required env name. The caller owns the values and
 *  clears them as soon as the install finishes or the dialog closes. */
export function RequiredEnvFields({
  required,
  values,
  onChange,
}: {
  required: readonly string[];
  values: Record<string, string>;
  onChange: (next: Record<string, string>) => void;
}) {
  const intl = useIntl();
  if (required.length === 0) return null;
  return (
    <div className="space-y-2" data-testid="required-env-fields">
      <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.requiredEnv.help' })}</p>
      {required.map((name) => (
        <div key={name} className="space-y-1">
          <label className="font-mono text-xs font-medium text-muted-foreground" htmlFor={`required-env-${name}`}>
            {name}
          </label>
          <Input
            id={`required-env-${name}`}
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={values[name] ?? ''}
            aria-label={name}
            onChange={(e) => onChange({ ...values, [name]: e.target.value })}
          />
        </div>
      ))}
    </div>
  );
}
