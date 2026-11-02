import { describe, it, expect, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { MarkdownContent } from './MarkdownContent';
import { MessageBubble } from './MessageBubble';

describe('MarkdownContent', () => {
  it('formats bold, GFM tables, lists and scrolling code', () => {
    const { container } = renderWithProviders(<MarkdownContent text={'**bold**\n\n- item\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n`inline`\n\n```js\nconst a = 1;\n```'} />);
    expect(container.querySelector('strong')).toHaveTextContent('bold');
    expect(container.querySelector('li')).toHaveTextContent('item');
    expect(screen.getByRole('table').parentElement).toHaveClass('overflow-x-auto');
    expect(container.querySelector('pre')).toHaveClass('overflow-x-auto');
    expect(container.querySelectorAll('code')).toHaveLength(2);
  });
  it('autolinks URLs and renders single links as hostname cards, including formatted labels', () => {
    renderWithProviders(<MarkdownContent text={'Visit https://example.com/path\n\n[**Guide**](https://docs.example.com/guide)\n\nhttps://example.org/page\n\n[Email](mailto:a@example.com)'} />);
    const card = screen.getByRole('link', { name: /docs.example.com.*Guide/ });
    expect(card).toHaveAttribute('title', 'https://docs.example.com/guide');
    expect(card.querySelector('svg')).toBeTruthy();
    expect(card).toHaveAttribute('target', '_blank');
    expect(card).toHaveAttribute('rel', 'noopener noreferrer');
    expect(screen.getByRole('link', { name: 'https://example.com/path' })).toHaveClass('break-all');
    expect(screen.getByRole('link', { name: /example.org/ }).querySelector('svg')).toBeTruthy();
    expect(screen.getByRole('link', { name: 'Email' }).querySelector('svg')).toBeNull();
  });
  it('shows the parsed hostname for disguised inline links but not bare URLs', () => {
    renderWithProviders(<MarkdownContent text={'[點此](https://evil.example/?q=x) https://bare.example/path'} />);
    const disguised = screen.getByRole('link', { name: /點此.*evil\.example/ });
    expect(disguised).toHaveTextContent('點此 (evil.example)');
    const bare = screen.getByRole('link', { name: 'https://bare.example/path' });
    expect(bare).toHaveTextContent('https://bare.example/path');
    expect(bare).not.toHaveTextContent('(bare.example)');
  });
  it.each(['javascript:alert%281%29', 'data:text/html,hello', 'vbscript:hello', '/relative', '//evil.example/x', '#fragment'])('rejects %s', href => {
    const { container } = renderWithProviders(<MarkdownContent text={`[unsafe](${href})`} />);
    expect(container.querySelector('a')).toBeNull();
    expect(container).toHaveTextContent('unsafe');
  });
  it('replaces remote images with links and other images with alt text', () => {
    const { container } = renderWithProviders(<MarkdownContent text={'![Remote](https://example.com/track)\n\n![](http://images.example.com/a)\n\n![Local](/local.png)\n\n![Data](data:image/png;base64,aaaa)\n\n![Mail](mailto:a@example.com)'} />);
    expect(container.querySelector('img')).toBeNull();
    expect(screen.getByRole('link', { name: 'Remote' })).toHaveAttribute('href', 'https://example.com/track');
    expect(screen.getByRole('link', { name: 'images.example.com' })).toBeTruthy();
    expect(container.querySelectorAll('a')).toHaveLength(2);
    expect(container).toHaveTextContent('Local');
    expect(container).toHaveTextContent('Data');
    expect(container).toHaveTextContent('Mail');
  });
  it('skips raw HTML', () => {
    const { container } = renderWithProviders(<MarkdownContent text={'<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n<iframe src="https://evil.example"></iframe>'} />);
    expect(container.querySelector('script, img, iframe')).toBeNull();
  });
  it.each(['**bold', '| a |', '```js\nunclosed', '[unfinished](https://'])('handles unfinished text %s', text => {
    expect(() => renderWithProviders(<MarkdownContent text={text} />)).not.toThrow();
  });
  it.each(['user', 'system'] as const)('keeps %s literal', role => {
    const text = '**literal**\n  - item';
    const { container } = renderWithProviders(<MessageBubble message={{ id: 'm', role, content: text, timestamp: 0 }} />);
    expect(container.querySelector('strong, ul')).toBeNull();
    expect(screen.getByText('**literal**', { exact: false }).closest('div')).toHaveClass('whitespace-pre-wrap');
    expect(container.textContent).toBe(text);
  });
  it('renders clickable citations in formatted prose and tables, excluding code and links', () => {
    const onOpen = vi.fn();
    const { container } = renderWithProviders(<MarkdownContent text={'**Fact [1]**\n\n| Source |\n| --- |\n| [1] |\n\n`[1]`\n\n```\n[1]\n```\n\nSee [**[1]**](https://example.com) and [![alt](https://img.example/x)](https://outer.example)'} citations={[{ n: 1, chunk_id: 10, source_id: 's', title: 'Source', locator: '', excerpt: '', state: 'available' }]} onOpen={onOpen} />);
    expect(container.querySelector('strong [data-citation-marker]')).toBeTruthy();
    expect(container.querySelector('td [data-citation-marker]')).toBeTruthy();
    expect(container.querySelector('code button, a button, a a, img')).toBeNull();
    const buttons = screen.getAllByRole('button', { name: /Source 1/ });
    expect(buttons).toHaveLength(2);
    fireEvent.click(buttons[0]);
    expect(onOpen).toHaveBeenCalledWith(10, buttons[0]);
  });
});
