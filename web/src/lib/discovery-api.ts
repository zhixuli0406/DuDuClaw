import { client } from '@/lib/ws-client';
import { isDeprecatedRuntime } from '@/lib/deprecated-runtimes';

// Brand names are not translated; only `openai-compat` takes a localized label when a formatter is supplied.
const RUNTIME_NAMES: Record<string, string> = { claude: 'Claude', codex: 'Codex', gemini: 'Gemini', antigravity: 'Antigravity', grok: 'Grok', 'openai-compat': 'OpenAI-compatible' };
export function runtimeLabel(id: string, intl?: { messages: Record<string, unknown>; formatMessage: (d: { id: string }) => string }): string {
  if (typeof id !== 'string' || id === '') return id ?? '';
  if (id === 'openai-compat' && intl && 'discovery.runtime.openaiCompat' in intl.messages) return intl.formatMessage({ id: 'discovery.runtime.openaiCompat' });
  const name = Object.prototype.hasOwnProperty.call(RUNTIME_NAMES, id) ? RUNTIME_NAMES[id] : id;
  // A deprecated runtime stays displayable but carries the localized marker.
  if (isDeprecatedRuntime(id) && intl) return `${name} (${intl.formatMessage({ id: 'common.deprecated' })})`;
  return name;
}
/** Public DTOs contain opaque approved IDs and never host workspace paths. */
export interface DiscoveryBudget { max_agent_calls: number; max_usd: number; max_wall_secs: number; max_rounds: number }
export interface DiscoveryCost { usd: number; usd_source: 'reported' | 'estimated' | 'unknown' | 'pending'; unknown_calls: number; wall_secs: number; input_tokens: number; output_tokens: number; cache_read_tokens: number }
export interface DiscoveryRun { run_id: string; task_id: string; title: string; status: string; approval_status: string; approval_expires_at?: string | null; current_round: number; branch_count: number; refine_count: number; runtime: string; budget: DiscoveryBudget; cost: DiscoveryCost; can_cancel: boolean; artifact_verified: boolean; degraded: boolean; isolation_degraded?: boolean; stop_code?: string | null; cancel_code?: string | null; tree_available?: boolean; tree_unavailable_reason?: string | null }
export interface DiscoveryNode { cell_id: string; round: number; branch: number; attempt: number; parent_id: string | null; status: string; score: number | null; model: string | null; cost: DiscoveryCost }
export interface DiscoveryRound { round: number; policy_id: string; beta: number; full_grid: string[]; completion: string[]; new_in: string[] }
export interface DiscoveryTree { run: DiscoveryRun; nodes: DiscoveryNode[]; rounds: DiscoveryRound[] }
export interface DiscoveryCatalog { roots: { id: string; label: string }[]; evaluators: { name: string; label: string }[]; runtimes: string[]; can_create: boolean; requires_approval: boolean }
export interface CreateDiscoveryInput { assigned_to: string; title: string; description: string; discovery: { approved_root_id: string; evaluator: string; runtime: string; model: string; branch_count: number; refine_count: number; max_parallelism: number; budget: DiscoveryBudget } }
export interface DiscoveryArtifact { run_id: string; cell_id: string; verified: true; files: { file_id: string; name: string; size_bytes: number }[] }
export interface DiscoveryDownload { run_id: string; cell_id: string; file_id: string; name: string; size_bytes: number; content_base64: string }
export const discoveryApi = {
  catalog: (agentId: string) => client.call('discovery.catalog', { agent_id: agentId }) as Promise<DiscoveryCatalog>,
  list: (agentId?: string) => client.call('discovery.list', { ...(agentId ? { agent_id: agentId } : {}), limit: 20 }) as Promise<{ runs: DiscoveryRun[] }>,
  tree: (runId: string) => client.call('discovery.tree', { run_id: runId }) as Promise<DiscoveryTree>,
  create: (input: CreateDiscoveryInput) => {
    const d=input.discovery;
    return client.call('tasks.create', { assigned_to: input.assigned_to, title: input.title, description: input.description, kind: 'discovery',
      discovery: { approved_root_id: d.approved_root_id, evaluator: d.evaluator, runtime: d.runtime, model: d.model,
        branch_count: d.branch_count, refine_count: d.refine_count, max_parallelism: d.max_parallelism,
        budget: { max_agent_calls: d.budget.max_agent_calls, max_usd: d.budget.max_usd, max_wall_secs: d.budget.max_wall_secs, max_rounds: d.budget.max_rounds } },
    }) as Promise<{ task_id: string; run_id: string; status: 'pending_approval' | 'queued'; approval_id: string | null }>;
  },
  cancel: (runId: string) => client.call('discovery.cancel', { run_id: runId }) as Promise<{ status: string }>,
  artifact: (runId: string) => client.call('discovery.artifact', { run_id: runId }) as Promise<DiscoveryArtifact>,
  download: (runId: string, fileId: string) => client.call('discovery.artifact', { run_id: runId, file_id: fileId }) as Promise<DiscoveryDownload>,
};
/** Bytes come from the ACL/manifest-verified RPC, never a server filesystem URL. */
export function artifactBlob(download: DiscoveryDownload, runId: string, fileId: string): Blob {
  if (download.run_id !== runId || download.file_id !== fileId || !Number.isSafeInteger(download.size_bytes)
      || download.size_bytes < 0 || download.size_bytes > 16 * 1024 * 1024
      || download.content_base64.length > Math.ceil(16 * 1024 * 1024 / 3) * 4
      || /[\\/\r\n]/.test(download.name) || !download.name) throw new Error('Invalid artifact response');
  const decoded=atob(download.content_base64);
  if (decoded.length !== download.size_bytes) throw new Error('Artifact size mismatch');
  return new Blob([Uint8Array.from(decoded, char => char.charCodeAt(0))], { type: 'application/octet-stream' });
}
