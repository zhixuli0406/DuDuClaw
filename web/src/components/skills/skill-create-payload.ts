/**
 * Pure parsing of an `skill_create` approval payload (T13.3). The gateway ships
 * the ARTIFACT that actually takes effect (the full SKILL.md) plus the safety
 * report and human fields — the approver reviews the patch, not a narrative
 * (security convention 4). Kept pure + typed so the render + the test both read
 * from one normalizer that tolerates a loosely-typed `unknown` payload.
 */
import type { SafetyReport, SafetyReportFinding } from '@/lib/api-custom-skills';

export interface SkillCreatePayload {
  custom_skill_id: string;
  slug: string;
  display_name: string;
  description_human: string;
  time_saved_value: number;
  time_saved_unit: string;
  /** Comma-separated tags. */
  tags: string;
  created_by_user: string;
  built_by_agent: string;
  /** The full SKILL.md that installs on approve. */
  skill_md: string;
  safety_report: SafetyReport | null;
}

function str(v: unknown, fallback = ''): string {
  return typeof v === 'string' ? v : fallback;
}

function num(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0;
}

function parseSafetyReport(v: unknown): SafetyReport | null {
  if (!v || typeof v !== 'object') return null;
  const r = v as Record<string, unknown>;
  const rawFindings = Array.isArray(r.findings) ? r.findings : [];
  const findings: SafetyReportFinding[] = rawFindings.map((f) => {
    const o = (f && typeof f === 'object' ? f : {}) as Record<string, unknown>;
    return {
      category: str(o.category),
      severity: str(o.severity),
      description: str(o.description),
      line_number: typeof o.line_number === 'number' ? o.line_number : null,
    };
  });
  const trial = (r.sandbox_trial && typeof r.sandbox_trial === 'object'
    ? (r.sandbox_trial as Record<string, unknown>)
    : {}) as Record<string, unknown>;
  return {
    passed: r.passed === true,
    risk_level: str(r.risk_level),
    findings,
    sandbox_trial: {
      ran: trial.ran === true,
      skip_reason: typeof trial.skip_reason === 'string' ? trial.skip_reason : undefined,
    },
  };
}

/** Normalize an opaque approval payload into a typed skill_create view, or null. */
export function parseSkillCreatePayload(payload: unknown): SkillCreatePayload | null {
  let obj: unknown = payload;
  if (typeof payload === 'string') {
    try {
      obj = JSON.parse(payload);
    } catch {
      return null;
    }
  }
  if (!obj || typeof obj !== 'object') return null;
  const p = obj as Record<string, unknown>;
  // Require the artifact to be present — without a SKILL.md there is nothing to
  // review, so we decline the specialized view and fall back to raw payload.
  if (typeof p.skill_md !== 'string' || p.skill_md.length === 0) return null;
  return {
    custom_skill_id: str(p.custom_skill_id),
    slug: str(p.slug),
    display_name: str(p.display_name),
    description_human: str(p.description_human),
    time_saved_value: num(p.time_saved_value),
    time_saved_unit: str(p.time_saved_unit, 'minutes_per_use'),
    tags: str(p.tags),
    created_by_user: str(p.created_by_user),
    built_by_agent: str(p.built_by_agent),
    skill_md: p.skill_md,
    safety_report: parseSafetyReport(p.safety_report),
  };
}
