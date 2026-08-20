/**
 * O-3 inline action/confirmation cards — structured chat artifacts rendered
 * in the operator console's message flow (`commercial/docs/DESIGN-agent-os-
 * native-apps-2026-08.md` §6.2/§6.3 O-3). See `artifact-types.ts` for the
 * data contract O-1/O-4 attach to a reply, and `fixtures.ts` for mock
 * payloads exercised by this directory's tests.
 */
export type {
  ChatArtifact,
  ChatArtifactType,
  DeviceStatusArtifact,
  UpdateStatusArtifact,
  SystemUpdateCheckInfo,
  SystemUpdateCheckPayload,
  BackupResultArtifact,
  BackupResultPayload,
  NetworkInfoArtifact,
  ConfirmActionArtifact,
  ConfirmActionKind,
  ConfirmActionPayload,
  UpdateConfirmArtifact,
  UpdateConfirmTarget,
  UpdateConfirmPayload,
  ApprovalRequestArtifact,
} from './artifact-types';
export { ChatArtifactCard } from './ChatArtifactCard';
export { ArtifactShell } from './ArtifactShell';
export { ArtifactCardSkeleton } from './ArtifactCardSkeleton';
export { DeviceStatusCard } from './DeviceStatusCard';
export { UpdateStatusCard } from './UpdateStatusCard';
export { BackupResultCard } from './BackupResultCard';
export { NetworkInfoCard } from './NetworkInfoCard';
export { ConfirmActionCard } from './ConfirmActionCard';
export { UpdateConfirmCard } from './UpdateConfirmCard';
export { ApprovalRequestCard } from './ApprovalRequestCard';
