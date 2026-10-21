/**
 * Document knowledge base (文件知識庫) — dashboard client.
 *
 * Collections are the notebooks; each holds sources (uploaded files, pasted
 * text, web pages). Every call is permission-checked by the gateway against
 * the collection's grants; nothing here decides who may see what.
 *
 * RPCs go over the dashboard WebSocket (`knowledge.*`). File upload and the
 * original-file download are plain HTTP with the dashboard's Bearer token:
 * `POST /api/knowledge/upload` (multipart, `collection_id` before the
 * files) and `GET /api/knowledge/sources/{id}/original`.
 */
import { client } from './ws-client';
import { useAuthStore } from '@/stores/auth-store';

export type KbRole = 'viewer' | 'editor' | 'owner';
export type KbSensitivity = 'public' | 'internal' | 'personal' | 'restricted';
export type KbPrincipalKind = 'user' | 'department' | 'agent' | 'channel';

export const KB_SENSITIVITIES: readonly KbSensitivity[] = ['public', 'internal', 'personal', 'restricted'];
export const KB_ROLES: readonly KbRole[] = ['viewer', 'editor', 'owner'];

export interface KbCollection {
  id: string;
  slug: string;
  name: string;
  description: string;
  department: string | null;
  sensitivity: KbSensitivity;
  embedding_policy: 'inherit' | 'local_only';
  acl_revision: number;
  created_by: string;
  created_at: string;
  updated_at: string;
  my_role: KbRole | null;
  source_count: number;
  ready_count: number;
  chunk_count: number;
}

export type KbSourceStatus =
  | 'awaiting_approval'
  | 'queued'
  | 'extracting'
  | 'indexing'
  | 'embedding'
  | 'ready'
  | 'failed'
  | 'deleting';

export interface KbJobProgress {
  kind: string;
  state: string;
  attempts: number;
  progress_done: number;
  progress_total: number;
}

export interface KbSource {
  id: string;
  collection_id: string;
  kind: 'file' | 'paste' | 'url' | 'attachment';
  title: string;
  original_name: string | null;
  url: string | null;
  format: string;
  bytes: number;
  chars: number;
  pages: number | null;
  chunk_count: number;
  status: KbSourceStatus;
  status_detail: string | null;
  error_code: string | null;
  error_message: string | null;
  warnings: unknown;
  created_by: string;
  created_at: string;
  updated_at: string;
  indexed_at: string | null;
  job: KbJobProgress | null;
}

export interface KbAclEntry {
  kind: KbPrincipalKind;
  id: string;
  role: KbRole;
  granted_by?: string;
  granted_at?: string;
}

export interface KbHit {
  chunk_id: number;
  source_id: string;
  collection_id: string;
  title: string;
  locator_text: string;
  heading_path: string;
  text: string;
  score: number;
  suspect: boolean;
}

export type KbRetrievalMode = 'hybrid' | 'keyword_only' | 'vector_only' | 'none';

export interface KbSearchResult {
  hits: KbHit[];
  mode: KbRetrievalMode;
  vector_unavailable: string | null;
  scan_capped: boolean;
}

export interface KbPassage {
  chunk_id: number;
  source_id: string;
  collection_id: string;
  title: string;
  locator_text: string;
  heading_path: string;
  segment_label: string | null;
  before: string;
  match: string;
  after: string;
  prev_chunk_id: number | null;
  next_chunk_id: number | null;
  suspect: boolean;
}

export interface KbAccessRow {
  id: number;
  at: string;
  op: string;
  agent_id: string | null;
  requester_kind: string;
  requester_id: string | null;
  channel: string | null;
  query: string | null;
  result_count: number;
  mode: string | null;
  outcome: 'ok' | 'denied' | 'error' | 'no_match';
}

export interface KbDeniedRow {
  id: number;
  at: string;
  op: string;
  collection_id: string | null;
  collection_name: string | null;
  agent_id: string | null;
  requester_kind: string;
  requester_id: string | null;
  channel: string | null;
  query: string | null;
}

export interface KbPersonRef {
  id: string;
  display_name: string;
  department: string | null;
}

export interface KbPrincipals {
  edition: 'personal' | 'enterprise';
  kinds: KbPrincipalKind[];
  users: KbPersonRef[];
  departments: string[];
  agents: KbPersonRef[];
  channels: string[];
}

