import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { CheckCircle2, Loader2, Mail, XCircle } from 'lucide-react';
import {
  api,
  type AgentGoogleIdentity,
  type GoogleIdentityTestStep,
} from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { toast } from '@/lib/toast';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
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

type Choice = 'home' | 'delegate' | 'owner' | 'user' | 'mine' | 'none';

function choiceOf(v: AgentGoogleIdentity): Choice {
  if (v.kind === 'delegate') return 'delegate';
  if (v.kind === 'none') return 'none';
  if (v.kind === 'owner') return 'owner';
  if (v.kind === 'user') return 'user';
  return 'home';
}

/**
 * 連線 — which Google account this AI employee uses. Everyone who can view
 * the employee sees which identity is in effect; only an Admin changes it
 * (公司共用帳號 / 代表某位同事（網域委派） / 不使用) and runs the connection
 * test, which proves the colleague's account works without sending anything.
 */
type AuthSnapshot = ReturnType<typeof useAuthStore.getState>;

function actorFingerprint(state: AuthSnapshot): string {
  // No credentials: every available identity/status/binding field participates.
  return JSON.stringify([state.user, state.bindings, state.isAuthenticated, state.initialized]);
}

export function AgentConnectionsTab({ agentId }: { agentId: string }) {
  const actorScope = useAuthStore(actorFingerprint);
  const actor = useAuthStore((state) => state.user);
  const enterprise = useSystemStore((state) => state.status?.edition_profile !== 'personal');
  const [actorEpoch, setActorEpoch] = useState(0);
  useLayoutEffect(() => {
    let previous = actorFingerprint(useAuthStore.getState());
    return useAuthStore.subscribe((state) => {
      const next = actorFingerprint(state);
      if (next !== previous) {
        previous = next;
        // Preserve intermediate changes even when React batches A→B→A.
        setActorEpoch((epoch) => epoch + 1);
      }
    });
  }, []);
  if (actor && actor.status !== 'active') return null;
  return <AgentConnectionsScope
    key={JSON.stringify([agentId, actorScope, actorEpoch, enterprise])}
    agentId={agentId}
    actorScope={actorScope}
  />;
}

