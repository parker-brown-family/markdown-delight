# markdown-delight — Build Plan v1

> A **tabful, tiling Linux Markdown editor** — open any `.md` file and it lands in a
> **gorgeous, themeable, native-snappy surface** instead of a flat gray pane — that
> can be **set as the system default Markdown editor**, runs as **snappy as a native
> app**, is **modifiable on the fly** (themes, effects, features at will), and is
> **open source, MIT, and shareable** with other Linux users.

This is the sibling of **terminal-delight**, and deliberately so. Terminal-delight already
won the hard, generic fights — *can GPUI host a polished, native-snappy, themeable tiling
surface on this exact box (X11 · NVIDIA · wgpu)?* — and proved them with a working G0a
substrate spike. markdown-delight **inherits that entire victory** and spends its risk
budget on the one thing that is genuinely new and genuinely harder here: **the editing
surface itself.**

The honest asymmetry, stated up front so the plan is built around it:

> Terminal-delight got to **outsource its hard core** — `alacritty_terminal` (Apache-2.0)
> is a complete, license-clean VT/grid/PTY engine. **markdown-delight does not get that
> gift.** The only mature, GPUI-native editing surface in our orbit is **Zed's `editor`
> crate — GPL-3.0-or-later — which is STUDY-ONLY for an MIT product.** So the editor core
> is *ours to build*. That is the project. Everything else (chrome, tabs, tiling, themes,
> packaging) is largely a port.

