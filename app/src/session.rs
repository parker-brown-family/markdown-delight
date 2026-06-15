//! session.rs — remember which files/notebooks were open and reopen them next
//! launch. Only docs with a real path are persisted; unsaved scratch buffers
//! are skipped (save them first — see Workspace::save_focused). Stored as TOML
//! at ~/.config/markdown-delight/session.toml.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
pub struct Session {
    /// Index (into `tabs`) that was active when last saved.
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub tabs: Vec<SessionTab>,
    /// The workspace ("outer") appearance — global theme id, seed, texture,
    /// grade, curve — so the chosen look survives a restart.
    #[serde(default)]
    pub outer: Option<crate::appearance::OuterAppearance>,
    /// The most-recent *inner* (pane) appearance — a best guess at the look new
    /// panes should take next launch, so the chosen tube survives a restart.
    #[serde(default)]
    pub inner: Option<crate::appearance::PaneAppearance>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionTab {
    /// Absolute path of the file/notebook this tab held.
    pub path: String,
    /// The tab's display name, if it had a custom one.
    #[serde(default)]
    pub name: Option<String>,
}

fn session_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/markdown-delight/session.toml")
}

/// Read the saved session (empty if none / unreadable / malformed).
pub fn load() -> Session {
    fs::read_to_string(session_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the session. Best-effort — failures are swallowed (never block the UI).
pub fn save(session: &Session) {
    let path = session_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(text) = toml::to_string(session) {
        let _ = fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{Colour, Grade, OuterAppearance, PaneAppearance};

    // save() swallows serialize errors, so a regression here would silently stop
    // ALL persistence — guard the outer+inner look round-trips through TOML.
    #[test]
    fn session_with_outer_and_inner_roundtrips_through_toml() {
        let mut inner = PaneAppearance::default();
        inner.set_colour(Colour::new("hacker"));
        inner.set_grade(Grade {
            brightness: 0.3,
            ..Default::default()
        });
        let s = Session {
            active: 1,
            tabs: vec![SessionTab {
                path: "/x.md".into(),
                name: Some("X".into()),
            }],
            outer: Some(OuterAppearance::new("paper")),
            inner: Some(inner),
        };
        let txt = toml::to_string(&s).expect("session serializes to TOML");
        let back: Session = toml::from_str(&txt).expect("and parses back");
        assert_eq!(back.active, 1);
        assert_eq!(back.outer.unwrap().colour.id, "paper");
        let bi = back.inner.unwrap();
        assert_eq!(bi.colour.as_ref().unwrap().id, "hacker");
        assert!(!bi.inherit_colour, "the colour override is retained");
        assert!((bi.grade.unwrap().brightness - 0.3).abs() < 1e-6);
    }
}
