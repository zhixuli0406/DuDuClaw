import type { ChatArtifact } from './artifact-types';
import { DeviceStatusCard } from './DeviceStatusCard';
import { UpdateStatusCard } from './UpdateStatusCard';
import { BackupResultCard } from './BackupResultCard';
import { NetworkInfoCard } from './NetworkInfoCard';
import { ConfirmActionCard } from './ConfirmActionCard';
import { UpdateConfirmCard } from './UpdateConfirmCard';
import { ApprovalRequestCard } from './ApprovalRequestCard';

/**
 * ChatArtifactCard — the single dispatch point O-1/O-4 render through: given
 * one `ChatArtifact` (see `artifact-types.ts`), pick and render the matching
 * card. This is the whole of O-3's public surface for `OperatorConsolePage`
 * — it never needs to know the individual card components exist.
 *
 * The `never` branch below is a compile-time completeness check: adding a
 * new `ChatArtifactType` without a case here fails `tsc`, not silently
 * renders nothing.
 */
export function ChatArtifactCard({ artifact }: { artifact: ChatArtifact }) {
  switch (artifact.type) {
    case 'device_status':
      return <DeviceStatusCard payload={artifact.payload} />;
    case 'update_status':
      return <UpdateStatusCard payload={artifact.payload} />;
    case 'backup_result':
      return <BackupResultCard payload={artifact.payload} />;
    case 'network_info':
      return <NetworkInfoCard payload={artifact.payload} />;
    case 'confirm_action':
      return <ConfirmActionCard payload={artifact.payload} />;
    case 'update_confirm':
      return <UpdateConfirmCard payload={artifact.payload} />;
    case 'approval_request':
      return <ApprovalRequestCard payload={artifact.payload} />;
    default: {
      const exhaustive: never = artifact;
      console.warn('[ChatArtifactCard] unknown artifact type', exhaustive);
      return null;
    }
  }
}
