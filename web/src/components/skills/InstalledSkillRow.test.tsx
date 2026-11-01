import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import type { SkillInfo } from '@/lib/api';
import { InstalledSkillRow } from './InstalledSkillRow';

function renderSkill(signature_status?: SkillInfo['signature_status']) {
  return renderWithProviders(
    <InstalledSkillRow
      skill={{ name: 'notes', content: '', signature_status, security_status: 'pass' }}
      owner="agnes"
      shared={false}
      canManage={false}
      selectable={false}
      selected={false}
      onSelect={vi.fn()}
      onShare={vi.fn()}
      onUsage={vi.fn()}
      onToggle={vi.fn()}
      onRemove={vi.fn()}
    />,
  );
}

describe('InstalledSkillRow source signature', () => {
  it('shows a legacy skill without a signature status as unsigned despite a passing scan', () => {
    renderSkill();
    expect(screen.getByText('Unsigned')).toBeInTheDocument();
    expect(screen.queryByText(/Verified source:/)).not.toBeInTheDocument();
  });

  it('identifies only a verified publisher', () => {
    renderSkill({ state: 'verified', publisher: 'example' });
    expect(screen.getByText('Verified source: example')).toBeInTheDocument();
  });

  it('does not present an unverified publisher claim as verified', () => {
    renderSkill({ state: 'unsigned', publisher: 'claimed-publisher' });
    expect(screen.getByText('Unsigned')).toBeInTheDocument();
    expect(screen.queryByText(/Verified source:/)).not.toBeInTheDocument();
  });
});
