import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Eye, Hand, MonitorPlay, PlayCircle, ShieldAlert, Square, Undo2 } from 'lucide-react';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Empty,
  useErrorMessage,
} from '@/components/mds';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  computerSessionApi,
  formatClock,
  sessionPhase,
  type ComputerSessionStatus,
  type ComputerViewTicket,
} from '@/lib/computer-session';
import { ComputerViewer, type ViewerEnd } from './ComputerViewer';

/** How often the status card refreshes while the tab is open. */
const POLL_MS = 5000;

const PHASE_TONE: Record<string, string> = {
  running: 'bg-success/12 text-success',
  paused: 'bg-amber-500/12 text-amber-700 dark:text-amber-300',
  held: 'bg-destructive/10 text-destructive',
  human: 'bg-orange-400/15 text-orange-700 dark:text-orange-300',
  none: 'bg-muted text-muted-foreground',
};

/**
 * The computer tab of an employee: whether its computer-use session runs or
 * is paused, how much of its budget is used, and the human controls (watch,
 * take over / hand back, resume after an injection pause, stop). Every
 * button is only a request: the gateway re-checks the account's role and
 * binding each time and its answer decides.
 */
export function ComputerSessionPanel({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);

  const [status, setStatus] = useState<ComputerSessionStatus | null>(null);
  const [denied, setDenied] = useState(false);
  const [ticket, setTicket] = useState<ComputerViewTicket | null>(null);
  const [ended, setEnded] = useState<ViewerEnd | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmStop, setConfirmStop] = useState(false);
  const [note, setNote] = useState('');

  const refresh = useCallback(async () => {
    try {
      const next = await computerSessionApi.status(agentId);
      setStatus(next);
      setDenied(false);
      if (!next.active) setTicket(null);
    } catch {
      setDenied(true);
    }
  }, [agentId]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  const run = async (fn: () => Promise<unknown>, done?: string) => {
    setBusy(true);
    try {
      await fn();
      if (done) toast.success(t(done));
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  const watch = () =>
    run(async () => {
      setEnded(null);
      setTicket(await computerSessionApi.view(agentId));
    });
  const takeOver = () =>
    run(async () => {
      setEnded(null);
      setTicket(await computerSessionApi.takeover(agentId));
    }, 'computerSession.takeover.done');
  const handBack = () =>
    run(async () => {
      await computerSessionApi.handBack(agentId, note);
      setNote('');
      setTicket(null);
    }, 'computerSession.handBack.done');
  const resume = () => run(() => computerSessionApi.resume(agentId), 'computerSession.resume.done');
  const stop = () =>
    run(async () => {
      setConfirmStop(false);
      setTicket(null);
      await computerSessionApi.stop(agentId);
    }, 'computerSession.stop.done');

  if (denied) {
    return (
      <div className="mx-auto w-full max-w-[1440px] p-4 sm:p-6">
        <Empty icon={MonitorPlay} variant="dashed" title={t('computerSession.denied')} />
      </div>
    );
  }

  const phase = sessionPhase(status);
  const active = status?.active ? status : null;
  const mine = Boolean(active?.takeover?.mine);

  return (
    <div className="mx-auto w-full max-w-[1440px] space-y-4 p-4 sm:p-6">
      <Card>
        <CardHeader>
          <CardTitle className="flex flex-wrap items-center gap-2 text-sm">
            <MonitorPlay className="size-4 text-muted-foreground" aria-hidden="true" />
            {t('computerSession.title')}
            <Badge className={cn('border-0', PHASE_TONE[phase])} data-testid="computer-phase">
              {t(`computerSession.phase.${phase}`)}
            </Badge>
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {!active ? (
            <p className="text-sm text-muted-foreground">{t('computerSession.none')}</p>
          ) : (
            <>
              <dl className="grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
                <Fact label={t('computerSession.fact.actions')}>
                  {active.actions_used}
                  {active.max_actions != null ? ` / ${active.max_actions}` : ''}
                </Fact>
                <Fact label={t('computerSession.fact.timeLeft')}>{formatClock(active.seconds_left)}</Fact>
                <Fact label={t('computerSession.fact.keepAlive')}>
                  {active.keep_alive_minutes
                    ? t('computerSession.fact.keepAliveMinutes', { n: active.keep_alive_minutes })
                    : t('computerSession.fact.keepAliveOff')}
                </Fact>
                <Fact label={t('computerSession.fact.viewers')}>{active.viewers}</Fact>
              </dl>

              {active.state === 'paused' && (
                <p className="text-sm text-muted-foreground">
                  {t('computerSession.pausedNote', { left: formatClock(active.paused_seconds_left) })}
                </p>
              )}

              {active.hold && (
                <div
                  role="alert"
                  className="flex flex-wrap items-start gap-3 rounded-xl border border-destructive/30 bg-destructive/5 p-3 text-sm"
                >
                  <ShieldAlert className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden="true" />
                  <div className="min-w-0 flex-1 space-y-1">
                    <p className="font-medium text-foreground">{t('computerSession.hold.title')}</p>
                    <p className="text-muted-foreground">
                      {t('computerSession.hold.body', { categories: active.hold.categories.join(', ') })}
                    </p>
                  </div>
                  <Button size="sm" variant="outline" disabled={busy} onClick={resume}>
                    <PlayCircle />
                    {t('computerSession.resume')}
                  </Button>
                </div>
              )}

              {active.takeover && (
                <div className="flex flex-wrap items-center gap-3 rounded-xl border border-orange-400/30 bg-orange-400/8 p-3 text-sm">
                  <Hand className="size-4 shrink-0 text-orange-600 dark:text-orange-300" aria-hidden="true" />
                  <p className="min-w-0 flex-1 text-foreground">
                    {mine
                      ? t('computerSession.takeover.mine', { left: formatClock(active.takeover.idle_seconds_left) })
                      : t('computerSession.takeover.other', { holder: active.takeover.holder })}
                  </p>
                </div>
              )}

              <div className="flex flex-wrap items-center gap-2">
                <Button size="sm" variant="outline" disabled={busy || !active.stream_available} onClick={watch}>
                  <Eye />
                  {t(ticket ? 'computerSession.reconnect' : 'computerSession.watch')}
                </Button>
                {!active.takeover && (
                  <Button size="sm" variant="brand" disabled={busy || !active.stream_available} onClick={takeOver}>
                    <Hand />
                    {t('computerSession.takeover')}
                  </Button>
                )}
                {mine && (
                  <>
                    <input
                      value={note}
                      onChange={(e) => setNote(e.target.value)}
                      maxLength={200}
                      aria-label={t('computerSession.handBack.note')}
                      placeholder={t('computerSession.handBack.note')}
                      className="h-7 min-w-0 flex-1 rounded-lg border border-input bg-background px-2.5 text-sm sm:max-w-xs"
                    />
                    <Button size="sm" variant="brandSubtle" disabled={busy} onClick={handBack}>
                      <Undo2 />
                      {t('computerSession.handBack')}
                    </Button>
                  </>
                )}
                <span className="flex-1" />
                {confirmStop ? (
                  <>
                    <span className="text-sm text-muted-foreground">{t('computerSession.stop.confirm')}</span>
                    <Button size="sm" variant="destructive" disabled={busy} onClick={stop}>
                      {t('computerSession.stop.yes')}
                    </Button>
                    <Button size="sm" variant="ghost" onClick={() => setConfirmStop(false)}>
                      {t('computerSession.stop.no')}
                    </Button>
                  </>
                ) : (
                  <Button size="sm" variant="ghost" disabled={busy} onClick={() => setConfirmStop(true)}>
                    <Square />
                    {t('computerSession.stop')}
                  </Button>
                )}
              </div>
              {!active.stream_available && (
                <p className="text-xs text-muted-foreground">{t('computerSession.noStream')}</p>
              )}
            </>
          )}
        </CardContent>
      </Card>

      {active && ticket && !ended && (
        <ComputerViewer
          ticket={ticket}
          onEnd={(why) => {
            setEnded(why);
            setTicket(null);
          }}
        />
      )}
      {active && ended && (
        <p className="text-sm text-muted-foreground" role="status">
          {t(`computerSession.viewer.ended.${ended}`)}
        </p>
      )}
      <p className="text-xs text-muted-foreground">{t('computerSession.footnote')}</p>
    </div>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="space-y-0.5">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="font-mono tabular-nums text-foreground">{children}</dd>
    </div>
  );
}
