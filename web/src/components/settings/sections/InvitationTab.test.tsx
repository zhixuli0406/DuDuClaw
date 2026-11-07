import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { InvitationTab } from './InvitationTab';

const content = { opening: 'Welcome', steps: 'Set a password', examples: ['Summarize', 'Draft', 'Remind'] as [string, string, string], course_url: '' };
const preview = { subject: 'Welcome aboard', html: '<p>Server-rendered preview</p>', text: 'Server-rendered preview', examples: content.examples };
beforeEach(() => {
  vi.restoreAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  vi.spyOn(api.invitation, 'get').mockResolvedValue({ locale: 'en', content });
  vi.spyOn(api.invitation, 'preview').mockResolvedValue(preview);
  vi.spyOn(api.invitation, 'update').mockResolvedValue({ locale: 'en', content });
});
it('renders the real server preview in a script-disabled sandbox and refreshes edits', async () => {
  renderWithProviders(<InvitationTab />);
  expect(await screen.findByDisplayValue('Welcome')).toBeInTheDocument();
  const frame = await screen.findByTitle('Invitation email preview');
  expect(frame).toHaveAttribute('sandbox', '');
  expect(frame).toHaveAttribute('srcdoc', preview.html);
  fireEvent.change(screen.getByLabelText('Opening text'), { target: { value: '<img src=x onerror=alert(1)>' } });
  await waitFor(() => expect(api.invitation.preview).toHaveBeenLastCalledWith({ locale: 'en', content: { ...content, opening: '<img src=x onerror=alert(1)>' } }));
});
it('refuses insecure course links and saves the selected locale with all examples', async () => {
  renderWithProviders(<InvitationTab />);
  const course = await screen.findByLabelText('Course link (optional)');
  fireEvent.change(course, { target: { value: 'http://example.test/course' } });
  expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
  fireEvent.change(course, { target: { value: 'https://example.test/course' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save' }));
  await waitFor(() => expect(api.invitation.update).toHaveBeenCalledWith('en', { ...content, course_url: 'https://example.test/course' }));
});
it('keeps an unreadable configuration disabled until retry succeeds', async () => {
  vi.mocked(api.invitation.get).mockRejectedValueOnce(new Error('network unavailable'));
  renderWithProviders(<InvitationTab />);
  expect(await screen.findByRole('alert')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
  expect(await screen.findByLabelText('Opening text')).toBeInTheDocument();
});
it.each(['employee', 'manager'])('never requests invitation settings for %s', role => {
  useAuthStore.setState({ user: { id: 'other', role } as never });
  renderWithProviders(<InvitationTab />);
  expect(api.invitation.get).not.toHaveBeenCalled();
  expect(api.invitation.preview).not.toHaveBeenCalled();
  expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument();
});
