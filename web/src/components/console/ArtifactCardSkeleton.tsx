import { useIntl } from 'react-intl';
import { Sparkles } from 'lucide-react';
import { Skeleton } from '@/components/mds';
import { ArtifactShell } from './ArtifactShell';

/**
 * Loading placeholder for a chat artifact that's still being prepared —
 * e.g. O-1 recognized a system-operation intent and dispatched the O-0
 * tool, but the result hasn't come back yet. Not wired to anything this
 * round (no artifact is produced end-to-end yet — see the WIRING TODO in
 * `artifact-types.ts`); exported so O-1/O-4 can show it between "user asked"
 * and "artifact arrived" instead of a bare typing indicator when the reply
 * is known to be building toward a structured card.
 */
export function ArtifactCardSkeleton({ label }: { label?: string }) {
  const intl = useIntl();
  return (
    <ArtifactShell icon={Sparkles} title={label ?? intl.formatMessage({ id: 'console.artifact.skeleton.title' })}>
      <div className="space-y-2">
        <Skeleton className="h-4 w-3/4" />
        <Skeleton className="h-4 w-1/2" />
        <Skeleton className="h-4 w-2/3" />
      </div>
    </ArtifactShell>
  );
}
