import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Dialog, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { useConnectionStore } from '@/stores/connection-store';
import { growthApi, type DailyReport } from '@/lib/api-growth';
import { DailyReportContent } from './DailyReportContent';

const LAST_SHOWN_KEY = 'duduclaw:growth:last-report-shown';

/** Local calendar day as `YYYY-MM-DD` — the per-day marker for the popup gate. */
export function localDayKey(now: Date = new Date()): string {
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, '0');
  const d = String(now.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

/**
 * Pure gate: should the settlement dialog pop today? True only when we have not
 * already shown it for `today`. Exported for unit test (T10 "當日只彈一次").
 */
export function shouldShowDailyReport(lastShown: string | null, today: string): boolean {
  return lastShown !== today;
}

/**
 * DailyReportCard — the once-per-day settlement popup (T10.3). On the first
 * dashboard open of each local day it fetches *yesterday's* report and shows it
 * in a dialog; the "shown" marker is persisted so reloads the same day stay
 * quiet. Mounted once in `MainLayout`. If the report fetch fails we simply don't
 * pop (an honest silent skip), and we don't burn the day's marker so a later
 * open can still surface it.
 */
export function DailyReportCard() {
  const intl = useIntl();
  const navigate = useNavigate();
  const authed = useConnectionStore((s) => s.state === 'authenticated');
  const [open, setOpen] = useState(false);
  const [report, setReport] = useState<DailyReport | null>(null);

  useEffect(() => {
    if (!authed) return;
    const today = localDayKey();
    let last: string | null = null;
    try {
      last = localStorage.getItem(LAST_SHOWN_KEY);
    } catch {
      /* private mode / quota — treat as never shown */
    }
    if (!shouldShowDailyReport(last, today)) return;

    let alive = true;
    growthApi
      .dailyReport()
      .then((r) => {
        if (!alive) return;
        setReport(r);
        setOpen(true);
        // Burn the marker only on a successful show, so a transient RPC failure
        // doesn't eat the day's report.
        try {
          localStorage.setItem(LAST_SHOWN_KEY, today);
        } catch {
          /* ignore */
        }
      })
      .catch(() => {
        /* silent — no report is honest; retry on the next fresh load */
      });
    return () => {
      alive = false;
    };
  }, [authed]);

  return (
    <Dialog
      open={open}
      onClose={() => setOpen(false)}
      title={intl.formatMessage({ id: 'growth.report.title' })}
    >
      {report && (
        <div className="space-y-5">
          <p className="text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'growth.report.subtitle' }, { date: report.date })}
          </p>
          <DailyReportContent report={report} />
          <div className="flex justify-end gap-2">
            <button type="button" className={buttonSecondary} onClick={() => setOpen(false)}>
              {intl.formatMessage({ id: 'growth.report.dismiss' })}
            </button>
            <button
              type="button"
              className={buttonPrimary}
              onClick={() => {
                setOpen(false);
                navigate('/growth');
              }}
            >
              {intl.formatMessage({ id: 'growth.report.viewDetails' })}
            </button>
          </div>
        </div>
      )}
    </Dialog>
  );
}
