import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { Store, Plus, KeyRound, Copy, Check, Trash2, Ban, Info, FileSignature, Download, Palette } from 'lucide-react';
import {
  Page,
  PageHeader,
  Card,
  Tabs,
  Button,
  EmptyState,
  Badge,
  Mono,
  type TabItem,
} from '@/components/ui';
import { Dialog, FormField, inputClass, buttonSecondary, buttonPrimary } from '@/components/shared/Dialog';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
import { BrandingTab } from '@/components/settings/sections/BrandingTab';
import { toast, formatError } from '@/lib/toast';
import {
  api,
  type DistributorProfile,
  type DistributorStats,
  type IssuedLicense,
  type BrandingConfig,
} from '@/lib/api';

/**
 * DistributorsPage (design-distributor-white-label §4.5) — the combined "白牌與經銷"
 * console (`/manage/distributors`, admin-gated). Two tabs on one page so the
 * brand-appearance settings and the distributor issuance ledger no longer live in
 * two unrelated corners (2026-07-12 walkthrough: "分開很難找"):
 *   - 品牌設定  → the reusable `BrandingTab` (product name / logo / accent / About
 *                HTML; keeps its WP8 field-level masking).
 *   - 經銷商簽發 → the OWNER console: sign OEM white-label licenses, keep the
 *                distributor ledger, and revoke locally.
 *
 * This is a /manage surface, so internal terms (license / OEM / fingerprint) are
 * fine in the distributor tab — the branding tab stays customer-facing.
 *
 * Fail-closed: when the gateway reports no issuer key configured, the distributor
 * tab shows an empty state explaining how to set `[distributor] issuer_key_path`
 * and never offers an issue action.
 */

type WhiteLabelTab = 'branding' | 'distributors';

const DEFAULT_EXPIRES_DAYS = 365;

/**
 * Branding fields a reseller can grant to a customer license (WP8). The `field`
 * values are the exact `branding.set` serde keys the gateway validates against;
 * `labelId` reuses the customer-facing branding labels so the wording matches
 * the "品牌設定" tab. Order mirrors the BrandingTab form.
 */
const BRANDING_FIELDS: { field: string; labelId: string }[] = [
  { field: 'product_name', labelId: 'branding.productName' },
  { field: 'subtitle', labelId: 'branding.subtitle' },
  { field: 'logo_data_uri', labelId: 'branding.logo' },
  { field: 'company_name', labelId: 'branding.company' },
  { field: 'website', labelId: 'branding.website' },
  { field: 'support_email', labelId: 'branding.supportEmail' },
  { field: 'description', labelId: 'branding.description' },
  { field: 'about_html', labelId: 'branding.aboutHtml' },
  { field: 'accent_color', labelId: 'branding.accent' },
];

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

