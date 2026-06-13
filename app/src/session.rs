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
