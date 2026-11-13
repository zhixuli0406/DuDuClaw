import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Copy, Library } from 'lucide-react';
import { api, type DepartmentInfo, type UserDetail } from '@/lib/api';
import { templateLibrary, type LibraryTemplate, type LibraryTemplateDetail, type TemplateCopyPreview } from '@/lib/template-library';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { formatError, toast } from '@/lib/toast';
import { seatLimitOf } from '@/lib/seat-limit';
import { SeatLimitNotice } from '@/components/shared/SeatLimitNotice';
import {
  ActorAvatar, Badge, Button, Card, CardContent, CardHeader, CardTitle, Checkbox,
  CollectionPageHeader, CollectionPageState, Dialog, DialogContent, DialogDescription,
  DialogFooter, DialogHeader, DialogTitle, Input, Select, SelectContent,
  SelectItem, SelectTrigger, SelectValue, Textarea,
} from '@/components/mds';

const lines = (value: string) => [...new Set(value.split(/[\n,]/).map((item) => item.trim()).filter(Boolean))];
const permissionLabels: Record<string, string> = {
  can_create_agents: 'agents.edit.canCreateAgents',
  can_send_cross_agent: 'agents.edit.canSendCrossAgent',
  can_modify_own_skills: 'agents.edit.canModifySkills',
  can_modify_own_soul: 'agents.edit.canModifySoul',
  can_schedule_tasks: 'agents.edit.canScheduleTasks',
};
type EditorMode = 'copy' | 'export' | 'exportTeam' | 'edit' | 'apply' | null;

