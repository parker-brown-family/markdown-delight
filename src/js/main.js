/* main.js — boot the theme engine, then build the initial workspace. */
import { initTheme } from './theme-engine.js';
import { Workspace } from './workspace.js';

initTheme();

const ws = new Workspace(
  document.getElementById('tabstrip'),
  document.getElementById('panes'),
);

/* Opening layout — the canonical markdown-delight shape:
   tab 1  →  [ source  |  live preview ]          (the editor)
   tab 2  →  [ files | [ source | preview ] ]     (with a file-tree sidebar) */
ws.addTab(
  { type: 'split', dir: 'row', sizes: [0.55, 0.45],
    children: [ws._make('editor', 'README.md'), ws._make('preview', 'preview')] },
  'README.md', true,
);
ws.addTab(
  { type: 'split', dir: 'row', sizes: [0.2, 0.8], children: [
    ws._make('filetree', 'files'),
    { type: 'split', dir: 'row', sizes: [0.55, 0.45],
      children: [ws._make('editor', 'PLAN.md'), ws._make('preview', 'preview')] },
  ] },
  'with sidebar', false,
);
ws.renderTabs();

window.__ws = ws;   // handy for console poking
