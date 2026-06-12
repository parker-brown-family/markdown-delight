/* ============================================================
   panes.js — content factories for leaf panes.
   Each returns { el, title, focus }. el is cached & reattached across
   re-renders so editor text / scroll survive splits & moves.

   markdown-delight leaf types (vs terminal-delight's terminal/panel/assistant):
     • editor   — syntax-ish source surface (a <textarea> in the reference;
                  the rope-backed GPUI editor core in the native app)
     • preview  — live-rendered Markdown (renderMarkdown here; comrak→GPUI native)
     • filetree — workspace file list (mock here; real fs walk native)

   A single shared `doc` links every editor to every preview, so the canonical
   source ▸ preview demo updates live as you type. (The native app scopes a doc
   per tab; one shared doc is plenty to show the look in a zero-build reference.)
   ============================================================ */
import { renderMarkdown } from './markdown.js';

let seq = 0;
const uid = () => `pane-${++seq}`;

const SAMPLE = `# markdown-delight

Open any \`.md\` file and it lands *here* — a themeable, native-snappy,
tabful, tiling editor — instead of a flat gray pane.

## Why it exists

The old default editor opens Markdown into a lifeless gray box. This is the
**same delight** as our terminal: phosphor themes, CRT-lite, compositor-fast.

> Same engine as terminal-delight. Different leaf content.

### What you're looking at

- A **source** pane (left) and a **live preview** pane (right)
- Type on the left — the preview tracks you in real time
- Switch theme & seed colour from the ◉ badge, top-right

### Task list (GFM)

- [x] Port the theme engine
- [x] Port the tiling / tabs chrome
- [x] Live source ▸ preview demo
- [ ] Build the rope-backed editor core (native, **G0b** — next)
- [ ] Register as the system default \`.md\` handler

### A table, because we can

| Pillar        | Status        |
|---------------|---------------|
| Snappy        | native target |
| Themeable     | inherited ✅   |
| Set as default| 0.2           |

\`\`\`rust
// the native app, in spirit
fn main() {
    application().run(open_window);
}
\`\`\`

See \`docs/PLAN.md\` for the full build plan.
`;

/* shared live document — every editor writes it, every preview renders it */
const doc = { text: SAMPLE };
const previews = new Set();
const notify = () => previews.forEach((fn) => fn());

/* ---------------- editor (source surface) ---------------- */
function createEditor(label) {
  const el = document.createElement('div');
  el.className = 'editor';

  const gutter = document.createElement('div');
  gutter.className = 'gutter';

  const ta = document.createElement('textarea');
  ta.className = 'src';
  ta.spellcheck = false;
  ta.autocapitalize = 'off';
  ta.setAttribute('aria-label', 'markdown source');
  ta.value = doc.text;

  const syncGutter = () => {
    const n = ta.value.split('\n').length;
    let s = '';
    for (let i = 1; i <= n; i++) s += i + '\n';
    gutter.textContent = s;
  };
  syncGutter();

  ta.addEventListener('input', () => { doc.text = ta.value; syncGutter(); notify(); });
  ta.addEventListener('scroll', () => { gutter.scrollTop = ta.scrollTop; });
  // soft-tab: 2 spaces, keep editing feeling intentional
  ta.addEventListener('keydown', (e) => {
    if (e.key === 'Tab') {
      e.preventDefault();
      const s = ta.selectionStart, en = ta.selectionEnd;
      ta.value = ta.value.slice(0, s) + '  ' + ta.value.slice(en);
      ta.selectionStart = ta.selectionEnd = s + 2;
      doc.text = ta.value; syncGutter(); notify();
    }
  });

  el.append(gutter, ta);
  return { el, title: label || 'untitled.md', focus: () => ta.focus() };
}

/* ---------------- preview (rendered markdown) ---------------- */
function createPreview(label) {
  const el = document.createElement('div');
  el.className = 'preview';
  const render = () => { el.innerHTML = renderMarkdown(doc.text); };
  previews.add(render);
  render();
  return { el, title: label || 'preview', focus: () => {} };
}

/* ---------------- file-tree (mock workspace) ---------------- */
const TREE = [
  ['dir', '▾', 'markdown-delight', 0, false],
  ['file', '·', 'README.md', 1, false],
  ['dir', '▾', 'docs', 1, false],
  ['file', '·', 'PLAN.md', 2, true],
  ['dir', '▾', 'src', 1, false],
  ['file', '·', 'index.html', 2, false],
  ['file', '·', 'styles/theme.css', 2, false],
  ['file', '·', 'js/markdown.js', 2, false],
];
function createFiletree(label) {
  const el = document.createElement('div');
  el.className = 'filetree';
  TREE.forEach(([kind, ico, name, indent, active]) => {
    const row = document.createElement('div');
    row.className = `ft-row ${kind === 'dir' ? 'dir' : ''} indent-${indent}${active ? ' is-active' : ''}`;
    row.innerHTML = `<span class="ft-ico">${kind === 'dir' ? ico : '▪'}</span><span>${name}</span>`;
    el.appendChild(row);
  });
  return { el, title: label || 'files', focus: () => {} };
}

const FACTORIES = { editor: createEditor, preview: createPreview, filetree: createFiletree };

export function makeContent(paneType = 'editor', label) {
  const make = FACTORIES[paneType] || createEditor;
  const c = make(label);
  return { id: uid(), paneType, ...c };
}
