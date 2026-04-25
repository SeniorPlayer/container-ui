//! Persistent user preferences.
//!
//! Best-effort: any IO error silently degrades to in-memory only. Stored at
//! `$XDG_CONFIG_HOME/cgui/state.json` (defaults to `~/.config/cgui/state.json`
//! on macOS — we deliberately don't follow Apple's `~/Library/Preferences`
//! convention because cgui is a CLI tool and dotfile-style config is friendlier
//! for terminal users.)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Prefs {
    /// Last active tab name (lowercased, e.g. "containers").
    pub tab: Option<String>,
    /// Sort key index per tab (`{"containers": 1, ...}`).
    pub sort: HashMap<String, u8>,
    /// Whether to show stopped containers as well as running.
    pub show_all: Option<bool>,
}

impl Prefs {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            // Atomic-ish: write to a sibling tmp file and rename.
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cgui").join("state.json"))
}
