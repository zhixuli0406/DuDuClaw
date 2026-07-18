import { useEffect } from 'react';
import { useIntl } from 'react-intl';
import { Globe2 } from 'lucide-react';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useVisibleAgents } from '@/lib/data-scope';
import { WorldStagePlaceholder } from '@/components/home/WorldStagePlaceholder';
import { PageHeader } from '@/components/mds';

/**
 * WorldPage (`/world`) — the immersive, full-bleed "嘟嘟事務所" world (openhuman
 * Tiny Place style). Unlike the 38vh Home band, this breaks out of the page
 * max-width and fills the viewport height (minus a slim mds PageHeader): the
 * world canvas is edge-to-edge, the ROOM scene panel floats top-right and a
 * small info card floats top-left. The `-m-6` cancels `MainLayout`'s content
 * padding so the shell truly reaches every edge; the height calc leaves room
 * for the mobile top bar (`3.5rem`), and the PageHeader (`h-12`) is carved out
 * of that budget via flexbox so the canvas below always gets the remainder.
 * Same degradation splitter as the band (variant `'full'`) — reduced motion /
 * no-WebGL still drop to the static scene.
 */
export function WorldPage() {
  const intl = useIntl();
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  // Data-scoped: an employee sees only their own AI staff (§3.4 WP11-T11.3).
  const agents = useVisibleAgents();
  const authed = useConnectionStore((s) => s.state) === 'authenticated';

  useEffect(() => {
    if (authed) fetchAgents();
  }, [authed, fetchAgents]);

  return (
    <div className="-m-6 flex h-[calc(100dvh-3.5rem)] flex-col overflow-hidden">
      <PageHeader hideTrigger className="shrink-0 px-5">
        <Globe2 className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.world' })}</h1>
        <span className="hidden truncate text-sm text-muted-foreground md:block">
          {intl.formatMessage({ id: 'nav.world.desc' })}
        </span>
      </PageHeader>
      <div className="relative min-h-0 flex-1">
        <WorldStagePlaceholder agents={agents} variant="full" />
      </div>
    </div>
  );
}
