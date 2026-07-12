import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Lock, Upload, Trash2, Package, Download, RotateCcw } from 'lucide-react';
import { api, type BrandingSetInput } from '@/lib/api';
import {
  useBrandingStore,
  brandLogoFrom,
  logoIsImage,
  DEFAULT_BRAND_LOGO,
} from '@/lib/branding';
import { readFileAsBase64 } from '@/lib/attachments';
import { toast, formatError } from '@/lib/toast';
import { Card, Button, Field, controlClass } from '@/components/ui';

/**
 * BrandingTab (design-distributor-white-label §4.4) — the distributor "品牌設定"
 * surface (SettingsPage ADVANCED group). A distributor whose license carries
 * `white_label` renames the product, uploads a logo, and edits the About-page
 * company block; every screen (Sidebar / title / login / About) reflects it as
 * soon as the save succeeds (the branding store is refreshed in place).
 *
 * When the instance is NOT white-label licensed the whole form is read-only with
 * an explanatory notice — no license/tier/OEM jargon leaks to this audience.
 */

/** 512 KB — must match the backend logo cap. Pre-checked here to fail fast. */
const MAX_LOGO_BYTES = 512 * 1024;
const ACCEPT = '.png,.jpg,.jpeg,.webp';
const ALLOWED_MIME = new Set(['image/png', 'image/jpeg', 'image/webp']);

/** Default brand accent (amber-500 from index.css) shown before a custom pick. */
const DEFAULT_ACCENT = '#f59e0b';