export function DistributorsPage() {
  const intl = useIntl();
  const [searchParams] = useSearchParams();
  // Default to 品牌設定 — the more commonly-edited surface and the one users
  // struggled to find. `?tab=distributors` (or legacy `?tab=branding`) deep-links.
  const [tab, setTab] = useState<WhiteLabelTab>(
    searchParams.get('tab') === 'distributors' ? 'distributors' : 'branding',
  );
  const [issuerConfigured, setIssuerConfigured] = useState<boolean | null>(null);
  const [refreshEndpointActive, setRefreshEndpointActive] = useState(false);
  const [stats, setStats] = useState<DistributorStats | null>(null);
  const [distributors, setDistributors] = useState<DistributorProfile[]>([]);
  const [loading, setLoading] = useState(true);

  // Dialog state
  const [addOpen, setAddOpen] = useState(false);
  const [signBundleOpen, setSignBundleOpen] = useState(false);
  const [issueTarget, setIssueTarget] = useState<DistributorProfile | null>(null);
  const [removeTarget, setRemoveTarget] = useState<DistributorProfile | null>(null);
  const [revokeTarget, setRevokeTarget] = useState<IssuedLicense | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [st, ls] = await Promise.all([api.distributor.status(), api.distributor.list()]);
      setIssuerConfigured(st.issuer_configured);
      setRefreshEndpointActive(st.refresh_endpoint_active ?? st.issuer_configured);
      setStats(st.stats);
      setDistributors(ls.distributors);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setIssuerConfigured(false);
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleAdd = async (input: { name: string; contact: string; note: string }) => {
    setBusy(true);
    try {
      await api.distributor.add(input);
      setAddOpen(false);
      toast.success(intl.formatMessage({ id: 'distributor.add.done' }));
      await refresh();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleRemove = async () => {
    if (!removeTarget) return;
    setBusy(true);
    try {
      await api.distributor.remove(removeTarget.id);
      setRemoveTarget(null);
      toast.success(intl.formatMessage({ id: 'distributor.remove.done' }));
      await refresh();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleRevoke = async () => {
    if (!revokeTarget) return;
    setBusy(true);
    try {
      await api.distributor.revoke(revokeTarget.id);
      setRevokeTarget(null);
      toast.success(intl.formatMessage({ id: 'distributor.revoke.done' }));
      await refresh();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  const tabItems: TabItem[] = [
    { id: 'branding', label: intl.formatMessage({ id: 'settings.branding' }), icon: Palette },
    { id: 'distributors', label: intl.formatMessage({ id: 'distributor.title' }), icon: Store },
  ];

  return (
    <Page>
      <PageHeader
        icon={Store}
        title={intl.formatMessage({ id: 'manage.distributors' })}
        subtitle={intl.formatMessage({ id: 'whitelabel.subtitle' })}
        actions={
          tab === 'distributors' && issuerConfigured ? (
            <div className="flex items-center gap-2">
              <Button
                variant="secondary"
                icon={FileSignature}
                onClick={() => setSignBundleOpen(true)}
              >
                {intl.formatMessage({ id: 'distributor.bundle.sign' })}
              </Button>
              <Button variant="primary" icon={Plus} onClick={() => setAddOpen(true)}>
                {intl.formatMessage({ id: 'distributor.add' })}
              </Button>
            </div>
          ) : undefined
        }
      />

      <Tabs items={tabItems} value={tab} onChange={(id) => setTab(id as WhiteLabelTab)} />

      {tab === 'branding' && <BrandingTab />}

      {tab === 'distributors' && (loading ? (
        <Card>
          <p className="py-8 text-center text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.loading' })}
          </p>
        </Card>
      ) : !issuerConfigured ? (
        <Card>
          <EmptyState
            icon={KeyRound}
            title={intl.formatMessage({ id: 'distributor.noIssuer.title' })}
            hint={intl.formatMessage({ id: 'distributor.noIssuer.hint' })}
          />
          <pre className="mx-auto mt-2 max-w-md overflow-x-auto rounded-lg bg-stone-500/8 px-4 py-3 text-xs text-stone-600 dark:bg-white/5 dark:text-stone-300">
{`[distributor]
issuer_key_path = "~/.duduclaw/keys/issuer.key"
public_url = "https://your-gateway.example.com"`}
          </pre>
          <p className="mx-auto mt-2 max-w-md text-center text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'distributor.noIssuer.publicUrl' })}
          </p>
        </Card>
      ) : (
        <>
          {/* Control-plane (P2) status + distributor setup guidance */}
          <Card>
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div className="flex items-center gap-2">
                <h2 className="text-sm font-semibold tracking-tight text-stone-900 dark:text-stone-50">
                  {intl.formatMessage({ id: 'distributor.controlPlane.title' })}
                </h2>
                <Badge tone={refreshEndpointActive ? 'success' : 'neutral'}>
                  {intl.formatMessage({
                    id: refreshEndpointActive
                      ? 'distributor.controlPlane.active'
                      : 'distributor.controlPlane.inactive',
                  })}
                </Badge>
              </div>
            </div>
            {refreshEndpointActive && (
              <div className="mt-3 flex items-start gap-2 rounded-lg border border-sky-500/30 bg-sky-500/10 px-3 py-2 text-xs text-sky-800 dark:text-sky-200">
                <Info className="mt-0.5 h-4 w-4 shrink-0" />
                <div className="min-w-0 space-y-1">
                  <p>{intl.formatMessage({ id: 'distributor.controlPlane.guide' })}</p>
                  <pre className="overflow-x-auto rounded bg-stone-500/8 px-2 py-1 text-[11px] text-stone-600 dark:bg-white/5 dark:text-stone-300">
{`DUDUCLAW_CONTROL_URL=https://your-gateway.example.com`}
                  </pre>
                </div>
              </div>
            )}
          </Card>

          {/* Stats */}
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
            <StatTile
              label={intl.formatMessage({ id: 'distributor.stats.distributors' })}
              value={stats?.total_distributors ?? distributors.length}
            />
            <StatTile
              label={intl.formatMessage({ id: 'distributor.stats.active' })}
              value={stats?.active_licenses ?? 0}
            />
            <StatTile
              label={intl.formatMessage({ id: 'distributor.stats.revoked' })}
              value={stats?.revoked_licenses ?? 0}
            />
          </div>

          {distributors.length === 0 ? (
            <Card>
              <EmptyState
                icon={Store}
                title={intl.formatMessage({ id: 'distributor.empty.title' })}
                hint={intl.formatMessage({ id: 'distributor.empty.hint' })}
              />
            </Card>
          ) : (
            distributors.map((d) => (
              <Card key={d.id}>
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <h2 className="text-base font-semibold tracking-tight text-stone-900 dark:text-stone-50">
                        {d.name}
                      </h2>
                      <Badge tone={d.status === 'active' ? 'success' : 'neutral'}>
                        {d.status}
                      </Badge>
                    </div>
                    {d.contact && (
                      <p className="mt-0.5 text-sm text-stone-500 dark:text-stone-400">{d.contact}</p>
                    )}
                    {d.note && (
                      <p className="mt-1 text-xs text-stone-400 dark:text-stone-500">{d.note}</p>
                    )}
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Button variant="secondary" icon={KeyRound} onClick={() => setIssueTarget(d)}>
                      {intl.formatMessage({ id: 'distributor.issue' })}
                    </Button>
                    <Button variant="ghost" icon={Trash2} onClick={() => setRemoveTarget(d)}>
                      {intl.formatMessage({ id: 'distributor.remove' })}
                    </Button>
                  </div>
                </div>

                {/* Issued licenses */}
                {d.licenses && d.licenses.length > 0 && (
                  <div className="mt-4 space-y-2 border-t border-stone-200/70 pt-3 dark:border-white/8">
                    {d.licenses.map((lic) => (
                      <LicenseRow
                        key={lic.id}
                        lic={lic}
                        revokeLabel={intl.formatMessage({ id: 'distributor.revoke' })}
                        expiryLabel={intl.formatMessage({ id: 'distributor.license.expires' })}
                        fingerprintLabel={intl.formatMessage({ id: 'distributor.license.fingerprint' })}
                        lastRefreshLabel={intl.formatMessage({ id: 'distributor.license.lastRefresh' })}
                        onRevoke={() => setRevokeTarget(lic)}
                      />
                    ))}
                  </div>
                )}
              </Card>
            ))
          )}
        </>
      ))}

      {/* Add distributor */}
      <AddDistributorDialog
        open={addOpen}
        busy={busy}
        onClose={() => setAddOpen(false)}
        onSubmit={handleAdd}
      />

      {/* Counter-sign a branding bundle for a distributor */}
      <SignBundleDialog
        open={signBundleOpen}
        distributors={distributors}
        onClose={() => setSignBundleOpen(false)}
      />

      {/* Issue license */}
      {issueTarget && (
        <IssueLicenseDialog
          distributor={issueTarget}
          onClose={() => {
            setIssueTarget(null);
            void refresh();
          }}
        />
      )}

      {/* Remove distributor */}
      <ConfirmDialog
        open={!!removeTarget}
        busy={busy}
        onClose={() => setRemoveTarget(null)}
        onConfirm={handleRemove}
        title={intl.formatMessage({ id: 'distributor.remove.confirm.title' })}
        message={intl.formatMessage({ id: 'distributor.remove.confirm.body' })}
        confirmLabel={intl.formatMessage({ id: 'distributor.remove' })}
      />

      {/* Revoke license */}
      <ConfirmDialog
        open={!!revokeTarget}
        busy={busy}
        onClose={() => setRevokeTarget(null)}
        onConfirm={handleRevoke}
        title={intl.formatMessage({ id: 'distributor.revoke.confirm.title' })}
        message={intl.formatMessage({ id: 'distributor.revoke.confirm.body' })}
        confirmLabel={intl.formatMessage({ id: 'distributor.revoke' })}
      />
    </Page>
  );
}

function StatTile({ label, value }: { label: string; value: number }) {
  return (
    <div className="panel px-4 py-3">
      <p className="text-xs text-stone-500 dark:text-stone-400">{label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums text-stone-900 dark:text-stone-50">
        {value}
      </p>
    </div>
  );
}

function LicenseRow({
  lic,
  revokeLabel,
  expiryLabel,
  fingerprintLabel,
  lastRefreshLabel,
  onRevoke,
}: {
  lic: IssuedLicense;
  revokeLabel: string;
  expiryLabel: string;
  fingerprintLabel: string;
  lastRefreshLabel: string;
  onRevoke: () => void;
}) {
  const revoked = lic.status === 'revoked';
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-stone-500/5 px-3 py-2 dark:bg-white/[0.03]">
      <div className="min-w-0 space-y-0.5">
        <div className="flex items-center gap-2">
          <Mono className="truncate text-xs">{lic.id}</Mono>
          <Badge tone={revoked ? 'danger' : 'success'}>{lic.status}</Badge>
        </div>
        <p className="truncate text-xs text-stone-400 dark:text-stone-500">
          {fingerprintLabel}: <Mono>{lic.machine_fingerprint}</Mono>
        </p>
        <p className="text-xs text-stone-400 dark:text-stone-500">
          {expiryLabel}: <Mono>{lic.expires_at}</Mono>
        </p>
        <p className="text-xs text-stone-400 dark:text-stone-500">
          {lastRefreshLabel}: <Mono>{lic.last_refresh_at || '—'}</Mono>
        </p>
      </div>
      {!revoked && (
        <Button variant="ghost" size="sm" icon={Ban} onClick={onRevoke}>
          {revokeLabel}
        </Button>
      )}
    </div>
  );
}

function AddDistributorDialog({
  open,
  busy,
  onClose,
  onSubmit,
}: {
  open: boolean;
  busy: boolean;
  onClose: () => void;
  onSubmit: (input: { name: string; contact: string; note: string }) => void;
}) {
  const intl = useIntl();
  const [name, setName] = useState('');
  const [contact, setContact] = useState('');
  const [note, setNote] = useState('');

  useEffect(() => {
    if (open) {
      setName('');
      setContact('');
      setNote('');
    }
  }, [open]);

  return (
    <Dialog open={open} onClose={onClose} title={intl.formatMessage({ id: 'distributor.add' })}>
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'distributor.field.name' })} htmlFor="dist-name">
          <input
            id="dist-name"
            className={inputClass}
            value={name}
            onChange={(e) => setName(e.target.value)}
            autoFocus
          />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'distributor.field.contact' })} htmlFor="dist-contact">
          <input
            id="dist-contact"
            className={inputClass}
            value={contact}
            onChange={(e) => setContact(e.target.value)}
          />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'distributor.field.note' })} htmlFor="dist-note">
          <input
            id="dist-note"
            className={inputClass}
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
        </FormField>
        <div className="flex justify-end gap-2 pt-1">
          <button type="button" className={buttonSecondary} onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            type="button"
            className={buttonPrimary}
            disabled={busy || name.trim().length === 0}
            onClick={() => onSubmit({ name: name.trim(), contact: contact.trim(), note: note.trim() })}
          >
            {busy ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function SignBundleDialog({
  open,
  distributors,
  onClose,
}: {
  open: boolean;
  distributors: DistributorProfile[];
  onClose: () => void;
}) {
  const intl = useIntl();
  const [distributorId, setDistributorId] = useState('');
  const [json, setJson] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (open) {
      setDistributorId(distributors[0]?.id ?? '');
      setJson('');
    }
  }, [open, distributors]);

  const handleSign = async () => {
    let branding: BrandingConfig;
    try {
      branding = JSON.parse(json) as BrandingConfig;
      if (typeof branding !== 'object' || branding === null || Array.isArray(branding)) {
        throw new Error('not an object');
      }
    } catch {
      toast.error(intl.formatMessage({ id: 'distributor.bundle.invalidJson' }));
      return;
    }
    setBusy(true);
    try {
      const res = await api.distributor.bundle.sign({ distributor_id: distributorId, branding });
      downloadJson('branding.bundle.json', res.bundle);
      toast.success(intl.formatMessage({ id: 'distributor.bundle.done' }));
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onClose={onClose} title={intl.formatMessage({ id: 'distributor.bundle.sign' })}>
      <div className="space-y-4">
        <p className="text-sm text-stone-600 dark:text-stone-300">
          {intl.formatMessage({ id: 'distributor.bundle.sign.desc' })}
        </p>
        <FormField
          label={intl.formatMessage({ id: 'distributor.bundle.selectDistributor' })}
          htmlFor="sign-distributor"
        >
          <select
            id="sign-distributor"
            className={inputClass}
            value={distributorId}
            onChange={(e) => setDistributorId(e.target.value)}
          >
            {distributors.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name}
              </option>
            ))}
          </select>
        </FormField>
        <FormField
          label={intl.formatMessage({ id: 'distributor.bundle.brandingJson' })}
          htmlFor="sign-json"
          hint={intl.formatMessage({ id: 'distributor.bundle.brandingJson.hint' })}
        >
          <textarea
            id="sign-json"
            className={`${inputClass} font-mono text-xs`}
            rows={8}
            spellCheck={false}
            value={json}
            onChange={(e) => setJson(e.target.value)}
            placeholder={'{\n  "product_name": "Acme",\n  "accent_color": "#3b82f6"\n}'}
          />
        </FormField>
        <div className="flex justify-end gap-2 pt-1">
          <button type="button" className={buttonSecondary} onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            type="button"
            className={buttonPrimary}
            disabled={busy || distributorId.length === 0 || json.trim().length === 0}
            onClick={handleSign}
          >
            <Download className="mr-1.5 inline h-4 w-4" />
            {busy
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'distributor.bundle.submit' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function IssueLicenseDialog({
  distributor,
  onClose,
}: {
  distributor: DistributorProfile;
  onClose: () => void;
}) {
  const intl = useIntl();
  const [fingerprint, setFingerprint] = useState('');
  const [expiresDays, setExpiresDays] = useState(DEFAULT_EXPIRES_DAYS);
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [blob, setBlob] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  // WP8: default = grant the full reseller branding range (omit the param).
  // Enabling `restrictBranding` narrows the customer to the checked fields.
  const [restrictBranding, setRestrictBranding] = useState(false);
  const [brandingFields, setBrandingFields] = useState<string[]>([]);

  const toggleField = (field: string) =>
    setBrandingFields((prev) =>
      prev.includes(field) ? prev.filter((f) => f !== field) : [...prev, field]
    );

  const handleIssue = async () => {
    setBusy(true);
    try {
      const res = await api.distributor.issue({
        distributor_id: distributor.id,
        machine_fingerprint: fingerprint.trim(),
        expires_days: expiresDays,
        note: note.trim() || undefined,
        // Omit for the full reseller range; send the (possibly empty) subset when
        // the reseller chose to restrict the customer's editable branding.
        branding_editable: restrictBranding ? brandingFields : undefined,
      });
      setBlob(res.license_blob);
      toast.success(intl.formatMessage({ id: 'distributor.issue.done' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleCopy = async () => {
    if (!blob) return;
    try {
      await navigator.clipboard.writeText(blob);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      /* clipboard blocked — the blob is still visible for manual copy */
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title={intl.formatMessage({ id: 'distributor.issue.title' }, { name: distributor.name })}
    >
      {blob ? (
        <div className="space-y-4">
          <p className="text-sm text-stone-600 dark:text-stone-300">
            {intl.formatMessage({ id: 'distributor.issue.result.intro' })}
          </p>
          <textarea
            readOnly
            value={blob}
            rows={5}
            className={`${inputClass} font-mono text-xs`}
            onFocus={(e) => e.currentTarget.select()}
          />
          <button type="button" className={buttonSecondary} onClick={handleCopy}>
            {copied ? (
              <>
                <Check className="mr-1.5 inline h-4 w-4 text-emerald-500" />
                {intl.formatMessage({ id: 'distributor.issue.copied' })}
              </>
            ) : (
              <>
                <Copy className="mr-1.5 inline h-4 w-4" />
                {intl.formatMessage({ id: 'distributor.issue.copy' })}
              </>
            )}
          </button>
          <div className="flex items-start gap-2 rounded-lg border border-sky-500/30 bg-sky-500/10 px-3 py-2 text-xs text-sky-800 dark:text-sky-200">
            <Info className="mt-0.5 h-4 w-4 shrink-0" />
            <p>{intl.formatMessage({ id: 'distributor.issue.result.guide' })}</p>
          </div>
          <div className="flex justify-end pt-1">
            <button type="button" className={buttonPrimary} onClick={onClose}>
              {intl.formatMessage({ id: 'common.done' })}
            </button>
          </div>
        </div>
      ) : (
        <div className="space-y-4">
          <FormField
            label={intl.formatMessage({ id: 'distributor.field.fingerprint' })}
            htmlFor="issue-fp"
            hint={intl.formatMessage({ id: 'distributor.field.fingerprint.hint' })}
          >
            <input
              id="issue-fp"
              className={inputClass}
              value={fingerprint}
              onChange={(e) => setFingerprint(e.target.value)}
              placeholder="a1b2c3…"
              autoFocus
            />
          </FormField>
          <FormField
            label={intl.formatMessage({ id: 'distributor.field.expiresDays' })}
            htmlFor="issue-days"
          >
            <input
              id="issue-days"
              type="number"
              min={1}
              className={inputClass}
              value={expiresDays}
              onChange={(e) => setExpiresDays(Math.max(1, Number(e.target.value) || DEFAULT_EXPIRES_DAYS))}
            />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'distributor.field.note' })} htmlFor="issue-note">
            <input
              id="issue-note"
              className={inputClass}
              value={note}
              onChange={(e) => setNote(e.target.value)}
            />
          </FormField>

          {/* WP8: branding edit-scope for the issued customer license. */}
          <div className="space-y-2 rounded-lg border border-stone-300/60 bg-stone-500/[0.03] p-3 dark:border-white/10 dark:bg-white/[0.02]">
            <label className="flex items-start gap-2 text-sm text-stone-700 dark:text-stone-300">
              <input
                type="checkbox"
                className="mt-0.5 h-4 w-4 shrink-0 accent-amber-500"
                checked={restrictBranding}
                onChange={(e) => setRestrictBranding(e.target.checked)}
              />
              <span>
                <span className="font-medium">
                  {intl.formatMessage({ id: 'distributor.issue.branding.restrict' })}
                </span>
                <span className="mt-0.5 block text-xs font-normal text-stone-400 dark:text-stone-500">
                  {intl.formatMessage({ id: 'distributor.issue.branding.hint' })}
                </span>
              </span>
            </label>
            {restrictBranding && (
              <div className="grid grid-cols-1 gap-1.5 border-t border-stone-200/70 pt-2 sm:grid-cols-2 dark:border-white/8">
                {BRANDING_FIELDS.map(({ field, labelId }) => (
                  <label
                    key={field}
                    className="flex items-center gap-2 text-sm text-stone-600 dark:text-stone-300"
                  >
                    <input
                      type="checkbox"
                      className="h-4 w-4 shrink-0 accent-amber-500"
                      checked={brandingFields.includes(field)}
                      onChange={() => toggleField(field)}
                    />
                    {intl.formatMessage({ id: labelId })}
                  </label>
                ))}
              </div>
            )}
          </div>

          <div className="flex justify-end gap-2 pt-1">
            <button type="button" className={buttonSecondary} onClick={onClose}>
              {intl.formatMessage({ id: 'common.cancel' })}
            </button>
            <button
              type="button"
              className={buttonPrimary}
              disabled={busy || fingerprint.trim().length === 0}
              onClick={handleIssue}
            >
              {busy
                ? intl.formatMessage({ id: 'common.saving' })
                : intl.formatMessage({ id: 'distributor.issue.submit' })}
            </button>
          </div>
        </div>
      )}
    </Dialog>
  );
}
