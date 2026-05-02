//! Phase-1 update detection. Queries GitHub Releases for the installed
//! container runtime and for cgui itself, compares against the running
//! version, and surfaces an `UpdateInfo` when a newer release exists.
//!
//! Strictly read-only at this phase — no download, no install, no auto-upgrade.
//! The status bar gets a chip; `cgui doctor` gets a section; nothing destructive.
//!
//! Network is minimal: 24h cache means at most ~2 GitHub API calls per repo
//! per day. Uses `curl` (always present on macOS) so we don't add an HTTPS
//! client dependency just for two endpoints.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Component {
    AppleContainer,
    CguiSelf,
}

impl Component {
    pub fn label(self) -> &'static str {
        match self {
            Component::AppleContainer => "container",
            Component::CguiSelf => "cgui",
        }
    }
    pub fn repo(self) -> &'static str {
        match self {
            Component::AppleContainer => "apple/container",
            Component::CguiSelf => "elementalcollision/cgui",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub component: Component,
    pub installed: String,
    pub latest: String,
    pub release_url: String,
    pub published_at: String,
    /// Release notes body, trimmed and capped. Empty if the API didn't
    /// return one or it failed to decode as UTF-8.
    pub notes: String,
    /// The signed `.pkg` asset for this release, if one exists. Phase 3
    /// uses this for download; phase 4 will use it for `installer`.
    pub asset: Option<SignedAsset>,
}

/// Metadata for the signed installer we'll download (and later install).
/// Stored in CachedRelease so reuse decisions don't need a fresh API hit.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SignedAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// Cached snapshot of one component's most recent check. Persisted in
/// `state.json` so we don't re-hit the GitHub API every refresh.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CachedRelease {
    pub component: String,        // Component label
    pub latest_tag: String,
    pub release_url: String,
    pub published_at: String,
    pub fetched_at: u64,          // unix seconds
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub asset: Option<SignedAsset>,
}

const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Public entry point. Returns one `UpdateInfo` per component that is
/// behind its latest release. Honours the user's opt-out for *automatic*
/// callers (TUI background task, doctor); explicit `cgui update` should
/// call `check_force` to bypass the gate.
pub async fn check(prefs: &mut crate::prefs::Prefs) -> Vec<UpdateInfo> {
    if prefs.auto_update_check == Some(false) {
        return Vec::new();
    }
    check_force(prefs).await
}

/// Same as `check` but ignores the opt-out — used by the explicit
/// `cgui update` subcommand where the user has typed the verb themselves.
pub async fn check_force(prefs: &mut crate::prefs::Prefs) -> Vec<UpdateInfo> {
    let mut out = Vec::new();
    for c in [Component::AppleContainer, Component::CguiSelf] {
        if let Some(info) = check_component(prefs, c).await {
            out.push(info);
        }
    }
    out
}

async fn check_component(
    prefs: &mut crate::prefs::Prefs,
    c: Component,
) -> Option<UpdateInfo> {
    let installed = installed_version(c)?;

    // Try cache first.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let cached = prefs
        .update_cache
        .iter()
        .find(|cr| cr.component == c.label() && now.saturating_sub(cr.fetched_at) < CACHE_TTL_SECS)
        .cloned();

    let latest = match cached {
        Some(cr) => cr,
        None => {
            let fresh = fetch_latest(c.repo()).await?;
            let asset = pick_signed_asset(c, &fresh.assets);
            let cr = CachedRelease {
                component: c.label().to_string(),
                latest_tag: fresh.tag_name,
                release_url: fresh.html_url,
                published_at: fresh.published_at,
                fetched_at: now,
                notes: trim_notes(&fresh.body),
                asset,
            };
            prefs
                .update_cache
                .retain(|x| x.component != c.label());
            prefs.update_cache.push(cr.clone());
            prefs.last_update_check = Some(now);
            prefs.save();
            cr
        }
    };

    if compare_versions(&installed, &latest.latest_tag) == std::cmp::Ordering::Less {
        Some(UpdateInfo {
            component: c,
            installed,
            latest: latest.latest_tag,
            release_url: latest.release_url,
            published_at: latest.published_at,
            notes: latest.notes,
            asset: latest.asset,
        })
    } else {
        None
    }
}

