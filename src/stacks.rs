//! Lightweight compose-style stacks. Each stack is a TOML file in
//! `$XDG_CONFIG_HOME/cgui/stacks/<name>.toml` describing 1..N services.
//! Bringing a stack up runs `container run -d --name <stack>_<svc> ...` per
//! service; bringing it down stops + deletes those containers.
//!
//! Schema:
//!
//! ```toml
//! name = "myapp"
//!
//! [[service]]
//! name = "db"
//! image = "docker.io/pgvector/pgvector:pg16"
//! env = { POSTGRES_USER = "test", POSTGRES_PASSWORD = "test" }
//! ports = ["15432:5432"]
//! volumes = ["dbdata:/var/lib/postgresql/data"]
//! network = "default"
//!
//! [[service]]
//! name = "api"
//! image = "myapp/api:latest"
//! depends_on = ["db"]
//! ```

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Deserialize)]
pub struct Stack {
    pub name: String,
    #[serde(rename = "service", default)]
    pub services: Vec<Service>,
    /// Path the stack was loaded from (None for synthesized stacks).
    #[serde(skip)]
    pub source: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Service {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    pub network: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Load every `*.toml` in the stacks dir.
pub fn load_all() -> Vec<Stack> {
    let Some(dir) = stacks_dir() else { return vec![] };
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out: Vec<Stack> = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(mut stack) = toml::from_str::<Stack>(&s) {
                stack.source = Some(p);
                out.push(stack);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn stacks_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cgui").join("stacks"))
}

/// Container name prefix: `<stack>_<service>`.
pub fn container_name(stack: &str, service: &str) -> String {
    format!("{stack}_{service}")
}

/// Build the `container run` argv for a single service.
pub fn run_args(stack: &str, svc: &Service) -> Vec<String> {
    let mut a: Vec<String> = vec!["run".into(), "-d".into(), "--name".into(), container_name(stack, &svc.name)];
    for (k, v) in &svc.env {
        a.push("-e".into());
        a.push(format!("{k}={v}"));
    }
    for p in &svc.ports {
        a.push("-p".into());
        a.push(p.clone());
    }
    for v in &svc.volumes {
        a.push("-v".into());
        a.push(v.clone());
    }
    if let Some(n) = &svc.network {
        a.push("--network".into());
        a.push(n.clone());
    }
    a.push(svc.image.clone());
    a.extend(svc.args.iter().cloned());
    a
}

/// Order services so `depends_on` comes before dependents (stable topo sort).
/// Cycles fall back to source order, since the operation will surface its
/// own error from `container run` anyway.
pub fn topo_order(stack: &Stack) -> Vec<&Service> {
    use std::collections::{HashMap, HashSet};
    let by_name: HashMap<&str, &Service> = stack
        .services
        .iter()
        .map(|s| (s.name.as_str(), s))
        .collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut order: Vec<&Service> = Vec::new();
    fn visit<'a>(
        name: &'a str,
        by_name: &HashMap<&'a str, &'a Service>,
        visited: &mut HashSet<&'a str>,
        order: &mut Vec<&'a Service>,
    ) {
        if visited.contains(name) {
            return;
        }
        visited.insert(name);
        if let Some(s) = by_name.get(name) {
            for dep in &s.depends_on {
                visit(dep.as_str(), by_name, visited, order);
            }
            order.push(s);
        }
    }
    for s in &stack.services {
        visit(s.name.as_str(), &by_name, &mut visited, &mut order);
    }
    order
}

/// Spawn an "up" pipeline: run each service in dependency order, streaming
/// status lines into `sink`. Errors from any one service are surfaced but
/// the rest still run (mirrors compose's default behavior).
pub fn spawn_up(stack: Stack, sink: Arc<Mutex<Vec<String>>>) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        push(&sink, format!("$ stack up: {}", stack.name));
        let mut errors: Vec<String> = Vec::new();
        for svc in topo_order(&stack) {
            let args = run_args(&stack.name, svc);
            push(
                &sink,
                format!("→ {} ({}): container {}", svc.name, svc.image, args.join(" ")),
            );
            let bin = crate::runtime::binary();
            match tokio::process::Command::new(&bin)
                .args(args.iter().map(|s| s.as_str()))
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    push(&sink, format!("  ✓ {}", id));
                }
                Ok(o) => {
                    let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    push(&sink, format!("  ✗ {}: {}", svc.name, msg));
                    errors.push(svc.name.clone());
                }
                Err(e) => {
                    push(&sink, format!("  ✗ {}: spawn error: {}", svc.name, e));
                    errors.push(svc.name.clone());
                }
            }
        }
        if errors.is_empty() {
            push(&sink, format!("✓ stack up: {} ({} services)", stack.name, stack.services.len()));
            Ok(())
        } else {
            let msg = format!("✗ stack up partial: {} failed", errors.join(", "));
            push(&sink, msg.clone());
            Err(anyhow!(msg))
        }
    })
}

