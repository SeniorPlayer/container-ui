//! Render the App. Pure ratatui — no I/O.

use crate::app::{App, Mode, Tab};
use crate::jsonhl;
use crate::pullprog;
use humansize::{format_size, BINARY};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Sparkline, Table, TableState, Tabs,
        Wrap,
    },
    Frame,
};

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header (tabs)
            Constraint::Length(5), // sparklines
            Constraint::Min(5),    // body
            Constraint::Length(1), // filter bar (always — empty when not filtering)
            Constraint::Length(1), // status
        ])
        .split(area);

    // Cache regions for mouse hit testing. Body region excludes the block
    // border so click-row math lines up with the rendered rows below.
    app.layout.tabs = Some(outer[0]);
    app.layout.body = Some(inner_body(outer[2]));

    draw_tabs(f, app, outer[0]);
    draw_sparklines(f, app, outer[1]);
    draw_body(f, app, outer[2]);
    draw_filter_bar(f, app, outer[3]);
    draw_status(f, app, outer[4]);

    // Overlays.
    match app.mode {
        Mode::Detail => draw_detail_overlay(f, app, area),
        Mode::PromptPull => draw_prompt_overlay(f, app, area),
        Mode::PromptBuild => draw_build_prompt_overlay(f, app, area),
        Mode::PullProgress => draw_pull_overlay(f, app, area),
        Mode::Help => draw_help_overlay(f, app, area),
        Mode::ContextMenu => draw_context_menu(f, app, area),
        Mode::Browse | Mode::Filter | Mode::LogSearch => {}
    }
}

/// Body Rect minus the block border (1 row top/bottom, 1 col left/right) and
/// the table header row (1).
fn inner_body(r: Rect) -> Rect {
    Rect {
        x: r.x.saturating_add(1),
        y: r.y.saturating_add(1).saturating_add(1),
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(3),
    }
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(Span::styled(t.label(), Style::default().fg(Color::White))))
        .collect();
    let idx = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    " cgui · Apple container front end ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
        )
        .select(idx)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_sparklines(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let cpu: Vec<u64> = app
        .cpu_history
        .iter()
        .map(|v| v.max(0.0).round() as u64)
        .collect();
    let mem: Vec<u64> = app
        .mem_history
        .iter()
        .map(|v| v.max(0.0).round() as u64)
        .collect();

    let cpu_now = app.cpu_history.back().copied().unwrap_or(0.0);
    let mem_now = app.mem_history.back().copied().unwrap_or(0.0);

    f.render_widget(
        Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" CPU  {cpu_now:>5.1}% (Σ across containers) ")),
            )
            .data(&cpu)
            .style(Style::default().fg(Color::Green)),
        cols[0],
    );
    f.render_widget(
        Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" MEM  {mem_now:>5.1}% of limit ")),
            )
            .data(&mem)
            .style(Style::default().fg(Color::Magenta)),
        cols[1],
    );
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    match app.tab {
        Tab::Containers => draw_containers(f, app, area),
        Tab::Images => draw_images(f, app, area),
        Tab::Volumes => draw_volumes(f, app, area),
        Tab::Networks => draw_networks(f, app, area),
        Tab::Logs => draw_logs(f, app, area),
    }
}

fn header_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn block_title(app: &App, label: &str, total: usize, shown: usize) -> String {
    let sort = app.sort_key.label(app.tab);
    if shown == total {
        format!(" {label} ({total}) · sort:{sort} ")
    } else {
        format!(" {label} ({shown}/{total}) · sort:{sort} · filter:{} ", app.filter)
    }
}

