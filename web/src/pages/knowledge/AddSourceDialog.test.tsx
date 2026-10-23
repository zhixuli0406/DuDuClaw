import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { AddSourceDialog, looksLikeWebAddress } from './AddSourceDialog';
import { callsOf, collection, mockRpc, renderAt } from './test-fixtures';

/** Minimal XMLHttpRequest stand-in; the test drives progress and the answer. */
class FakeXhr {
  static last: FakeXhr | null = null;
  upload: { onprogress: ((e: { lengthComputable: boolean; loaded: number; total: number }) => void) | null } = { onprogress: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  status = 0;
  responseText = '';
  method = '';
  url = '';
  headers: Record<string, string> = {};
  body: FormData | null = null;
  constructor() {
    FakeXhr.last = this;
  }
  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }
  setRequestHeader(k: string, v: string) {
    this.headers[k] = v;
  }
  send(body: FormData) {
    this.body = body;
  }
  abort() {
    this.onabort?.();
  }
  progress(loaded: number, total: number) {
    this.upload.onprogress?.({ lengthComputable: true, loaded, total });
  }
  respond(status: number, body: unknown) {
    this.status = status;
    this.responseText = JSON.stringify(body);
    this.onload?.();
  }
}

beforeEach(() => {
  vi.clearAllMocks();
  FakeXhr.last = null;
  vi.stubGlobal('XMLHttpRequest', FakeXhr);
  useAuthStore.setState({ jwt: 'tok-1' } as never);
});
afterEach(() => vi.unstubAllGlobals());

function file(name: string, size = 10) {
  return new File(['x'.repeat(size)], name, { type: 'application/octet-stream' });
}

