import { useEffect, useState, type ComponentType, type ReactNode } from 'react';
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
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardAction,
  Badge,
  Button,
  Empty,
  Input,
  Textarea,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogClose,
} from '@/components/mds';
import {
  api,
  type PartnerProfile,
  type PartnerStats,
  type PartnerCustomer,
} from '@/lib/api';

// ── Styling helpers (not mock data) ──────────────────────

type BadgeTone = 'neutral' | 'success' | 'warning' | 'danger' | 'info' | 'accent';

/** Map a Calm-Glass-era tone name to an MDS Badge className override. */
const TONE_CLASS: Record<BadgeTone, string> = {
  neutral: '',
  success: 'bg-success/15 text-success',
  warning: 'bg-warning/15 text-warning',
  danger: 'bg-destructive/10 text-destructive',
  info: 'bg-info/15 text-info',
  accent: 'bg-brand/15 text-brand',
};

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

/** A themed pill using the tone→class map above. */
function ToneBadge({ tone, children }: { tone: BadgeTone; children: ReactNode }) {
  return (
    <Badge variant="secondary" className={cn(TONE_CLASS[tone])}>
      {children}
    </Badge>
  );
}

/** Local labeled-field wrapper (spec §4 form pattern). */
function Field({
  label,
  required,
  children,
  className,
}: {
  label: string;
  required?: boolean;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('space-y-1.5', className)}>
      <label className="text-xs font-medium text-muted-foreground">
        {label}
        {required && <span className="ml-0.5 text-destructive">*</span>}
      </label>
      {children}
    </div>
  );
}

