import { meetingFetch, type MeetingJob } from './api';

export const DRIVE_FILE_SCOPE = 'https://www.googleapis.com/auth/drive.file';
export const MEET_READ_SCOPE = 'https://www.googleapis.com/auth/meetings.space.readonly';
export interface MeetingDriveStatus {
  connected: boolean;
  authorized: boolean;
  picker_configured: boolean;
  client_id: string | null;
  developer_key: string | null;
  app_id: string | null;
  account_email: string | null;
}
export interface DriveRecordingFile { id: string; name: string; mime_type: string; size: number; modified_time?: string }
export interface MeetRecording { file_id: string; start_time: string; conference: string }
export interface PickedRecording { id: string; name: string }
const query = (agent: string, search?: string, pageToken?: string) => {
  const params = new URLSearchParams({ agent });
  if (search) params.set('search', search);
  if (pageToken) params.set('page_token', pageToken);
  return params.toString();
};
export const meetingDriveApi = {
  status: async (signal?: AbortSignal): Promise<MeetingDriveStatus> => (await meetingFetch('/api/meeting-drive/status', { signal, cache: 'no-store' })).json(),
  files: async (agent: string, search: string, pageToken?: string, signal?: AbortSignal): Promise<{ files: DriveRecordingFile[]; next_page_token?: string }> =>
    (await meetingFetch(`/api/meeting-drive/files?${query(agent, search, pageToken)}`, { signal, cache: 'no-store' })).json(),
  recordings: async (agent: string, pageToken?: string, signal?: AbortSignal): Promise<{ recordings: MeetRecording[]; next_page_token?: string; truncated: boolean }> =>
    (await meetingFetch(`/api/meeting-drive/recordings?${query(agent, undefined, pageToken)}`, { signal, cache: 'no-store' })).json(),
  registerSelection: async (agent: string, fileId: string, signal?: AbortSignal): Promise<{ selection_receipt: string; expires_in: number }> =>
    (await meetingFetch('/api/meeting-drive/selection', { method: 'POST', signal, cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ agent_id: agent, file_id: fileId, confirm: true }) })).json(),
  import: async (agent: string, fileId: string, selectionReceipt: string, signal?: AbortSignal): Promise<MeetingJob> =>
    (await meetingFetch('/api/meeting-drive/import', { method: 'POST', signal, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ agent_id: agent, file_id: fileId, selection_receipt: selectionReceipt, confirm: true }) })).json(),
};
