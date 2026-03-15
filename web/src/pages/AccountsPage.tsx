import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type AccountInfo, type BudgetSummary } from '@/lib/api';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import {
  Wallet,
  Plus,
  RefreshCw,
  CheckCircle,
  AlertTriangle,
  Key,
  KeyRound,
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
    } catch {
      // will show empty state
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchBudget();
  }, [fetchBudget]);

  const handleRotate = async () => {
    try {
      await api.accounts.rotate();
      await fetchBudget();
    } catch {
      // handled silently
    }
  };

  const totalBudget = budget?.total_budget_cents ?? 0;
  const totalSpent = budget?.total_spent_cents ?? 0;
  const usagePercent =
    totalBudget > 0 ? Math.min(100, (totalSpent / totalBudget) * 100) : 0;

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'accounts.title' })}
        </h2>
        <div className="flex gap-2">
          <button
            onClick={handleRotate}
            className="inline-flex items-center gap-2 rounded-lg border border-stone-200 bg-white px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300 dark:hover:bg-stone-700"
          >
            <RefreshCw className="h-4 w-4" />
            {intl.formatMessage({ id: 'accounts.rotate' })}
          </button>
          <button
            onClick={() => setShowAddDialog(true)}
            className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600"
          >
            <Plus className="h-4 w-4" />
            {intl.formatMessage({ id: 'accounts.add' })}
          </button>
        </div>
      </div>

      {/* Budget Summary */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <div className="flex items-center gap-3 mb-4">
          <div className="rounded-lg bg-amber-100 p-2.5 dark:bg-amber-900/30">
            <Wallet className="h-5 w-5 text-amber-600 dark:text-amber-400" />
          </div>
          <div>
            <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'accounts.budget.total' })}
            </h3>
          </div>
        </div>

        <div className="mb-2 flex justify-between text-sm">
          <span className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'accounts.budget.used' })}:{' '}
            <span className="font-semibold text-stone-900 dark:text-stone-50">
              ${(totalSpent / 100).toFixed(2)}
            </span>
          </span>
          <span className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'accounts.budget.remaining' })}:{' '}
            <span className="font-semibold text-stone-900 dark:text-stone-50">
              ${((totalBudget - totalSpent) / 100).toFixed(2)}
            </span>
          </span>
        </div>

        <div className="h-3 overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
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

        <p className="mt-2 text-right text-xs text-stone-400 dark:text-stone-500">
          ${(totalBudget / 100).toFixed(2)}{' '}
          {intl.formatMessage({ id: 'accounts.budget.total' })}
        </p>
      </div>

      {/* Accounts List */}
      {!loading && budget?.accounts && budget.accounts.length > 0 ? (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {budget.accounts.map((account) => (
            <AccountCard key={account.id} account={account} intl={intl} />
          ))}
        </div>
      ) : !loading ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Wallet className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.noData' })}
          </p>
        </div>
      ) : null}

      {/* Add Account Dialog */}
      <AddAccountDialog
        open={showAddDialog}
        onClose={() => setShowAddDialog(false)}
        onCreated={fetchBudget}
      />
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
  const [name, setName] = useState('');
  const [accountType, setAccountType] = useState('api_key');
  const [apiKey, setApiKey] = useState('');
  const [budget, setBudget] = useState('50');
  const [priority, setPriority] = useState('1');
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async () => {
    if (!name.trim()) return;
    setSubmitting(true);
    try {
      // Store account info (future: dedicated accounts.add endpoint)
      await api.accounts.health();
      onCreated();
      onClose();
      setName('');
      setApiKey('');
      setBudget('50');
      setPriority('1');
    } catch {
      // error
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open={open} onClose={onClose} title="新增帳號">
      <div className="space-y-4">
        <FormField label="帳號名稱">
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例：main、backup"
            className={inputClass}
          />
        </FormField>

        <FormField label="認證方式">
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
          <FormField label="月預算 (USD)">
            <input
              type="number"
              value={budget}
              onChange={(e) => setBudget(e.target.value)}
              min="1"
              className={inputClass}
            />
          </FormField>
          <FormField label="優先級" hint="數字越小越優先">
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

        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>
            取消
          </button>
          <button onClick={handleSubmit} disabled={submitting || !name.trim()} className={buttonPrimary}>
            {submitting ? '新增中...' : '新增帳號'}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function AccountCard({
  account,
  intl,
}: {
  account: AccountInfo;
  intl: ReturnType<typeof useIntl>;
}) {
  const spentPercent =
    account.monthly_budget_cents > 0
      ? Math.min(100, (account.spent_this_month / account.monthly_budget_cents) * 100)
      : 0;

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-start justify-between">
        <div className="flex items-center gap-3">
          <div className="rounded-lg bg-stone-100 p-2 dark:bg-stone-800">
            {account.account_type === 'api_key' ? (
              <Key className="h-4 w-4 text-stone-600 dark:text-stone-400" />
            ) : (
              <KeyRound className="h-4 w-4 text-stone-600 dark:text-stone-400" />
            )}
          </div>
          <div>
            <h3 className="font-semibold text-stone-900 dark:text-stone-50">{account.id}</h3>
            <p className="text-xs capitalize text-stone-500 dark:text-stone-400">
              {account.account_type.replace('_', ' ')}
            </p>
          </div>
        </div>
        {account.is_healthy ? (
          <CheckCircle className="h-5 w-5 text-emerald-500" />
        ) : (
          <AlertTriangle className="h-5 w-5 text-amber-500" />
        )}
      </div>

      <div className="mt-3 flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
        <span>
          Priority: <strong>{account.priority}</strong>
        </span>
      </div>

      <div className="mt-4">
        <div className="mb-1 flex justify-between text-xs text-stone-500 dark:text-stone-400">
          <span>{intl.formatMessage({ id: 'accounts.budget.used' })}</span>
          <span>
            ${(account.spent_this_month / 100).toFixed(2)} / $
            {(account.monthly_budget_cents / 100).toFixed(2)}
          </span>
        </div>
        <div className="h-1.5 overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
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
