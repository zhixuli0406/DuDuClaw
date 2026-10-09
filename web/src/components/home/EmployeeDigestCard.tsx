import { useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Paperclip, ThumbsDown, ThumbsUp, PencilLine } from 'lucide-react';
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Textarea } from '@/components/mds';
import {
  api,
  digestArtifactKey,
  type DeliverableVerdict,
  type DigestArtifact,
  type EmployeeDigest,
} from '@/lib/api';

type Translate = (id: string, values?: Record<string, string | number>) => string;

/**
 * 👍 / 👎 / "needs changes" (with an optional note) for one digest target.
 * `send` performs the server call; the server checks the viewer's binding
 * and the task's audience.
 */
function FeedbackRow({
  itemKey,
  given,
  label,
  t,
  send,
}: {
  itemKey: string;
  given: DeliverableVerdict | undefined;
  label: ReactNode;
  t: Translate;
  send: (verdict: DeliverableVerdict, note?: string) => Promise<boolean>;
}) {
  const [noteOpen, setNoteOpen] = useState(false);
  const [note, setNote] = useState('');
  return (
    <li className="flex flex-col gap-1" data-testid={`digest-item-${itemKey}`}>
      <div className="flex flex-wrap items-center gap-2">
        {label}
        {given && <Badge variant="secondary">{t(`home.digest.verdict.${given}`)}</Badge>}
        <span className="ml-auto flex gap-1">
          <Button size="sm" variant="ghost" aria-label={t('home.digest.up')} onClick={() => void send('up')}>
            <ThumbsUp className="size-3.5" />
          </Button>
          <Button size="sm" variant="ghost" aria-label={t('home.digest.down')} onClick={() => void send('down')}>
            <ThumbsDown className="size-3.5" />
          </Button>
          <Button
            size="sm"
            variant="ghost"
            aria-label={t('home.digest.changes')}
            onClick={() => {
              setNoteOpen(true);
              setNote('');
            }}
          >
            <PencilLine className="size-3.5" />
          </Button>
        </span>
      </div>
      {noteOpen && (
        <div className="space-y-1">
          <Textarea
            className="h-14"
            value={note}
            placeholder={t('home.digest.notePlaceholder')}
            onChange={(e) => setNote(e.target.value)}
          />
          <Button
            size="sm"
            onClick={() =>
              void send('changes', note).then((ok) => {
                if (ok) setNoteOpen(false);
              })
            }
          >
            {t('home.digest.sendChanges')}
          </Button>
        </div>
      )}
    </li>
  );
}

/**
 * EmployeeDigestCard (P9) — the newest per-employee "while you were away"
 * digest the gateway assembled (no model call), with 👍 / 👎 / "needs
 * changes" on each finished item and on each file the employee handed over.
 * Renders nothing while the feature is off or before the first digest exists
 * ("無事不報"); every item and every feedback write is checked server-side
 * against the viewer's binding and the task's audience.
 */
export function EmployeeDigestCard({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  const t: Translate = (id, values) => intl.formatMessage({ id }, values);
  const [digest, setDigest] = useState<EmployeeDigest | null>(null);
  const [verdicts, setVerdicts] = useState<Record<string, DeliverableVerdict>>({});
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

  const record = async (key: string, verdict: DeliverableVerdict, call: () => Promise<unknown>) => {
    setError(null);
    try {
      await call();
      setVerdicts((v) => ({ ...v, [key]: verdict }));
      return true;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return false;
    }
  };

  const sendTask = (taskId: string) => (verdict: DeliverableVerdict, note?: string) =>
    record(taskId, verdict, () => api.digest.feedback(taskId, verdict, note));
  const sendFile = (file: DigestArtifact) => (verdict: DeliverableVerdict, note?: string) =>
    record(digestArtifactKey(file), verdict, () => api.digest.feedbackArtifact(file, verdict, note));

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
              {(a.artifacts_total ?? 0) > 0 ? ` · ${t('home.digest.files', { count: a.artifacts_total ?? 0 })}` : ''}
              {a.spend_usd != null && a.spend_usd > 0 ? ` · $${a.spend_usd.toFixed(2)}` : ''}
            </p>
            <ul className="space-y-1">
              {a.finished.map((item) => (
                <FeedbackRow
                  key={item.id}
                  itemKey={item.id}
                  given={verdicts[item.id]}
                  t={t}
                  send={sendTask(item.id)}
                  label={
                    <Link className="underline-offset-2 hover:underline" to={`/tasks/${item.id}`}>
                      {item.title}
                    </Link>
                  }
                />
              ))}
              {(a.artifacts ?? []).map((file) => {
                const key = digestArtifactKey(file);
                return (
                  <FeedbackRow
                    key={key}
                    itemKey={key}
                    given={verdicts[key]}
                    t={t}
                    send={sendFile(file)}
                    label={
                      <span className="inline-flex items-center gap-1">
                        <Paperclip className="size-3.5 text-muted-foreground" aria-label={t('home.digest.fileLabel')} />
                        {file.task_id ? (
                          <Link className="underline-offset-2 hover:underline" to={`/tasks/${file.task_id}`}>
                            {file.name}
                          </Link>
                        ) : (
                          <span>{file.name}</span>
                        )}
                      </span>
                    }
                  />
                );
              })}
            </ul>
          </section>
        ))}
      </CardContent>
    </Card>
  );
}
