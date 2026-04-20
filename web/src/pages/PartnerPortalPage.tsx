import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Handshake,
  Users,
  DollarSign,
  TrendingUp,
  Award,
  Copy,
  Check,
  Download,
  FileText,
  Presentation,
  BookOpen,
  RefreshCw,
  Eye,
  Plus,
  Loader2,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  api,
  type PartnerProfile,
  type PartnerStats,
  type PartnerCustomer,
} from '@/lib/api';

// ── Styling helpers (not mock data) ──────────────────────

const TIER_COLORS: Record<string, string> = {
  standard: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
  silver: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
  gold: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  platinum: 'bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400',
};

const STATUS_COLORS: Record<string, string> = {
  active: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
  trial: 'bg-sky-100 text-sky-700 dark:bg-sky-900/30 dark:text-sky-400',
  expiring: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  expired: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
  cancelled: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-400',
};

const CUSTOMER_TIER_COLORS: Record<string, string> = {
  standard: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
  pro: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  enterprise: 'bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400',
};

const DURATION_OPTIONS = [
  { months: 12, label: '1yr' },
  { months: 24, label: '2yr' },
  { months: 36, label: '3yr' },
];

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

  // License generation (existing)
  const [licenseTier, setLicenseTier] = useState('pro');
  const [customerName, setCustomerName] = useState('');
  const [duration, setDuration] = useState(12);
  const [generatedKey, setGeneratedKey] = useState('');
  const [generating, setGenerating] = useState(false);
  const [copied, setCopied] = useState(false);
  const [licenseError, setLicenseError] = useState<string | null>(null);

  // Add customer modal
  const [showAddCustomer, setShowAddCustomer] = useState(false);

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

  const handleGenerate = async () => {
    if (!customerName.trim()) return;
    setGenerating(true);
    setGeneratedKey('');
    setLicenseError(null);
    try {
      const result = await api.partner.generateLicense({
        tier: licenseTier,
        customer: customerName,
        months: duration,
      });
      setGeneratedKey(result?.key ?? '');
    } catch (err) {
      setGeneratedKey('');
      const message = err instanceof Error ? err.message : String(err);
      setLicenseError(
        intl.formatMessage({ id: 'partner.licenseError' }, { message }),
      );
    } finally {
      setGenerating(false);
    }
  };

  const handleCopy = async () => {
    if (!generatedKey) return;
    try {
      await navigator.clipboard.writeText(generatedKey);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // clipboard not available
    }
  };

  const isProfileEmpty = !profile || !profile.company || profile.company.trim() === '';

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'partner.title' })}
      </h2>

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
            <svg
              xmlns="http://www.w3.org/2000/svg"
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <line x1="18" y1="6" x2="6" y2="18"></line>
              <line x1="6" y1="6" x2="18" y2="18"></line>
            </svg>
          </button>
        </div>
      )}

      {loading && !profile && (
        <div className="flex items-center gap-2 rounded-xl border border-stone-200 bg-white p-6 text-sm text-stone-600 dark:border-stone-800 dark:bg-stone-900 dark:text-stone-400">
          <Loader2 className="h-4 w-4 animate-spin" />
          <span>{intl.formatMessage({ id: 'common.loading' })}</span>
        </div>
      )}

      {!loading && isProfileEmpty && (
        <PartnerOnboardingCard onSaved={refresh} />
      )}

      {!isProfileEmpty && profile && (
        <>
          {/* Partner Status Card */}
          <div className="rounded-xl border-2 border-amber-300 bg-white p-6 dark:border-amber-700 dark:bg-stone-900">
            <div className="flex items-center gap-3 mb-6">
              <div className="rounded-lg bg-amber-500 p-2.5">
                <Handshake className="h-5 w-5 text-white" />
              </div>
              <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
                {intl.formatMessage({ id: 'partner.status' })}
              </h3>
            </div>

            <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-4">
              <div className="space-y-1.5">
                <span className="text-sm text-stone-500 dark:text-stone-400">
                  {profile.company}
                </span>
                <div className="flex items-center gap-2">
                  <Award className="h-4 w-4 text-amber-500" />
                  <span
                    className={cn(
                      'inline-flex rounded-full px-3 py-1 text-sm font-semibold',
                      TIER_COLORS[profile.tier] ?? TIER_COLORS.standard,
                    )}
                  >
                    {intl.formatMessage({ id: `partner.tier.${profile.tier}` })}
                  </span>
                </div>
              </div>

              <div className="space-y-1.5">
                <span className="text-sm text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'partner.partnerId' })}
                </span>
                <code className="block rounded bg-stone-100 px-2 py-0.5 font-mono text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
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
          </div>

          {/* Sales Dashboard - 4 StatCards */}
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
            <StatCard
              icon={<FileText className="h-5 w-5 text-white" />}
              label={intl.formatMessage({ id: 'partner.totalSold' })}
              value={(stats?.total_sold ?? 0).toLocaleString()}
              bg="bg-amber-500"
            />
            <StatCard
              icon={<Users className="h-5 w-5 text-white" />}
              label={intl.formatMessage({ id: 'partner.activeCustomers' })}
              value={(stats?.active_customers ?? 0).toLocaleString()}
              bg="bg-emerald-500"
            />
            <StatCard
              icon={<DollarSign className="h-5 w-5 text-white" />}
              label={intl.formatMessage({ id: 'partner.monthlyRevenue' })}
              value={formatDollars(stats?.this_month_commission_cents ?? 0)}
              bg="bg-sky-500"
            />
            <StatCard
              icon={<TrendingUp className="h-5 w-5 text-white" />}
              label={intl.formatMessage({ id: 'partner.commission' })}
              value={formatDollars(stats?.lifetime_commission_cents ?? 0)}
              bg="bg-violet-500"
            />
          </div>

          {/* Customer Management Table */}
          <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
            <div className="mb-5 flex items-center justify-between">
              <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
                {intl.formatMessage({ id: 'partner.customers' })}
              </h3>
              <button
                type="button"
                onClick={() => setShowAddCustomer(true)}
                className="inline-flex items-center gap-1.5 rounded-lg bg-amber-500 px-3 py-1.5 text-sm font-medium text-white transition-colors hover:bg-amber-600"
              >
                <Plus className="h-4 w-4" />
                {intl.formatMessage({ id: 'partner.addCustomer' })}
              </button>
            </div>

            {customers.length === 0 ? (
              <div className="py-8 text-center text-sm text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'partner.empty' })}
              </div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-stone-200 dark:border-stone-700">
                      <th className="py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.customerName' })}
                      </th>
                      <th className="py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.licenseTier' })}
                      </th>
                      <th className="py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.activated' })}
                      </th>
                      <th className="py-3 text-left font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'billing.status' })}
                      </th>
                      <th className="py-3 text-right font-medium text-stone-500 dark:text-stone-400">
                        {intl.formatMessage({ id: 'partner.actions' })}
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {customers.map((customer) => (
                      <tr
                        key={customer.id}
                        className="border-b border-stone-100 last:border-0 dark:border-stone-800"
                      >
                        <td className="py-3 text-sm font-medium text-stone-900 dark:text-stone-100">
                          {customer.name}
                        </td>
                        <td className="py-3">
                          <span
                            className={cn(
                              'inline-flex rounded-full px-2.5 py-0.5 text-xs font-semibold',
                              CUSTOMER_TIER_COLORS[customer.tier] ??
                                CUSTOMER_TIER_COLORS.standard,
                            )}
                          >
                            {intl.formatMessage({ id: `license.${customer.tier}` })}
                          </span>
                        </td>
                        <td className="py-3 text-sm text-stone-600 dark:text-stone-400">
                          {new Date(customer.activated_at).toLocaleDateString()}
                        </td>
                        <td className="py-3">
                          <span
                            className={cn(
                              'inline-flex rounded-full px-2.5 py-0.5 text-xs font-semibold capitalize',
                              STATUS_COLORS[customer.status] ?? STATUS_COLORS.active,
                            )}
                          >
                            {intl.formatMessage({
                              id: `partner.status.${customer.status}`,
                            })}
                          </span>
                        </td>
                        <td className="py-3 text-right">
                          <div className="flex items-center justify-end gap-2">
                            <button className="rounded-lg border border-stone-300 p-1.5 text-stone-500 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-400 dark:hover:bg-stone-800">
                              <Eye className="h-3.5 w-3.5" />
                            </button>
                            <button className="rounded-lg border border-stone-300 p-1.5 text-stone-500 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-400 dark:hover:bg-stone-800">
                              <RefreshCw className="h-3.5 w-3.5" />
                            </button>
                          </div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        </>
      )}

      {/* License Generation Section — kept per spec */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-5 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'partner.generateLicense' })}
        </h3>

        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {/* Tier selector */}
          <div className="space-y-1.5">
            <label className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.licenseTier' })}
            </label>
            <select
              value={licenseTier}
              onChange={(e) => setLicenseTier(e.target.value)}
              className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
            >
              <option value="pro">Pro</option>
              <option value="enterprise">Enterprise</option>
            </select>
          </div>

          {/* Customer name */}
          <div className="space-y-1.5">
            <label className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.customerName' })}
            </label>
            <input
              type="text"
              value={customerName}
              onChange={(e) => setCustomerName(e.target.value)}
              placeholder="Acme Corp"
              className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500"
            />
          </div>

          {/* Duration selector */}
          <div className="space-y-1.5">
            <label className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.duration' })}
            </label>
            <select
              value={duration}
              onChange={(e) => setDuration(Number(e.target.value))}
              className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
            >
              {DURATION_OPTIONS.map((opt) => (
                <option key={opt.months} value={opt.months}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>

          {/* Generate button */}
          <div className="flex items-end">
            <button
              onClick={handleGenerate}
              disabled={generating || !customerName.trim()}
              className="w-full rounded-lg bg-amber-500 px-5 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
            >
              {generating
                ? intl.formatMessage({ id: 'common.saving' })
                : intl.formatMessage({ id: 'partner.generate' })}
            </button>
          </div>
        </div>

        {/* Generated key display */}
        {generatedKey && (
          <div className="mt-4 flex items-center gap-3 rounded-lg border border-emerald-200 bg-emerald-50 p-4 dark:border-emerald-800 dark:bg-emerald-900/20">
            <code className="flex-1 font-mono text-sm font-semibold text-emerald-700 dark:text-emerald-400">
              {generatedKey}
            </code>
            <button
              onClick={handleCopy}
              className="rounded-lg border border-emerald-300 p-2 text-emerald-600 transition-colors hover:bg-emerald-100 dark:border-emerald-700 dark:text-emerald-400 dark:hover:bg-emerald-900/40"
            >
              {copied ? (
                <Check className="h-4 w-4" />
              ) : (
                <Copy className="h-4 w-4" />
              )}
            </button>
          </div>
        )}

        {/* License generation error */}
        {licenseError && (
          <div
            role="alert"
            className="mt-4 flex items-start justify-between gap-3 rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300"
          >
            <span className="flex-1">{licenseError}</span>
            <button
              type="button"
              onClick={() => setLicenseError(null)}
              className="shrink-0 text-rose-500 hover:text-rose-700 dark:text-rose-400 dark:hover:text-rose-200"
              aria-label="Dismiss"
            >
              <svg
                xmlns="http://www.w3.org/2000/svg"
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <line x1="18" y1="6" x2="6" y2="18"></line>
                <line x1="6" y1="6" x2="18" y2="18"></line>
              </svg>
            </button>
          </div>
        )}
      </div>

      {/* Marketing Materials */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-5 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'partner.materials' })}
        </h3>

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
      </div>

      {showAddCustomer && (
        <AddCustomerModal
          onClose={() => setShowAddCustomer(false)}
          onSaved={() => {
            setShowAddCustomer(false);
            void refresh();
          }}
        />
      )}
    </div>
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
    <form
      onSubmit={handleSubmit}
      className="rounded-xl border-2 border-dashed border-amber-300 bg-amber-50/40 p-6 dark:border-amber-700 dark:bg-amber-900/10"
    >
      <div className="mb-2 flex items-center gap-3">
        <div className="rounded-lg bg-amber-500 p-2.5">
          <Handshake className="h-5 w-5 text-white" />
        </div>
        <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'partner.setup.title' })}
        </h3>
      </div>
      <p className="mb-5 text-sm text-stone-600 dark:text-stone-400">
        {intl.formatMessage({ id: 'partner.setup.description' })}
      </p>

      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5">
          <label className="text-sm text-stone-600 dark:text-stone-300">
            {intl.formatMessage({ id: 'partner.setup.company' })}
          </label>
          <input
            type="text"
            value={company}
            onChange={(e) => setCompany(e.target.value)}
            required
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          />
        </div>
        <div className="space-y-1.5">
          <label className="text-sm text-stone-600 dark:text-stone-300">
            {intl.formatMessage({ id: 'partner.setup.tier' })}
          </label>
          <select
            value={tier}
            onChange={(e) => setTier(e.target.value)}
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          >
            {PROFILE_TIERS.map((t) => (
              <option key={t} value={t}>
                {intl.formatMessage({ id: `partner.tier.${t}` })}
              </option>
            ))}
          </select>
        </div>
        <div className="space-y-1.5">
          <label className="text-sm text-stone-600 dark:text-stone-300">
            {intl.formatMessage({ id: 'partner.setup.partnerId' })}
          </label>
          <input
            type="text"
            value={partnerId}
            onChange={(e) => setPartnerId(e.target.value)}
            placeholder="PTR-2025-0042"
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          />
        </div>
        <div className="space-y-1.5">
          <label className="text-sm text-stone-600 dark:text-stone-300">
            {intl.formatMessage({ id: 'partner.setup.certifiedAt' })}
          </label>
          <input
            type="date"
            value={certifiedAt}
            onChange={(e) => setCertifiedAt(e.target.value)}
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
          />
        </div>
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
        <button
          type="submit"
          disabled={saving || !company.trim()}
          className="rounded-lg bg-amber-500 px-5 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          {saving
            ? intl.formatMessage({ id: 'common.saving' })
            : intl.formatMessage({ id: 'partner.setup.save' })}
        </button>
      </div>
    </form>
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
        className="w-full max-w-lg rounded-xl border border-stone-200 bg-white p-6 shadow-xl dark:border-stone-700 dark:bg-stone-900"
      >
        <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'partner.addCustomer' })}
        </h3>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-sm text-stone-600 dark:text-stone-300">
              {intl.formatMessage({ id: 'partner.customerName' })}
            </label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
              className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
            />
          </div>

          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-1.5">
              <label className="text-sm text-stone-600 dark:text-stone-300">
                {intl.formatMessage({ id: 'partner.licenseTier' })}
              </label>
              <select
                value={tier}
                onChange={(e) => setTier(e.target.value)}
                className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
              >
                {CUSTOMER_TIERS.map((t) => (
                  <option key={t} value={t}>
                    {intl.formatMessage({ id: `license.${t}` })}
                  </option>
                ))}
              </select>
            </div>
            <div className="space-y-1.5">
              <label className="text-sm text-stone-600 dark:text-stone-300">
                {intl.formatMessage({ id: 'billing.status' })}
              </label>
              <select
                value={status}
                onChange={(e) => setStatus(e.target.value)}
                className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
              >
                {CUSTOMER_STATUSES.map((s) => (
                  <option key={s} value={s}>
                    {intl.formatMessage({ id: `partner.status.${s}` })}
                  </option>
                ))}
              </select>
            </div>
            <div className="space-y-1.5">
              <label className="text-sm text-stone-600 dark:text-stone-300">
                {intl.formatMessage({ id: 'partner.activated' })}
              </label>
              <input
                type="date"
                value={activatedAt}
                onChange={(e) => setActivatedAt(e.target.value)}
                required
                className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-sm text-stone-600 dark:text-stone-300">
                {intl.formatMessage({ id: 'partner.commissionDollars' })}
              </label>
              <input
                type="number"
                min="0"
                step="0.01"
                value={commissionDollars}
                onChange={(e) => setCommissionDollars(e.target.value)}
                className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
              />
            </div>
          </div>

          <div className="space-y-1.5">
            <label className="text-sm text-stone-600 dark:text-stone-300">
              {intl.formatMessage({ id: 'partner.notes' })}
            </label>
            <textarea
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              rows={2}
              className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
            />
          </div>
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
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg border border-stone-300 px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-800"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            type="submit"
            disabled={saving || !name.trim()}
            className="rounded-lg bg-amber-500 px-5 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            {saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </form>
    </div>
  );
}

// ── Sub-components ───────────────────────────────────────

function StatCard({
  icon,
  label,
  value,
  bg,
}: {
  readonly icon: React.ReactNode;
  readonly label: string;
  readonly value: string;
  readonly bg: string;
}) {
  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3">
        <div className={cn('rounded-lg p-2.5', bg)}>{icon}</div>
        <div>
          <p className="text-sm text-stone-500 dark:text-stone-400">{label}</p>
          <p className="text-xl font-semibold text-stone-900 dark:text-stone-50">
            {value}
          </p>
        </div>
      </div>
    </div>
  );
}

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
      className="flex items-start gap-3 rounded-xl border border-stone-200 p-4 transition-colors hover:bg-stone-50 dark:border-stone-700 dark:hover:bg-stone-800"
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
