mod app;
mod cli;
mod container;
mod jsonhl;
mod prefs;
mod pullprog;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io::stdout, time::Duration};
use tokio::time::{interval, MissedTickBehavior};

use crate::app::{App, ContextAction, ContextMenu, Mode, OperationKind, Tab};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    if let Some(code) = cli::dispatch_cli(&cli)? {
        std::process::exit(code);
    }
    run_tui().await
}

async fn run_tui() -> Result<()> {
    enter_terminal()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    let result = event_loop(&mut term).await;

    leave_terminal()?;
    term.show_cursor()?;
    result
}

fn enter_terminal() -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(())
}

fn leave_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

async fn event_loop<B: ratatui::backend::Backend>(term: &mut Terminal<B>) -> Result<()> {
    let mut app = App::new();
    app.refresh().await.ok();

    let mut events = EventStream::new();
    let mut tick = interval(Duration::from_millis(2000));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut redraw = interval(Duration::from_millis(150));
    redraw.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut pull_handle: Option<tokio::task::JoinHandle<Result<()>>> = None;

    while app.running {
        // Reap finished pull task.
        if let Some(h) = pull_handle.as_ref() {
            if h.is_finished() {
                let h = pull_handle.take().unwrap();
                let res = h.await.unwrap_or_else(|e| Err(anyhow::anyhow!("join: {e}")));
                app.pull_running = false;
                let verb = app.op_kind.verb();
                match res {
                    Ok(()) => app.set_status(format!("{verb} complete.")),
                    Err(e) => app.set_status(format!("{verb} failed: {e}")),
                }
                app.refresh().await.ok();
            }
        }

        term.draw(|f| ui::draw(f, &mut app))?;

        tokio::select! {
            _ = tick.tick() => {
                if matches!(app.mode, Mode::Browse | Mode::Filter | Mode::PullProgress | Mode::Detail) {
                    app.refresh().await.ok();
                }
            }
            _ = redraw.tick() => { /* re-render only */ }
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) => {
                        if k.kind != crossterm::event::KeyEventKind::Press { continue; }
                        handle_key(term, &mut app, &mut pull_handle, k.code, k.modifiers).await?;
                    }
                    Some(Ok(Event::Mouse(m))) => handle_mouse(&mut app, m).await,
                    _ => {}
                }
            }
        }
    }
    app.save_prefs();
    Ok(())
}

async fn handle_mouse(app: &mut App, m: MouseEvent) {
    // Wheel scroll first — works in any mode that has a scrollable view.
    match m.kind {
        MouseEventKind::ScrollDown => return wheel(app, 3),
        MouseEventKind::ScrollUp => return wheel(app, -3),
        _ => {}
    }

    // Right-click → context menu (browse mode only).
    if let MouseEventKind::Down(MouseButton::Right) = m.kind {
        if app.mode == Mode::Browse {
            open_context_menu(app, m.column, m.row);
        }
        return;
    }

    // From here on, only handle left-clicks.
    if !matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }

    // Help overlay dismisses on any click.
    if app.mode == Mode::Help {
        app.mode = Mode::Browse;
        return;
    }
    // Context menu: click an item to activate, click elsewhere to dismiss.
    if app.mode == Mode::ContextMenu {
        let menu_rect = context_menu_rect(app);
        if let (Some(menu), Some(r)) = (app.context_menu.clone(), menu_rect) {
            if hit(r, m.column, m.row) {
                let idx = (m.row.saturating_sub(r.y).saturating_sub(1)) as usize;
                if idx < menu.items.len() {
                    let action = menu.items[idx].1;
                    app.mode = Mode::Browse;
                    app.context_menu = None;
                    invoke_context_action(app, action).await;
                    return;
                }
            }
        }
        app.mode = Mode::Browse;
        app.context_menu = None;
        return;
    }
    // Other overlays swallow left-clicks rather than mis-firing on chrome.
    if matches!(
        app.mode,
        Mode::Detail | Mode::PromptPull | Mode::PromptBuild | Mode::PullProgress
    ) {
        return;
    }

    if let Some(tabs) = app.layout.tabs {
        if hit(tabs, m.column, m.row) {
            if let Some(t) = tab_from_x(tabs, m.column) {
                app.set_tab(t);
            }
            return;
        }
    }
    if let Some(body) = app.layout.body {
        if hit(body, m.column, m.row) {
            let row = (m.row.saturating_sub(body.y)) as usize;
            let n = app.row_count();
            if n > 0 && row < n {
                app.selected = row;
            }
        }
    }
}