Decision taken (Parker, this session): **stay MIT, build our own editor core.** Full
ownership, no bolted-together spaghetti, shareable — at the cost of writing the
buffer/cursor/selection/undo engine ourselves. The GPL fast-path (reuse Zed's `editor`)
is recorded as the rejected fallback, not the plan.

---

## 1. North Star → measurable acceptance criteria (staged)

These are the 1.0 bars. **MVP 0.1 is NOT judged against all of them** — each row lists the
milestone where it starts being enforced.

| Pillar | Acceptance criterion | Enforced from |
|---|---|---|
| **Snappy like native** | Focused-pane keystroke→photon within **+2 ms of a baseline native editor** (gedit/kate), same box, instrumented | 0.1 (crude) → 1.0 (rigorous) |
| **Opens our files** | Registered system default for `text/markdown`; double-click any `.md`/`.markdown` opens *us*, ≤300 ms cold to first paint | 0.2 (registration smoke test) → 1.0 |
| **Big files don't choke** | 5 MB Markdown opens, scrolls, and edits without jank; rope-backed, no full-string reflow on keystroke | 1.0 (1 MB at 0.1) |
| **The look is free** | CRT-lite + theme effects add ≤1 ms GPU frame time, 0 added input-latency frames | 0.1 for CRT-lite; 0.4 for true shaders |
| **Polished like a web app** | Blind A/B reads top-tier; bar = Zed/Typora-grade | 0.1: "distinct visual identity"; 1.0: full bar |
| **Modify on the fly** | Theme/config changes hot-reload from data files, no recompile | **0.1 (day one — inherited from terminal-delight)** |
| **Edits are safe** | Never silently lose a buffer: atomic save, external-change detection, crash/restore of unsaved tabs | 0.2 → 1.0 |
| **Open & shareable** | One-command install; themes are portable data files; MIT throughout | 0.2 packaging smoke test; 1.0 full |

---

## 2. Architecture

```
markdown-delight (single Rust process, MIT)
├─ GPUI ──────── GPU-rendered chrome + tabs + tiling + panes (one render model)   ← INHERITED, de-risked
├─ editor core ─ OUR code: rope buffer · cursor/selection · undo · input · view   ← THE NEW WORK
│   ├─ buffer    : ropey (MIT) — rope text storage, O(log n) edits
│   ├─ highlight : tree-sitter + tree-sitter-md (MIT) — incremental syntax tree
│   └─ view      : GPUI elements — gutter, lines, cursor, selection (clean-room)
├─ markdown ──── comrak (BSD-2) CommonMark+GFM → AST → preview render             ← LIBRARY
├─ files ─────── open/save (atomic) · file watch · file-tree sidebar · MIME assoc
├─ theme ─────── hot-reloaded data files (palette/font/effect dials) — DAY ONE    ← INHERITED
└─ visuals ───── CRT-lite first (primitives); true post-process shader = research ← INHERITED ladder
```

### Inherited from terminal-delight (do not re-litigate)
- **Substrate: GPUI**, consumed from the pinned `zed-upstream` checkout via path deps
  (`../../zed-upstream/crates/gpui`, `gpui_platform` with `x11`,`wayland`). Terminal-delight's
  **G0a already proved** GPUI window + keyboard input + fixed-grid monospace text render on
  this box (X11 · NVIDIA · wgpu renderer). markdown-delight's G0a is therefore a **re-verify,
  not a discovery** — confirm our crate links and paints text; the substrate question is
  closed.
- **License strategy: MIT.** `gpui`/`gpui_platform` = Apache-2.0 ✅. **Zed's `editor`,
  `markdown`, `rope`, `text`, `language` crates = GPL-3.0-or-later → STUDY ONLY, never copy.**
  Clean-room rule (carried verbatim from terminal-delight): architectural facts may be learned
  from Zed (e.g. "wrap the buffer in a shared rope, drive an incremental tree-sitter parse,
  map the tree to highlight spans"); function bodies / identifiers / structure-level
  transcription may not. Write the editor from `ropey`/`tree-sitter`/`comrak` public API docs,
  not with Zed's `editor` source open. Obligations: ship `THIRD-PARTY-LICENSES` (cargo-about);
  `cargo deny check licenses` in CI with an MIT/Apache/BSD/ISC allowlist.
- **Chrome/tiling/themes:** the browser design reference (`index.html`, `src/`) is ported
  near-wholesale from terminal-delight — same 3-tier theme engine, same binary tiling tree,
  same tabs/splitters/detach gesture. The leaf-pane *content* changes (editor / preview /
  file-tree instead of terminal / panel / assistant); the workspace shell does not.
- **Visuals fallback ladder** (unchanged): GPUI + CRT-lite → GPUI + minimal shader patch →
  raw wgpu (last resort) → Ghostty-style shader research branch (parallel, not a product
  fallback).

### New decisions for markdown-delight

- **D1 — Editor core is ours (MIT).** No Apache/MIT drop-in GPUI editor widget exists; Zed's
  is GPL. We build buffer + cursor + selection + undo + input + view on GPUI primitives.
  - **Buffer:** `ropey` (MIT) — rope; O(log n) inserts/deletes; line indexing; the thing we
    must *not* hand-roll. (Alternatives `crop`, `xi-rope` noted in R-backlog; ropey is the
    default — most-used, best-documented.)
  - **Syntax:** `tree-sitter` + `tree-sitter-md` (MIT) — incremental parse; map nodes →
    highlight spans for the source pane. Same grammar family Zed/Neovim use, pulled directly
    (license-clean), not via Zed's GPL `language` crate.
  - **Undo:** our own ring of rope-diff transactions (coalesced by time + edit adjacency).
  - **What we explicitly DON'T build for 0.1:** multi-cursor, code folding, LSP, vim mode.
    Those are 1.0+ and must not bloat the core.

- **D2 — Preview engine: `comrak` (BSD-2-Clause).** CommonMark **+ GFM complete** out of the
  box: tables, task lists, strikethrough, autolinks, footnotes. Parse to AST, render the AST
  to GPUI elements (not to an HTML webview — we want native snappiness and full theme
  control, no embedded browser). `pulldown-cmark` (MIT, faster, streaming) is the recorded
  alternative if comrak's AST proves heavy; comrak is the default for GFM completeness. **No
  webview, ever** — that would betray the snappiness pillar and bolt on a framework.

- **D3 — Editing model: split source | live-preview for MVP; "Live Preview" hybrid is the
  signature upgrade; full WYSIWYG is a research branch.** Rationale (the award-winning,
  de-risked sequencing):
  - **0.1 — Split source ▸ preview.** Syntax-highlighted Markdown source pane beside a
    live-rendered preview pane. This **maps perfectly onto the inherited tiling tree** — a
    split *is* the product — ships fast, and is already a polished, distinct editor. Replaces
    MarkText's flat gray pane on day one of usefulness.
  - **0.4 — "Live Preview" hybrid (the delight).** Obsidian-class inline styling *in the
    source pane*: headings size up, **bold** renders bold, lists get bullets, links color —
    while the raw Markdown stays editable when the cursor is on the line. This is the seamless
    feel **without** the hardest problem (full reflow/layout of a WYSIWYG document model). It
    is the chosen middle path and the project's signature.
  - **Stretch — full seamless WYSIWYG (Typora/MarkText reflow).** The true hard core; earns
    its way in only if the hybrid proves insufficient. This is markdown-delight's equivalent
    of terminal-delight's "true post-process shader" research branch: identity is already
    delivered without it; it must justify its risk.

- **D4 — File handling is a first-class pillar, not an afterthought.** A text editor that
  loses a buffer is disqualified regardless of how it looks. Atomic save (write-temp +
  rename), external-change detection (`notify` crate, MIT), and crash-restore of unsaved tabs
  are gated milestones (D-pillar "Edits are safe"), not nice-to-haves.

- **D5 — Default-handler registration is the "set as default editor" requirement.** Ship a
  `markdown-delight.desktop` with `MimeType=text/markdown;text/x-markdown;` and a one-command
  `make-default` that runs `xdg-mime default markdown-delight.desktop text/markdown
  text/x-markdown` + `update-desktop-database`. This is what makes double-clicking a `.md`
  open *us* instead of MarkText. Packaged form (AppImage/Flatpak) registers on install.

---

## 3. Milestones

### MVP 0.1 — "two-pane markdown editor" ← THE TARGET
One native GPUI app · one tab · **source pane + live-preview pane** (split) · open a `.md`
from argv or a file dialog · **syntax-highlighted, rope-backed editable source** · live
comrak preview that re-renders as you type · **save** (atomic) · **theme-file hot reload**
(palette/font/effect dials) · CRT-lite, disable-able · basic window/layout restore.

**Success =** 30 minutes of real writing without rage-quitting; open a real README, edit it,
watch the preview track live, save, reopen externally-correct; typing feels at least as snappy
as MarkText; theme changes reload live; the app already has a distinct visual identity that
makes the old gray pane embarrassing.
**Test matrix:** this very `PLAN.md`, a GFM-heavy doc (tables/tasklists/footnotes), a 1 MB
generated file, a file with CJK + emoji, a file edited externally while open.

### Sub-gates to 0.1 (facts fast — boring-core never hostage to the shiny problem)
| Gate | Proves | Kill / fallback trigger |
|---|---|---|
| **G0a** | Our crate links `gpui`/`gpui_platform`; window opens; renders a `.md` file's text in the hacker palette (re-verify of terminal-delight's proven substrate) | GPUI won't build/paint from *our* crate on this box → re-pin rev / raise with terminal-delight |
| **G0b** | **Editor core spike** — ropey buffer + GPUI text view + cursor + insert/delete/backspace + selection. The new hard core. (This is the moral equivalent of terminal-delight's G0b "real shell".) | Input→buffer→render loop can't hit native feel on GPUI primitives → re-scope view layer / reconsider GPL editor fallback |
| **G0c** | Syntax highlight — tree-sitter-md incremental parse → colored spans in the source pane | tree-sitter integration intractable → fall back to regex-lite highlight for 0.1 |
| **G0d** | Live preview — comrak AST → GPUI element tree, re-rendered on edit (debounced); CRT-lite identity on both panes | comrak AST→GPUI too heavy per keystroke → debounce harder / switch to pulldown-cmark |
| **G0e** | File save (atomic) + open dialog + crude keystroke→present latency instrumentation | save corrupts / latency ≫ baseline and unfixable → file-layer or substrate rework |

### Then
- **0.2** — tabs · multiple open files · up to N split panes · **file-tree sidebar** ·
  **session restore** (serialize tab + tiling tree + open paths + cursor) · **atomic save +
  external-change detection + unsaved-crash restore** · **default-handler registration** +
  **packaging smoke test (AppImage or Flatpak)**. ← the "open any md through our app, as
  default" requirement lands here.
- **0.3** — detach pane → own OS window (inherited gesture) · find/replace · synced-scroll
  source↔preview · drag-a-pane gestures.
- **0.4** — **"Live Preview" hybrid** inline styling in the source pane (the signature
  delight) · custom shader support **iff** the terminal-delight R1 shader branch proved it
  (else CRT-lite deepens).
- **1.0** — large-file rigor (5 MB) · full latency rig vs baseline · multi-cursor · snippets ·
  theme gallery · public docs · full packaging + MIME registration on install. Full WYSIWYG
  reflow promoted only if the hybrid demanded it.

---

## 4. Research backlog (parallel, never blocking the build)

| # | Question | Feeds | Status |
|---|---|---|---|
| **R1** | Editor view on GPUI primitives: can we render gutter+lines+cursor+selection at native feel, and is per-line vs single-text-element the right shape? | G0b | **open — the #1 risk; spike it first** |
| **R2** | Rope choice: `ropey` vs `crop` vs `xi-rope` — line indexing, grapheme handling, edit perf at 5 MB | G0b, 1.0 | open (default `ropey`) |
| **R3** | Markdown render: comrak AST→GPUI elements vs pulldown-cmark streaming; cost per keystroke; debounce strategy | G0d | open (default `comrak`, debounced) |
| **R4** | Markdown correctness matrix: GFM tables/tasklists/footnotes, nested lists, fenced code + inner syntax highlight, math (KaTeX?), front-matter, embedded HTML, Unicode/CJK width, emoji | 0.1→1.0 | open |
| **R5** | "Live Preview" hybrid feasibility on our view layer — inline decoration model, cursor-reveals-raw-markup, performance | 0.4 | open |
| **R6** | Default-handler + packaging: AppImage vs Flatpak for a GPU Rust app; reliable `xdg-mime` default across GNOME/KDE/XFCE; sandbox file access (Flatpak portals) | 0.2 | open (inherits terminal-delight R7 half) |
| **R7** | Linux matrix: Wayland/X11 · NVIDIA/AMD/Intel · fractional scaling · clipboard + primary selection · IME for CJK input | G0a, 0.2 | open (terminal-delight supplied the NVIDIA/X11 half) |
| **R8** | Competitive: MarkText / Typora / Obsidian / Zed / Marktext-vs-us — *exactly why does someone switch?* | positioning, 1.0 | open |

---

## 5. Adversarial hardening

Terminal-delight's register (C1–C17) carries over where it applies (compositor-only effects,
data-vs-code iteration split, every milestone ships a working artifact, per-pane panic
isolation + session restore, themes as the no-Rust contribution path). markdown-delight-specific
critiques:

- **M1 (less leverage than the sibling — the central honesty).** We do **not** get an
  Apache full-editor gift. The editing surface is real, original work and the dominant risk.
  *Mitigation:* G0b spiked first; MVP scoped to split source+preview so the editing surface
  can be *minimal* and still ship a polished product; WYSIWYG deferred.
- **M2 (WYSIWYG tar-pit).** Seamless Typora-style reflow can swallow the whole timeline.
  *Mitigation:* it is explicitly **out** of 0.1; the "Live Preview" hybrid (0.4) delivers the
  feel at a fraction of the cost; full reflow must earn its way in.
- **M3 (data-loss = instant disqualification).** A beautiful editor that drops a buffer is
  worthless. *Mitigation:* "Edits are safe" is a gated pillar — atomic save, external-change
  detect, crash-restore — not a polish-phase afterthought.
- **M4 (webview temptation).** The fast way to render Markdown is an embedded browser; it
  would betray snappiness and bolt on exactly the spaghetti framework Parker rejects.
  *Mitigation:* **hard no on webview** — comrak AST → native GPUI elements only (D2).
- **M5 (big-file cliff).** Naïve String buffers reflow the whole document per keystroke and
  die at a few MB. *Mitigation:* rope from line one (D1/R2); 5 MB is a 1.0 acceptance bar
  with a 1 MB checkpoint at 0.1.
- **M6 (default-handler is the actual user requirement).** "Opens nicer" is worthless if
  double-click still routes to MarkText. *Mitigation:* MIME registration is a named 0.2
  deliverable with a smoke test (D5), not assumed.
- **M7 (clean-room slip).** Zed's GPL `editor`/`markdown` crates are *right there* and
  tempting to peek at while writing ours. *Mitigation:* the clean-room rule is binding; build
  from `ropey`/`tree-sitter`/`comrak` docs; `cargo deny` + license CI enforce the boundary.

---

## 6. Status

- Plan v1 adopted. Repo scaffolded this session: **browser design reference** (`index.html`,
  `src/` — ported theme engine + tiling chrome + live source▸preview demo) as the design
  reference for theme tokens, effect dials, chrome UX, and the editor/preview/file-tree pane
  shapes — **not** foundation code; and the **native app skeleton** (`app/`, reusing the
  pinned `gpui`) with the **G0a spike** standing.
- Next concrete step: **G0b** — the editor-core spike (ropey buffer + GPUI text view + cursor
  + edit loop), with R1–R3 running in parallel. That is the gate that decides everything.
</content>
</invoke>
