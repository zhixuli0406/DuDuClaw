import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  CollectionPageHeader,
  CollectionPageState,
  ErrorState,
  Card,
  CardContent,
  Badge,
  Button,
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  Textarea,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/mds';
import {
  Mail,
  MailOpen,
  Send,
  Archive,
  ShieldAlert,
  CheckCircle2,
  XCircle,
  AlertTriangle,
  Clock,
} from 'lucide-react';
import { api, type MailMessage, type MailMessageFull, type MailDraft, type MailStatus } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';

/**
 * MailPage (`/mail`, Agent Mail / P2-d) — 信箱.
 *
 * Two halves, deliberately asymmetric:
 *
 * - **收件匣** is a reading surface. Mail that arrived is shown as it arrived,
 *   including anything the prompt-injection scanner flagged — a flagged
 *   message is marked, never hidden, because the person is the one who has to
 *   judge it. What "flagged" actually buys is upstream: such a mail never
 *   wakes an AI employee on its own.
 * - **待寄出** is a decision surface. Nothing in it has been sent. Each row is
 *   a draft an AI employee wrote and a human has to confirm; confirming
 *   records the decision and the gateway performs the send on its next pass,
 *   so this page reports 「已確認，排隊寄出」 rather than claiming 「已寄出」.
 *
 * All six `mail.*` RPCs are `require_manager!`-gated server-side and the route
 * mirrors that.
 */
