//! comments.rs — the comment-mode model + storage seam (Phase A).
//!
//! Google-Docs-style review comments for a doc, kept DELIBERATELY off the user's
//! disk-of-record: threads persist to `~/.config/markdown-delight/comments/<key>.json`
//! keyed by the doc's canonical path — never beside the `.md`, so there is nothing
//! to accidentally commit. The optional sidecar export (Phase E) additionally
//! guards itself via `.git/info/exclude` (see `ensure_git_ignored`).
//!
//! Anchoring survives edits: each thread remembers a content fingerprint of its
//! block plus the exact quoted text, and `reanchor` re-locates it after every
//! reparse — when the text is gone the thread is marked `deprecated` (kept, never
//! silently dropped). This module owns NO UI; it is the clean, plugin-ready core.

#![allow(dead_code)] // surface fills in across phases B–E; keep the model complete

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// Where a thread attaches in the document.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Anchor {
    /// fingerprint of the target block's normalized plain text
    pub block_fp: u64,
    /// nth block sharing that fingerprint (disambiguates duplicate blocks)
    #[serde(default)]
    pub block_ord: usize,
    /// char offsets within the block for a range comment; `None` = whole block
    #[serde(default)]
    pub range: Option<(usize, usize)>,
    /// the exact selected/blocked text — drives the magnifier + re-anchoring
    #[serde(default)]
    pub quote: String,
}

