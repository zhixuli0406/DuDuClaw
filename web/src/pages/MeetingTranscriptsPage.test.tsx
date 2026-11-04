import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { MeetingJobList, MeetingTranscriptsPage } from './MeetingTranscriptsPage';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { meetingApi, uploadMeeting } from '@/lib/meeting/api';
vi.mock('@/lib/meeting/audio', () => ({ prepareMeetingAudio: vi.fn(async (file: File) => ({ file, path: 'original' })) }));
vi.mock('@/lib/meeting/api', () => ({ meetingApi: { list: vi.fn(), cancel: vi.fn(), download: vi.fn() }, uploadMeeting: vi.fn() }));
beforeEach(() => { vi.clearAllMocks(); vi.mocked(meetingApi.list).mockResolvedValue({ jobs: [], max_bytes: 4 * 1024 ** 3 }); });
describe('meeting transcripts', () => {
  it('renders job progress, cancellation and completed downloads', () => {
    const cancel = vi.fn(); const download = vi.fn();
    renderWithProviders(<MeetingJobList jobs={[
      { job_id: 'one', state: 'transcribing', chunk: 2, chunks: 4, created_at: 0 },
      { job_id: 'two', state: 'done', chunk: 4, chunks: 4, created_at: 0 },
    ]} onCancel={cancel} onDownload={download} />);
    expect(screen.getByRole('progressbar')).toHaveAttribute('max', '4');
    fireEvent.click(screen.getByRole('button', { name: /Cancel transcription|取消轉錄/ }));
    expect(cancel).toHaveBeenCalledWith('one');
    fireEvent.click(screen.getByRole('button', { name: /Download transcript|下載逐字稿/ }));
    expect(download).toHaveBeenCalledWith('two', 'md');
  });
  it('shows only selectable employees with work access to non-admins and reports upload progress', async () => {
    useAuthStore.setState({ user: { id: 'person', role: 'employee', email: '', display_name: 'Person', status: 'active' } });
    vi.spyOn(api.agents, 'callable').mockResolvedValue({ default_agent: 'main', agents: [
      { name: 'alice', display_name: 'Alice', call_text: '@alice', via: 'binding', level: 'operator', assignable: true, selectable: true },
      { name: 'main', display_name: 'Main', call_text: '@main', role: 'main', via: 'binding', level: 'owner', assignable: true, selectable: true },
      { name: 'bob', display_name: 'Bob', call_text: '@bob', via: 'team', level: 'operator', assignable: true, selectable: false },
      { name: 'team', display_name: 'Team member', call_text: '@team', via: 'team', level: 'operator', assignable: true, selectable: true },
      { name: 'viewer', display_name: 'Viewer', call_text: '@viewer', via: 'binding', level: 'viewer', assignable: false, selectable: true },
    ] });
    vi.mocked(uploadMeeting).mockImplementation(async (_file, _agent, _user, progress) => { progress(0.5); return new Promise(() => {}); });
    renderWithProviders(<MeetingTranscriptsPage />);
    await screen.findByRole('option', { name: 'Alice' });
    expect(screen.queryByRole('option', { name: 'Main' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Bob' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Team member' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Viewer' })).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(/Choose audio or video|選擇錄音或影片/), { target: { files: [new File(['audio'], 'meeting.wav')] } });
    fireEvent.click(screen.getByRole('button', { name: /Start transcription|開始轉錄/ }));
    await waitFor(() => expect(screen.getByRole('progressbar')).toHaveAttribute('value', '0.5'));
    expect(uploadMeeting).toHaveBeenCalledWith(expect.any(File), 'alice', 'person', expect.any(Function), expect.any(AbortSignal));
  });
});