function AgentConnectionsScope({ agentId, actorScope }: { agentId: string; actorScope: string }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin' && s.user.status === 'active');
  const ownEmail = useAuthStore((s) => s.user?.email);
  const userId = useAuthStore((s) => s.user?.id);
  const isOwner = useAuthStore((s) => s.bindings.some(b=>b.agent_name===agentId&&b.access_level==='owner'));
  const enterprise = useSystemStore((s)=>s.status?.edition_profile !== 'personal');

  const [view, setView] = useState<AgentGoogleIdentity | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [editing, setEditing] = useState(false);
  const [choice, setChoice] = useState<Choice>('home');
  const [email, setEmail] = useState('');
  const [fallback, setFallback] = useState(false);
  const [selectedUser,setSelectedUser] = useState('');
  const [requesters,setRequesters] = useState<AgentGoogleIdentity['requesters']>('bound_people');
  const [confirmed,setConfirmed] = useState(false);
  const [anyoneConfirmed,setAnyoneConfirmed] = useState(false);
  const [busy, setBusy] = useState<'idle' | 'saving' | 'testing'>('idle');
  const [steps, setSteps] = useState<GoogleIdentityTestStep[] | null>(null);
  const [primaryPending, setPrimaryPending] = useState<{
    agentId: string; userId: string; name: string;
  } | null>(null);

  const live = useRef(true);
  const loadEpoch = useRef(0);
  useLayoutEffect(() => {
    live.current = actorFingerprint(useAuthStore.getState()) === actorScope;
    const unsubscribe = useAuthStore.subscribe((state) => {
      if (actorFingerprint(state) !== actorScope) live.current = false;
    });
    return () => { live.current = false; unsubscribe(); };
  }, [actorScope]);
  const isCurrent = useCallback(() => live.current
    && actorFingerprint(useAuthStore.getState()) === actorScope, [actorScope]);

  const load = useCallback(async () => {
    if (!isCurrent()) return;
    const request = ++loadEpoch.current;
    try {
      const v = await api.googleIdentity.get(agentId);
      if (!isCurrent() || request !== loadEpoch.current) return;
      setView(v);
      setError(null);
    } catch (e) {
      if (isCurrent() && request === loadEpoch.current) setError(e);
    }
  }, [agentId, isCurrent]);

  useEffect(() => {
    void load();
  }, [load]);

  const startEdit = () => {
    if (!view) return;
    setChoice(isAdmin ? choiceOf(view) : view.kind==='none' ? 'none' : 'mine');
    setEmail(view.email ?? '');
    setFallback(view.fallback === 'home');
    setSelectedUser(view.selected_person?.user_id ?? view.owners?.[0]?.user_id ?? '');
    setRequesters(view.requesters);
    setConfirmed(false);
    setAnyoneConfirmed(false);
    setEditing(true);
  };

  const save = async () => {
    if (!isCurrent()) return;
    const identity =
      choice === 'delegate' ? `delegate:${email.trim()}` : choice==='user' ? `user:${selectedUser}` : choice==='mine' ? `user:${userId}` : choice;
    const personal = ['owner','user','mine'].includes(choice);
    setBusy('saving');
    try {
      const v = await api.googleIdentity.set({
        agent_id: agentId,
        identity,
        ...(choice === 'delegate'||personal ? {
          fallback: fallback ? 'home' : 'none',
          requesters,
          ...(anyoneConfirmed ? {confirm_anyone:true} : {}),
          ...(confirmed ? {confirm_shared_access:true} : {}),
        } : {}),
      });
      if (!isCurrent()) return;
      setView(v);
      setEditing(false);
      setSteps(null);
      toast.success(t('agentConnections.saved'));
    } catch (e) {
      if (isCurrent()) toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      if (isCurrent()) setBusy('idle');
    }
  };

  const runTest = async () => {
    if (!isCurrent()) return;
    setBusy('testing');
    try {
      const r = await api.googleIdentity.test(agentId);
      if (isCurrent()) setSteps(r.steps);
    } catch (e) {
      if (isCurrent()) toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      if (isCurrent()) setBusy('idle');
    }
  };

  const confirmPrimaryOwner = async () => {
    if (!isCurrent() || !primaryPending || primaryPending.agentId !== agentId || !isAdmin || !enterprise || busy !== 'idle') return;
    setBusy('saving');
    try {
      await api.personalGoogle.primaryOwner(primaryPending.agentId, primaryPending.userId);
      if (!isCurrent()) return;
      setPrimaryPending(null);
      await load();
    } catch (e) {
      if (isCurrent()) toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      if (isCurrent()) setBusy('idle');
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
          : view.kind==='owner'||view.kind==='user'
            ? t('agentConnections.identity.personal',{name:view.selected_person?.name ?? '—'})
            : t('agentConnections.identity.invalid');

  const stateBadge =
    view.state === 'ready'||view.state === 'configured' ? (
      <Badge className="bg-success/15 text-success">{t(view.state==='configured'?'agentConnections.state.configured':'agentConnections.state.ready')}</Badge>
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
        {view.message && (
          <p className="text-sm text-warning">{view.message}</p>
        )}
        {['delegate','owner','user'].includes(view.kind) && (
          <p className="text-sm text-muted-foreground">
            {view.requesters === 'anyone'
              ? t('agentConnections.requesters.anyone')
              : view.requesters === 'owner_only'
                ? t('agentConnections.requesters.ownerOnly', { email: view.email ?? '' })
                : t('agentConnections.requesters.bound')}
            {view.fallback === 'home' ? ` ${t('agentConnections.fallbackOn')}` : ''}
          </p>
        )}
        {(view.kind==='owner'||view.kind==='user') && <Link to="/me/connections" className="text-sm underline">{t('myConnections.title')}</Link>}

        {view.email && ownEmail?.trim().toLowerCase() === view.email.toLowerCase() && (
          <Link to="/mail" className="text-sm underline">{t('nav.mail')}</Link>
        )}

        {(isAdmin||(enterprise&&isOwner)) && !editing && (
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={startEdit}>
              {t('agentConnections.change')}
            </Button>
            {isAdmin&&view.kind === 'delegate' && (
              <Button variant="outline" onClick={() => void runTest()} disabled={busy !== 'idle'}>
                {busy === 'testing' ? <Loader2 className="size-4 animate-spin" /> : <CheckCircle2 className="size-4" />}
                {t('agentConnections.test')}
              </Button>
            )}
          </div>
        )}
        {!isAdmin && !isOwner && (
          <p className="text-xs text-muted-foreground">{t('agentConnections.adminOnly')}</p>
        )}

        {(isAdmin||(enterprise&&isOwner)) && editing && (
          <div className="space-y-3 rounded-lg border border-border p-3">
            <Segmented<Choice>
              value={choice}
              onValueChange={setChoice}
              aria-label={t('agentConnections.google.title')}
              options={isAdmin ? [
                { value: 'home', label: t('agentConnections.choice.home') },
                { value: 'delegate', label: t('agentConnections.choice.delegate') },
                ...(enterprise ? [{value:'owner' as const,label:t('agentConnections.choice.owner')},{value:'user' as const,label:t('agentConnections.choice.user')}] : []),
                { value: 'none', label: t('agentConnections.choice.none') },
              ] : [{value:'mine',label:t('agentConnections.choice.mine')},{value:'none',label:t('agentConnections.choice.none')}]}
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
            {choice==='user' && <select aria-label={t('agentConnections.choice.user')} value={selectedUser} onChange={e=>setSelectedUser(e.target.value)} className="rounded border border-border bg-background p-2 text-sm">{view.owners?.map(person=><option key={person.user_id} value={person.user_id}>{person.name} ({person.email})</option>)}</select>}
            {['owner','user','mine'].includes(choice) && <div className="space-y-2"><p className="text-sm text-muted-foreground">{t('agentConnections.personalHint')}</p>
              {isAdmin&&<label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={fallback} onChange={e=>setFallback(e.target.checked)}/>{t('agentConnections.fallback')}</label>}
              {!isAdmin&&<><p className="text-sm">{view.bound_people?.map(person=>`${person.name} (${person.email})`).join('、')}</p><p className="text-xs text-muted-foreground">{t('myConnections.channels',{channels:view.allowed_channels?.join('、')||t('myConnections.noChannels')})}</p><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={confirmed} onChange={e=>setConfirmed(e.target.checked)}/>{t('agentConnections.personalConfirm')}</label></>}
            </div>}
            {['delegate','owner','user','mine'].includes(choice) && <div className="space-y-2"><label htmlFor="google-requesters" className="text-sm">{t('agentConnections.requesterLabel')}</label><select id="google-requesters" value={requesters} onChange={e=>setRequesters(e.target.value as AgentGoogleIdentity['requesters'])} className="ml-2 rounded border border-border bg-background p-2 text-sm">
              {isAdmin&&<option value="anyone">{t('agentConnections.requesters.anyone')}</option>}
              {(isAdmin||view.requesters!=='owner_only')&&<option value="bound_people">{t('agentConnections.requesters.bound')}</option>}
              <option value="owner_only">{t('agentConnections.requesters.ownerOnly',{email:view.selected_person?.login_email??email})}</option>
            </select>{requesters==='anyone'&&<label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={anyoneConfirmed} onChange={e=>setAnyoneConfirmed(e.target.checked)}/>{t('agentConnections.anyoneConfirm')}</label>}</div>}
            <div className="flex gap-2">
              <Button
                onClick={() => void save()}
                disabled={busy !== 'idle' || (choice === 'delegate' && !email.trim())||(choice==='user'&&!selectedUser)||(!isAdmin&&choice!=='none'&&!confirmed)||(['delegate','owner','user','mine'].includes(choice)&&requesters==='anyone'&&!anyoneConfirmed)}
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
        {isAdmin&&enterprise&&view.owners&&view.owners.length>1&&<div className="space-y-2">{view.owners.map(person=><div key={person.user_id} className="flex items-center justify-between gap-3 text-sm"><span>{person.name} ({person.email})</span><Button variant="outline" disabled={person.is_primary||busy!=='idle'} onClick={() => setPrimaryPending({agentId, userId: person.user_id, name: person.name})}>{t('agentConnections.primary')}</Button></div>)}</div>}

        <ConfirmDialog
          open={primaryPending !== null && primaryPending.agentId === agentId && isAdmin && enterprise}
          onClose={() => { if (busy === 'idle') setPrimaryPending(null); }}
          onConfirm={() => void confirmPrimaryOwner()}
          title={t('agentConnections.primaryConfirmTitle')}
          message={t('agentConnections.primaryConfirmMessage', {name: primaryPending?.name ?? ''})}
          confirmLabel={t('common.confirm')}
          busy={busy !== 'idle'}
        />

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
