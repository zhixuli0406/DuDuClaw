import { useState } from 'react';
import { useIntl } from 'react-intl';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { Button } from '@/components/mds';
import { api, type MaintenanceAuditEvent } from '@/lib/api';
import {
  auditKindMessageId,
  closeTriggerMessageId,
  enableErrorMessage,
} from '@/lib/maintenance-ssh';

const HISTORY_LIMIT = 30;

const str = (v: unknown): string | null => (typeof v === 'string' && v.trim() !== '' ? v : null);
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

/**
 * The maintenance history (design §7.1: every window's key fingerprint is
 * listed). Loaded on demand when expanded; newest first. Strings that came
 * from the device (sources, menu actions) are rendered as plain text.
 */
export function MaintenanceHistory() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [open, setOpen] = useState(false);
  const [events, setEvents] = useState<MaintenanceAuditEvent[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(false);

  async function load() {
    setLoading(true);
    try {
      const res = await api.maintenance.history(HISTORY_LIMIT);
      setEvents(res.events);
      setFailed(false);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  }

  function toggle() {
    const next = !open;
    setOpen(next);
    if (next) void load();
  }

  function details(ev: MaintenanceAuditEvent): string[] {
    const out: string[] = [];
    const who = str(ev.enabled_by_email) ?? str(ev.email) ?? str(ev.viewed_by_email);
    if (who) out.push(who);
    const fp = str(ev.key_fingerprint);
    if (fp) out.push(t('device.maintenance.history.detail.key', { fp }));
    const source = str(ev.source);
    if (source) out.push(t('device.maintenance.history.detail.source', { source }));
    const count = num(ev.count);
    if (ev.kind === 'ssh_auth_failed' && count !== null) {
      out.push(t('device.maintenance.history.detail.count', { n: count }));
    }
    const trigger = str(ev.os_trigger) ?? (ev.kind === 'ssh_os_close_record' ? str(ev.trigger) : null);
    if (trigger) out.push(t(closeTriggerMessageId(trigger)));
    if (ev.events_lost === true) out.push(t('device.maintenance.history.detail.eventsLost'));
    const message = str(ev.message);
    if (ev.kind === 'ssh_menu_action' && message) out.push(message);
    if (ev.kind === 'enable_failed' && str(ev.code)) {
      const ref = enableErrorMessage({ code: ev.code as string, reason: str(ev.reason) ?? undefined });
      if (ref) out.push(t(ref.id, ref.values));
    }
    if (str(ev.close_error)) out.push(t('device.maintenance.history.detail.closeUnconfirmed'));
    return out;
  }

  return (
    <div className="space-y-2">
      <Button
        variant="ghost"
        size="sm"
        onClick={toggle}
        aria-expanded={open}
        aria-controls="maintenance-history-list"
        className="-ml-2"
      >
        {open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />}
        {t('device.maintenance.history.title')}
      </Button>
      {open && (
        <div id="maintenance-history-list" aria-busy={loading} className="space-y-2">
          {failed && <p className="text-xs text-destructive">{t('device.maintenance.history.loadFailed')}</p>}
          {events && events.length === 0 && (
            <p className="text-xs text-muted-foreground">{t('device.maintenance.history.empty')}</p>
          )}
          {events && events.length > 0 && (
            <ol className="space-y-1.5">
              {events.map((ev, i) => {
                const d = details(ev);
                return (
                <li key={`${ev.ts_ms}-${ev.kind}-${i}`} className="text-xs">
                  <span className="text-muted-foreground">
                    {intl.formatDate(ev.ts_ms, {
                      month: 'short',
                      day: 'numeric',
                      hour: '2-digit',
                      minute: '2-digit',
                    })}
                  </span>{' '}
                  <span className="font-medium text-foreground">{t(auditKindMessageId(ev.kind))}</span>
                  {d.length > 0 && <span className="block break-all text-muted-foreground">{d.join(' · ')}</span>}
                </li>
                );
              })}
            </ol>
          )}
        </div>
      )}
    </div>
  );
}
