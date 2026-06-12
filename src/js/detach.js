/* ============================================================
   detach.js — break a pane out into its own OS window.
   Opens popout.html, which boots the theme engine and rebuilds the pane.
   Theme/seed/scale stay in lockstep via BroadcastChannel (see theme-engine).
   ============================================================ */
let n = 0;
export function detachPane(paneType = 'editor', title = 'pane') {
  const params = new URLSearchParams({ type: paneType, title });
  const features = 'width=720,height=460,menubar=no,toolbar=no,location=no,status=no';
  const win = window.open(`popout.html?${params}`, `md-popout-${++n}`, features);
  if (!win) alert('Pop-out blocked — allow popups for markdown-delight to break panes out.');
  return win;
}