/// Spawn a "down" pipeline: stop+delete every service container in reverse
/// dependency order. Missing containers are not errors (down is idempotent).
pub fn spawn_down(stack: Stack, sink: Arc<Mutex<Vec<String>>>) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        push(&sink, format!("$ stack down: {}", stack.name));
        let bin = crate::runtime::binary();
        let order: Vec<&Service> = topo_order(&stack);
        for svc in order.into_iter().rev() {
            let name = container_name(&stack.name, &svc.name);
            // Stop (ignore errors).
            let _ = tokio::process::Command::new(&bin)
                .args(["stop", &name])
                .output()
                .await;
            // Delete (ignore "not found").
            match tokio::process::Command::new(&bin)
                .args(["delete", &name])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    push(&sink, format!("  ✓ rm {name}"));
                }
                Ok(_) => push(&sink, format!("  · {name} already gone")),
                Err(e) => push(&sink, format!("  ✗ {name}: {e}")),
            }
        }
        push(&sink, format!("✓ stack down: {}", stack.name));
        Ok(())
    })
}

fn push(sink: &Arc<Mutex<Vec<String>>>, line: String) {
    if let Ok(mut v) = sink.lock() {
        if v.len() >= 2000 {
            v.drain(0..1000);
        }
        v.push(line);
    }
}

/// Path of the stack file `<dir>/<name>.toml`. Returns None if no config dir.
pub fn path_for(name: &str) -> Option<PathBuf> {
    stacks_dir().map(|d| d.join(format!("{name}.toml")))
}

/// Create a new stack file with a starter template. Returns the path on
/// success; errors if the file already exists or no config dir is available.
pub fn create_template(name: &str) -> Result<PathBuf> {
    let Some(dir) = stacks_dir() else {
        return Err(anyhow!("no XDG_CONFIG_HOME or HOME — can't write stack"));
    };
    std::fs::create_dir_all(&dir).context("create stacks dir")?;
    let p = dir.join(format!("{name}.toml"));
    if p.exists() {
        return Err(anyhow!("stack '{name}' already exists at {}", p.display()));
    }
    let body = format!(
        r#"# cgui stack — bring up with `u`, tear down with `D`.
name = "{name}"

[[service]]
name = "app"
image = "docker.io/library/alpine:latest"
# env = {{ KEY = "value" }}
# ports = ["8080:80"]
# volumes = ["mydata:/data"]
# network = "default"
# depends_on = []
# args = ["sh", "-c", "while true; do date; sleep 5; done"]
"#
    );
    std::fs::write(&p, body).context("write stack template")?;
    Ok(p)
}

/// Best-effort: write a sample stack file the first time the user opens the
/// Stacks tab and it's empty, so they have something to play with.
pub fn ensure_sample() -> Result<Option<PathBuf>> {
    let Some(dir) = stacks_dir() else { return Ok(None) };
    std::fs::create_dir_all(&dir).context("create stacks dir")?;
    let p = dir.join("example.toml");
    if p.exists() {
        return Ok(None);
    }
    let body = r#"# cgui example stack — bring up with `u`, tear down with `D`.
name = "example"

[[service]]
name = "db"
image = "docker.io/pgvector/pgvector:pg16"
env = { POSTGRES_USER = "test", POSTGRES_PASSWORD = "test" }
ports = ["15432:5432"]
"#;
    std::fs::write(&p, body).context("write example stack")?;
    Ok(Some(p))
}

