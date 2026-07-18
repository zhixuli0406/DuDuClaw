import { useEffect, useState, useCallback, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import {
  Store,
  Plus,
  KeyRound,
  Copy,
  Check,
  Trash2,
  Ban,
  Info,
  FileSignature,
  Download,
  Palette,
  MoreHorizontal,
  Loader2,
} from 'lucide-react';
import {
  Card,
  CardContent,
  Tabs,
  TabsList,
  TabsTab,
  Button,
  Badge,
  Input,
  Textarea,
  Checkbox,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  Empty,
} from '@/components/mds';
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

/** ListGrid column templates (spec §4 ListGrid). */
const DIST_COLUMNS = 'minmax(0,1.6fr) minmax(0,1.4fr) minmax(0,0.7fr) 2.5rem';
const LEDGER_COLUMNS =
  'minmax(0,1.4fr) minmax(0,1fr) minmax(0,0.7fr) minmax(0,1fr) minmax(0,1.4fr) 2.5rem';

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

/** Compact a long license id to `first8…last4` for the ledger. */
function maskId(id: string): string {
  return id.length <= 12 ? id : `${id.slice(0, 8)}…${id.slice(-4)}`;
}

/** Stacked label + control block used across the dialogs (spec §5.3). */
function DialogField({
  label,
  help,
  htmlFor,
  children,
}: {
  label: string;
  help?: string;
  htmlFor?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label htmlFor={htmlFor} className="text-sm font-medium text-foreground">
        {label}
      </label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
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

  // Flatten every distributor's issued licenses into a single ledger — the
  // "customer" column reuses the parent distributor name (IssuedLicense has no
  // human-facing customer/plan field to surface here).
  const ledgerRows = distributors.flatMap((d) =>
    (d.licenses ?? []).map((lic) => ({ lic, distributorName: d.name })),
  );

  return (
    <div className="mx-auto w-full max-w-5xl space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Store className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">
              {intl.formatMessage({ id: 'manage.distributors' })}
            </h1>
            <p className="text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'whitelabel.subtitle' })}
            </p>
          </div>
        </div>
        {tab === 'distributors' && issuerConfigured && (
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => setSignBundleOpen(true)}>
              <FileSignature />
              <span className="hidden sm:inline">
                {intl.formatMessage({ id: 'distributor.bundle.sign' })}
              </span>
            </Button>
            <Button variant="brand" size="sm" onClick={() => setAddOpen(true)}>
              <Plus />
              <span className="hidden sm:inline">
                {intl.formatMessage({ id: 'distributor.add' })}
              </span>
            </Button>
          </div>
        )}
      </div>

      <Tabs variant="line" value={tab} onValueChange={(v) => setTab(v as WhiteLabelTab)}>
        <TabsList>
          <TabsTab value="branding">
            <Palette />
            {intl.formatMessage({ id: 'settings.branding' })}
          </TabsTab>
          <TabsTab value="distributors">
            <Store />
            {intl.formatMessage({ id: 'distributor.title' })}
          </TabsTab>
        </TabsList>
      </Tabs>

      {tab === 'branding' && <BrandingTab />}

      {tab === 'distributors' &&
        (loading ? (
          <div className="flex items-center justify-center py-16">
            <Loader2 className="size-6 animate-spin text-muted-foreground" />
          </div>
        ) : !issuerConfigured ? (
          <Card>
            <CardContent>
              <Empty
                icon={KeyRound}
                title={intl.formatMessage({ id: 'distributor.noIssuer.title' })}
                description={intl.formatMessage({ id: 'distributor.noIssuer.hint' })}
              />
              <pre className="mx-auto mt-2 max-w-md overflow-x-auto rounded-lg bg-muted px-4 py-3 text-xs text-muted-foreground">
{`[distributor]
issuer_key_path = "~/.duduclaw/keys/issuer.key"
public_url = "https://your-gateway.example.com"`}
              </pre>
              <p className="mx-auto mt-2 max-w-md text-center text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'distributor.noIssuer.publicUrl' })}
              </p>
            </CardContent>
          </Card>
        ) : (
          <>
            {/* Control-plane (P2) status + distributor setup guidance */}
            <Card>
              <CardContent className="flex flex-wrap items-center justify-between gap-3">
                <div className="flex items-center gap-2">
                  <h2 className="text-sm font-semibold">
                    {intl.formatMessage({ id: 'distributor.controlPlane.title' })}
                  </h2>
                  <Badge
                    variant={refreshEndpointActive ? 'secondary' : 'ghost'}
                    className={refreshEndpointActive ? 'bg-success/15 text-success' : undefined}
                  >
                    {intl.formatMessage({
                      id: refreshEndpointActive
                        ? 'distributor.controlPlane.active'
                        : 'distributor.controlPlane.inactive',
                    })}
                  </Badge>
                </div>
              </CardContent>
              {refreshEndpointActive && (
                <CardContent>
                  <div className="flex items-start gap-2 rounded-lg border border-info/30 bg-info/10 px-3 py-2 text-xs">
                    <Info className="mt-0.5 h-4 w-4 shrink-0" />
                    <div className="min-w-0 space-y-1">
                      <p>{intl.formatMessage({ id: 'distributor.controlPlane.guide' })}</p>
                      <pre className="overflow-x-auto rounded bg-muted px-2 py-1 text-[11px] text-muted-foreground">
{`DUDUCLAW_CONTROL_URL=https://your-gateway.example.com`}
                      </pre>
                    </div>
                  </div>
                </CardContent>
              )}
            </Card>

            {/* Stats */}
            <div className="grid grid-cols-3 gap-3">
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

            {/* Distributors list */}
            <section className="space-y-3">
              <h2 className="text-sm font-medium">
                {intl.formatMessage({ id: 'distributor.list.title' })}
              </h2>
              {distributors.length === 0 ? (
                <Empty
                  icon={Store}
                  title={intl.formatMessage({ id: 'distributor.empty.title' })}
                  description={intl.formatMessage({ id: 'distributor.empty.hint' })}
                />
              ) : (
                <div className="overflow-hidden rounded-xl border border-surface-border">
                  <ListGridContainer
                    columns={DIST_COLUMNS}
                    className="!h-auto [&>[aria-hidden]]:hidden"
                    header={
                      <ListGridHeader>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.name' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.contact' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.status' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell aria-hidden />
                      </ListGridHeader>
                    }
                  >
                    {distributors.map((d) => {
                      const active = d.status === 'active';
                      return (
                        <ListGridRow key={d.id} rowSize="lg" className="cursor-default">
                          <ListGridCell>
                            <div className="min-w-0">
                              <p className="truncate text-sm font-medium text-foreground">{d.name}</p>
                              {d.note && (
                                <p className="truncate text-xs text-muted-foreground">{d.note}</p>
                              )}
                            </div>
                          </ListGridCell>
                          <ListGridCell>
                            <span className="truncate text-sm text-muted-foreground">
                              {d.contact || '—'}
                            </span>
                          </ListGridCell>
                          <ListGridCell>
                            <Badge
                              variant={active ? 'secondary' : 'outline'}
                              className={active ? 'bg-success/15 text-success' : undefined}
                            >
                              {d.status}
                            </Badge>
                          </ListGridCell>
                          <ListGridCell className="justify-end">
                            <DropdownMenu>
                              <DropdownMenuTrigger
                                render={
                                  <Button
                                    variant="ghost"
                                    size="icon-sm"
                                    aria-label={intl.formatMessage({ id: 'common.more' })}
                                    data-stop-row-nav
                                  />
                                }
                              >
                                <MoreHorizontal />
                              </DropdownMenuTrigger>
                              <DropdownMenuContent>
                                <DropdownMenuItem onClick={() => setIssueTarget(d)}>
                                  <KeyRound />
                                  {intl.formatMessage({ id: 'distributor.issue' })}
                                </DropdownMenuItem>
                                <DropdownMenuItem
                                  variant="destructive"
                                  onClick={() => setRemoveTarget(d)}
                                >
                                  <Trash2 />
                                  {intl.formatMessage({ id: 'distributor.remove' })}
                                </DropdownMenuItem>
                              </DropdownMenuContent>
                            </DropdownMenu>
                          </ListGridCell>
                        </ListGridRow>
                      );
                    })}
                  </ListGridContainer>
                </div>
              )}
            </section>

            {/* Issued-license ledger */}
            <section className="space-y-3">
              <h2 className="text-sm font-medium">
                {intl.formatMessage({ id: 'distributor.ledger.title' })}
              </h2>
              {ledgerRows.length === 0 ? (
                <Empty
                  icon={KeyRound}
                  title={intl.formatMessage({ id: 'distributor.ledger.empty' })}
                  description={intl.formatMessage({ id: 'distributor.empty.hint' })}
                />
              ) : (
                <div className="overflow-hidden rounded-xl border border-surface-border">
                  <ListGridContainer
                    columns={LEDGER_COLUMNS}
                    className="!h-auto [&>[aria-hidden]]:hidden"
                    header={
                      <ListGridHeader>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.license' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.customer' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.status' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.expires' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell>
                          {intl.formatMessage({ id: 'distributor.col.fingerprint' })}
                        </ListGridHeaderCell>
                        <ListGridHeaderCell aria-hidden />
                      </ListGridHeader>
                    }
                  >
                    {ledgerRows.map(({ lic, distributorName }) => {
                      const revoked = lic.status === 'revoked';
                      return (
                        <ListGridRow key={lic.id} className="cursor-default">
                          <ListGridCell>
                            <span className="truncate font-mono text-xs">{maskId(lic.id)}</span>
                          </ListGridCell>
                          <ListGridCell>
                            <span className="truncate text-sm text-muted-foreground">
                              {distributorName}
                            </span>
                          </ListGridCell>
                          <ListGridCell>
                            <Badge
                              variant={revoked ? 'destructive' : 'secondary'}
                              className={revoked ? undefined : 'bg-success/15 text-success'}
                            >
                              {lic.status}
                            </Badge>
                          </ListGridCell>
                          <ListGridCell>
                            <span className="truncate font-mono text-xs text-muted-foreground">
                              {lic.expires_at}
                            </span>
                          </ListGridCell>
                          <ListGridCell>
                            <span className="truncate font-mono text-xs text-muted-foreground">
                              {lic.machine_fingerprint}
                            </span>
                          </ListGridCell>
                          <ListGridCell className="justify-end">
                            {!revoked && (
                              <DropdownMenu>
                                <DropdownMenuTrigger
                                  render={
                                    <Button
                                      variant="ghost"
                                      size="icon-sm"
                                      aria-label={intl.formatMessage({ id: 'common.more' })}
                                      data-stop-row-nav
                                    />
                                  }
                                >
                                  <MoreHorizontal />
                                </DropdownMenuTrigger>
                                <DropdownMenuContent>
                                  <DropdownMenuItem
                                    variant="destructive"
                                    onClick={() => setRevokeTarget(lic)}
                                  >
                                    <Ban />
                                    {intl.formatMessage({ id: 'distributor.revoke' })}
                                  </DropdownMenuItem>
                                </DropdownMenuContent>
                              </DropdownMenu>
                            )}
                          </ListGridCell>
                        </ListGridRow>
                      );
                    })}
                  </ListGridContainer>
                </div>
              )}
            </section>
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
    </div>
  );
}

function StatTile({ label, value }: { label: string; value: number }) {
  return (
    <Card>
      <CardContent>
        <p className="text-xs text-muted-foreground">{label}</p>
        <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
      </CardContent>
    </Card>
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
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'distributor.add' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <DialogField label={intl.formatMessage({ id: 'distributor.field.name' })} htmlFor="dist-name">
            <Input
              id="dist-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              autoFocus
            />
          </DialogField>
          <DialogField
            label={intl.formatMessage({ id: 'distributor.field.contact' })}
            htmlFor="dist-contact"
          >
            <Input id="dist-contact" value={contact} onChange={(e) => setContact(e.target.value)} />
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'distributor.field.note' })} htmlFor="dist-note">
            <Input id="dist-note" value={note} onChange={(e) => setNote(e.target.value)} />
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button
            variant="brand"
            disabled={busy || name.trim().length === 0}
            onClick={() => onSubmit({ name: name.trim(), contact: contact.trim(), note: note.trim() })}
          >
            {busy ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
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
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'distributor.bundle.sign' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'distributor.bundle.sign.desc' })}
          </p>
          <DialogField label={intl.formatMessage({ id: 'distributor.bundle.selectDistributor' })}>
            <Select value={distributorId} onValueChange={(v) => setDistributorId(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>
                  {distributors.find((d) => d.id === distributorId)?.name}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                {distributors.map((d) => (
                  <SelectItem key={d.id} value={d.id}>
                    {d.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
          <DialogField
            label={intl.formatMessage({ id: 'distributor.bundle.brandingJson' })}
            help={intl.formatMessage({ id: 'distributor.bundle.brandingJson.hint' })}
            htmlFor="sign-json"
          >
            <Textarea
              id="sign-json"
              className="font-mono text-xs"
              rows={8}
              spellCheck={false}
              value={json}
              onChange={(e) => setJson(e.target.value)}
              placeholder={'{\n  "product_name": "Acme",\n  "accent_color": "#3b82f6"\n}'}
            />
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button
            variant="brand"
            disabled={busy || distributorId.length === 0 || json.trim().length === 0}
            onClick={handleSign}
          >
            <Download />
            {busy
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'distributor.bundle.submit' })}
          </Button>
        </DialogFooter>
      </DialogContent>
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
      prev.includes(field) ? prev.filter((f) => f !== field) : [...prev, field],
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
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {intl.formatMessage({ id: 'distributor.issue.title' }, { name: distributor.name })}
          </DialogTitle>
        </DialogHeader>
        {blob ? (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'distributor.issue.result.intro' })}
            </p>
            <Textarea
              readOnly
              value={blob}
              rows={5}
              className="font-mono text-xs"
              onFocus={(e) => e.currentTarget.select()}
            />
            <Button variant="outline" onClick={handleCopy}>
              {copied ? (
                <>
                  <Check className="text-success" />
                  {intl.formatMessage({ id: 'distributor.issue.copied' })}
                </>
              ) : (
                <>
                  <Copy />
                  {intl.formatMessage({ id: 'distributor.issue.copy' })}
                </>
              )}
            </Button>
            <div className="flex items-start gap-2 rounded-lg border border-info/30 bg-info/10 px-3 py-2 text-xs">
              <Info className="mt-0.5 h-4 w-4 shrink-0" />
              <p>{intl.formatMessage({ id: 'distributor.issue.result.guide' })}</p>
            </div>
            <DialogFooter>
              <Button variant="brand" onClick={onClose}>
                {intl.formatMessage({ id: 'common.done' })}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <div className="space-y-4">
            <DialogField
              label={intl.formatMessage({ id: 'distributor.field.fingerprint' })}
              help={intl.formatMessage({ id: 'distributor.field.fingerprint.hint' })}
              htmlFor="issue-fp"
            >
              <Input
                id="issue-fp"
                value={fingerprint}
                onChange={(e) => setFingerprint(e.target.value)}
                placeholder="a1b2c3…"
                autoFocus
              />
            </DialogField>
            <DialogField
              label={intl.formatMessage({ id: 'distributor.field.expiresDays' })}
              htmlFor="issue-days"
            >
              <Input
                id="issue-days"
                type="number"
                min={1}
                value={expiresDays}
                onChange={(e) =>
                  setExpiresDays(Math.max(1, Number(e.target.value) || DEFAULT_EXPIRES_DAYS))
                }
              />
            </DialogField>
            <DialogField
              label={intl.formatMessage({ id: 'distributor.field.note' })}
              htmlFor="issue-note"
            >
              <Input id="issue-note" value={note} onChange={(e) => setNote(e.target.value)} />
            </DialogField>

            {/* WP8: branding edit-scope for the issued customer license. */}
            <div className="space-y-2 rounded-lg border border-surface-border bg-muted/40 p-3">
              <div className="flex items-start gap-2">
                <Checkbox
                  className="mt-0.5"
                  checked={restrictBranding}
                  onCheckedChange={(c) => setRestrictBranding(c === true)}
                />
                <span className="text-sm text-foreground">
                  <span className="font-medium">
                    {intl.formatMessage({ id: 'distributor.issue.branding.restrict' })}
                  </span>
                  <span className="mt-0.5 block text-xs font-normal text-muted-foreground">
                    {intl.formatMessage({ id: 'distributor.issue.branding.hint' })}
                  </span>
                </span>
              </div>
              {restrictBranding && (
                <div className="grid grid-cols-1 gap-1.5 border-t border-surface-border pt-2 sm:grid-cols-2">
                  {BRANDING_FIELDS.map(({ field, labelId }) => (
                    <label
                      key={field}
                      className="flex items-center gap-2 text-sm text-muted-foreground"
                    >
                      <Checkbox
                        checked={brandingFields.includes(field)}
                        onCheckedChange={() => toggleField(field)}
                      />
                      {intl.formatMessage({ id: labelId })}
                    </label>
                  ))}
                </div>
              )}
            </div>

            <DialogFooter>
              <Button variant="outline" onClick={onClose}>
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
              <Button
                variant="brand"
                disabled={busy || fingerprint.trim().length === 0}
                onClick={handleIssue}
              >
                {busy
                  ? intl.formatMessage({ id: 'common.saving' })
                  : intl.formatMessage({ id: 'distributor.issue.submit' })}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