fn draw_containers(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["", "ID", "IMAGE", "STATUS", "CPU%", "MEM", "PORTS"])
        .style(header_style());
    let view = app.view_indices();
    let stats = app.stats_by_id();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let c = &app.containers[i];
            let status_style = match c.status.as_str() {
                "running" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                "stopped" | "exited" => Style::default().fg(Color::Red),
                _ => Style::default().fg(Color::Yellow),
            };
            let mark = if app.marked.contains(&c.id) {
                Cell::from("●").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            } else {
                Cell::from(" ")
            };

            // Live stats overlay if available (running container with a stats sample),
            // else fall back to the configured CPU count and memory limit.
            let (cpu_cell, mem_cell) = match stats.get(&c.id) {
                Some(&(cpu, used, limit)) => {
                    let cpu_str = format!("{cpu:>5.1}%");
                    let cpu_style = cpu_color(cpu);
                    let mem_str = if limit > 0 {
                        format!(
                            "{} / {}",
                            format_size(used, BINARY),
                            format_size(limit, BINARY)
                        )
                    } else {
                        format_size(used, BINARY)
                    };
                    let mem_pct = if limit > 0 {
                        (used as f64 / limit as f64) * 100.0
                    } else {
                        0.0
                    };
                    (
                        Cell::from(cpu_str).style(cpu_style),
                        Cell::from(mem_str).style(mem_color(mem_pct)),
                    )
                }
                None => (
                    Cell::from(format!("    {}", c.cpus))
                        .style(Style::default().fg(Color::DarkGray)),
                    Cell::from(format_size(c.memory_bytes, BINARY))
                        .style(Style::default().fg(Color::DarkGray)),
                ),
            };

            Row::new(vec![
                mark,
                Cell::from(c.id.clone()),
                Cell::from(c.image.clone()).style(Style::default().fg(Color::Blue)),
                Cell::from(c.status.clone()).style(status_style),
                cpu_cell,
                mem_cell,
                Cell::from(c.ports.join(", ")),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(2),
        Constraint::Percentage(22),
        Constraint::Percentage(30),
        Constraint::Length(10),
        Constraint::Length(7),
        Constraint::Length(20),
        Constraint::Min(10),
    ];
    let mark_count = app.marked.len();
    let mut title = block_title(app, "Containers", app.containers.len(), rows.len());
    if mark_count > 0 {
        title = format!("{title}· marked:{mark_count} ");
    }
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = TableState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn cpu_color(pct: f64) -> Style {
    if pct >= 80.0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if pct >= 40.0 {
        Style::default().fg(Color::Yellow)
    } else if pct > 0.0 {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
fn mem_color(pct: f64) -> Style {
    if pct >= 90.0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if pct >= 70.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Magenta)
    }
}

fn draw_images(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["REFERENCE", "SIZE", "DIGEST"]).style(header_style());
    let view = app.view_indices();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let im = &app.images[i];
            Row::new(vec![
                Cell::from(im.reference.clone()).style(Style::default().fg(Color::Blue)),
                Cell::from(im.size.clone()),
                Cell::from(short_digest(&im.digest)).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Percentage(50),
        Constraint::Length(12),
        Constraint::Min(10),
    ];
    let title = block_title(app, "Images", app.images.len(), rows.len());
    let mut state = TableState::default();
    state.select(Some(app.selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_volumes(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["NAME", "DRIVER", "SOURCE"]).style(header_style());
    let view = app.view_indices();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let v = &app.volumes[i];
            Row::new(vec![
                Cell::from(v.name.clone()),
                Cell::from(v.driver.clone()),
                Cell::from(v.source.clone()).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Percentage(30),
        Constraint::Length(10),
        Constraint::Min(20),
    ];
    let title = block_title(app, "Volumes", app.volumes.len(), rows.len());
    let mut state = TableState::default();
    state.select(Some(app.selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_networks(f: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["ID", "MODE", "STATE", "SUBNET"]).style(header_style());
    let view = app.view_indices();
    let rows: Vec<Row> = view
        .iter()
        .map(|&i| {
            let n = &app.networks[i];
            let state_style = if n.state == "running" {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            Row::new(vec![
                Cell::from(n.id.clone()),
                Cell::from(n.mode.clone()),
                Cell::from(n.state.clone()).style(state_style),
                Cell::from(n.subnet.clone()).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(20),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(20),
    ];
    let title = block_title(app, "Networks", app.networks.len(), rows.len());
    let mut state = TableState::default();
    state.select(Some(app.selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let title = match (&app.log_target, app.log_search.is_empty()) {
        (Some(id), true) => format!(" Logs · {id} (/ search · l reload · wheel scrolls) "),
        (Some(id), false) => format!(
            " Logs · {id} · search:{}  ({} matches) ",
            app.log_search,
            count_matches(&app.logs, &app.log_search)
        ),
        (None, _) => " Logs (select a container, press l) ".to_string(),
    };

    let lines: Vec<Line> = if app.logs.is_empty() {
        vec![Line::from("No logs loaded.")]
    } else if app.log_search.is_empty() {
        app.logs.lines().map(|l| Line::from(l.to_string())).collect()
    } else {
        app.logs
            .lines()
            .map(|l| highlight_search(l, &app.log_search))
            .collect()
    };

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.log_scroll, 0))
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn count_matches(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let needle = needle.to_lowercase();
    text.lines()
        .map(|l| {
            let lower = l.to_lowercase();
            let mut start = 0usize;
            let mut n = 0;
            while let Some(off) = lower[start..].find(&needle) {
                n += 1;
                start += off + needle.len().max(1);
            }
            n
        })
        .sum()
}

/// Render a single log line as a sequence of Spans, with case-insensitive
/// highlighting of any occurrence of `needle`. Preserves original casing.
fn highlight_search(line: &str, needle: &str) -> Line<'static> {
    if needle.is_empty() {
        return Line::from(line.to_string());
    }
    let lower = line.to_lowercase();
    let needle_lc = needle.to_lowercase();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize;
    while let Some(off) = lower[cursor..].find(&needle_lc) {
        let abs = cursor + off;
        if abs > cursor {
            spans.push(Span::raw(line[cursor..abs].to_string()));
        }
        let end = abs + needle.len();
        spans.push(Span::styled(
            line[abs..end].to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        cursor = end;
        if needle.is_empty() {
            break;
        }
    }
    if cursor < line.len() {
        spans.push(Span::raw(line[cursor..].to_string()));
    }
    Line::from(spans)
}

fn draw_filter_bar(f: &mut Frame, app: &App, area: Rect) {
    if app.mode == Mode::Filter {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow)),
            Span::raw(&app.filter),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::styled(
                "   (Enter apply · Esc cancel · Backspace)",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        f.render_widget(p, area);
    } else if app.mode == Mode::LogSearch {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow)),
            Span::raw(&app.log_search),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::styled(
                "   (search-as-you-type · Enter keep · Esc clear)",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        f.render_widget(p, area);
    } else if !app.filter.is_empty() {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.filter, Style::default().fg(Color::Yellow)),
            Span::styled("   (/ edit · Esc clear)", Style::default().fg(Color::DarkGray)),
        ]));
        f.render_widget(p, area);
    } else if app.tab == Tab::Logs && !app.log_search.is_empty() {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" search: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.log_search, Style::default().fg(Color::Yellow)),
            Span::styled("   (/ edit · Esc clear)", Style::default().fg(Color::DarkGray)),
        ]));
        f.render_widget(p, area);
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        format!(" {} ", app.status),
        Style::default().fg(Color::Black).bg(Color::White),
    )];
    // Background-pull indicator: visible whenever a pull is running OR finished
    // but not currently focused, prompting the user to re-attach with `P`.
    if app.pull_attachable() && app.mode != Mode::PullProgress {
        let pct = app
            .pull_log
            .lock()
            .ok()
            .and_then(|v| pullprog::parse_progress(&v))
            .map(|p| format!("{:.0}%", p * 100.0))
            .unwrap_or_else(|| "…".into());
        let label = match (&app.pull_reference, app.pull_running) {
            (Some(r), true) => format!(" ⟳ pulling {r} {pct}  P to view "),
            (Some(r), false) => format!(" ✓ pulled {r}  P to view "),
            (None, true) => " ⟳ pull running · P to view ".into(),
            (None, false) => " ✓ pull done · P to view ".into(),
        };
        let style = if app.pull_running {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(label, style));
    }
    let p = Paragraph::new(Line::from(spans));
    f.render_widget(p, area);
}

fn centered(area: Rect, w_pct: u16, h_pct: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - h_pct) / 2),
            Constraint::Percentage(h_pct),
            Constraint::Percentage((100 - h_pct) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - w_pct) / 2),
            Constraint::Percentage(w_pct),
            Constraint::Percentage((100 - w_pct) / 2),
        ])
        .split(v[1])[1]
}

