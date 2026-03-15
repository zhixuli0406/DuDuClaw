import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type AccountInfo, type BudgetSummary } from '@/lib/api';
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

  const fetchBudget = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.accounts.budgetSummary();
      setBudget(result);
    } catch {
      // error handled silently
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
      // error handled silently
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
          <button className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600">
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
    </div>
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
      ? Math.min(
          100,
          (account.spent_this_month / account.monthly_budget_cents) * 100
        )
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
            <h3 className="font-semibold text-stone-900 dark:text-stone-50">
              {account.id}
            </h3>
            <p className="text-xs capitalize text-stone-500 dark:text-stone-400">
              {account.account_type.replace('_', ' ')}
            </p>
          </div>
        </div>

        {/* Health indicator */}
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

      {/* Spend bar */}
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
