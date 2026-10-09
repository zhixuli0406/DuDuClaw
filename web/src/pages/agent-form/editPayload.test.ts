import { describe, it, expect } from 'vitest';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import {
  buildUpdatePayload,
  clampInt,
  isAdminOnlyPath,
  kvRowsFromTable,
  parseKvRow,
  teamRolesShareVendor,
  type EditFormState,
  type PayloadContext,
} from './editPayload';
import {
  DEFAULT_ADVANCED,
  DEFAULT_CAPABILITIES,
  DEFAULT_EVOLUTION_ADVANCED,
  DEFAULT_ODOO,
  DEFAULT_OS_WATCH,
  DEFAULT_RESEARCH,
  DEFAULT_RUNTIME,
  DEFAULT_V168,
} from './defaults';

const CTX: PayloadContext = {
  savedCloudPreferred: 'claude-sonnet',
  savedCloudFallback: '',
  hasSavedLocal: false,
  firstCloudModel: 'claude-haiku',
};

function state(over: Partial<EditFormState> = {}): EditFormState {
  return {
    form: {
      display_name: 'Bot',
      reports_to: 'boss',
      department: 'ops',
      heartbeat_cron: '*/5 * * * *',
      preferred: 'claude-sonnet',
      fallback: '',
      local_model: '',
    },
    caps: DEFAULT_CAPABILITIES,
    osWatch: DEFAULT_OS_WATCH,
    research: DEFAULT_RESEARCH,
    runtime: DEFAULT_RUNTIME,
    evoAdv: DEFAULT_EVOLUTION_ADVANCED,
    odoo: { ...DEFAULT_ODOO, unblock_models: [], clear_api_key: false, clear_password: false },
    adv: DEFAULT_ADVANCED,
    v: DEFAULT_V168,
    ...over,
  };
}

