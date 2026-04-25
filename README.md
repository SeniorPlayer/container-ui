# cgui — visual front end for Apple's `container`

A fast, single-binary Rust TUI for [`apple/container`](https://github.com/apple/container)
built on [ratatui](https://ratatui.rs) + [crossterm](https://github.com/crossterm-rs/crossterm),
with a **Docker-compatible command shim** so muscle memory keeps working.

```
┌ cgui · Apple container front end ──────────────────────────────────────┐
│ [Containers]  Images  Volumes  Networks  Logs                          │
├──────────────────────┬─────────────────────────────────────────────────┤
│ CPU   12.4%          │ MEM    37.1% of limit                           │
│ ▁▂▃▅▇▆▅▃▂▁▁▂▃▅▇█▇▅▃ │ ▂▂▃▃▄▄▅▅▅▆▆▇▇▇▇▆▅▄▃▂                            │
├──────────────────────┴─────────────────────────────────────────────────┤
│ ID            IMAGE                            STATUS  CPUS  MEM       │
│▶ clerk-pg-test docker.io/pgvector/pgvector:pg16 running   4   1.0 GiB  │
│  redis-test    docker.io/library/redis:7        stopped   2  512 MiB  │
└────────────────────────────────────────────────────────────────────────┘
 q quit · ←→ tabs · ↑↓ select · r refresh · s start · x stop · K kill · d delete · l logs · a all
```

## Why

Apple's `container` is great but CLI-only. `cgui` gives you:

- **Live overview** — every running container, image, volume, and network at a glance, with sparklines for aggregate CPU/memory pulled from `container stats`.
- **One-key actions** — start, stop, kill, delete, view logs without leaving the TUI.
- **Drop-in `docker` muscle memory** — `cgui ps`, `cgui images`, `cgui rm`, `cgui rmi`, `cgui pull`, etc. translate to the right `container` invocation.

## Install

Requires:
- Rust 1.85+ (or update the pinned versions in `Cargo.toml`)
- Apple's `container` CLI on `$PATH` (and `container system start` running)

```bash
cargo build --release
cp target/release/cgui /usr/local/bin/   # optional
```

## Use

### TUI

```bash
cgui            # launch TUI
cgui tui        # same
```

| Key            | Action                                          |
| -------------- | ----------------------------------------------- |
| `q` / `Esc`    | Quit (or clear filter, if one is set)           |
| `Tab` / `→`    | Next tab                                        |
| `Shift+Tab`/`←`| Prev tab                                        |
| `↑` / `↓` / `j`| Move selection                                  |
| `Space`        | **Mark / unmark** the highlighted container for batch ops |
| `Enter`        | **Inspect** — open syntax-highlighted JSON detail pane |
| `/`            | **Filter** rows in current tab (Enter to apply) |
| `o`            | Cycle **sort** key for current tab              |
| `r`            | Refresh now                                     |
| `a`            | Toggle show-all vs running-only                 |
| `s`            | Start (operates on marked set, else highlighted row) |
| `x`            | Stop  (operates on marked set, else highlighted row) |
| `K`            | Kill  (operates on marked set, else highlighted row) |
| `d`            | Delete (operates on marked set, else highlighted row; clears marks on success) |
| `l`            | Load logs for selected → Logs tab               |
| `e`            | **Exec** — drop into `/bin/sh` in selected container (Ctrl-D returns to TUI) |
| `p`            | **Pull** an image (Images tab) — opens prompt + live progress modal |
| `b`            | **Build** an image (Images tab) — two-field prompt, then streaming modal |
| `P`            | **Re-attach** to a backgrounded pull or build      |
| `?`            | Toggle the **per-tab help** overlay                |
| **Mouse L**    | Click a tab title to switch tabs · click a row to select it |
| **Mouse R**    | Right-click anywhere → **context menu** of actions for the current tab |
| **Wheel**      | Scroll Logs · Inspect detail · Pull/Build stream   |
| `X`            | Open the **runtime profile picker** (switch which CLI cgui shells out to) |
| `Ctrl-R`       | (in Logs `/` search) toggle **regex** mode         |
| `Ctrl-O`       | (in Build prompt) open the **file picker** for the build context |
| `F`            | (Containers) start **follow-mode log streaming** · (Logs) toggle stop/start |
| `↑` / `↓`      | (in Pull/Build prompts) cycle through **recent presets** |
| `T`            | (Images) **Trivy scan** of selected image (HIGH+CRITICAL) |
| `u` / `D`      | (Stacks) **Up** / **Down** the selected stack       |

On the Logs tab `/` enters **search-as-you-type**: matches highlight in yellow as you type, with a live match counter in the title (`Logs · foo · search:err  (4 matches)`). Enter exits the input but keeps the highlight; `Esc` clears.

The pull modal renders a colored **Gauge** driven by a permissive parser of the streamed output (recognises `42%`, `12.3MB/45.6MB`, and `3/8` layer ratios — newest match wins). `Esc` backgrounds the modal: a yellow `⟳ pulling ref 42% — P to view` chip appears in the status bar so you can re-open it any time. When the pull finishes the chip turns green; pressing `P` shows the final log.

The Containers table shows **live CPU% and MEM** (used / limit) per row when a stats sample is available, with traffic-light coloring. Marked rows display a yellow `●` in the leftmost column.

On the **Volumes tab**, `Enter` opens a richer detail pane: capacity from the CLI, actual on-disk size from the backing image (sparse images are honest about it), a unicode fill bar (`[████░░░░░░] 42.3%`), and the full inspect JSON below.

User preferences (last tab, per-tab sort key, show-all toggle) are persisted to `$XDG_CONFIG_HOME/cgui/state.json` (defaults to `~/.config/cgui/state.json`). Saved on every relevant change and on quit; missing or malformed files are silently ignored.

### Theme

Drop a `theme.toml` next to the state file:

```toml
# ~/.config/cgui/theme.toml — all fields optional
accent  = "#88c0d0"   # tab highlight, modal borders, headers
primary = "white"     # default body text
muted   = "darkgray"  # punctuation, hints, dim labels
success = "#a3be8c"   # running status, ok results
warning = "yellow"    # marks, mid-progress, in-flight
danger  = "red"       # stopped, errors, high CPU
info    = "blue"      # image refs, links
```

Accepts named colors (`red`, `darkgray`, `lightcyan`, …), `#RRGGBB`, and `rgb(r, g, b)` for truecolor terminals. Missing fields fall back to the built-in defaults; a malformed file is silently ignored.

### Runtime profiles

cgui can drive any Docker-compatible CLI, not just Apple's `container`. Drop a `profiles.toml` next to `state.json`:

```toml
# ~/.config/cgui/profiles.toml
default = "container"

[[profile]]
name = "container"
binary = "container"

[[profile]]
name = "docker"
binary = "/usr/local/bin/docker"

[[profile]]
name = "podman"
binary = "/opt/homebrew/bin/podman"
```

Press `X` in the TUI to open the picker, ↑↓ + Enter to activate. The choice is saved to `state.json` so `cgui ps`, `cgui images`, etc. (the Docker-compat shim) honor it on next launch too. The active runtime is shown in the top header (`cgui · runtime: docker`).

### Resource alerts

The `[alerts]` section of `theme.toml` configures per-row CPU/MEM thresholds:

```toml
[alerts]
cpu_warn  = 60.0   # tint the row when CPU% exceeds this (steady)
cpu_alert = 85.0   # pulse when CPU% exceeds this
mem_warn  = 70.0
mem_alert = 90.0
pulse     = true   # set to false for steady highlight at alert level
```

The Containers row's background is steady-tinted at `warn` and pulses at `alert` (alternating once per ~500 ms). Defaults are 60/85/70/90 with pulse on.

### Recent presets

The pull and build prompts remember your last 10 invocations. `↑` cycles into the history (saving whatever you'd typed), `↓` cycles back; the prompt footer shows your position (e.g. `↑↓ recent (2/7)`). Storage is in the same `state.json` next to the rest of your prefs.

### Follow-mode logs

Press `F` on a Containers row to start a `container logs -f` stream into the Logs tab; press `F` again on the Logs tab to stop. The header colors green and shows `● follow` while live; auto-tails when scroll is at the top, otherwise pins to your scroll position. Combined with `/` + `Ctrl-R`, you get live regex log monitoring.

### Stacks

The **Stacks** tab is a tiny compose-style runner. Each stack lives in `~/.config/cgui/stacks/<name>.toml`:

```toml
name = "myapp"

[[service]]
name = "db"
image = "docker.io/pgvector/pgvector:pg16"
env = { POSTGRES_USER = "test", POSTGRES_PASSWORD = "test" }
ports = ["15432:5432"]
volumes = ["dbdata:/var/lib/postgresql/data"]

[[service]]
name = "api"
image = "myapp/api:latest"
network = "default"
depends_on = ["db"]
ports = ["8080:8080"]
```

In the TUI: `u` brings the stack up (`container run -d --name <stack>_<service> …` per service in topological dependency order), `D` tears it down (stop + delete every service container, in reverse). Both stream into the same modal as pull/build, so you see exactly what's executing. The `RUNNING` column shows `<up>/<total>` per stack with traffic-light coloring.

A starter `example.toml` is dropped on first run. The Stacks tab's `Enter` opens a detail pane showing the parsed services and the exact `container run` plan.

### `cgui doctor`

```
$ cgui doctor
== cgui doctor ==
✓ active profile: container → container
✓ `container` resolves to /usr/local/bin/container
✓ `container --version` → container CLI version 0.11.0
✓ container system status: running
! no profiles.toml at ~/.config/cgui/profiles.toml (using built-in default)
✓ state.json at ~/.config/cgui/state.json parses cleanly
! trivy not on PATH (image scan disabled — `brew install trivy`)
== all checks passed ==
```

Exit code 0 if everything's green, 1 otherwise. Useful for CI or scripting.

### Trivy image scan

If [trivy](https://github.com/aquasecurity/trivy) is on `$PATH`, press `T` on an Images row (or right-click → Trivy scan). Runs `trivy image --quiet --severity HIGH,CRITICAL <ref>` and streams the report into the same modal as pull/build. `Esc` backgrounds it; `P` re-attaches.

In the Detail pane: `↑↓`/`PgUp`/`PgDn` scroll, `Esc` closes.
In the Pull modal: `Esc` hides; pull keeps running in the background and the status bar reports completion.

State refreshes every 2s; sparklines smooth across ~4 minutes of history (120 samples).

### Docker-compat shim

| You type                | Runs                          |
| ----------------------- | ----------------------------- |
| `cgui ps [-a]`          | `container ls [-a]`           |
| `cgui images`           | `container image ls`          |
| `cgui rmi REF`          | `container image delete REF`  |
| `cgui pull REF`         | `container image pull REF`    |
| `cgui push REF`         | `container image push REF`    |
| `cgui tag SRC DST`      | `container image tag SRC DST` |
| `cgui login REGISTRY`   | `container registry login …`  |
| `cgui logout REGISTRY`  | `container registry logout …` |
| `cgui rm ID`            | `container delete ID`         |
| `cgui top`              | `container stats`             |
| `cgui run …`            | `container run …` (passthrough)|
| `cgui exec …`           | `container exec …` (passthrough)|
| `cgui logs …`           | `container logs …` (passthrough)|
| `cgui build …`          | `container build …` (passthrough)|

Anything not in the table is passed through unchanged, so the shim never gets in your way.

## Architecture

- `src/container.rs` — async wrapper around the `container` binary; always invokes `--format json` and decodes defensively into typed structs.
- `src/cli.rs` — `clap`-based Docker-compat verb translator.
- `src/app.rs` — pure TUI state machine (no I/O in render path).
- `src/ui.rs` — ratatui rendering: tabs, tables, sparklines, status bar.
- `src/main.rs` — terminal setup, input + tick loop on `tokio::select!`.

State refresh is async and best-effort: if one source (e.g. `volume ls`) fails, the rest still update and the error surfaces in the status bar.

## Progress

| Feature                                              | Status     | Landed in       |
| ---------------------------------------------------- | ---------- | --------------- |
| Tabs · Containers/Images/Volumes/Networks/Logs       | ✅ shipped | 0.1.0           |
| Aggregate CPU/MEM sparklines                         | ✅ shipped | 0.1.0           |
| One-key lifecycle (start/stop/kill/delete/logs)      | ✅ shipped | 0.1.0           |
| Docker-compat CLI shim (`ps`, `images`, `rmi`, …)    | ✅ shipped | 0.1.0           |
| `e` exec shell-out (`/bin/sh` in selected container) | ✅ shipped | 0.2.0           |
| `p` image pull with live streaming progress modal    | ✅ shipped | 0.2.0           |
| `/` filter + `o` sort across all resource tabs       | ✅ shipped | 0.2.0           |
| `Enter` inspect detail pane (`container inspect` JSON)| ✅ shipped | 0.2.0           |
| Per-row live CPU/MEM in Containers table             | ✅ shipped | 0.3.0           |
| `Space` multi-select + batch start/stop/kill/delete  | ✅ shipped | 0.3.0           |
| Syntax-highlighted JSON in inspect pane              | ✅ shipped | 0.3.0           |
| Parsed % gauge for image pulls                       | ✅ shipped | 0.4.0           |
| Search-as-you-type in Logs tab (highlighted matches) | ✅ shipped | 0.4.0           |
| `Esc` backgrounds pull modal · `P` re-attaches       | ✅ shipped | 0.4.0           |
| Volume detail: capacity + on-disk usage + fill gauge | ✅ shipped | 0.5.0           |
| Per-tab help overlay (`?`)                           | ✅ shipped | 0.5.0           |
| Mouse: click tabs and rows to select                 | ✅ shipped | 0.5.0           |
| Persisted prefs (tab, sort, show-all) at `~/.config/cgui/state.json` | ✅ shipped | 0.5.0 |
| Wheel scroll in long views (logs, inspect, op stream) | ✅ shipped | 0.6.0          |
| Right-click context menu                              | ✅ shipped | 0.6.0          |
| Configurable theme via `~/.config/cgui/theme.toml`    | ✅ shipped | 0.6.0          |
| `b` image build with same streaming progress modal    | ✅ shipped | 0.6.0          |
| Per-container CPU sparkline column                    | ✅ shipped | 0.7.0          |
| Regex log search (`Ctrl-R` toggles in `/`)            | ✅ shipped | 0.7.0          |
| Build context file picker (`Ctrl-O` from build prompt)| ✅ shipped | 0.7.0          |
| Runtime profile switcher (`X`) + `profiles.toml`      | ✅ shipped | 0.7.0          |
| Recent pull/build presets (↑↓ in prompts)             | ✅ shipped | 0.8.0          |
| Follow-mode log streaming (`F`) with auto-tail        | ✅ shipped | 0.8.0          |
| Configurable resource alerts (`[alerts]` in theme)    | ✅ shipped | 0.8.0          |
| `cgui doctor` environment health check                | ✅ shipped | 0.9.0          |
| Network detail pane (mode/state/subnets/nameservers)  | ✅ shipped | 0.9.0          |
| Trivy image scan (`T` on Images tab)                  | ✅ shipped | 0.9.0          |
| **Stacks** tab — compose-style multi-service sessions | ✅ shipped | 0.9.0          |
| Optional GUI front end (Tauri)                        | 🟡 planned | —              |

## Roadmap

- Optional GUI front end (Tauri) sharing the same `container.rs` core
- Stack edit/create from the TUI (currently file-based only)
- Trivy results parser → severity-grouped table view (currently raw text)
- Compose-format import (translate docker-compose.yml → cgui stack TOML)
