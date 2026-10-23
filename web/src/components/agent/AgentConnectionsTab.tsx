import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { CheckCircle2, Loader2, Mail, XCircle } from 'lucide-react';
import {
  api,
  type AgentGoogleIdentity,
  type GoogleIdentityTestStep,
} from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { toast } from '@/lib/toast';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  ErrorState,
  Input,
  Segmented,
  Skeleton,
} from '@/components/mds';

type Choice = 'home' | 'delegate' | 'none';

function choiceOf(v: AgentGoogleIdentity): Choice {
  if (v.kind === 'delegate') return 'delegate';
  if (v.kind === 'none') return 'none';
  return 'home';
}

/**
 * 連線 — which Google account this AI employee uses. Everyone who can view
 * the employee sees which identity is in effect; only an Admin changes it
 * (公司共用帳號 / 代表某位同事（網域委派） / 不使用) and runs the connection
 * test, which proves the colleague's account works without sending anything.
 */
export function AgentConnectionsTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const ownEmail = useAuthStore((s) => s.user?.email);

  const [view, setView] = useState<AgentGoogleIdentity | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [editing, setEditing] = useState(false);
  const [choice, setChoice] = useState<Choice>('home');
  const [email, setEmail] = useState('');
  const [fallback, setFallback] = useState(false);
  const [busy, setBusy] = useState<'idle' | 'saving' | 'testing'>('idle');
  const [steps, setSteps] = useState<GoogleIdentityTestStep[] | null>(null);

  const load = useCallback(async () => {
    try {
      const v = await api.googleIdentity.get(agentId);
      setView(v);
      setError(null);
    } catch (e) {
      setError(e);
    }
  }, [agentId]);

  useEffect(() => {
    void load();
  }, [load]);

  const startEdit = () => {
    if (!view) return;
    setChoice(choiceOf(view));
    setEmail(view.email ?? '');
    setFallback(view.fallback === 'home');
    setEditing(true);
  };

  const save = async () => {
    const identity =
      choice === 'delegate' ? `delegate:${email.trim()}` : choice;
    setBusy('saving');
    try {
      const v = await api.googleIdentity.set({
        agent_id: agentId,
        identity,
        ...(choice === 'delegate' ? {
          fallback: fallback ? 'home' : 'none',
          requesters: view?.requesters ?? 'bound_people',
        } : {}),
      });
      setView(v);
      setEditing(false);
      setSteps(null);
      toast.success(t('agentConnections.saved'));
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy('idle');
    }
  };

  const runTest = async () => {
    setBusy('testing');
    try {
      const r = await api.googleIdentity.test(agentId);
      setSteps(r.steps);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy('idle');
    }
  };

  if (error) return <ErrorState error={error} onRetry={() => void load()} />;
  if (!view) return <Skeleton className="h-40 w-full" />;

  const current =
    view.kind === 'delegate'
      ? t('agentConnections.identity.delegate', { email: view.email ?? '' })
      : view.kind === 'none'
        ? t('agentConnections.identity.none')
        : view.kind === 'home'
          ? t('agentConnections.identity.home')
          : t('agentConnections.identity.invalid');

  const stateBadge =
    view.state === 'ready' ? (
      <Badge className="bg-success/15 text-success">{t('agentConnections.state.ready')}</Badge>
    ) : view.state === 'blocked' ? (
      <Badge className="bg-warning/15 text-warning">{t('agentConnections.state.blocked')}</Badge>
    ) : null;

  return (
    <Card data-testid="agent-connections-google">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Mail className="size-4" />
          {t('agentConnections.google.title')}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted-foreground">{t('agentConnections.current')}</span>
          <span className="font-medium" data-testid="agent-connections-current">{current}</span>
          {stateBadge}
        </div>
        {view.state === 'blocked' && view.message && (
          <p className="text-sm text-warning">{view.message}</p>
        )}
        {view.kind === 'delegate' && (
          <p className="text-sm text-muted-foreground">
            {view.requesters === 'anyone'
              ? t('agentConnections.requesters.anyone')
              : view.requesters === 'owner_only'
                ? t('agentConnections.requesters.ownerOnly', { email: view.email ?? '' })
                : t('agentConnections.requesters.bound')}
            {view.fallback === 'home' ? ` ${t('agentConnections.fallbackOn')}` : ''}
          </p>
        )}

        {view.email && ownEmail?.trim().toLowerCase() === view.email.toLowerCase() && (
          <Link to="/mail" className="text-sm underline">{t('nav.mail')}</Link>
        )}

        {isAdmin && !editing && (
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={startEdit}>
              {t('agentConnections.change')}
            </Button>
            {view.kind === 'delegate' && (
              <Button variant="outline" onClick={() => void runTest()} disabled={busy !== 'idle'}>
                {busy === 'testing' ? <Loader2 className="size-4 animate-spin" /> : <CheckCircle2 className="size-4" />}
                {t('agentConnections.test')}
              </Button>
            )}
          </div>
        )}
        {!isAdmin && (
          <p className="text-xs text-muted-foreground">{t('agentConnections.adminOnly')}</p>
        )}

        {isAdmin && editing && (
          <div className="space-y-3 rounded-lg border border-border p-3">
            <Segmented<Choice>
              value={choice}
              onValueChange={setChoice}
              aria-label={t('agentConnections.google.title')}
              options={[
                { value: 'home', label: t('agentConnections.choice.home') },
                { value: 'delegate', label: t('agentConnections.choice.delegate') },
                { value: 'none', label: t('agentConnections.choice.none') },
              ]}
            />
            {choice === 'delegate' && (
              <div className="space-y-2">
                <label className="text-xs font-medium" htmlFor="agent-google-email">
                  {t('agentConnections.email')}
                </label>
                <Input
                  id="agent-google-email"
                  type="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="alice@customer.com"
                />
                <p className="text-xs text-muted-foreground">{t('agentConnections.delegateHint')}</p>
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={fallback}
                    onChange={(e) => setFallback(e.target.checked)}
                  />
                  {t('agentConnections.fallback')}
                </label>
              </div>
            )}
            <div className="flex gap-2">
              <Button
                onClick={() => void save()}
                disabled={busy !== 'idle' || (choice === 'delegate' && !email.trim())}
              >
                {busy === 'saving' && <Loader2 className="size-4 animate-spin" />}
                {t('common.save')}
              </Button>
              <Button variant="outline" onClick={() => setEditing(false)} disabled={busy !== 'idle'}>
                {t('common.cancel')}
              </Button>
            </div>
          </div>
        )}

        {steps && (
          <ul className="space-y-1.5 text-sm" data-testid="agent-connections-steps">
            {steps.map((s) => (
              <li key={s.id} className="flex items-start gap-2">
                {s.ok ? (
                  <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" />
                ) : (
                  <XCircle className="mt-0.5 size-4 shrink-0 text-destructive" />
                )}
                <span>
                  <span className="font-medium">{s.label}</span>
                  <span className="text-muted-foreground"> — {s.detail}</span>
                </span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
