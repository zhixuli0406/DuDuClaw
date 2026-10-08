import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ThumbsDown, ThumbsUp, PencilLine } from 'lucide-react';
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Textarea } from '@/components/mds';
import { api, type DeliverableVerdict, type EmployeeDigest } from '@/lib/api';

/**
 * EmployeeDigestCard (P9) — the newest per-employee "while you were away"
 * digest the gateway assembled (no model call), with 👍 / 👎 / "needs
 * changes" on each finished item. Renders nothing while the feature is off
 * or before the first digest exists ("無事不報"); every item and every
 * feedback write is checked server-side against the viewer's binding and
 * the task's audience.
 */
export function EmployeeDigestCard({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [digest, setDigest] = useState<EmployeeDigest | null>(null);
  const [verdicts, setVerdicts] = useState<Record<string, DeliverableVerdict>>({});
  const [noteFor, setNoteFor] = useState<string | null>(null);
  const [note, setNote] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    api.digest
      .latest()
      .then((r) => {
        if (!alive) return;
        setDigest(r?.enabled && r.digest ? r.digest : null);
        setVerdicts(r?.feedback ?? {});
      })
      .catch(() => {
        // Informational card: hide on error.
      });
    return () => {
      alive = false;
    };
  }, [enabled]);

  if (!digest || digest.agents.length === 0) return null;

  const send = async (taskId: string, verdict: DeliverableVerdict, text?: string) => {
    setError(null);
    try {
      await api.digest.feedback(taskId, verdict, text);
      setVerdicts((v) => ({ ...v, [taskId]: verdict }));
      setNoteFor(null);
      setNote('');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <Card data-testid="employee-digest">
      <CardHeader>
        <CardTitle>{t('home.digest.title', { date: digest.date })}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4 text-sm">
        {error && <p className="text-destructive">{error}</p>}
        {digest.agents.map((a) => (
          <section key={a.agent_id} className="space-y-1">
            <h3 className="font-medium">{a.agent_id}</h3>
            <p className="text-xs text-muted-foreground">
              {t('home.digest.counts', {
                done: a.finished_total,
                pending: a.pending_approvals,
                runs: a.runs_done + a.runs_not_done,
                activity: a.activity_count,
              })}
              {a.spend_usd != null && a.spend_usd > 0 ? ` · $${a.spend_usd.toFixed(2)}` : ''}
            </p>
            <ul className="space-y-1">
              {a.finished.map((item) => {
                const given = verdicts[item.id];
                return (
                  <li key={item.id} className="flex flex-col gap-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <Link className="underline-offset-2 hover:underline" to={`/tasks/${item.id}`}>
                        {item.title}
                      </Link>
                      {given && <Badge variant="secondary">{t(`home.digest.verdict.${given}`)}</Badge>}
                      <span className="ml-auto flex gap-1">
                        <Button
                          size="sm"
                          variant="ghost"
                          aria-label={t('home.digest.up')}
                          onClick={() => void send(item.id, 'up')}
                        >
                          <ThumbsUp className="size-3.5" />
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          aria-label={t('home.digest.down')}
                          onClick={() => void send(item.id, 'down')}
                        >
                          <ThumbsDown className="size-3.5" />
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          aria-label={t('home.digest.changes')}
                          onClick={() => {
                            setNoteFor(item.id);
                            setNote('');
                          }}
                        >
                          <PencilLine className="size-3.5" />
                        </Button>
                      </span>
                    </div>
                    {noteFor === item.id && (
                      <div className="space-y-1">
                        <Textarea
                          className="h-14"
                          value={note}
                          placeholder={t('home.digest.notePlaceholder')}
                          onChange={(e) => setNote(e.target.value)}
                        />
                        <Button size="sm" onClick={() => void send(item.id, 'changes', note)}>
                          {t('home.digest.sendChanges')}
                        </Button>
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        ))}
      </CardContent>
    </Card>
  );
}
