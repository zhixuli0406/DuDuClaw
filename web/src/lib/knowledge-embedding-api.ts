/**
 * Meaning-based search for the document knowledge base (文件知識庫) —
 * dashboard client for `knowledge.embedding.*`.
 *
 * `status` is readable by any signed-in user; every change is Admin-only and
 * enforced by the gateway. Choosing a cloud provider requires an explicit
 * acknowledgement that de-identified passages are sent to that vendor; the
 * gateway records who acknowledged what and refuses cloud calls without it.
 */
import { client } from './ws-client';

export type KbEmbeddingProvider = 'local' | 'openai' | 'gemini' | 'none';

export interface KbEmbeddingModel {
  id: string;
  display_name: string;
  license: string;
  license_source: string;
  repo: string;
  revision: string;
  dim: number;
  speed: 'fast' | 'slow' | string;
  /** Bytes an install would download now (model, plus the runtime when missing). */
  size_bytes: number;
  model_bytes: number;
  installed: boolean;
  problem: string | null;
  selected: boolean;
}

export interface KbEmbeddingProgress {
  file: string;
  done_bytes: number;
  total_bytes: number;
}

export interface KbEmbeddingCloudTarget {
  provider?: 'openai' | 'gemini';
  vendor?: string;
  model?: string;
  host?: string;
  ack_valid?: boolean;
  error?: string;
}

export interface KbEmbeddingAck {
  by: string;
  at: string;
  provider: string;
  host: string;
  model: string;
}

export interface KbEmbeddingSettings {
  provider: KbEmbeddingProvider;
  available: boolean;
  mode: 'hybrid' | 'keyword_only';
  reason: string | null;
  coverage: number;
  model_id: string | null;
  egress: 'local' | 'cloud';
  chunks_with_vectors: number;
  chunks_total: number;
  vector_rows: number;
  max_vector_rows: number;
  local_model: string;
  local_loaded: boolean;
  platform_supported: boolean;
  models: KbEmbeddingModel[];
  job: {
    model: string | null;
    state: 'idle' | 'downloading' | 'error';
    progress: KbEmbeddingProgress | null;
    error: string | null;
  };
  cloud: KbEmbeddingCloudTarget | null;
  cloud_providers: Array<{ id: 'openai' | 'gemini'; vendor: string; default_model: string; host: string }>;
  acknowledgement: KbEmbeddingAck | null;
  redaction: { ready: boolean; reason?: string } | null;
  is_admin: boolean;
}

export interface KbEmbeddingSelection {
  provider: KbEmbeddingProvider;
  local_model?: string;
  cloud_model?: string;
  cloud_account?: string;
  acknowledge_disclosure?: boolean;
}

const call = <T,>(method: string, params: Record<string, unknown> = {}) =>
  client.call(method, params) as Promise<T>;

export const knowledgeEmbeddingApi = {
  status: () => call<KbEmbeddingSettings>('knowledge.embedding.status'),
  install: (model: string) => call<{ started: boolean; installed: boolean }>('knowledge.embedding.install', { model }),
  cancel: () => call<{ cancelled: boolean }>('knowledge.embedding.cancel'),
  remove: (model: string) => call<{ removed: boolean }>('knowledge.embedding.remove', { model }),
  verifyOffline: (model: string) =>
    call<{ installed: boolean; queued: number }>('knowledge.embedding.verify_offline', { model }),
  select: (selection: KbEmbeddingSelection) =>
    call<{ saved: boolean; queued: number }>('knowledge.embedding.select', { ...selection }),
  rebuild: () => call<{ queued: number }>('knowledge.embedding.rebuild'),
};
