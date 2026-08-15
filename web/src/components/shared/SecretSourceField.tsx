import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/mds';

export type SecretSourceMode = 'direct' | 'env' | 'keychain' | 'file' | 'manager';

export interface ParsedSecretSource {
  mode: SecretSourceMode;
  envName: string;
  keychainService: string;
  keychainAccount: string;
  filePath: string;
  /** The literal value for `direct` (plaintext) and `manager` (raw `secret://…` URI) modes. */
  raw: string;
}

const EMPTY_FIELDS: Omit<ParsedSecretSource, 'mode'> = {
  envName: '',
  keychainService: '',
  keychainAccount: '',
  filePath: '',
  raw: '',
};

/**
 * Reverse-parse a stored field value into the mode + guided sub-fields that
 * produced it. Mirrors the backend's `SecretUri::parse`
 * (`crates/duduclaw-security/src/secret_manager/mod.rs`) closely enough to
 * round-trip every shape `SecretRef::classify` accepts, but this is pure
 * front-end string matching — it never validates against the backend.
 *
 * Anything not shaped like `secret://…` is `direct`: the plaintext-in-config
 * path the doctrine calls "legacy" but still fully supports.
 */
export function parseSecretSource(value: string): ParsedSecretSource {
  if (value.startsWith('secret://env/')) {
    return { ...EMPTY_FIELDS, mode: 'env', envName: value.slice('secret://env/'.length) };
  }
  if (value.startsWith('secret://keychain/')) {
    const rest = value.slice('secret://keychain/'.length);
    const slash = rest.indexOf('/');
    return slash === -1
      ? { ...EMPTY_FIELDS, mode: 'keychain', keychainAccount: rest }
      : {
          ...EMPTY_FIELDS,
          mode: 'keychain',
          keychainService: rest.slice(0, slash),
          keychainAccount: rest.slice(slash + 1),
        };
  }
  if (value.startsWith('secret://file/')) {
    return { ...EMPTY_FIELDS, mode: 'file', filePath: value.slice('secret://file/'.length) };
  }
  if (value.startsWith('secret://')) {
    // vault / onepassword / infisical / local — backend-specific parameters
    // this component doesn't model; kept as the raw URI (§ manager mode).
    return { ...EMPTY_FIELDS, mode: 'manager', raw: value };
  }
  return { ...EMPTY_FIELDS, mode: 'direct', raw: value };
}

/**
 * Compose the stored field value from a mode + its guided sub-fields. Pure
 * string-building — never calls the backend. The three guided dialects
 * (`env` / `keychain` / `file`) are exactly what `SecretRef::classify`
 * already parses, so nothing on the write path changes.
 */
export function composeSecretSource(
  mode: SecretSourceMode,
  fields: Partial<Omit<ParsedSecretSource, 'mode'>>,
): string {
  switch (mode) {
    case 'env': {
      const name = (fields.envName ?? '').trim();
      return name ? `secret://env/${name}` : '';
    }
    case 'keychain': {
      const account = (fields.keychainAccount ?? '').trim();
      if (!account) return '';
      const service = (fields.keychainService ?? '').trim();
      return service ? `secret://keychain/${service}/${account}` : `secret://keychain/${account}`;
    }
    case 'file': {
      const path = (fields.filePath ?? '').trim();
      return path ? `secret://file/${path}` : '';
    }
    case 'manager':
    case 'direct':
    default:
      return fields.raw ?? '';
  }
}

export interface SecretSourceFieldProps {
  /** Current stored value — plaintext, or a `secret://…` reference. */
  value: string;
  onChange: (value: string) => void;
  /** Placeholder for the "type it in" mode only (e.g. "Paste your Bot Token..."). */
  placeholder?: string;
  /** Input type for the "type it in" mode's field. Guided modes always render
   *  plain text — a variable name / service·account / path / URI is a
   *  reference, not the secret itself, so masking it buys nothing. */
  type?: 'password' | 'text';
  autoComplete?: string;
  disabled?: boolean;
  className?: string;
  id?: string;
}

/**
 * SecretSourceField (WP-5C) — a credential input that makes the `secret://`
 * dashboard-only affordance discoverable instead of requiring the operator to
 * already know the URI grammar. A small source picker next to the field
 * switches between typing the value directly and three guided reference
 * builders (environment variable / OS keychain / mounted file); an existing
 * `secret://…` value reverse-parses back into whichever mode produced it on
 * mount. Vault / 1Password / Infisical need backend-specific parameters this
 * component doesn't model, so "secrets manager" mode is a raw `secret://`
 * input with an example placeholder instead of a guided form.
 *
 * Switching modes clears the field rather than carrying over a value that no
 * longer means what it looks like — a leftover "direct" string is not a
 * meaningful env var name, and vice versa.
 */