/// Pick the asset cgui should download for `c`. For Apple's container we
/// require the **signed** installer; we deliberately refuse the unsigned
/// variant to keep the install path safe by default. For cgui itself we
/// look for an OS+arch matched archive or raw binary.
fn pick_signed_asset(c: Component, assets: &[GhAsset]) -> Option<SignedAsset> {
    match c {
        Component::AppleContainer => {
            let a = assets.iter().find(|a| a.name.contains("installer-signed.pkg"))?;
            Some(SignedAsset {
                name: a.name.clone(),
                url: a.browser_download_url.clone(),
                size: a.size,
            })
        }
        Component::CguiSelf => pick_self_asset(assets),
    }
}

/// Pick the most appropriate cgui release asset for the current host.
/// Preference order:
///   1. exact `<arch>-apple-<os>` archive (e.g. `cgui-aarch64-apple-darwin.tar.gz`)
///   2. anything mentioning `macos` / `darwin` (when on macOS)
///   3. a bare `cgui` binary
///
/// Returns None if no plausible asset is published — phase 5's self-update
/// will surface a clean "no asset" message instead of doing anything risky.
fn pick_self_asset(assets: &[GhAsset]) -> Option<SignedAsset> {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    let exact = format!("{arch}-apple-{os}");
    let on_macos = os == "macos";

    // 1. exact triple match
    if let Some(a) = assets.iter().find(|a| a.name.to_lowercase().contains(&exact)) {
        return Some(SignedAsset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        });
    }
    // 2. macOS-tagged archive
    if on_macos {
        if let Some(a) = assets.iter().find(|a| {
            let n = a.name.to_lowercase();
            n.contains("cgui") && (n.contains("macos") || n.contains("darwin"))
        }) {
            return Some(SignedAsset {
                name: a.name.clone(),
                url: a.browser_download_url.clone(),
                size: a.size,
            });
        }
    }
    // 3. bare `cgui` binary asset
    if let Some(a) = assets.iter().find(|a| a.name == "cgui") {
        return Some(SignedAsset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        });
    }
    None
}

/// How cgui itself was installed — drives the self-update route.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CguiInstallMethod {
    /// Replace the binary in place (atomic rename over `current_exe`).
    Binary,
    /// `brew upgrade cgui` (no sudo, brew handles it).
    Brew,
    /// Cargo-installed; cgui shouldn't manage cargo state, just suggest
    /// the right command.
    Cargo,
}

pub fn cgui_install_method() -> CguiInstallMethod {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return CguiInstallMethod::Binary,
    };
    let path = exe.display().to_string();
    if path.contains("/Cellar/")
        || path.contains("/opt/homebrew/")
        || path.contains("/.linuxbrew/")
    {
        return CguiInstallMethod::Brew;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = std::path::PathBuf::from(home).join(".cargo").join("bin");
        if exe.starts_with(&cargo_bin) {
            return CguiInstallMethod::Cargo;
        }
    }
    CguiInstallMethod::Binary
}