fn draw_detail_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 80, 80);
    f.render_widget(Clear, r);
    let title = " Inspect (↑↓/PgUp/PgDn scroll · Esc close) ";
    let lines = jsonhl::highlight(&app.detail);
    let p = Paragraph::new(lines)
        .scroll((app.detail_scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(title, Style::default().fg(Color::Cyan))),
        );
    f.render_widget(p, r);
}

fn draw_prompt_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 60, 20);
    f.render_widget(Clear, r);
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            "Image reference to pull:",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.prompt_buf, Style::default().fg(Color::Yellow)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter pull · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Pull image "),
    );
    f.render_widget(body, r);
}

fn draw_help_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 70, 80);
    f.render_widget(Clear, r);

    let mut lines: Vec<Line> = Vec::new();
    let h = |k: &str, d: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("  {k:<14}"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::raw(d.to_string()),
        ])
    };
    let section = |title: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
    };

    lines.push(section("Global"));
    lines.push(h("q / Esc", "Quit (or clear filter/search if set)"));
    lines.push(h("Tab / →", "Next tab"));
    lines.push(h("Shift-Tab / ←", "Prev tab"));
    lines.push(h("↑ ↓ / j", "Move selection"));
    lines.push(h("Enter", "Inspect (open detail pane)"));
    lines.push(h("/", "Filter (Logs: search-as-you-type)"));
    lines.push(h("o", "Cycle sort key for current tab"));
    lines.push(h("r", "Refresh"));
    lines.push(h("a", "Toggle show-all vs running-only"));
    lines.push(h("?", "Toggle this help"));
    lines.push(h("Mouse", "Click tab title or row to select"));
    lines.push(Line::from(""));

    match app.tab {
        Tab::Containers => {
            lines.push(section("Containers"));
            lines.push(h("Space", "Mark / unmark for batch ops"));
            lines.push(h("s / x / K / d", "Start / stop / kill / delete"));
            lines.push(h("l", "Load logs into Logs tab"));
            lines.push(h("e", "Exec /bin/sh in selected container"));
        }
        Tab::Images => {
            lines.push(section("Images"));
            lines.push(h("p", "Pull image (prompt + progress modal)"));
            lines.push(h("P", "Re-attach to backgrounded pull"));
        }
        Tab::Volumes => {
            lines.push(section("Volumes"));
            lines.push(h("Enter", "Detail: capacity, on-disk, fill bar, JSON"));
        }
        Tab::Networks => {
            lines.push(section("Networks"));
            lines.push(h("Enter", "Inspect network JSON"));
        }
        Tab::Logs => {
            lines.push(section("Logs"));
            lines.push(h("/", "Search-as-you-type (highlight matches)"));
            lines.push(h("Esc", "Clear search"));
        }
    }
    lines.push(Line::from(""));
    lines.push(section("Pull modal"));
    lines.push(h("Esc", "Background the modal (status bar chip + P to re-attach)"));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press ? or Esc to close",
        Style::default().fg(Color::DarkGray),
    )));

    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                format!(" cgui · help · {} ", app.tab.label()),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(p, r);
}