describe('buildUpdatePayload — only dirty fields are sent', () => {
  it('sends nothing when nothing is dirty', () => {
    const r = buildUpdatePayload(state(), new Set(), CTX);
    expect(r.hasChanges).toBe(false);
    expect(r.payload).toEqual({});
  });

  it('a display-name edit sends only display_name (no cron, no org fields, no local table)', () => {
    const r = buildUpdatePayload(state(), new Set(['form.display_name']), CTX);
    expect(r.payload).toEqual({ display_name: 'Bot' });
  });

  it('does not send an unset proactive max_turns', () => {
    const r = buildUpdatePayload(state(), new Set(['adv.proactive_max_turns']), CTX);
    expect(r.payload).toEqual({});
  });

  it('never sends the removed dead keys even if a stale dirty path exists', () => {
    const r = buildUpdatePayload(
      state(),
      new Set(['form.sticker_enabled', 'form.skill_security_scan', 'form.readonly_project', 'form.max_concurrent', 'adv.stagnation_enabled']),
      CTX,
    );
    expect(r.payload).toEqual({});
  });

  it('sends heartbeat_cron only when it was edited', () => {
    const r = buildUpdatePayload(state(), new Set(['form.heartbeat_cron']), CTX);
    expect(r.payload).toEqual({ heartbeat_cron: '*/5 * * * *' });
  });

  it('never writes local_backend / context_length / gpu_layers, and no prefer_local for a cloud-only employee', () => {
    const r = buildUpdatePayload(state(), new Set(['form.preferred']), CTX);
    expect(r.payload).toEqual({ preferred: 'claude-sonnet' });
  });

  it('decomposes a local preferred model into the local table', () => {
    const s = state({ form: { preferred: 'local:qwen3', fallback: '' } });
    const r = buildUpdatePayload(s, new Set(['form.preferred']), CTX);
    expect(r.payload).toEqual({ preferred: 'claude-sonnet', local_model: 'qwen3', prefer_local: true });
  });

  it('sends only the edited capability keys and computer_use_config keys', () => {
    const s = state({
      caps: {
        ...DEFAULT_CAPABILITIES,
        approval_required_tools: ['send_message'],
        computer_use_config: { ...DEFAULT_CAPABILITIES.computer_use_config, max_actions: 7 },
      },
    });
    const r = buildUpdatePayload(
      s,
      new Set(['caps.approval_required_tools', 'caps.computer_use_config.max_actions']),
      CTX,
    );
    expect(r.payload).toEqual({
      capabilities: { approval_required_tools: ['send_message'], computer_use_config: { max_actions: 7 } },
    });
  });

  it('sends the keep-alive and takeover windows only when edited, clamped to range', () => {
    const s = state({
      caps: {
        ...DEFAULT_CAPABILITIES,
        computer_use_config: { ...DEFAULT_CAPABILITIES.computer_use_config, keep_alive_minutes: 30, takeover_idle_minutes: 5 },
      },
    });
    const r = buildUpdatePayload(s, new Set(['caps.computer_use_config.keep_alive_minutes']), CTX);
    expect(r.payload).toEqual({ capabilities: { computer_use_config: { keep_alive_minutes: 30 } } });
    expect(clampInt(999, 0, 240)).toBe(240);
    expect(clampInt(-3, 0, 240)).toBe(0);
    expect(clampInt(0, 1, 60)).toBe(1);
    expect(clampInt(2.6, 1, 60)).toBe(3);
    expect(clampInt(Number.NaN, 1, 60)).toBe(1);
  });

  it('sends only edited odoo keys (no blanking profile/url when one allowlist changes)', () => {
    const s = state({
      odoo: { ...DEFAULT_ODOO, allowed_models: ['crm.lead'], unblock_models: [], clear_api_key: false, clear_password: false },
    });
    const r = buildUpdatePayload(s, new Set(['odoo.allowed_models']), CTX);
    expect(r.payload).toEqual({ odoo: { allowed_models: ['crm.lead'] } });
  });

  it('maps the v1.68 controls onto their sections', () => {
    const v = {
      ...DEFAULT_V168,
      daily_cap_cents: 250,
      effort: 'high' as const,
      minimal_context: false,
      fork_enabled: true,
      team_enabled: false,
      team_roles: { ...DEFAULT_V168.team_roles, verifier: { runtime: 'codex', model: 'gpt-5', effort: 'high' } },
      guardrails_enabled: true,
      guardrails_deny_phrases: ['secret'],
      decision_continuity: true,
      decision_ttl_days: 14,
      night_engine_enabled: true,
    };
    const dirty = new Set([
      'v.daily_cap_cents', 'v.effort', 'v.minimal_context', 'v.fork_enabled', 'v.team_enabled',
      'v.team_roles.verifier.runtime', 'v.team_roles.verifier.model', 'v.guardrails_enabled',
      'v.guardrails_deny_phrases', 'v.decision_continuity', 'v.decision_ttl_days', 'v.night_engine_enabled',
    ]);
    const r = buildUpdatePayload(state({ v }), dirty, CTX);
    expect(r.payload).toEqual({
      budget: { daily_cap_cents: 250 },
      model: { effort: 'high' },
      runtime: { minimal_context: false },
      fork: { enabled: true },
      team: { enabled: false, roles: { verifier: { runtime: 'codex', model: 'gpt-5' } } },
      guardrails: { enabled: true, deny_phrases: ['secret'] },
      memory: { decision_continuity: true, decision_ttl_days: 14 },
      night_engine: { enabled: true },
    });
  });

  it('sends typed key/value entries and skips invalid rows', () => {
    const adv = {
      ...DEFAULT_ADVANCED,
      kv: [
        { section: 'prompt', key: 'cli_bare_mode', value: 'true', type: 'boolean' as const },
        { section: 'prompt', key: 'minimal_core_kb', value: '4', type: 'integer' as const },
        { section: 'fork', key: 'default_budget_usd', value: '1.5', type: 'float' as const },
        { section: 'skills', key: 'recommended', value: 'a, b', type: 'string_array' as const },
        { section: 'prompt', key: 'bad', value: '4.2', type: 'integer' as const },
      ],
    };
    const r = buildUpdatePayload(state({ adv }), new Set(['kv']), CTX);
    expect(r.includesKv).toBe(true);
    expect(r.payload.advanced_kv).toEqual([
      { section: 'prompt', key: 'cli_bare_mode', value: true, type: 'boolean' },
      { section: 'prompt', key: 'minimal_core_kb', value: 4, type: 'integer' },
      { section: 'fork', key: 'default_budget_usd', value: 1.5, type: 'float' },
      { section: 'skills', key: 'recommended', value: ['a', 'b'], type: 'string_array' },
    ]);
  });
});

