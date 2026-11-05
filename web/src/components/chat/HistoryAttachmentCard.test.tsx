import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { historyToMessages } from '@/stores/chat-store';
import { MessageBubble } from './MessageBubble';
import { HistoryAttachmentCard } from './HistoryAttachmentCard';

const image = { id: 'opaque-image', name: '照片.png', mime: 'image/png', size: 2048 };
const document = { id: 'opaque-document', name: '報告.docx', mime: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document', size: 1234 };
const okBlob = (mime: string) => ({
  ok: true,
  headers: new Headers({ 'Content-Type': mime, 'Content-Length': '13' }),
  body: new ReadableStream<Uint8Array>({ start(controller) {
    controller.enqueue(new TextEncoder().encode('file contents'));
    controller.close();
  } }),
});
const fetchMock = vi.fn();
const createUrl = vi.fn(() => 'blob:attachment-preview');
const revokeUrl = vi.fn();
const originalCreate = Object.getOwnPropertyDescriptor(URL, 'createObjectURL');
const originalRevoke = Object.getOwnPropertyDescriptor(URL, 'revokeObjectURL');

beforeEach(() => {
  useAuthStore.setState({ jwt: 'session-jwt' });
  fetchMock.mockReset();
  createUrl.mockClear();
  revokeUrl.mockClear();
  vi.stubGlobal('fetch', fetchMock);
  Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: createUrl });
  Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: revokeUrl });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalCreate) Object.defineProperty(URL, 'createObjectURL', originalCreate);
  else Reflect.deleteProperty(URL, 'createObjectURL');
  if (originalRevoke) Object.defineProperty(URL, 'revokeObjectURL', originalRevoke);
  else Reflect.deleteProperty(URL, 'revokeObjectURL');
});

