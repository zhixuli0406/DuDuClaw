import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type AccountInfo, type BudgetSummary } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { Page, PageHeader, Card, StatCard, Button, Badge, EmptyState } from '@/components/ui';
import {
  Wallet,
  Plus,
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

export function AccountsPage() {
  const intl = useIntl();
  const [budget, setBudget] = useState<BudgetSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [showAddDialog, setShowAddDialog] = useState(false);

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
  const usagePercent =
    totalBudget > 0 ? Math.min(100, (totalSpent / totalBudget) * 100) : 0;

  return (
    <Page>
      <PageHeader
        icon={Wallet}
        title={intl.formatMessage({ id: 'nav.accounts' })}
        subtitle={intl.formatMessage({ id: 'accounts.title' })}
        actions={
          <>
            <Button variant="secondary" icon={RefreshCw} onClick={handleRotate}>
              {intl.formatMessage({ id: 'accounts.rotate' })}
            </Button>
            <Button variant="primary" icon={Plus} onClick={() => setShowAddDialog(true)}>
              {intl.formatMessage({ id: 'accounts.add' })}
            </Button>
          </>
        }
      />

      {/* Budget Summary KPIs */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <StatCard
          icon={TrendingUp}
          tone="warning"
          label={intl.formatMessage({ id: 'accounts.budget.used' })}
          value={`$${(totalSpent / 100).toFixed(2)}`}
        />
        <StatCard
          icon={PiggyBank}
          tone="success"
          label={intl.formatMessage({ id: 'accounts.budget.remaining' })}
          value={`$${((totalBudget - totalSpent) / 100).toFixed(2)}`}
        />
        <StatCard
          icon={Wallet}
          tone="accent"
          label={intl.formatMessage({ id: 'accounts.budget.total' })}
          value={`$${(totalBudget / 100).toFixed(2)}`}
        />
      </div>

      {/* Budget Summary progress */}
      <Card title={intl.formatMessage({ id: 'accounts.budget.total' })}>
        <div className="h-3 overflow-hidden rounded-full bg-stone-500/15 dark:bg-white/10">
          <div
            className={cn(
              'h-full rounded-full transition-all',
              usagePercent > 80
                ? 'bg-rose-500'
                : usagePercent > 60
                  ? 'bg-amber-500'
                  : 'bg-emerald-500'
            )}
            style={{ width: `${usagePercent}%` }}
          />
        </div>
        <p className="mt-2 flex justify-between text-xs text-stone-500 dark:text-stone-400">
          <span className="tabular-nums">
            ${(totalSpent / 100).toFixed(2)} / ${(totalBudget / 100).toFixed(2)}
          </span>
          <span className="tabular-nums">{usagePercent.toFixed(0)}%</span>
        </p>
      </Card>

      {/* Accounts List */}
      {!loading && budget?.accounts && budget.accounts.length > 0 ? (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {budget.accounts.map((account) => (
            <AccountCard key={account.id} account={account} intl={intl} onBudgetUpdated={fetchBudget} />
          ))}
        </div>
      ) : !loading ? (
        <Card>
          <EmptyState
            icon={Wallet}
            title={intl.formatMessage({ id: 'common.noData' })}
          />
        </Card>
      ) : null}

      {/* Add Account Dialog */}
      <AddAccountDialog
        open={showAddDialog}
        onClose={() => setShowAddDialog(false)}
        onCreated={fetchBudget}
      />
    </Page>
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

  return (
    <Dialog open={open} onClose={onClose} title={intl.formatMessage({ id: 'accounts.add' })}>
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'accounts.provider.name' })}>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={intl.formatMessage({ id: 'accounts.name.placeholder' })}
            className={inputClass}
          />
        </FormField>

        <FormField label={intl.formatMessage({ id: 'accounts.provider.authMethod' })}>
          <select
            value={accountType}
            onChange={(e) => setAccountType(e.target.value)}
            className={selectClass}
          >
            <option value="api_key">API Key</option>
            <option value="oauth">OAuth Token</option>
          </select>
        </FormField>

        <FormField label={accountType === 'api_key' ? 'API Key' : 'OAuth Token'}>
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder={accountType === 'api_key' ? 'sk-ant-...' : 'oauth-token-...'}
            className={inputClass}
          />
        </FormField>

        <div className="grid grid-cols-2 gap-4">
          <FormField label={intl.formatMessage({ id: 'accounts.provider.budget' })}>
            <input
              type="number"
              value={budget}
              onChange={(e) => setBudget(e.target.value)}
              min="1"
              className={inputClass}
            />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'accounts.provider.priority' })} hint={intl.formatMessage({ id: 'accounts.provider.priorityHint' })}>
            <input
              type="number"
              value={priority}
              onChange={(e) => setPriority(e.target.value)}
              min="1"
              max="10"
              className={inputClass}
            />
          </FormField>
        </div>

        {error && (
          <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>
        )}

        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button onClick={handleSubmit} disabled={submitting || !name.trim()} className={buttonPrimary}>
            {submitting ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'accounts.add' })}
          </button>
        </div>
      </div>
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
    <div className="panel p-5">
      <div className="flex items-start justify-between">
        <div className="flex items-center gap-3">
          <div className="rounded-lg bg-stone-500/10 p-2 dark:bg-white/5">
            {(account.auth_method ?? 'unknown') === 'apikey' ? (
              <Key className="h-4 w-4 text-stone-600 dark:text-stone-400" />
            ) : (
              <KeyRound className="h-4 w-4 text-stone-600 dark:text-stone-400" />
            )}
          </div>
          <div>
            <h3 className="font-semibold text-stone-900 dark:text-stone-50">{account.id}</h3>
            <p className="text-xs capitalize text-stone-500 dark:text-stone-400">
              {(account.auth_method ?? account.account_type ?? 'unknown').replace('_', ' ')}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            icon={Settings2}
            onClick={() => setShowDetails(true)}
            title={intl.formatMessage({ id: 'accounts.edit' })}
            aria-label={intl.formatMessage({ id: 'accounts.edit' })}
          />
          {account.is_healthy ? (
            <Badge tone="success" dot>
              <CheckCircle className="h-3.5 w-3.5" />
            </Badge>
          ) : (
            <Badge tone="warning" dot>
              <AlertTriangle className="h-3.5 w-3.5" />
            </Badge>
          )}
        </div>
      </div>

      <EditAccountDialog
        account={account}
        open={showDetails}
        onClose={() => setShowDetails(false)}
        onSaved={() => { setShowDetails(false); onBudgetUpdated(); }}
      />

      <div className="mt-3 flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
        <span>
          {intl.formatMessage({ id: 'accounts.priority' })}: <strong className="tabular-nums">{account.priority}</strong>
        </span>
      </div>

      <div className="mt-4">
        <div className="mb-1 flex items-center justify-between text-xs text-stone-500 dark:text-stone-400">
          <span>{intl.formatMessage({ id: 'accounts.budget.used' })}</span>
          {editing ? (
            <div className="flex items-center gap-1">
              <span className="tabular-nums">${(account.spent_this_month / 100).toFixed(2)} / $</span>
              <input
                type="number"
                min="1"
                value={budgetInput}
                onChange={(e) => setBudgetInput(e.target.value)}
                className="w-20 rounded-lg border border-amber-400 bg-[var(--panel-fill)] px-1.5 py-0.5 text-xs tabular-nums text-stone-900 focus:outline-none dark:border-amber-600 dark:text-stone-50"
                autoFocus
              />
              <Button size="sm" variant="primary" onClick={handleSaveBudget} disabled={saving}>
                {intl.formatMessage({ id: 'common.save' })}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setEditing(false)}>
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
            </div>
          ) : (
            <button
              onClick={() => { setBudgetInput(String(account.monthly_budget_cents / 100)); setEditing(true); }}
              className="flex items-center gap-1 hover:text-amber-600 dark:hover:text-amber-400"
            >
              <span className="tabular-nums">
                ${(account.spent_this_month / 100).toFixed(2)} / $
                {(account.monthly_budget_cents / 100).toFixed(2)}
              </span>
              <Pencil className="h-3 w-3" />
            </button>
          )}
        </div>
        <div className="h-1.5 overflow-hidden rounded-full bg-stone-500/15 dark:bg-white/10">
          <div
            className={cn(
              'h-full rounded-full transition-all',
              spentPercent > 80 ? 'bg-rose-500' : 'bg-amber-500'
            )}
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
    <Dialog open={open} onClose={onClose} title={`${intl.formatMessage({ id: 'accounts.edit' })} — ${account.id}`}>
      <div className="space-y-4">
        <div className="grid grid-cols-2 gap-3">
          <FormField label={intl.formatMessage({ id: 'accounts.provider.priority' })}>
            <input type="number" min={1} max={100} value={priority} onChange={(e) => setPriority(e.target.value)} className={inputClass} />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'accounts.provider.budget' })}>
            <input type="number" min={0} value={budget} onChange={(e) => setBudget(e.target.value)} className={inputClass} />
          </FormField>
        </div>
        <FormField label={intl.formatMessage({ id: 'accounts.field.label' })}>
          <input type="text" value={label} onChange={(e) => setLabel(e.target.value)} className={inputClass} />
        </FormField>
        <div className="grid grid-cols-2 gap-3">
          <FormField label={intl.formatMessage({ id: 'accounts.field.email' })}>
            <input type="email" value={email} onChange={(e) => setEmail(e.target.value)} className={inputClass} />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'accounts.field.subscription' })}>
            <input type="text" value={subscription} onChange={(e) => setSubscription(e.target.value)} placeholder="pro / max / team" className={inputClass} />
          </FormField>
        </div>
        <FormField label={intl.formatMessage({ id: 'accounts.field.profile' })} hint={intl.formatMessage({ id: 'accounts.field.profile.hint' })}>
          <input type="text" value={profile} onChange={(e) => setProfile(e.target.value)} className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'accounts.field.tags' })}>
          <ChipEditor values={tags} onChange={setTags} placeholder="prod" addLabel={intl.formatMessage({ id: 'common.add' })} />
        </FormField>
        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button onClick={handleSubmit} disabled={saving} className={buttonPrimary}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
