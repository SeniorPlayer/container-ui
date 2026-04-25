//! Permissive parser for `container image pull` progress output.
//!
//! `container` doesn't expose a machine-readable progress channel, so we
//! best-effort scrape the human stream. We try, in order on each line,
//! starting from the newest:
//!
//! 1. an explicit percentage like `42%` or `42.5%`
//! 2. a byte ratio like `12.3MB/45.6MB` (any common decimal SI prefix)
//! 3. a layer ratio like `3/8`
//!
//! First parseable value wins. Returns a fraction in [0.0, 1.0].

pub fn parse_progress(lines: &[String]) -> Option<f64> {
    for line in lines.iter().rev() {
        if let Some(p) = parse_percent(line) {
            return Some(clamp(p));
        }
        if let Some(p) = parse_byte_ratio(line) {
            return Some(clamp(p));
        }
        if let Some(p) = parse_int_ratio(line) {
            return Some(clamp(p));
        }
    }
    None
}

/// A short status snippet (last non-empty line) suitable for the gauge label.
pub fn status_label(lines: &[String]) -> String {
    lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

fn clamp(p: f64) -> f64 {
    p.clamp(0.0, 1.0)
}

fn parse_percent(line: &str) -> Option<f64> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            // Skip optional whitespace before %.
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'%' {
                if let Ok(n) = std::str::from_utf8(&bytes[start..i]) {
                    if let Ok(v) = n.parse::<f64>() {
                        return Some(v / 100.0);
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Find a `<num><unit>/<num><unit>` ratio anywhere in the line. Returns the
/// resulting fraction. Units: B, KB, MB, GB, TB, KiB, MiB, GiB, TiB.
fn parse_byte_ratio(line: &str) -> Option<f64> {
    // Walk substrings around any '/' character.
    for (slash, _) in line.match_indices('/') {
        let lhs = parse_size_ending_at(&line[..slash])?;
        let rhs = parse_size_starting_at(&line[slash + 1..])?;
        if rhs > 0.0 {
            return Some(lhs / rhs);
        }
    }
    None
}

/// Pull a number+unit ending at the rightmost char of `s` (ignoring trailing
/// whitespace).
fn parse_size_ending_at(s: &str) -> Option<f64> {
    let trimmed = s.trim_end();
    let bytes = trimmed.as_bytes();
    // Consume unit letters from the end.
    let mut end = bytes.len();
    let mut unit_end = end;
    while end > 0 && bytes[end - 1].is_ascii_alphabetic() {
        end -= 1;
    }
    let unit = &trimmed[end..unit_end];
    if unit.is_empty() {
        return None;
    }
    let mult = unit_multiplier(unit)?;
    // Now consume digits/`.` backwards from `end`.
    unit_end = end;
    while end > 0 && (bytes[end - 1].is_ascii_digit() || bytes[end - 1] == b'.') {
        end -= 1;
    }
    let num = &trimmed[end..unit_end];
    if num.is_empty() {
        return None;
    }
    num.parse::<f64>().ok().map(|v| v * mult)
}

/// Pull a number+unit starting at the first non-whitespace char of `s`.
fn parse_size_starting_at(s: &str) -> Option<f64> {
    let trimmed = s.trim_start();
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let num: f64 = trimmed[..i].parse().ok()?;
    let mut j = i;
    while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
        j += 1;
    }
    let unit = &trimmed[i..j];
    let mult = unit_multiplier(unit)?;
    Some(num * mult)
}

fn unit_multiplier(unit: &str) -> Option<f64> {
    Some(match unit {
        "B" => 1.0,
        "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0_f64.powi(3),
        "TiB" => 1024.0_f64.powi(4),
        _ => return None,
    })
}

/// Layer counts: `3/8` (no units, plausibly small).
fn parse_int_ratio(line: &str) -> Option<f64> {
    for (slash, _) in line.match_indices('/') {
        let lhs = trailing_int(&line[..slash])?;
        let rhs = leading_int(&line[slash + 1..])?;
        if rhs > 0 && rhs < 10_000 && lhs <= rhs {
            return Some(lhs as f64 / rhs as f64);
        }
    }
    None
}

fn trailing_int(s: &str) -> Option<u64> {
    let bytes = s.trim_end().as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end == bytes.len() {
        return None;
    }
    std::str::from_utf8(&bytes[end..]).ok()?.parse().ok()
}

fn leading_int(s: &str) -> Option<u64> {
    let bytes = s.trim_start().as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    std::str::from_utf8(&bytes[..i]).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn percent() {
        assert_eq!(parse_progress(&lines(&["downloading 42%"])), Some(0.42));
        assert_eq!(parse_progress(&lines(&["downloading 42.5 %"])), Some(0.425));
    }
    #[test]
    fn byte_ratio() {
        let p = parse_progress(&lines(&["pulling 12MB/24MB layer abc"])).unwrap();
        assert!((p - 0.5).abs() < 1e-6);
        let p = parse_progress(&lines(&["1.5GiB/3GiB"])).unwrap();
        assert!((p - 0.5).abs() < 1e-6);
    }
    #[test]
    fn int_ratio() {
        assert_eq!(parse_progress(&lines(&["layers 3/8"])), Some(3.0 / 8.0));
    }
    #[test]
    fn newest_wins() {
        assert_eq!(
            parse_progress(&lines(&["10%", "50%", "ignore me"])),
            Some(0.5)
        );
    }
    #[test]
    fn nothing() {
        assert_eq!(parse_progress(&lines(&["nothing matches here"])), None);
    }
}
