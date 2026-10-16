import { useIntl } from 'react-intl';
import { Textarea } from '@/components/mds';
import { keyReasonMessageId, type KeyReason } from '@/lib/maintenance-ssh';

/**
 * The technician's public key, shown only while the SSH option is ticked
 * (design §7.1). `hint` is the client pre-check result; the parent clears the
 * field itself when a private key is pasted and passes `private_key` here so
 * the warning stays visible over the now-empty box. The hint never repeats
 * the input.
 */
export function SshKeyField({
  value,
  onChange,
  hint,
  showEmptyHint,
}: {
  value: string;
  onChange: (next: string) => void;
  hint: KeyReason | null;
  /** Show "paste a key" for an empty box only after the person tried to submit. */
  showEmptyHint: boolean;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const visibleHint = hint === 'empty' && !showEmptyHint ? null : hint;
  const hintId = 'maintenance-ssh-key-hint';
  const helpId = 'maintenance-ssh-key-help';

  return (
    <div className="space-y-1.5 pl-6">
      <label className="text-xs font-medium text-muted-foreground" htmlFor="maintenance-ssh-key">
        {t('device.maintenance.ssh.key.label')}
      </label>
      <Textarea
        id="maintenance-ssh-key"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={3}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        spellCheck={false}
        placeholder="ssh-ed25519 AAAA…"
        aria-invalid={visibleHint !== null}
        aria-describedby={visibleHint ? `${helpId} ${hintId}` : helpId}
        className="font-mono text-xs md:text-xs"
      />
      <p id={helpId} className="text-xs text-muted-foreground">
        {t('device.maintenance.ssh.key.help')}
      </p>
      <p id={hintId} className="text-xs text-destructive" role="status" aria-live="polite">
        {visibleHint ? t(keyReasonMessageId(visibleHint)) : ''}
      </p>
    </div>
  );
}
