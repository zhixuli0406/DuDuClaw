import { Component, useContext, type ErrorInfo, type ReactNode } from 'react';
import { IntlContext } from 'react-intl';

/**
 * L2 round 5: safety net above the router and layout. `RouteErrorBoundary`
 * only covers the routed page; this one catches a throw in the layout chrome
 * (sidebar, CommandPalette, AssignSheet …), `ToastProvider` or the router.
 *
 * Coverage limit: it is mounted INSIDE `IntlProvider` in `main.tsx` (so the
 * fallback can be translated), which means a crash in `Root` itself or in
 * `IntlProvider` is NOT caught here. Its fallback depends on nothing it might
 * share a crash with — no layout, no stores, no design-system components —
 * and still renders plain strings if no intl context is reachable.
 */
export class AppErrorBoundary extends Component<{ children: ReactNode }, { error: unknown }> {
  state: { error: unknown } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error('[AppErrorBoundary]', error, info.componentStack);
  }

  render() {
    if (this.state.error != null) return <AppErrorFallback />;
    return this.props.children;
  }
}

// Used only when no IntlProvider is reachable (e.g. it is what crashed).
const PLAIN = {
  title: "This page couldn't be shown",
  description: 'Something went wrong. Reload to try again.',
  reload: 'Reload',
};

function AppErrorFallback() {
  const intl = useContext(IntlContext);
  const text = (id: string, plain: string) => {
    if (!intl) return plain;
    try {
      return intl.formatMessage({ id, defaultMessage: plain });
    } catch {
      return plain;
    }
  };
  return (
    <div role="alert" className="flex min-h-screen flex-col items-center justify-center gap-3 bg-background p-6 text-center text-foreground">
      <p className="text-lg font-semibold">{text('errorBoundary.title', PLAIN.title)}</p>
      <p className="max-w-md text-sm text-muted-foreground">{text('errorBoundary.appDescription', PLAIN.description)}</p>
      <button
        type="button"
        onClick={() => window.location.reload()}
        className="rounded-md border border-surface-border px-3 py-1.5 text-sm font-medium hover:bg-muted"
      >
        {text('errorBoundary.reload', PLAIN.reload)}
      </button>
    </div>
  );
}
