import { useEffect, useMemo, useState, type CSSProperties } from 'react';
import { createPortal } from 'react-dom';
import { useIntl } from 'react-intl';
import { useNavigate, useLocation } from 'react-router';
import { useTourStore } from '@/stores/tour-store';
import { useAuthStore } from '@/stores/auth-store';
import { visibleTourSteps } from './tour-steps';
import { Button } from '@/components/ui';
import { X } from 'lucide-react';

const SPOT_PAD = 6;
const POP_W = 320;

/**
 * GuidedTour — a lightweight spotlight walkthrough (no external deps). For each
 * step it navigates to the step's route, finds the target element, dims the
 * page with a box-shadow "hole" over it, and shows a popover with Back / Next /
 * Skip. Escapable any time; if a target can't be located it falls back to a
 * centered card so the tour always advances. Mounted once in MainLayout.
 */
export function GuidedTour() {
  const intl = useIntl();
  const navigate = useNavigate();
  const location = useLocation();
  const role = useAuthStore((s) => s.user?.role);

  const status = useTourStore((s) => s.status);
  const stepIndex = useTourStore((s) => s.stepIndex);
  const next = useTourStore((s) => s.next);
  const back = useTourStore((s) => s.back);
  const skip = useTourStore((s) => s.skip);
  const finish = useTourStore((s) => s.finish);

  const steps = useMemo(() => visibleTourSteps(role), [role]);
  const current = steps[stepIndex];
  const [rect, setRect] = useState<DOMRect | null>(null);

  const running = status === 'running' && !!current;

  // Past the last step ⇒ complete.
  useEffect(() => {
    if (status === 'running' && stepIndex >= steps.length) finish();
  }, [status, stepIndex, steps.length, finish]);

  // Drive each step: ensure correct route, then locate + spotlight the target.
  useEffect(() => {
    if (!running) return;
    if (location.pathname !== current.route) {
      navigate(current.route);
      return; // rerun once the route settles
    }
    let cancelled = false;
    let timer = 0;
    let tries = 0;
    setRect(null);
    const tick = () => {
      if (cancelled) return;
      const el = current.target ? document.querySelector(current.target) : null;
      if (el) {
        el.scrollIntoView({ block: 'center', behavior: 'smooth' });
        setRect(el.getBoundingClientRect());
      } else if (tries < 30) {
        tries += 1;
        timer = window.setTimeout(tick, 50);
      } else {
        setRect(null); // centered fallback
      }
    };
    timer = window.setTimeout(tick, 0);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [running, current, location.pathname, navigate]);

  // Keep the spotlight glued to the target on scroll / resize.
  useEffect(() => {
    if (!running || !current?.target) return;
    const reposition = () => {
      const el = document.querySelector(current.target!);
      if (el) setRect(el.getBoundingClientRect());
    };
    window.addEventListener('resize', reposition);
    window.addEventListener('scroll', reposition, true);
    return () => {
      window.removeEventListener('resize', reposition);
      window.removeEventListener('scroll', reposition, true);
    };
  }, [running, current]);

  // Esc skips the whole tour.
  useEffect(() => {
    if (!running) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') skip();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [running, skip]);

  if (!running) return null;

  const isLast = stepIndex >= steps.length - 1;
  const spot = rect
    ? {
        top: rect.top - SPOT_PAD,
        left: rect.left - SPOT_PAD,
        width: rect.width + SPOT_PAD * 2,
        height: rect.height + SPOT_PAD * 2,
      }
    : null;

  const popStyle: CSSProperties = rect
    ? {
        position: 'fixed',
        top: Math.min(Math.max(16, rect.top), window.innerHeight - 240),
        left: Math.min(rect.right + 16, window.innerWidth - POP_W - 16),
        width: POP_W,
      }
    : {
        position: 'fixed',
        top: '50%',
        left: '50%',
        width: POP_W,
        transform: 'translate(-50%, -50%)',
      };

  return createPortal(
    <div
      className="fixed inset-0 z-[100]"
      role="dialog"
      aria-modal="true"
      aria-label={intl.formatMessage({ id: current.titleKey })}
    >
      {/* Click-blocker (transparent — the spotlight shadow provides the dim). */}
      <div className="fixed inset-0" aria-hidden="true" />

      {spot ? (
        <div
          className="pointer-events-none fixed rounded-lg transition-all duration-200 motion-reduce:transition-none"
          style={{ ...spot, boxShadow: '0 0 0 9999px rgba(0,0,0,0.55)' }}
          aria-hidden="true"
        />
      ) : (
        <div className="fixed inset-0 bg-black/55" aria-hidden="true" />
      )}

      {/* Popover */}
      <div className="panel z-[101] space-y-3 p-4 shadow-xl" style={popStyle}>
        <div className="flex items-start justify-between gap-2">
          <span className="text-[11px] font-medium uppercase tracking-wider text-amber-600 dark:text-amber-400">
            {intl.formatMessage(
              { id: 'tour.stepOf' },
              { current: stepIndex + 1, total: steps.length },
            )}
          </span>
          <button
            onClick={skip}
            className="rounded p-1 text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
            aria-label={intl.formatMessage({ id: 'tour.skip' })}
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="space-y-1">
          <h3 className="text-sm font-semibold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: current.titleKey })}
          </h3>
          <p className="text-xs leading-relaxed text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: current.bodyKey })}
          </p>
        </div>

        <div className="flex items-center justify-between pt-1">
          <button
            onClick={skip}
            className="text-xs text-stone-400 transition-colors hover:text-stone-600 dark:hover:text-stone-300"
          >
            {intl.formatMessage({ id: 'tour.skip' })}
          </button>
          <div className="flex items-center gap-2">
            {stepIndex > 0 && (
              <Button variant="secondary" size="sm" onClick={back}>
                {intl.formatMessage({ id: 'tour.back' })}
              </Button>
            )}
            <Button variant="primary" size="sm" onClick={() => (isLast ? finish() : next())}>
              {intl.formatMessage({ id: isLast ? 'tour.finish' : 'tour.next' })}
            </Button>
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
