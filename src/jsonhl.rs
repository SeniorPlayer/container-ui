//! Tiny dependency-free JSON syntax highlighter for the inspect detail pane.
//!
//! Tokenizes already-pretty-printed JSON character-by-character and emits
//! `ratatui::text::Line`s with colored `Span`s. Not a strict parser — it's
//! permissive on purpose so it never panics on slightly-off output.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Copy, Clone, PartialEq, Eq)]
enum Ctx {
    /// Outside any string. Next `"..."` is a key if the surrounding context
    /// expects one (object key position).
    Default,
    /// Just saw `{` or `,` inside an object — next string is a key.
    ExpectKey,
    /// Just saw `:` — next value is a value (string here is non-key).
    ExpectValue,
}

pub fn highlight(text: &str) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(text.lines().count() + 1);
    for line in text.lines() {
        out.push(highlight_line(line));
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

fn highlight_line(line: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut chars = line.chars().peekable();
    let mut ctx = Ctx::Default;
    // Track whether we're at the start of a value position based on the last
    // structural char seen on this line. Multi-line context isn't tracked
    // (good enough for our use).
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                let mut s = String::new();
                while let Some(&p) = chars.peek() {
                    if p == ' ' || p == '\t' {
                        s.push(p);
                        chars.next();
                    } else {
                        break;
                    }
                }
                spans.push(Span::raw(s));
            }
            '{' | '[' => {
                chars.next();
                spans.push(styled(c.to_string(), bracket_style()));
                ctx = if c == '{' { Ctx::ExpectKey } else { Ctx::ExpectValue };
            }
            '}' | ']' => {
                chars.next();
                spans.push(styled(c.to_string(), bracket_style()));
                ctx = Ctx::Default;
            }
            ',' => {
                chars.next();
                spans.push(styled(",".to_string(), punct_style()));
                // After a comma, if we were producing values inside an object,
                // the next string is a key. Heuristic: if last bracket was {.
                // We approximate by toggling to ExpectKey whenever ctx was
                // ExpectValue (covers object value→next key) and leaving array
                // contexts as ExpectValue.
                if ctx == Ctx::ExpectValue {
                    ctx = Ctx::ExpectKey;
                }
            }
            ':' => {
                chars.next();
                spans.push(styled(":".to_string(), punct_style()));
                ctx = Ctx::ExpectValue;
            }
            '"' => {
                let s = consume_string(&mut chars);
                let style = if ctx == Ctx::ExpectKey {
                    key_style()
                } else {
                    string_style()
                };
                spans.push(styled(s, style));
                // After a key string, the next thing should be `:`. We don't
                // change ctx here; the `:` arm handles it.
                if ctx == Ctx::ExpectValue {
                    // value emitted, await comma or close
                }
            }
            c if c == '-' || c.is_ascii_digit() => {
                let s = consume_while(&mut chars, |c| {
                    c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')
                });
                spans.push(styled(s, number_style()));
            }
            c if c.is_ascii_alphabetic() => {
                let s = consume_while(&mut chars, |c| c.is_ascii_alphabetic());
                let style = match s.as_str() {
                    "true" | "false" => bool_style(),
                    "null" => null_style(),
                    _ => Style::default(),
                };
                spans.push(styled(s, style));
            }
            _ => {
                chars.next();
                spans.push(Span::raw(c.to_string()));
            }
        }
    }
    Line::from(spans)
}

fn consume_string<I>(chars: &mut std::iter::Peekable<I>) -> String
where
    I: Iterator<Item = char>,
{
    let mut s = String::new();
    let _ = chars.next(); // opening "
    s.push('"');
    let mut escaped = false;
    for c in chars.by_ref() {
        s.push(c);
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            break;
        }
    }
    s
}

fn consume_while<F, I>(chars: &mut std::iter::Peekable<I>, pred: F) -> String
where
    F: Fn(char) -> bool,
    I: Iterator<Item = char>,
{
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if pred(c) {
            s.push(c);
            chars.next();
        } else {
            break;
        }
    }
    s
}

fn styled(s: String, style: Style) -> Span<'static> {
    Span::styled(s, style)
}

fn key_style() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}
fn string_style() -> Style {
    Style::default().fg(Color::Green)
}
fn number_style() -> Style {
    Style::default().fg(Color::Magenta)
}
fn bool_style() -> Style {
    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
}
fn null_style() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
}
fn bracket_style() -> Style {
    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
}
fn punct_style() -> Style {
    Style::default().fg(Color::DarkGray)
}
