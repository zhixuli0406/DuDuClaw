import { Component, type ErrorInfo, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { ErrorState } from '@/components/mds';

/**
 * L2 round 4: a render error inside one routed page (e.g. an unknown status
 * reaching an icon map) used to unmount the WHOLE app — no boundary existed.
 * This catches it at the layout level so the sidebar and navigation stay
 * usable. Navigating away recovers: `MainLayout` passes the pathname as
 * `resetKey` (and also re-keys the page wrapper on route change).
 */
export class RouteErrorBoundary extends Component<{ children: ReactNode; resetKey?: string }, { error: unknown }> {
  state: { error: unknown } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error };
  }

  // Explicit recovery on navigation (the layout passes the pathname), so the
  // boundary does not rely on its parent happening to re-key it.
  componentDidUpdate(prev: { resetKey?: string }) {
    if (this.state.error != null && prev.resetKey !== this.props.resetKey) this.setState({ error: null });
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error('[RouteErrorBoundary]', error, info.componentStack);
  }

  render() {
    if (this.state.error != null) return <RouteErrorFallback error={this.state.error} />;
    return this.props.children;
  }
}

function RouteErrorFallback({ error }: { error: unknown }) {
  const intl = useIntl();
  return (
    <ErrorState
      title={intl.formatMessage({ id: 'errorBoundary.title' })}
      description={intl.formatMessage({ id: 'errorBoundary.description' })}
      error={error}
      onRetry={() => window.location.reload()}
      retryLabel={intl.formatMessage({ id: 'errorBoundary.reload' })}
    />
  );
}
