//! finder.rs — the Ctrl+P fuzzy file finder's index + matcher.
//!
//! One global index, per-pane UI: a background thread walks $HOME once at
//! startup (gitignore-aware via the `ignore` crate, hidden dirs skipped)
//! collecting every .md/.markdown path, streaming batches into a shared Vec
//! so the finder is usable while the walk is still running. Matching is
//! nucleo (Helix's fzf-style matcher) over the home-relative display string,
//! so what you type matches what you see.

use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

use ignore::WalkBuilder;
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// Runaway guard — a pathological disk won't balloon the index.
const MAX_FILES: usize = 100_000;

pub struct Hit {
    /// Absolute path, ready for fs::read_to_string.
    pub path: String,
    /// Home-relative ("~/…") string the row shows and the matcher scored.
    pub display: String,
    /// Matched char positions in `display` (for highlight).
    pub indices: Vec<u32>,
}

pub struct FileIndex {
    files: Arc<RwLock<Vec<String>>>, // display strings ("~/…")
    home: String,
    done: Arc<AtomicBool>,
}

impl FileIndex {
    /// Kick off the background walk and return immediately.
    pub fn spawn() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let files = Arc::new(RwLock::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let (files_w, done_w, root) = (files.clone(), done.clone(), home.clone());
        std::thread::spawn(move || {
            let mut batch: Vec<String> = Vec::with_capacity(256);
            let walk = WalkBuilder::new(&root)
                .follow_links(false)
                .filter_entry(|e| {
                    // .git/.cache etc. are hidden and already skipped
                    !matches!(e.file_name().to_str(), Some("node_modules" | "target" | "snap"))
                })
                .build();
            let mut total = 0usize;
            for entry in walk.flatten() {
                if total >= MAX_FILES {
                    break;
                }
                let is_md = entry.file_type().is_some_and(|t| t.is_file())
                    && entry
                        .path()
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
                if !is_md {
                    continue;
                }
                let p = entry.path().to_string_lossy();
                let display = match p.strip_prefix(root.as_str()) {
                    Some(rest) => format!("~{rest}"),
                    None => p.into_owned(),
                };
                batch.push(display);
                total += 1;
                if batch.len() >= 256 {
                    files_w.write().unwrap().append(&mut batch);
                }
            }
            let mut v = files_w.write().unwrap();
            v.append(&mut batch);
            // empty-query view: shallow paths first, then alphabetical
            v.sort_by_key(|s| (s.bytes().filter(|b| *b == b'/').count(), s.clone()));
            done_w.store(true, Ordering::Release);
        });
        Self { files, home, done }
    }

    /// How many paths the walk has collected so far (cheap; grows during
    /// indexing). Callers cache against this to know when to recompute.
    pub fn total(&self) -> usize {
        self.files.read().unwrap().len()
    }

    /// Top `max` fuzzy matches for `query`; also reports (still_indexing, total).
    /// Recomputing is O(files) — callers should cache and only call on query
    /// change or when `total()` grows, never every frame.
    pub fn hits(&self, query: &str, max: usize) -> (Vec<Hit>, bool, usize) {
        let files = self.files.read().unwrap();
        let indexing = !self.done.load(Ordering::Acquire);
        let total = files.len();
        let hits = rank(&files, &self.home, query, max);
        (hits, indexing, total)
    }
}

/// Convert a "~/…" display string back to an absolute path under `home`.
fn to_abs(home: &str, display: &str) -> String {
    match display.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => display.to_string(),
    }
}

/// Pure ranking core (no locks/IO) — fuzzy-match `query` against `files`,
/// returning at most `max` Hits, best first. Extracted so it is unit-testable.
fn rank(files: &[String], home: &str, query: &str, max: usize) -> Vec<Hit> {
    if query.trim().is_empty() {
        return files
            .iter()
            .take(max)
            .map(|d| Hit {
                path: to_abs(home, d),
                display: d.clone(),
                indices: vec![],
            })
            .collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pat = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, &String, Vec<u32>)> = Vec::new();
    for d in files.iter() {
        let mut indices = Vec::new();
        let hay = Utf32Str::new(d, &mut buf);
        if let Some(score) = pat.indices(hay, &mut matcher, &mut indices) {
            indices.sort_unstable();
            indices.dedup();
            scored.push((score, d, indices));
        }
    }
    // higher score first; tie-break toward fewer matched chars (tighter match)
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.len().cmp(&b.2.len())));
    scored
        .into_iter()
        .take(max)
        .map(|(_, d, indices)| Hit {
            path: to_abs(home, d),
            display: d.clone(),
            indices,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<String> {
        vec![
            "~/notes/todo.md".into(),
            "~/notes/ideas.md".into(),
            "~/work/report.md".into(),
            "~/work/readme.md".into(),
            "~/deep/a/b/c/readme.md".into(),
        ]
    }

    #[test]
    fn empty_query_returns_all_capped() {
        let f = sample();
        let hits = rank(&f, "/home/me", "", 3);
        assert_eq!(hits.len(), 3);
        assert!(hits[0].indices.is_empty());
    }

    #[test]
    fn fuzzy_subsequence_matches_and_ranks() {
        let f = sample();
        let hits = rank(&f, "/home/me", "todo", 10);
        assert_eq!(hits[0].display, "~/notes/todo.md");
        // "rdme" subsequence hits both readme files, nothing else
        let hits = rank(&f, "/home/me", "rdme", 10);
        assert!(hits.iter().all(|h| h.display.contains("readme")));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn no_match_yields_empty() {
        let hits = rank(&sample(), "/home/me", "zzqq", 10);
        assert!(hits.is_empty());
    }

    #[test]
    fn indices_point_at_matched_chars() {
        let hits = rank(&sample(), "/home/me", "todo", 10);
        let h = &hits[0];
        let got: String = h.indices.iter().map(|&i| h.display.chars().nth(i as usize).unwrap()).collect();
        assert_eq!(got.to_lowercase(), "todo");
    }

    #[test]
    fn to_abs_round_trips_home() {
        assert_eq!(to_abs("/home/me", "~/notes/todo.md"), "/home/me/notes/todo.md");
        // a non-~ display (outside home) is returned unchanged
        assert_eq!(to_abs("/home/me", "/etc/x.md"), "/etc/x.md");
    }
}
