import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { api, type AgentInfo } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Input } from '@/components/mds';

interface CredentialMetadata {
  id: string;
  label: string;
  login_url: string;
  origin: string;
  login_path: string;
  allowed_paths: string[];
  username: string;
  allowed_agents: string[];
}

interface CredentialForm {
  id?: string;
  label: string;
  login_url: string;
  allowed_paths: string;
  username: string;
  allowed_agents: string[];
}

const emptyForm = (): CredentialForm => ({ label: '', login_url: '', allowed_paths: '', username: '', allowed_agents: [] });

/** Passwords live in the input only until submission, never in list/edit state. */
export function WebsiteCredentialsPanel() {
  const intl = useIntl();
  const isAdmin = useAuthStore((s) => s.user?.role) === 'admin';
  const connection = useConnectionStore((s) => s.state);
  const passwordInput = useRef<HTMLInputElement>(null);
  const [records, setRecords] = useState<CredentialMetadata[]>([]);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [form, setForm] = useState<CredentialForm | null>(null);
  const [deleteId, setDeleteId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [saved, setSaved] = useState(false);
  const t = (key: string) => intl.formatMessage({ id: `websiteCredentials.${key}` });

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [result, roster] = await Promise.all([
        client.call('website_credentials.list', {}) as Promise<{ credentials: CredentialMetadata[] }>,
        api.agents.list(),
      ]);
      // Select the metadata fields explicitly; never retain unexpected values.
      setRecords((result.credentials ?? []).map(({ id, label, login_url, origin, login_path, allowed_paths, username, allowed_agents }) =>
        ({ id, label, login_url, origin, login_path, allowed_paths: allowed_paths ?? [], username, allowed_agents })));
      setAgents(roster.agents ?? []);
      setError(false);
    } catch {
      setError(true);
    } finally { setLoading(false); }
  }, []);

  useEffect(() => {
    if (isAdmin && connection === 'authenticated') void refresh();
    else {
      if (passwordInput.current) passwordInput.current.value = '';
      setRecords([]);
      setForm(null);
    }
  }, [isAdmin, connection, refresh]);

  const closeForm = () => {
    if (passwordInput.current) passwordInput.current.value = '';
    setForm(null);
  };

  const save = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!isAdmin || !form || busy || !form.allowed_agents.length || !passwordInput.current?.value) return;
    setBusy(true);
    setError(false);
    setSaved(false);
    try {
      const request = client.call('website_credentials.upsert', {
        id: form.id, label: form.label, login_url: form.login_url, username: form.username,
        allowed_agents: form.allowed_agents,
        allowed_paths: form.allowed_paths.split('\n').map((path) => path.trim()).filter(Boolean),
        password: passwordInput.current.value,
      });
      passwordInput.current.value = '';
      await request;
      closeForm();
      setSaved(true);
      await refresh();
    } catch {
      // Do not echo RPC errors: an upstream validation error may contain input.
      setError(true);
    } finally {
      if (passwordInput.current) passwordInput.current.value = '';
      setBusy(false);
    }
  };

  const remove = async (id: string) => {
    if (!isAdmin || busy) return;
    setBusy(true);
    setSaved(false);
    setError(false);
    try {
      await client.call('website_credentials.delete', { id });
      setDeleteId(null);
      if (form?.id === id) closeForm();
      await refresh();
    } catch { setError(true); }
    finally { setBusy(false); }
  };

  if (!isAdmin) return null;
  return <Card>
    <CardHeader>
      <CardTitle className="text-sm">{t('title')}</CardTitle>
      <CardDescription>{t('description')}</CardDescription>
    </CardHeader>
    <CardContent className="space-y-4">
      <p className="text-sm text-muted-foreground">{t('limits')}</p>
      {error && <div role="alert" className="space-y-2 text-sm text-destructive">
        <p>{t('error')}</p>
        <Button variant="outline" size="sm" disabled={busy || loading} onClick={() => void refresh()}>{t('retry')}</Button>
      </div>}
      {saved && <p role="status" className="text-sm">{t('saved')}</p>}
      {loading ? <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>
        : records.length === 0 && <p className="text-sm text-muted-foreground">{t('empty')}</p>}
      <ul className="space-y-3">
        {records.map((record) => <li key={record.id} className="rounded-lg border border-surface-border p-3">
          <p className="break-words text-sm font-medium">{record.label}</p>
          <p className="break-all text-sm text-muted-foreground">{record.login_url} · {record.username}</p>
          <p className="break-words text-sm text-muted-foreground">{t('agents')}: {record.allowed_agents.join(', ')}</p>
          <div className="mt-2 flex flex-wrap gap-2">
            <Button variant="outline" size="sm" disabled={busy} onClick={() => {
              if (passwordInput.current) passwordInput.current.value = '';
              setForm({ id: record.id, label: record.label, login_url: record.login_url,
                username: record.username, allowed_agents: [...record.allowed_agents],
                allowed_paths: record.allowed_paths.join('\n') });
              setDeleteId(null);
              setSaved(false);
            }}>{t('edit')}</Button>
            <Button variant="outline" size="sm" disabled={busy} onClick={() => setDeleteId(record.id)}>{t('delete')}</Button>
          </div>
          {deleteId === record.id && <div role="alert" className="mt-3 space-y-2 text-sm">
            <p>{t('deleteConfirm')}</p>
            <div className="flex gap-2">
              <Button variant="destructive" size="sm" disabled={busy} onClick={() => void remove(record.id)}>{t('delete')}</Button>
              <Button variant="outline" size="sm" disabled={busy} onClick={() => setDeleteId(null)}>{t('cancel')}</Button>
            </div>
          </div>}
        </li>)}
      </ul>
      {!form ? <Button variant="outline" size="sm" disabled={busy || loading || error} onClick={() => {
        setForm(emptyForm()); setSaved(false); setDeleteId(null);
      }}>{t('add')}</Button> : <form autoComplete="off" onSubmit={(event) => void save(event)} className="space-y-3">
        <fieldset disabled={busy} className="space-y-3">
          <legend className="mb-2 text-sm font-medium">{t(form.id ? 'edit' : 'add')}</legend>
          {(['label', 'login_url', 'username'] as const).map((field) => <label key={field} className="block space-y-1 text-sm">
            <span>{t(field)}</span>
            <Input required autoComplete="off" type={field === 'login_url' ? 'url' : 'text'}
              pattern={field === 'login_url' ? 'https://.+' : undefined} value={form[field]} onChange={(event) => setForm({ ...form, [field]: event.target.value })} />
          </label>)}
          <label className="block space-y-1 text-sm">
            <span>{t('allowed_paths')}</span>
            <textarea value={form.allowed_paths} rows={3} autoComplete="off"
              className="w-full rounded-md border border-surface-border bg-transparent px-3 py-2"
              aria-describedby="website-login-paths-hint"
              onChange={(event) => setForm({ ...form, allowed_paths: event.target.value })} />
          </label>
          <p id="website-login-paths-hint" className="text-sm text-muted-foreground">{t('allowedPathsHint')}</p>
          <label className="block space-y-1 text-sm">
            <span>{t('password')}</span>
            <Input ref={passwordInput} type="password" autoComplete="off" required />
          </label>
          <p className="text-sm text-muted-foreground">{t('passwordHint')}</p>
          <fieldset className="space-y-2">
            <legend className="mb-1 text-sm">{t('agents')}</legend>
            {agents.length === 0 && <p className="text-sm text-muted-foreground">{t('noAgents')}</p>}
            {agents.map((agent) => <label key={agent.name} className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={form.allowed_agents.includes(agent.name)} onChange={(event) => setForm({
                ...form, allowed_agents: event.target.checked
                  ? [...form.allowed_agents, agent.name]
                  : form.allowed_agents.filter((id) => id !== agent.name),
              })} />
              {agent.display_name || agent.name} ({agent.name})
            </label>)}
          </fieldset>
          <div className="flex flex-wrap gap-2">
            <Button type="submit" size="sm" disabled={!form.allowed_agents.length}>{t('save')}</Button>
            <Button type="button" variant="outline" size="sm" onClick={closeForm}>{t('cancel')}</Button>
          </div>
        </fieldset>
      </form>}
    </CardContent>
  </Card>;
}
