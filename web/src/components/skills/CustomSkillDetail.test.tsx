import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen, act, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import type { CustomSkillRecord } from '@/lib/api-custom-skills';

const listCustomSkills = vi.fn();
const generateCustomSkill = vi.fn();

vi.mock('@/lib/api-custom-skills', () => ({
  listCustomSkills: (...a: unknown[]) => listCustomSkills(...a),
  generateCustomSkill: (...a: unknown[]) => generateCustomSkill(...a),
  updateCustomSkill: vi.fn(),
  submitCustomSkill: vi.fn(),
  retireCustomSkill: vi.fn(),
}));

import { CustomSkillDetail } from './CustomSkillDetail';

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
    status: 'generating',
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

describe('CustomSkillDetail polling (RM-088)', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] });
    listCustomSkills.mockReset();
    generateCustomSkill.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => false });
  });

  it('a steady generating record causes a bounded number of list calls', async () => {
    listCustomSkills.mockResolvedValue({ custom_skills: [makeRecord()], count: 1 });
    renderWithProviders(<CustomSkillDetail id="sk-1" />);
    await flush();
    expect(listCustomSkills).toHaveBeenCalledTimes(1);

    await flush(3900);
    expect(listCustomSkills).toHaveBeenCalledTimes(1); // first poll at 4s, not before
    await flush(200);
    expect(listCustomSkills).toHaveBeenCalledTimes(2);

    // Backoff 4s → 8s → 16s → 32s: 60s of steady state is a handful of calls, not dozens.
    await flush(60_000);
    expect(listCustomSkills.mock.calls.length).toBeLessThanOrEqual(6);
    // The page content stayed on screen (no full-page loader swap).
    expect(screen.getAllByText('My Skill').length).toBeGreaterThan(0);
  });

  it('does not poll a draft and keeps in-progress edits', async () => {
    listCustomSkills.mockResolvedValue({ custom_skills: [makeRecord({ status: 'draft' })], count: 1 });
    renderWithProviders(<CustomSkillDetail id="sk-1" />);
    await flush();
    const input = screen.getByDisplayValue('My Skill');
    fireEvent.change(input, { target: { value: 'Edited name' } });
    await flush(120_000);
    expect(listCustomSkills).toHaveBeenCalledTimes(1);
    expect(screen.getByDisplayValue('Edited name')).toBeInTheDocument();
  });

  it('pauses polling while the tab is hidden', async () => {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
    listCustomSkills.mockResolvedValue({ custom_skills: [makeRecord()], count: 1 });
    renderWithProviders(<CustomSkillDetail id="sk-1" />);
    await flush();
    await flush(120_000);
    expect(listCustomSkills).toHaveBeenCalledTimes(1);
  });

  it('shows the failure reason and a retry that regenerates', async () => {
    listCustomSkills.mockResolvedValue({
      custom_skills: [makeRecord({ status: 'draft', generation_error: 'no_draft', can_regenerate: true })],
      count: 1,
    });
    generateCustomSkill.mockResolvedValue({ success: true });
    renderWithProviders(<CustomSkillDetail id="sk-1" />);
    await flush();
    // The reason CODE is localized, never shown raw.
    expect(screen.getByText('The assistant finished but did not write the skill draft.')).toBeInTheDocument();
    expect(screen.queryByText('no_draft')).not.toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Try again/ }));
    });
    await flush();
    expect(generateCustomSkill).toHaveBeenCalledWith({ id: 'sk-1' });
  });

  it('slow hint follows server can_regenerate, not the browser clock', async () => {
    // Browser-clock age is huge, but the server says the attempt is still fresh.
    listCustomSkills.mockResolvedValue({
      custom_skills: [
        makeRecord({ generation_started_at: '2020-01-01T00:00:00Z', generation_age_secs: 5, can_regenerate: false }),
      ],
      count: 1,
    });
    renderWithProviders(<CustomSkillDetail id="sk-1" />);
    await flush();
    expect(screen.queryByText(/taking longer than usual/i)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Try again/ })).not.toBeInTheDocument();
  });

  it('shows the slow hint and retry when the server allows regenerate', async () => {
    listCustomSkills.mockResolvedValue({
      custom_skills: [makeRecord({ generation_age_secs: 400, can_regenerate: true })],
      count: 1,
    });
    generateCustomSkill.mockResolvedValue({ success: true });
    renderWithProviders(<CustomSkillDetail id="sk-1" />);
    await flush();
    expect(screen.getByText(/taking longer than usual/i)).toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Try again/ }));
    });
    await flush();
    expect(generateCustomSkill).toHaveBeenCalledWith({ id: 'sk-1' });
  });
});
