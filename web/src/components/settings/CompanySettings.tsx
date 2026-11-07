import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input, SettingsCard, SettingsRow, Spinner } from '@/components/mds';
import { ProductLogo } from '@/components/brand/ProductLogo';
import { useEffectiveName } from '@/lib/product-identity';
import { client } from '@/lib/ws-client';
import { formatError } from '@/lib/toast';
import { parseCompany, useCompanyStore } from '@/stores/company-store';
import { useAuthStore } from '@/stores/auth-store';

export const COMPANY_LOGO_MAX_BYTES = 1024 * 1024;
export function validCompanyLogo(file: Pick<File, 'size' | 'type'>): boolean {
  return file.size > 0 && file.size <= COMPANY_LOGO_MAX_BYTES
    && ['image/png', 'image/jpeg', 'image/webp'].includes(file.type);
}

export function CompanySettings() {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `companyInfo.${key}` });
  const productName = useEffectiveName();
  const [name, setName] = useState('');
  const [logoUrl, setLogoUrl] = useState<string | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [removeLogo, setRemoveLogo] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  const fileInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError('');
    void client.call('company.get', {}).then((value) => {
      if (!active) return;
      const company = parseCompany(value);
      setName(company.name);
      setLogoUrl(company.logo_url);
      setLoaded(true);
      useCompanyStore.getState().setCompany(company);
    }).catch((err: unknown) => { if (active) setError(formatError(err)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [attempt]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!loaded || saving) return;
    setSaving(true);
    setError('');
    setSaved(false);
    try {
      if (file && !useAuthStore.getState().jwt) throw new Error('HTTP 401');
      const params: { name: string; remove_logo?: boolean } = { name: name.trim() };
      if (!file && removeLogo) params.remove_logo = true;
      let company = parseCompany(await client.call('company.update', params));
      // The name is already persisted even if the separate upload fails.
      useCompanyStore.getState().setCompany(company);
      setName(company.name);
      setLogoUrl(company.logo_url);
      if (file) {
        const jwt = useAuthStore.getState().jwt;
        if (!jwt) throw new Error('HTTP 401');
        const response = await fetch('/api/company/logo', {
          method: 'POST',
          headers: { Authorization: `Bearer ${jwt}`, 'Content-Type': file.type },
          body: file,
          credentials: 'omit',
          redirect: 'error',
        });
        if (!response.ok) {
          const body: unknown = await response.json().catch(() => null);
          const message = body && typeof body === 'object' && 'error' in body
            && typeof body.error === 'string' ? body.error : `HTTP ${response.status}`;
          throw new Error(message);
        }
        company = parseCompany(await response.json());
        useCompanyStore.getState().setCompany(company);
        setName(company.name);
        setLogoUrl(company.logo_url);
      }
      setFile(null);
      setRemoveLogo(false);
      if (fileInput.current) fileInput.current.value = '';
      setSaved(true);
    } catch (err) {
      setError(formatError(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) return <Spinner label={t('loading')} />;
  if (!loaded) return <div className="space-y-4">
    <p role="alert" className="text-sm text-destructive">{error}</p>
    <Button variant="outline" onClick={() => setAttempt((value) => value + 1)}>{t('retry')}</Button>
  </div>;

  return <form onSubmit={submit} className="space-y-6">
    <SettingsCard>
      <SettingsRow label={<label htmlFor="company-name">{t('name')}</label>} description={t('nameHint')} tier="text">
        <Input className="h-11" id="company-name" value={name} disabled={saving} maxLength={120}
          onChange={(event) => { setName(event.target.value); setSaved(false); }} />
      </SettingsRow>
      <SettingsRow label={<label htmlFor="company-logo">{t('logo')}</label>} description={t('logoHint')} tier="text">
        <div className="space-y-3">
          {logoUrl && !removeLogo ? <img src={logoUrl} alt={name || productName} className="size-16 object-contain" />
            : <ProductLogo size={64} title={productName} />}
          <Input className="h-11" ref={fileInput} id="company-logo" type="file" accept="image/png,image/jpeg,image/webp" disabled={saving}
            onChange={(event) => {
              const candidate = event.target.files?.[0];
              setSaved(false);
              if (!candidate) { setFile(null); return; }
              if (!validCompanyLogo(candidate)) {
                setError(t('invalidLogo'));
                setFile(null);
                event.target.value = '';
                return;
              }
              setError('');
              setFile(candidate);
              setRemoveLogo(false);
            }} />
          {(logoUrl || file) && <Button type="button" variant="outline" disabled={saving || removeLogo}
            onClick={() => {
              setRemoveLogo(true); setFile(null); setSaved(false);
              if (fileInput.current) fileInput.current.value = '';
            }}>{t('removeLogo')}</Button>}
          {removeLogo && <p className="text-xs text-muted-foreground">{t('removePending')}</p>}
        </div>
      </SettingsRow>
    </SettingsCard>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {saved && <p role="status" className="text-sm text-success">{t('saved')}</p>}
    <Button type="submit" className="min-h-11" disabled={saving}>{saving ? t('saving') : t('save')}</Button>
  </form>;
}
