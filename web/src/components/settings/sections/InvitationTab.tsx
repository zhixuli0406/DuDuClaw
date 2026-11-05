import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { formatError, toast } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import { Button, Card, CardContent, Input, SettingsSection, Textarea } from '@/components/mds';
import { OptionSelect } from '@/components/settings/controls';

interface InvitationContent {
  opening: string;
  steps: string;
  examples: [string, string, string];
  course_url: string;
}
interface Preview { subject: string; text: string; html: string }

export function InvitationTab() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const isAdmin = useAuthStore(s => s.user?.role === 'admin');
  const userId = useAuthStore(s => s.user?.id);
  const [locale, setLocale] = useState(intl.locale === 'ja-JP' ? 'ja-JP' : intl.locale === 'en' ? 'en' : 'zh-TW');
  const [content, setContent] = useState<InvitationContent | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [previewError, setPreviewError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let alive = true;
    setContent(null); setPreview(null); setLoadError(null);
    if (isAdmin) void api.invitation.get(locale).then(result => {
      if (alive) setContent(result.content);
    }).catch(error => { if (alive) setLoadError(error); });
    return () => { alive = false; };
  }, [isAdmin, userId, locale, revision]);

  useEffect(() => {
    let alive = true;
    setPreview(null); setPreviewError(null);
    if (!isAdmin || !content) return;
    const timer = setTimeout(() => {
      void api.invitation.preview({ locale, content }).then(result => {
        if (alive) setPreview(result);
      }).catch(error => { if (alive) setPreviewError(error); });
    }, 300);
    return () => { alive = false; clearTimeout(timer); };
  }, [isAdmin, userId, locale, content]);

  if (!isAdmin) return null;
  const save = async () => {
    if (!content || saving) return;
    setSaving(true);
    try {
      const result = await api.invitation.update(locale, content);
      if (useAuthStore.getState().user?.id !== userId) return;
      setContent(result.content);
      toast.success(t('invitation.saved'));
    } catch (error) { if (useAuthStore.getState().user?.id === userId) toast.error(formatError(error)); }
    finally { setSaving(false); }
  };
  const validCourse = !content?.course_url || (() => {
    try { const url = new URL(content.course_url); return url.protocol === 'https:' && !url.username && !url.password; }
    catch { return false; }
  })();

  return <div className="space-y-8">
    <div className="space-y-2">
      <label htmlFor="invitation-locale" className="text-sm font-medium">{t('invitation.language')}</label>
      <OptionSelect id="invitation-locale" value={locale} onChange={setLocale} disabled={saving} showRaw={false}
        options={[{ value: 'zh-TW', label: '繁體中文' }, { value: 'en', label: 'English' }, { value: 'ja-JP', label: '日本語' }]} />
      <p className="text-xs text-muted-foreground">{t('invitation.languageHint')}</p>
    </div>
    {loadError != null ? <div role="alert" className="space-y-3 text-sm">
      <p>{formatError(loadError)}</p><Button variant="outline" onClick={() => setRevision(r => r + 1)}>{t('errorState.retry')}</Button>
    </div> : !content ? <p role="status" className="text-sm text-muted-foreground">{t('common.loading')}</p> : <>
      <SettingsSection title={t('invitation.content')} description={t('invitation.contentHint')}>
        <Card><CardContent className="space-y-5">
          <div className="space-y-2"><label htmlFor="invitation-opening" className="text-sm font-medium">{t('invitation.opening')}</label>
            <Textarea id="invitation-opening" rows={3} maxLength={4000} disabled={saving} value={content.opening} onChange={e => setContent({ ...content, opening: e.target.value })} /></div>
          <div className="space-y-2"><label htmlFor="invitation-steps" className="text-sm font-medium">{t('invitation.steps')}</label>
            <Textarea id="invitation-steps" rows={5} maxLength={8000} disabled={saving} value={content.steps} onChange={e => setContent({ ...content, steps: e.target.value })} /></div>
          {content.examples.map((example, index) => <div key={index} className="space-y-2">
            <label htmlFor={`invitation-example-${index}`} className="text-sm font-medium">{intl.formatMessage({ id: 'invitation.example' }, { number: index + 1 })}</label>
            <Textarea id={`invitation-example-${index}`} rows={2} maxLength={1000} disabled={saving} value={example} onChange={e => {
              const examples: [string, string, string] = [...content.examples]; examples[index] = e.target.value;
              setContent({ ...content, examples });
            }} />
          </div>)}
          <div className="space-y-2"><label htmlFor="invitation-course" className="text-sm font-medium">{t('invitation.course')}</label>
            <Input id="invitation-course" type="url" maxLength={2048} disabled={saving} value={content.course_url} aria-invalid={!validCourse} aria-describedby="invitation-course-hint" onChange={e => setContent({ ...content, course_url: e.target.value })} />
            <p id="invitation-course-hint" className="text-xs text-muted-foreground">{t('invitation.courseHint')}</p>
          </div>
        </CardContent></Card>
      </SettingsSection>
      <Button variant="brand" disabled={saving || !validCourse || !content.opening.trim() || !content.steps.trim() || content.examples.some(example => !example.trim())} onClick={() => void save()}>{t(saving ? 'common.saving' : 'common.save')}</Button>
      <SettingsSection title={t('invitation.preview')} description={t('invitation.previewHint')}>
        {previewError != null ? <p role="alert" className="text-sm text-destructive">{formatError(previewError)}</p>
          : preview ? <Card><CardContent className="space-y-3">
            <p className="text-sm font-medium">{preview.subject}</p>
            <iframe title={t('invitation.preview')} sandbox="" referrerPolicy="no-referrer" srcDoc={preview.html} className="h-96 w-full rounded-lg border border-border bg-background" />
          </CardContent></Card>
            : <p role="status" className="text-sm text-muted-foreground">{t('common.loading')}</p>}
      </SettingsSection>
    </>}
  </div>;
}