describe('typed key/value helpers', () => {
  it('reports which part of a row is wrong', () => {
    expect(parseKvRow({ section: 'Prompt!', key: 'k', value: 'v', type: 'string' })).toEqual({ kind: 'error', error: 'section' });
    expect(parseKvRow({ section: 'prompt', key: 'a-b', value: 'v', type: 'string' })).toEqual({ kind: 'error', error: 'key' });
    expect(parseKvRow({ section: 'prompt', key: 'Mode', value: 'v', type: 'string' })).toEqual({ kind: 'error', error: 'key' });
    expect(parseKvRow({ section: 'a.b.c.d', key: 'k', value: 'v', type: 'string' })).toEqual({ kind: 'error', error: 'section' });
    expect(parseKvRow({ section: 'evolution.noise_band', key: 'k', value: '1', type: 'integer' }).kind).toBe('ok');
    expect(parseKvRow({ section: 'sticker', key: 'k', value: 'v', type: 'string' })).toEqual({ kind: 'error', error: 'section' });
    expect(parseKvRow({ section: 'prompt', key: 'k', value: 'yes', type: 'boolean' })).toEqual({ kind: 'error', error: 'value' });
    expect(parseKvRow({ section: 'prompt', key: '', value: '', type: 'string' })).toEqual({ kind: 'empty' });
    for (const sec of ['ptc', 'cultural_context', 'capabilities', 'runtime', 'odoo', 'mcp', 'channels', 'agent', 'container', 'permissions']) {
      expect(parseKvRow({ section: sec, key: 'k', value: 'v', type: 'string' })).toEqual({ kind: 'error', error: 'section' });
    }
  });

  it('prefills rows with types inferred from the inspect table', () => {
    expect(kvRowsFromTable('prompt', { cli_bare_mode: true, minimal_core_kb: 4, mode: 'minimal', r: 0.5, l: ['x'] })).toEqual([
      { section: 'prompt', key: 'cli_bare_mode', value: 'true', type: 'boolean' },
      { section: 'prompt', key: 'minimal_core_kb', value: '4', type: 'integer' },
      { section: 'prompt', key: 'mode', value: 'minimal', type: 'string' },
      { section: 'prompt', key: 'r', value: '0.5', type: 'float' },
      { section: 'prompt', key: 'l', value: 'x', type: 'string_array' },
    ]);
  });
});

describe('admin-only paths and the team vendor rule', () => {
  it('classifies org, capability, sandbox and soul fields as admin-only', () => {
    for (const p of ['form.reports_to', 'form.department', 'form.can_modify_own_soul', 'form.sandbox_enabled',
      'form.network_access', 'caps.scoped_tools', 'caps.computer_use_config.max_actions']) {
      expect(isAdminOnlyPath(p)).toBe(true);
    }
    for (const p of ['form.display_name', 'v.fork_enabled', 'kv', 'runtime.provider']) {
      expect(isAdminOnlyPath(p)).toBe(false);
    }
  });

  it('flags executor and verifier on the same vendor', () => {
    const roles = { ...DEFAULT_V168.team_roles };
    expect(teamRolesShareVendor({ ...roles, executor: { runtime: 'claude', model: '', effort: '' }, verifier: { runtime: 'claude', model: '', effort: '' } })).toBe(true);
    expect(teamRolesShareVendor({ ...roles, executor: { runtime: 'claude', model: '', effort: '' }, verifier: { runtime: 'codex', model: '', effort: '' } })).toBe(false);
    expect(teamRolesShareVendor(roles)).toBe(false);
  });
});

describe('i18n catalogues', () => {
  it('have identical key sets in all three locales', () => {
    const keys = (o: Record<string, string>) => Object.keys(o).sort();
    expect(keys(zhTW)).toEqual(keys(en));
    expect(keys(jaJP)).toEqual(keys(en));
  });
});