fn draw_pull_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 80, 60);
    f.render_widget(Clear, r);

    let lines = match app.pull_log.lock() {
        Ok(v) => v.clone(),
        Err(_) => vec!["<lock poisoned>".to_string()],
    };
    let progress = pullprog::parse_progress(&lines);
    let status_line = pullprog::status_label(&lines);

    let participle = app.op_kind.participle();
    let done = app.op_kind.done();
    let title = match (&app.pull_reference, app.pull_running) {
        (Some(r), true) => format!(" {participle} {r} · Esc backgrounds (P re-attach) "),
        (Some(r), false) => format!(" {done} {r} · Esc closes "),
        (None, true) => format!(" {participle}… · Esc backgrounds (P re-attach) "),
        (None, false) => format!(" {done} · Esc closes "),
    };
    let border_color = if app.pull_running { app.theme.warning } else { app.theme.success };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, Style::default().fg(border_color).add_modifier(Modifier::BOLD)));
    let inner = block.inner(r);
    f.render_widget(block, r);

    // Reserve top 3 rows for the gauge, the rest for the stream.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    // --- Gauge ---
    let pct = progress.unwrap_or(0.0);
    let gauge_label = match (progress, app.pull_running) {
        (Some(p), _) => format!("{:>5.1}% — {}", p * 100.0, truncate(&status_line, inner.width.saturating_sub(20) as usize)),
        (None, true) => format!("…  {}", truncate(&status_line, inner.width.saturating_sub(8) as usize)),
        (None, false) => "done".to_string(),
    };
    let gauge_color = if !app.pull_running || pct >= 0.66 {
        app.theme.success
    } else if pct >= 0.33 {
        app.theme.warning
    } else {
        app.theme.accent
    };
    let g = Gauge::default()
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)))
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Black))
        .ratio(if app.pull_running { pct } else { 1.0 })
        .label(Span::styled(gauge_label, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)));
    f.render_widget(g, split[0]);

    // --- Stream body ---
    // When the user has scrolled (op_scroll > 0), show from the top with that
    // offset. Otherwise auto-tail to the last screenful so a long pull keeps
    // the latest line visible without extra interaction.
    let h = split[1].height as usize;
    let body = if app.op_scroll == 0 {
        let start = lines.len().saturating_sub(h);
        lines[start..].join("\n")
    } else {
        lines.join("\n")
    };
    let p = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .scroll((app.op_scroll, 0));
    f.render_widget(p, split[1]);
}

