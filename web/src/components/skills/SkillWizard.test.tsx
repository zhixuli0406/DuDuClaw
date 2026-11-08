import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen, act, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import type { CustomSkillRecord } from '@/lib/api-custom-skills';

const listCustomSkills = vi.fn();
const createCustomSkill = vi.fn();
const generateCustomSkill = vi.fn();

vi.mock('@/lib/api-custom-skills', () => ({
  createCustomSkill: (...a: unknown[]) => createCustomSkill(...a),
  generateCustomSkill: (...a: unknown[]) => generateCustomSkill(...a),
  listCustomSkills: (...a: unknown[]) => listCustomSkills(...a),
  updateCustomSkill: vi.fn(),
  submitCustomSkill: vi.fn(),
}));

const AGENTS = [{ name: 'builder', display_name: 'Builder', status: 'active' }];
const fetchAgents = vi.fn();
vi.mock('@/lib/data-scope', () => ({ useSelectableAgents: () => AGENTS }));
vi.mock('@/stores/callable-agents-store', () => ({ canSelectAgent: () => true }));
vi.mock('@/stores/agents-store', () => ({ useAgentsStore: () => ({ fetchAgents }) }));

import { SkillWizard } from './SkillWizard';

function makeRecord(over: Partial<CustomSkillRecord> = {}): CustomSkillRecord {
  return {
    id: 'sk-1',
    slug: 'my-skill',
    display_name: 'My Skill',
    description_human: 'does a thing',
    time_saved_value: 30,
    time_saved_unit: 'minutes_per_use',
    tags: '',
    created_by_user: 'alice',
    built_by_agent: 'builder',
    status: 'draft',
    approval_id: null,
    rejection_reason: null,
    created_at: '2026-11-08T00:00:00Z',
    updated_at: '2026-11-08T00:00:00Z',
    approved_at: null,
    usage_count: 0,
    saved_hours_estimate: 0,
    generation_started_at: new Date().toISOString(),
    generation_error: null,
    ...over,
  };
}

async function flush(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function startWizard() {
  renderWithProviders(<SkillWizard />);
  await flush();
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'Summarise my daily reports' } });
  await flush();
  await act(async () => {
    fireEvent.click(screen.getByRole('button', { name: /Start generating/ }));
  });
  await flush();
}

describe('SkillWizard generate step', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] });
    listCustomSkills.mockReset();
    createCustomSkill.mockReset();
    generateCustomSkill.mockReset();
    createCustomSkill.mockResolvedValue(makeRecord());
    generateCustomSkill.mockResolvedValue({ success: true, id: 'sk-1', draft_path: '/tmp/d/SKILL.md', status: 'generating' });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('a steady generating record causes a bounded number of list calls', async () => {
    // Every poll returns a NEW object with identical status/updated_at.
    listCustomSkills.mockImplementation(async () => ({
      custom_skills: [makeRecord({ status: 'generating' })],
      count: 1,
    }));
    await startWizard();
    expect(screen.getByText('The AI staffer is drafting…')).toBeInTheDocument();
    const afterStart = listCustomSkills.mock.calls.length;
    expect(afterStart).toBeLessThanOrEqual(2); // immediate refresh only

    await flush(60_000);
    // 4s, 12s, 28s, 60s ⇒ at most ~4 backoff polls — never back-to-back requests.
    expect(listCustomSkills.mock.calls.length - afterStart).toBeLessThanOrEqual(5);
  });

  it('stops polling once the draft is ready', async () => {
    listCustomSkills.mockImplementation(async () => ({
      custom_skills: [makeRecord({ status: 'draft', generation_error: null })],
      count: 1,
    }));
    await startWizard();
    const settled = listCustomSkills.mock.calls.length;
    await flush(120_000);
    expect(listCustomSkills.mock.calls.length).toBe(settled);
  });

  it('renders the failure reason and a retry that regenerates', async () => {
    listCustomSkills.mockImplementation(async () => ({
      custom_skills: [makeRecord({ status: 'draft', generation_error: 'dispatch:timeout', can_regenerate: true })],
      count: 1,
    }));
    await startWizard();
    expect(screen.getByText('The skill draft could not be generated')).toBeInTheDocument();
    // The reason CODE is localized, never shown raw.
    expect(screen.getByText('The assistant timed out.')).toBeInTheDocument();
    expect(screen.queryByText('dispatch:timeout')).not.toBeInTheDocument();

    generateCustomSkill.mockClear();
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Try again/ }));
    });
    await flush();
    expect(generateCustomSkill).toHaveBeenCalledTimes(1);
  });

  it('slow hint + retry follow server can_regenerate, not the browser clock', async () => {
    // Started "long ago" by the browser clock, but the server says it is not stale.
    listCustomSkills.mockImplementation(async () => ({
      custom_skills: [
        makeRecord({
          status: 'generating',
          generation_started_at: '2020-01-01T00:00:00Z',
          generation_age_secs: 5,
          can_regenerate: false,
        }),
      ],
      count: 1,
    }));
    await startWizard();
    await flush(60_000);
    expect(screen.queryByText(/taking longer than usual/i)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Regenerate/ })).toBeDisabled();
  });

  it('shows the slow hint and an enabled retry once the server allows regenerate', async () => {
    listCustomSkills.mockImplementation(async () => ({
      custom_skills: [
        makeRecord({
          status: 'generating',
          generation_started_at: new Date().toISOString(),
          generation_age_secs: 400,
          can_regenerate: true,
        }),
      ],
      count: 1,
    }));
    await startWizard();
    expect(screen.getByText(/taking longer than usual/i)).toBeInTheDocument();
    const retry = screen.getByRole('button', { name: /Try again/ });
    expect(retry).toBeEnabled();
    generateCustomSkill.mockClear();
    await act(async () => {
      fireEvent.click(retry);
    });
    await flush();
    expect(generateCustomSkill).toHaveBeenCalledTimes(1);
  });
});
