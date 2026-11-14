import DOMPurify from 'dompurify';
import { marked } from 'marked';

/** Renders a package README to HTML that is safe to insert into the page. */
export function renderMarkdown(source: string): string {
  const html = marked.parse(source, { async: false, gfm: true });
  return DOMPurify.sanitize(html, {
    // A README comes from whoever published the package; allow text formatting only.
    FORBID_TAGS: ['style', 'form', 'input', 'button', 'iframe', 'video', 'audio'],
    FORBID_ATTR: ['style', 'class', 'id'],
  });
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} kB`;
  }
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function formatDate(date: Date | null): string {
  return date ? date.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : '';
}
