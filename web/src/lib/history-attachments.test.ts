import { describe, expect, it, vi } from 'vitest';
import { historyDownloadName, parseHistoryAttachments, readHistoryAttachmentBlob } from './history-attachments';
import { historyToMessages } from '@/stores/chat-store';

describe('history attachment references', () => {
  it('accepts structured references and drops unexpected path fields', () => {
    const attachment = { id: 'opaque', name: 'report.pdf', size: 10, mime: 'application/pdf' };
    expect(parseHistoryAttachments([{ ...attachment, path: '/private/host/file' }])).toEqual([attachment]);
    expect(historyToMessages([{ role: 'assistant', content: 'done', timestamp: '2026-11-05', attachments: [attachment] }])[0].attachments).toEqual([attachment]);
  });
  it('ignores malformed references and never interprets Markdown as file authority', () => {
    expect(parseHistoryAttachments([null, {}, { id: 'file', name: 'x', size: -1, mime: 'text/plain' }])).toEqual([]);
    expect(parseHistoryAttachments('/Users/private/file')).toEqual([]);
    const message = historyToMessages([{ role: 'user', timestamp: '2026-11-05', content: '[📎 x (file)](/private/file)' }])[0];
    expect(message.attachments).toBeUndefined();
  });
});

describe('bounded attachment reads', () => {
  const signal = () => new AbortController().signal;
  const response = (length: string | null, chunks: number[]) => {
    const cancel = vi.fn();
    const headers = new Headers({ 'Content-Type': 'image/png' });
    if (length !== null) headers.set('Content-Length', length);
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        const size = chunks.shift();
        if (size === undefined) controller.close();
        else controller.enqueue(new Uint8Array(size));
      },
      cancel,
    });
    return { response: { headers, body } as Response, cancel };
  };

  it('reads an exact bounded body', async () => {
    const { response: reply } = response('3', [1, 2]);
    const blob = await readHistoryAttachmentBlob(reply, 3, signal());
    expect(blob.size).toBe(3);
    expect(blob.type).toBe('image/png');
  });

  it.each([null, '', '-1', '2.5', '9007199254740992', '11'])('cancels before reading invalid or oversized length %s', async length => {
    const { response: reply, cancel } = response(length, [1]);
    await expect(readHistoryAttachmentBlob(reply, 10, signal())).rejects.toThrow();
    expect(cancel).toHaveBeenCalledOnce();
  });

  it('cancels a growing body as soon as its declared length is exceeded', async () => {
    const { response: reply, cancel } = response('3', [2, 2, 8]);
    await expect(readHistoryAttachmentBlob(reply, 3, signal())).rejects.toThrow();
    expect(cancel).toHaveBeenCalledOnce();
  });

  it('rejects a truncated body', async () => {
    const { response: reply } = response('3', [2]);
    await expect(readHistoryAttachmentBlob(reply, 3, signal())).rejects.toThrow();
  });
});

describe('history download filename', () => {
  it('decodes the server filename and strips path components', () => {
    expect(historyDownloadName("attachment; filename*=UTF-8''" + encodeURIComponent('../報告.docx'), 'forged.exe')).toBe('報告.docx');
    expect(historyDownloadName('attachment; filename="folder/report.pdf"', 'forged.exe')).toBe('report.pdf');
    expect(historyDownloadName(null, 'legacy.pdf')).toBe('legacy.pdf');
  });
  it('refuses malformed, control-character and dot filenames', () => {
    for (const value of ['bad%ZZ', '..', '%00bad.pdf']) {
      expect(historyDownloadName("attachment; filename*=UTF-8''" + value, 'forged.exe')).toBe('attachment');
    }
  });
});
