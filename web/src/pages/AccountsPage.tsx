import { useEffect, useState, useCallback, type ComponentType, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type AccountInfo, type BudgetSummary } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { ChipEditor } from '@/components/shared/ChipEditor';
import {
  Button,
  Badge,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Empty,
  Input,
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
  DialogClose,
} from '@/components/mds';
import { CliLoginModal, type LoginRuntime } from '@/components/CliLoginModal';
import {
  Wallet,
  Plus,
  LogIn,
  RefreshCw,
  CheckCircle,
  AlertTriangle,
  Key,
  KeyRound,
  Pencil,
  Settings2,
  TrendingUp,
  PiggyBank,
} from 'lucide-react';

/** KPI tile (spec §5.5): tinted icon + label + big value. */
function StatTile({
  icon: Icon,
  tone,
  label,
  value,
}: {
  icon: ComponentType<{ className?: string }>;
  tone: 'brand' | 'success' | 'warning';
  label: string;
  value: string;
}) {
  const toneClass = tone === 'success' ? 'text-success' : tone === 'warning' ? 'text-warning' : 'text-brand';
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

/** Local labeled-field wrapper (spec §4 form pattern). */
function Field({
  label,
  hint,
  children,
  className,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('space-y-1.5', className)}>
      <label className="text-xs font-medium text-muted-foreground">{label}</label>
      {children}
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

const CLI_OPTIONS: ReadonlyArray<[LoginRuntime, string]> = [
  ['claude', 'Claude'],
  ['codex', 'Codex'],
  ['gemini', 'Gemini'],
  ['antigravity', 'Antigravity (agy)'],
  ['grok', 'Grok（SuperGrok 訂閱）'],
];

/**
 * AccountsPage — multi-account rotation surface (MDS, the accounts tab of
 * `/manage/billing` + legacy `/accounts`). Budget KPIs, a usage bar, and a grid
 * of per-account cards; add / edit / one-click-login all go through MDS dialogs.
 */
export function AccountsPage() {
  const intl = useIntl();
  const [budget, setBudget] = useState<BudgetSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [loginRuntime, setLoginRuntime] = useState<LoginRuntime | null>(null);

  const fetchBudget = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.accounts.budgetSummary();
      setBudget(result);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    fetchBudget();
  }, [fetchBudget]);

  const handleRotate = async () => {
    try {
      await api.accounts.rotate();
      await fetchBudget();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  const totalBudget = budget?.total_budget_cents ?? 0;
  const totalSpent = budget?.total_spent_cents ?? 0;
  const usagePercent = totalBudget > 0 ? Math.min(100, (totalSpent / totalBudget) * 100) : 0;

  return (
    <div className="space-y-6">
      {/* Slim tab header — description left, actions right (spec §5.2). */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'accounts.title' })}</p>
        <div className="flex flex-wrap items-center gap-2">
          <Button variant="outline" size="sm" onClick={handleRotate}>
            <RefreshCw />
            {intl.formatMessage({ id: 'accounts.rotate' })}
          </Button>
          <Button variant="outline" size="sm" onClick={() => setPickerOpen(true)}>
            <LogIn />
            一鍵登入
          </Button>
          <Button variant="brand" size="sm" onClick={() => setShowAddDialog(true)}>
            <Plus />
            {intl.formatMessage({ id: 'accounts.add' })}
          </Button>
        </div>
      </div>

      {/* CLI picker for one-click login */}
      <Dialog open={pickerOpen} onOpenChange={(o) => !o && setPickerOpen(false)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>選擇要登入的 CLI</DialogTitle>
          </DialogHeader>
          <div className="grid grid-cols-2 gap-2">
            {CLI_OPTIONS.map(([rt, label]) => (
              <Button
                key={rt}
                variant="outline"
                onClick={() => {
                  setPickerOpen(false);
                  setLoginRuntime(rt);
                }}
              >
                {label}
              </Button>
            ))}
          </div>
          <p className="text-xs text-muted-foreground">
            會在伺服器以 PTY 驅動該 CLI 的原生登入流程並串到這裡。Claude 走 setup-token（遠端可用）；其餘走 localhost 回呼（限自架）。
          </p>
        </DialogContent>
      </Dialog>

      {loginRuntime && (
        <CliLoginModal
          open={loginRuntime !== null}
          runtime={loginRuntime}
          onClose={() => setLoginRuntime(null)}
          onSuccess={fetchBudget}
        />
      )}

      {/* Budget Summary KPIs */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <StatTile
          icon={TrendingUp}
          tone="warning"
          label={intl.formatMessage({ id: 'accounts.budget.used' })}
          value={`$${(totalSpent / 100).toFixed(2)}`}
        />
        <StatTile
          icon={PiggyBank}
          tone="success"
          label={intl.formatMessage({ id: 'accounts.budget.remaining' })}
          value={`$${((totalBudget - totalSpent) / 100).toFixed(2)}`}
        />
        <StatTile
          icon={Wallet}
          tone="brand"
          label={intl.formatMessage({ id: 'accounts.budget.total' })}
          value={`$${(totalBudget / 100).toFixed(2)}`}
        />
      </div>

      {/* Budget Summary progress */}
      <Card>
        <CardHeader>
          <CardTitle>{intl.formatMessage({ id: 'accounts.budget.total' })}</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="h-3 overflow-hidden rounded-full bg-muted">
            <div
              className={cn(
                'h-full rounded-full transition-all',
                usagePercent > 80 ? 'bg-destructive' : usagePercent > 60 ? 'bg-warning' : 'bg-success'
              )}
              style={{ width: `${usagePercent}%` }}
            />
          </div>
          <p className="mt-2 flex justify-between text-xs text-muted-foreground">
            <span className="tabular-nums">
              ${(totalSpent / 100).toFixed(2)} / ${(totalBudget / 100).toFixed(2)}
            </span>
            <span className="tabular-nums">{usagePercent.toFixed(0)}%</span>
          </p>
        </CardContent>
      </Card>

      {/* Accounts List */}
      {!loading && budget?.accounts && budget.accounts.length > 0 ? (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {budget.accounts.map((account) => (
            <AccountCard key={account.id} account={account} intl={intl} onBudgetUpdated={fetchBudget} />
          ))}
        </div>
      ) : !loading ? (
        <Empty icon={Wallet} title={intl.formatMessage({ id: 'common.noData' })} variant="dashed" />
      ) : null}

      {/* Add Account Dialog */}
      <AddAccountDialog open={showAddDialog} onClose={() => setShowAddDialog(false)} onCreated={fetchBudget} />
    </div>
  );
}

function AddAccountDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const [name, setName] = useState('');
  const [accountType, setAccountType] = useState('api_key');
  const [apiKey, setApiKey] = useState('');
  const [budget, setBudget] = useState('50');
  const [priority, setPriority] = useState('1');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async () => {
    if (!name.trim()) return;
    if (!apiKey.trim()) {
      setError(intl.formatMessage({ id: 'accounts.provider.keyRequired' }));
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await api.accounts.add({
        id: name.trim(),
        type: accountType,
        key: apiKey.trim(),
        monthly_budget_cents: Math.round(Number(budget) * 100),
        priority: Number(priority),
      });
      onCreated();
      onClose();
      setName('');
      setApiKey('');
      setBudget('50');
      setPriority('1');
    } catch {
      setError(intl.formatMessage({ id: 'accounts.provider.addFailed' }));
    } finally {
      setSubmitting(false);
    }
  };

  const keyLabel = accountType === 'api_key' ? 'API Key' : 'OAuth Token';

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'accounts.add' })}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <Field label={intl.formatMessage({ id: 'accounts.provider.name' })}>
            <Input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={intl.formatMessage({ id: 'accounts.name.placeholder' })}
            />
          </Field>

          <Field label={intl.formatMessage({ id: 'accounts.provider.authMethod' })}>
            <Select value={accountType} onValueChange={(v) => setAccountType(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{accountType === 'api_key' ? 'API Key' : 'OAuth Token'}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="api_key">API Key</SelectItem>
                <SelectItem value="oauth">OAuth Token</SelectItem>
              </SelectContent>
            </Select>
          </Field>

          <Field label={keyLabel}>
            <Input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={accountType === 'api_key' ? 'sk-ant-...' : 'oauth-token-...'}
            />
          </Field>

          <div className="grid grid-cols-2 gap-4">
            <Field label={intl.formatMessage({ id: 'accounts.provider.budget' })}>
              <Input type="number" value={budget} onChange={(e) => setBudget(e.target.value)} min="1" />
            </Field>
            <Field
              label={intl.formatMessage({ id: 'accounts.provider.priority' })}
              hint={intl.formatMessage({ id: 'accounts.provider.priorityHint' })}
            >
              <Input type="number" value={priority} onChange={(e) => setPriority(e.target.value)} min="1" max="10" />
            </Field>
          </div>

          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>

        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="brand" onClick={handleSubmit} disabled={submitting || !name.trim()}>
            {submitting ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'accounts.add' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function AccountCard({
  account,
  intl,
  onBudgetUpdated,
}: {
  account: AccountInfo;
  intl: ReturnType<typeof useIntl>;
  onBudgetUpdated: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [showDetails, setShowDetails] = useState(false);
  const [budgetInput, setBudgetInput] = useState(String(account.monthly_budget_cents / 100));
  const [saving, setSaving] = useState(false);

  const spentPercent =
    account.monthly_budget_cents > 0
      ? Math.min(100, (account.spent_this_month / account.monthly_budget_cents) * 100)
      : 0;

  const handleSaveBudget = async () => {
    setSaving(true);
    try {
      await api.accounts.updateBudget(account.id, Math.round(Number(budgetInput) * 100));
      setEditing(false);
      onBudgetUpdated();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="rounded-xl border border-surface-border bg-surface p-5 shadow-[var(--surface-shadow)]">
      <div className="flex items-start justify-between">
        <div className="flex items-center gap-3">
          <div className="rounded-lg bg-muted p-2 text-muted-foreground">
            {(account.auth_method ?? 'unknown') === 'apikey' ? (
              <Key className="size-4" />
            ) : (
              <KeyRound className="size-4" />
            )}
          </div>
          <div>
            <h3 className="font-medium text-foreground">{account.id}</h3>
            <p className="text-xs capitalize text-muted-foreground">
              {(account.auth_method ?? account.account_type ?? 'unknown').replace('_', ' ')}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => setShowDetails(true)}
            title={intl.formatMessage({ id: 'accounts.edit' })}
            aria-label={intl.formatMessage({ id: 'accounts.edit' })}
          >
            <Settings2 />
          </Button>
          {account.is_healthy ? (
            <Badge variant="secondary" className="bg-success/15 text-success">
              <CheckCircle />
            </Badge>
          ) : (
            <Badge variant="secondary" className="bg-warning/15 text-warning">
              <AlertTriangle />
            </Badge>
          )}
        </div>
      </div>

      <EditAccountDialog
        account={account}
        open={showDetails}
        onClose={() => setShowDetails(false)}
        onSaved={() => {
          setShowDetails(false);
          onBudgetUpdated();
        }}
      />

      <div className="mt-3 flex items-center gap-2 text-xs text-muted-foreground">
        <span>
          {intl.formatMessage({ id: 'accounts.priority' })}:{' '}
          <strong className="tabular-nums text-foreground">{account.priority}</strong>
        </span>
      </div>

      <div className="mt-4">
        <div className="mb-1 flex items-center justify-between text-xs text-muted-foreground">
          <span>{intl.formatMessage({ id: 'accounts.budget.used' })}</span>
          {editing ? (
            <div className="flex items-center gap-1">
              <span className="tabular-nums">${(account.spent_this_month / 100).toFixed(2)} / $</span>
              <Input
                type="number"
                min="1"
                value={budgetInput}
                onChange={(e) => setBudgetInput(e.target.value)}
                className="h-7 w-20 tabular-nums"
                autoFocus
              />
              <Button size="sm" variant="brand" onClick={handleSaveBudget} disabled={saving}>
                {intl.formatMessage({ id: 'common.save' })}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setEditing(false)}>
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
            </div>
          ) : (
            <button
              onClick={() => {
                setBudgetInput(String(account.monthly_budget_cents / 100));
                setEditing(true);
              }}
              className="flex items-center gap-1 hover:text-brand"
            >
              <span className="tabular-nums">
                ${(account.spent_this_month / 100).toFixed(2)} / $
                {(account.monthly_budget_cents / 100).toFixed(2)}
              </span>
              <Pencil className="size-3" />
            </button>
          )}
        </div>
        <div className="h-1.5 overflow-hidden rounded-full bg-muted">
          <div
            className={cn('h-full rounded-full transition-all', spentPercent > 80 ? 'bg-destructive' : 'bg-warning')}
            style={{ width: `${spentPercent}%` }}
          />
        </div>
      </div>
    </div>
  );
}

// ── G.5 — full per-account edit (priority/tags/profile/email/subscription/label) ──

function EditAccountDialog({
  account,
  open,
  onClose,
  onSaved,
}: {
  account: AccountInfo;
  open: boolean;
  onClose: () => void;
  onSaved: () => void;
}) {
  const intl = useIntl();
  const [priority, setPriority] = useState(String(account.priority ?? 1));
  const [label, setLabel] = useState(account.label ?? '');
  const [email, setEmail] = useState(account.email ?? '');
  const [subscription, setSubscription] = useState(account.subscription ?? '');
  const [profile, setProfile] = useState('');
  const [tags, setTags] = useState<string[]>([]);
  const [budget, setBudget] = useState(String((account.monthly_budget_cents ?? 0) / 100));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Reset form whenever a different account opens.
  useEffect(() => {
    if (open) {
      setPriority(String(account.priority ?? 1));
      setLabel(account.label ?? '');
      setEmail(account.email ?? '');
      setSubscription(account.subscription ?? '');
      setProfile('');
      setTags([]);
      setBudget(String((account.monthly_budget_cents ?? 0) / 100));
      setError(null);
    }
  }, [open, account]);

  const handleSubmit = async () => {
    setSaving(true);
    setError(null);
    try {
      await api.accounts.update({
        account_id: account.id,
        priority: Number(priority),
        label,
        email,
        subscription,
        ...(profile.trim() !== '' ? { profile: profile.trim() } : {}),
        ...(tags.length > 0 ? { tags } : {}),
        monthly_budget_cents: Math.round(Number(budget) * 100),
      });
      onSaved();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {intl.formatMessage({ id: 'accounts.edit' })} — {account.id}
          </DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <div className="grid grid-cols-2 gap-3">
            <Field label={intl.formatMessage({ id: 'accounts.provider.priority' })}>
              <Input type="number" min={1} max={100} value={priority} onChange={(e) => setPriority(e.target.value)} />
            </Field>
            <Field label={intl.formatMessage({ id: 'accounts.provider.budget' })}>
              <Input type="number" min={0} value={budget} onChange={(e) => setBudget(e.target.value)} />
            </Field>
          </div>
          <Field label={intl.formatMessage({ id: 'accounts.field.label' })}>
            <Input type="text" value={label} onChange={(e) => setLabel(e.target.value)} />
          </Field>
          <div className="grid grid-cols-2 gap-3">
            <Field label={intl.formatMessage({ id: 'accounts.field.email' })}>
              <Input type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
            </Field>
            <Field label={intl.formatMessage({ id: 'accounts.field.subscription' })}>
              <Input
                type="text"
                value={subscription}
                onChange={(e) => setSubscription(e.target.value)}
                placeholder="pro / max / team"
              />
            </Field>
          </div>
          <Field
            label={intl.formatMessage({ id: 'accounts.field.profile' })}
            hint={intl.formatMessage({ id: 'accounts.field.profile.hint' })}
          >
            <Input type="text" value={profile} onChange={(e) => setProfile(e.target.value)} />
          </Field>
          <Field label={intl.formatMessage({ id: 'accounts.field.tags' })}>
            <ChipEditor values={tags} onChange={setTags} placeholder="prod" addLabel={intl.formatMessage({ id: 'common.add' })} />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="brand" onClick={handleSubmit} disabled={saving}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
