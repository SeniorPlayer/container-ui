//! Background tasks that the TUI cares about:
//!
//! * **FSEvents** on `~/.config/cgui/stacks/` — emit `Event::StacksChanged`
//!   when a `*.toml` file is created, modified, or removed so the TUI can
//!   reload the stack list without the user pressing `r`.
//! * **Restart watcher** — every ~10 s, check each running stack's services
//!   against their `restart` policy and re-run any that have stopped.
//! * **Healthcheck loop** — every ~10 s, run each service's healthcheck and
//!   publish the result as `Event::Health`. We intentionally keep the loop
//!   coarse-grained (10 s) and rely on the per-service `interval_s` to
//!   debounce expensive probes.

use crate::stacks::{self, Healthcheck, RestartPolicy, Service, Stack};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub enum Event {
    StacksChanged,
    Health {
        stack: String,
        service: String,
        ok: bool,
        message: String,
    },
    Status(String),
}

/// Spawn the FSEvents watcher in a blocking thread; events flow through `tx`.
/// Returns the watcher handle so it lives at least as long as the channel.
pub fn spawn_fs_watcher(tx: UnboundedSender<Event>) -> Option<notify::RecommendedWatcher> {
    use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    let dir = stacks::stacks_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let tx_inner = tx.clone();
    let mut w: RecommendedWatcher = match RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(
                    ev.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    let _ = tx_inner.send(Event::StacksChanged);
                }
            }
        },
        Config::default(),
    ) {
        Ok(w) => w,
        Err(_) => return None,
    };
    if w.watch(&dir, RecursiveMode::NonRecursive).is_err() {
        return None;
    }
    Some(w)
}

/// Spawn the periodic restart + healthcheck loop. Runs forever; the caller
/// keeps a JoinHandle if it wants to abort it on shutdown. Loads stack files
/// from disk each tick so reconfiguration is picked up automatically.
pub fn spawn_restart_health(tx: UnboundedSender<Event>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Per (stack, svc): when last health probe fired.
        let mut last_probe: std::collections::HashMap<(String, String), Instant> =
            std::collections::HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            let stacks_now = stacks::load_all();
            for stack in &stacks_now {
                for svc in &stack.services {
                    // --- restart policy ---
                    if matches!(svc.restart_policy(), RestartPolicy::Always | RestartPolicy::OnFailure) {
                        let name = stacks::container_name(&stack.name, &svc.name);
                        if let Some(state) = container_state(&name).await {
                            let should_restart = match svc.restart_policy() {
                                RestartPolicy::Always => state == "stopped" || state == "exited",
                                RestartPolicy::OnFailure => state == "exited",
                                _ => false,
                            };
                            if should_restart {
                                let _ = tx.send(Event::Status(format!(
                                    "restart: {} ({state}) → start",
                                    name
                                )));
                                let _ = run_start(&name).await;
                            }
                        }
                    }
                    // --- healthcheck ---
                    if let Some(hc) = &svc.healthcheck {
                        let key = (stack.name.clone(), svc.name.clone());
                        let due = match last_probe.get(&key) {
                            Some(t) => t.elapsed() >= Duration::from_secs(hc.interval_s.max(1)),
                            None => true,
                        };
                        if !due {
                            continue;
                        }
                        last_probe.insert(key.clone(), Instant::now());
                        let (ok, message) = probe(stack, svc, hc).await;
                        let _ = tx.send(Event::Health {
                            stack: stack.name.clone(),
                            service: svc.name.clone(),
                            ok,
                            message,
                        });
                    }
                }
            }
        }
    })
}

async fn container_state(name: &str) -> Option<String> {
    // `container inspect` returns an array; we just want top-level "status".
    let out = tokio::process::Command::new(crate::runtime::binary())
        .args(["inspect", name])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = v.as_array()?;
    let first = arr.first()?;
    Some(
        first
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string(),
    )
}

async fn run_start(name: &str) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new(crate::runtime::binary())
        .args(["start", name])
        .output()
        .await
}

async fn probe(stack: &Stack, svc: &Service, hc: &Healthcheck) -> (bool, String) {
    match hc.kind.as_str() {
        "cmd" => probe_cmd(stack, svc, hc).await,
        _ => probe_tcp(svc, hc).await,
    }
}

/// TCP healthcheck. Looks at the service's `ports` list for an entry like
/// `<host>:<container>` matching `target` on either side, then attempts a
/// 1 s TCP connect to `127.0.0.1:<host>`. If `target` is empty, falls back
/// to the first published port.
async fn probe_tcp(svc: &Service, hc: &Healthcheck) -> (bool, String) {
    let want: Option<&str> = hc.target.as_deref();
    // Collect (host, container) pairs from the published ports list.
    let pairs: Vec<(String, String)> = svc
        .ports
        .iter()
        .filter_map(|p| p.split_once(':').map(|(h, c)| (h.to_string(), c.to_string())))
        .collect();
    let port: Option<String> = match want {
        None => pairs.first().map(|(h, _)| h.clone()),
        Some(t) => pairs
            .iter()
            .find(|(h, c)| h == t || c == t)
            .map(|(h, _)| h.clone())
            .or_else(|| Some(t.to_string())),
    };
    let Some(port) = port else {
        return (false, "no published port for tcp probe".into());
    };
    let addr = format!("127.0.0.1:{port}");
    let timeout = Duration::from_secs(1);
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => (true, format!("tcp {addr} ok")),
        Ok(Err(e)) => (false, format!("tcp {addr}: {e}")),
        Err(_) => (false, format!("tcp {addr}: timeout")),
    }
}

/// Exec a command inside the container; success = exit 0.
async fn probe_cmd(stack: &Stack, svc: &Service, hc: &Healthcheck) -> (bool, String) {
    if hc.command.is_empty() {
        return (false, "healthcheck.command is empty".into());
    }
    let name = stacks::container_name(&stack.name, &svc.name);
    let mut args: Vec<String> = vec!["exec".into(), name.clone()];
    args.extend(hc.command.iter().cloned());
    let out = tokio::process::Command::new(crate::runtime::binary())
        .args(args.iter().map(|s| s.as_str()))
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => (true, format!("exec ok ({})", hc.command.join(" "))),
        Ok(o) => (
            false,
            format!(
                "exec exit {}: {}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stderr).trim()
            ),
        ),
        Err(e) => (false, format!("exec spawn: {e}")),
    }
}