export interface KbEmbeddingStatus {
  provider: string;
  available: boolean;
  mode: KbRetrievalMode;
  reason: string | null;
  coverage: number;
}

export interface KbAgentCollections {
  granted: Array<{
    id: string;
    slug: string;
    name: string;
    description: string;
    sensitivity: KbSensitivity;
    source_count: number;
  }>;
  /** `null` = every granted collection. */
  bound: string[] | null;
  /** The employee's settings file could not be read: treated as none. */
  unreadable: boolean;
}

export interface KbUploadFileResult {
  name: string;
  ok: boolean;
  source_id?: string;
  duplicate?: boolean;
  error_code?: string;
  message?: string;
}

export interface KbNewCollection {
  name: string;
  slug?: string;
  description?: string;
  department?: string;
  sensitivity?: KbSensitivity;
}

export interface KbCollectionPatch {
  name?: string;
  description?: string;
  department?: string;
  sensitivity?: KbSensitivity;
  embedding_policy?: 'inherit' | 'local_only';
}

const call = <T>(method: string, params?: Record<string, unknown>) =>
  client.call(method, params) as Promise<T>;

export const knowledgeApi = {
  listCollections: () =>
    call<{ collections: KbCollection[]; can_create: boolean }>('knowledge.collections.list'),
  getCollection: (id: string) => call<{ collection: KbCollection }>('knowledge.collections.get', { id }),
  createCollection: (fields: KbNewCollection) =>
    call<{ collection: KbCollection }>('knowledge.collections.create', { ...fields }),
  updateCollection: (id: string, expectedUpdatedAt: string, patch: KbCollectionPatch) =>
    call<{ collection: KbCollection }>('knowledge.collections.update', {
      id,
      expected_updated_at: expectedUpdatedAt,
      ...patch,
    }),
  deleteCollection: (id: string, confirmName: string) =>
    call<{ deleted: boolean }>('knowledge.collections.delete', { id, confirm_name: confirmName }),

  getAcl: (collectionId: string) =>
    call<{ entries: KbAclEntry[]; revision: number }>('knowledge.acl.get', { collection_id: collectionId }),
  setAcl: (collectionId: string, expectedRevision: number, entries: KbAclEntry[]) =>
    call<{ revision: number }>('knowledge.acl.set', {
      collection_id: collectionId,
      expected_revision: expectedRevision,
      entries: entries.map(({ kind, id, role }) => ({ kind, id, role })),
    }),
  searchPrincipals: (collectionId: string, query: string) =>
    call<KbPrincipals>('knowledge.principals.search', { collection_id: collectionId, query }),

  listSources: (collectionId: string) =>
    call<{ sources: KbSource[] }>('knowledge.sources.list', { collection_id: collectionId }),
  addPaste: (collectionId: string, text: string, title?: string) =>
    call<{ source: KbSource; duplicate: boolean }>('knowledge.sources.add', {
      collection_id: collectionId,
      kind: 'paste',
      text,
      ...(title ? { title } : {}),
    }),
  addUrl: (collectionId: string, url: string, title?: string) =>
    call<{ source: KbSource; duplicate: boolean }>('knowledge.sources.add', {
      collection_id: collectionId,
      kind: 'url',
      url,
      ...(title ? { title } : {}),
    }),
  deleteSource: (sourceId: string) => call<{ deleted: boolean }>('knowledge.sources.delete', { source_id: sourceId }),
  reindexSource: (sourceId: string) => call<{ queued: boolean }>('knowledge.sources.reindex', { source_id: sourceId }),

  search: (query: string, collections?: string[], limit = 10) =>
    call<KbSearchResult>('knowledge.search', { query, limit, ...(collections ? { collections } : {}) }),
  getPassage: (chunkId: number) => call<KbPassage>('knowledge.passage.get', { chunk_id: chunkId }),

  accessLog: (collectionId: string, beforeId?: number) =>
    call<{ entries: KbAccessRow[] }>('knowledge.access_log.list', {
      collection_id: collectionId,
      limit: 50,
      ...(beforeId !== undefined ? { before_id: beforeId } : {}),
    }),
  deniedReads: (beforeId?: number) =>
    call<{ entries: KbDeniedRow[] }>('knowledge.access_log.denied', {
      limit: 50,
      ...(beforeId !== undefined ? { before_id: beforeId } : {}),
    }),
  embeddingStatus: () => call<KbEmbeddingStatus>('knowledge.embedding.status'),

  agentCollections: (agentId: string) =>
    call<KbAgentCollections>('knowledge.agent.collections', { agent_id: agentId }),
  /** `null` = every granted collection; `[]` = none. Written through `agents.update`. */
  bindAgent: (agentId: string, collections: string[] | null) =>
    call<unknown>('agents.update', { agent_id: agentId, knowledge: { collections } }),
};