export function TemplatesPage() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const isAdmin = useAuthStore((state) => state.user?.role === 'admin');
  const isPersonal = useSystemStore((state) => state.status?.edition_profile === 'personal');
  const { agents, fetchAgents } = useAgentsStore();
  const [templates, setTemplates] = useState<LibraryTemplate[]>([]);
  const [premiumLocked, setPremiumLocked] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [query, setQuery] = useState('');
  const [kind, setKind] = useState('all');
  const [source, setSource] = useState('all');
  const [selectedId, setSelectedId] = useState('');
  const [detail, setDetail] = useState<LibraryTemplateDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState('');
  const [detailReload, setDetailReload] = useState(0);
  const selection = useRef(0);
  const [mode, setMode] = useState<EditorMode>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState('');
  const [name, setName] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [description, setDescription] = useState('');
  const [agent, setAgent] = useState('');
  const [members, setMembers] = useState<LibraryTemplateDetail[]>([]);
  const [skills, setSkills] = useState<string[]>([]);
  const [allowed, setAllowed] = useState('');
  const [denied, setDenied] = useState('');
  const [department, setDepartment] = useState('');
  const [boundUsers, setBoundUsers] = useState<string[]>([]);
  const [departments, setDepartments] = useState<DepartmentInfo[]>([]);
  const [users, setUsers] = useState<UserDetail[]>([]);
  const [placementLoading, setPlacementLoading] = useState(false);
  const [placementError, setPlacementError] = useState('');
  const [review, setReview] = useState(false);
  const [reviewed, setReviewed] = useState(false);
  const [copyPreview, setCopyPreview] = useState<TemplateCopyPreview | null>(null);
  const reviewRequest = useRef(0);
  useEffect(() => {
    reviewRequest.current++;
    setReview(false);
    setReviewed(false);
    setCopyPreview(null);
    return () => { reviewRequest.current++; };
  }, [mode, name, description, agent, detail, members, skills, allowed, denied, displayName, department, boundUsers]);
  const [result, setResult] = useState<{ success: boolean; agents: string[]; warning?: string; error?: string | null; seat_limit?: unknown } | null>(null);

  const load = useCallback(async () => {
    if (!isAdmin) return;
    setLoading(true);
    setError('');
    try {
      const response = await templateLibrary.catalog();
      setTemplates(response.templates);
      setPremiumLocked(response.premium_locked ?? false);
    } catch (e) { setError(formatError(e)); }
    finally { setLoading(false); }
  }, [isAdmin]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    const request = ++selection.current;
    setDetail(null);
    setDetailError('');
    if (!selectedId || !isAdmin) return;
    setDetailLoading(true);
    templateLibrary.get(selectedId).then((value) => {
      if (request === selection.current) setDetail(value);
    }).catch((e) => {
      if (request === selection.current) setDetailError(formatError(e));
    }).finally(() => {
      if (request === selection.current) setDetailLoading(false);
    });
    return () => { selection.current++; };
  }, [selectedId, isAdmin, detailReload]);

  const loadPlacement = async () => {
    setPlacementLoading(true);
    setPlacementError('');
    try {
      const [people, groups] = await Promise.all([
        isPersonal ? Promise.resolve({ users: [] }) : api.users.list(), isPersonal ? Promise.resolve({ departments: [] }) : api.departments.list(),
      ]);
      setUsers(people.users.filter((user) => user.status === 'active'));
      setDepartments(groups.departments);
    } catch (e) { setPlacementError(formatError(e)); }
    finally { setPlacementLoading(false); }
  };

  const open = (next: EditorMode) => {
    setMode(next);
    setActionError('');
    setResult(null);
    setReview(false);
    setName(next === 'apply' ? '' : detail?.name ?? '');
    setDisplayName('');
    setDescription(next === 'export' || next === 'exportTeam' ? '' : detail?.description ?? '');
    setAgent('');
    setDepartment('');
    setBoundUsers([]);
    setMembers(detail?.members?.map((m) => ({ ...m })) ?? []);
    setSkills(detail?.skills ?? []);
    setAllowed((detail?.allowed_tools ?? []).join('\n'));
    setDenied((detail?.denied_tools ?? []).join('\n'));
    if (next === 'export' || next === 'exportTeam') { setName(''); void fetchAgents().catch((e) => setActionError(formatError(e))); }
    if (next === 'apply') void loadPlacement();
  };

  const copySource = () => ({
    ...(mode === 'export' ? { agent } : mode === 'exportTeam' ? { team_agent: agent } : { template_id: detail?.id }),
    name: name.trim(), description,
  });

  const beginReview = async () => {
    setActionError('');
    setReviewed(false);
    if (mode !== 'copy' && mode !== 'export' && mode !== 'exportTeam') {
      setReview(true);
      return;
    }
    const request = ++reviewRequest.current;
    setBusy(true);
    try {
      const preview = await templateLibrary.previewCopy(copySource());
      if (request !== reviewRequest.current) return;
      setCopyPreview(preview);
      setReview(true);
    } catch (e) {
      if (request === reviewRequest.current) setActionError(formatError(e));
    } finally { setBusy(false); }
  };

  const submit = async () => {
    if (busy || !name.trim() || !review || !reviewed) return;
    setBusy(true);
    setActionError('');
    try {
      if (mode === 'apply' && detail) {
        const applied = await templateLibrary.apply({
          id: detail.id, ...(typeof detail.revision === 'number' ? { revision: detail.revision } : {}),
          name: name.trim(), display_name: displayName.trim(),
          department, bound_users: boundUsers, reviewed: true,
        });
        setResult(applied);
        // A partial team has committed employees. Never retry the whole batch.
        if (applied.warning) toast.info(t(applied.warning === 'template_changed' ? 'templates.templateChanged' : applied.warning === 'binding_failed' ? 'templates.bindingFailed' : applied.warning === 'seat_limit' ? 'agents.seatLimit.title' : 'templates.creationFailed'));
        void fetchAgents().catch(() => {});
        return;
      }
      let selected = selectedId;
      if (mode === 'edit' && detail && typeof detail.revision === 'number') {
        await templateLibrary.save({
          id: detail.id, revision: detail.revision, name: name.trim(), description, reviewed: true,
          skills, allowed_tools: lines(allowed), denied_tools: lines(denied),
          ...(detail.kind === 'team' ? { members: members.map((m) => ({ id: m.id, edit: {
            name: m.name, description: m.description, skills: m.skills, allowed_tools: lines(m.allowed_tools.join('\n')), denied_tools: lines(m.denied_tools.join('\n')),
          } })) } : {}),
        });
      } else if (mode === 'copy' || mode === 'export' || mode === 'exportTeam') {
        if (!copyPreview) return;
        const copied = await templateLibrary.copy({
          ...copySource(), reviewed: true, review_digest: copyPreview.review_digest,
        });
        selected = copied.id;
      } else { return; }
      setMode(null);
      toast.success(t('templates.saved'));
      await load();
      if (selected === selectedId) setDetail(await templateLibrary.get(selected));
      else setSelectedId(selected);
    } catch (e) { setActionError(formatError(e)); }
    finally { setBusy(false); }
  };

  if (!isAdmin) return <CollectionPageState state="empty" title={t('templates.adminOnly')} />;

  const visible = templates.filter((item) =>
    (kind === 'all' || item.kind === kind) && (source === 'all' || item.source === source)
    && `${item.name} ${item.description}`.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
  );
  const validIdentity = /^[a-z0-9][a-z0-9-]{0,63}$/.test(name) && !!displayName.trim();
  const metadataTooLong = mode !== 'apply' && (new TextEncoder().encode(name.trim()).length > 160 || new TextEncoder().encode(description.trim()).length > 4000);
  const canSubmit = !!name.trim() && !metadataTooLong && !busy && ((mode !== 'export' && mode !== 'exportTeam') || !!agent)
    && (mode !== 'edit' || typeof detail?.revision === 'number');
  const reviewDetail = copyPreview?.preview ?? (detail && mode === 'edit' ? { ...detail, skills, members: detail.kind === 'team' ? members : undefined } : detail);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <CollectionPageHeader icon={Library} title={t('nav.templates')} count={templates.length}
        action={<div className="flex flex-wrap gap-2"><Button variant="outline" onClick={() => open('export')}><Copy />{t('templates.export')}</Button><Button variant="outline" onClick={() => open('exportTeam')}><Copy />{t('templates.exportTeam')}</Button></div>} />
      <div className="mx-auto w-full max-w-6xl space-y-6 p-4 md:p-8">
        <p className="text-sm text-muted-foreground">{t('nav.templates.desc')}</p>
        <div className="flex flex-wrap gap-3">
          <Input className="h-11 min-w-48 flex-1" value={query} onChange={(e) => setQuery(e.target.value)} aria-label={t('templates.search')} placeholder={t('templates.search')} />
          <Picker label={t('templates.kind')} value={kind} onChange={setKind} options={['all', 'agent', 'team'].map((v) => ({ value: v, label: t(`templates.${v}`) }))} />
          <Picker label={t('templates.source')} value={source} onChange={setSource} options={['all', 'builtin', 'company'].map((v) => ({ value: v, label: t(`templates.${v}`) }))} />
        </div>
        {premiumLocked && <p className="text-sm text-muted-foreground">{t('templates.premiumLocked')}</p>}
        {loading ? <CollectionPageState state="loading" title={t('common.loading')} /> : error ? (
          <CollectionPageState state="error" title={t('templates.loadError')} description={error} action={<Button onClick={() => void load()}>{t('templates.retry')}</Button>} />
        ) : (
          <div className="grid items-start gap-6 lg:grid-cols-2">
            <div className="space-y-3">
              {!visible.length && <CollectionPageState state="empty" title={t('templates.empty')} />}
              {visible.map((item) => (
                <button key={item.id} type="button" aria-pressed={selectedId === item.id} onClick={() => setSelectedId(item.id)}
                  className="w-full rounded-xl border border-surface-border bg-surface p-5 text-left shadow-[var(--surface-shadow)] hover:bg-surface-hover focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50 aria-pressed:border-brand aria-pressed:bg-surface-selected">
                  <div className="flex items-center gap-3">
                    <ActorAvatar actorType={item.kind === 'team' ? 'squad' : 'agent'} name={item.name} size="lg" />
                    <span className="min-w-0 flex-1 truncate font-medium" title={item.name}>{item.name}</span>
                    <Badge variant="secondary">{t(`templates.${item.source}`)}</Badge>
                  </div>
                  <p className="mt-3 line-clamp-2 text-sm text-muted-foreground">{item.description}</p>
                  <p className="mt-3 text-xs text-muted-foreground">{t(`templates.${item.kind}`)}</p>
                </button>
              ))}
            </div>
            <section aria-label={t('templates.detail')} className="min-w-0">
              {detailLoading ? <CollectionPageState state="loading" title={t('common.loading')} /> : detailError ? (
                <CollectionPageState state="error" title={t('templates.loadError')} description={detailError} action={<Button onClick={() => setDetailReload((value) => value + 1)}>{t('templates.retry')}</Button>} />
              ) : detail ? (
                <Card>
                  <CardHeader><CardTitle>{detail.name}</CardTitle><p className="text-sm text-muted-foreground">{detail.description}</p></CardHeader>
                  <CardContent className="space-y-5">
                    <TemplateTexts value={detail} empty={t('templates.none')} />
                    <Detail label={t('templates.skills')}><TagList values={detail.skills} empty={t('templates.none')} /></Detail>
                    <Detail label={t('templates.allowed')}><TagList values={detail.allowed_tools} empty={t('templates.defaultTools')} /></Detail>
                    <Detail label={t('templates.denied')}><TagList values={detail.denied_tools} empty={t('templates.none')} /></Detail>
                    {detail.permissions && <Detail label={t('agents.edit.section.permissions')}><dl className="space-y-2">{Object.entries(permissionLabels).filter(([key]) => typeof detail.permissions?.[key] === 'boolean').map(([key, label]) => <div key={key} className="flex justify-between gap-4"><dt>{t(label)}</dt><dd>{t(detail.permissions?.[key] ? 'templates.enabled' : 'templates.disabled')}</dd></div>)}</dl></Detail>}
                    {Array.isArray(detail.permissions?.allowed_channels) && detail.permissions.allowed_channels.length > 0 && <Detail label={t('channels.detail.behavior.allowedChannels')}><TagList values={detail.permissions.allowed_channels} empty={t('templates.none')} /></Detail>}
                    <div className="grid grid-cols-2 gap-4">
                      <Detail label={t('templates.model')}>{detail.model || t('templates.default')}</Detail>
                      <Detail label={t('templates.runtime')}>{detail.runtime || t('templates.default')}</Detail>
                    </div>
                    {!!detail.members?.length && <Detail label={t('templates.members')}><div className="space-y-3">{detail.members.map((member) => <details key={member.id} className="rounded-lg border p-3"><summary className="cursor-pointer font-medium">{member.name}</summary><div className="mt-3 space-y-3"><Detail label={t('templates.skills')}><TagList values={member.skills} empty={t('templates.none')} /></Detail><Detail label={t('templates.allowed')}><TagList values={member.allowed_tools} empty={t('templates.defaultTools')} /></Detail><Detail label={t('templates.denied')}><TagList values={member.denied_tools} empty={t('templates.none')} /></Detail>{member.permissions && <Detail label={t('agents.edit.section.permissions')}><dl>{Object.entries(permissionLabels).filter(([key]) => typeof member.permissions?.[key] === 'boolean').map(([key, label]) => <div key={key} className="flex justify-between gap-4"><dt>{t(label)}</dt><dd>{t(member.permissions?.[key] ? 'templates.enabled' : 'templates.disabled')}</dd></div>)}</dl></Detail>}<p>{member.model || t('templates.default')} · {member.runtime || t('templates.default')}</p></div></details>)}</div></Detail>}
                    <div className="flex flex-wrap gap-2">
                      <Button variant="brand" className="h-11" disabled={detail.locked} onClick={() => open('apply')}>{t('templates.apply')}</Button>
                      {<Button variant="outline" className="h-11" disabled={detail.locked} onClick={() => open(detail.source === 'company' ? 'edit' : 'copy')}>{t(detail.source === 'company' ? 'templates.edit' : 'templates.copy')}</Button>}
                      {detail.source === 'company' && <Button variant="outline" className="h-11" disabled={detail.locked} onClick={() => open('copy')}>{t('templates.copy')}</Button>}
                    </div>
                    {detail.kind === 'team' && <p className="text-xs text-muted-foreground">{t('templates.teamCopyHelp')}</p>}
                    {detail.source === 'builtin' && <p className="text-xs text-muted-foreground">{t('templates.readOnly')}</p>}
                  </CardContent>
                </Card>
              ) : <CollectionPageState state="empty" title={t('templates.select')} />}
            </section>
          </div>
        )}
      </div>
      <Dialog open={mode !== null} onOpenChange={(value) => { if (!value && !busy) setMode(null); }}>
        <DialogContent className="max-h-[90svh] overflow-y-auto sm:max-w-2xl" showClose={!busy}>
          <DialogHeader><DialogTitle>{t(`templates.${mode ?? 'edit'}`)}</DialogTitle><DialogDescription>{t(mode === 'apply' ? 'templates.applyHelp' : 'templates.copyHelp')}</DialogDescription></DialogHeader>
          {result ? (
            <div className="space-y-4" role="status">
              {!(result.warning === 'seat_limit' && result.agents.length === 0) && <p>{t(result.success ? 'templates.applied' : 'templates.partial')}</p>}
              {seatLimitOf(result.seat_limit) ? <SeatLimitNotice info={seatLimitOf(result.seat_limit)!} />
                : result.error && <p role="alert" className="whitespace-pre-wrap break-words text-sm text-destructive">{result.error}</p>}
              {result.warning === 'template_changed' && <p role="alert">{t('templates.templateChanged')}</p>}
              <ul className="space-y-2">{result.agents.map((id) => <li key={id}><Link className="inline-flex items-center gap-2 text-brand underline" to={`/agents/${encodeURIComponent(id)}`}><ActorAvatar actorType="agent" name={id} />{id}</Link></li>)}</ul>
            </div>
          ) : review ? (
            <div className="space-y-4">
              <Detail label={t(mode === 'apply' ? 'templates.displayName' : 'templates.name')}>{mode === 'apply' ? `${displayName} · ${name}` : name}</Detail>
              {mode === 'apply' && <>
                <Detail label={t('templates.department')}>{department || t('templates.none')}</Detail>
                <Detail label={t('templates.boundUsers')}>{boundUsers.map((id) => users.find((user) => user.id === id)?.display_name ?? id).join(', ') || t('templates.none')}</Detail>
                {detail?.kind === 'team' && <p className="text-sm text-muted-foreground">{t('templates.teamHelp')}</p>}
              </>}
              <p className="text-sm">{t('templates.reviewWarning')}</p>
              {reviewDetail && <TemplateTexts value={reviewDetail} empty={t('templates.none')} />}
              <label className="flex min-h-11 items-start gap-3 text-sm"><Checkbox checked={reviewed} onCheckedChange={(checked) => setReviewed(checked === true)} />{t('templates.reviewed')}</label>
            </div>
          ) : mode === 'apply' ? (
            <div className="space-y-5">
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label={t('templates.employeeId')}><Input className="h-11" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} /><span className="text-xs text-muted-foreground">{t('templates.idHelp')}</span></Field>
                <Field label={t('templates.displayName')}><Input className="h-11" value={displayName} maxLength={200} onChange={(e) => setDisplayName(e.target.value)} /></Field>
              </div>
              {placementLoading ? <p role="status">{t('common.loading')}</p> : placementError ? <div role="alert"><p>{placementError}</p><Button onClick={() => void loadPlacement()}>{t('templates.retry')}</Button></div> : <>
                {!isPersonal && <Picker label={t('templates.department')} value={department} onChange={setDepartment} options={[{ value: '', label: t('templates.none') }, ...departments.map((item) => ({ value: item.name, label: item.name }))]} />}
                {!isPersonal && <fieldset className="space-y-3"><legend className="mb-2 text-sm font-medium">{t('templates.boundUsers')}</legend><p className="text-xs text-muted-foreground">{t('templates.bindHelp')}</p>
                  {!users.length && <p className="text-sm text-muted-foreground">{t('templates.none')}</p>}
                  {users.map((user) => <label key={user.id} className="flex min-h-11 items-center gap-3"><Checkbox checked={boundUsers.includes(user.id)} onCheckedChange={(checked) => setBoundUsers((prev) => checked ? [...prev, user.id] : prev.filter((id) => id !== user.id))} /><ActorAvatar actorType="user" name={user.display_name} />{user.display_name}</label>)}
                </fieldset>}
              </>}
            </div>
          ) : (
            <div className="space-y-4">
              {(mode === 'export' || mode === 'exportTeam') && <Picker label={t(mode === 'exportTeam' ? 'templates.teamRoot' : 'templates.employee')} value={agent} onChange={setAgent} options={[{ value: '', label: t('templates.chooseEmployee') }, ...agents.map((item) => ({ value: item.name, label: item.display_name || item.name }))]} />}
              <Field label={t('templates.name')}><Input className="h-11" value={name} maxLength={160} onChange={(e) => setName(e.target.value)} /></Field>
              <Field label={t('templates.description')}><Textarea value={description} maxLength={4000} onChange={(e) => setDescription(e.target.value)} /></Field>
              {mode === 'edit' && detail?.kind === 'team' && <div className="space-y-4">
                <p className="text-sm text-muted-foreground">{t('templates.teamEditHelp')}</p>
                {members.map((member, index) => {
                  const change = (patch: Partial<LibraryTemplateDetail>) => setMembers((prev) => prev.map((m) => m.id === member.id ? { ...m, ...patch } : m));
                  const original = detail.members?.find((m) => m.id === member.id);
                  return <fieldset key={member.id} className="space-y-3 rounded-lg border p-4"><legend className="px-2 font-medium">{member.name}</legend>
                    <Field label={t('templates.name')}><Input value={member.name} maxLength={160} onChange={(e) => change({ name: e.target.value })} /></Field>
                    <Field label={t('templates.description')}><Textarea value={member.description} maxLength={4000} onChange={(e) => change({ description: e.target.value })} /></Field>
                    <div>{original?.skills.map((skill) => <label key={skill} className="flex min-h-11 items-center gap-3"><Checkbox checked={member.skills.includes(skill)} onCheckedChange={(checked) => change({ skills: checked ? [...member.skills, skill] : member.skills.filter((s) => s !== skill) })} />{skill}</label>)}</div>
                    <Field label={t('templates.allowed')}><Textarea value={member.allowed_tools.join('\n')} onChange={(e) => change({ allowed_tools: e.target.value.split('\n') })} /></Field>
                    <Field label={t('templates.denied')}><Textarea value={member.denied_tools.join('\n')} onChange={(e) => change({ denied_tools: e.target.value.split('\n') })} /></Field>
                    {index > 0 && <Button variant="outline" onClick={() => setMembers((prev) => {
                      const removed = new Set([member.id]);
                      for (let i = 0; i < prev.length; i++) for (const m of prev) if (m.parent_id && removed.has(m.parent_id)) removed.add(m.id);
                      return prev.filter((m) => !removed.has(m.id));
                    })}>{t('templates.removeMember')}</Button>}
                  </fieldset>;
                })}
              </div>}
              {mode === 'edit' && detail?.kind === 'agent' && <>
                <p className="text-sm text-muted-foreground">{t('templates.narrowHelp')}</p>
                <fieldset><legend className="mb-2 text-sm font-medium">{t('templates.skills')}</legend>{detail?.skills.map((skill) => <label key={skill} className="flex min-h-11 items-center gap-3"><Checkbox checked={skills.includes(skill)} onCheckedChange={(checked) => setSkills((prev) => checked ? [...prev, skill] : prev.filter((item) => item !== skill))} />{skill}</label>)}</fieldset>
                <Field label={t('templates.allowed')}><Textarea value={allowed} onChange={(e) => setAllowed(e.target.value)} /><span className="text-xs text-muted-foreground">{t('templates.toolsHelp')}</span></Field>
                <Field label={t('templates.denied')}><Textarea value={denied} onChange={(e) => setDenied(e.target.value)} /></Field>
              </>}
            </div>
          )}
          {metadataTooLong && <p role="alert" className="text-sm text-destructive">{t('templates.textTooLong')}</p>}
          {actionError && <p role="alert" className="text-sm text-destructive">{actionError}</p>}
          <DialogFooter>
            <Button variant="outline" className="h-11" disabled={busy} onClick={() => setMode(null)}>{t(result ? 'templates.close' : 'common.cancel')}</Button>
            {!result && review && <Button variant="outline" className="h-11" disabled={busy} onClick={() => { setReview(false); setReviewed(false); }}>{t('templates.back')}</Button>}
            {!result && !review ? <Button variant="brand" className="h-11" disabled={!canSubmit || (mode === 'apply' && (!validIdentity || placementLoading || !!placementError))} onClick={() => void beginReview()}>{t(busy ? 'common.loading' : 'templates.review')}</Button> : !result && <Button variant="brand" className="h-11" disabled={!canSubmit || !reviewed} onClick={() => void submit()}>{t(busy ? 'common.loading' : mode === 'apply' ? 'templates.confirm' : 'templates.save')}</Button>}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return <label className="flex flex-col gap-2 text-sm font-medium">{label}{children}</label>;
}
function Detail({ label, children }: { label: string; children: ReactNode }) {
  return <div className="space-y-2 text-sm"><h3 className="text-xs font-medium text-muted-foreground">{label}</h3>{children}</div>;
}
function TagList({ values, empty }: { values: string[]; empty: string }) {
  return values.length ? <div className="flex flex-wrap gap-2">{values.map((value) => <Badge className="max-w-full whitespace-normal break-all" key={value} variant="outline">{value}</Badge>)}</div> : <p className="text-sm text-muted-foreground">{empty}</p>;
}
function Picker({ label, value, onChange, options }: { label: string; value: string; onChange: (value: string) => void; options: Array<{ value: string; label: string }> }) {
  return <div className="space-y-2"><span className="sr-only">{label}</span><Select value={value} onValueChange={(next) => { if (next !== null) onChange(next); }} items={options}><SelectTrigger className="h-11 w-full min-w-36" aria-label={label}><SelectValue /></SelectTrigger><SelectContent>{options.map((item) => <SelectItem key={item.value} value={item.value}>{item.label}</SelectItem>)}</SelectContent></Select></div>;
}

// Render raw text: Markdown rendering would conceal comments and alter review content.
function TemplateTexts({ value, empty }: { value: LibraryTemplateDetail; empty: string }) {
  return <div className="space-y-4">
    <Detail label="SOUL.md"><pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words text-sm">{value.soul_md || empty}</pre></Detail>
    <Detail label="CONTRACT.toml"><pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words text-sm">{value.contract_toml || empty}</pre></Detail>
    {value.skills.map((skill) => <Detail key={skill} label={`${skill}/SKILL.md`}><pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words text-sm">{value.skill_contents?.[skill] ?? empty}</pre></Detail>)}
    {value.members?.map((member) => <section key={member.id} className="space-y-3 rounded-lg border p-3"><h3 className="font-medium">{member.name}</h3><TemplateTexts value={member} empty={empty} /></section>)}
  </div>;
}
