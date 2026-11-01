import { createElement, type ReactNode } from 'react';
import ReactMarkdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { ExternalLink } from 'lucide-react';
import type { ChatCitation } from '@/lib/knowledge-citations';
import { CitedText, type OpenCitation } from './CitationChips';

/** Require an explicit scheme: relative and protocol-relative URLs are never links. */
function allowedUrl(value: string | undefined, image = false): URL | undefined {
  if (!value || !/^(https?:\/\/|mailto:)/i.test(value) || /[\u0000-\u0020\u007f]/.test(value)) return;
  try {
    const url = new URL(value);
    if (url.protocol === 'http:' || url.protocol === 'https:' || (!image && url.protocol === 'mailto:')) return url;
  } catch { /* Incomplete streaming URLs remain text. */ }
}

type TextNode = { type: string; value?: string; properties?: { alt?: unknown }; children?: TextNode[] };
function labelText(node: TextNode): string {
  if (node.type === 'text') return node.value ?? '';
  if (node.properties?.alt) return String(node.properties.alt);
  return node.children?.map(labelText).join('') ?? '';
}

const linkClass = 'break-all text-primary underline underline-offset-2 outline-none focus-visible:ring-3 focus-visible:ring-ring/50';

/** Untrusted assistant Markdown. HTML is skipped and images never create network requests. */
export function MarkdownContent({ text, citations, onOpen }: {
  text: string;
  citations?: readonly ChatCitation[];
  onOpen?: OpenCitation;
}) {
  const cite = (children: ReactNode): ReactNode => Array.isArray(children)
    ? children.map((child, i) => typeof child === 'string' && citations?.length && onOpen
      ? <CitedText key={i} text={child} citations={citations} onOpen={onOpen} /> : child)
    : typeof children === 'string' && citations?.length && onOpen
      ? <CitedText text={children} citations={citations} onOpen={onOpen} /> : children;
  const components: Components = {
    p: ({ children, node }) => {
      const only = node?.children.length === 1 ? node.children[0] : undefined;
      const href = only?.type === 'element' && only.tagName === 'a' ? String(only.properties.href ?? '') : undefined;
      const url = allowedUrl(href, true);
      if (url && only?.type === 'element') {
        const label = labelText(only);
        return <p><a href={href} target="_blank" rel="noopener noreferrer" title={href}
          aria-label={`${url.hostname}${label !== href ? ` · ${label}` : ''} · ${href}`}
          className="inline-flex max-w-full items-center gap-2 rounded-md border border-surface-border bg-secondary px-3 py-2 text-primary outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50">
          <ExternalLink className="size-4 shrink-0" aria-hidden="true" />
          <span className="min-w-0 break-all">{url.hostname}{label !== href && <> · {label}</>}</span>
        </a></p>;
      }
      return <p>{cite(children)}</p>;
    },
    // Flatten link labels so image fallbacks and citation buttons cannot nest inside anchors.
    a: ({ href, node }) => {
      const url = allowedUrl(href);
      if (!url) return <>{node && labelText(node)}</>;
      const label = node ? labelText(node) : '';
      const comparableHref = url.protocol === 'mailto:' ? url.pathname : href?.replace(/\/$/, '');
      const comparableLabel = label.replace(/\/$/, '');
      const showHostname = url.protocol !== 'mailto:' && comparableLabel !== comparableHref;
      return <a href={href} target="_blank" rel="noopener noreferrer" title={href} className={linkClass}>
        {label}{showHostname && <> <span className="text-muted-foreground text-xs">({url.hostname})</span></>}
      </a>;
    },
    img: ({ src, alt }) => {
      const href = typeof src === 'string' ? src : undefined;
      const url = allowedUrl(href, true);
      return url ? <a href={href} target="_blank" rel="noopener noreferrer" title={href} className={linkClass}>{alt || url.hostname}</a> : <>{alt}</>;
    },
    table: ({ children }) => <div className="my-2 max-w-full overflow-x-auto"><table className="w-full border-collapse text-sm">{children}</table></div>,
    pre: ({ children }) => <pre className="my-2 max-w-full overflow-x-auto rounded-md bg-muted p-3 text-xs [&_code]:bg-transparent [&_code]:p-0">{children}</pre>,
    code: ({ children }) => <code className="rounded bg-muted px-1 py-0.5 font-mono text-[0.85em]">{children}</code>,
  };
  // Apply citations only to prose leaves, never code or interactive link labels.
  Object.assign(components, Object.fromEntries(
    ['strong', 'em', 'del', 'li', 'th', 'td', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'blockquote'].map(tag => [
      tag, ({ children }: { children?: ReactNode }) => createElement(tag, null, cite(children)),
    ]),
  ));
  return <div className="min-w-0 max-w-full break-words text-sm leading-relaxed [&_p]:my-1.5 [&_p:first-child]:mt-0 [&_p:last-child]:mb-0 [&_ul]:my-1.5 [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:my-1.5 [&_ol]:list-decimal [&_ol]:pl-5 [&_li]:my-0.5 [&_h1]:text-lg [&_h2]:text-base [&_h3]:text-sm [&_h1]:font-semibold [&_h2]:font-semibold [&_h3]:font-semibold [&_h4]:font-semibold [&_h5]:font-semibold [&_h6]:font-semibold [&_h1]:my-2 [&_h2]:my-2 [&_h3]:my-2 [&_h4]:my-2 [&_h5]:my-2 [&_h6]:my-2 [&_blockquote]:my-2 [&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground [&_hr]:my-3 [&_hr]:border-border [&_th]:border [&_th]:border-border [&_th]:bg-muted [&_th]:px-2 [&_th]:py-1 [&_td]:border [&_td]:border-border [&_td]:px-2 [&_td]:py-1">
    <ReactMarkdown skipHtml remarkPlugins={[remarkGfm]} urlTransform={value => allowedUrl(value) ? value : ''} components={components}>{text}</ReactMarkdown>
  </div>;
}
