import { useEffect } from 'react';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useVisibleAgents } from '@/lib/data-scope';
import { WorldStagePlaceholder } from '@/components/home/WorldStagePlaceholder';

/**
 * WorldPage (`/world`) — the immersive, full-bleed "嘟嘟事務所" world (openhuman
 * Tiny Place style). Unlike the 38vh Home band, this breaks out of the page
 * max-width and fills the viewport height (minus the header): the world canvas
 * is edge-to-edge, the ROOM scene panel floats top-right and a small info card
 * floats top-left. The `-m-6` cancels `MainLayout`'s `<main>` padding so the
 * canvas truly reaches every edge; the height calc leaves room for the `h-14`
 * header. Same degradation splitter as the band (variant `'full'`) — reduced
 * motion / no-WebGL still drop to the static scene.
 */
export function WorldPage() {
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  // Data-scoped: an employee sees only their own AI staff (§3.4 WP11-T11.3).
  const agents = useVisibleAgents();
  const authed = useConnectionStore((s) => s.state) === 'authenticated';

  useEffect(() => {
    if (authed) fetchAgents();
  }, [authed, fetchAgents]);

  return (
    <div className="-m-6 h-[calc(100dvh-3.5rem)] overflow-hidden">
      <WorldStagePlaceholder agents={agents} variant="full" />
    </div>
  );
}
