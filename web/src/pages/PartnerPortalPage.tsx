import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Handshake,
  Users,
  DollarSign,
  TrendingUp,
  Award,
  Check,
  Download,
  FileText,
  Presentation,
  BookOpen,
  Plus,
  Loader2,
  Pencil,
  Trash2,
  Terminal,
  X,
} from 'lucide-react';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  Page,
  PageHeader,
  Card,
  StatCard,
  Badge,
  Button,
  EmptyState,
  Field,
  controlClass,
} from '@/components/ui';
import {
  api,
  type PartnerProfile,
  type PartnerStats,
  type PartnerCustomer,
} from '@/lib/api';

// ── Styling helpers (not mock data) ──────────────────────

type BadgeTone = 'neutral' | 'success' | 'warning' | 'danger' | 'info' | 'accent';

const TIER_TONES: Record<string, BadgeTone> = {
  standard: 'neutral',
  silver: 'neutral',
  gold: 'accent',
  platinum: 'info',
};

const STATUS_TONES: Record<string, BadgeTone> = {
  active: 'success',
  trial: 'info',
  expiring: 'warning',
  expired: 'danger',
  cancelled: 'neutral',
};

const CUSTOMER_TIER_TONES: Record<string, BadgeTone> = {
  standard: 'neutral',
  pro: 'accent',
  enterprise: 'info',
};

const PROFILE_TIERS = ['standard', 'silver', 'gold', 'platinum'] as const;
const CUSTOMER_TIERS = ['standard', 'pro', 'enterprise'] as const;
const CUSTOMER_STATUSES = ['active', 'trial', 'expired', 'cancelled'] as const;

