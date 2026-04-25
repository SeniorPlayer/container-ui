//! Runtime profile: which container CLI to shell out to.
//!
//! Profiles are loaded from `$XDG_CONFIG_HOME/cgui/profiles.toml`. The active
//! profile is remembered in `state.json` (via the `prefs` module). The runtime
//! exposes a thread-safe `binary()` getter that `container.rs` calls instead
//! of a hardcoded string, so the user can switch CLIs (`container`, `docker`,
//! `nerdctl`, `podman`, …) without restarting cgui.
//!
//! TOML schema:
//!
//! ```toml
//! default = "container"
//!
//! [[profile]]
//! name = "container"
//! binary = "container"
//!
//! [[profile]]
//! name = "docker"
//! binary = "/usr/local/bin/docker"
//! ```

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub binary: String,
}

impl Profile {
    fn container_default() -> Self {
        Self {
            name: "container".into(),
            binary: "container".into(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawProfiles {
    default: Option<String>,
    #[serde(default)]
    profile: Vec<RawProfile>,
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    name: String,
    binary: String,
}

/// All profiles known at startup, in the order they appear in the TOML file.
/// Always non-empty: if no file is present, contains the implicit
/// `container` default.
pub fn load_profiles() -> Vec<Profile> {
    let Some(p) = path() else {
        return vec![Profile::container_default()];
    };
    let Ok(s) = std::fs::read_to_string(&p) else {
        return vec![Profile::container_default()];
    };
    let raw: RawProfiles = match toml::from_str(&s) {
        Ok(r) => r,
        Err(_) => return vec![Profile::container_default()],
    };
    let mut v: Vec<Profile> = raw
        .profile
        .into_iter()
        .map(|p| Profile {
            name: p.name,
            binary: p.binary,
        })
        .collect();
    if v.is_empty() {
        v.push(Profile::container_default());
    }
    v
}

/// Default profile name from `profiles.toml`'s `default = "..."` field, if any.
pub fn default_name() -> Option<String> {
    let p = path()?;
    let s = std::fs::read_to_string(&p).ok()?;
    let raw: RawProfiles = toml::from_str(&s).ok()?;
    raw.default
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cgui").join("profiles.toml"))
}

// --- Active runtime ---

static STATE: LazyLock<RwLock<Active>> = LazyLock::new(|| {
    RwLock::new(Active {
        name: "container".into(),
        binary: "container".into(),
    })
});

#[derive(Debug, Clone)]
struct Active {
    name: String,
    binary: String,
}

pub fn set_active(p: &Profile) {
    let mut g = STATE.write().expect("runtime state lock");
    g.name = p.name.clone();
    g.binary = p.binary.clone();
}

pub fn binary() -> String {
    STATE.read().expect("runtime state lock").binary.clone()
}

pub fn name() -> String {
    STATE.read().expect("runtime state lock").name.clone()
}
