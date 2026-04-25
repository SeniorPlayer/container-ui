//! Color theme. Loads from `$XDG_CONFIG_HOME/cgui/theme.toml` if present;
//! otherwise uses the built-in defaults. Missing/malformed files are
//! silently ignored — never blocks startup.
//!
//! TOML schema (all fields optional, named after the role they play in the
//! UI rather than literal colors):
//!
//! ```toml
//! accent  = "cyan"        # tab highlight, modal borders, headers
//! primary = "white"       # default body text
//! muted   = "darkgray"    # punctuation, hints, dim labels
//! success = "green"       # running status, ok results
//! warning = "yellow"      # marks, mid-progress, in-flight
//! danger  = "red"         # stopped, errors, high CPU
//! info    = "blue"        # image refs, links
//! key     = "cyan"        # JSON keys, header rows
//! string  = "green"       # JSON strings
//! number  = "magenta"     # JSON numbers, MEM bar
//! ```
//!
//! Color values: any of `black`, `red`, `green`, `yellow`, `blue`, `magenta`,
//! `cyan`, `white`, `darkgray`, `lightred`, `lightgreen`, `lightyellow`,
//! `lightblue`, `lightmagenta`, `lightcyan`, `gray`, or `#RRGGBB` /
//! `rgb(r,g,b)` for truecolor terminals.

use ratatui::style::Color;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Theme {
    pub accent: Color,
    pub primary: Color,
    pub muted: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    pub key: Color,
    pub string: Color,
    pub number: Color,
    pub alerts: Alerts,
}

/// Resource-alert thresholds + appearance. Pulse alternates the row's
/// background once per ~500 ms when an alert is active.
#[derive(Debug, Clone)]
pub struct Alerts {
    pub cpu_warn: f64,
    pub cpu_alert: f64,
    pub mem_warn: f64,
    pub mem_alert: f64,
    pub pulse: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            cpu_warn: 60.0,
            cpu_alert: 85.0,
            mem_warn: 70.0,
            mem_alert: 90.0,
            pulse: true,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AlertLevel {
    None,
    Warn,
    Alert,
}

impl Alerts {
    pub fn cpu_level(&self, pct: f64) -> AlertLevel {
        if pct >= self.cpu_alert {
            AlertLevel::Alert
        } else if pct >= self.cpu_warn {
            AlertLevel::Warn
        } else {
            AlertLevel::None
        }
    }
    pub fn mem_level(&self, pct: f64) -> AlertLevel {
        if pct >= self.mem_alert {
            AlertLevel::Alert
        } else if pct >= self.mem_warn {
            AlertLevel::Warn
        } else {
            AlertLevel::None
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Cyan,
            primary: Color::White,
            muted: Color::DarkGray,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
            info: Color::Blue,
            key: Color::Cyan,
            string: Color::Green,
            number: Color::Magenta,
            alerts: Alerts::default(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct Raw {
    accent: Option<String>,
    primary: Option<String>,
    muted: Option<String>,
    success: Option<String>,
    warning: Option<String>,
    danger: Option<String>,
    info: Option<String>,
    key: Option<String>,
    string: Option<String>,
    number: Option<String>,
    alerts: Option<RawAlerts>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAlerts {
    cpu_warn: Option<f64>,
    cpu_alert: Option<f64>,
    mem_warn: Option<f64>,
    mem_alert: Option<f64>,
    pulse: Option<bool>,
}

impl Theme {
    pub fn load() -> Self {
        let mut t = Self::default();
        let Some(path) = path() else { return t };
        let Ok(s) = std::fs::read_to_string(&path) else { return t };
        let Ok(raw) = toml::from_str::<Raw>(&s) else { return t };
        if let Some(c) = raw.accent.as_deref().and_then(parse_color) { t.accent = c; }
        if let Some(c) = raw.primary.as_deref().and_then(parse_color) { t.primary = c; }
        if let Some(c) = raw.muted.as_deref().and_then(parse_color) { t.muted = c; }
        if let Some(c) = raw.success.as_deref().and_then(parse_color) { t.success = c; }
        if let Some(c) = raw.warning.as_deref().and_then(parse_color) { t.warning = c; }
        if let Some(c) = raw.danger.as_deref().and_then(parse_color) { t.danger = c; }
        if let Some(c) = raw.info.as_deref().and_then(parse_color) { t.info = c; }
        if let Some(c) = raw.key.as_deref().and_then(parse_color) { t.key = c; }
        if let Some(c) = raw.string.as_deref().and_then(parse_color) { t.string = c; }
        if let Some(c) = raw.number.as_deref().and_then(parse_color) { t.number = c; }
        if let Some(a) = raw.alerts {
            if let Some(v) = a.cpu_warn { t.alerts.cpu_warn = v; }
            if let Some(v) = a.cpu_alert { t.alerts.cpu_alert = v; }
            if let Some(v) = a.mem_warn { t.alerts.mem_warn = v; }
            if let Some(v) = a.mem_alert { t.alerts.mem_alert = v; }
            if let Some(v) = a.pulse { t.alerts.pulse = v; }
        }
        t
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cgui").join("theme.toml"))
}

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    // #RRGGBB
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
    }
    // rgb(r,g,b)
    if let Some(rest) = s.strip_prefix("rgb(") {
        let body = rest.trim_end_matches(')');
        let parts: Vec<&str> = body.split(',').map(|p| p.trim()).collect();
        if parts.len() == 3 {
            let r: u8 = parts[0].parse().ok()?;
            let g: u8 = parts[1].parse().ok()?;
            let b: u8 = parts[2].parse().ok()?;
            return Some(Color::Rgb(r, g, b));
        }
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "reset" => Color::Reset,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_colors() {
        assert_eq!(parse_color("cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("DarkGray"), Some(Color::DarkGray));
        assert_eq!(parse_color("nope"), None);
    }
    #[test]
    fn hex_color() {
        assert_eq!(parse_color("#ff8800"), Some(Color::Rgb(255, 136, 0)));
    }
    #[test]
    fn rgb_color() {
        assert_eq!(parse_color("rgb(10, 20, 30)"), Some(Color::Rgb(10, 20, 30)));
    }
}