// Format integer cents → "$1,234.56". Stays locale-agnostic so it matches
// how the dashboard already renders monetary values.
function formatDollars(cents: number): string {
  const dollars = cents / 100;
  return `$${dollars.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

// ── Component ────────────────────────────────────────────

export function PartnerPortalPage() {
  const intl = useIntl();

  // Partner data state
  const [profile, setProfile] = useState<PartnerProfile | null>(null);
  const [stats, setStats] = useState<PartnerStats | null>(null);
  const [customers, setCustomers] = useState<PartnerCustomer[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Add / edit / delete customer modals
  const [showAddCustomer, setShowAddCustomer] = useState(false);
  const [editCustomer, setEditCustomer] = useState<PartnerCustomer | null>(null);
  const [deleteCustomer, setDeleteCustomer] = useState<PartnerCustomer | null>(null);

  const refresh = async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const [profileData, statsData, customersData] = await Promise.all([
        api.partner.profile(),
        api.partner.stats(),
        api.partner.customers(),
      ]);
      setProfile(profileData);
      setStats(statsData);
      setCustomers(customersData.customers ?? []);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setLoadError(message);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const handleDeleteCustomer = async () => {
    if (!deleteCustomer) return;
    try {
      await api.partner.deleteCustomer(deleteCustomer.id);
      toast.success(intl.formatMessage({ id: 'partner.customer.deleted' }));
      setDeleteCustomer(null);
      void refresh();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  const isProfileEmpty = !profile || !profile.company || profile.company.trim() === '';

  return (
    <Page>
      <PageHeader
        icon={Handshake}
        title={intl.formatMessage({ id: 'nav.partner' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
      />

      {/* Top-of-page load error (R1 pattern: rose alert, dismissible). */}
      {loadError && (
        <div
          role="alert"
          className="flex items-start justify-between gap-3 rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300"
        >
          <span className="flex-1">
            {intl.formatMessage(
              { id: 'partner.loadError' },
              { message: loadError },
            )}
          </span>
          <button
            type="button"
            onClick={() => setLoadError(null)}
            className="shrink-0 text-rose-500 hover:text-rose-700 dark:text-rose-400 dark:hover:text-rose-200"
            aria-label="Dismiss"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      {loading && !profile && (
        <Card>
          <div className="flex items-center gap-2 text-sm text-stone-600 dark:text-stone-400">
            <Loader2 className="h-4 w-4 animate-spin" />
            <span>{intl.formatMessage({ id: 'common.loading' })}</span>
          </div>
        </Card>
      )}

      {!loading && isProfileEmpty && (
        <PartnerOnboardingCard onSaved={refresh} />
      )}

      {!isProfileEmpty && profile && (
        <>
          {/* Partner Status Card */}
          <Card
            title={
              <span className="flex items-center gap-2">
                <Handshake className="h-4 w-4 text-amber-500" />
                {intl.formatMessage({ id: 'partner.status' })}
              </span>
            }
          >
            <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-4">
              <div className="space-y-1.5">
                <span className="text-sm text-stone-500 dark:text-stone-400">
                  {profile.company}
                </span>
                <div className="flex items-center gap-2">
                  <Award className="h-4 w-4 text-amber-500" />
                  <Badge tone={TIER_TONES[profile.tier] ?? 'neutral'}>
                    {intl.formatMessage({ id: `partner.tier.${profile.tier}` })}
                  </Badge>
                </div>
              </div>

              <div className="space-y-1.5">
                <span className="text-sm text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'partner.partnerId' })}
                </span>
                <code className="block rounded bg-stone-500/10 px-2 py-0.5 font-mono text-xs text-stone-600 dark:text-stone-400">
                  {profile.partner_id ?? '—'}
                </code>
              </div>

              <div className="space-y-1.5">
                <span className="text-sm text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'partner.certification' })}
                </span>
                <div className="flex items-center gap-2">
                  {profile.certified_at ? (
                    <>
                      <Check className="h-4 w-4 text-emerald-500" />
                      <span className="text-sm font-medium text-emerald-600 dark:text-emerald-400">
                        {intl.formatMessage({ id: 'partner.certified' })}
                      </span>
                    </>
                  ) : (
                    <span className="text-sm text-stone-500">
                      {intl.formatMessage({ id: 'partner.pending' })}
                    </span>
                  )}
                </div>
              </div>

              <div className="space-y-1.5">
                <span className="text-sm text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'partner.since' })}
                </span>
                <span className="text-sm font-medium text-stone-900 dark:text-stone-50">
                  {profile.certified_at
                    ? new Date(profile.certified_at).toLocaleDateString()
                    : '—'}
                </span>
              </div>
            </div>
          </Card>

          {/* Sales Dashboard - 4 StatCards */}
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
            <StatCard
              icon={FileText}
              tone="accent"
              label={intl.formatMessage({ id: 'partner.totalSold' })}
              value={(stats?.total_sold ?? 0).toLocaleString()}
            />
            <StatCard
              icon={Users}
              tone="success"
              label={intl.formatMessage({ id: 'partner.activeCustomers' })}
              value={(stats?.active_customers ?? 0).toLocaleString()}
            />
            <StatCard
              icon={DollarSign}
              tone="warning"
              label={intl.formatMessage({ id: 'partner.monthlyRevenue' })}
              value={formatDollars(stats?.this_month_commission_cents ?? 0)}
            />
            <StatCard
              icon={TrendingUp}
              tone="neutral"
              label={intl.formatMessage({ id: 'partner.commission' })}
              value={formatDollars(stats?.lifetime_commission_cents ?? 0)}
            />
          </div>

          {/* Customer Management Table */}
          <Card
            padded={false}
            title={intl.formatMessage({ id: 'partner.customers' })}
            actions={
              <Button
                variant="primary"
                size="sm"
                icon={Plus}
                onClick={() => setShowAddCustomer(true)}
              >
                {intl.formatMessage({ id: 'partner.addCustomer' })}
              </Button>
            }
          >
            {customers.length === 0 ? (
              <EmptyState
                icon={Users}
                title={intl.formatMessage({ id: 'partner.empty' })}
              />
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-[var(--panel-border)]">
                      <th className="px-5 py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.customerName' })}
                      </th>
                      <th className="px-5 py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.licenseTier' })}
                      </th>
                      <th className="px-5 py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.activated' })}
                      </th>
                      <th className="px-5 py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'billing.status' })}
                      </th>
                      <th className="px-5 py-3 text-right font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.actions' })}
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {customers.map((customer) => (
                      <tr
                        key={customer.id}
                        className="border-b border-[var(--panel-border)] last:border-0"
                      >
                        <td className="px-5 py-3 text-sm font-medium text-stone-900 dark:text-stone-100">
                          {customer.name}
                        </td>
                        <td className="px-5 py-3">
                          <Badge tone={CUSTOMER_TIER_TONES[customer.tier] ?? 'neutral'}>
                            {intl.formatMessage({ id: `license.${customer.tier}` })}
                          </Badge>
                        </td>
                        <td className="px-5 py-3 text-sm text-stone-600 dark:text-stone-400">
                          {new Date(customer.activated_at).toLocaleDateString()}
                        </td>
                        <td className="px-5 py-3">
                          <Badge tone={STATUS_TONES[customer.status] ?? 'success'}>
                            {intl.formatMessage({
                              id: `partner.status.${customer.status}`,
                            })}
                          </Badge>
                        </td>
                        <td className="px-5 py-3 text-right">
                          <div className="flex items-center justify-end gap-2">
                            <Button
                              variant="secondary"
                              size="sm"
                              icon={Pencil}
                              onClick={() => setEditCustomer(customer)}
                              title={intl.formatMessage({ id: 'common.edit' })}
                              aria-label={intl.formatMessage({ id: 'common.edit' })}
                            />
                            <Button
                              variant="secondary"
                              size="sm"
                              icon={Trash2}
                              onClick={() => setDeleteCustomer(customer)}
                              title={intl.formatMessage({ id: 'common.delete' })}
                              aria-label={intl.formatMessage({ id: 'common.delete' })}
                              className="text-rose-500 hover:text-rose-600 dark:text-rose-400"
                            />
                          </div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Card>
        </>
      )}

      {/* License Generation — CLI-only (UI.4). License activation is not exposed
          over the dashboard RPC; surface a clear pointer to the CLI instead of a
          non-functional client-side stub. */}
      <Card title={intl.formatMessage({ id: 'partner.generateLicense' })}>
        <div className="flex items-start gap-3 rounded-lg border border-[var(--panel-border)] bg-stone-500/5 p-4 dark:bg-white/5">
          <Terminal className="mt-0.5 h-5 w-5 shrink-0 text-stone-400" />
          <div className="space-y-2">
            <p className="text-sm text-stone-600 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.license.cliOnly' })}
            </p>
            <code className="block rounded bg-stone-900 px-3 py-2 font-mono text-xs text-emerald-400">
              duduclaw license generate --tier &lt;pro|enterprise&gt; --customer &lt;name&gt; --months &lt;n&gt;
            </code>
          </div>
        </div>
      </Card>

      {/* Marketing Materials */}
      <Card title={intl.formatMessage({ id: 'partner.materials' })}>
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <MaterialCard
            icon={<Presentation className="h-5 w-5 text-amber-600 dark:text-amber-400" />}
            title={intl.formatMessage({ id: 'partner.downloadSlides' })}
            description={intl.formatMessage({ id: 'partner.slideDecks' }) + ' (PDF, 4.2 MB)'}
            href="#"
          />
          <MaterialCard
            icon={<FileText className="h-5 w-5 text-amber-600 dark:text-amber-400" />}
            title={intl.formatMessage({ id: 'partner.dmTemplate' })}
            description={intl.formatMessage({ id: 'partner.dmTemplate' }) + ' (DOCX, 1.8 MB)'}
            href="#"
          />
          <MaterialCard
            icon={<BookOpen className="h-5 w-5 text-amber-600 dark:text-amber-400" />}
            title={intl.formatMessage({ id: 'partner.downloadCaseStudy' })}
            description={intl.formatMessage({ id: 'partner.caseStudies' }) + ' (PDF, 6.1 MB)'}
            href="#"
          />
        </div>
      </Card>

      {showAddCustomer && (
        <AddCustomerModal
          onClose={() => setShowAddCustomer(false)}
          onSaved={() => {
            setShowAddCustomer(false);
            void refresh();
          }}
        />
      )}

      {editCustomer && (
        <EditCustomerModal
          customer={editCustomer}
          onClose={() => setEditCustomer(null)}
          onSaved={() => {
            setEditCustomer(null);
            void refresh();
          }}
        />
      )}

      {deleteCustomer && (
        <Dialog open onClose={() => setDeleteCustomer(null)} title={intl.formatMessage({ id: 'partner.customer.delete' })}>
          <div className="space-y-4">
            <p className="text-sm text-stone-600 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.customer.delete.confirm' }, { name: deleteCustomer.name })}
            </p>
            <div className="flex justify-end gap-3 pt-2">
              <button onClick={() => setDeleteCustomer(null)} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
              <button
                onClick={handleDeleteCustomer}
                className="inline-flex items-center justify-center gap-2 rounded-lg bg-rose-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-rose-600"
              >
                {intl.formatMessage({ id: 'common.delete' })}
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </Page>
  );
}

// ── UI.2 — edit customer modal ──

function EditCustomerModal({
  customer,
  onClose,
  onSaved,
}: {
  customer: PartnerCustomer;
  onClose: () => void;
  onSaved: () => void;
}) {
  const intl = useIntl();
  const [name, setName] = useState(customer.name);
  const [tier, setTier] = useState(customer.tier);
  const [status, setStatus] = useState(customer.status);
  const [notes, setNotes] = useState(customer.notes ?? '');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async () => {
    setSubmitting(true);
    setError(null);
    try {
      await api.partner.updateCustomer(customer.id, {
        name,
        tier,
        status,
        notes,
      });
      toast.success(intl.formatMessage({ id: 'partner.customer.updated' }));
      onSaved();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'partner.customer.edit' })}>
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'partner.customerName' })}>
          <input type="text" value={name} onChange={(e) => setName(e.target.value)} className={inputClass} />
        </FormField>
        <div className="grid grid-cols-2 gap-3">
          <FormField label={intl.formatMessage({ id: 'partner.licenseTier' })}>
            <select value={tier} onChange={(e) => setTier(e.target.value)} className={selectClass}>
              {CUSTOMER_TIERS.map((t) => (
                <option key={t} value={t}>{intl.formatMessage({ id: `license.${t}` })}</option>
              ))}
            </select>
          </FormField>
          <FormField label={intl.formatMessage({ id: 'billing.status' })}>
            <select value={status} onChange={(e) => setStatus(e.target.value)} className={selectClass}>
              {CUSTOMER_STATUSES.map((s) => (
                <option key={s} value={s}>{intl.formatMessage({ id: `partner.status.${s}` })}</option>
              ))}
            </select>
          </FormField>
        </div>
        <FormField label={intl.formatMessage({ id: 'partner.customer.notes' })}>
          <textarea value={notes} onChange={(e) => setNotes(e.target.value)} rows={3} className={cn(inputClass, 'resize-none')} />
        </FormField>
        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button onClick={handleSubmit} disabled={submitting} className={buttonPrimary}>
            {submitting ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

// ── Onboarding card (shown when profile.company is empty) ──

function PartnerOnboardingCard({ onSaved }: { readonly onSaved: () => void }) {
  const intl = useIntl();
  const [company, setCompany] = useState('');
  const [tier, setTier] = useState<string>('standard');
  const [partnerId, setPartnerId] = useState('');
  const [certifiedAt, setCertifiedAt] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!company.trim()) return;
    setSaving(true);
    setError(null);
    try {
      // ISO-8601 date input → UTC midnight RFC-3339 for backend compatibility.
      const certifiedAtIso = certifiedAt
        ? `${certifiedAt}T00:00:00+00:00`
        : null;
      await api.partner.updateProfile({
        company: company.trim(),
        tier,
        partner_id: partnerId.trim() || null,
        certified_at: certifiedAtIso,
      });
      onSaved();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card>
      <form onSubmit={handleSubmit}>
        <div className="mb-2 flex items-center gap-3">
          <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-amber-500/12 text-amber-600 ring-1 ring-inset ring-amber-500/20 dark:bg-amber-400/10 dark:text-amber-400">
            <Handshake className="h-5 w-5" />
          </span>
          <h3 className="text-sm font-semibold text-stone-800 dark:text-stone-100">
            {intl.formatMessage({ id: 'partner.setup.title' })}
          </h3>
        </div>
        <p className="mb-5 text-sm text-stone-600 dark:text-stone-400">
          {intl.formatMessage({ id: 'partner.setup.description' })}
        </p>

        <div className="grid gap-4 sm:grid-cols-2">
          <Field label={intl.formatMessage({ id: 'partner.setup.company' })} required>
            <input
              type="text"
              value={company}
              onChange={(e) => setCompany(e.target.value)}
              required
              className={controlClass}
            />
          </Field>
          <Field label={intl.formatMessage({ id: 'partner.setup.tier' })}>
            <select
              value={tier}
              onChange={(e) => setTier(e.target.value)}
              className={controlClass}
            >
              {PROFILE_TIERS.map((t) => (
                <option key={t} value={t}>
                  {intl.formatMessage({ id: `partner.tier.${t}` })}
                </option>
              ))}
            </select>
          </Field>
          <Field label={intl.formatMessage({ id: 'partner.setup.partnerId' })}>
            <input
              type="text"
              value={partnerId}
              onChange={(e) => setPartnerId(e.target.value)}
              placeholder="PTR-2025-0042"
              className={controlClass}
            />
          </Field>
          <Field label={intl.formatMessage({ id: 'partner.setup.certifiedAt' })}>
            <input
              type="date"
              value={certifiedAt}
              onChange={(e) => setCertifiedAt(e.target.value)}
              className={controlClass}
            />
          </Field>
        </div>

        {error && (
          <div
            role="alert"
            className="mt-4 rounded-lg border border-rose-200 bg-rose-50 px-4 py-2 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300"
          >
            {error}
          </div>
        )}

        <div className="mt-5 flex justify-end">
          <Button
            type="submit"
            variant="primary"
            disabled={saving || !company.trim()}
          >
            {saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'partner.setup.save' })}
          </Button>
        </div>
      </form>
    </Card>
  );
}

// ── Add Customer modal ───────────────────────────────────

function AddCustomerModal({
  onClose,
  onSaved,
}: {
  readonly onClose: () => void;
  readonly onSaved: () => void;
}) {
  const intl = useIntl();
  const [name, setName] = useState('');
  const [tier, setTier] = useState<string>('pro');
  const [status, setStatus] = useState<string>('active');
  const [activatedAt, setActivatedAt] = useState(
    new Date().toISOString().slice(0, 10),
  );
  const [commissionDollars, setCommissionDollars] = useState('0');
  const [notes, setNotes] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    setSaving(true);
    setError(null);
    try {
      const cents = Math.round(Number(commissionDollars || '0') * 100);
      if (!Number.isFinite(cents) || cents < 0) {
        throw new Error('Invalid commission amount');
      }
      await api.partner.addCustomer({
        name: name.trim(),
        tier,
        status,
        activated_at: `${activatedAt}T00:00:00+00:00`,
        commission_cents: cents,
        notes: notes.trim() || null,
      });
      onSaved();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-stone-900/40 p-4 backdrop-blur-sm"
      onClick={onClose}
    >
      <form
        onSubmit={handleSubmit}
        onClick={(e) => e.stopPropagation()}
        className="w-full max-w-lg glass-overlay rounded-2xl p-6"
      >
        <h3 className="mb-4 text-lg font-semibold tracking-tight text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'partner.addCustomer' })}
        </h3>

        <div className="space-y-4">
          <Field label={intl.formatMessage({ id: 'partner.customerName' })} required>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
              className={controlClass}
            />
          </Field>

          <div className="grid gap-4 sm:grid-cols-2">
            <Field label={intl.formatMessage({ id: 'partner.licenseTier' })}>
              <select
                value={tier}
                onChange={(e) => setTier(e.target.value)}
                className={controlClass}
              >
                {CUSTOMER_TIERS.map((t) => (
                  <option key={t} value={t}>
                    {intl.formatMessage({ id: `license.${t}` })}
                  </option>
                ))}
              </select>
            </Field>
            <Field label={intl.formatMessage({ id: 'billing.status' })}>
              <select
                value={status}
                onChange={(e) => setStatus(e.target.value)}
                className={controlClass}
              >
                {CUSTOMER_STATUSES.map((s) => (
                  <option key={s} value={s}>
                    {intl.formatMessage({ id: `partner.status.${s}` })}
                  </option>
                ))}
              </select>
            </Field>
            <Field label={intl.formatMessage({ id: 'partner.activated' })}>
              <input
                type="date"
                value={activatedAt}
                onChange={(e) => setActivatedAt(e.target.value)}
                required
                className={controlClass}
              />
            </Field>
            <Field label={intl.formatMessage({ id: 'partner.commissionDollars' })}>
              <input
                type="number"
                min="0"
                step="0.01"
                value={commissionDollars}
                onChange={(e) => setCommissionDollars(e.target.value)}
                className={controlClass}
              />
            </Field>
          </div>

          <Field label={intl.formatMessage({ id: 'partner.notes' })}>
            <textarea
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              rows={2}
              className={cn(controlClass, 'h-auto py-2 resize-none')}
            />
          </Field>
        </div>

        {error && (
          <div
            role="alert"
            className="mt-4 rounded-lg border border-rose-200 bg-rose-50 px-4 py-2 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300"
          >
            {error}
          </div>
        )}

        <div className="mt-5 flex items-center justify-end gap-2">
          <Button type="button" variant="secondary" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button type="submit" variant="primary" disabled={saving || !name.trim()}>
            {saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </form>
    </div>
  );
}

// ── Sub-components ───────────────────────────────────────

function MaterialCard({
  icon,
  title,
  description,
  href,
}: {
  readonly icon: React.ReactNode;
  readonly title: string;
  readonly description: string;
  readonly href: string;
}) {
  return (
    <a
      href={href}
      className="panel panel-hover flex items-start gap-3 p-4"
    >
      <div className="mt-0.5">{icon}</div>
      <div className="flex-1">
        <p className="text-sm font-medium text-stone-900 dark:text-stone-100">
          {title}
        </p>
        <p className="mt-1 text-xs text-stone-500 dark:text-stone-400">
          {description}
        </p>
      </div>
      <Download className="mt-0.5 h-4 w-4 shrink-0 text-stone-400" />
    </a>
  );
}