/// Atomic in-place replacement of cgui's running binary. Handles raw-binary
/// assets and `*.tar.gz` / `*.tgz` archives that contain a `cgui` at any
/// depth. Sequence:
///
/// 1. (if archive) extract via `tar -xzf` to a sibling tmp dir
/// 2. locate the `cgui` binary inside the extracted tree
/// 3. copy bytes to `<current_exe>.new`, chmod 0755
/// 4. `std::fs::rename` over the running binary — POSIX guarantees this is
///    atomic on the same filesystem; the kernel keeps the running process's
///    inode mapped so we don't crash mid-flight
/// 5. clean up the staging tmp dir
///
/// Caller is expected to tell the user to restart cgui — verify_post_install
/// can't compare versions in-process because we're still the old code.
pub async fn install_self_binary(
    downloaded: std::path::PathBuf,
    sink: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let parent = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current_exe has no parent dir: {}", exe.display()))?;

    push(
        &sink,
        format!(
            "→ replacing {} (running) — atomic rename, no sudo",
            exe.display()
        ),
    );

    // Resolve the actual binary, extracting the archive if needed.
    let name_lc = downloaded
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let is_archive =
        name_lc.ends_with(".tar.gz") || name_lc.ends_with(".tgz") || name_lc.ends_with(".tar");

    let (binary_path, tmp_extract): (std::path::PathBuf, Option<std::path::PathBuf>) = if is_archive
    {
        let tmpdir = parent.join(".cgui-extract");
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir)?;
        push(&sink, format!("→ extracting {} → {}", name_lc, tmpdir.display()));
        let tar_arg = if name_lc.ends_with(".tar") { "-xf" } else { "-xzf" };
        let out = tokio::process::Command::new("tar")
            .arg(tar_arg)
            .arg(&downloaded)
            .arg("-C")
            .arg(&tmpdir)
            .output()
            .await?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&tmpdir);
            return Err(anyhow::anyhow!(
                "tar extract failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let found = find_cgui_in(&tmpdir).ok_or_else(|| {
            anyhow::anyhow!("no `cgui` binary found inside {}", downloaded.display())
        })?;
        (found, Some(tmpdir))
    } else {
        (downloaded.clone(), None)
    };

    let staged = exe.with_extension("new");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(&binary_path, &staged)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged, perms)?;
    }

    std::fs::rename(&staged, &exe)?;
    push(
        &sink,
        format!(
            "✓ replaced {} — restart cgui to use the new version",
            exe.display()
        ),
    );

    if let Some(d) = tmp_extract {
        let _ = std::fs::remove_dir_all(&d);
    }
    Ok(())
}

fn find_cgui_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.file_name() == Some(std::ffi::OsStr::new("cgui")) {
            if let Ok(meta) = std::fs::metadata(&p) {
                if meta.is_file() {
                    return Some(p);
                }
            }
        }
        if p.is_dir() {
            if let Some(found) = find_cgui_in(&p) {
                return Some(found);
            }
        }
    }
    None
}

/// Cap release notes so a runaway body can't blow up the modal or the
/// state.json on disk. We keep ~80 lines of `\n`-trimmed body.
fn trim_notes(body: &str) -> String {
    let body = body.replace("\r\n", "\n");
    let lines: Vec<&str> = body.lines().take(80).collect();
    lines.join("\n")
}

/// Where downloaded installers live. `~/Library/Caches/cgui/` on macOS,
/// `$XDG_CACHE_HOME/cgui/` elsewhere if set.
pub fn cache_dir() -> Option<std::path::PathBuf> {
    if let Some(c) = std::env::var_os("XDG_CACHE_HOME") {
        return Some(std::path::PathBuf::from(c).join("cgui"));
    }
    let home = std::env::var_os("HOME")?;
    let p = std::path::PathBuf::from(home);
    Some(if cfg!(target_os = "macos") {
        p.join("Library").join("Caches").join("cgui")
    } else {
        p.join(".cache").join("cgui")
    })
}

pub fn cache_path_for(asset: &SignedAsset) -> Option<std::path::PathBuf> {
    Some(cache_dir()?.join(&asset.name))
}