fn wheel(app: &mut App, delta: i32) {
    let bump = |s: &mut u16| {
        if delta > 0 {
            *s = s.saturating_add(delta as u16);
        } else {
            *s = s.saturating_sub((-delta) as u16);
        }
    };
    match app.mode {
        Mode::Detail => bump(&mut app.detail_scroll),
        Mode::PullProgress => bump(&mut app.op_scroll),
        Mode::Help | Mode::PromptPull | Mode::PromptBuild | Mode::ContextMenu => {}
        _ => {
            if app.tab == Tab::Logs {
                bump(&mut app.log_scroll);
            }
        }
    }
}

fn open_context_menu(app: &mut App, x: u16, y: u16) {
    let items: Vec<(String, ContextAction)> = match app.tab {
        Tab::Containers => vec![
            ("Inspect".into(), ContextAction::Inspect),
            ("Logs".into(), ContextAction::Logs),
            ("Start".into(), ContextAction::Start),
            ("Stop".into(), ContextAction::Stop),
            ("Kill".into(), ContextAction::Kill),
            ("Delete".into(), ContextAction::Delete),
            ("Exec /bin/sh".into(), ContextAction::Exec),
            ("Refresh".into(), ContextAction::Refresh),
            ("Toggle show-all".into(), ContextAction::ToggleAll),
            ("Help".into(), ContextAction::Help),
        ],
        Tab::Images => vec![
            ("Inspect".into(), ContextAction::Inspect),
            ("Pull image…".into(), ContextAction::Pull),
            ("Delete".into(), ContextAction::Delete),
            ("Refresh".into(), ContextAction::Refresh),
            ("Help".into(), ContextAction::Help),
        ],
        Tab::Volumes | Tab::Networks => vec![
            ("Inspect".into(), ContextAction::Inspect),
            ("Refresh".into(), ContextAction::Refresh),
            ("Help".into(), ContextAction::Help),
        ],
        Tab::Logs => vec![
            ("Refresh".into(), ContextAction::Refresh),
            ("Help".into(), ContextAction::Help),
        ],
    };
    // Snap selection to the row under the cursor where useful.
    if let Some(body) = app.layout.body {
        if hit(body, x, y) {
            let row = (y.saturating_sub(body.y)) as usize;
            let n = app.row_count();
            if n > 0 && row < n {
                app.selected = row;
            }
        }
    }
    app.context_menu = Some(ContextMenu {
        x,
        y,
        items,
        selected: 0,
    });
    app.mode = Mode::ContextMenu;
}

fn context_menu_rect(app: &App) -> Option<ratatui::layout::Rect> {
    let area = app.layout.body?; // approximation of total drawable area
    let menu = app.context_menu.as_ref()?;
    let width: u16 = (menu
        .items
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(10) as u16)
        .saturating_add(4);
    let height: u16 = (menu.items.len() as u16).saturating_add(2);
    let max_x = area.x + area.width;
    let max_y = area.y + area.height;
    let x = menu.x.min(max_x.saturating_sub(width));
    let y = menu.y.min(max_y.saturating_sub(height));
    Some(ratatui::layout::Rect { x, y, width, height })
}

async fn invoke_context_action(app: &mut App, action: ContextAction) {
    match action {
        ContextAction::Inspect => open_detail(app).await,
        ContextAction::Logs => load_logs(app).await,
        ContextAction::Start => batch_action(app, "start").await,
        ContextAction::Stop => batch_action(app, "stop").await,
        ContextAction::Kill => batch_action(app, "kill").await,
        ContextAction::Delete => batch_action(app, "delete").await,
        ContextAction::Exec => app.set_status("exec from menu: press 'e' on the row"),
        ContextAction::Pull => {
            app.prompt_buf.clear();
            app.mode = Mode::PromptPull;
            app.set_status("Type image reference, Enter to pull");
        }
        ContextAction::Refresh => {
            app.set_status("Refreshing…");
            app.refresh().await.ok();
            app.set_status("Refreshed.");
        }
        ContextAction::ToggleAll => {
            app.show_all = !app.show_all;
            app.save_prefs();
            app.refresh().await.ok();
        }
        ContextAction::Help => app.mode = Mode::Help,
    }
}