/** KPI tile (spec §5.5). */
function StatTile({
  icon: Icon,
  tone,
  label,
  value,
}: {
  icon: ComponentType<{ className?: string }>;
  tone: 'brand' | 'success' | 'warning' | 'neutral';
  label: string;
  value: string;
}) {
  const toneClass =
    tone === 'success'
      ? 'text-success'
      : tone === 'warning'
        ? 'text-warning'
        : tone === 'neutral'
          ? 'text-muted-foreground'
          : 'text-brand';
  return (
    <div className="rounded-lg border border-surface-border bg-card p-4">
      <div className="flex items-center gap-2">
        <Icon className={cn('size-4', toneClass)} />
        <p className="text-sm text-muted-foreground">{label}</p>
      </div>
      <p className="mt-2 text-2xl font-semibold tabular-nums text-foreground">{value}</p>
    </div>
  );
}

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
    <div className="space-y-6">
      {/* Top-of-page load error (rose alert, dismissible). */}
      {loadError && (
        <div
          role="alert"
          className="flex items-start justify-between gap-3 rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
        >
          <span className="flex-1">
            {intl.formatMessage({ id: 'partner.loadError' }, { message: loadError })}
          </span>
          <button
            type="button"
            onClick={() => setLoadError(null)}
            className="shrink-0 text-destructive/70 hover:text-destructive"
            aria-label="Dismiss"
          >
            <X className="size-4" />
          </button>
        </div>
      )}

      {loading && !profile && (
        <Card>
          <CardContent>
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
              <span>{intl.formatMessage({ id: 'common.loading' })}</span>
            </div>
          </CardContent>
        </Card>
      )}

      {!loading && isProfileEmpty && <PartnerOnboardingCard onSaved={refresh} />}

      {!isProfileEmpty && profile && (
        <>
          {/* Partner Status Card */}
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Handshake className="size-4 text-brand" />
                {intl.formatMessage({ id: 'partner.status' })}
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-4">
                <div className="space-y-1.5">
                  <span className="text-sm text-muted-foreground">{profile.company}</span>
                  <div className="flex items-center gap-2">
                    <Award className="size-4 text-brand" />
                    <ToneBadge tone={TIER_TONES[profile.tier] ?? 'neutral'}>
                      {intl.formatMessage({ id: `partner.tier.${profile.tier}` })}
                    </ToneBadge>
                  </div>
                </div>

                <div className="space-y-1.5">
                  <span className="text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'partner.partnerId' })}
                  </span>
                  <code className="block w-fit rounded bg-muted px-2 py-0.5 font-mono text-xs text-muted-foreground">
                    {profile.partner_id ?? '—'}
                  </code>
                </div>

                <div className="space-y-1.5">
                  <span className="text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'partner.certification' })}
                  </span>
                  <div className="flex items-center gap-2">
                    {profile.certified_at ? (
                      <>
                        <Check className="size-4 text-success" />
                        <span className="text-sm font-medium text-success">
                          {intl.formatMessage({ id: 'partner.certified' })}
                        </span>
                      </>
                    ) : (
                      <span className="text-sm text-muted-foreground">
                        {intl.formatMessage({ id: 'partner.pending' })}
                      </span>
                    )}
                  </div>
                </div>

                <div className="space-y-1.5">
                  <span className="text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'partner.since' })}
                  </span>
                  <span className="text-sm font-medium text-foreground">
                    {profile.certified_at ? new Date(profile.certified_at).toLocaleDateString() : '—'}
                  </span>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Sales Dashboard — 4 KPI tiles */}
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
            <StatTile
              icon={FileText}
              tone="brand"
              label={intl.formatMessage({ id: 'partner.totalSold' })}
              value={(stats?.total_sold ?? 0).toLocaleString()}
            />
            <StatTile
              icon={Users}
              tone="success"
              label={intl.formatMessage({ id: 'partner.activeCustomers' })}
              value={(stats?.active_customers ?? 0).toLocaleString()}
            />
            <StatTile
              icon={DollarSign}
              tone="warning"
              label={intl.formatMessage({ id: 'partner.monthlyRevenue' })}
              value={formatDollars(stats?.this_month_commission_cents ?? 0)}
            />
            <StatTile
              icon={TrendingUp}
              tone="neutral"
              label={intl.formatMessage({ id: 'partner.commission' })}
              value={formatDollars(stats?.lifetime_commission_cents ?? 0)}
            />
          </div>

          {/* Customer Management Table */}
          <Card>
            <CardHeader>
              <CardTitle>{intl.formatMessage({ id: 'partner.customers' })}</CardTitle>
              <CardAction>
                <Button variant="brand" size="sm" onClick={() => setShowAddCustomer(true)}>
                  <Plus />
                  {intl.formatMessage({ id: 'partner.addCustomer' })}
                </Button>
              </CardAction>
            </CardHeader>
            {customers.length === 0 ? (
              <CardContent>
                <Empty icon={Users} title={intl.formatMessage({ id: 'partner.empty' })} variant="dashed" />
              </CardContent>
            ) : (
              <div className="overflow-x-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{intl.formatMessage({ id: 'partner.customerName' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'partner.licenseTier' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'partner.activated' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.status' })}</TableHead>
                      <TableHead className="text-right">{intl.formatMessage({ id: 'partner.actions' })}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {customers.map((customer) => (
                      <TableRow key={customer.id}>
                        <TableCell className="font-medium text-foreground">{customer.name}</TableCell>
                        <TableCell>
                          <ToneBadge tone={CUSTOMER_TIER_TONES[customer.tier] ?? 'neutral'}>
                            {intl.formatMessage({ id: `license.${customer.tier}` })}
                          </ToneBadge>
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {new Date(customer.activated_at).toLocaleDateString()}
                        </TableCell>
                        <TableCell>
                          <ToneBadge tone={STATUS_TONES[customer.status] ?? 'success'}>
                            {intl.formatMessage({ id: `partner.status.${customer.status}` })}
                          </ToneBadge>
                        </TableCell>
                        <TableCell className="text-right">
                          <div className="flex items-center justify-end gap-1">
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              onClick={() => setEditCustomer(customer)}
                              title={intl.formatMessage({ id: 'common.edit' })}
                              aria-label={intl.formatMessage({ id: 'common.edit' })}
                            >
                              <Pencil />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              onClick={() => setDeleteCustomer(customer)}
                              title={intl.formatMessage({ id: 'common.delete' })}
                              aria-label={intl.formatMessage({ id: 'common.delete' })}
                              className="text-destructive hover:bg-destructive/10"
                            >
                              <Trash2 />
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            )}
          </Card>
        </>
      )}

      {/* License Generation — CLI-only (UI.4). License activation is not exposed
          over the dashboard RPC; surface a clear pointer to the CLI instead of a
          non-functional client-side stub. */}
      <Card>
        <CardHeader>
          <CardTitle>{intl.formatMessage({ id: 'partner.generateLicense' })}</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="flex items-start gap-3 rounded-lg border border-surface-border bg-muted/50 p-4">
            <Terminal className="mt-0.5 size-5 shrink-0 text-muted-foreground" />
            <div className="space-y-2">
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'partner.license.cliOnly' })}
              </p>
              <code className="block rounded bg-stone-900 px-3 py-2 font-mono text-xs text-emerald-400">
                duduclaw license generate --tier &lt;pro|enterprise&gt; --customer &lt;name&gt; --months &lt;n&gt;
              </code>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Marketing Materials */}
      <Card>
        <CardHeader>
          <CardTitle>{intl.formatMessage({ id: 'partner.materials' })}</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <MaterialCard
              icon={<Presentation className="size-5 text-brand" />}
              title={intl.formatMessage({ id: 'partner.downloadSlides' })}
              description={intl.formatMessage({ id: 'partner.slideDecks' }) + ' (PDF, 4.2 MB)'}
              href="#"
            />
            <MaterialCard
              icon={<FileText className="size-5 text-brand" />}
              title={intl.formatMessage({ id: 'partner.dmTemplate' })}
              description={intl.formatMessage({ id: 'partner.dmTemplate' }) + ' (DOCX, 1.8 MB)'}
              href="#"
            />
            <MaterialCard
              icon={<BookOpen className="size-5 text-brand" />}
              title={intl.formatMessage({ id: 'partner.downloadCaseStudy' })}
              description={intl.formatMessage({ id: 'partner.caseStudies' }) + ' (PDF, 6.1 MB)'}
              href="#"
            />
          </div>
        </CardContent>
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

      <Dialog open={deleteCustomer !== null} onOpenChange={(o) => !o && setDeleteCustomer(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'partner.customer.delete' })}</DialogTitle>
          </DialogHeader>
          <p className="text-sm text-muted-foreground">
            {deleteCustomer &&
              intl.formatMessage({ id: 'partner.customer.delete.confirm' }, { name: deleteCustomer.name })}
          </p>
          <DialogFooter>
            <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
            <Button variant="destructive" onClick={handleDeleteCustomer}>
              {intl.formatMessage({ id: 'common.delete' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
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
      await api.partner.updateCustomer(customer.id, { name, tier, status, notes });
      toast.success(intl.formatMessage({ id: 'partner.customer.updated' }));
      onSaved();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'partner.customer.edit' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <Field label={intl.formatMessage({ id: 'partner.customerName' })}>
            <Input type="text" value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <div className="grid grid-cols-2 gap-3">
            <Field label={intl.formatMessage({ id: 'partner.licenseTier' })}>
              <Select value={tier} onValueChange={(v) => setTier(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>{intl.formatMessage({ id: `license.${tier}` })}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {CUSTOMER_TIERS.map((tv) => (
                    <SelectItem key={tv} value={tv}>
                      {intl.formatMessage({ id: `license.${tv}` })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label={intl.formatMessage({ id: 'billing.status' })}>
              <Select value={status} onValueChange={(v) => setStatus(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>{intl.formatMessage({ id: `partner.status.${status}` })}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {CUSTOMER_STATUSES.map((sv) => (
                    <SelectItem key={sv} value={sv}>
                      {intl.formatMessage({ id: `partner.status.${sv}` })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
          <Field label={intl.formatMessage({ id: 'partner.customer.notes' })}>
            <Textarea value={notes} onChange={(e) => setNotes(e.target.value)} rows={3} />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="brand" onClick={handleSubmit} disabled={submitting}>
            {submitting ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
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
      const certifiedAtIso = certifiedAt ? `${certifiedAt}T00:00:00+00:00` : null;
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
      <CardContent>
        <form onSubmit={handleSubmit}>
          <div className="mb-2 flex items-center gap-3">
            <span className="grid size-10 shrink-0 place-items-center rounded-xl bg-brand/12 text-brand ring-1 ring-inset ring-brand/20">
              <Handshake className="size-5" />
            </span>
            <h3 className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'partner.setup.title' })}
            </h3>
          </div>
          <p className="mb-5 text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'partner.setup.description' })}
          </p>

          <div className="grid gap-4 sm:grid-cols-2">
            <Field label={intl.formatMessage({ id: 'partner.setup.company' })} required>
              <Input type="text" value={company} onChange={(e) => setCompany(e.target.value)} required />
            </Field>
            <Field label={intl.formatMessage({ id: 'partner.setup.tier' })}>
              <Select value={tier} onValueChange={(v) => setTier(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>{intl.formatMessage({ id: `partner.tier.${tier}` })}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {PROFILE_TIERS.map((tv) => (
                    <SelectItem key={tv} value={tv}>
                      {intl.formatMessage({ id: `partner.tier.${tv}` })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label={intl.formatMessage({ id: 'partner.setup.partnerId' })}>
              <Input
                type="text"
                value={partnerId}
                onChange={(e) => setPartnerId(e.target.value)}
                placeholder="PTR-2025-0042"
              />
            </Field>
            <Field label={intl.formatMessage({ id: 'partner.setup.certifiedAt' })}>
              <Input type="date" value={certifiedAt} onChange={(e) => setCertifiedAt(e.target.value)} />
            </Field>
          </div>

          {error && (
            <div
              role="alert"
              className="mt-4 rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-2 text-sm text-destructive"
            >
              {error}
            </div>
          )}

          <div className="mt-5 flex justify-end">
            <Button type="submit" variant="brand" disabled={saving || !company.trim()}>
              {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'partner.setup.save' })}
            </Button>
          </div>
        </form>
      </CardContent>
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
  const [activatedAt, setActivatedAt] = useState(new Date().toISOString().slice(0, 10));
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
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'partner.addCustomer' })}</DialogTitle>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="space-y-4">
          <Field label={intl.formatMessage({ id: 'partner.customerName' })} required>
            <Input type="text" value={name} onChange={(e) => setName(e.target.value)} required />
          </Field>

          <div className="grid gap-4 sm:grid-cols-2">
            <Field label={intl.formatMessage({ id: 'partner.licenseTier' })}>
              <Select value={tier} onValueChange={(v) => setTier(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>{intl.formatMessage({ id: `license.${tier}` })}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {CUSTOMER_TIERS.map((tv) => (
                    <SelectItem key={tv} value={tv}>
                      {intl.formatMessage({ id: `license.${tv}` })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label={intl.formatMessage({ id: 'billing.status' })}>
              <Select value={status} onValueChange={(v) => setStatus(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>{intl.formatMessage({ id: `partner.status.${status}` })}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {CUSTOMER_STATUSES.map((sv) => (
                    <SelectItem key={sv} value={sv}>
                      {intl.formatMessage({ id: `partner.status.${sv}` })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label={intl.formatMessage({ id: 'partner.activated' })}>
              <Input type="date" value={activatedAt} onChange={(e) => setActivatedAt(e.target.value)} required />
            </Field>
            <Field label={intl.formatMessage({ id: 'partner.commissionDollars' })}>
              <Input
                type="number"
                min="0"
                step="0.01"
                value={commissionDollars}
                onChange={(e) => setCommissionDollars(e.target.value)}
              />
            </Field>
          </div>

          <Field label={intl.formatMessage({ id: 'partner.notes' })}>
            <Textarea value={notes} onChange={(e) => setNotes(e.target.value)} rows={2} />
          </Field>

          {error && (
            <div
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-2 text-sm text-destructive"
            >
              {error}
            </div>
          )}

          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {intl.formatMessage({ id: 'common.cancel' })}
            </Button>
            <Button type="submit" variant="brand" disabled={saving || !name.trim()}>
              {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

// ── Sub-components ───────────────────────────────────────

function MaterialCard({
  icon,
  title,
  description,
  href,
}: {
  readonly icon: ReactNode;
  readonly title: string;
  readonly description: string;
  readonly href: string;
}) {
  return (
    <a
      href={href}
      className="flex items-start gap-3 rounded-xl border border-surface-border bg-surface p-4 transition-colors hover:bg-surface-hover"
    >
      <div className="mt-0.5">{icon}</div>
      <div className="flex-1">
        <p className="text-sm font-medium text-foreground">{title}</p>
        <p className="mt-1 text-xs text-muted-foreground">{description}</p>
      </div>
      <Download className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
    </a>
  );
}