export function MailPage() {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );
  const connState = useConnectionStore((s) => s.state);

  const [tab, setTab] = useState<'inbox' | 'outbox'>('inbox');
  const [status, setStatus] = useState<MailStatus | null>(null);
  const [messages, setMessages] = useState<MailMessage[] | null>(null);
  const [drafts, setDrafts] = useState<MailDraft[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [open, setOpen] = useState<MailMessageFull | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  // Which draft's 拒絕 note box is expanded, and what's typed into it so far
  // (keyed by mail_id — several drafts can be pending at once, each with its
  // own in-progress note).
  const [rejectingId, setRejectingId] = useState<string | null>(null);
  const [rejectNotes, setRejectNotes] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    try {
      const [st, inbox, out] = await Promise.all([
        api.mail.status(),
        api.mail.list({ limit: 50 }),
        api.mail.outbox({ limit: 50 }),
      ]);
      setStatus(st);
      setMessages(inbox.messages ?? []);
      setDrafts(out.drafts ?? []);
      setLoadError(null);
    } catch (e) {
      setLoadError(e);
    }
  }, []);

  useEffect(() => {
    if (connState === 'authenticated') void load();
  }, [connState, load]);

  const openMail = async (mailId: string) => {
    setActionError(null);
    try {
      const full = await api.mail.read(mailId);
      setOpen(full);
      // Reading marks it read server-side; reflect that without a full reload.
      setMessages((prev) => prev?.map((m) => (m.mail_id === mailId ? { ...m, read: true } : m)) ?? prev);
    } catch (e) {
      setActionError(String(e));
    }
  };

  const archive = async (mailId: string) => {
    setBusy(mailId);
    setActionError(null);
    try {
      await api.mail.archive(mailId);
      setMessages((prev) => prev?.filter((m) => m.mail_id !== mailId) ?? prev);
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const decide = async (mailId: string, approve: boolean, note?: string) => {
    setBusy(mailId);
    setActionError(null);
    try {
      await api.mail.decide(mailId, approve, note);
      setRejectingId((cur) => (cur === mailId ? null : cur));
      setRejectNotes((prev) => {
        if (!(mailId in prev)) return prev;
        const next = { ...prev };
        delete next[mailId];
        return next;
      });
      await load();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const pendingCount = drafts?.filter((d) => d.status === 'pending').length ?? 0;
  const unreadCount = messages?.filter((m) => !m.read).length ?? 0;

  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader
        icon={Mail}
        title={t('mail.title')}
        count={messages?.length}
        description={t('mail.subtitle')}
      />

      <div className="flex-1 overflow-y-auto p-5">
        {messages === null && loadError != null ? (
          <ErrorState icon={Mail} error={loadError} onRetry={() => void load()} />
        ) : messages === null ? (
          <CollectionPageState state="loading" title={t('mail.title')} />
        ) : status && !status.enabled ? (
          <CollectionPageState
            state="empty"
            icon={Mail}
            title={t('mail.disabled.title')}
            description={t('mail.disabled.desc')}
          />
        ) : (
          <>
            {status && !status.smtp_configured && pendingCount > 0 && (
              <Card className="mb-4 border-amber-500/40">
                <CardContent className="flex items-start gap-2 py-3 text-sm">
                  <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-500" />
                  <span>{t('mail.warn.noSmtp')}</span>
                </CardContent>
              </Card>
            )}
            {actionError && (
              <Card className="mb-4 border-destructive/40">
                <CardContent className="py-3 text-sm text-destructive">{actionError}</CardContent>
              </Card>
            )}

            <Tabs value={tab} onValueChange={(v) => setTab(v as 'inbox' | 'outbox')} className="flex flex-col gap-4">
              <TabsList>
                <TabsTab value="inbox">{t('mail.tabs.inbox', { n: unreadCount })}</TabsTab>
                <TabsTab value="outbox">{t('mail.tabs.outbox', { n: pendingCount })}</TabsTab>
              </TabsList>

              <TabsPanel value="inbox" className="space-y-3">
                {messages.length === 0 ? (
                  <CollectionPageState
                    state="empty"
                    icon={MailOpen}
                    title={t('mail.inbox.empty.title')}
                    description={t('mail.inbox.empty.desc')}
                  />
                ) : (
                  messages.map((m) => (
                    <Card key={m.mail_id} className={m.read ? undefined : 'border-brand/40'}>
                      <CardContent className="flex flex-col gap-2 py-3">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="font-medium">{m.subject || t('mail.noSubject')}</span>
                          {!m.read && <Badge variant="default">{t('mail.badge.unread')}</Badge>}
                          {m.flagged && (
                            <Badge variant="destructive" className="gap-1">
                              <ShieldAlert className="size-3" />
                              {t('mail.badge.flagged')}
                            </Badge>
                          )}
                          {m.handled && <Badge variant="secondary">{t('mail.badge.handled')}</Badge>}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          {t('mail.meta', { from: m.from, at: m.received_at, source: m.source })}
                        </div>
                        <p className="line-clamp-2 text-sm text-muted-foreground">{m.snippet}</p>
                        <div className="flex gap-2">
                          <Button size="sm" variant="outline" onClick={() => void openMail(m.mail_id)}>
                            {t('mail.action.open')}
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={busy === m.mail_id}
                            onClick={() => void archive(m.mail_id)}
                          >
                            <Archive className="size-3.5" />
                            {t('mail.action.archive')}
                          </Button>
                        </div>
                      </CardContent>
                    </Card>
                  ))
                )}
              </TabsPanel>

              <TabsPanel value="outbox" className="space-y-3">
                <p className="text-sm text-muted-foreground">{t('mail.outbox.rule')}</p>
                {drafts === null || drafts.length === 0 ? (
                  <CollectionPageState
                    state="empty"
                    icon={Send}
                    title={t('mail.outbox.empty.title')}
                    description={t('mail.outbox.empty.desc')}
                  />
                ) : (
                  drafts.map((d) => (
                    <Card key={d.mail_id}>
                      <CardContent className="flex flex-col gap-2 py-3">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="font-medium">{d.subject}</span>
                          <DraftStatusBadge status={d.status} label={t(`mail.status.${d.status}`)} />
                        </div>
                        <div className="text-xs text-muted-foreground">
                          {t('mail.draft.meta', { to: d.to, agent: d.agent_id, at: d.created_at })}
                        </div>
                        <p className="whitespace-pre-wrap text-sm text-muted-foreground">{d.body}</p>
                        {d.note && <p className="text-xs text-muted-foreground">{d.note}</p>}
                        {d.status === 'pending' && rejectingId === d.mail_id ? (
                          <div className="space-y-2">
                            <Textarea
                              className="h-16 resize-y"
                              value={rejectNotes[d.mail_id] ?? ''}
                              onChange={(e) =>
                                setRejectNotes((prev) => ({ ...prev, [d.mail_id]: e.target.value }))
                              }
                              placeholder={t('mail.action.rejectNote.placeholder')}
                              autoFocus
                            />
                            <div className="flex gap-2">
                              <Button
                                size="sm"
                                variant="destructive"
                                disabled={busy === d.mail_id}
                                onClick={() => void decide(d.mail_id, false, rejectNotes[d.mail_id])}
                              >
                                <XCircle className="size-3.5" />
                                {t('mail.action.confirmReject')}
                              </Button>
                              <Button
                                size="sm"
                                variant="ghost"
                                disabled={busy === d.mail_id}
                                onClick={() => setRejectingId(null)}
                              >
                                {t('common.cancel')}
                              </Button>
                            </div>
                          </div>
                        ) : d.status === 'pending' ? (
                          <div className="flex gap-2">
                            <Button
                              size="sm"
                              disabled={busy === d.mail_id}
                              onClick={() => void decide(d.mail_id, true)}
                            >
                              <CheckCircle2 className="size-3.5" />
                              {t('mail.action.confirmSend')}
                            </Button>
                            <Button
                              size="sm"
                              variant="outline"
                              disabled={busy === d.mail_id}
                              onClick={() => setRejectingId(d.mail_id)}
                            >
                              <XCircle className="size-3.5" />
                              {t('mail.action.reject')}
                            </Button>
                          </div>
                        ) : null}
                      </CardContent>
                    </Card>
                  ))
                )}
              </TabsPanel>
            </Tabs>
          </>
        )}
      </div>

      <Dialog open={open !== null} onOpenChange={(o) => !o && setOpen(null)}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>{open?.subject || t('mail.noSubject')}</DialogTitle>
          </DialogHeader>
          {open && (
            <div className="space-y-3">
              <div className="text-xs text-muted-foreground">
                {t('mail.meta', { from: open.from, at: open.received_at, source: open.source })}
              </div>
              {open.flagged && (
                <div className="flex items-start gap-2 rounded-md border border-destructive/40 p-3 text-sm">
                  <ShieldAlert className="mt-0.5 size-4 shrink-0 text-destructive" />
                  <span>{t('mail.flagged.notice')}</span>
                </div>
              )}
              <p className="max-h-[50vh] overflow-y-auto whitespace-pre-wrap text-sm">{open.body}</p>
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(null)}>
              {t('mail.action.close')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

/** Status pill for an outgoing draft. `pending` is the only state in which
 *  nothing has left the building, so it reads as a waiting state, not a
 *  success one. */
function DraftStatusBadge({ status, label }: { status: MailDraft['status']; label: string }) {
  if (status === 'sent') {
    return (
      <Badge variant="secondary" className="gap-1">
        <Send className="size-3" />
        {label}
      </Badge>
    );
  }
  if (status === 'failed') {
    return (
      <Badge variant="destructive" className="gap-1">
        <AlertTriangle className="size-3" />
        {label}
      </Badge>
    );
  }
  if (status === 'rejected') {
    return (
      <Badge variant="outline" className="gap-1">
        <XCircle className="size-3" />
        {label}
      </Badge>
    );
  }
  return (
    <Badge variant="default" className="gap-1">
      <Clock className="size-3" />
      {label}
    </Badge>
  );
}
