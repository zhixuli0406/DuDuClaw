/**
 * Structural copies of the paperclip adapter contract, transcribed first-hand
 * from `paperclipai/paperclip` `docs/adapters/creating-an-adapter.md`
 * (verified 2026-07-11).
 *
 * They are declared locally instead of importing `@paperclipai/adapter-utils`
 * so this package builds hermetically; the contract is structural, so a host
 * passing its own context object type-checks against these shapes.
 */

export interface AdapterInvocationMeta {
  [key: string]: unknown;
}

export interface AdapterExecutionContext {
  runId: string;
  agent: {
    id: string;
    companyId: string;
    name: string;
    adapterConfig: unknown;
  };
  runtime: {
    sessionId: string | null;
    sessionParams: Record<string, unknown> | null;
  };
  config: Record<string, unknown>;
  context: Record<string, unknown>;
  onLog: (stream: "stdout" | "stderr", chunk: string) => Promise<void>;
  onMeta?: (meta: AdapterInvocationMeta) => Promise<void>;
  onSpawn?: (meta: { pid: number; startedAt: string }) => Promise<void>;
}

export interface AdapterExecutionResult {
  exitCode: number | null;
  signal: string | null;
  timedOut: boolean;
  errorMessage?: string | null;
  usage?: { inputTokens: number; outputTokens: number };
  sessionParams?: Record<string, unknown> | null;
  sessionDisplayId?: string | null;
  provider?: string | null;
  model?: string | null;
  costUsd?: number | null;
  clearSession?: boolean;
}

export type AdapterCheckLevel = "info" | "warn" | "error";

export interface AdapterEnvironmentCheck {
  level: AdapterCheckLevel;
  message: string;
  code: string;
  hint?: string;
}

export interface AdapterEnvironmentTestContext {
  adapterType: string;
  config?: Record<string, unknown>;
}

export interface AdapterEnvironmentTestResult {
  adapterType: string;
  status: "pass" | "warn" | "fail";
  checks: AdapterEnvironmentCheck[];
  testedAt: string;
}

export interface ServerAdapterModule {
  type: string;
  execute: (ctx: AdapterExecutionContext) => Promise<AdapterExecutionResult>;
  testEnvironment: (
    ctx: AdapterEnvironmentTestContext,
  ) => Promise<AdapterEnvironmentTestResult>;
  supportsLocalAgentJwt?: boolean;
  supportsInstructionsBundle?: boolean;
  instructionsPathKey?: string;
  requiresMaterializedRuntimeSkills?: boolean;
}

/** One row of `duduclaw agent list --json`. */
export interface DuduclawAgent {
  name: string;
  display_name: string;
  role: string;
  status: string;
  trigger: string;
  reports_to: string;
  icon: string;
  model: string;
}
