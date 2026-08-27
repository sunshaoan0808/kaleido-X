/**
 * foliate-reader/markdown.js — Markdown→HTML converter + Book factory (Phase 1.2 split)
 * Pure functions: mdToHtml(md), makeBookFromHTML(fullHtml, title, tocItems)
 */
// ─── Markdown → HTML converter ──────────────────────────────────────────────

export function mdToHtml(md) {
  if (!md || typeof md !== 'string') return { html: '<p>无内容</p>', toc: [] };

  let html = md
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');

  const toc = [];
  let headingIndex = 0;
  html = html.replace(/^(#{1,6})\s+(.+)$/gm, (m, hashes, text) => {
    const level = hashes.length;
    const id = `heading-${headingIndex}`;
    const label = text.trim();
    toc.push({ level, id, label });
    headingIndex++;
    return `<h${level} id="${id}">${label}</h${level}>`;
  });

  html = html.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  html = html.replace(/(?<!\*)\*([^*]+)\*(?!\*)/g, '<em>$1</em>');
  html = html.replace(/`([^`]+)`/g, '<code>$1</code>');
  html = html.replace(/```(\w*)\n([\s\S]*?)```/g, '<pre><code>$2</code></pre>');
  html = html.replace(/^>\s+(.+)$/gm, '<blockquote>$1</blockquote>');
  html = html.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, '<img src="$2" alt="$1" loading="lazy">');
  html = html.replace(/(?<!!)\[([^\]]*)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');

  // ── Footnotes: collect definitions, convert inline refs ──────────────
  const footnotes = {};
  html = html.replace(/^\[\^([^\]]+)\]:\s*(.+)$/gm, (m, label, content) => {
    footnotes[label] = content.trim();
    return '';
  });
  html = html.replace(/\[\^([^\]]+)\]/g, (m, label) => {
    if (footnotes[label]) {
      return `<sup><a href="#fn-${label}" class="footnote-ref" data-footnote="${label}">${label}</a></sup>`;
    }
    return m;
  });
  if (Object.keys(footnotes).length) {
    const fnList = Object.entries(footnotes)
      .map(([label, content]) => `<li id="fn-${label}">${content} <a href="#fnref-${label}">↩</a></li>`)
      .join('\n');
    html += `\n<hr class="footnotes-sep">\n<ol class="footnotes">\n${fnList}\n</ol>`;
  }

  let inList = false, inOrderedList = false;
  const lines = html.split('\n');
  const out = [];
  for (const line of lines) {
    const ulMatch = line.match(/^(\s*)[-*+]\s+(.+)$/);
    const olMatch = line.match(/^(\s*)\d+\.\s+(.+)$/);
    const isHeading = /^<h\d>/.test(line);
    const isBlock = /^<(pre|blockquote)/.test(line);
    if (ulMatch) {
      if (inOrderedList) { out.push('</ol>'); inOrderedList = false; }
      if (!inList) { out.push('<ul>'); inList = true; }
      out.push(`<li>${ulMatch[2]}</li>`);
    } else if (olMatch) {
      if (inList) { out.push('</ul>'); inList = false; }
      if (!inOrderedList) { out.push('<ol>'); inOrderedList = true; }
      out.push(`<li>${olMatch[2]}</li>`);
    } else {
      if (inList) { out.push('</ul>'); inList = false; }
      if (inOrderedList) { out.push('</ol>'); inOrderedList = false; }
      if (isHeading || isBlock) out.push(line);
      else if (line.trim()) out.push(`<p>${line}</p>`);
    }
  }
  if (inList) out.push('</ul>');
  if (inOrderedList) out.push('</ol>');
  html = out.join('\n');

  return { html: `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  body {
    font-family: Georgia, "Noto Serif", "Source Han Serif SC", serif;
    font-size: 16px; line-height: 1.8; padding: 24px 32px;
    max-width: 720px; margin: 0 auto;
    color: var(--theme-text, #333);
    word-wrap: break-word;
  }
  h1 { font-size: 1.8em; margin: 1.5em 0 0.5em; text-align: center; }
  h2 { font-size: 1.5em; margin: 1.2em 0 0.5em; border-bottom: 1px solid var(--theme-border, #eee); padding-bottom: 0.3em; }
  h3 { font-size: 1.3em; margin: 1em 0 0.4em; }
  h4 { font-size: 1.15em; margin: 0.8em 0 0.3em; }
  p { margin: 0.8em 0; text-indent: 2em; }
  p:first-child { text-indent: 0; }
  ul, ol { margin: 0.5em 0 0.5em 1.5em; }
  li { margin: 0.3em 0; }
  strong { font-weight: 700; }
  em { font-style: italic; }
  pre { background: var(--theme-toolbar, #f5f5f5); padding: 1em; border-radius: 6px; overflow-x: auto; font-size: 0.9em; line-height: 1.4; }
  code { background: var(--theme-toolbar, #f0f0f0); padding: 0.15em 0.4em; border-radius: 3px; font-family: "SF Mono", monospace; font-size: 0.9em; }
  pre code { background: transparent; padding: 0; }
  blockquote { border-left: 4px solid var(--theme-border, #ccc); padding: 0.5em 1em; margin: 1em 0; color: var(--theme-text, #555); background: var(--theme-toolbar, #f9f9f9); }
  img { max-width: 100%; height: auto; border-radius: 4px; margin: 1em 0; }
  a { color: var(--theme-accent, #1a73e8); text-decoration: none; }
  a:hover { text-decoration: underline; }
  table { border-collapse: collapse; width: 100%; margin: 1em 0; }
  th, td { border: 1px solid var(--theme-border, #ddd); padding: 8px 12px; text-align: left; }
  th { background: var(--theme-toolbar, #f5f5f5); font-weight: 600; }
  hr { border: none; border-top: 1px solid var(--theme-border, #ddd); margin: 2em 0; }
  ::-webkit-scrollbar { width: 6px; }
  ::-webkit-scrollbar-thumb { background: var(--theme-border, #ccc); border-radius: 3px; }
  /* Footnotes */
  sup.footnote-ref a { text-decoration: none; color: var(--theme-accent, #1a73e8); font-size: 0.85em; }
  .footnotes-sep { margin: 2em 0 1em; }
  .footnotes { font-size: 0.9em; color: var(--theme-text, #555); }
  .footnotes li { margin: 0.5em 0; }
  .footnotes li:target { background: rgba(26,115,232,0.1); padding: 0.3em 0.5em; border-radius: 4px; }
</style>
</head>
<body>${html}</body>
</html>`, toc };
}

// ─── Book object factory ────────────────────────────────────────────────────

export function makeBookFromHTML(fullHtml, title, tocItems) {
  let _blobUrl = null;
  const label = title || '正文';
  const toc = tocItems && tocItems.length > 0
    ? tocItems.map((item, i) => ({
        href: `section-1#${item.id}`,
        label: item.label,
        subitems: [],
      }))
    : [{ href: 'section-1', label }];
  return {
    metadata: { title: title || '未知书名', language: 'zh-CN' },
    sections: [{
      id: 'section-1', href: 'section-1', label,
      load: async () => {
        const blob = new Blob([fullHtml], { type: 'text/html; charset=utf-8' });
        _blobUrl = URL.createObjectURL(blob);
        return _blobUrl;
      },
      unload: () => { if (_blobUrl) { URL.revokeObjectURL(_blobUrl); _blobUrl = null; } },
    }],
    toc,
    resolveHref(href) {
      if (href && typeof href === 'object') return href; // 已是 {index, anchor} 目标
      const index = this.sections.findIndex(s => typeof href === 'string' && href.split('#')[0] === s.href);
      if (index === -1) return { index: 0 };
      let anchor;
      const hash = typeof href === 'string' && href.includes('#') ? href.split('#')[1] : null;
      if (hash) anchor = doc => doc.getElementById(hash);
      return { index, anchor };
    },
  };
}
