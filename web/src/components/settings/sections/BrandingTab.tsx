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
import {
  Button,
  Input,
  Textarea,
  SettingsCard,
  SettingsRow,
  SettingsSection,
} from '@/components/mds';

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
  const editableFields = useBrandingStore((s) => s.editableFields);
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

  // WP8 field-level masking (a refinement layered ON TOP of the `locked` gate):
  // even on a white-label instance a customer license may grant only a subset of
  // branding fields. A field NOT in `editableFields` is provider-managed → its
  // control is disabled and a plain-language hint explains why. `canEdit` gates
  // both the control state AND which keys we include in the save payload (so the
  // whole request is never rejected for touching an off-limits field).
  const canEdit = (field: string) => !locked && editableFields.includes(field);
  const fieldDisabled = (field: string) => !canEdit(field);
  const fieldManaged = (field: string) => !locked && !editableFields.includes(field);
  const managedMsg = intl.formatMessage({ id: 'branding.field.managed' });
  const managedTitle = (field: string) => (fieldManaged(field) ? managedMsg : undefined);
  // Shared control props for the uniform text/color inputs — same `disabled` /
  // `title` every field wires by hand. (The logo control is bespoke and opts out.)
  const fieldProps = (field: string) => ({
    disabled: fieldDisabled(field),
    title: managedTitle(field),
  });
  const managedHint = (field: string) =>
    fieldManaged(field) ? (
      <p className="mt-1.5 flex items-center gap-1.5 text-xs text-warning">
        <Lock className="h-3 w-3 shrink-0" />
        {managedMsg}
      </p>
    ) : null;

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
      // Only send fields this instance may edit — provider-managed fields are
      // omitted so the gateway preserves their value instead of rejecting the
      // whole request for touching an off-limits field. Table-driven so the
      // per-field key + value + trim policy lives in one place.
      const saveFields: ReadonlyArray<[keyof BrandingSetInput, string]> = [
        ['product_name', productName.trim()],
        ['subtitle', subtitle.trim()],
        ['company_name', companyName.trim()],
        ['website', website.trim()],
        ['support_email', supportEmail.trim()],
        ['description', description.trim()],
        ['logo_data_uri', logo],
        ['about_html', aboutHtml],
        ['accent_color', accentColor],
      ];
      const input: BrandingSetInput = {};
      for (const [key, value] of saveFields) {
        if (canEdit(key)) (input as Record<string, string>)[key] = value;
      }
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
    <div className="space-y-6">
      <div className="space-y-1">
        <h2 className="text-base font-medium">{intl.formatMessage({ id: 'branding.title' })}</h2>
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'branding.intro' })}
        </p>
      </div>

      {locked && (
        <div className="flex items-start gap-3 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm text-warning">
          <Lock className="mt-0.5 h-4 w-4 shrink-0" />
          <div>
            <p className="font-medium">{intl.formatMessage({ id: 'branding.locked.title' })}</p>
            <p className="mt-0.5">{intl.formatMessage({ id: 'branding.locked.body' })}</p>
          </div>
        </div>
      )}

      {/* Logo (full-width block) */}
      <SettingsSection title={intl.formatMessage({ id: 'branding.logo' })}>
        <div className="flex items-center gap-4">
          {previewIsImage ? (
            <img
              src={previewLogo}
              alt=""
              className="h-16 w-16 shrink-0 rounded-2xl object-cover ring-1 ring-surface-border"
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
              disabled={fieldDisabled('logo_data_uri')}
              className="hidden"
              onChange={(e) => {
                const f = e.target.files?.[0];
                if (f) void handleFile(f);
                e.target.value = '';
              }}
            />
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                disabled={fieldDisabled('logo_data_uri')}
                onClick={() => fileRef.current?.click()}
              >
                <Upload />
                {intl.formatMessage({ id: 'branding.logo.upload' })}
              </Button>
              {previewIsImage && (
                <Button
                  variant="ghost"
                  disabled={fieldDisabled('logo_data_uri')}
                  onClick={() => setLogo('')}
                >
                  <Trash2 />
                  {intl.formatMessage({ id: 'branding.logo.remove' })}
                </Button>
              )}
            </div>
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'branding.logo.hint' })}
            </p>
            {managedHint('logo_data_uri')}
          </div>
        </div>
      </SettingsSection>

      {/* Single-line text fields (label left, control right) */}
      <SettingsCard>
        <SettingsRow label={intl.formatMessage({ id: 'branding.productName' })} tier="text">
          <Input
            id="branding-name"
            value={productName}
            {...fieldProps('product_name')}
            maxLength={60}
            onChange={(e) => setProductName(e.target.value)}
            placeholder="DuDuClaw"
          />
          {managedHint('product_name')}
        </SettingsRow>

        <SettingsRow label={intl.formatMessage({ id: 'branding.subtitle' })} tier="text">
          <Input
            id="branding-subtitle"
            value={subtitle}
            {...fieldProps('subtitle')}
            maxLength={120}
            onChange={(e) => setSubtitle(e.target.value)}
          />
          {managedHint('subtitle')}
        </SettingsRow>

        <SettingsRow label={intl.formatMessage({ id: 'branding.company' })} tier="text">
          <Input
            id="branding-company"
            value={companyName}
            {...fieldProps('company_name')}
            maxLength={120}
            onChange={(e) => setCompanyName(e.target.value)}
          />
          {managedHint('company_name')}
        </SettingsRow>

        <SettingsRow label={intl.formatMessage({ id: 'branding.website' })} tier="text">
          <Input
            id="branding-website"
            type="url"
            value={website}
            {...fieldProps('website')}
            onChange={(e) => setWebsite(e.target.value)}
            placeholder="https://example.com"
          />
          {managedHint('website')}
        </SettingsRow>

        <SettingsRow label={intl.formatMessage({ id: 'branding.supportEmail' })} tier="text">
          <Input
            id="branding-email"
            type="email"
            value={supportEmail}
            {...fieldProps('support_email')}
            onChange={(e) => setSupportEmail(e.target.value)}
            placeholder="support@example.com"
          />
          {managedHint('support_email')}
        </SettingsRow>
      </SettingsCard>

      {/* Description (full-width block — multiline) */}
      <SettingsSection title={intl.formatMessage({ id: 'branding.description' })}>
        <Textarea
          id="branding-desc"
          value={description}
          {...fieldProps('description')}
          maxLength={500}
          rows={3}
          onChange={(e) => setDescription(e.target.value)}
        />
        {managedHint('description')}
      </SettingsSection>

      {/* Accent color (design §10.4) */}
      <SettingsSection
        title={intl.formatMessage({ id: 'branding.accent' })}
        description={intl.formatMessage({ id: 'branding.accent.hint' })}
      >
        <div className="flex items-center gap-3">
          <input
            id="branding-accent"
            type="color"
            value={accentColor || DEFAULT_ACCENT}
            {...fieldProps('accent_color')}
            onChange={(e) => setAccentColor(e.target.value)}
            className="h-9 w-14 shrink-0 cursor-pointer rounded-lg border border-surface-border bg-transparent p-1 disabled:cursor-not-allowed"
          />
          <span className="font-mono text-sm uppercase text-muted-foreground">
            {accentColor || DEFAULT_ACCENT}
          </span>
          {accentColor && (
            <Button
              variant="ghost"
              disabled={fieldDisabled('accent_color')}
              onClick={() => setAccentColor('')}
            >
              <RotateCcw />
              {intl.formatMessage({ id: 'branding.accent.reset' })}
            </Button>
          )}
        </div>
        {managedHint('accent_color')}
      </SettingsSection>

      {/* About-page HTML editor + sanitized preview (design §10.2) */}
      <SettingsSection
        title={intl.formatMessage({ id: 'branding.aboutHtml' })}
        description={intl.formatMessage({ id: 'branding.aboutHtml.hint' })}
      >
        <div className="grid gap-3 lg:grid-cols-2">
          <Textarea
            id="branding-about-html"
            value={aboutHtml}
            {...fieldProps('about_html')}
            rows={10}
            spellCheck={false}
            onChange={(e) => setAboutHtml(e.target.value)}
            className="font-mono text-xs leading-relaxed"
            placeholder={'<h2>關於我們</h2>\n<p>…</p>'}
          />
          <div className="min-h-[10rem] overflow-auto rounded-lg border border-surface-border bg-muted/40 p-4">
            <p className="mb-2 text-[11px] font-semibold tracking-wider text-muted-foreground uppercase">
              {intl.formatMessage({ id: 'branding.aboutHtml.preview' })}
              {previewing && (
                <span className="ml-2 font-normal normal-case text-muted-foreground">
                  {intl.formatMessage({ id: 'common.loading' })}
                </span>
              )}
            </p>
            {previewHtml ? (
              // Safe: `previewHtml` is the backend-sanitized string, never raw input.
              <div
                className="branding-about-preview text-sm text-foreground"
                dangerouslySetInnerHTML={{ __html: previewHtml }}
              />
            ) : (
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'branding.aboutHtml.previewEmpty' })}
              </p>
            )}
          </div>
        </div>
        {managedHint('about_html')}
      </SettingsSection>

      {/* Signed branding bundle (design §10.3) */}
      <div className="rounded-xl border border-surface-border bg-muted/40 p-4">
        <div className="flex items-center gap-2">
          <Package className="h-4 w-4 text-muted-foreground" />
          <h3 className="text-sm font-semibold">
            {intl.formatMessage({ id: 'branding.bundle.section' })}
          </h3>
        </div>
        <p className="mt-1.5 text-xs leading-relaxed text-muted-foreground">
          {intl.formatMessage({ id: 'branding.bundle.desc' })}
        </p>
        <div className="mt-3">
          <Button variant="outline" disabled={locked || bundling} onClick={handleGenerateBundle}>
            <Download />
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
        <Button variant="brand" onClick={handleSave} disabled={locked || saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </div>
  );
}
