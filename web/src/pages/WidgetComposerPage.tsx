import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  BreadcrumbHeader,
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  Button,
  Input,
  Textarea,
  Tabs,
  TabsList,
  TabsTab,
  SubmitButton,
} from '@/components/mds';
import { CustomWidgetFrame } from '@/components/home/CustomWidgetFrame';
import { RefreshCw } from 'lucide-react';

/** Guided data-source picker options → bridge methods the prompt cites. */
const DATA_SOURCES = ['agents', 'tasks', 'cost', 'channels', 'system'] as const;
const STYLES = ['stat', 'list', 'bars', 'free'] as const;

/** A pill-style toggle chip (data source / style). */
function Chip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'rounded-full border px-3 py-1 text-xs font-medium transition-colors',
        active
          ? 'border-brand bg-brand/10 text-brand'
          : 'border-input text-muted-foreground hover:bg-muted',
      )}
    >
      {children}
    </button>
  );
}

/**
 * `/widgets/new` + `/widgets/:id/edit` — the widget composer (guided
 * natural-language flow for everyone; raw HTML mode for admins via `?mode=html`
 * or when editing an html-origin widget), re-skinned onto MDS. Preview always
 * runs in the SAME sandbox as the dashboard, so what you see is what ships.
 */
export function WidgetComposerPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { id: editId } = useParams<{ id: string }>();
  const [searchParams] = useSearchParams();
  const user = useAuthStore((s) => s.user);
  const isAdmin = hasMinRole(user?.role, 'admin');

  const [htmlMode, setHtmlMode] = useState(searchParams.get('mode') === 'html');
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [sources, setSources] = useState<string[]>([]);
  const [style, setStyle] = useState<string>('stat');
  const [freeform, setFreeform] = useState('');
  const [feedback, setFeedback] = useState('');
  const [html, setHtml] = useState<string | null>(null); // generated / edited draft
  const [htmlDraft, setHtmlDraft] = useState('');        // html-mode textarea
  const [generating, setGenerating] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Edit mode: seed the composer from the stored widget.
  useEffect(() => {
    if (!editId) return;
    let alive = true;
    api.widgetsCustom
      .get(editId)
      .then((w) => {
        if (!alive) return;
        setTitle(w.title);
        setDescription(w.description);
        setHtml(w.html);
        setHtmlDraft(w.html);
        if (w.origin === 'html') setHtmlMode(true);
      })
      .catch((e) => alive && setLoadError(formatError(e)));
    return () => {
      alive = false;
    };
  }, [editId]);

  const toggleSource = (s: string) =>
    setSources((cur) => (cur.includes(s) ? cur.filter((x) => x !== s) : [...cur, s]));

  const generate = async (revise: boolean) => {
    if (!revise && sources.length === 0 && freeform.trim() === '') {
      toast.error(intl.formatMessage({ id: 'widgets.compose.needInput' }));
      return;
    }
    setGenerating(true);
    try {
      const r = await api.widgetsCustom.generate({
        prompt: freeform,
        style: intl.formatMessage({ id: `widgets.compose.style.${style}` }),
        data_sources: sources.map((s) => intl.formatMessage({ id: `widgets.compose.source.${s}` })),
        ...(revise && html ? { prior_html: html, feedback } : {}),
      });
      setHtml(r.html);
      setFeedback('');
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setGenerating(false);
    }
  };

  const save = async () => {
    const finalHtml = htmlMode ? htmlDraft : html;
    if (!finalHtml || finalHtml.trim() === '') {
      toast.error(intl.formatMessage({ id: 'widgets.compose.needHtml' }));
      return;
    }
    if (title.trim() === '') {
      toast.error(intl.formatMessage({ id: 'widgets.compose.needTitle' }));
      return;
    }
    setSaving(true);
    try {
      if (editId) {
        await api.widgetsCustom.update(editId, { title, description, html: finalHtml });
      } else {
        await api.widgetsCustom.create({
          title,
          description,
          html: finalHtml,
          origin: htmlMode ? 'html' : 'ai',
        });
      }
      toast.success(intl.formatMessage({ id: 'widgets.compose.saved' }));
      navigate('/widgets');
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const previewHtml = htmlMode ? htmlDraft : html;
  const heading = useMemo(() => {
    if (editId) return intl.formatMessage({ id: 'widgets.compose.editTitle' });
    return intl.formatMessage({ id: htmlMode ? 'widgets.newHtml' : 'widgets.new' });
  }, [editId, htmlMode, intl]);

  if (loadError) {
    return (
      <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
        <BreadcrumbHeader
          hideTrigger
          segments={[
            { label: intl.formatMessage({ id: 'widgets.title' }), onClick: () => navigate('/widgets') },
            { label: heading },
          ]}
        />
        <div className="mx-auto w-full max-w-6xl p-6">
          <Card>
            <CardContent>
              <p className="py-4 text-center text-sm text-muted-foreground">{loadError}</p>
              <div className="flex justify-center">
                <Button variant="outline" onClick={() => navigate('/widgets')}>
                  {intl.formatMessage({ id: 'common.back' })}
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    );
  }

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <BreadcrumbHeader
        hideTrigger
        segments={[
          { label: intl.formatMessage({ id: 'widgets.title' }), onClick: () => navigate('/widgets') },
          { label: heading },
        ]}
        actions={
          <Button variant="brand" size="sm" onClick={() => void save()} disabled={saving || !previewHtml}>
            {intl.formatMessage({ id: saving ? 'common.saving' : 'common.save' })}
          </Button>
        }
      />

      <div className="mx-auto w-full max-w-6xl p-4 md:p-6">
        <div className="grid gap-6 lg:grid-cols-2">
          {/* ── Left: authoring ── */}
          <div className="space-y-4">
            <Card>
              <CardHeader>
                <CardTitle>{intl.formatMessage({ id: 'widgets.compose.metaTitle' })}</CardTitle>
              </CardHeader>
              <CardContent className="space-y-3">
                <Input
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  placeholder={intl.formatMessage({ id: 'widgets.compose.titlePlaceholder' })}
                />
                <Input
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                  placeholder={intl.formatMessage({ id: 'widgets.compose.descPlaceholder' })}
                />
              </CardContent>
            </Card>

            {/* Admin can switch between the guided flow and raw HTML. */}
            {isAdmin && (
              <Tabs
                variant="line"
                value={htmlMode ? 'html' : 'guided'}
                onValueChange={(v) => setHtmlMode(v === 'html')}
              >
                <TabsList>
                  <TabsTab value="guided">{intl.formatMessage({ id: 'widgets.compose.guideTitle' })}</TabsTab>
                  <TabsTab value="html">{intl.formatMessage({ id: 'widgets.compose.htmlTitle' })}</TabsTab>
                </TabsList>
              </Tabs>
            )}

            {htmlMode && isAdmin ? (
              <Card>
                <CardHeader>
                  <CardTitle>{intl.formatMessage({ id: 'widgets.compose.htmlTitle' })}</CardTitle>
                </CardHeader>
                <CardContent>
                  <p className="mb-2 text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'widgets.compose.htmlHint' })}
                  </p>
                  <Textarea
                    value={htmlDraft}
                    onChange={(e) => setHtmlDraft(e.target.value)}
                    rows={20}
                    spellCheck={false}
                    className="font-mono text-xs leading-relaxed"
                    placeholder={'<div>...</div>\n<script>\n  duduclaw.call(\'agents.summary\').then(...)\n</script>'}
                  />
                </CardContent>
              </Card>
            ) : (
              <>
                <Card>
                  <CardHeader>
                    <CardTitle>{intl.formatMessage({ id: 'widgets.compose.guideTitle' })}</CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-4">
                    <div>
                      <p className="mb-1.5 text-sm font-medium text-foreground">
                        {intl.formatMessage({ id: 'widgets.compose.sourcesLabel' })}
                      </p>
                      <div className="flex flex-wrap gap-2">
                        {DATA_SOURCES.map((s) => (
                          <Chip key={s} active={sources.includes(s)} onClick={() => toggleSource(s)}>
                            {intl.formatMessage({ id: `widgets.compose.source.${s}` })}
                          </Chip>
                        ))}
                      </div>
                    </div>
                    <div>
                      <p className="mb-1.5 text-sm font-medium text-foreground">
                        {intl.formatMessage({ id: 'widgets.compose.styleLabel' })}
                      </p>
                      <div className="flex flex-wrap gap-2">
                        {STYLES.map((s) => (
                          <Chip key={s} active={style === s} onClick={() => setStyle(s)}>
                            {intl.formatMessage({ id: `widgets.compose.style.${s}` })}
                          </Chip>
                        ))}
                      </div>
                    </div>
                    <div>
                      <p className="mb-1.5 text-sm font-medium text-foreground">
                        {intl.formatMessage({ id: 'widgets.compose.freeformLabel' })}
                      </p>
                      <div className="relative">
                        <Textarea
                          value={freeform}
                          onChange={(e) => setFreeform(e.target.value)}
                          rows={3}
                          className="pr-12"
                          placeholder={intl.formatMessage({ id: 'widgets.compose.freeformPlaceholder' })}
                        />
                        <SubmitButton
                          state={generating ? 'submitting' : 'idle'}
                          disabled={generating}
                          onClick={() => void generate(false)}
                          aria-label={intl.formatMessage({ id: 'widgets.compose.generate' })}
                          className="absolute bottom-2 right-2"
                        />
                      </div>
                    </div>
                  </CardContent>
                </Card>

                {html !== null && (
                  <Card>
                    <CardHeader>
                      <CardTitle>{intl.formatMessage({ id: 'widgets.compose.reviseTitle' })}</CardTitle>
                    </CardHeader>
                    <CardContent className="space-y-3">
                      <Textarea
                        value={feedback}
                        onChange={(e) => setFeedback(e.target.value)}
                        rows={2}
                        placeholder={intl.formatMessage({ id: 'widgets.compose.feedbackPlaceholder' })}
                      />
                      <Button
                        variant="outline"
                        disabled={generating || feedback.trim() === ''}
                        onClick={() => void generate(true)}
                      >
                        <RefreshCw />
                        {intl.formatMessage({ id: generating ? 'widgets.compose.generating' : 'widgets.compose.revise' })}
                      </Button>
                    </CardContent>
                  </Card>
                )}
              </>
            )}
          </div>

          {/* ── Right: live sandbox preview (the real runtime, not a mock) ── */}
          <div>
            {previewHtml && previewHtml.trim() !== '' ? (
              <CustomWidgetFrame
                html={previewHtml}
                title={title || intl.formatMessage({ id: 'widgets.compose.previewTitle' })}
              />
            ) : (
              <Card>
                <CardHeader>
                  <CardTitle>{intl.formatMessage({ id: 'widgets.compose.previewTitle' })}</CardTitle>
                </CardHeader>
                <CardContent>
                  <p className="py-10 text-center text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'widgets.compose.previewEmpty' })}
                  </p>
                </CardContent>
              </Card>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