fn hit(r: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Map an x coordinate inside the tab bar to a Tab. ratatui's `Tabs` widget
/// renders titles separated by " │ " (length 3) inside a 1-col bordered box,
/// each title padded by 1 space on each side. We replicate that math here.
fn tab_from_x(tabs_rect: ratatui::layout::Rect, x: u16) -> Option<app::Tab> {
    let inside = x.checked_sub(tabs_rect.x.saturating_add(1))?; // skip border
    // Each rendered tab takes: " label "  (len + 2). Separator " │ " (3).
    let mut cursor: u16 = 0;
    for (i, t) in app::Tab::ALL.iter().enumerate() {
        let label_len = t.label().chars().count() as u16 + 2;
        if inside >= cursor && inside < cursor + label_len {
            return Some(app::Tab::ALL[i]);
        }
        cursor = cursor + label_len + 3;
    }
    None
}

async fn handle_key<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
    pull_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    code: KeyCode,
    mods: KeyModifiers,
) -> Result<()> {
    // Mode-specific input first.
    match app.mode.clone() {
        Mode::Filter => {
            match code {
                KeyCode::Esc => {
                    app.filter.clear();
                    app.mode = Mode::Browse;
                    app.selected = 0;
                    app.reset_status();
                }
                KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    app.set_status(format!("filter applied: {}", app.filter));
                }
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.selected = 0;
                }
                KeyCode::Char(c) => {
                    app.filter.push(c);
                    app.selected = 0;
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::PromptPull => {
            match code {
                KeyCode::Esc => {
                    app.prompt_buf.clear();
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Enter => {
                    let reference = std::mem::take(&mut app.prompt_buf);
                    if reference.trim().is_empty() {
                        app.mode = Mode::Browse;
                        app.set_status("pull cancelled (empty reference)");
                        return Ok(());
                    }
                    if let Ok(mut v) = app.pull_log.lock() {
                        v.clear();
                    }
                    app.pull_running = true;
                    app.op_kind = OperationKind::Pull;
                    app.pull_reference = Some(reference.clone());
                    app.op_scroll = 0;
                    *pull_handle = Some(container::spawn_pull(reference.clone(), app.pull_log.clone()));
                    app.mode = Mode::PullProgress;
                    app.set_status(format!("pulling {reference}…"));
                }
                KeyCode::Backspace => {
                    app.prompt_buf.pop();
                }
                KeyCode::Char(c) => {
                    app.prompt_buf.push(c);
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::Detail => {
            match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    app.detail_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.detail_scroll = app.detail_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.detail_scroll = app.detail_scroll.saturating_sub(1);
                }
                KeyCode::PageDown => {
                    app.detail_scroll = app.detail_scroll.saturating_add(20);
                }
                KeyCode::PageUp => {
                    app.detail_scroll = app.detail_scroll.saturating_sub(20);
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::PullProgress => {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                app.mode = Mode::Browse;
                if app.pull_running {
                    app.set_status("pull running in background · P to re-attach");
                }
            }
            return Ok(());
        }
        Mode::Help => {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter) {
                app.mode = Mode::Browse;
            }
            return Ok(());
        }
        Mode::ContextMenu => {
            let len = app.context_menu.as_ref().map(|m| m.items.len()).unwrap_or(0);
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.mode = Mode::Browse;
                    app.context_menu = None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(m) = app.context_menu.as_mut() {
                        if !m.items.is_empty() {
                            m.selected = (m.selected + 1).min(m.items.len() - 1);
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(m) = app.context_menu.as_mut() {
                        if m.selected > 0 {
                            m.selected -= 1;
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(m) = app.context_menu.as_ref() {
                        if !m.items.is_empty() && m.selected < len {
                            let action = m.items[m.selected].1;
                            app.mode = Mode::Browse;
                            app.context_menu = None;
                            invoke_context_action(app, action).await;
                        }
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::PromptBuild => {
            match code {
                KeyCode::Esc => {
                    app.build_path.clear();
                    app.build_tag.clear();
                    app.build_field = 0;
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Tab => app.build_field = if app.build_field == 0 { 1 } else { 0 },
                KeyCode::Enter => {
                    let path = app.build_path.trim().to_string();
                    if path.is_empty() {
                        app.set_status("build cancelled (empty context path)");
                        app.mode = Mode::Browse;
                        return Ok(());
                    }
                    let tag = if app.build_tag.trim().is_empty() {
                        None
                    } else {
                        Some(app.build_tag.trim().to_string())
                    };
                    if let Ok(mut v) = app.pull_log.lock() {
                        v.clear();
                    }
                    app.pull_running = true;
                    app.op_kind = OperationKind::Build;
                    app.pull_reference = Some(tag.clone().unwrap_or_else(|| path.clone()));
                    app.op_scroll = 0;
                    *pull_handle = Some(container::spawn_build(path.clone(), tag, app.pull_log.clone()));
                    app.mode = Mode::PullProgress;
                    app.set_status(format!("building {path}…"));
                }
                KeyCode::Backspace => {
                    if app.build_field == 0 {
                        app.build_path.pop();
                    } else {
                        app.build_tag.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if app.build_field == 0 {
                        app.build_path.push(c);
                    } else {
                        app.build_tag.push(c);
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::LogSearch => {
            match code {
                KeyCode::Esc => {
                    app.log_search.clear();
                    app.mode = Mode::Browse;
                    app.reset_status();
                }
                KeyCode::Enter => {
                    app.mode = Mode::Browse;
                    if app.log_search.is_empty() {
                        app.reset_status();
                    } else {
                        app.set_status(format!("search: {}", app.log_search));
                    }
                }
                KeyCode::Backspace => {
                    app.log_search.pop();
                }
                KeyCode::Char(c) => {
                    app.log_search.push(c);
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::Browse => {}
    }

    // Browse mode.
    match code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if !app.filter.is_empty() {
                app.filter.clear();
                app.selected = 0;
                app.reset_status();
            } else if app.tab == Tab::Logs && !app.log_search.is_empty() {
                app.log_search.clear();
                app.reset_status();
            } else {
                app.running = false;
            }
        }
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.running = false,
        KeyCode::Tab | KeyCode::Right => app.next_tab(),
        KeyCode::BackTab | KeyCode::Left => app.prev_tab(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Up => app.move_up(),
        KeyCode::Char('r') => {
            app.set_status("Refreshing…");
            app.refresh().await.ok();
            app.set_status("Refreshed.");
        }
        KeyCode::Char('a') => {
            app.show_all = !app.show_all;
            app.set_status(if app.show_all {
                "Showing all"
            } else {
                "Showing running only"
            });
            app.save_prefs();
            app.refresh().await.ok();
        }
        KeyCode::Char('?') => {
            app.mode = Mode::Help;
        }
        KeyCode::Char('/') => {
            if app.tab == Tab::Logs {
                app.mode = Mode::LogSearch;
                app.set_status("Search logs…");
            } else {
                app.mode = Mode::Filter;
                app.set_status("Filter…");
            }
        }
        KeyCode::Char('P') => {
            if app.pull_attachable() {
                app.mode = Mode::PullProgress;
                app.set_status("re-attached to pull");
            } else {
                app.set_status("no pull to re-attach");
            }
        }
        KeyCode::Char('o') => {
            app.sort_key = app.sort_key.cycle(app.tab);
            app.selected = 0;
            app.sort_keys
                .insert(app.tab.key().to_string(), app.sort_key.0);
            app.save_prefs();
            app.set_status(format!("sort: {}", app.sort_key.label(app.tab)));
        }
        KeyCode::Enter => open_detail(app).await,
        KeyCode::Char('p') if app.tab == Tab::Images => {
            app.prompt_buf.clear();
            app.mode = Mode::PromptPull;
            app.set_status("Type image reference, Enter to pull");
        }
        KeyCode::Char('b') if app.tab == Tab::Images => {
            app.build_path.clear();
            app.build_tag.clear();
            app.build_field = 0;
            app.mode = Mode::PromptBuild;
            app.set_status("Build context path, then Tab → tag, Enter to start");
        }
        KeyCode::Char(' ') if app.tab == Tab::Containers => {
            app.toggle_mark_current_container();
            app.move_down();
        }
        KeyCode::Char('s') if app.tab == Tab::Containers => batch_action(app, "start").await,
        KeyCode::Char('x') if app.tab == Tab::Containers => batch_action(app, "stop").await,
        KeyCode::Char('K') if app.tab == Tab::Containers => batch_action(app, "kill").await,
        KeyCode::Char('d') if app.tab == Tab::Containers => batch_action(app, "delete").await,
        KeyCode::Char('l') if app.tab == Tab::Containers => load_logs(app).await,
        KeyCode::Char('e') if app.tab == Tab::Containers => exec_shell(term, app).await?,
        _ => {}
    }
    Ok(())
}

/// Run a lifecycle verb against either the marked set (if any) or the
/// currently highlighted row. Aggregates ok/err per id into a one-line status.
async fn batch_action(app: &mut App, verb: &str) {
    let ids = app.target_container_ids();
    if ids.is_empty() {
        app.set_status("No selection.");
        return;
    }
    let n = ids.len();
    if n == 1 {
        app.set_status(format!("{verb} {}…", ids[0]));
    } else {
        app.set_status(format!("{verb} ×{n}…"));
    }

    let mut ok = 0usize;
    let mut errs: Vec<String> = Vec::new();
    for id in &ids {
        let r = match verb {
            "start" => container::start(id).await,
            "stop" => container::stop(id).await,
            "kill" => container::kill(id).await,
            "delete" => container::delete(id).await,
            _ => Ok(()),
        };
        match r {
            Ok(()) => ok += 1,
            Err(e) => errs.push(format!("{id}: {e}")),
        }
    }

    if errs.is_empty() {
        app.set_status(format!("{verb} ok ({ok}/{n})"));
    } else {
        let first = errs.into_iter().next().unwrap_or_default();
        app.set_status(format!("{verb} {ok}/{n} ok · err: {first}"));
    }

    // Drop marks for any ids that no longer exist (deleted) — safest to just
    // clear them all on a successful batch verb so subsequent actions don't
    // accidentally re-target.
    if ok == n && matches!(verb, "delete") {
        app.marked.clear();
    }
    app.refresh().await.ok();
}

async fn load_logs(app: &mut App) {
    let Some(id) = app.current_container_id() else {
        app.set_status("No selection.");
        return;
    };
    app.set_status(format!("loading logs for {id}…"));
    match container::logs(&id, 500).await {
        Ok(s) => {
            app.logs = s;
            app.log_target = Some(id);
            app.log_scroll = 0;
            app.set_tab(Tab::Logs);
            app.set_status("Logs loaded.");
        }
        Err(e) => app.set_status(format!("logs error: {e}")),
    }
}

async fn open_detail(app: &mut App) {
    let target = match app.tab {
        Tab::Containers => app.current_container_id(),
        Tab::Images => app.current_image_ref(),
        Tab::Networks => app
            .selected_row()
            .and_then(|i| app.networks.get(i).map(|n| n.id.clone())),
        Tab::Volumes => app
            .selected_row()
            .and_then(|i| app.volumes.get(i).map(|v| v.name.clone())),
        Tab::Logs => None,
    };
    let Some(id) = target else {
        app.set_status("No selection to inspect.");
        return;
    };
    app.set_status(format!("inspecting {id}…"));
    let result = match app.tab {
        Tab::Volumes => container::volume_detail(&id).await,
        _ => container::inspect(&id).await,
    };
    match result {
        Ok(s) => {
            app.detail = s;
            app.detail_scroll = 0;
            app.mode = Mode::Detail;
            app.set_status(format!("inspect {id}"));
        }
        Err(e) => app.set_status(format!("inspect error: {e}")),
    }
}

/// Drop into `container exec -ti <id> /bin/sh` for the selected container.
/// We tear the TUI down (leave alt screen, leave raw mode) so the child can
/// own the terminal, then rebuild it on return.
async fn exec_shell<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    let Some(id) = app.current_container_id() else {
        app.set_status("No selection.");
        return Ok(());
    };

    leave_terminal()?;
    println!("\n--- cgui exec → container exec -ti {id} /bin/sh (Ctrl-D to return) ---\n");

    // Try sh; the user can re-run this if they want bash.
    let status = std::process::Command::new("container")
        .args(["exec", "-ti", &id, "/bin/sh"])
        .status();

    enter_terminal()?;
    term.clear()?;

    match status {
        Ok(s) if s.success() => app.set_status(format!("exec {id}: exited 0")),
        Ok(s) => app.set_status(format!("exec {id}: exited {s}")),
        Err(e) => app.set_status(format!("exec {id}: spawn error: {e}")),
    }
    app.refresh().await.ok();
    Ok(())
}