export function SecretSourceField({
  value,
  onChange,
  placeholder,
  type = 'password',
  autoComplete = 'off',
  disabled,
  className,
  id,
}: SecretSourceFieldProps) {
  const intl = useIntl();
  const t = (msgId: string) => intl.formatMessage({ id: msgId });

  const [state, setState] = useState<ParsedSecretSource>(() => parseSecretSource(value));

  // Tracks the last value we ourselves emitted, so an external `value` change
  // (e.g. the page reloading config after Save) re-syncs the guided fields,
  // while our own onChange round-tripping back through the parent's state
  // does not fight the operator mid-keystroke.
  const lastEmitted = useRef(value);
  useEffect(() => {
    if (value === lastEmitted.current) return;
    setState(parseSecretSource(value));
    lastEmitted.current = value;
  }, [value]);

  const emit = (next: string) => {
    lastEmitted.current = next;
    onChange(next);
  };

  const handleModeChange = (nextMode: SecretSourceMode) => {
    const next: ParsedSecretSource = { ...EMPTY_FIELDS, mode: nextMode };
    setState(next);
    emit('');
  };

  const updateField = (key: keyof Omit<ParsedSecretSource, 'mode'>, fieldValue: string) => {
    const next = { ...state, [key]: fieldValue };
    setState(next);
    emit(composeSecretSource(next.mode, next));
  };

  return (
    <div className={cn('space-y-1.5', className)} data-slot="secret-source-field">
      <Select value={state.mode} onValueChange={(v) => handleModeChange(v as SecretSourceMode)} disabled={disabled}>
        <SelectTrigger className="w-full" aria-label={t('secretSource.mode.ariaLabel')}>
          <SelectValue>{t(`secretSource.mode.${state.mode}`)}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="direct">{t('secretSource.mode.direct')}</SelectItem>
          <SelectItem value="env">{t('secretSource.mode.env')}</SelectItem>
          <SelectItem value="keychain">{t('secretSource.mode.keychain')}</SelectItem>
          <SelectItem value="file">{t('secretSource.mode.file')}</SelectItem>
          <SelectItem value="manager">{t('secretSource.mode.manager')}</SelectItem>
        </SelectContent>
      </Select>

      {state.mode === 'direct' && (
        <Input
          id={id}
          type={type}
          value={state.raw}
          placeholder={placeholder}
          autoComplete={autoComplete}
          disabled={disabled}
          onChange={(e) => updateField('raw', e.target.value)}
        />
      )}

      {state.mode === 'env' && (
        <>
          <Input
            id={id}
            type="text"
            value={state.envName}
            placeholder={t('secretSource.env.placeholder')}
            autoComplete="off"
            disabled={disabled}
            onChange={(e) => updateField('envName', e.target.value)}
          />
          <p className="text-xs text-muted-foreground">{t('secretSource.env.hint')}</p>
        </>
      )}

      {state.mode === 'keychain' && (
        <>
          <div className="grid grid-cols-2 gap-2">
            <Input
              type="text"
              value={state.keychainService}
              placeholder={t('secretSource.keychain.service.placeholder')}
              autoComplete="off"
              disabled={disabled}
              aria-label={t('secretSource.keychain.service.label')}
              onChange={(e) => updateField('keychainService', e.target.value)}
            />
            <Input
              id={id}
              type="text"
              value={state.keychainAccount}
              placeholder={t('secretSource.keychain.account.placeholder')}
              autoComplete="off"
              disabled={disabled}
              aria-label={t('secretSource.keychain.account.label')}
              onChange={(e) => updateField('keychainAccount', e.target.value)}
            />
          </div>
          <p className="text-xs text-muted-foreground">{t('secretSource.keychain.hint')}</p>
        </>
      )}

      {state.mode === 'file' && (
        <>
          <Input
            id={id}
            type="text"
            value={state.filePath}
            placeholder={t('secretSource.file.placeholder')}
            autoComplete="off"
            disabled={disabled}
            onChange={(e) => updateField('filePath', e.target.value)}
          />
          <p className="text-xs text-muted-foreground">{t('secretSource.file.hint')}</p>
        </>
      )}

      {state.mode === 'manager' && (
        <>
          <Input
            id={id}
            type="text"
            value={state.raw}
            placeholder={t('secretSource.manager.placeholder')}
            autoComplete="off"
            disabled={disabled}
            onChange={(e) => updateField('raw', e.target.value)}
          />
          <p className="text-xs text-muted-foreground">{t('secretSource.manager.hint')}</p>
        </>
      )}
    </div>
  );
}