impl Anchor {
    pub fn whole_block(fp: u64, ord: usize, quote: String) -> Self {
        Self {
            block_fp: fp,
            block_ord: ord,
            range: None,
            quote,
        }
    }
    pub fn span(fp: u64, ord: usize, range: (usize, usize), quote: String) -> Self {
        Self {
            block_fp: fp,
            block_ord: ord,
            range: Some(range),
            quote,
        }
    }
    pub fn is_range(&self) -> bool {
        self.range.is_some()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub body: String,
    /// millis since the Unix epoch
    pub ts: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Thread {
    pub id: String,
    pub anchor: Anchor,
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub resolved: bool,
    /// set by `reanchor` when the anchored text can no longer be found
    #[serde(default)]
    pub deprecated: bool,
}

/// All threads for one document.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct CommentStore {
    #[serde(default)]
    pub threads: Vec<Thread>,
    /// per-session monotonic id counter (not persisted)
    #[serde(skip)]
    seq: u64,
}

/// Anchor-relevant view of a top-level block, built by `render::parse_with_meta`.
#[derive(Clone, Debug)]
pub struct BlockMeta {
    pub fp: u64,
    pub plain: String,
    /// source byte range of the block in the buffer (for future source bridging)
    pub src: std::ops::Range<usize>,
}

// ── free helpers ──────────────────────────────────────────────────────────

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Collapse all runs of whitespace to single spaces and trim — the basis for a
/// fingerprint that survives reflow/indent churn but changes on real edits.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stable content fingerprint of a block's text.
pub fn fingerprint(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    normalize(text).hash(&mut h);
    h.finish()
}

/// A stable key for the doc's comment file: hash of its canonical absolute path,
/// or a `scratch:<id>` basis for unsaved notebooks.
pub fn doc_key(path: Option<&Path>, scratch_id: &str) -> String {
    let basis = match path {
        Some(p) => fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned(),
        None => format!("scratch:{scratch_id}"),
    };
    let mut h = DefaultHasher::new();
    basis.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Best-effort comment author: `git config user.name`, else `$USER`, else "anon".
pub fn default_author() -> String {
    if let Ok(out) = Command::new("git").args(["config", "user.name"]).output() {
        if out.status.success() {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anon".into())
}

fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/markdown-delight")
}

fn store_path(key: &str) -> PathBuf {
    config_dir().join("comments").join(format!("{key}.json"))
}

// ── git-safety guard (Phase E helper, unit-tested in Phase A) ──────────────

/// Walk up from `start` to the enclosing repo's `.git` directory, if any.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        let g = cur.join(".git");
        if g.is_dir() {
            return Some(g);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Ensure each pattern is locally git-ignored for the repo containing `near`, by
/// appending to `.git/info/exclude` — a per-clone, itself-untracked ignore file.
/// Idempotent; a no-op (returns `Ok(false)`) outside a repo or when all patterns
/// are already present. This is what makes exported comments *un-committable*
/// without ever touching the user's tracked `.gitignore`.
pub fn ensure_git_ignored(near: &Path, patterns: &[&str]) -> std::io::Result<bool> {
    let Some(git_dir) = find_git_dir(near) else {
        return Ok(false);
    };
    let exclude = git_dir.join("info").join("exclude");
    let existing = fs::read_to_string(&exclude).unwrap_or_default();
    let missing: Vec<&str> = patterns
        .iter()
        .copied()
        .filter(|p| !existing.lines().any(|l| l.trim() == *p))
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    if let Some(dir) = exclude.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# markdown-delight: local-only comment sidecars (never committed)\n");
    for p in missing {
        out.push_str(p);
        out.push('\n');
    }
    fs::write(&exclude, out)?;
    Ok(true)
}

/// Export a doc's comments to a sidecar `<file>.comments.json` beside it, and
/// — if it's inside a repo — append the ignore patterns to `.git/info/exclude`
/// so the sidecar (and any project comments dir) can never be committed.
/// Returns the sidecar path written.
pub fn export_sidecar(doc_path: &Path, store: &CommentStore) -> std::io::Result<PathBuf> {
    let name = doc_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".into());
    let side = doc_path.with_file_name(format!("{name}.comments.json"));
    let json = serde_json::to_string_pretty(store).unwrap_or_default();
    fs::write(&side, json)?;
    // local-only guard — never touches the tracked .gitignore
    let _ = ensure_git_ignored(doc_path, &["*.comments.json", ".markdown-delight/"]);
    Ok(side)
}

// ── "copy with comments": the agent-facing review artifact ──────────────────

/// Resolve a thread's anchor to its current block index (mirrors `reanchor`'s
/// fingerprint-then-ordinal logic).
fn block_index_for(meta: &[BlockMeta], a: &Anchor) -> Option<usize> {
    let same: Vec<usize> = meta
        .iter()
        .enumerate()
        .filter(|(_, b)| b.fp == a.block_fp)
        .map(|(i, _)| i)
        .collect();
    same.get(a.block_ord)
        .copied()
        .or_else(|| same.first().copied())
}

/// A one-line, length-capped form of an anchor's quote (for "on …" headers).
fn short_quote(quote: &str) -> Option<String> {
    let q = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    if q.is_empty() {
        return None;
    }
    Some(if q.chars().count() > 80 {
        format!("{}…", q.chars().take(80).collect::<String>())
    } else {
        q
    })
}

/// Render one thread as a Markdown blockquote callout (no trailing newline).
fn render_thread(t: &Thread, orphan: bool) -> String {
    let author = t
        .comments
        .first()
        .map(|c| c.author.as_str())
        .unwrap_or("reviewer");
    let mut tags = String::new();
    if t.resolved {
        tags.push_str(" ✓ resolved");
    }
    if orphan {
        tags.push_str(" ⚠ orphaned");
    }
    // a range comment (or any orphan) names the text it referred to
    let on = if t.anchor.is_range() || orphan {
        short_quote(&t.anchor.quote).map(|q| format!(" on “{q}”"))
    } else {
        None
    };

    let mut lines: Vec<String> = vec![format!(
        "💬 **{author}**{}{}:",
        on.unwrap_or_default(),
        tags
    )];
    for (i, c) in t.comments.iter().enumerate() {
        if i > 0 {
            lines.push(String::new()); // blank line between replies
            lines.push(format!("↳ **{}**:", c.author));
        }
        for l in c.body.lines() {
            lines.push(l.to_string());
        }
    }
    lines
        .iter()
        .map(|l| {
            if l.is_empty() {
                ">".into()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build **"the document, with my comments"** — the original markdown verbatim,
/// with every live thread injected as a `> 💬` blockquote right after the block
/// it annotates (range comments quote the exact span). Threads whose anchored
/// text was edited away are appended in a trailing section so no feedback is ever
/// lost. A leading HTML comment tells the receiving agent what it's looking at.
pub fn review_markdown(source: &str, meta: &[BlockMeta], store: &CommentStore) -> String {
    let live: Vec<&Thread> = store.threads.iter().filter(|t| !t.deprecated).collect();

    // bucket threads by target block (BTreeMap → ascending byte order)
    let mut per_block: std::collections::BTreeMap<usize, Vec<&Thread>> = Default::default();
    let mut orphans: Vec<&Thread> = Vec::new();
    for t in &live {
        match block_index_for(meta, &t.anchor) {
            Some(i) => per_block.entry(i).or_default().push(t),
            None => orphans.push(t),
        }
    }

    let n = live.len();
    let mut out = format!(
        "<!-- markdown-delight review: {n} inline comment{} below, each a \"> 💬\" \
blockquote placed right after the section it refers to. The document is otherwise \
verbatim. -->\n\n",
        if n == 1 { "" } else { "s" }
    );

    // splice callouts in at each annotated block's end-of-source position
    let mut cursor = 0usize;
    for (i, threads) in &per_block {
        let pos = meta[*i].src.end.min(source.len()).max(cursor);
        out.push_str(&source[cursor..pos]);
        // blank line, the callouts (one per thread), blank line — valid md block
        out.push('\n');
        for t in threads {
            out.push_str(&render_thread(t, false));
            out.push_str("\n\n");
        }
        cursor = pos;
    }
    out.push_str(&source[cursor..]);

    if !orphans.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(
            "\n---\n\n<!-- Comments whose anchored text was edited away — kept for reference. -->\n\n",
        );
        for t in &orphans {
            out.push_str(&render_thread(t, true));
            out.push_str("\n\n");
        }
    }
    out
}

// ── store ──────────────────────────────────────────────────────────────────

impl CommentStore {
    /// Load a doc's threads (empty if none / unreadable / malformed).
    pub fn load(key: &str) -> Self {
        fs::read_to_string(store_path(key))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist. Best-effort — failures are swallowed (never block the UI).
    pub fn save(&self, key: &str) {
        let path = store_path(key);
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(txt) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, txt);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{:x}-{}", now_millis(), self.seq)
    }

    /// The whole-block thread for a block, if one exists and is live.
    pub fn block_thread(&self, fp: u64, ord: usize) -> Option<&Thread> {
        self.threads.iter().find(|t| {
            !t.deprecated
                && t.anchor.range.is_none()
                && t.anchor.block_fp == fp
                && t.anchor.block_ord == ord
        })
    }

    /// All live threads (block + range) attached to a given block.
    pub fn threads_for_block(&self, fp: u64, ord: usize) -> Vec<&Thread> {
        self.threads
            .iter()
            .filter(|t| !t.deprecated && t.anchor.block_fp == fp && t.anchor.block_ord == ord)
            .collect()
    }

    pub fn thread(&self, id: &str) -> Option<&Thread> {
        self.threads.iter().find(|t| t.id == id)
    }

    pub fn deprecated(&self) -> impl Iterator<Item = &Thread> {
        self.threads.iter().filter(|t| t.deprecated)
    }

    /// Start a new thread with its first comment; returns the thread id.
    pub fn new_thread(&mut self, anchor: Anchor, author: String, body: String) -> String {
        let cid = self.next_id("c");
        let tid = self.next_id("t");
        self.threads.push(Thread {
            id: tid.clone(),
            anchor,
            comments: vec![Comment {
                id: cid,
                author,
                body,
                ts: now_millis(),
            }],
            resolved: false,
            deprecated: false,
        });
        tid
    }

    pub fn reply(&mut self, thread_id: &str, author: String, body: String) {
        let cid = self.next_id("c");
        if let Some(t) = self.threads.iter_mut().find(|t| t.id == thread_id) {
            t.comments.push(Comment {
                id: cid,
                author,
                body,
                ts: now_millis(),
            });
        }
    }

    pub fn set_resolved(&mut self, id: &str, v: bool) {
        if let Some(t) = self.threads.iter_mut().find(|t| t.id == id) {
            t.resolved = v;
        }
    }

    pub fn delete(&mut self, id: &str) {
        self.threads.retain(|t| t.id != id);
    }

    /// Re-locate every thread against the current block set; mark `deprecated`
    /// when its text can no longer be found. Run after each reparse.
    pub fn reanchor(&mut self, blocks: &[BlockMeta]) {
        for thread in &mut self.threads {
            let a = &mut thread.anchor;
            // candidates sharing the stored fingerprint, in document order
            let same_fp: Vec<&BlockMeta> = blocks.iter().filter(|b| b.fp == a.block_fp).collect();
            let chosen = same_fp
                .get(a.block_ord)
                .copied()
                .or_else(|| same_fp.first().copied());

            if let Some(b) = chosen {
                // fingerprint still present — for a range, confirm/relocate the quote
                if a.range.is_some() {
                    match relocate(&b.plain, a.quote.trim()) {
                        Some(r) => {
                            a.range = Some(r);
                            thread.deprecated = false;
                        }
                        None => thread.deprecated = true,
                    }
                } else {
                    thread.deprecated = false;
                }
                continue;
            }

            // fingerprint gone — try to relocate by quote across all blocks
            let q = a.quote.trim();
            if !q.is_empty() {
                if let Some((bi, range)) = blocks
                    .iter()
                    .enumerate()
                    .find_map(|(i, b)| relocate(&b.plain, q).map(|r| (i, r)))
                {
                    a.block_fp = blocks[bi].fp;
                    a.block_ord = blocks[..bi]
                        .iter()
                        .filter(|x| x.fp == blocks[bi].fp)
                        .count();
                    if a.range.is_some() {
                        a.range = Some(range);
                    }
                    thread.deprecated = false;
                    continue;
                }
            }
            thread.deprecated = true;
        }
    }
}

/// Find `quote` inside `plain`, returning its char-offset range if present.
fn relocate(plain: &str, quote: &str) -> Option<(usize, usize)> {
    if quote.is_empty() {
        return None;
    }
    let byte = plain.find(quote)?;
    let start = plain[..byte].chars().count();
    Some((start, start + quote.chars().count()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_whitespace_stable_but_content_sensitive() {
        assert_eq!(
            fingerprint("hello   world\n"),
            fingerprint("  hello world ")
        );
        assert_ne!(fingerprint("hello world"), fingerprint("goodbye world"));
    }

    #[test]
    fn git_guard_is_idempotent_and_local() {
        let tmp = std::env::temp_dir().join(format!("md-cmt-test-{}", now_millis()));
        fs::create_dir_all(tmp.join(".git/info")).unwrap();
        let doc = tmp.join("note.md");
        fs::write(&doc, "x").unwrap();
        let pats = ["*.comments.json", "/.markdown-delight/"];

        assert!(ensure_git_ignored(&doc, &pats).unwrap()); // first call writes
        assert!(!ensure_git_ignored(&doc, &pats).unwrap()); // second is a no-op

        let ex = fs::read_to_string(tmp.join(".git/info/exclude")).unwrap();
        assert_eq!(ex.matches("*.comments.json").count(), 1);
        assert_eq!(ex.matches("/.markdown-delight/").count(), 1);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn git_guard_noop_outside_repo() {
        let tmp = std::env::temp_dir().join(format!("md-norepo-{}", now_millis()));
        fs::create_dir_all(&tmp).unwrap();
        let doc = tmp.join("note.md");
        fs::write(&doc, "x").unwrap();
        assert!(!ensure_git_ignored(&doc, &["*.comments.json"]).unwrap());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn export_writes_sidecar_and_local_ignore() {
        let tmp = std::env::temp_dir().join(format!("md-export-{}", now_millis()));
        fs::create_dir_all(tmp.join(".git/info")).unwrap();
        let doc = tmp.join("notes.md");
        fs::write(&doc, "# hi").unwrap();

        let mut store = CommentStore::default();
        store.new_thread(
            Anchor::whole_block(1, 0, "hi".into()),
            "t".into(),
            "nice".into(),
        );

        let side = export_sidecar(&doc, &store).unwrap();
        assert!(side.exists());
        assert_eq!(side.file_name().unwrap(), "notes.md.comments.json");
        // round-trips
        let back: CommentStore = serde_json::from_str(&fs::read_to_string(&side).unwrap()).unwrap();
        assert_eq!(back.threads.len(), 1);
        // and the sidecar pattern is locally git-ignored (can't be committed)
        let ex = fs::read_to_string(tmp.join(".git/info/exclude")).unwrap();
        assert!(ex.contains("*.comments.json"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn reanchor_relocates_then_deprecates() {
        let fp_a = fingerprint("alpha line");
        let mut store = CommentStore::default();
        let tid = store.new_thread(
            Anchor::whole_block(fp_a, 0, "alpha line".into()),
            "tester".into(),
            "looks good".into(),
        );

        // block still present (possibly reordered) → live
        let blocks = vec![
            BlockMeta {
                fp: fingerprint("intro"),
                plain: "intro".into(),
                src: 0..5,
            },
            BlockMeta {
                fp: fp_a,
                plain: "alpha line".into(),
                src: 6..16,
            },
        ];
        store.reanchor(&blocks);
        assert!(!store.thread(&tid).unwrap().deprecated);

        // block text removed entirely → deprecated, but retained
        let gone = vec![BlockMeta {
            fp: fingerprint("intro"),
            plain: "intro".into(),
            src: 0..5,
        }];
        store.reanchor(&gone);
        assert!(store.thread(&tid).unwrap().deprecated);
        assert_eq!(store.threads.len(), 1);
    }

    #[test]
    fn range_quote_relocates_within_edited_block() {
        let fp = fingerprint("the quick brown fox");
        let mut store = CommentStore::default();
        let tid = store.new_thread(
            Anchor::span(fp, 0, (4, 9), "quick".into()),
            "t".into(),
            "word choice".into(),
        );
        // same words, extra padding → fp differs, but quote still findable
        let blocks = vec![BlockMeta {
            fp: fingerprint("oh the quick brown fox ran"),
            plain: "oh the quick brown fox ran".into(),
            src: 0..26,
        }];
        store.reanchor(&blocks);
        let t = store.thread(&tid).unwrap();
        assert!(!t.deprecated);
        assert_eq!(t.anchor.range, Some((7, 12))); // "quick" at new offset
    }

    #[test]
    fn review_injects_comment_after_its_block() {
        let src = "# Report\n\nThe sky is green.\n\nDone.\n";
        let (_, meta) = crate::render::parse_with_meta(src);
        let i = meta.iter().position(|m| m.plain.contains("sky")).unwrap();
        let mut store = CommentStore::default();
        store.new_thread(
            Anchor::whole_block(meta[i].fp, 0, meta[i].plain.clone()),
            "Parker".into(),
            "should be blue".into(),
        );
        let out = review_markdown(src, &meta, &store);
        assert!(out.contains("The sky is green."), "original kept verbatim");
        assert!(out.contains("> 💬 **Parker**"), "comment injected");
        // the callout sits between its block and the next one
        let sky = out.find("sky is green").unwrap();
        let cmt = out.find("should be blue").unwrap();
        let done = out.find("Done.").unwrap();
        assert!(
            sky < cmt && cmt < done,
            "comment between its block and next"
        );
    }

    #[test]
    fn review_quotes_the_span_for_range_comments() {
        let src = "Alpha beta gamma.\n";
        let (_, meta) = crate::render::parse_with_meta(src);
        let mut store = CommentStore::default();
        store.new_thread(
            Anchor::span(meta[0].fp, 0, (6, 10), "beta".into()),
            "P".into(),
            "word choice".into(),
        );
        let out = review_markdown(src, &meta, &store);
        assert!(out.contains("on “beta”"), "range comment quotes its span");
        assert!(out.contains("> word choice"));
    }

    #[test]
    fn review_keeps_orphaned_comments_in_a_trailing_section() {
        let src = "Only line.\n";
        let (_, meta) = crate::render::parse_with_meta(src);
        let mut store = CommentStore::default();
        // anchor to a block that doesn't exist in meta → orphan
        store.new_thread(
            Anchor::whole_block(99, 0, "vanished text".into()),
            "P".into(),
            "still matters".into(),
        );
        let out = review_markdown(src, &meta, &store);
        assert!(out.contains("Only line."), "doc kept");
        assert!(out.contains("orphaned"), "orphan section present");
        assert!(out.contains("still matters"), "orphan comment retained");
    }
}
