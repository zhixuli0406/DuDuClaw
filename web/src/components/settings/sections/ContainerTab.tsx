import { useCallback, useState } from 'react';
import { useIntl } from 'react-intl';
import { RefreshCw } from 'lucide-react';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Button, ErrorState, SettingsSection, SettingsCard } from '@/components/mds';
import { SettingRow } from './shared';

/**
 * 容器 settings tab.
 *
 * Until the 2026-08 audit this tab rendered three string literals — engine
 * "Docker", socket "/var/run/docker.sock", status "已偵測" with a green badge —
 * on a machine that may have neither Docker nor Podman installed. It was not a
 * missing error state; it was a permanently-successful screen unrelated to
 * reality.
 *
 * It now reports what the gateway actually probed. `system.doctor` runs
 * `docker info`, falls back to `podman info`, and returns a `container_runtime`
 * check — the only container probe the gateway exposes. Because that RPC also
 * spawns the MCP cold-start and model probes (~20s worst case), the probe is
 * user-initiated rather than automatic: before you press it the fields honestly
 * read 未偵測, never a fabricated success.
 *
 * What the probe does not report is the socket path, so that row says
 * "未回報" instead of guessing the platform default.
 */

type ProbeState =
  | { kind: 'idle' }
  | { kind: 'probing' }
  | { kind: 'done'; status: 'pass' | 'warn' | 'fail'; engine: string | null; message: string }
  | { kind: 'error'; error: unknown };

/**
 * The gateway phrases this check as "<engine> daemon is running" /
 * "<engine> found but daemon is not running" / "No container runtime
 * (docker/podman) found in PATH". Pull the engine name out when it named one;
 * never invent one.
 */
function engineFrom(message: string): string | null {
  const m = message.match(/\b(docker|podman)\b/i);
  return m ? m[1].toLowerCase() : null;
}

export function ContainerTab() {
  const intl = useIntl();
  const [state, setState] = useState<ProbeState>({ kind: 'idle' });

  const probe = useCallback(async () => {
    setState({ kind: 'probing' });
    try {
      const res = await api.system.doctor();
      const check = res?.checks?.find((c) => c.name === 'container_runtime');
      if (!check) {
        // An older gateway without this check: say we could not tell, rather
        // than reporting a green state we never received.
        setState({ kind: 'idle' });
        toast.error(intl.formatMessage({ id: 'errorState.manage.container.probeFailed' }));
        return;
      }
      setState({
        kind: 'done',
        status: check.status,
        engine: engineFrom(check.message),
        message: check.message,
      });
    } catch (e) {
      console.warn('[api]', e);
      setState({ kind: 'error', error: e });
      toast.error(
        intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }),
      );
    }
  }, [intl]);

  const notProbed = intl.formatMessage({ id: 'errorState.manage.container.notProbed' });

  const engineValue =
    state.kind === 'done' && state.engine
      ? state.engine === 'podman'
        ? 'Podman'
        : 'Docker'
      : notProbed;

  const statusValue =
    state.kind === 'probing'
      ? intl.formatMessage({ id: 'errorState.manage.container.probing' })
      : state.kind === 'done'
        ? state.status === 'pass'
          ? intl.formatMessage({ id: 'errorState.manage.container.running' })
          : state.engine
            ? intl.formatMessage({ id: 'errorState.manage.container.notRunning' })
            : intl.formatMessage({ id: 'errorState.manage.container.missing' })
        : notProbed;

  // Only a probe that actually came back "pass" earns the success colour.
  const statusBadge: 'emerald' | 'amber' | 'rose' | undefined =
    state.kind === 'done'
      ? state.status === 'pass'
        ? 'emerald'
        : state.engine
          ? 'amber'
          : 'rose'
      : undefined;

  return (
    <SettingsSection description={intl.formatMessage({ id: 'errorState.manage.container.desc' })}>
      <SettingsCard>
        <SettingRow
          label={intl.formatMessage({ id: 'settings.container.engine' })}
          value={engineValue}
        />
        <SettingRow
          label={intl.formatMessage({ id: 'settings.container.socket' })}
          value={intl.formatMessage({ id: 'errorState.manage.container.socketUnreported' })}
        />
        <SettingRow
          label={intl.formatMessage({ id: 'settings.container.status' })}
          value={statusValue}
          badge={statusBadge}
        />
      </SettingsCard>

      {state.kind === 'error' ? (
        <ErrorState
          variant="inline"
          error={state.error}
          title={intl.formatMessage({ id: 'errorState.manage.container.probeFailed' })}
          onRetry={() => void probe()}
        />
      ) : (
        <p className="text-xs text-muted-foreground">
          {state.kind === 'done'
            ? state.status === 'pass'
              ? intl.formatMessage({ id: 'errorState.manage.container.socketHint' })
              : state.engine
                ? state.message
                : intl.formatMessage({ id: 'errorState.manage.container.missingHint' })
            : intl.formatMessage({ id: 'errorState.manage.container.notProbedHint' })}
        </p>
      )}

      <div className="flex justify-end">
        <Button
          variant="outline"
          size="sm"
          onClick={() => void probe()}
          disabled={state.kind === 'probing'}
        >
          <RefreshCw className={state.kind === 'probing' ? 'animate-spin' : undefined} />
          {state.kind === 'probing'
            ? intl.formatMessage({ id: 'errorState.manage.container.probing' })
            : intl.formatMessage({ id: 'errorState.manage.container.probe' })}
        </Button>
      </div>
    </SettingsSection>
  );
}