describe('AddSourceDialog', () => {
  it('keeps one body box while switching between upload, paste and web address', async () => {
    const user = userEvent.setup();
    renderAt(<AddSourceDialog collection={collection()} onClose={vi.fn()} onAdded={vi.fn()} />);
    const body = await screen.findByTestId('add-source-body');
    expect(body.className).toContain('min-h-');
    await user.click(screen.getByRole('radio', { name: /Paste text/ }));
    expect(screen.getByTestId('add-source-body')).toBe(body);
    await user.click(screen.getByRole('radio', { name: /Web page/ }));
    expect(screen.getByTestId('add-source-body')).toBe(body);
  });

  it('uploads dropped files with a progress bar and lists each outcome', async () => {
    const user = userEvent.setup();
    const onAdded = vi.fn();
    renderAt(<AddSourceDialog collection={collection()} onClose={vi.fn()} onAdded={onAdded} />);
    const zone = await screen.findByTestId('kb-dropzone');
    fireEvent.drop(zone, { dataTransfer: { files: [file('a.pdf'), file('b.doc'), file('a.pdf')] } });
    expect(screen.getByText('a.pdf')).toBeInTheDocument();
    expect(screen.getByText('b.doc')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Upload 2 files' }));

    const xhr = FakeXhr.last!;
    expect(xhr.method).toBe('POST');
    expect(xhr.url).toBe('/api/knowledge/upload');
    expect(xhr.headers.Authorization).toBe('Bearer tok-1');
    const parts = Array.from(xhr.body!.entries()).map(([k]) => k);
    expect(parts).toEqual(['collection_id', 'file', 'file']);

    act(() => xhr.progress(40, 100));
    expect(screen.getByRole('progressbar', { name: 'Upload progress' })).toHaveAttribute('aria-valuenow', '40');
    act(() =>
      xhr.respond(200, {
        files: [
          { name: 'a.pdf', ok: true, source_id: 's1', duplicate: false },
          { name: 'b.doc', ok: false, error_code: 'legacy_office', message: '舊版 Office 格式無法讀取。' },
        ],
      }),
    );
    expect(await screen.findByText(/added, waiting to be read/)).toBeInTheDocument();
    expect(screen.getByText(/舊版 Office 格式無法讀取。/)).toBeInTheDocument();
    expect(screen.queryByRole('progressbar')).toBeNull();
    expect(onAdded).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
  });

  it('reports a refused upload (for example, no permission) without crashing', async () => {
    const user = userEvent.setup();
    renderAt(<AddSourceDialog collection={collection()} onClose={vi.fn()} onAdded={vi.fn()} />);
    fireEvent.drop(await screen.findByTestId('kb-dropzone'), { dataTransfer: { files: [file('a.pdf')] } });
    await user.click(screen.getByRole('button', { name: 'Upload 1 file' }));
    act(() => FakeXhr.last!.respond(413, { error: '上傳內容格式不正確或超過大小上限。' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('上傳內容格式不正確或超過大小上限。');
  });

  it('adds pasted text', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    mockRpc({ 'knowledge.sources.add': () => ({ source: { title: 'Note' }, duplicate: false }) });
    renderAt(<AddSourceDialog collection={collection()} onClose={onClose} onAdded={vi.fn()} />);
    await user.click(screen.getByRole('radio', { name: 'Paste text' }));
    const add = screen.getByRole('button', { name: 'Add' });
    expect(add).toBeDisabled();
    await user.type(screen.getByLabelText('Title (optional)'), 'Note');
    await user.type(screen.getByLabelText('Text'), '請假要提前三天申請。');
    await user.click(add);
    await waitFor(() =>
      expect(callsOf('knowledge.sources.add')).toEqual([{ collection_id: 'col_1', kind: 'paste', text: '請假要提前三天申請。', title: 'Note' }]),
    );
    expect(onClose).toHaveBeenCalled();
  });

  it('accepts only web addresses for a web page', async () => {
    const user = userEvent.setup();
    mockRpc({ 'knowledge.sources.add': () => ({ source: { title: 'x' }, duplicate: true }) });
    renderAt(<AddSourceDialog collection={collection()} onClose={vi.fn()} onAdded={vi.fn()} />);
    await user.click(screen.getByRole('radio', { name: 'Web page' }));
    await user.type(screen.getByLabelText('Address'), 'file:///etc/passwd');
    expect(screen.getByRole('button', { name: 'Add' })).toBeDisabled();
    await user.clear(screen.getByLabelText('Address'));
    await user.type(screen.getByLabelText('Address'), 'https://example.com/faq');
    await user.click(screen.getByRole('button', { name: 'Add' }));
    await waitFor(() => expect(callsOf('knowledge.sources.add')[0]).toMatchObject({ kind: 'url', url: 'https://example.com/faq' }));
  });

  it('looksLikeWebAddress', () => {
    expect(looksLikeWebAddress('https://a.example')).toBe(true);
    expect(looksLikeWebAddress('http://a.example/x')).toBe(true);
    expect(looksLikeWebAddress('ftp://a.example')).toBe(false);
    expect(looksLikeWebAddress('not a url')).toBe(false);
  });

  it('refuses unsupported and oversized files before sending, using the gateway limits', async () => {
    const user = userEvent.setup();
    mockRpc({ 'knowledge.limits': () => ({ max_source_bytes: 1024 * 1024, max_files_per_upload: 2, extensions: ['pdf', 'md'] }) });
    renderAt(<AddSourceDialog collection={collection()} onClose={vi.fn()} onAdded={vi.fn()} />);
    await waitFor(() => expect(callsOf('knowledge.limits')).toHaveLength(1));
    expect(await screen.findByText(/Each file up to 1 MB/)).toBeInTheDocument();
    fireEvent.drop(screen.getByTestId('kb-dropzone'), {
      dataTransfer: { files: [file('a.pdf'), file('old.doc'), file('big.pdf', 2 * 1024 * 1024), file('b.md'), file('c.md')] },
    });
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('old.doc — this file type is not supported');
    expect(alert).toHaveTextContent('big.pdf — larger than the 1 MB limit');
    expect(alert).toHaveTextContent('c.md — only 2 files at a time');
    await user.click(screen.getByRole('button', { name: 'Upload 2 files' }));
    const names = Array.from(FakeXhr.last!.body!.getAll('file')).map((f) => (f as File).name);
    expect(names).toEqual(['a.pdf', 'b.md']);
  });
});