describe('history attachment cards', () => {
  it.each(['user', 'assistant'])('renders %s history references as cards with name, type, size and download', role => {
    const [message] = historyToMessages([{ role, content: '', timestamp: '2026-11-05T00:00:00Z', attachments: [document] }]);
    renderWithProviders(<MessageBubble message={message} />);
    expect(screen.getByText('報告.docx')).toBeInTheDocument();
    expect(screen.getByText(/application\/vnd.openxmlformats/)).toHaveTextContent('1.2K');
    expect(screen.getByText('Download to open in your document viewer.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Download' })).toBeEnabled();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('loads an authenticated thumbnail, puts no JWT in the URL, and revokes it on unmount', async () => {
    fetchMock.mockImplementation(() => Promise.resolve(okBlob('image/png')));
    const { unmount } = renderWithProviders(<HistoryAttachmentCard attachment={image} />);
    expect(await screen.findByRole('img', { name: '照片.png' })).toHaveAttribute('src', 'blob:attachment-preview');
    expect(fetchMock).toHaveBeenCalledWith('/api/chat/attachments?id=opaque-image&preview=true', expect.objectContaining({
      headers: { Authorization: 'Bearer session-jwt' }, cache: 'no-store', signal: expect.any(AbortSignal),
    }));
    unmount();
    expect(revokeUrl).toHaveBeenCalledWith('blob:attachment-preview');
    expect(fetchMock.mock.calls[0][1].signal.aborted).toBe(true);
  });

  it('waits until an image card approaches the viewport before fetching its blob', async () => {
    let intersect!: (entries: { isIntersecting: boolean }[]) => void;
    const observe = vi.fn();
    const disconnect = vi.fn();
    vi.stubGlobal('IntersectionObserver', class {
      constructor(callback: typeof intersect) { intersect = callback; }
      observe = observe;
      disconnect = disconnect;
    });
    fetchMock.mockImplementation(() => Promise.resolve(okBlob('image/png')));
    const { unmount } = renderWithProviders(<HistoryAttachmentCard attachment={image} />);
    expect(observe).toHaveBeenCalledOnce();
    expect(fetchMock).not.toHaveBeenCalled();
    act(() => intersect([{ isIntersecting: false }]));
    expect(fetchMock).not.toHaveBeenCalled();
    act(() => intersect([{ isIntersecting: true }]));
    await screen.findByRole('img', { name: '照片.png' });
    expect(fetchMock).toHaveBeenCalledOnce();
    act(() => intersect([{ isIntersecting: true }]));
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(disconnect).toHaveBeenCalled();
    unmount();
    expect(revokeUrl).toHaveBeenCalledWith('blob:attachment-preview');
  });

  it('never automatically fetches images above the preview cap, but allows explicit download', async () => {
    fetchMock.mockImplementation(() => Promise.resolve(okBlob('image/png')));
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => undefined);
    renderWithProviders(<HistoryAttachmentCard attachment={{ ...image, size: 2 * 1024 * 1024 + 1 }} />);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(screen.getByText('Images larger than 2 MiB can be downloaded to view.')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Download' }));
    await waitFor(() => expect(click).toHaveBeenCalledOnce());
    expect(fetchMock.mock.calls[0][0]).toBe('/api/chat/attachments?id=opaque-image');
  });

  it('disables oversized downloads without fetching their contents', () => {
    renderWithProviders(<HistoryAttachmentCard attachment={{ ...image, size: 20 * 1024 * 1024 + 1 }} />);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Download' })).toBeDisabled();
    expect(screen.getByText('This attachment exceeds the 20 MiB download limit.')).toBeInTheDocument();
  });

  it('downloads using the server filename instead of a forged display-label extension', async () => {
    fetchMock.mockResolvedValue({ ...okBlob(document.mime), headers: new Headers({
      'Content-Type': document.mime, 'Content-Length': '13',
      'Content-Disposition': "attachment; filename*=UTF-8''" + encodeURIComponent('實際報告.docx'),
    }) });
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
      expect(this.download).toBe('實際報告.docx');
    });
    renderWithProviders(<HistoryAttachmentCard attachment={{ ...document, name: 'forged.exe' }} />);
    fireEvent.click(screen.getByRole('button', { name: 'Download' }));
    await waitFor(() => expect(click).toHaveBeenCalledOnce());
  });

  it('rechecks permission on every download and creates a local download link', async () => {
    fetchMock.mockImplementation(() => Promise.resolve(okBlob(document.mime)));
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
      expect(this.href).toBe('blob:attachment-preview');
      expect(this.download).toBe('報告.docx');
    });
    const { unmount } = renderWithProviders(<HistoryAttachmentCard attachment={document} />);
    fireEvent.click(screen.getByRole('button', { name: 'Download' }));
    await waitFor(() => expect(click).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole('button', { name: 'Download' }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    unmount();
    expect(revokeUrl).toHaveBeenCalledWith('blob:attachment-preview');
  });

  it('handles denied reads without making a blob URL and disables downloads after logout', async () => {
    fetchMock.mockResolvedValue({ ok: false, status: 403 });
    renderWithProviders(<HistoryAttachmentCard attachment={image} />);
    expect(await screen.findByRole('alert')).toHaveTextContent('This attachment is unavailable');
    expect(createUrl).not.toHaveBeenCalled();
    act(() => useAuthStore.setState({ jwt: null }));
    expect(screen.getByRole('button', { name: 'Download' })).toBeDisabled();
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
  });

  it('discards late image responses after logout', async () => {
    let finish!: (response: ReturnType<typeof okBlob>) => void;
    fetchMock.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
    renderWithProviders(<HistoryAttachmentCard attachment={image} />);
    act(() => useAuthStore.setState({ jwt: null }));
    await act(async () => finish(okBlob('image/png')));
    expect(createUrl).not.toHaveBeenCalled();
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
  });

  it('does not inline SVG and rejects a mismatched thumbnail response type', async () => {
    fetchMock.mockResolvedValue(okBlob('image/svg+xml'));
    const { rerender } = renderWithProviders(<HistoryAttachmentCard attachment={{ ...image, mime: 'image/svg+xml' }} />);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(screen.getByText('Download to open in your document viewer.')).toBeInTheDocument();
    rerender(<HistoryAttachmentCard attachment={image} />);
    await screen.findByRole('alert');
    expect(createUrl).not.toHaveBeenCalled();
  });
});
