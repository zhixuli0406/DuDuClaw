import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  Button,
} from '@/components/mds';
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
 * Pure gate: does yesterday's report contain any actual activity? A fresh
 * install (or a fully idle day) settles to all-zero — popping a zero card on
 * first open reads as a bug ("brand-new system has no yesterday"). Exported
 * for unit test.
 */
export function reportHasActivity(r: DailyReport): boolean {
  return (
    r.tasks_completed > 0 ||
    r.cost_cents > 0 ||
    r.xp_gained > 0 ||
    r.new_knowledge_pages > 0 ||
    r.most_active_agent != null
  );
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
        if (!reportHasActivity(r)) {
          // Idle/empty yesterday — stay quiet, and still burn the marker so we
          // don't re-fetch on every reload (yesterday's numbers can't change).
          try {
            localStorage.setItem(LAST_SHOWN_KEY, today);
          } catch {
            /* ignore */
          }
          return;
        }
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

  if (!report) return null;

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'growth.report.title' })}</DialogTitle>
          <DialogDescription>
            {intl.formatMessage({ id: 'growth.report.subtitle' }, { date: report.date })}
          </DialogDescription>
        </DialogHeader>
        <DailyReportContent report={report} />
        <DialogFooter>
          <DialogClose
            render={
              <Button variant="outline">
                {intl.formatMessage({ id: 'growth.report.dismiss' })}
              </Button>
            }
          />
          <Button
            variant="brand"
            onClick={() => {
              setOpen(false);
              navigate('/growth');
            }}
          >
            {intl.formatMessage({ id: 'growth.report.viewDetails' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
