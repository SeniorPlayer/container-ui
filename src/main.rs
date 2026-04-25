mod app;
mod cli;
mod container;
mod jsonhl;
mod prefs;
mod pullprog;
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

use crate::app::{App, Mode, Tab};

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
                match res {
                    Ok(()) => app.set_status("Pull complete."),
                    Err(e) => app.set_status(format!("Pull failed: {e}")),
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
    if !matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }
    // Block clicks while overlays are up (other than the help overlay, which
    // also closes on click anywhere).
    match app.mode {
        app::Mode::Help => {
            app.mode = app::Mode::Browse;
            return;
        }
        app::Mode::Detail | app::Mode::PromptPull | app::Mode::PullProgress => return,
        _ => {}
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
                    app.pull_reference = Some(reference.clone());
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
            app.tab = Tab::Logs;
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