/// Spawn a download of `asset` into the cache dir. Streams progress
/// (downloaded / total bytes, plus percent) into `sink` once a second.
/// Reuses the cached file if it exists with the right size — no network
/// call, just a status line and immediate completion.
///
/// Publishes the final absolute path through `result_path` on success so
/// the (single) modal reaper can stay generic over OperationKind. On any
/// failure the partial download is removed and `result_path` stays None.
pub fn spawn_download(
    asset: SignedAsset,
    sink: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    result_path: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        let dest = cache_path_for(&asset)
            .ok_or_else(|| anyhow::anyhow!("no cache dir (HOME unset?)"))?;
        let dir = dest
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no parent dir for {}", dest.display()))?;
        std::fs::create_dir_all(dir)?;

        // Cache reuse — only when the existing file matches the API-reported
        // size exactly. Partial downloads are nuked so we never install half
        // a pkg.
        if let Ok(meta) = std::fs::metadata(&dest) {
            if meta.len() == asset.size {
                push(&sink, format!("✓ cached at {} ({} bytes)", dest.display(), meta.len()));
                if let Ok(mut g) = result_path.lock() {
                    *g = Some(dest.clone());
                }
                return Ok(());
            } else {
                push(
                    &sink,
                    format!(
                        "stale partial ({} of {} bytes) — refetching",
                        meta.len(),
                        asset.size
                    ),
                );
                let _ = std::fs::remove_file(&dest);
            }
        }

        let tmp = dest.with_extension("part");
        let _ = std::fs::remove_file(&tmp);
        push(
            &sink,
            format!(
                "$ curl -fL -o {} {} ({} MB)",
                tmp.display(),
                asset.url,
                asset.size / 1024 / 1024
            ),
        );

        let mut child = tokio::process::Command::new("curl")
            .args(["-fL", "--silent", "--show-error", "-o"])
            .arg(&tmp)
            .arg(&asset.url)
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let mut interval = tokio::time::interval(std::time::Duration::from_millis(1000));
        interval.tick().await; // burn the immediate tick
        let outcome: anyhow::Result<()> = loop {
            tokio::select! {
                status = child.wait() => {
                    let s = status?;
                    if s.success() {
                        break Ok(());
                    } else {
                        let mut err = String::new();
                        if let Some(stderr) = child.stderr.as_mut() {
                            use tokio::io::AsyncReadExt;
                            let _ = stderr.read_to_string(&mut err).await;
                        }
                        break Err(anyhow::anyhow!(
                            "curl exited {}: {}",
                            s,
                            err.trim()
                        ));
                    }
                }
                _ = interval.tick() => {
                    if let Ok(meta) = std::fs::metadata(&tmp) {
                        let pct = if asset.size > 0 {
                            (meta.len() as f64 / asset.size as f64) * 100.0
                        } else {
                            0.0
                        };
                        push(
                            &sink,
                            format!(
                                "  {} / {} ({:.1}%)",
                                human_mb(meta.len()),
                                human_mb(asset.size),
                                pct
                            ),
                        );
                    }
                }
            }
        };

        match outcome {
            Ok(()) => {
                let final_size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
                if final_size != asset.size {
                    let _ = std::fs::remove_file(&tmp);
                    let msg = format!(
                        "size mismatch: got {} bytes, expected {}",
                        final_size, asset.size
                    );
                    push(&sink, format!("✗ {msg}"));
                    return Err(anyhow::anyhow!(msg));
                }
                std::fs::rename(&tmp, &dest)?;
                push(
                    &sink,
                    format!(
                        "✓ cached at {} ({} bytes)",
                        dest.display(),
                        final_size
                    ),
                );
                if let Ok(mut g) = result_path.lock() {
                    *g = Some(dest.clone());
                }
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                push(&sink, format!("✗ {e}"));
                Err(e)
            }
        }
    })
}

/// How the user installed the runtime — drives whether `[I]nstall` runs
/// `sudo installer -pkg …` or `brew upgrade container`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    Pkg,
    Brew,
}

