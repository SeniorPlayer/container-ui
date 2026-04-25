//! `docker-compose.yml` → cgui stack TOML import.
//!
//! We support a useful subset of compose v2/v3 service syntax (image, env,
//! ports, volumes, depends_on, networks, command). Unrecognised keys are
//! silently dropped — this is a pragmatic translator, not a full compose
//! engine.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct ComposeFile {
    #[serde(default)]
    services: BTreeMap<String, ComposeService>,
}

#[derive(Debug, Default, Deserialize)]
struct ComposeService {
    image: Option<String>,
    #[serde(default)]
    environment: EnvSpec,
    #[serde(default)]
    ports: Vec<StringOrLong>,
    #[serde(default)]
    volumes: Vec<StringOrLong>,
    #[serde(default)]
    depends_on: DependsOn,
    #[serde(default)]
    networks: NetworksSpec,
    #[serde(default)]
    command: Option<CommandSpec>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum EnvSpec {
    #[default]
    None,
    Map(BTreeMap<String, serde_yaml::Value>),
    List(Vec<String>),
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum DependsOn {
    #[default]
    None,
    List(Vec<String>),
    Map(BTreeMap<String, serde_yaml::Value>),
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum NetworksSpec {
    #[default]
    None,
    List(Vec<String>),
    Map(BTreeMap<String, serde_yaml::Value>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CommandSpec {
    String(String),
    List(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrLong {
    String(String),
    Map(BTreeMap<String, serde_yaml::Value>),
}

impl StringOrLong {
    fn as_short(&self) -> Option<String> {
        match self {
            StringOrLong::String(s) => Some(s.clone()),
            StringOrLong::Map(m) => {
                // Translate compose long syntax for ports + volumes back to
                // short syntax where we can.
                let target = m.get("target").and_then(yaml_str);
                let published = m.get("published").and_then(yaml_str);
                if let (Some(p), Some(t)) = (&published, &target) {
                    return Some(format!("{p}:{t}"));
                }
                let source = m.get("source").and_then(yaml_str);
                if let (Some(s), Some(t)) = (&source, &target) {
                    return Some(format!("{s}:{t}"));
                }
                target
            }
        }
    }
}

fn yaml_str(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Translate a `docker-compose.yml` file into a cgui stack TOML body.
/// Returns the rendered TOML string. The caller writes it.
pub fn import(yaml_path: &Path, stack_name: &str) -> Result<String> {
    let body = std::fs::read_to_string(yaml_path)
        .with_context(|| format!("read {}", yaml_path.display()))?;
    let cf: ComposeFile = serde_yaml::from_str(&body)
        .with_context(|| format!("parse {} as compose YAML", yaml_path.display()))?;
    if cf.services.is_empty() {
        return Err(anyhow!("no services found in {}", yaml_path.display()));
    }

    let mut out = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(out, "# Imported from {}", yaml_path.display());
    let _ = writeln!(out, "name = {}", toml_escape(stack_name));
    let _ = writeln!(out);

    for (svc_name, svc) in &cf.services {
        let _ = writeln!(out, "[[service]]");
        let _ = writeln!(out, "name = {}", toml_escape(svc_name));
        let image = svc.image.clone().unwrap_or_else(|| "alpine:latest".into());
        let _ = writeln!(out, "image = {}", toml_escape(&image));

        // env
        let env_pairs: Vec<(String, String)> = match &svc.environment {
            EnvSpec::None => vec![],
            EnvSpec::Map(m) => m
                .iter()
                .filter_map(|(k, v)| yaml_str(v).map(|s| (k.clone(), s)))
                .collect(),
            EnvSpec::List(l) => l
                .iter()
                .filter_map(|s| {
                    s.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect(),
        };
        if !env_pairs.is_empty() {
            let inline = env_pairs
                .iter()
                .map(|(k, v)| format!("{k} = {}", toml_escape(v)))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(out, "env = {{ {inline} }}");
        }

        // ports
        let ports: Vec<String> = svc.ports.iter().filter_map(|p| p.as_short()).collect();
        if !ports.is_empty() {
            let _ = writeln!(out, "ports = {}", toml_array(&ports));
        }
        // volumes
        let volumes: Vec<String> = svc.volumes.iter().filter_map(|p| p.as_short()).collect();
        if !volumes.is_empty() {
            let _ = writeln!(out, "volumes = {}", toml_array(&volumes));
        }
        // network — pick the first if multiple
        let net = match &svc.networks {
            NetworksSpec::None => None,
            NetworksSpec::List(l) => l.first().cloned(),
            NetworksSpec::Map(m) => m.keys().next().cloned(),
        };
        if let Some(n) = net {
            let _ = writeln!(out, "network = {}", toml_escape(&n));
        }
        // depends_on
        let deps: Vec<String> = match &svc.depends_on {
            DependsOn::None => vec![],
            DependsOn::List(l) => l.clone(),
            DependsOn::Map(m) => m.keys().cloned().collect(),
        };
        if !deps.is_empty() {
            let _ = writeln!(out, "depends_on = {}", toml_array(&deps));
        }
        // command → args
        let cmd: Vec<String> = match &svc.command {
            None => vec![],
            Some(CommandSpec::String(s)) => {
                // shell-split is overkill; compose's "command: foo bar" goes
                // verbatim. We preserve it as a single arg + sh -c style is
                // user's call.
                vec!["sh".into(), "-c".into(), s.clone()]
            }
            Some(CommandSpec::List(l)) => l.clone(),
        };
        if !cmd.is_empty() {
            let _ = writeln!(out, "args = {}", toml_array(&cmd));
        }
        let _ = writeln!(out);
    }
    Ok(out)
}

fn toml_escape(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
fn toml_array(items: &[String]) -> String {
    let inner = items
        .iter()
        .map(|s| toml_escape(s))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn simple_services() {
        let dir = tempdir();
        let p = dir.join("docker-compose.yml");
        std::fs::write(
            &p,
            r#"
services:
  db:
    image: postgres:16
    environment:
      POSTGRES_USER: test
      POSTGRES_PASSWORD: test
    ports: ["15432:5432"]
    volumes: ["dbdata:/var/lib/postgresql/data"]
  api:
    image: myapp/api:latest
    depends_on: ["db"]
    networks: ["default"]
    command: ["./run.sh", "--port", "8080"]
"#,
        )
        .unwrap();
        let toml = import(&p, "myapp").unwrap();
        assert!(toml.contains("name = \"myapp\""));
        assert!(toml.contains("[[service]]"));
        assert!(toml.contains("name = \"db\""));
        assert!(toml.contains("image = \"postgres:16\""));
        assert!(toml.contains("POSTGRES_USER = \"test\""));
        assert!(toml.contains("ports = [\"15432:5432\"]"));
        assert!(toml.contains("depends_on = [\"db\"]"));
        assert!(toml.contains("network = \"default\""));
        assert!(toml.contains("args = [\"./run.sh\", \"--port\", \"8080\"]"));
    }

    #[test]
    fn env_list_form() {
        let dir = tempdir();
        let p = dir.join("c.yml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(
            f,
            r#"
services:
  db:
    image: alpine
    environment:
      - FOO=bar
      - BAZ=qux
"#
        )
        .unwrap();
        let toml = import(&p, "x").unwrap();
        assert!(toml.contains("FOO = \"bar\""));
        assert!(toml.contains("BAZ = \"qux\""));
    }

    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let n: u32 = std::process::id() ^ (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos());
        p.push(format!("cgui-test-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
