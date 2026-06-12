/* ============================================================
   markdown.js — a small CommonMark-subset + GFM renderer for the
   BROWN browser design reference only.

   This is intentionally compact and is NOT the product renderer. In the
   native app this whole job is done by `comrak` (BSD-2, CommonMark + GFM
   complete) producing an AST that is painted as GPUI elements — no webview,
   no innerHTML. See docs/PLAN.md §2 D2. This file exists so the reference
   can show a live, theme-correct source ▸ preview without any build step.

   Supported here: headings, bold/italic/strikethrough/inline-code/links,
   fenced code blocks, blockquotes, ordered/unordered/task lists, GFM tables,
   horizontal rules, paragraphs. Good enough to look real.
   ============================================================ */

// escape only & and < — a literal > renders fine as text and keeping it lets
// block detection (blockquote `>`) run on the escaped buffer.
const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;');

/* inline span parsing — runs on already-escaped text */
function inline(text) {
  return text
    .replace(/`([^`]+)`/g, (_, c) => `<code>${c}</code>`)
    .replace(/!\[([^\]]*)\]\(([^)\s]+)[^)]*\)/g, (_, a, h) => `<img alt="${a}" src="${h}">`)
    .replace(/\[([^\]]+)\]\(([^)\s]+)[^)]*\)/g, (_, t, h) => `<a href="${h}">${t}</a>`)
    .replace(/\*\*([^*]+)\*\*/g, (_, c) => `<strong>${c}</strong>`)
    .replace(/__([^_]+)__/g, (_, c) => `<strong>${c}</strong>`)
    .replace(/(^|[^*])\*([^*\n]+)\*/g, (_, p, c) => `${p}<em>${c}</em>`)
    .replace(/(^|[^_])_([^_\n]+)_/g, (_, p, c) => `${p}<em>${c}</em>`)
    .replace(/~~([^~]+)~~/g, (_, c) => `<del>${c}</del>`);
}

function tableRow(line, cell = 'td') {
  const cells = line.replace(/^\||\|$/g, '').split('|');
  return '<tr>' + cells.map((c) => `<${cell}>${inline(c.trim())}</${cell}>`).join('') + '</tr>';
}

export function renderMarkdown(src) {
  const lines = esc(src).replace(/\r\n/g, '\n').split('\n');
  const out = [];
  let i = 0;
  const listStack = []; // {tag}
  const closeLists = () => { while (listStack.length) out.push(`</${listStack.pop().tag}>`); };

  while (i < lines.length) {
    let line = lines[i];

    // fenced code block
    const fence = line.match(/^\s*```(.*)$/);
    if (fence) {
      closeLists();
      const buf = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i++]);
      i++; // closing fence
      const lang = fence[1].trim();
      out.push(`<pre><code${lang ? ` data-lang="${lang}"` : ''}>${buf.join('\n')}</code></pre>`);
      continue;
    }

    // horizontal rule
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) { closeLists(); out.push('<hr>'); i++; continue; }

    // heading
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) { closeLists(); const n = h[1].length; out.push(`<h${n}>${inline(h[2].trim())}</h${n}>`); i++; continue; }

    // blockquote (greedy consecutive)
    if (/^\s*>\s?/.test(line)) {
      closeLists();
      const buf = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) buf.push(lines[i++].replace(/^\s*>\s?/, ''));
      out.push(`<blockquote>${inline(buf.join(' '))}</blockquote>`);
      continue;
    }

    // GFM table: header row + delimiter row
    if (/\|/.test(line) && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|[\s:|-]*$/.test(lines[i + 1]) && /-/.test(lines[i + 1])) {
      closeLists();
      const head = tableRow(line, 'th');
      i += 2;
      const body = [];
      while (i < lines.length && /\|/.test(lines[i]) && lines[i].trim() !== '') body.push(tableRow(lines[i++]));
      out.push(`<table><thead>${head}</thead><tbody>${body.join('')}</tbody></table>`);
      continue;
    }

    // lists (unordered / ordered / task)
    const li = line.match(/^(\s*)([-*+]|\d+\.)\s+(.*)$/);
    if (li) {
      const ordered = /\d+\./.test(li[2]);
      const tag = ordered ? 'ol' : 'ul';
      if (!listStack.length || listStack[listStack.length - 1].tag !== tag) {
        closeLists();
        listStack.push({ tag });
        out.push(`<${tag}>`);
      }
      let item = li[3];
      const task = item.match(/^\[([ xX])\]\s+(.*)$/);
      if (task) {
        const checked = task[1].toLowerCase() === 'x' ? ' checked' : '';
        out.push(`<li><input type="checkbox" disabled${checked}>${inline(task[2])}</li>`);
      } else {
        out.push(`<li>${inline(item)}</li>`);
      }
      i++;
      continue;
    }

    // blank line
    if (line.trim() === '') { closeLists(); i++; continue; }

    // paragraph (merge consecutive non-structural lines)
    closeLists();
    const para = [line];
    i++;
    while (i < lines.length && lines[i].trim() !== '' &&
           !/^(#{1,6}\s|\s*>|\s*```|\s*([-*+]|\d+\.)\s|\s*([-*_])(\s*\3){2,}\s*$)/.test(lines[i])) {
      para.push(lines[i++]);
    }
    out.push(`<p>${inline(para.join(' '))}</p>`);
  }
  closeLists();
  return out.join('\n');
}