/// Detect whether `container` is brew-installed by inspecting `which`'s
/// output for a brew-typical path. Conservative: anything else is `Pkg`.
pub fn install_kind() -> InstallKind {
    let bin = crate::runtime::binary();
    let out = std::process::Command::new("which").arg(&bin).output();
    if let Ok(o) = out {
        if o.status.success() {
            let path = String::from_utf8_lossy(&o.stdout);
            let path = path.trim();
            if path.contains("/Cellar/")
                || path.contains("/opt/homebrew/")
                || path.contains("/.linuxbrew/")
            {
                return InstallKind::Brew;
            }
        }
    }
    InstallKind::Pkg
}

/// Argv for `sudo installer` against a downloaded pkg. Caller is
/// responsible for the suspend-TUI dance.
pub fn installer_argv(pkg: &std::path::Path) -> Vec<String> {
    vec![
        "sudo".into(),
        "installer".into(),
        "-pkg".into(),
        pkg.display().to_string(),
        "-target".into(),
        "/".into(),
    ]
}

/// Argv for the brew upgrade path. No sudo; brew handles its own prompts.
pub fn brew_upgrade_argv(c: Component) -> Vec<String> {
    vec![
        "brew".into(),
        "upgrade".into(),
        match c {
            Component::AppleContainer => "container".into(),
            Component::CguiSelf => "cgui".into(),
        },
    ]
}

fn human_mb(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{:.1} MiB", (bytes as f64) / (1024.0 * 1024.0))
    }
}

fn push(sink: &std::sync::Arc<std::sync::Mutex<Vec<String>>>, line: String) {
    if let Ok(mut v) = sink.lock() {
        if v.len() >= 2000 {
            v.drain(0..1000);
        }
        v.push(line);
    }
}

fn installed_version(c: Component) -> Option<String> {
    match c {
        Component::AppleContainer => {
            // `container --version` prints e.g. "container CLI version 0.12.3 (build: …)"
            let out = std::process::Command::new(crate::runtime::binary())
                .arg("--version")
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout);
            s.split_whitespace()
                .find(|t| {
                    let t = t.trim_start_matches('v');
                    parse_version(t).is_some()
                })
                .map(|s| s.trim_start_matches('v').to_string())
        }
        Component::CguiSelf => Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize, Clone)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

async fn fetch_latest(repo: &str) -> Option<GhRelease> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        tokio::process::Command::new("curl")
            .args([
                "-sSL",
                "--max-time",
                "6",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "User-Agent: cgui",
                &url,
            ])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice::<GhRelease>(&out.stdout).ok()
}

/// Parse `MAJOR.MINOR.PATCH` (with optional leading `v`). Returns None for
/// any non-numeric or extra-suffix variant; we don't try to handle pre-release
/// tags in phase 1.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next()?.parse().ok()?;
    let patch_part = it.next()?;
    // Allow build/pre-release suffix on patch (e.g. "3-beta1") — take leading digits.
    let patch_digits: String = patch_part.chars().take_while(|c| c.is_ascii_digit()).collect();
    let patch: u32 = patch_digits.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(av), Some(bv)) => av.cmp(&bv),
        // If either side fails to parse, treat as equal so we don't false-alarm.
        _ => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn parse_clean() {
        assert_eq!(parse_version("0.12.3"), Some((0, 12, 3)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
    }
    #[test]
    fn parse_suffix() {
        assert_eq!(parse_version("0.12.3-beta1"), Some((0, 12, 3)));
    }
    #[test]
    fn parse_bad() {
        assert_eq!(parse_version("0.12"), None);
        assert_eq!(parse_version("not.a.version"), None);
    }
    #[test]
    fn cmp_works() {
        assert_eq!(compare_versions("0.12.3", "0.13.0"), Ordering::Less);
        assert_eq!(compare_versions("0.13.0", "0.12.3"), Ordering::Greater);
        assert_eq!(compare_versions("0.12.3", "0.12.3"), Ordering::Equal);
        assert_eq!(compare_versions("garbage", "0.1.0"), Ordering::Equal);
    }
}
