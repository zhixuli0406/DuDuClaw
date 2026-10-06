import { describe, expect, it } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { TaskAudienceNotice } from './TaskAudienceNotice';

describe('TaskAudienceNotice', () => {
  it('says who limited the task, without account ids', () => {
    renderWithProviders(<TaskAudienceNotice restriction={{ state: 'limited', keys: ['user:4f1c-uuid', 'role:manager', 'channel:telegram'], sources: [{ round: '2', from_role: 'executor', packet_id: 'p', keys: ['user:4f1c-uuid'] }] }} />);
    expect(screen.getByText(/limited to: 1 named accounts; role Manager; channel telegram/)).toBeInTheDocument();
    expect(screen.getByText(/Round 2, written by the Execution role/)).toBeInTheDocument();
    expect(screen.queryByText(/4f1c-uuid/)).not.toBeInTheDocument();
  });
  it('marks damaged packets and the reader-outside case; open renders nothing', () => {
    const { rerender } = renderWithProviders(<TaskAudienceNotice restriction={{ state: 'unreadable', keys: [], sources: [] }} />);
    expect(screen.getByText(/cannot be read or is damaged/)).toBeInTheDocument();
    rerender(<TaskAudienceNotice restricted />);
    expect(screen.getByText(/You can see only its card/)).toBeInTheDocument();
    rerender(<TaskAudienceNotice restriction={{ state: 'open', keys: [], sources: [] }} />);
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });
});