function authHeader(): Record<string, string> {
  const jwt = useAuthStore.getState().jwt;
  return jwt ? { Authorization: `Bearer ${jwt}` } : {};
}

/** Thrown by the HTTP helpers; `message` is the gateway's own sentence. */
export class KbHttpError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'KbHttpError';
  }
}

function parseError(status: number, body: string): KbHttpError {
  try {
    const parsed = JSON.parse(body) as { error?: unknown };
    if (typeof parsed.error === 'string' && parsed.error.trim() !== '') {
      return new KbHttpError(parsed.error, status);
    }
  } catch {
    /* not JSON */
  }
  return new KbHttpError(`HTTP ${status}`, status);
}

export interface UploadHandle {
  promise: Promise<KbUploadFileResult[]>;
  abort: () => void;
}

/**
 * Upload files to a collection. `onProgress` gets 0–1 while the request
 * body is sent; the promise resolves with one result per file.
 */
export function uploadSources(
  collectionId: string,
  files: readonly File[],
  onProgress?: (fraction: number) => void,
): UploadHandle {
  const xhr = new XMLHttpRequest();
  const promise = new Promise<KbUploadFileResult[]>((resolve, reject) => {
    const form = new FormData();
    // The gateway checks the collection before it stores any byte, so the
    // id must come first.
    form.append('collection_id', collectionId);
    for (const f of files) form.append('file', f, f.name);
    xhr.open('POST', '/api/knowledge/upload');
    for (const [k, v] of Object.entries(authHeader())) xhr.setRequestHeader(k, v);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && onProgress) onProgress(e.total > 0 ? e.loaded / e.total : 0);
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          const body = JSON.parse(xhr.responseText) as { files?: KbUploadFileResult[] };
          resolve(Array.isArray(body.files) ? body.files : []);
        } catch {
          reject(new KbHttpError('上傳結果無法解讀', xhr.status));
        }
      } else {
        reject(parseError(xhr.status, xhr.responseText));
      }
    };
    xhr.onerror = () => reject(new KbHttpError('Connection lost', 0));
    xhr.onabort = () => reject(new KbHttpError('aborted', 0));
    xhr.send(form);
  });
  return { promise, abort: () => xhr.abort() };
}

/** Download a source's original file through the browser. */
export async function downloadOriginal(source: Pick<KbSource, 'id' | 'title' | 'original_name' | 'format'>): Promise<void> {
  const res = await fetch(`/api/knowledge/sources/${encodeURIComponent(source.id)}/original`, {
    headers: authHeader(),
  });
  if (!res.ok) throw parseError(res.status, await res.text());
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement('a');
    a.href = url;
    a.download = source.original_name || `${source.title}.${source.format}`;
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** File types the upload route accepts (by extension). */
export const KB_UPLOAD_ACCEPT = '.pdf,.docx,.pptx,.xlsx,.xlsm,.xls,.csv,.txt,.md,.markdown,.html,.htm';

/** Most files one upload may carry (gateway limit). */
export const KB_MAX_FILES_PER_UPLOAD = 20;

/** Statuses that are still moving; the sources list keeps polling while any is present. */
export function isSourceBusy(status: KbSourceStatus): boolean {
  return status === 'queued' || status === 'extracting' || status === 'indexing' || status === 'embedding' || status === 'deleting';
}

/** Where a passage sits. The locator already names the section for
 *  section-located passages, so the heading is added only when it is new. */
export function whereText(locator: string, heading: string): string {
  if (!heading || locator.includes(heading)) return locator;
  return locator ? `${locator} · ${heading}` : heading;
}
