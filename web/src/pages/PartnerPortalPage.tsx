import { useState } from 'react';
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
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';

// ── Placeholder Data (replaced by API when partner backend is available) ──

const PARTNER_STATUS = {
  tier: 'gold' as const,
  company: 'TechBridge Solutions Ltd.',
  certified: true,
  certifiedDate: '2025-08-15',
  partnerId: 'PTR-2025-0042',
};

const SALES_STATS = {
  totalSold: 147,
  activeCustomers: 89,
  monthlyRevenue: 24_680,
  commission: 3_702,
};

const MOCK_CUSTOMERS = [
  { id: 'cust-001', name: 'Sunrise Bakery Co.', tier: 'pro', activated: '2025-11-02', status: 'active' },
  { id: 'cust-002', name: 'FastTrack Logistics', tier: 'enterprise', activated: '2025-09-18', status: 'active' },
  { id: 'cust-003', name: 'GreenLeaf Organic', tier: 'pro', activated: '2026-01-05', status: 'active' },
  { id: 'cust-004', name: 'BlueSky Aviation', tier: 'enterprise', activated: '2025-07-22', status: 'expiring' },
  { id: 'cust-005', name: 'MountainView Hotel', tier: 'pro', activated: '2025-12-10', status: 'active' },
  { id: 'cust-006', name: 'Pacific Trading Inc.', tier: 'pro', activated: '2024-06-30', status: 'expired' },
];

const TIER_COLORS: Record<string, string> = {
  silver: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
  gold: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  platinum: 'bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400',
};

const STATUS_COLORS: Record<string, string> = {
  active: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
  expiring: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  expired: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
};

const DURATION_OPTIONS = [
  { months: 12, label: '1yr' },
  { months: 24, label: '2yr' },
  { months: 36, label: '3yr' },
];

// ── Component ────────────────────────────────────────────

export function PartnerPortalPage() {
  const intl = useIntl();
  const [licenseTier, setLicenseTier] = useState('pro');
  const [customerName, setCustomerName] = useState('');
  const [duration, setDuration] = useState(12);
  const [generatedKey, setGeneratedKey] = useState('');
  const [generating, setGenerating] = useState(false);
  const [copied, setCopied] = useState(false);

  const handleGenerate = async () => {
    if (!customerName.trim()) return;
    setGenerating(true);
    setGeneratedKey('');
    try {
      const result = await api.partner.generateLicense({
        tier: licenseTier,
        customer: customerName,
        months: duration,
      });
      setGeneratedKey(result?.key ?? '');
    } catch {
      setGeneratedKey('');
      // TODO: show error to user
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

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'partner.title' })}
      </h2>

      {/* Demo Banner */}
      <div className="rounded-lg border border-stone-300 bg-stone-100 px-4 py-2 text-sm text-stone-600 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-400">
        Partner Portal is in preview mode. Data shown is for demonstration purposes.
      </div>

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
              {PARTNER_STATUS.company}
            </span>
            <div className="flex items-center gap-2">
              <Award className="h-4 w-4 text-amber-500" />
              <span
                className={cn(
                  'inline-flex rounded-full px-3 py-1 text-sm font-semibold',
                  TIER_COLORS[PARTNER_STATUS.tier]
                )}
              >
                {intl.formatMessage({
                  id: `partner.tier.${PARTNER_STATUS.tier}`,
                })}
              </span>
            </div>
          </div>

          <div className="space-y-1.5">
            <span className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.partnerId' })}
            </span>
            <code className="block rounded bg-stone-100 px-2 py-0.5 font-mono text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
              {PARTNER_STATUS.partnerId}
            </code>
          </div>

          <div className="space-y-1.5">
            <span className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.certification' })}
            </span>
            <div className="flex items-center gap-2">
              {PARTNER_STATUS.certified ? (
                <>
                  <Check className="h-4 w-4 text-emerald-500" />
                  <span className="text-sm font-medium text-emerald-600 dark:text-emerald-400">
                    {intl.formatMessage({ id: 'partner.certified' })}
                  </span>
                </>
              ) : (
                <span className="text-sm text-stone-500">{intl.formatMessage({ id: 'partner.pending' })}</span>
              )}
            </div>
          </div>

          <div className="space-y-1.5">
            <span className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'partner.since' })}
            </span>
            <span className="text-sm font-medium text-stone-900 dark:text-stone-50">
              {new Date(PARTNER_STATUS.certifiedDate).toLocaleDateString()}
            </span>
          </div>
        </div>
      </div>

      {/* Sales Dashboard - 4 StatCards */}
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          icon={<FileText className="h-5 w-5 text-white" />}
          label={intl.formatMessage({ id: 'partner.totalSold' })}
          value={SALES_STATS.totalSold.toLocaleString()}
          bg="bg-amber-500"
        />
        <StatCard
          icon={<Users className="h-5 w-5 text-white" />}
          label={intl.formatMessage({ id: 'partner.activeCustomers' })}
          value={SALES_STATS.activeCustomers.toLocaleString()}
          bg="bg-emerald-500"
        />
        <StatCard
          icon={<DollarSign className="h-5 w-5 text-white" />}
          label={intl.formatMessage({ id: 'partner.monthlyRevenue' })}
          value={`$${SALES_STATS.monthlyRevenue.toLocaleString()}`}
          bg="bg-sky-500"
        />
        <StatCard
          icon={<TrendingUp className="h-5 w-5 text-white" />}
          label={intl.formatMessage({ id: 'partner.commission' })}
          value={`$${SALES_STATS.commission.toLocaleString()}`}
          bg="bg-violet-500"
        />
      </div>

      {/* Customer Management Table */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-5 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'partner.customers' })}
        </h3>

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
              {MOCK_CUSTOMERS.map((customer) => (
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
                        customer.tier === 'enterprise'
                          ? 'bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400'
                          : 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
                      )}
                    >
                      {intl.formatMessage({ id: `license.${customer.tier}` })}
                    </span>
                  </td>
                  <td className="py-3 text-sm text-stone-600 dark:text-stone-400">
                    {new Date(customer.activated).toLocaleDateString()}
                  </td>
                  <td className="py-3">
                    <span
                      className={cn(
                        'inline-flex rounded-full px-2.5 py-0.5 text-xs font-semibold capitalize',
                        STATUS_COLORS[customer.status]
                      )}
                    >
                      {intl.formatMessage({ id: `partner.status.${customer.status}` })}
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
      </div>

      {/* License Generation Section */}
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
