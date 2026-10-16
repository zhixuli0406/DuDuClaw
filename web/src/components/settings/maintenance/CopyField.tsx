import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Check, Copy } from 'lucide-react';
import { Button } from '@/components/mds';

/**
 * A read-only value (command, fingerprint) with a copy button. The value is
 * shown in a monospace block that wraps, so long fingerprints never overflow
 * a phone-width card. The button's accessible name says what it copies; a
 * polite live region announces the result.
 */
export function CopyField({ value, label }: { value: string; label: string }) {
  const intl = useIntl();
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const timer = useRef<number | null>(null);

  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setState('copied');
    } catch {
      setState('failed');
    }
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setState('idle'), 2000);
  }

  return (
    <div className="flex items-start gap-2">
      <code className="min-w-0 flex-1 break-all rounded-md bg-muted px-2 py-1 font-mono text-xs text-foreground">
        {value}
      </code>
      <Button
        variant="outline"
        size="icon-sm"
        onClick={() => void copy()}
        aria-label={intl.formatMessage({ id: 'device.maintenance.ssh.copy' }, { what: label })}
      >
        {state === 'copied' ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      </Button>
      <span className="sr-only" role="status" aria-live="polite">
        {state === 'copied' && intl.formatMessage({ id: 'device.maintenance.ssh.copied' })}
        {state === 'failed' && intl.formatMessage({ id: 'device.maintenance.ssh.copyFailed' })}
      </span>
    </div>
  );
}