fn draw_build_prompt_overlay(f: &mut Frame, app: &App, area: Rect) {
    let r = centered(area, 70, 30);
    f.render_widget(Clear, r);
    let label = |text: &str, active: bool| -> Span<'static> {
        Span::styled(
            text.to_string(),
            if active {
                Style::default().fg(app.theme.warning).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            },
        )
    };
    let cursor_for = |i: u8| if app.build_field == i { "█" } else { " " };
    let body = Paragraph::new(vec![
        Line::from(label("Build context (path or URL):", app.build_field == 0)),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.build_path, Style::default().fg(app.theme.warning)),
            Span::styled(cursor_for(0), Style::default().fg(app.theme.warning)),
        ]),
        Line::from(""),
        Line::from(label("Tag (optional, e.g. myapp:latest):", app.build_field == 1)),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(&app.build_tag, Style::default().fg(app.theme.warning)),
            Span::styled(cursor_for(1), Style::default().fg(app.theme.warning)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Tab switches fields · Enter starts build · Esc cancels",
            Style::default().fg(app.theme.muted),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent))
            .title(Span::styled(
                " Build image ",
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(body, r);
}

fn draw_context_menu(f: &mut Frame, app: &App, area: Rect) {
    let Some(menu) = &app.context_menu else { return };
    let width: u16 = (menu
        .items
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(10) as u16)
        .saturating_add(4);
    let height: u16 = (menu.items.len() as u16).saturating_add(2);
    // Anchor near the click but keep on-screen.
    let x = menu.x.min(area.width.saturating_sub(width));
    let y = menu.y.min(area.height.saturating_sub(height));
    let r = Rect { x, y, width, height };
    f.render_widget(Clear, r);

    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let is_sel = i == menu.selected;
            let style = if is_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.primary)
            };
            Line::from(Span::styled(format!(" {label} "), style))
        })
        .collect();
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.accent)),
    );
    f.render_widget(p, r);
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 || s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn short_digest(d: &str) -> String {
    d.split(':').nth(1).map(|s| s[..s.len().min(12)].to_string()).unwrap_or_default()
}