/** Trigger a client-side download of a pretty-printed JSON object. */
function downloadJson(filename: string, obj: unknown): void {
  const blob = new Blob([JSON.stringify(obj, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export function BrandingTab() {
  const intl = useIntl();
  const branding = useBrandingStore((s) => s.branding);
  const whiteLabelActive = useBrandingStore((s) => s.whiteLabelActive);
  const fetchBranding = useBrandingStore((s) => s.fetch);
  const setBranding = useBrandingStore((s) => s.setBranding);
  const fileRef = useRef<HTMLInputElement>(null);
  const cleanupRef = useRef<(() => void) | null>(null);

  const [productName, setProductName] = useState('');
  const [subtitle, setSubtitle] = useState('');
  const [companyName, setCompanyName] = useState('');
  const [website, setWebsite] = useState('');
  const [supportEmail, setSupportEmail] = useState('');
  const [description, setDescription] = useState('');
  // null = unchanged from server; '' = explicit clear; data URI = new/existing.
  const [logo, setLogo] = useState<string>('');
  const [aboutHtml, setAboutHtml] = useState('');
  // '' = default amber (cleared); '#rrggbb' = custom accent.
  const [accentColor, setAccentColor] = useState('');
  const [saving, setSaving] = useState(false);

  // Live-sanitized preview of the About HTML (server-sanitized — never render raw).
  const [previewHtml, setPreviewHtml] = useState('');
  const [previewing, setPreviewing] = useState(false);
  const [bundling, setBundling] = useState(false);

  // Ensure we have the authoritative branding, then hydrate the form from it.
  useEffect(() => {
    fetchBranding();
  }, [fetchBranding]);

  useEffect(() => {
    setProductName(branding?.product_name ?? '');
    setSubtitle(branding?.subtitle ?? '');
    setCompanyName(branding?.company_name ?? '');
    setWebsite(branding?.website ?? '');
    setSupportEmail(branding?.support_email ?? '');
    setDescription(branding?.description ?? '');
    setLogo(branding?.logo_data_uri ?? '');
    setAboutHtml(branding?.about_html ?? '');
    setAccentColor(branding?.accent_color ?? '');
  }, [branding]);

  const locked = !whiteLabelActive;

  // Debounced (~500ms) server-side sanitize for the preview pane. We ONLY ever
  // render the string the backend returns — the raw textarea value is never
  // fed to dangerouslySetInnerHTML.
  useEffect(() => {
    if (locked) return;
    const raw = aboutHtml.trim();
    if (!raw) {
      setPreviewHtml('');
      setPreviewing(false);
      return;
    }
    setPreviewing(true);
    const handle = setTimeout(() => {
      let cancelled = false;
      api.branding
        .preview(raw)
        .then((res) => {
          if (!cancelled) setPreviewHtml(res.sanitized_html);
        })
        .catch(() => {
          if (!cancelled) setPreviewHtml('');
        })
        .finally(() => {
          if (!cancelled) setPreviewing(false);
        });
      // store cancel flag on the timeout closure via ref-less capture
      cleanupRef.current = () => {
        cancelled = true;
      };
    }, 500);
    return () => {
      clearTimeout(handle);
      cleanupRef.current?.();
    };
  }, [aboutHtml, locked]);
  const previewLogo = logo && logoIsImage(logo) ? logo : brandLogoFrom(branding);
  const previewIsImage = logoIsImage(previewLogo);

  const handleFile = async (file: File) => {
    if (!ALLOWED_MIME.has(file.type)) {
      toast.error(intl.formatMessage({ id: 'branding.logo.badType' }));
      return;
    }
    if (file.size > MAX_LOGO_BYTES) {
      toast.error(intl.formatMessage({ id: 'branding.logo.tooLarge' }));
      return;
    }
    try {
      const b64 = await readFileAsBase64(file);
      setLogo(`data:${file.type};base64,${b64}`);
    } catch {
      toast.error(intl.formatMessage({ id: 'branding.logo.readFailed' }));
    }
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      const input: BrandingSetInput = {
        product_name: productName.trim(),
        subtitle: subtitle.trim(),
        company_name: companyName.trim(),
        website: website.trim(),
        support_email: supportEmail.trim(),
        description: description.trim(),
        logo_data_uri: logo,
        about_html: aboutHtml,
        accent_color: accentColor,
      };
      const res = await api.branding.set(input);
      // Refresh the store in place so the Sidebar / title update instantly.
      setBranding(res.branding, true);
      toast.success(intl.formatMessage({ id: 'branding.saved' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const handleReset = async () => {
    setSaving(true);
    try {
      await api.branding.reset();
      await fetchBranding();
      toast.success(intl.formatMessage({ id: 'branding.reset.done' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const handleGenerateBundle = async () => {
    setBundling(true);
    try {
      const res = await api.branding.bundle.create();
      downloadJson('branding.bundle.json', res.bundle);
      toast.success(intl.formatMessage({ id: 'branding.bundle.done' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setBundling(false);
    }
  };

  return (
    <Card title={intl.formatMessage({ id: 'branding.title' })}>
      <div className="space-y-5">
        <p className="text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'branding.intro' })}
        </p>

        {locked && (
          <div className="flex items-start gap-3 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-200">
            <Lock className="mt-0.5 h-4 w-4 shrink-0" />
            <div>
              <p className="font-medium">{intl.formatMessage({ id: 'branding.locked.title' })}</p>
              <p className="mt-0.5 text-amber-700/90 dark:text-amber-300/90">
                {intl.formatMessage({ id: 'branding.locked.body' })}
              </p>
            </div>
          </div>
        )}

        {/* Logo */}
        <Field label={intl.formatMessage({ id: 'branding.logo' })} htmlFor="branding-logo">
          <div className="flex items-center gap-4">
            {previewIsImage ? (
              <img
                src={previewLogo}
                alt=""
                className="h-16 w-16 shrink-0 rounded-2xl object-cover ring-1 ring-stone-300/50 dark:ring-white/10"
              />
            ) : (
              <span
                className="grid h-16 w-16 shrink-0 place-items-center rounded-2xl bg-gradient-to-b from-amber-400 to-amber-500 text-3xl"
                role="img"
                aria-hidden="true"
              >
                {DEFAULT_BRAND_LOGO}
              </span>
            )}
            <div className="flex flex-col gap-2">
              <input
                ref={fileRef}
                id="branding-logo"
                type="file"
                accept={ACCEPT}
                disabled={locked}
                className="hidden"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) void handleFile(f);
                  e.target.value = '';
                }}
              />
              <div className="flex items-center gap-2">
                <Button
                  variant="secondary"
                  disabled={locked}
                  onClick={() => fileRef.current?.click()}
                >
                  <Upload className="mr-1.5 h-4 w-4" />
                  {intl.formatMessage({ id: 'branding.logo.upload' })}
                </Button>
                {previewIsImage && (
                  <Button variant="ghost" disabled={locked} onClick={() => setLogo('')}>
                    <Trash2 className="mr-1.5 h-4 w-4" />
                    {intl.formatMessage({ id: 'branding.logo.remove' })}
                  </Button>
                )}
              </div>
              <p className="text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'branding.logo.hint' })}
              </p>
            </div>
          </div>
        </Field>

        {/* Text fields */}
        <Field label={intl.formatMessage({ id: 'branding.productName' })} htmlFor="branding-name">
          <input
            id="branding-name"
            type="text"
            value={productName}
            disabled={locked}
            maxLength={60}
            onChange={(e) => setProductName(e.target.value)}
            className={controlClass}
            placeholder="DuDuClaw"
          />
        </Field>

        <Field label={intl.formatMessage({ id: 'branding.subtitle' })} htmlFor="branding-subtitle">
          <input
            id="branding-subtitle"
            type="text"
            value={subtitle}
            disabled={locked}
            maxLength={120}
            onChange={(e) => setSubtitle(e.target.value)}
            className={controlClass}
          />
        </Field>

        <Field label={intl.formatMessage({ id: 'branding.description' })} htmlFor="branding-desc">
          <textarea
            id="branding-desc"
            value={description}
            disabled={locked}
            maxLength={500}
            rows={3}
            onChange={(e) => setDescription(e.target.value)}
            className={controlClass}
          />
        </Field>

        <Field label={intl.formatMessage({ id: 'branding.company' })} htmlFor="branding-company">
          <input
            id="branding-company"
            type="text"
            value={companyName}
            disabled={locked}
            maxLength={120}
            onChange={(e) => setCompanyName(e.target.value)}
            className={controlClass}
          />
        </Field>

        <Field label={intl.formatMessage({ id: 'branding.website' })} htmlFor="branding-website">
          <input
            id="branding-website"
            type="url"
            value={website}
            disabled={locked}
            onChange={(e) => setWebsite(e.target.value)}
            className={controlClass}
            placeholder="https://example.com"
          />
        </Field>

        <Field label={intl.formatMessage({ id: 'branding.supportEmail' })} htmlFor="branding-email">
          <input
            id="branding-email"
            type="email"
            value={supportEmail}
            disabled={locked}
            onChange={(e) => setSupportEmail(e.target.value)}
            className={controlClass}
            placeholder="support@example.com"
          />
        </Field>

        {/* Accent color (design §10.4) */}
        <Field label={intl.formatMessage({ id: 'branding.accent' })} htmlFor="branding-accent">
          <div className="flex items-center gap-3">
            <input
              id="branding-accent"
              type="color"
              value={accentColor || DEFAULT_ACCENT}
              disabled={locked}
              onChange={(e) => setAccentColor(e.target.value)}
              className="h-9 w-14 shrink-0 cursor-pointer rounded-lg border border-stone-300/60 bg-transparent p-1 disabled:cursor-not-allowed dark:border-white/10"
            />
            <span className="font-mono text-sm uppercase text-stone-600 dark:text-stone-300">
              {accentColor || DEFAULT_ACCENT}
            </span>
            {accentColor && (
              <Button variant="ghost" disabled={locked} onClick={() => setAccentColor('')}>
                <RotateCcw className="mr-1.5 h-4 w-4" />
                {intl.formatMessage({ id: 'branding.accent.reset' })}
              </Button>
            )}
          </div>
          <p className="mt-1.5 text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'branding.accent.hint' })}
          </p>
        </Field>

        {/* About-page HTML editor + sanitized preview (design §10.2) */}
        <Field label={intl.formatMessage({ id: 'branding.aboutHtml' })} htmlFor="branding-about-html">
          <div className="grid gap-3 lg:grid-cols-2">
            <textarea
              id="branding-about-html"
              value={aboutHtml}
              disabled={locked}
              rows={10}
              spellCheck={false}
              onChange={(e) => setAboutHtml(e.target.value)}
              className={`${controlClass} font-mono text-xs leading-relaxed`}
              placeholder={'<h2>關於我們</h2>\n<p>…</p>'}
            />
            <div className="panel min-h-[10rem] overflow-auto p-4">
              <p className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'branding.aboutHtml.preview' })}
                {previewing && (
                  <span className="ml-2 font-normal normal-case text-stone-400">
                    {intl.formatMessage({ id: 'common.loading' })}
                  </span>
                )}
              </p>
              {previewHtml ? (
                // Safe: `previewHtml` is the backend-sanitized string, never raw input.
                <div
                  className="branding-about-preview text-sm text-stone-700 dark:text-stone-200"
                  dangerouslySetInnerHTML={{ __html: previewHtml }}
                />
              ) : (
                <p className="text-sm text-stone-400 dark:text-stone-500">
                  {intl.formatMessage({ id: 'branding.aboutHtml.previewEmpty' })}
                </p>
              )}
            </div>
          </div>
          <p className="mt-1.5 text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'branding.aboutHtml.hint' })}
          </p>
        </Field>

        {/* Signed branding bundle (design §10.3) */}
        <div className="rounded-card border border-stone-200/60 bg-stone-500/[0.03] p-4 dark:border-white/10 dark:bg-white/[0.02]">
          <div className="flex items-center gap-2">
            <Package className="h-4 w-4 text-stone-500 dark:text-stone-400" />
            <h3 className="text-sm font-semibold tracking-tight text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'branding.bundle.section' })}
            </h3>
          </div>
          <p className="mt-1.5 text-xs leading-relaxed text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'branding.bundle.desc' })}
          </p>
          <div className="mt-3">
            <Button variant="secondary" disabled={locked || bundling} onClick={handleGenerateBundle}>
              <Download className="mr-1.5 h-4 w-4" />
              {bundling
                ? intl.formatMessage({ id: 'branding.bundle.generating' })
                : intl.formatMessage({ id: 'branding.bundle.generate' })}
            </Button>
          </div>
        </div>

        <div className="flex items-center justify-end gap-2 pt-1">
          <Button variant="ghost" onClick={handleReset} disabled={locked || saving}>
            {intl.formatMessage({ id: 'branding.reset' })}
          </Button>
          <Button variant="primary" onClick={handleSave} disabled={locked || saving}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </div>
    </Card>
  );
}
