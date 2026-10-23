import { useParams } from 'react-router';
import { ComputerWatchPanel } from '@/components/agent/ComputerWatchPanel';

export function ComputerWatchPage() {
  const { agentId } = useParams<{ agentId: string }>();
  if (!agentId || !/^[A-Za-z0-9_-]+$/.test(agentId) || agentId.length > 128) return null;
  return <ComputerWatchPanel key={agentId} agentId={agentId} />;
}
