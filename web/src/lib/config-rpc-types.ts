/**
 * Wire types for the v1.68 dashboard-switches RPCs (W2). Contract:
 * `commercial/docs/TODO-dashboard-switches-v1.68-2026-10.md` — `tick.sources.*`,
 * `config.raw.get/set`, and the `restart_required` field the config writers
 * now return. Every field the gateway may omit is optional: an older gateway
 * answers without them and the dashboard must still work.
 */

export type TickSourceKind = 'http_poll' | 'command' | 'file_tail' | 'websocket';

/** One `[[tick.sources]]` entry as `tick.sources.list` returns it. Header
 *  values never come back — only how many are stored. `valid`/`error` is the
 *  boot loader's own verdict on the stored entry. */
export interface TickSourceConfig {
  id: string;
  kind: TickSourceKind;
  enabled?: boolean;
  url?: string;
  /** argv — the first element is the program. */
  command?: string[];
  path?: string;
  interval_secs?: number;
  /** field name → JSON pointer (`price = "/data/price"`). */
  json_fields?: Record<string, string>;
  headers_count?: number;
  max_events_per_minute?: number;
  baseline_max_age_secs?: number;
  subscribe?: string[];
  ping_interval_secs?: number;
  idle_timeout_secs?: number;
  emit_unchanged?: boolean;
  persist_every_n?: number;
  valid?: boolean;
  error?: string | null;
}

export interface TickSourcesListResult {
  enabled?: boolean;
  allow_command_sources?: boolean;
  preset?: string | null;
  dns_ttl_secs?: number | null;
  sources: TickSourceConfig[];
  /** false ⇒ edits are saved but only run after a gateway restart. */
  hot_reload_available?: boolean;
}

/** `tick.sources.upsert` params. A key that is absent keeps the stored
 *  value; `null` removes it. `headers` sent as an object replaces the stored
 *  map; values may be `secret://` references. */
export interface TickSourceUpsert {
  id: string;
  kind: TickSourceKind;
  enabled?: boolean;
  url?: string | null;
  command?: string[] | null;
  path?: string | null;
  interval_secs?: number | null;
  json_fields?: Record<string, string> | null;
  headers?: Record<string, string> | null;
  max_events_per_minute?: number | null;
  baseline_max_age_secs?: number | null;
  subscribe?: string[] | null;
}

export interface TickSourcesWriteResult {
  success?: boolean;
  changes?: string[];
  hot_reloaded?: boolean;
  restart_required?: boolean | string[];
}

/** `config.raw.*` target: config.toml, inference.toml or one employee's agent.toml. */
export type ConfigRawFile = 'config' | 'inference' | `agent:${string}`;

export interface ConfigRawGetResult {
  file?: string;
  exists?: boolean;
  /** Raw TOML with secret values replaced by `«set»`. */
  content: string;
  /** Paths of the values shown masked. */
  masked?: string[];
  /** Sections left out entirely (e.g. `mcp_keys`, whose keys are secrets). */
  hidden_sections?: string[];
  /** Send back as `base_hash` so a concurrent edit is refused. */
  hash?: string;
}

export interface ConfigRawSetResult {
  success?: boolean;
  /** Path of the backup written before the change (`<file>.bak-<ts>`). */
  backup?: string;
  restart_required?: string[] | boolean;
  changed_sections?: string[];
  hot_reloaded?: string[];
}

/** Error position the server reports when the new text does not parse. */
export interface ConfigRawErrorLocation {
  line?: number;
  column?: number;
  message: string;
}
